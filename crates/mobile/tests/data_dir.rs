//! The one part of the C ABI whose behaviour depends on being called *first*,
//! so it gets a test binary of its own: `set_app_dir` resolves a process-wide
//! value, and a test sharing this process could not observe the un-resolved
//! state it needs. The null-pointer test is safe to run alongside it because it
//! is refused before anything global is touched.

use std::ffi::{CStr, CString};

use van_goal_mobile::vg_set_data_dir;

/// Read a string the library handed out and release it, the way Dart does.
unsafe fn take(ptr: *mut std::ffi::c_char) -> String {
    let text = CStr::from_ptr(ptr).to_string_lossy().to_string();
    van_goal_mobile::vg_free(ptr);
    text
}

#[test]
fn the_host_names_the_app_directory_exactly_once() {
    let dir = std::env::temp_dir().join("van-goal-mobile-host-dir-test");
    let _ = std::fs::remove_dir_all(&dir);

    let path = CString::new(dir.to_str().expect("temp dir is UTF-8")).expect("no NUL");
    let first = unsafe { take(vg_set_data_dir(path.as_ptr())) };
    assert_eq!(
        first, r#"{"ok":true}"#,
        "the first caller names the directory"
    );
    assert!(dir.is_dir(), "the directory is created for the host");

    // A second call cannot move an app that is already reading and writing
    // somewhere, so it is refused rather than silently ignored.
    let elsewhere = CString::new("/tmp").expect("no NUL");
    let second = unsafe { take(vg_set_data_dir(elsewhere.as_ptr())) };
    assert_eq!(second, r#"{"ok":false}"#);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_null_directory_is_refused_rather_than_crashing() {
    let reply = unsafe { take(vg_set_data_dir(std::ptr::null())) };
    assert!(
        reply.contains("null argument"),
        "a null pointer must come back as an error, not a segfault: {reply}"
    );
}
