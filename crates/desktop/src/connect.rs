//! Connecting, disconnecting, switching backends, and coming back on its own
//! when a connection drops.
//!
//! The GPUI client had no reconnect at all: a gateway restart, a laptop lid or
//! a network change left it "Disconnected" until the user pressed Refresh. This
//! one retries with the mobile bridge's backoff for as long as the backend's
//! switch is on, and re-attaches the session on screen when it is back.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use tokio::sync::MutexGuard as AsyncGuard;

use van_goal_core::agent::Backend;
use van_goal_core::local_server::ManagedServer;
use van_goal_core::log_debug;
use van_goal_core::models::{AgentEvent, BackendConfig, ConnectionState};
use van_goal_core::settings::BackendKind;

use crate::client::{backend_config, Shared};
use crate::state::{visible_sessions, ComposerAction, State};

/// How long to wait before the next attempt, in seconds, from the first failure
/// onwards. The last value is used for every later attempt.
const RETRY_SECONDS: [u64; 6] = [1, 2, 4, 8, 15, 30];

/// Probe the backend and open its event stream, then bring the view back.
///
/// A connect already in flight owns that work: an explicit refresh that lands
/// in the middle of one must not stack a second probe and resume on top of it.
pub fn connect(shared: &Arc<Shared>) {
    let (generation, settings, backend) = {
        let mut state = shared.lock();
        if state.connection == ConnectionState::Connecting && !state.reconnecting {
            return;
        }
        state.reconnecting = false;
        state.connection = ConnectionState::Connecting;
        state.transport_ready = false;
        state.connect_generation = state.connect_generation.wrapping_add(1);
        state.changed();
        (
            state.connect_generation,
            state.settings.clone(),
            state.backend.clone(),
        )
    };
    log_debug!(
        "app",
        "connect requested backend={}",
        settings.backend_kind.id()
    );

    let task = shared.clone();
    shared.spawn(async move {
        let kind = settings.backend_kind;
        // The password is needed before the first request: a protected server
        // answers 401 to a password-less probe, which reads as unreachable. A
        // managed server's own health check only takes one when it has a
        // password of its own (mimo serve); the gateway token a Hermes server
        // hands out is discovered after the probe instead.
        let server_password = if kind.server_takes_password() {
            settings.session_token.trim().to_string()
        } else {
            String::new()
        };
        let workspace = settings.workspace_trimmed();
        let base_url = match ManagedServer::for_kind(kind) {
            Some(server) if settings.is_managed_local_backend() => {
                task.local_server
                    .ensure_running(
                        server,
                        settings.resolved_port(),
                        workspace.as_deref(),
                        &server_password,
                    )
                    .await
            }
            _ => Ok(settings.active_backend_url()),
        };
        let probed: Result<Option<String>> = async {
            let base_url = base_url?;
            let config = BackendConfig {
                base_url: base_url.clone(),
                ..backend_config(&settings)
            };
            let mut backend = backend.lock().await;
            backend.probe(&config).await?;
            // A local Hermes hands out its own token; fetch it once and keep it.
            if kind == BackendKind::Hermes && settings.session_token.trim().is_empty() {
                return Ok(Some(backend.discover_credential(&base_url).await?));
            }
            Ok(None)
        }
        .await;

        {
            let mut state = task.lock();
            if state.connect_generation != generation {
                return;
            }
            match probed {
                Err(error) => {
                    let message = error.to_string();
                    state.connection = ConnectionState::Failed(message.clone());
                    state.error(message);
                    drop(state);
                    schedule_retry(&task, generation);
                    return;
                }
                Ok(Some(token)) if !token.trim().is_empty() => {
                    state.settings.session_token = token;
                    state.settings.save();
                    state.settings_changed();
                }
                Ok(_) => {}
            }
        }

        // One backend lock for the stream and the session it carries, so
        // nothing else can slip in between opening one and attaching the other.
        let backend = task.lock().backend.clone();
        let mut guard = backend.lock().await;
        if task.lock().connect_generation != generation {
            return;
        }
        if let Err(error) = ensure_stream(&task, &mut guard).await {
            drop(guard);
            schedule_retry(&task, generation);
            log_debug!("app", "gateway connect failed: {error}");
            return;
        }
        drop(guard);

        crate::sessions::refresh(&task, false);
        // Come back to the conversation that was open, or stay on a blank
        // chat: a session is only created when its first prompt is sent, so
        // connecting never leaves an empty session behind on the backend.
        let reopen = {
            let state = task.lock();
            state.selected.clone()
        };
        match reopen {
            Some(session) => crate::sessions::open(&task, session),
            None => crate::sessions::clear_scope_if_blank(&task),
        }
    });
}

