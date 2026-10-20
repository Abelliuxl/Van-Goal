//! The session list, and which session is on screen.

use std::collections::HashSet;
use std::sync::Arc;

use van_goal_core::chat::{merge_fetched_sessions, Conversation};
use van_goal_core::log_debug;
use van_goal_core::models::{now_unix, AgentSession, BackendCaps, CachedState, ConnectionState};

use crate::client::{backend_config, Shared};
use crate::state::{cache_key, refresh_needs_reconnect, visible_messages, State};

/// Re-read the session list. An explicit refresh (the toolbar, ⌘R) is also how
/// a dead event stream is restored: listing alone can succeed over HTTP while
/// the stream that carries replies stays down.
pub fn refresh(shared: &Arc<Shared>, show_progress: bool) {
    let (backend, config, expects_history) = {
        let mut state = shared.lock();
        if refresh_needs_reconnect(show_progress, state.transport_ready) {
            let kind = state.kind();
            if state.settings.is_backend_enabled(kind) {
                drop(state);
                crate::connect::connect(shared);
            } else {
                state.error(format!(
                    "{} is switched off. Turn it on in Settings to connect.",
                    kind.display_name()
                ));
            }
            return;
        }
        if show_progress {
            state.is_refreshing = true;
        }
        state.refresh_inflight = true;
        state.changed();
        (
            state.backend.clone(),
            backend_config(&state.settings),
            state.caps.contains(BackendCaps::SESSION_HISTORY),
        )
    };
    let task = shared.clone();
    shared.spawn(async move {
        let (result, backend_id) = {
            let mut guard = backend.lock().await;
            (guard.list_sessions(&config).await, guard.id())
        };
        let mut state = task.lock();
        state.refresh_inflight = false;
        if show_progress {
            state.is_refreshing = false;
        }
        state.changed();
        // Answered by a backend that has since been switched away from.
        if state.backend_id() != backend_id {
            return;
        }
        match result {
            Ok(fetched) if expects_history => {
                let fetched: Vec<AgentSession> = fetched
                    .into_iter()
                    .map(|mut session| {
                        session.backend_id = Some(backend_id.to_string());
                        session.archived.get_or_insert(false);
                        session
                    })
                    .collect();
                let selected = state.selected.clone();
                let merged = merge_fetched_sessions(fetched, selected.as_ref());
                state.sessions = state.filter_visible(merged);
                // Keep the selection's row in step with the list's, so a name
                // the gateway gave the chat replaces "New Chat" in the header.
                if let Some(selected) = state.selected.as_ref().map(|s| s.id.clone()) {
                    if let Some(row) = state
                        .sessions
                        .iter()
                        .find(|row| row.id == selected)
                        .cloned()
                    {
                        state.selected = Some(row);
                    }
                }
                crate::cache::sync_sessions(&mut state);
                crate::cache::write(&task, &mut state);
            }
            Ok(_) => {}
            Err(error) => {
                let message = error.to_string();
                if state.connection == ConnectionState::Connected {
                    state.connection = ConnectionState::Degraded(message.clone());
                }
                state.error(message);
            }
        }
    });
}

/// Re-read the list without the progress indicator, at most one at a time: a
/// gateway sends "sessions changed" in bursts.
pub fn refresh_quietly(shared: &Arc<Shared>) {
    if shared.lock().refresh_inflight {
        return;
    }
    refresh(shared, false);
}

