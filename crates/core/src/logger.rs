use chrono::Local;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Minimal debug logger writing to `~/Library/Application Support/VanGoal/VanGoal.log`.
/// Mirrors the SwiftUI sibling's logger: disabled by default, opt-in from Settings.
pub struct VanGoalLogger {
    enabled: AtomicBool,
    path: PathBuf,
    handle: Mutex<Option<File>>,
}

impl Default for VanGoalLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl VanGoalLogger {
    fn log_dir() -> PathBuf {
        dirs::app_dir()
    }

    pub fn new() -> Self {
        let dir = Self::log_dir();
        let _ = std::fs::create_dir_all(&dir);
        Self {
            enabled: AtomicBool::new(false),
            path: dir.join("VanGoal.log"),
            handle: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn log(&self, category: &str, message: impl AsRef<str>) {
        if !self.is_enabled() {
            return;
        }
        let line = format!(
            "{} [{category}] {}\n",
            Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            message.as_ref()
        );
        let mut guard = self.handle.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
            *guard = file;
        }
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    pub fn clear(&self) {
        let mut guard = self.handle.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
        let _ = std::fs::write(&self.path, b"");
    }
}

pub fn global_logger() -> &'static VanGoalLogger {
    static LOGGER: std::sync::OnceLock<VanGoalLogger> = std::sync::OnceLock::new();
    LOGGER.get_or_init(VanGoalLogger::new)
}

#[macro_export]
macro_rules! log_debug {
    ($category:expr, $($arg:tt)*) => {
        if $crate::logger::global_logger().is_enabled() {
            $crate::logger::global_logger().log($category, format!($($arg)*));
        }
    };
}

/// Small helper namespace so the rest of the app can locate support dirs.
pub mod dirs {
    use std::path::PathBuf;
    use std::sync::RwLock;

    /// App-data directory name. The app was called Hermit until it was renamed to
    /// Van-Goal; the old directory is moved into place once so that settings, the
    /// session cache and the OpenClaw device identity survive the rename.
    const APP_DIR_NAME: &str = "VanGoal";
    const LEGACY_APP_DIR_NAME: &str = "HermitGPUI";

    /// Where the app-data directory ended up, resolved once. `None` until
    /// something asks, so a host that has a directory of its own to offer can
    /// name it first.
    static RESOLVED_APP_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

    pub fn home() -> PathBuf {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/"))
    }

    /// Only the non-test path resolves through here, since tests redirect to a
    /// temporary directory.
    #[cfg_attr(any(test, feature = "testing"), allow(dead_code))]
    pub fn app_support() -> PathBuf {
        home().join("Library/Application Support")
    }

    /// Directory holding settings, the session cache and the OpenClaw device
    /// identity. Every caller goes through here, so the rename happens once,
    /// before anything reads or creates the directory.
    ///
    /// On a platform with no `~/Library/Application Support` the host has to
    /// name the directory first — see [`set_app_dir`].
    pub fn app_dir() -> PathBuf {
        if let Some(dir) = RESOLVED_APP_DIR
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return dir;
        }
        let resolved = resolve_app_dir();
        *RESOLVED_APP_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(resolved.clone());
        resolved
    }