/// Open the backend's event stream, if this client has none, onto a channel of
/// its own.
///
/// Each stream gets a fresh channel and an epoch. An adapter that is replaced
/// still reports its old socket closing, and on a channel shared between
/// streams that `Disconnected` would arrive after the new stream's `Connected`
/// and mark a healthy connection down.
pub async fn ensure_stream(
    shared: &Arc<Shared>,
    backend: &mut AsyncGuard<'_, Backend>,
) -> Result<()> {
    let (config, sender) = {
        let mut state = shared.lock();
        if state.transport_ready {
            return Ok(());
        }
        if state.kind() == BackendKind::Hermes && state.settings.session_token.trim().is_empty() {
            let message = "Missing Hermes session token".to_string();
            state.connection = ConnectionState::Failed(message.clone());
            state.error(message.clone());
            return Err(anyhow!(message));
        }
        state.gateway_connecting = true;
        if state.connection != ConnectionState::Connected {
            state.connection = ConnectionState::Connecting;
        }
        state.changed();
        let sender = open_channel(shared, &mut state);
        (backend_config(&state.settings), sender)
    };
    let epoch = shared.lock().connection_epoch;
    let preference = shared
        .lock()
        .settings
        .xuexiaomi_models
        .get(&config.base_url)
        .cloned();
    backend.restore_model_preference(preference);
    let result = backend.connect(config, sender).await;

    let mut state = shared.lock();
    if state.connection_epoch != epoch {
        // Overtaken by a disconnect or a newer stream while connecting.
        return Err(anyhow!("the connection was replaced"));
    }
    state.gateway_connecting = false;
    match result {
        Ok(()) => {
            state.transport_ready = true;
            state.changed();
            Ok(())
        }
        Err(error) => {
            state.transport_ready = false;
            state.connection = ConnectionState::Failed(error.to_string());
            state.error(error.to_string());
            Err(error)
        }
    }
}

fn open_channel(shared: &Arc<Shared>, state: &mut State) -> UnboundedSender<AgentEvent> {
    state.connection_epoch = state.connection_epoch.wrapping_add(1);
    let epoch = state.connection_epoch;
    let (sender, mut receiver) = unbounded::<AgentEvent>();
    let pump = shared.clone();
    shared.spawn(async move {
        while let Some(event) = receiver.next().await {
            crate::events::handle(&pump, epoch, event);
        }
    });
    sender
}

/// Try again later, if the user still wants this backend connected and nothing
/// newer has happened since the attempt that failed.
pub fn schedule_retry(shared: &Arc<Shared>, generation: u64) {
    let wait = {
        let mut state = shared.lock();
        let kind = state.kind();
        if state.connect_generation != generation || !state.settings.is_backend_enabled(kind) {
            return;
        }
        let wait = RETRY_SECONDS[state.retry_attempt.min(RETRY_SECONDS.len() - 1)];
        state.retry_attempt += 1;
        state.reconnecting = true;
        state.changed();
        wait
    };
    log_debug!("app", "reconnecting in {wait}s");
    let task = shared.clone();
    shared.spawn(async move {
        tokio::time::sleep(Duration::from_secs(wait)).await;
        let still_wanted = {
            let state = task.lock();
            state.connect_generation == generation && state.reconnecting
        };
        if still_wanted {
            connect(&task);
        }
    });
}

/// The stream went away on its own (not because the user asked): come back.
pub fn stream_lost(shared: &Arc<Shared>) {
    let generation = shared.lock().connect_generation;
    schedule_retry(shared, generation);
}

pub fn disconnect(shared: &Arc<Shared>) {
    let backend = {
        let mut state = shared.lock();
        state.advance_view_keep_screen();
        state.connect_generation = state.connect_generation.wrapping_add(1);
        state.connection_epoch = state.connection_epoch.wrapping_add(1);
        state.cache_generation = state.cache_generation.wrapping_add(1);
        state.transport_ready = false;
        state.gateway_connecting = false;
        state.reconnecting = false;
        state.retry_attempt = 0;
        state.live_id = None;
        state.connection = ConnectionState::Disconnected;
        state.clarify = None;
        state.stopping = false;
        if state.conversation.is_sending() {
            state.conversation.finish_turn();
            state.messages_changed();
        }
        state.changed();
        state.backend.clone()
    };
    shared.spawn(async move {
        backend.lock().await.disconnect();
    });
}

/// Turn a backend's switch on, switching to it if it is not the active one.
pub fn enable_backend(shared: &Arc<Shared>, kind: BackendKind) {
    let already = {
        let state = shared.lock();
        state.kind() == kind && state.settings.is_backend_enabled(kind)
    };
    if already {
        connect(shared);
    } else {
        switch_backend(shared, kind);
    }
}

