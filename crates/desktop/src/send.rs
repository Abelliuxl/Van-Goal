//! Sending a prompt, stopping a turn, answering a question, and the queue.
//!
//! A prompt goes to the session on screen, or nowhere:
//!
//! * a live session → send to it;
//! * no live connection to the session on screen → **resume it, then send**;
//! * no session on screen at all → a genuinely new chat, the only case that
//!   creates one.
//!
//! The prompt appears in the transcript the moment it is sent, with the reply's
//! placeholder under it. If it cannot be delivered it is taken back out of the
//! transcript and returned to the composer, so nothing typed is ever lost.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};

use van_goal_core::log_debug;
use van_goal_core::models::{
    now_unix, AgentSession, ComposerAttachment, ConnectionState, QueuedPrompt,
};

use crate::client::{backend_config, Shared};
use crate::state::{submitted_prompt, ComposerAction, State};

/// How long a prompt typed while connecting waits for the connection.
const CONNECT_WAIT: Duration = Duration::from_secs(45);

/// How long the backend gets to end a turn the user stopped before the spinner
/// is stopped here. Some backends acknowledge an interrupt and never say the
/// turn ended.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// The composer's Send: answer the question being asked, queue behind the turn
/// that is running, or send.
pub fn submit(shared: &Arc<Shared>, text: String, attachments: Vec<ComposerAttachment>) {
    let text = text.trim().to_string();
    let mut state = shared.lock();
    if state.clarify.is_some() {
        if text.is_empty() {
            return;
        }
        state.composer.push(ComposerAction::Clear);
        drop(state);
        answer(shared, text);
        return;
    }
    if text.is_empty() && attachments.is_empty() {
        return;
    }
    let kind = state.kind();
    if !state.settings.is_backend_enabled(kind) {
        state.error(format!(
            "{} is switched off. Turn it on in Settings to send.",
            kind.display_name()
        ));
        return;
    }
    state.composer.push(ComposerAction::Clear);
    if state.is_sending() {
        state.queue.push(QueuedPrompt::new(text, attachments));
        state.changed();
        return;
    }
    drop(state);
    send(shared, text, attachments);
}

/// Put the prompt on screen and deliver it.
fn send(shared: &Arc<Shared>, text: String, attachments: Vec<ComposerAttachment>) {
    let (generation, prompt_id, turn) = {
        let mut state = shared.lock();
        state.conversation.begin_turn_with_placeholder(&text);
        let messages = state.conversation.messages_mut();
        // The user's message is the one just before the placeholder; it keeps
        // the files it was sent with so the transcript can show them.
        let index = messages.len() - 2;
        messages[index].attachments = attachments.clone();
        let prompt_id = messages[index].id.clone();
        state.turn_seq = state.turn_seq.wrapping_add(1);
        state.stopping = false;
        state.last_error = None;
        state.messages_changed();
        crate::cache::schedule(shared, &mut state);
        (state.view_generation, prompt_id, state.turn_seq)
    };

    let task = shared.clone();
    shared.spawn(async move {
        let outcome = deliver(&task, generation, &text, &attachments).await;
        if let Err(error) = outcome {
            log_debug!("app", "prompt not sent: {error}");
            take_back(
                &task,
                &prompt_id,
                turn,
                text,
                attachments,
                error.to_string(),
            );
        }
    });
}

async fn deliver(
    shared: &Arc<Shared>,
    generation: u64,
    text: &str,
    attachments: &[ComposerAttachment],
) -> Result<()> {
    wait_for_connection(shared).await?;

    let backend = shared.lock().backend.clone();
    let mut guard = backend.lock().await;
    crate::connect::ensure_stream(shared, &mut guard).await?;

    let (live, stored, config) = {
        let state = shared.lock();
        if state.view_generation != generation {
            return Err(anyhow!(
                "The conversation changed before the prompt was sent."
            ));
        }
        (
            state.live_id.clone(),
            state.stored_id.clone(),
            backend_config(&state.settings),
        )
    };

    let live = match (live, stored) {
        (Some(live), _) => live,
        // The session on screen exists on the backend; only the connection to
        // it is missing — the normal state right after a reconnect.
        (None, Some(stored)) => {
            let ids = guard
                .resume_session(&config, &stored)
                .await
                .map_err(|error| anyhow!("Could not open the session: {error}"))?;
            let mut state = shared.lock();
            if state.view_generation != generation {
                return Err(anyhow!(
                    "The conversation changed before the prompt was sent."
                ));
            }
            state.live_id = Some(ids.live_id.clone());
            state.stored_id = Some(ids.stored_id.unwrap_or(stored));
            state.changed();
            ids.live_id
        }
        (None, None) => {
            guard.clear_session_scope();
            let ids = guard
                .create_session(&config)
                .await
                .map_err(|error| anyhow!("Could not start a session: {error}"))?;
            let mut state = shared.lock();
            if state.view_generation != generation {
                return Err(anyhow!(
                    "The conversation changed before the prompt was sent."
                ));
            }
            adopt_new_session(shared, &mut state, &ids.live_id, ids.stored_id.clone());
            ids.live_id
        }
    };

    guard
        .submit_prompt(&live, &submitted_prompt(text, attachments))
        .await
        .map_err(|error| anyhow!("Could not send the prompt: {error}"))?;
    drop(guard);
    // The gateway names a chat from its first prompt; pick that name up.
    crate::sessions::refresh_quietly(shared);
    Ok(())
}