    /// Point the app-data directory at a location the host chooses.
    ///
    /// Android and iOS do not have `~/Library/Application Support`: an app is
    /// given a private directory by the platform, and the mobile client passes
    /// it here before anything reads [`app_dir`]. Returns `false` when the
    /// directory has already been resolved, because the choice is made once and
    /// remembered — a caller that gets `false` is telling the app to read and
    /// write somewhere other than where it already has.
    pub fn set_app_dir(path: PathBuf) -> bool {
        let _ = std::fs::create_dir_all(&path);
        let mut slot = RESOLVED_APP_DIR.write().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return false;
        }
        *slot = Some(path);
        true
    }

    /// Unit tests must never touch the real app data. Building an `AppState`
    /// loads and saves settings, so a test run would otherwise rewrite the
    /// developer's own preferences, cache and paired device identity.
    ///
    /// This is a feature rather than `cfg(test)` because `cfg(test)` does not
    /// reach a dependency: when `crates/desktop` builds its test binary, this
    /// crate is compiled *without* `cfg(test)` and would resolve the real
    /// directory — which is exactly what happened the first time these crates
    /// were split apart. The desktop crate turns the feature on from its
    /// `[dev-dependencies]`, so it is off for every real build.
    #[cfg(any(test, feature = "testing"))]
    fn resolve_app_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("van-goal-test-app-dir-{}", std::process::id()));
        // The operating system reuses process ids, and this directory is never
        // cleaned up, so a run can otherwise inherit the settings and cache a
        // previous run left behind and fail on assertions about defaults.
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[cfg(not(any(test, feature = "testing")))]
    fn resolve_app_dir() -> PathBuf {
        migrate_app_dir(&app_support())
    }

    /// Resolve the app directory under `support`, moving the pre-rename
    /// `HermitGPUI` directory into place the first time. Losing that directory
    /// would mean losing the stored credentials, the session cache and the
    /// OpenClaw device pairing — which the Gateway would have to approve again.
    ///
    /// Unused while [`resolve_app_dir`] is redirected to a temporary directory,
    /// which is the case in every test build.
    #[cfg_attr(any(test, feature = "testing"), allow(dead_code))]
    fn migrate_app_dir(support: &std::path::Path) -> PathBuf {
        let current = support.join(APP_DIR_NAME);
        let legacy = support.join(LEGACY_APP_DIR_NAME);
        if !current.exists() && legacy.exists() && std::fs::rename(&legacy, &current).is_err() {
            // A rename fails across volumes; copying still keeps the node's
            // settings, cache and paired device identity.
            let _ = copy_tree(&legacy, &current);
        }
        current
    }

    /// Test seam: [`app_dir`] caches its result in a `OnceLock`, so it cannot be
    /// exercised more than once per process.
    #[cfg(test)]
    pub fn migrate_app_dir_for_test(support: &std::path::Path) -> PathBuf {
        migrate_app_dir(support)
    }

    #[cfg_attr(any(test, feature = "testing"), allow(dead_code))]
    fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), &target)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::dirs::migrate_app_dir_for_test;

    struct TempSupport(std::path::PathBuf);

    impl TempSupport {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("van-goal-dirs-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempSupport {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Upgrading from the Hermit build must not orphan the settings, the session
    /// cache or the OpenClaw device identity that live in the old directory.
    #[test]
    fn the_pre_rename_app_directory_is_moved_into_place() {
        let support = TempSupport::new();
        let legacy = support.path().join("HermitGPUI");
        std::fs::create_dir_all(legacy.join("nested")).expect("legacy dir");
        std::fs::write(
            legacy.join("settings.json"),
            b"{\"backend_kind\":\"OpenClaw\"}",
        )
        .expect("legacy settings");
        std::fs::write(
            legacy.join("nested/openclaw-device-identity.json"),
            b"device",
        )
        .expect("legacy device identity");

        let resolved = migrate_app_dir_for_test(support.path());

        assert_eq!(resolved, support.path().join("VanGoal"));
        assert_eq!(
            std::fs::read(resolved.join("settings.json")).expect("moved settings"),
            b"{\"backend_kind\":\"OpenClaw\"}"
        );
        assert_eq!(
            std::fs::read(resolved.join("nested/openclaw-device-identity.json"))
                .expect("moved device identity"),
            b"device"
        );
        assert!(
            !legacy.exists(),
            "the old directory is moved, not copied, so nothing is left behind"
        );
    }

    #[test]
    fn an_existing_app_directory_is_never_overwritten() {
        let support = TempSupport::new();
        std::fs::create_dir_all(support.path().join("HermitGPUI")).expect("legacy dir");
        std::fs::write(support.path().join("HermitGPUI/settings.json"), b"legacy")
            .expect("legacy settings");
        std::fs::create_dir_all(support.path().join("VanGoal")).expect("current dir");
        std::fs::write(support.path().join("VanGoal/settings.json"), b"current")
            .expect("current settings");

        let resolved = migrate_app_dir_for_test(support.path());

        assert_eq!(
            std::fs::read(resolved.join("settings.json")).expect("current settings"),
            b"current",
            "an existing VanGoal directory must win over the legacy one"
        );
    }

    #[test]
    fn a_fresh_install_just_gets_the_new_directory() {
        let support = TempSupport::new();
        assert_eq!(
            migrate_app_dir_for_test(support.path()),
            support.path().join("VanGoal")
        );
    }
}