/// Turn a backend's switch off. The choice survives a restart.
pub fn disable_backend(shared: &Arc<Shared>, kind: BackendKind) {
    let active = {
        let mut state = shared.lock();
        state.settings.set_backend_enabled(kind, false);
        state.settings.save();
        state.settings_changed();
        state.kind() == kind
    };
    if active {
        disconnect(shared);
    }
}

/// Make `kind` the active backend. Choosing a backend means "use this one", so
/// its switch is turned on — which turns every other backend off.
pub fn switch_backend(shared: &Arc<Shared>, kind: BackendKind) {
    let old = {
        let mut state = shared.lock();
        state.advance_view();
        state.connect_generation = state.connect_generation.wrapping_add(1);
        state.connection_epoch = state.connection_epoch.wrapping_add(1);
        state.cache_generation = state.cache_generation.wrapping_add(1);
        let old = state.backend.clone();

        state.settings.set_backend_enabled(kind, true);
        state.settings.switch_backend(kind);
        state.settings.save();
        let backend = Backend::make(kind);
        state.caps = backend.capabilities();
        state.backend = Arc::new(tokio::sync::Mutex::new(backend));
        state.gateway_model = None;

        state.transport_ready = false;
        state.gateway_connecting = false;
        state.reconnecting = false;
        state.retry_attempt = 0;
        state.connection = ConnectionState::Disconnected;
        state.selected = None;
        state.sessions = visible_sessions(&state.cached.sessions, kind.id(), &state.cached);
        state.conversation = van_goal_core::chat::Conversation::new();
        state.live_id = None;
        state.stored_id = None;
        state.clarify = None;
        state.stopping = false;
        state.last_error = None;
        // Prompts waiting for the previous backend's session must not be sent
        // to this one: they go back to the composer instead.
        return_pending_to_composer(&mut state);
        state.messages_changed();
        state.settings_changed();
        old
    };
    shared.spawn(async move {
        old.lock().await.disconnect();
    });
    connect(shared);
}

/// Give every queued prompt back to the composer, joined, so leaving a
/// conversation never throws away what the user typed for it — and never sends
/// it into the conversation they went to instead.
pub fn return_pending_to_composer(state: &mut State) {
    let mut texts = Vec::new();
    let mut attachments = Vec::new();
    for item in std::mem::take(&mut state.queue) {
        texts.push(item.text);
        attachments.extend(item.attachments);
    }
    texts.retain(|text| !text.trim().is_empty());
    if texts.is_empty() && attachments.is_empty() {
        return;
    }
    state.composer.push(ComposerAction::Restore {
        text: texts.join("\n\n"),
        attachments,
    });
    state.changed();
}

/// Put the managed server into the directory the settings now name.
///
/// Only a server this app started is moved, never while a reply is being
/// written into the directory being left, and the connection follows it.
pub fn apply_workspace(shared: &Arc<Shared>) {
    let plan = {
        let mut state = shared.lock();
        let kind = state.kind();
        let Some(server) = ManagedServer::for_kind(kind) else {
            return;
        };
        if !state.settings.is_managed_local_backend()
            || !shared.local_server.is_managing()
            || !state.settings.is_backend_enabled(kind)
        {
            return;
        }
        let workspace = state.settings.workspace_trimmed();
        if shared.local_server.started_in() == workspace {
            return;
        }
        if state.is_sending() {
            shared.local_server.set_message(format!(
                "{} is still running in {}; a reply is being written, so the workspace applies at the next connect.",
                server.label(),
                shared
                    .local_server
                    .started_in()
                    .unwrap_or_else(|| "the app's own directory".into())
            ));
            state.changed();
            return;
        }
        (
            server,
            state.settings.resolved_port(),
            workspace,
            state.settings.session_token.trim().to_string(),
        )
    };
    let (server, port, workspace, credential) = plan;
    let task = shared.clone();
    shared.spawn(async move {
        let result = task
            .local_server
            .restart_in(server, port, workspace.as_deref(), &credential)
            .await;
        {
            let mut state = task.lock();
            if let Err(error) = result {
                state.error(error.to_string());
            }
            // The stream was attached to the server that just stopped.
            state.transport_ready = false;
            state.connection_epoch = state.connection_epoch.wrapping_add(1);
            state.live_id = None;
            state.changed();
        }
        connect(&task);
    });
}

impl State {
    /// A disconnect invalidates in-flight results without changing what is on
    /// screen: the transcript stays readable while offline.
    pub fn advance_view_keep_screen(&mut self) {
        self.view_generation = self.view_generation.wrapping_add(1);
    }
}
