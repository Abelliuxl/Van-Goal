//! Persisting the conversation on screen into the session cache.
//!
//! Writes are debounced while text streams in (a long reply would otherwise
//! serialize the whole cache once per token) and immediate at the points where
//! losing them would hurt: the end of a turn, opening and creating a session.

use std::sync::Arc;
use std::time::Duration;

use van_goal_core::models::now_unix;

use crate::client::Shared;
use crate::state::{cache_key, State};

const DEBOUNCE: Duration = Duration::from_millis(900);

/// The key the conversation on screen is filed under, always prefixed with the
/// backend that issued the id: a bare id means nothing to another backend.
fn current_key(state: &State) -> Option<String> {
    let id = state
        .selected
        .as_ref()
        .map(|session| session.id.clone())
        .or_else(|| state.stored_id.clone())
        .or_else(|| state.live_id.clone())?;
    Some(cache_key(state.backend_id(), &id))
}

/// Write the conversation on screen into the cache and the cache to disk.
pub fn save_now(shared: &Shared, state: &mut State) {
    if let Some(key) = current_key(state) {
        let messages = state.messages().to_vec();
        state
            .cached
            .messages_by_session_id
            .insert(key.clone(), messages);
        state.cached.selected_session_id = Some(key);
    }
    write(shared, state);
}

/// Write the cache to disk as it stands (sessions, archive sets, selection).
pub fn write(shared: &Shared, state: &mut State) {
    state.cached.updated_at = Some(now_unix());
    // The store serializes on a thread of its own, so this returns at once.
    shared.cache.save(state.cached.clone());
    state.changed();
}

/// Save the conversation on screen once it has stopped changing for a moment.
pub fn schedule(shared: &Arc<Shared>, state: &mut State) {
    state.cache_generation = state.cache_generation.wrapping_add(1);
    let generation = state.cache_generation;
    let task = shared.clone();
    shared.spawn(async move {
        tokio::time::sleep(DEBOUNCE).await;
        let mut state = task.lock();
        if state.cache_generation == generation {
            save_now(&task, &mut state);
        }
    });
}

/// Replace this backend's sessions inside the cache with the ones listed now.
pub fn sync_sessions(state: &mut State) {
    let backend_id = state.backend_id();
    state
        .cached
        .sessions
        .retain(|session| session.backend_id.as_deref().unwrap_or("hermes") != backend_id);
    let current = state.sessions.clone();
    state.cached.sessions.extend(current);
}

pub fn summary(state: &State) -> String {
    format!(
        "{} cached session(s), {} cached transcript(s)",
        state.cached.sessions.len(),
        state.cached.messages_by_session_id.len()
    )
}