/// A chat created by its first prompt becomes the session on screen, listed
/// at the top until the backend's own list names it.
fn adopt_new_session(shared: &Shared, state: &mut State, live: &str, stored: Option<String>) {
    let local_id = stored.clone().unwrap_or_else(|| live.to_string());
    state.live_id = Some(live.to_string());
    state.stored_id = Some(local_id.clone());
    let now = now_unix();
    let session = AgentSession {
        id: local_id.clone(),
        title: Some("New Chat".into()),
        started_at: Some(now),
        last_active: Some(now),
        message_count: Some(0),
        is_active: Some(true),
        archived: Some(false),
        profile: state.settings.normalized_profile(),
        backend_id: Some(state.backend_id().to_string()),
        ..AgentSession::default()
    };
    if !state.sessions.iter().any(|row| row.id == local_id) {
        state.sessions.insert(0, session.clone());
    }
    state.selected = Some(session);
    crate::cache::sync_sessions(state);
    crate::cache::save_now(shared, state);
}

/// Wait for the event stream if a connect is on its way; start one if the
/// backend is on but nothing is connecting.
async fn wait_for_connection(shared: &Arc<Shared>) -> Result<()> {
    let needs_connect = {
        let state = shared.lock();
        !state.transport_ready
            && state.connection != ConnectionState::Connecting
            && !state.gateway_connecting
    };
    if needs_connect {
        crate::connect::connect(shared);
    }
    let deadline = tokio::time::Instant::now() + CONNECT_WAIT;
    loop {
        {
            let state = shared.lock();
            if state.transport_ready {
                return Ok(());
            }
            // A stream that dropped on its own is retried within a second or
            // two, which is worth waiting for; a backend that is failing is
            // not, and the prompt goes back with the reason.
            let settled = !state.gateway_connecting
                && state.connection != ConnectionState::Connecting
                && !(state.reconnecting && state.connection == ConnectionState::Disconnected);
            if settled {
                // The connect finished without a stream: say why.
                return Err(anyhow!(state.last_error.clone().unwrap_or_else(
                    || format!("{} is not connected.", state.kind().display_name())
                )));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!("No connection after {}s.", CONNECT_WAIT.as_secs()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The prompt could not be delivered: take it back out of the transcript and
/// give it to the composer, with the reason.
fn take_back(
    shared: &Arc<Shared>,
    prompt_id: &str,
    turn: u64,
    text: String,
    attachments: Vec<ComposerAttachment>,
    error: String,
) {
    let mut state = shared.lock();
    let messages = state.conversation.messages_mut();
    if let Some(index) = messages.iter().position(|message| message.id == prompt_id) {
        // The prompt and the placeholder after it; anything the backend did
        // manage to stream in is kept.
        let mut end = index + 1;
        while end < messages.len() && messages[end].is_empty_shell_or_placeholder() {
            end += 1;
        }
        messages.drain(index..end);
    }
    if state.turn_seq == turn {
        state.conversation.finish_turn();
    }
    state
        .composer
        .push(ComposerAction::Restore { text, attachments });
    state.messages_changed();
    state.error(error);
}

trait Placeholder {
    fn is_empty_shell_or_placeholder(&self) -> bool;
}

impl Placeholder for van_goal_core::models::ChatMessage {
    fn is_empty_shell_or_placeholder(&self) -> bool {
        self.role == van_goal_core::models::MessageRole::Assistant
            && self.content.trim().is_empty()
            && self.tool_calls.is_empty()
    }
}

/// The turn has ended: the question it asked is stale, the transcript is worth
/// persisting, the gateway may have renamed the chat, and the next queued
/// prompt can go.
pub fn after_turn(shared: &Arc<Shared>) {
    let next = {
        let mut state = shared.lock();
        state.clarify = None;
        state.stopping = false;
        crate::cache::save_now(shared, &mut state);
        state.changed();
        if state.is_sending() || state.queue.is_empty() {
            None
        } else {
            Some(state.queue.remove(0))
        }
    };
    crate::sessions::refresh_quietly(shared);
    if let Some(next) = next {
        send(shared, next.text, next.attachments);
    }
}

/// Stop the turn that is running.
pub fn stop(shared: &Arc<Shared>) {
    let (backend, live, turn) = {
        let mut state = shared.lock();
        if !state.is_sending() || state.stopping {
            return;
        }
        state.stopping = true;
        state.changed();
        (state.backend.clone(), state.live_id.clone(), state.turn_seq)
    };
    let task = shared.clone();
    shared.spawn(async move {
        if let Some(live) = live {
            if let Err(error) = backend.lock().await.interrupt(&live).await {
                let mut state = task.lock();
                state.stopping = false;
                state.error(format!("Could not stop the turn: {error}"));
                return;
            }
        }
        // The backend normally ends the turn itself; if it does not, the user
        // asked for it to stop and the spinner must not keep turning.
        tokio::time::sleep(STOP_GRACE).await;
        let ended_here = {
            let mut state = task.lock();
            if state.turn_seq == turn && state.is_sending() {
                state.conversation.finish_turn();
                state.messages_changed();
                true
            } else {
                false
            }
        };
        if ended_here {
            after_turn(&task);
        }
    });
}

/// Answer the question the agent asked. The answer starts a new turn of its
/// own, shown as the user's message.
pub fn answer(shared: &Arc<Shared>, text: String) {
    let text = text.trim().to_string();
    let (backend, clarify) = {
        let mut state = shared.lock();
        let Some(clarify) = state.clarify.take() else {
            return;
        };
        if text.is_empty() {
            state.clarify = Some(clarify);
            return;
        }
        state.conversation.finish_turn();
        state.conversation.begin_turn_with_placeholder(&text);
        state.turn_seq = state.turn_seq.wrapping_add(1);
        state.messages_changed();
        crate::cache::schedule(shared, &mut state);
        (state.backend.clone(), clarify)
    };
    let task = shared.clone();
    shared.spawn(async move {
        let result = backend
            .lock()
            .await
            .respond_to_interaction(&clarify.request_id, &text)
            .await;
        if let Err(error) = result {
            let mut state = task.lock();
            state.conversation.finish_turn();
            state.messages_changed();
            state.error(format!("Could not answer: {error}"));
        }
    });
}

pub fn dismiss_clarify(shared: &Arc<Shared>) {
    let (backend, clarify) = {
        let mut state = shared.lock();
        let Some(clarify) = state.clarify.take() else {
            return;
        };
        state.changed();
        (state.backend.clone(), clarify)
    };
    let task = shared.clone();
    shared.spawn(async move {
        let result = backend
            .lock()
            .await
            .respond_to_interaction(&clarify.request_id, "")
            .await;
        if let Err(error) = result {
            task.lock()
                .error(format!("Could not skip the question: {error}"));
        }
    });
}

/// Send a queued prompt now: it moves to the front of the queue and the running
/// turn is stopped, and the end of that turn sends it. Sending it straight away
/// would let the stopped turn's last events complete the new one's reply.
pub fn send_queued_now(shared: &Arc<Shared>, id: &str) {
    let sending = {
        let mut state = shared.lock();
        let Some(index) = state.queue.iter().position(|item| item.id == id) else {
            return;
        };
        let item = state.queue.remove(index);
        state.queue.insert(0, item);
        state.changed();
        state.is_sending()
    };
    if sending {
        stop(shared);
    } else {
        after_turn(shared);
    }
}

pub fn edit_queued(shared: &Arc<Shared>, id: &str) {
    let mut state = shared.lock();
    let Some(index) = state.queue.iter().position(|item| item.id == id) else {
        return;
    };
    let item = state.queue.remove(index);
    state.composer.push(ComposerAction::Restore {
        text: item.text,
        attachments: item.attachments,
    });
    state.changed();
}

pub fn cancel_queued(shared: &Arc<Shared>, id: &str) {
    let mut state = shared.lock();
    state.queue.retain(|item| item.id != id);
    state.changed();
}
