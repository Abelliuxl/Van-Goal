//! C ABI over `van-goal-core`, for the Flutter client.
//!
//! Flutter has neither Rust nor a tokio runtime, so this crate supplies both:
//! it owns the runtime, drives a [`Backend`](van_goal_core::agent::Backend),
//! and exposes a deliberately small surface to Dart.
//!
//! ## The shape of the API
//!
//! Three functions, JSON strings in and JSON strings out:
//!
//! * [`vg_command`] takes one command and returns an acknowledgement.
//! * [`vg_poll`] drains everything that has happened since the last call.
//! * [`vg_free`] releases a string this library handed out.
//!
//! ## Why every call returns immediately
//!
//! Dart calls these on its platform thread. Listing sessions, connecting or
//! sending a prompt all talk to the network, so none of them may block: a
//! command is queued onto the tokio runtime and its result comes back through
//! [`vg_poll`] as an event. The Flutter side is therefore a loop that drains
//! events and rebuilds its widget tree, which is how it wants to work anyway.
//!
//! ## Why JSON rather than generated bindings
//!
//! Hand-written FFI over JSON is a smaller thing to depend on than a code
//! generator whose version has to match this crate's, and the payloads here are
//! chat messages — the encoding cost is irrelevant next to the network round
//! trip. It also means the wire format is inspectable in a log.
//!
//! ## Panics
//!
//! Unwinding across an `extern "C"` boundary is undefined behaviour, so every
//! entry point wraps its body in `catch_unwind` and reports a panic as an error
//! string instead of aborting the process.

mod client;

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

use client::client;

/// Release a string returned by [`vg_command`] or [`vg_poll`].
///
/// # Safety
///
/// `ptr` must be a pointer this library returned and not yet freed, or null.
#[no_mangle]
pub unsafe extern "C" fn vg_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

/// Name the directory the app may keep its settings, session cache and OpenClaw
/// device identity in.
///
/// Android and iOS do not hand an app a home directory the way macOS does; they
/// give it a private one chosen by the platform, so the host has to say where.
/// Call this before any other `vg_` function: the choice is made once, and a
/// reply of `{"ok":false}` means something already read the directory and the
/// app is using a different one.
///
/// # Safety
///
/// `path` must be a NUL-terminated UTF-8 string, or null.
#[no_mangle]
pub unsafe extern "C" fn vg_set_data_dir(path: *const c_char) -> *mut c_char {
    let dir = match read(path) {
        Ok(text) => text,
        Err(error) => return reply(serde_json::json!({ "ok": false, "error": error })),
    };
    let accepted = van_goal_core::logger::dirs::set_app_dir(std::path::PathBuf::from(dir.as_str()));
    reply(serde_json::json!({ "ok": accepted }))
}

/// Run one command. Returns `{"ok":true}` or `{"ok":false,"error":"…"}`.
///
/// The command's actual result — sessions, a transcript, an error from the
/// gateway — arrives later through [`vg_poll`]. The few commands whose answer is
/// a value rather than an event (the saved preferences, the markdown blocks of a
/// message) put it in the reply next to `ok`, because it is computed here and
/// instantly: routing it through the queue would mean matching a request to an
/// event for no gain.
///
/// # Safety
///
/// `json` must be a NUL-terminated UTF-8 string, or null.
#[no_mangle]
pub unsafe extern "C" fn vg_command(json: *const c_char) -> *mut c_char {
    let command = match read(json) {
        Ok(text) => text,
        Err(error) => return reply(serde_json::json!({ "ok": false, "error": error })),
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| client().run(&command)));
    match outcome {
        Ok(Ok(payload)) => {
            let mut answer = serde_json::json!({ "ok": true });
            if let (Some(answer), Some(payload)) = (answer.as_object_mut(), payload) {
                if let Some(fields) = payload.as_object() {
                    for (key, value) in fields {
                        answer.insert(key.clone(), value.clone());
                    }
                }
            }
            reply(answer)
        }
        Ok(Err(error)) => reply(serde_json::json!({ "ok": false, "error": error.to_string() })),
        Err(_) => reply(serde_json::json!({
            "ok": false,
            "error": "van-goal panicked handling the command",
        })),
    }
}

/// Take every event queued since the last call, as a JSON array.
///
/// Returns `[]` when nothing has happened, which is the common case: the Dart
/// side calls this on a timer.
#[no_mangle]
pub extern "C" fn vg_poll() -> *mut c_char {
    let events = catch_unwind(AssertUnwindSafe(|| client().poll())).unwrap_or_else(|_| {
        vec![serde_json::json!({ "event": "error", "message": "poll panicked" })]
    });
    reply(serde_json::Value::Array(events))
}

/// Borrow a NUL-terminated string from Dart.
unsafe fn read(ptr: *const c_char) -> Result<String, String> {
    if ptr.is_null() {
        return Err("null argument".into());
    }
    CStr::from_ptr(ptr)
        .to_str()
        .map(str::to_string)
        .map_err(|error| format!("argument was not UTF-8: {error}"))
}

/// Hand a string to Dart. JSON escaping means the payload never contains a NUL,
/// but `CString::new` is checked rather than unwrapped so a bug upstream cannot
/// take the process down.
fn reply(value: serde_json::Value) -> *mut c_char {
    match CString::new(value.to_string()) {
        Ok(text) => text.into_raw(),
        Err(_) => CString::new(r#"{"ok":false,"error":"reply contained a NUL"}"#)
            .expect("literal has no NUL")
            .into_raw(),
    }
}
