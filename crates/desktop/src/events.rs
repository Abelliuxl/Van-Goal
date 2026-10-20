//! Folding the backend's events into the state.

use std::sync::Arc;

use van_goal_core::chat::ConversationChange;
use van_goal_core::log_debug;
use van_goal_core::models::{AgentEvent, ConnectionState, PendingClarify};

use crate::client::Shared;
use crate::state::accepts_turn_events;

pub fn handle(shared: &Arc<Shared>, epoch: u64, event: AgentEvent) {
    let mut state = shared.lock();
    // Events from a stream this client has already replaced or closed. The
    // socket going away is the last thing such a stream says, and it must not
    // mark the connection that replaced it down.
    if state.connection_epoch != epoch {
        return;
    }

    // A turn's traffic is only the open transcript's while a session is
    // subscribed. During a switch the session being left is still streaming
    // into this connection, and its reply would be written into the chat the
    // user just opened.
    let is_turn_event = matches!(
        &event,
        AgentEvent::MessageStart
            | AgentEvent::MessageDelta { .. }
            | AgentEvent::MessageComplete(_)
            | AgentEvent::Tool(_)
            | AgentEvent::TurnFailed(_)
            | AgentEvent::Clarify { .. }
    );
    if is_turn_event && !accepts_turn_events(state.live_id.as_deref(), state.stored_id.as_deref()) {
        log_debug!("app", "turn event dropped while a session is opening");
        return;
    }

    match event {
        AgentEvent::Connected => {
            state.transport_ready = true;
            state.gateway_connecting = false;
            state.connection = ConnectionState::Connected;
            state.reconnecting = false;
            state.retry_attempt = 0;
            state.last_error = None;
            state.changed();
        }
        AgentEvent::SessionInfo(id) => {
            state.live_id = Some(id);
            state.transport_ready = true;
            state.changed();
        }
        AgentEvent::SessionsChanged => {
            drop(state);
            crate::sessions::refresh_quietly(shared);
        }
        AgentEvent::Clarify {
            question,
            choices,
            request_id,
            session_id,
        } => {
            // A question asked in another session is not this chat's to answer.
            if let (Some(asked_in), Some(live)) = (session_id.as_deref(), state.live_id.as_deref())
            {
                let stored = state.stored_id.as_deref();
                if asked_in != live && Some(asked_in) != stored {
                    return;
                }
            }
            state.clarify = Some(PendingClarify {
                session_id,
                question,
                choices,
                request_id,
            });
            state.changed();
        }
        AgentEvent::Disconnected | AgentEvent::Failed(_) => {
            let failure = match &event {
                AgentEvent::Failed(message) => Some(message.clone()),
                _ => None,
            };
            state.transport_ready = false;
            state.gateway_connecting = false;
            state.clarify = None;
            // The connection is gone, and with it the subscription: the
            // session on screen is reattached when the stream comes back.
            state.live_id = None;
            // The rest of a reply on a dead stream is never coming.
            if state.conversation.is_sending() {
                state.conversation.finish_turn();
                state.messages_changed();
            }
            match failure {
                Some(message) => {
                    state.connection = ConnectionState::Failed(message.clone());
                    state.error(message);
                }
                None => {
                    if state.connection == ConnectionState::Connected {
                        state.connection = ConnectionState::Disconnected;
                    }
                    state.changed();
                }
            }
            let enabled = state.settings.is_backend_enabled(state.kind());
            drop(state);
            if enabled {
                crate::connect::stream_lost(shared);
            }
        }
        // The user stopped this turn, and the backend reports that as a
        // failure ("请求已停止"). What streamed in so far is the reply they
        // chose to keep, not something to replace with an error.
        AgentEvent::TurnFailed(_) if state.stopping => {
            state.conversation.finish_turn();
            state.messages_changed();
            drop(state);
            crate::send::after_turn(shared);
        }
        AgentEvent::TurnFailed(message) => {
            state.last_error = Some(message.clone());
            // The failure goes into the transcript as the turn's final text, so
            // it is read where the reply would have been.
            state
                .conversation
                .complete_message(Some(&format!("Error: {message}")));
            state.messages_changed();
            drop(state);
            crate::send::after_turn(shared);
        }
        turn_event => {
            let finished = matches!(turn_event, AgentEvent::MessageComplete(_));
            let was_sending = state.conversation.is_sending();
            match state.conversation.apply(&turn_event) {
                ConversationChange::None => {}
                ConversationChange::Streaming { id, .. } => {
                    if !state.streamed.contains(&id) {
                        state.streamed.push(id);
                    }
                    state.changed();
                    crate::cache::schedule(shared, &mut state);
                }
                ConversationChange::Structure => {
                    state.messages_changed();
                    crate::cache::schedule(shared, &mut state);
                }
            }
            // The GPUI client only ran the end-of-turn work for a failed turn,
            // so a queued prompt never went out after a normal reply. A second
            // completion for the same turn finds nothing sending and is a no-op.
            if finished && was_sending {
                drop(state);
                crate::send::after_turn(shared);
            }
        }
    }
}