/// Show a session and attach this connection to it.
///
/// Both halves matter and the order is the point: the transcript is shown
/// *before* the session is resumed, so the conversation on screen and the
/// session the next prompt goes to are the same one.
pub fn open(shared: &Arc<Shared>, session: AgentSession) {
    let (generation, enabled) = {
        let mut state = shared.lock();
        let generation = state.advance_view();
        // Prompts queued in the conversation being left belong there.
        crate::connect::return_pending_to_composer(&mut state);
        state.selected = Some(session.clone());
        state.clarify = None;
        state.stopping = false;
        // Not attached to it yet: `accepts_turn_events` holds turn traffic off
        // until the backend confirms, which is why the live id is cleared here.
        state.live_id = None;
        state.stored_id = Some(session.id.clone());
        let key = cache_key(state.backend_id(), &session.id);
        state.cached.selected_session_id = Some(key.clone());
        let cached = state
            .cached
            .messages_by_session_id
            .get(&key)
            .filter(|messages| !messages.is_empty())
            .map(|messages| visible_messages(messages, now_unix()));
        state.conversation = Conversation::new();
        if let Some(messages) = cached {
            state.conversation.set_transcript(messages);
        }
        state.messages_changed();
        crate::cache::write(shared, &mut state);
        let kind = state.kind();
        (generation, state.settings.is_backend_enabled(kind))
    };
    // A backend that is switched off shows what this client has kept and asks
    // it nothing; the session is attached when the backend is turned on.
    if !enabled {
        return;
    }

    let task = shared.clone();
    let id = session.id;
    shared.spawn(async move {
        let backend = task.lock().backend.clone();
        let mut guard = backend.lock().await;
        if !task.lock().view_is_current(generation, Some(&id)) {
            return;
        }
        if crate::connect::ensure_stream(&task, &mut guard)
            .await
            .is_err()
        {
            drop(guard);
            crate::connect::stream_lost(&task);
            return;
        }
        let config = backend_config(&task.lock().settings);

        match guard.messages(&config, &id).await {
            Ok(fetched) => {
                let mut state = task.lock();
                // The user can switch again while the history is in flight;
                // the reply belongs to the view that asked for it.
                if !state.view_is_current(generation, Some(&id)) {
                    return;
                }
                let visible = visible_messages(&fetched, now_unix());
                // A turn in flight owns its bubble until it completes, and the
                // history does not contain the reply being written yet.
                if !state.is_sending() {
                    // History carries no tool calls, so it is merged rather
                    // than substituted: what this client watched survives.
                    state.conversation.merge_transcript(visible);
                    state.messages_changed();
                    crate::cache::save_now(&task, &mut state);
                }
            }
            Err(error) => {
                let mut state = task.lock();
                if state.view_is_current(generation, Some(&id)) {
                    state.error(error.to_string());
                }
            }
        }

        if !task.lock().view_is_current(generation, Some(&id)) {
            return;
        }
        let resumed = guard.resume_session(&config, &id).await;
        let mut state = task.lock();
        if !state.view_is_current(generation, Some(&id)) {
            // `resume_session` changed the adapter's scope. If the user moved
            // to a blank chat meanwhile, put it back; a newer session has its
            // own `open` queued behind this lock and will set its own.
            if state.selected.is_none() {
                drop(state);
                guard.clear_session_scope();
            }
            return;
        }
        match resumed {
            Ok(ids) => {
                state.live_id = Some(ids.live_id.clone());
                state.stored_id = Some(ids.stored_id.unwrap_or(id));
                state.transport_ready = true;
                state.changed();
            }
            Err(error) => {
                log_debug!("app", "resume failed: {error}");
                state.error(format!("Could not open the session: {error}"));
            }
        }
    });
}

/// Start a chat that has no session on the backend yet. The session is created
/// by its first prompt, so a new chat that is never used leaves nothing behind.
pub fn new_chat(shared: &Arc<Shared>) {
    {
        let mut state = shared.lock();
        state.advance_view();
        crate::connect::return_pending_to_composer(&mut state);
        state.selected = None;
        state.conversation = Conversation::new();
        state.clarify = None;
        state.stopping = false;
        state.live_id = None;
        state.stored_id = None;
        // A relaunch comes back to this blank chat, not the one just left.
        state.cached.selected_session_id = None;
        state.messages_changed();
        crate::cache::write(shared, &mut state);
    }
    clear_scope_if_blank(shared);
}

/// Until the first prompt creates this chat, the adapter must not stay scoped
/// to the session the user left: its late frames would be the only labelled
/// frames that pass the filter, and they would land in the blank chat.
pub fn clear_scope_if_blank(shared: &Arc<Shared>) {
    let (backend, generation) = {
        let state = shared.lock();
        (state.backend.clone(), state.view_generation)
    };
    let task = shared.clone();
    shared.spawn(async move {
        let mut guard = backend.lock().await;
        if task.lock().view_is_current(generation, None) {
            guard.clear_session_scope();
        }
    });
}

pub fn find(state: &State, id: &str) -> Option<AgentSession> {
    state
        .sessions
        .iter()
        .find(|session| session.id == id)
        .cloned()
        .or_else(|| state.selected.clone().filter(|session| session.id == id))
}

/// Archive or delete sessions. Both are local: the backend keeps the session,
/// this client stops listing it (and, for a delete, forgets its transcript).
pub fn remove(shared: &Arc<Shared>, ids: Vec<String>, delete: bool) {
    if ids.is_empty() {
        return;
    }
    let removed_selected = {
        let mut state = shared.lock();
        let backend_id = state.backend_id();
        let ids: HashSet<String> = ids.into_iter().collect();
        for id in &ids {
            let key = cache_key(backend_id, id);
            if delete {
                state.cached.deleted_session_ids.insert(key.clone());
                state.cached.messages_by_session_id.remove(&key);
            } else {
                state.cached.archived_session_ids.insert(key);
            }
        }
        state.sessions.retain(|session| !ids.contains(&session.id));
        crate::cache::sync_sessions(&mut state);
        crate::cache::write(shared, &mut state);
        state
            .selected_id()
            .is_some_and(|selected| ids.contains(selected))
    };
    if removed_selected {
        new_chat(shared);
    }
}

pub fn clear_cache(shared: &Arc<Shared>) {
    {
        let mut state = shared.lock();
        state.cached = CachedState::default();
        shared.cache.clear();
        state.sessions.clear();
        state.changed();
    }
    new_chat(shared);
    refresh_quietly(shared);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_found_in_the_list_or_as_the_selection() {
        let client = crate::client::client();
        let mut state = client.shared.lock();
        state.sessions = vec![AgentSession {
            id: "listed".into(),
            ..AgentSession::default()
        }];
        state.selected = Some(AgentSession {
            id: "created-here".into(),
            ..AgentSession::default()
        });
        assert!(find(&state, "listed").is_some());
        assert!(find(&state, "created-here").is_some());
        assert!(find(&state, "gone").is_none());
    }
}
