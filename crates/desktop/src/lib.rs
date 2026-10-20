//! C ABI over `van-goal-core`, for the MyGo desktop client.
//!
//! The Go side draws; this side owns the connection, the session state machine
//! and the message list. It is the desktop counterpart of `crates/mobile`, and
//! it is shaped the same way for the same reasons: a hand-written ABI over JSON
//! is a smaller thing to depend on than a binding generator, and the payloads
//! are chat messages, whose encoding cost is nothing next to the network.
//!
//! ## The shape of the API
//!
//! * [`vgd_command`] runs one command and returns `{"ok":true, …}` or
//!   `{"ok":false,"error":…}`. Nothing in it blocks on the network: work that
//!   talks to a backend is spawned onto the bridge's runtime.
//! * [`vgd_wait`] blocks the calling thread until something changed or the
//!   timeout passed. The Go side runs it on a goroutine of its own, so the
//!   window redraws the moment a delta lands and an idle app costs nothing.
//! * [`vgd_poll`] takes what changed since the last call: the small state
//!   payload, the message list when its structure changed, and the text of
//!   messages that only streamed further.
//! * [`vgd_free`] releases a string this library handed out.
//!
//! ## Panics
//!
//! Unwinding across `extern "C"` is undefined behaviour, so every entry point
//! catches it and reports a panic as an error instead of aborting the process.

mod cache;
mod client;
mod commands;
mod connect;
mod events;
mod hermes;
mod markdown;
mod payload;
mod send;
mod sessions;
mod state;

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::{json, Value};

pub use client::client;

/// Point the app-data directory somewhere else. Only for the Go side's tests,
/// which must never read or rewrite the developer's real settings; an app
/// leaves it alone and gets the platform's own directory. Must come before any
/// other call, and returns 0 when something already resolved the directory.
///
/// # Safety
///
/// `path` must be a NUL-terminated UTF-8 string, or null.
#[no_mangle]
pub unsafe extern "C" fn vgd_set_data_dir(path: *const c_char) -> i32 {
    match read(path) {
        Ok(dir) => i32::from(van_goal_core::logger::dirs::set_app_dir(dir.into())),
        Err(_) => 0,
    }
}

/// Release a string returned by this library.
///
/// # Safety
///
/// `ptr` must be a pointer this library returned and not yet freed, or null.
#[no_mangle]
pub unsafe extern "C" fn vgd_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

/// Run one command.
///
/// # Safety
///
/// `json` must be a NUL-terminated UTF-8 string, or null.
#[no_mangle]
pub unsafe extern "C" fn vgd_command(json: *const c_char) -> *mut c_char {
    let command = match read(json) {
        Ok(text) => text,
        Err(error) => return reply(json!({ "ok": false, "error": error })),
    };
    match catch_unwind(AssertUnwindSafe(|| client().run(&command))) {
        Ok(Ok(payload)) => {
            let mut answer = json!({ "ok": true });
            if let (Some(answer), Some(Value::Object(fields))) = (answer.as_object_mut(), payload) {
                answer.extend(fields);
            }
            reply(answer)
        }
        Ok(Err(error)) => reply(json!({ "ok": false, "error": error.to_string() })),
        Err(_) => reply(json!({ "ok": false, "error": "van-goal panicked handling the command" })),
    }
}

/// Block until something changed, or until `timeout_ms` passed. Returns 1 when
/// there is something to poll, 0 on a timeout.
#[no_mangle]
pub extern "C" fn vgd_wait(timeout_ms: u32) -> i32 {
    catch_unwind(|| client().wait(timeout_ms))
        .map(i32::from)
        .unwrap_or(1)
}

/// Take what changed since the last call, as one JSON object.
#[no_mangle]
pub extern "C" fn vgd_poll() -> *mut c_char {
    let update = catch_unwind(AssertUnwindSafe(|| client().poll()))
        .unwrap_or_else(|_| json!({ "error": "poll panicked" }));
    reply(update)
}

unsafe fn read(ptr: *const c_char) -> Result<String, String> {
    if ptr.is_null() {
        return Err("null argument".into());
    }
    CStr::from_ptr(ptr)
        .to_str()
        .map(str::to_string)
        .map_err(|error| format!("argument was not UTF-8: {error}"))
}

/// JSON escaping means a payload never contains a NUL, but `CString::new` is
/// checked rather than unwrapped so a bug upstream cannot take the app down.
fn reply(value: Value) -> *mut c_char {
    match CString::new(value.to_string()) {
        Ok(text) => text.into_raw(),
        Err(_) => CString::new(r#"{"ok":false,"error":"reply contained a NUL"}"#)
            .expect("literal has no NUL")
            .into_raw(),
    }
}
