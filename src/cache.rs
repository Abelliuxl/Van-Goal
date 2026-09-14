use crate::logger::global_logger;
use crate::models::CachedState;
use crate::{log_debug, logger::dirs};
use std::path::PathBuf;

/// On-disk cache of sessions and transcripts, saved off the hot path via a
/// background thread (mirrors the SwiftUI SessionCacheStore).
#[derive(Clone)]
pub struct SessionCacheStore {
    path: PathBuf,
}

impl SessionCacheStore {
    pub fn new() -> Self {
        let dir = dirs::app_dir();
        let _ = std::fs::create_dir_all(&dir);
        Self {
            path: dir.join("SessionCache.json"),
        }
    }

    pub fn load(&self) -> CachedState {
        match std::fs::read(&self.path) {
            Ok(data) => match serde_json::from_slice::<CachedState>(&data) {
                Ok(state) => {
                    log_debug!(
                        "cache",
                        "loaded cache sessions={} messageSets={}",
                        state.sessions.len(),
                        state.messages_by_session_id.len()
                    );
                    state
                }
                Err(error) => {
                    global_logger().log("cache", format!("cache load failed: {error}"));
                    CachedState::default()
                }
            },
            Err(_) => CachedState::default(),
        }
    }

    /// Serialize + write on a detached background thread; the caller returns
    /// immediately so streaming deltas never block the UI.
    pub fn save(&self, state: CachedState) {
        let path = self.path.clone();
        std::thread::spawn(move || match serde_json::to_vec(&state) {
            Ok(data) => {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, &data).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                log_debug!(
                    "cache",
                    "saved cache sessions={} messageSets={}",
                    state.sessions.len(),
                    state.messages_by_session_id.len()
                );
            }
            Err(error) => {
                global_logger().log("cache", format!("cache save failed: {error}"));
            }
        });
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
        global_logger().log("cache", "cache cleared");
    }
}
