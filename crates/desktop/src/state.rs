use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use gpui::Task;
use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;
use van_goal_core::agent::Backend;
use van_goal_core::cache::SessionCacheStore;
use van_goal_core::chat::{merge_fetched_sessions, Conversation, ConversationChange};
use van_goal_core::local_server::{LocalServerManager, ManagedServer};
use van_goal_core::models::*;
use van_goal_core::settings::{BackendKind, Settings};
use van_goal_core::{hermes_config, log_debug};

/// Tokio runtime stored as a GPUI global; all backend I/O runs on it while the
/// UI stays on the GPUI main-thread executor.
pub struct TokioGlobal(pub tokio::runtime::Runtime);

impl gpui::Global for TokioGlobal {}

pub fn tokio_spawn<T: Send + 'static>(
    cx: &gpui::App,
    future: impl Future<Output = T> + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    cx.global::<TokioGlobal>().0.spawn(future)
}

struct ConnectOutcome {
    discovered_token: Option<String>,
    error: Option<String>,
}

/// What is actually stored for the active backend, split by where it lives.
/// Settings renders this so that "field is empty" and "nothing is saved" stop
/// looking like the same thing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoredCredential {
    /// Characters of gateway token or server password held in settings.
    pub saved_characters: usize,
    /// Characters of the paired-device token OpenClaw holds for this gateway.
    pub device_token_characters: usize,
}

impl StoredCredential {
    pub fn is_stored(self) -> bool {
        self.saved_characters > 0 || self.device_token_characters > 0
    }
}

/// A prompt typed before there was a live session to send it to.
///
/// `session` is the session it has to go to, and the distinction is the whole
/// point: `Some` means the user was looking at an existing conversation, so the
/// connection has to *resume* it — creating a session instead is how a prompt
/// once landed in a conversation the user had never opened, which reads as one
/// chat's messages turning up in another. `None` means a genuinely new chat,
/// which is the only case that needs a session created.
#[derive(Clone, Debug)]
pub struct HeldPrompt {
    pub session: Option<String>,
    pub text: String,
    pub attachments: Vec<ComposerAttachment>,
}

impl HeldPrompt {
    /// The prompt as the composer should show it again, if it cannot be sent.
    fn into_composer(self) -> (String, Vec<ComposerAttachment>) {
        (self.text, self.attachments)
    }
}

/// Single source of truth for sessions, messages, streaming and sending.
pub struct AppState {
    pub settings: Settings,
    pub local_server: Arc<LocalServerManager>,
    pub connection_state: ConnectionState,
    pub sessions: Vec<AgentSession>,
    pub selected_session: Option<AgentSession>,
    /// The conversation on screen. The fold rules live in `chat::Conversation`
    /// (placeholder per turn, tool calls folded into one bubble, duplicates
    /// dropped, every streaming message stopped when the turn ends) so this
    /// frontend and the mobile bridge cannot disagree about them.
    pub conversation: Conversation,
    pub composer_text: String,
    pub composer_attachments: Vec<ComposerAttachment>,
    pub is_refreshing_sessions: bool,
    pub transport_ready: bool,
    gateway_connecting: bool,
    pub pending_queue: Vec<QueuedPrompt>,
    pub pending_clarify: Option<PendingClarify>,
    pub last_error: Option<String>,
    pub cache_summary: String,
    pub available_models: Vec<ModelOption>,
    pub model_provider_groups: Vec<(String, Vec<ModelOption>)>,
    pub current_model_provider: String,
    pub current_model_name: String,
    pub is_switching_model: bool,
    pub context_window_tokens: i64,
    pub permission_mode: PermissionMode,
    pub is_changing_permission_mode: bool,
    /// A prompt typed before there was a live session to send it to, and the
    /// session it belongs to — see [`HeldPrompt`].
    pub pending_after_start: Option<HeldPrompt>,
    /// A silent session-list refresh is in flight (debounces the gateway's
    /// "sessions changed" hints).
    sessions_refresh_inflight: bool,
    /// A streaming-delta repaint is scheduled: text moved inside a bubble that
    /// is already on screen needs no repaint of its own, and one scheduled
    /// repaint carries every delta that arrived meanwhile — see
    /// [`Self::notify_streaming`].
    stream_notify_task: Option<Task<()>>,

    backend: Arc<AsyncMutex<Backend>>,
    backend_id: &'static str,
    /// Also exposed by [`AppState::backend_display_name`]; crate-visible so a
    /// test can point the label at a different backend.
    pub(crate) backend_display_name: &'static str,
    pub(crate) backend_caps: BackendCaps,
    cache_store: SessionCacheStore,
    cached_state: CachedState,
    /// Identifies the most recent operation that changed which conversation is
    /// on screen. Async history/resume/create results captured under an older
    /// value must not mutate the new conversation.
    session_view_generation: u64,
    live_gateway_session_id: Option<String>,
    stored_gateway_session_id: Option<String>,
    event_tx: UnboundedSender<AgentEvent>,
    cache_save_task: Option<Task<()>>,
}

impl AppState {
    pub fn new(cx: &mut gpui::Context<Self>) -> Self {
        let settings = Settings::load();
        let cache_store = SessionCacheStore::new();
        let cached_state = cache_store.load();
        let sessions = Self::visible_sessions_from(
            &cached_state.sessions,
            settings.backend_kind.id(),
            &cached_state,
        );
        let selected = cached_state
            .selected_session_id
            .as_deref()
            .and_then(|selected_id| {
                sessions
                    .iter()
                    .find(|session| cache_key(session, settings.backend_kind.id()) == selected_id)
                    .cloned()
            });
        let messages = selected
            .as_ref()
            .and_then(|session| {
                cached_state
                    .messages_by_session_id
                    .get(&cache_key(session, settings.backend_kind.id()))
                    .cloned()
            })
            .unwrap_or_default();

        let (event_tx, event_rx) = unbounded::<AgentEvent>();

        van_goal_core::logger::global_logger().set_enabled(settings.debug_logging_enabled);

        let kind = settings.backend_kind;
        let mut state = Self {
            settings: settings.clone(),
            local_server: Arc::new(LocalServerManager::default()),
            connection_state: ConnectionState::Disconnected,
            sessions,
            selected_session: selected,
            conversation: Conversation::new(),
            composer_text: String::new(),
            composer_attachments: Vec::new(),
            is_refreshing_sessions: false,
            transport_ready: false,
            gateway_connecting: false,
            pending_queue: Vec::new(),
            pending_clarify: None,
            last_error: None,
            cache_summary: String::new(),
            available_models: Vec::new(),
            model_provider_groups: Vec::new(),
            current_model_provider: String::new(),
            current_model_name: String::new(),
            is_switching_model: false,
            context_window_tokens: hermes_config::DEFAULT_CONTEXT_WINDOW,
            permission_mode: PermissionMode::FullAccess,
            is_changing_permission_mode: false,
            pending_after_start: None,
            stream_notify_task: None,
            sessions_refresh_inflight: false,
            backend: Arc::new(AsyncMutex::new(Backend::make(kind))),
            backend_id: backend_static_id(kind),
            backend_display_name: backend_static_name(kind),
            backend_caps: Backend::make(kind).capabilities(),
            cache_store,
            cached_state,
            session_view_generation: 0,
            live_gateway_session_id: None,
            stored_gateway_session_id: None,
            event_tx,
            cache_save_task: None,
        };
        state.update_cache_summary();
        state
            .conversation
            .set_transcript(Self::visible_messages(&messages));
        state.refresh_hermes_model_config();
        state.refresh_permission_mode();

        // Bridge backend events into the state machine.
        cx.spawn(async move |this, cx| {
            let mut receiver: UnboundedReceiver<AgentEvent> = event_rx;
            while let Some(event) = receiver.next().await {
                let _ = this.update(cx, |state, cx| state.handle_event(event, cx));
            }
        })
        .detach();

        state
    }

    // ------------------------------------------------------------------
    // Derived values
    // ------------------------------------------------------------------

    pub fn can_send(&self) -> bool {
        self.connection_state == ConnectionState::Connected
            && (!self.composer_text.trim().is_empty() || !self.composer_attachments.is_empty())
    }

    /// The messages on screen, folded by [`Conversation`].
    pub fn messages(&self) -> &[ChatMessage] {
        self.conversation.messages()
    }

    /// Whether a prompt has been submitted and its turn has not ended.
    pub fn is_sending(&self) -> bool {
        self.conversation.is_sending()
    }

    /// Forget the credential backing the active backend. OpenClaw keeps its
    /// paired-device token outside settings, so clearing only the text field
    /// would leave the app still able to connect — which is exactly what made
    /// "Clear Credential" look like it did nothing.
    pub fn clear_credential(&mut self, cx: &mut gpui::Context<Self>) {
        let kind = self.settings.backend_kind;
        self.settings.session_token = String::new();
        let forgot_device_token = kind == BackendKind::OpenClaw
            && van_goal_core::agent::openclaw::forget_device_token(
                &self.settings.active_backend_url(),
            );
        self.settings.save();
        log_debug!(
            "app",
            "credential cleared backend={} deviceToken={}",
            kind.id(),
            forgot_device_token
        );
        cx.notify();
    }

    pub fn backend_caps(&self) -> BackendCaps {
        self.backend_caps
    }

    /// The credential that actually backs the active backend. Settings shows
    /// this instead of the raw token field, because an empty field does not mean
    /// "no credential": OpenClaw authenticates with a paired-device token kept
    /// in the app-data secret store, so its gateway-token field is legitimately
    /// empty while connecting still works.
    pub fn stored_credential(&self) -> StoredCredential {
        let kind = self.settings.backend_kind;
        let device_token_characters = if kind == BackendKind::OpenClaw {
            van_goal_core::agent::openclaw::stored_device_token_characters(
                &self.settings.active_backend_url(),
            )
        } else {
            0
        };
        StoredCredential {
            saved_characters: self.settings.stored_credential_characters(kind),
            device_token_characters,
        }
    }

    pub fn backend_display_name(&self) -> &'static str {
        self.backend_display_name
    }

    pub fn backend_id(&self) -> &'static str {
        self.backend_id
    }

    /// How full the context of the session on screen is.
    ///
    /// The backend's own accounting is preferred: a Gateway lists what each
    /// session's context is holding and how wide that session's window is, and
    /// a number it reported is worth more than one this client works out from
    /// the text it happens to hold. The estimate is for backends that report
    /// nothing, and it is marked so the frontends can say as much — see
    /// [`ContextUsage::measured`].
    pub fn context_usage(&self) -> ContextUsage {
        reported_context_usage(self.selected_session.as_ref(), &self.sessions).unwrap_or_else(
            || ContextUsage::estimated(self.estimated_context_tokens(), self.context_window_tokens),
        )
    }

    fn estimated_context_tokens(&self) -> i64 {
        const FIXED_PROMPT_TOKENS: i64 = 17_400;
        let message_chars: usize = self
            .messages()
            .iter()
            .map(|message| {
                message.content.len()
                    + message
                        .attachments
                        .iter()
                        .map(|a| a.path.len() + 8)
                        .sum::<usize>()
                    + message
                        .tool_calls
                        .iter()
                        .map(|call| call.detail.len().min(2_000))
                        .sum::<usize>()
            })
            .sum();
        let composer_chars = self.composer_text.len()
            + self
                .composer_attachments
                .iter()
                .map(|a| a.path.len() + 8)
                .sum::<usize>();
        FIXED_PROMPT_TOKENS + (((message_chars + composer_chars) as i64) / 4).max(0)
    }

    fn backend_config(&self) -> BackendConfig {
        BackendConfig {
            base_url: self.settings.active_backend_url(),
            credential: self.settings.session_token.trim().to_string(),
            profile: self.settings.normalized_profile(),
            workspace: self.settings.workspace_trimmed(),
        }
    }

    // ------------------------------------------------------------------
    // Bootstrap / connect
    // ------------------------------------------------------------------

    pub fn bootstrap(&mut self, cx: &mut gpui::Context<Self>) {
        let kind = self.settings.backend_kind;
        log_debug!(
            "app",
            "bootstrap backend={} enabled={}",
            kind.id(),
            self.settings.is_backend_enabled(kind)
        );
        self.refresh_hermes_model_config();
        self.refresh_permission_mode();
        // Only a backend whose own switch is on connects. A backend the user
        // switched off stays off across launches instead of reconnecting.
        if self.settings.is_backend_enabled(kind) {
            self.connect(cx);
        }
    }

    pub fn connect(&mut self, cx: &mut gpui::Context<Self>) {
        // An explicit refresh can arrive while an earlier reconnect is still
        // probing the backend. Do not stack another probe/stream/session resume
        // on top of it; the in-flight connect owns that work.
        if self.connection_state == ConnectionState::Connecting || self.gateway_connecting {
            return;
        }
        log_debug!("app", "connect requested");
        self.connection_state = ConnectionState::Connecting;
        self.transport_ready = false;
        cx.notify();

        let settings = self.settings.clone();
        let local_server = self.local_server.clone();
        let backend = self.backend.clone();
        let managed_server = ManagedServer::for_kind(settings.backend_kind);
        let port = settings.resolved_port();
        let fallback_url = settings.active_backend_url();
        // The password is needed before the first request, not after it: a
        // protected server answers 401 to a password-less probe, which would
        // read as a connection failure. A server that takes no password ignores
        // the header, so passing a stale one is harmless.
        let credential = if settings.backend_kind.server_takes_password() {
            settings.session_token.trim().to_string()
        } else {
            String::new()
        };
        let workspace = settings.workspace_trimmed();

        let join = tokio_spawn(cx, async move {
            let base_url = match managed_server {
                Some(server) => {
                    match local_server
                        .ensure_running(server, port, workspace.as_deref(), &credential)
                        .await
                    {
                        Ok(url) => url,
                        Err(error) => {
                            return ConnectOutcome {
                                discovered_token: None,
                                error: Some(error.to_string()),
                            };
                        }
                    }
                }
                None => fallback_url.clone(),
            };
            let config = BackendConfig {
                base_url: base_url.clone(),
                credential: credential.clone(),
                profile: settings.normalized_profile(),
                workspace: workspace.clone(),
            };
            if let Err(error) = backend.lock().await.probe(&config).await {
                return ConnectOutcome {
                    discovered_token: None,
                    error: Some(error.to_string()),
                };
            }
            let discovered_token = if settings.backend_kind == BackendKind::Hermes
                && settings.session_token.trim().is_empty()
            {
                match backend.lock().await.discover_credential(&base_url).await {
                    Ok(token) => Some(token),
                    Err(error) => {
                        return ConnectOutcome {
                            discovered_token: None,
                            error: Some(error.to_string()),
                        };
                    }
                }
            } else {
                None
            };
            ConnectOutcome {
                discovered_token,
                error: None,
            }
        });

        cx.spawn(async move |this, cx| {
            let outcome = join.await.unwrap_or(ConnectOutcome {
                discovered_token: None,
                error: Some("connect task failed".into()),
            });
            let _ = this.update(cx, |state, cx| {
                if let Some(error) = outcome.error {
                    state.connection_state = ConnectionState::Failed(error.clone());
                    state.last_error = Some(error);
                    log_debug!("app", "connect failed");
                } else {
                    if let Some(token) = outcome.discovered_token {
                        state.settings.session_token = token;
                        state.settings.save();
                    }
                    state.connect_gateway(cx);
                    state.refresh_sessions(false, cx);
                    if let Some(session) = state.selected_session.clone() {
                        state.resume_session(session, cx);
                    } else {
                        state.start_fresh_chat(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the backend's event stream onto the shared channel.
    fn connect_gateway(&mut self, cx: &mut gpui::Context<Self>) {
        if self.transport_ready || self.gateway_connecting {
            return;
        }
        if self.settings.backend_kind == BackendKind::Hermes
            && self.settings.session_token.trim().is_empty()
        {
            self.connection_state = ConnectionState::Failed("Missing Hermes session token".into());
            return;
        }
        self.gateway_connecting = true;
        let backend = self.backend.clone();
        let config = self.backend_config();
        let event_tx = self.event_tx.clone();
        let join = tokio_spawn(cx, async move {
            backend.lock().await.connect(config, event_tx).await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("connect task failed")));
            let _ = this.update(cx, |state, cx| {
                state.gateway_connecting = false;
                if let Err(error) = result {
                    state.transport_ready = false;
                    state.connection_state = ConnectionState::Failed(error.to_string());
                    state.last_error = Some(error.to_string());
                    log_debug!("app", "gateway connect failed: {error}");
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-read the session list without touching the progress indicator. Used to
    /// pick up names and state the gateway changed behind our back.
    fn refresh_sessions_quietly(&mut self, cx: &mut gpui::Context<Self>) {
        if self.sessions_refresh_inflight {
            return;
        }
        self.refresh_sessions(false, cx);
    }

    pub fn refresh_sessions(&mut self, show_progress: bool, cx: &mut gpui::Context<Self>) {
        // The toolbar/keyboard refresh is also the user's way to recover after
        // the app has slept long enough for its event stream to die. Merely
        // listing sessions is insufficient: some backends can satisfy that
        // request over HTTP, and OpenClaw can lazily open a request-only socket,
        // neither of which restores the event stream that carries replies.
        // A full connect also re-lists sessions and resumes the chat on screen.
        // Silent refreshes are backend hints and must not restart a healthy
        // stream (or turn one disconnect into a retry loop).
        if refresh_needs_reconnect(show_progress, self.transport_ready) {
            log_debug!("app", "explicit refresh reconnecting dead transport");
            self.connect(cx);
            return;
        }
        if show_progress {
            self.is_refreshing_sessions = true;
            cx.notify();
        }
        self.sessions_refresh_inflight = true;
        let backend = self.backend.clone();
        let config = self.backend_config();
        let expects_history = self.backend_caps.contains(BackendCaps::SESSION_HISTORY);
        let join = tokio_spawn(cx, async move {
            let mut guard = backend.lock().await;
            let result = guard.list_sessions(&config).await;
            let id = guard.id();
            (result, id)
        });
        cx.spawn(async move |this, cx| {
            let (result, backend_id) = join
                .await
                .unwrap_or((Err(anyhow::anyhow!("list sessions task failed")), "unknown"));
            let _ = this.update(cx, |state, cx| {
                if show_progress {
                    state.is_refreshing_sessions = false;
                }
                state.sessions_refresh_inflight = false;
                match result {
                    Ok(fetched) => {
                        if expects_history {
                            let fetched: Vec<AgentSession> = fetched
                                .into_iter()
                                .map(|mut session| {
                                    session.backend_id = Some(backend_id.to_string());
                                    if session.archived.is_none() {
                                        session.archived = Some(false);
                                    }
                                    session
                                })
                                .collect();
                            let selected = state.selected_session.clone();
                            state.sessions = state
                                .filter_visible(merge_fetched_sessions(fetched, selected.as_ref()));
                            state.sync_cached_sessions(backend_id);
                            state.update_cache_summary();
                            let cache = state.cache_store.clone();
                            let snapshot = state.cached_state.clone();
                            cache.save(snapshot);
                        }
                    }
                    Err(error) => {
                        state.last_error = Some(error.to_string());
                        if state.connection_state == ConnectionState::Connected {
                            state.connection_state = ConnectionState::Degraded(error.to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn resume_session(&mut self, session: AgentSession, cx: &mut gpui::Context<Self>) {
        let view_generation = self.advance_session_view();
        self.selected_session = Some(session.clone());
        self.pending_clarify = None;
        // The session on screen is not the one this connection is subscribed to
        // until the gateway says so: until then the messages being loaded belong
        // to a subscription that is about to be replaced, and anything arriving
        // is the previous session's. Dropping the live id is what makes
        // `accepts_turn_events` hold events off for that window.
        self.live_gateway_session_id = None;
        self.stored_gateway_session_id = Some(session.id.clone());
        let key = cache_key(&session, self.settings.backend_kind.id());
        self.cached_state.selected_session_id = Some(key.clone());
        if let Some(cached) = self
            .cached_state
            .messages_by_session_id
            .get(&key)
            .cloned()
            .filter(|cached| !cached.is_empty())
        {
            self.conversation
                .set_transcript(Self::visible_messages(&cached));
        } else {
            self.conversation = Conversation::new();
        }
        cx.notify();

        self.load_messages(&session, cx);
        self.connect_gateway(cx);

        let backend = self.backend.clone();
        let config = self.backend_config();
        let session_id = session.id.clone();
        // Kept back for the task that reports the outcome: the id itself is
        // moved into the resume below.
        let resumed_id = session.id.clone();
        let join = tokio_spawn(cx, async move {
            backend
                .lock()
                .await
                .resume_session(&config, &session_id)
                .await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("resume task failed")));
            let _ = this.update(cx, |state, cx| {
                if !session_view_is_current(
                    view_generation,
                    state.session_view_generation,
                    state
                        .selected_session
                        .as_ref()
                        .map(|session| session.id.as_str()),
                    Some(resumed_id.as_str()),
                ) {
                    log_debug!("app", "stale resume result ignored session={resumed_id}");
                    return;
                }
                match result {
                    Ok(ids) => {
                        state.live_gateway_session_id = Some(ids.live_id.clone());
                        state.stored_gateway_session_id =
                            Some(ids.stored_id.unwrap_or_else(|| ids.live_id.clone()));
                        state.transport_ready = true;
                        let cache = state.cache_store.clone();
                        let snapshot = state.cached_state.clone();
                        cache.save(snapshot);
                        // A prompt held for *this* session can go now. It was held
                        // because the connection had no live session to send it to,
                        // not because the user wanted to wait.
                        let belongs_here = state.pending_after_start.as_ref().is_some_and(|held| {
                            held.session.as_deref() == Some(ids.live_id.as_str())
                        });
                        let held = belongs_here
                            .then(|| state.pending_after_start.take())
                            .flatten();
                        if let Some(held) = held {
                            let (text, attachments) = held.into_composer();
                            state.perform_send(text, attachments, ids.live_id.clone(), cx);
                        }
                    }
                    Err(error) => {
                        state.last_error = Some(error.to_string());
                        // Give back a prompt that was waiting for this session, so
                        // nothing the user typed is lost when the resume fails.
                        let belongs_here = state.pending_after_start.as_ref().is_some_and(|held| {
                            held.session.as_deref() == Some(resumed_id.as_str())
                        });
                        let held = belongs_here
                            .then(|| state.pending_after_start.take())
                            .flatten();
                        if let Some(held) = held {
                            let (text, attachments) = held.into_composer();
                            state.composer_text = text;
                            state.composer_attachments = attachments;
                        }
                        log_debug!("app", "resume gateway failed: {error}");
                    }
                }
            });
        })
        .detach();
    }

    pub fn start_fresh_chat(&mut self, cx: &mut gpui::Context<Self>) {
        let view_generation = self.advance_session_view();
        self.selected_session = None;
        self.conversation = Conversation::new();
        self.pending_clarify = None;
        self.live_gateway_session_id = None;
        self.stored_gateway_session_id = None;
        cx.notify();

        self.connect_gateway(cx);
        let backend = self.backend.clone();
        let config = self.backend_config();
        let profile = self.settings.normalized_profile();
        let backend_id = self.backend_id().to_string();
        let join = tokio_spawn(cx, async move {
            let mut backend = backend.lock().await;
            backend.clear_session_scope();
            backend.create_session(&config).await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("create task failed")));
            let _ = this.update(cx, |state, cx| {
                if !session_view_is_current(
                    view_generation,
                    state.session_view_generation,
                    state
                        .selected_session
                        .as_ref()
                        .map(|session| session.id.as_str()),
                    None,
                ) {
                    log_debug!("app", "stale create result ignored");
                    return;
                }
                match result {
                    Ok(ids) => {
                        state.live_gateway_session_id = Some(ids.live_id.clone());
                        state.stored_gateway_session_id = ids.stored_id.clone();
                        state.transport_ready = true;
                        let local_id = ids.stored_id.clone().unwrap_or_else(|| ids.live_id.clone());
                        let now = now_unix();
                        let local_session = AgentSession {
                            id: local_id.clone(),
                            title: Some("New Chat".into()),
                            cwd: None,
                            model: None,
                            provider: None,
                            started_at: Some(now),
                            last_active: Some(now),
                            message_count: Some(0),
                            is_active: Some(true),
                            archived: Some(false),
                            profile: profile.clone(),
                            backend_id: Some(backend_id.clone()),
                            // A chat with nothing in it has nothing to count;
                            // the first refresh after a turn fills these in.
                            used_tokens: None,
                            context_tokens: None,
                        };
                        state.selected_session = Some(local_session.clone());
                        if !state.sessions.iter().any(|session| session.id == local_id) {
                            state.sessions.insert(0, local_session);
                        }
                        state.sync_cached_sessions(&backend_id);
                        state.cached_state.selected_session_id =
                            Some(format!("{}::{local_id}", state.settings.backend_kind.id()));
                        state.update_cache_summary();
                        let cache = state.cache_store.clone();
                        let snapshot = state.cached_state.clone();
                        cache.save(snapshot);
                        log_debug!(
                            "app",
                            "fresh gateway session live={} stored={:?}",
                            ids.live_id,
                            ids.stored_id
                        );
                        // The gateway names a session itself (after the client
                        // that created it); re-read the list so the sidebar
                        // shows that name instead of the local placeholder.
                        state.refresh_sessions_quietly(cx);

                        // Flush a prompt that was held back until a live
                        // session existed — but only one that was waiting for a
                        // *new* chat. A prompt held for a conversation the user
                        // was reading belongs there, not in this new one.
                        let held = state
                            .pending_after_start
                            .as_ref()
                            .is_some_and(|held| held.session.is_none());
                        if held {
                            if let Some(held) = state.pending_after_start.take() {
                                if let Some(session_id) = state.live_gateway_session_id.clone() {
                                    state.composer_text = String::new();
                                    state.composer_attachments = Vec::new();
                                    state.perform_send(held.text, held.attachments, session_id, cx);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        state.connection_state = ConnectionState::Failed(error.to_string());
                        state.last_error = Some(error.to_string());
                        state.transport_ready = false;
                        // Give the held-back prompt back to the composer so
                        // nothing the user typed is lost. One held for another
                        // conversation stays held: that session is still the
                        // one it has to go to.
                        let held = state
                            .pending_after_start
                            .as_ref()
                            .is_some_and(|held| held.session.is_none());
                        if held {
                            if let Some(held) = state.pending_after_start.take() {
                                let (text, attachments) = held.into_composer();
                                state.composer_text = text;
                                state.composer_attachments = attachments;
                            }
                        }
                        log_debug!("app", "session.create failed: {error}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------------
    // Sending
    // ------------------------------------------------------------------

    pub fn send_composer(&mut self, cx: &mut gpui::Context<Self>) {
        let text = self.composer_text.trim().to_string();
        if self.pending_clarify.is_some() {
            if text.is_empty() {
                return;
            }
            self.composer_text = String::new();
            self.composer_attachments = Vec::new();
            self.answer_clarify(text, cx);
            return;
        }
        let attachments = self.composer_attachments.clone();
        if text.is_empty() && attachments.is_empty() {
            return;
        }

        if !self.transport_ready || self.live_gateway_session_id.is_none() {
            // There is no live session to send to. Which one the prompt belongs
            // to decides what happens next, and getting it wrong is invisible:
            // a session created here would take the prompt while the window
            // still shows the conversation the user was reading.
            let existing = self.stored_gateway_session_id.clone().or_else(|| {
                self.selected_session
                    .as_ref()
                    .map(|session| session.id.clone())
            });
            match existing {
                Some(session) => {
                    // The conversation on screen still exists on the backend; it
                    // just is not subscribed to on this connection yet. Wait for
                    // that, and send the prompt there.
                    let session_row = self
                        .selected_session
                        .clone()
                        .or_else(|| self.sessions.iter().find(|row| row.id == session).cloned());
                    match session_row {
                        Some(session_row) => {
                            self.pending_after_start = Some(HeldPrompt {
                                session: Some(session),
                                text,
                                attachments,
                            });
                            log_debug!("app", "no live session on send; resuming the open session");
                            self.resume_session(session_row, cx);
                        }
                        None => {
                            self.composer_text = text;
                            self.composer_attachments = attachments;
                            self.last_error = Some("That session is no longer in the list.".into());
                        }
                    }
                    cx.notify();
                    return;
                }
                None => {
                    // No session on screen at all: a new chat, which does need
                    // one created. Keep the composer content visible until the
                    // prompt actually submits, so a failed session setup never
                    // eats the user's text.
                    self.pending_after_start = Some(HeldPrompt {
                        session: None,
                        text,
                        attachments,
                    });
                    log_debug!("app", "no live session on send; creating fresh chat first");
                    self.start_fresh_chat(cx);
                    return;
                }
            }
        }

        if self.is_sending() {
            self.pending_queue
                .push(QueuedPrompt::new(text, attachments));
            self.composer_text = String::new();
            self.composer_attachments = Vec::new();
            log_debug!(
                "app",
                "queued pending prompt queue={}",
                self.pending_queue.len()
            );
            cx.notify();
            return;
        }

        self.composer_text = String::new();
        self.composer_attachments = Vec::new();
        let session_id = self.live_gateway_session_id.clone().unwrap();
        self.perform_send(text, attachments, session_id, cx);
    }

    fn perform_send(
        &mut self,
        text: String,
        attachments: Vec<ComposerAttachment>,
        session_id: String,
        cx: &mut gpui::Context<Self>,
    ) {
        let submitted = submitted_prompt(&text, &attachments);
        self.conversation.begin_turn_with_placeholder(&text);
        self.schedule_current_messages_cache_save(cx);
        cx.notify();

        let backend = self.backend.clone();
        let join = tokio_spawn(cx, async move {
            backend
                .lock()
                .await
                .submit_prompt(&session_id, &submitted)
                .await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("submit task failed")));
            let _ = this.update(cx, |state, cx| {
                if let Err(error) = result {
                    state.last_error = Some(error.to_string());
                    state.conversation.finish_turn();
                    log_debug!("app", "prompt.submit failed: {error}");
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn interrupt_running(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(session_id) = self.live_gateway_session_id.clone() else {
            return;
        };
        if !self.is_sending() {
            return;
        }
        log_debug!("queue", "interrupt session={session_id}");
        let backend = self.backend.clone();
        let join = tokio_spawn(cx, async move {
            backend.lock().await.interrupt(&session_id).await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("interrupt task failed")));
            let _ = this.update(cx, |state, _cx| {
                if let Err(error) = result {
                    state.last_error = Some(format!("Could not interrupt: {error}"));
                    log_debug!("queue", "interrupt failed: {error}");
                }
            });
        })
        .detach();
    }

    pub fn answer_clarify(&mut self, answer: String, cx: &mut gpui::Context<Self>) {
        let Some(clarify) = self.pending_clarify.clone() else {
            return;
        };
        let trimmed = answer.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.pending_clarify = None;

        self.conversation.finish_turn();
        self.conversation.begin_turn_with_placeholder(&trimmed);
        self.schedule_current_messages_cache_save(cx);
        log_debug!(
            "queue",
            "clarify respond id={} chars={}",
            clarify.request_id,
            trimmed.len()
        );
        cx.notify();

        let backend = self.backend.clone();
        let request_id = clarify.request_id.clone();
        let join = tokio_spawn(cx, async move {
            backend
                .lock()
                .await
                .respond_to_interaction(&request_id, &trimmed)
                .await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("clarify task failed")));
            let _ = this.update(cx, |state, _cx| {
                if let Err(error) = result {
                    state.last_error = Some(format!("Could not answer: {error}"));
                    state.conversation.finish_turn();
                    log_debug!("queue", "clarify.respond failed: {error}");
                }
            });
        })
        .detach();
    }

    pub fn dismiss_clarify(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(clarify) = self.pending_clarify.clone() else {
            return;
        };
        self.pending_clarify = None;
        log_debug!("queue", "clarify dismiss id={}", clarify.request_id);
        cx.notify();

        let backend = self.backend.clone();
        let request_id = clarify.request_id.clone();
        let join = tokio_spawn(cx, async move {
            backend
                .lock()
                .await
                .respond_to_interaction(&request_id, "")
                .await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("dismiss task failed")));
            let _ = this.update(cx, |state, _cx| {
                if let Err(error) = result {
                    state.last_error = Some(format!("Could not dismiss clarify: {error}"));
                    log_debug!("queue", "clarify dismiss failed: {error}");
                }
            });
        })
        .detach();
    }

    pub fn send_queued_now(&mut self, id: &str, cx: &mut gpui::Context<Self>) {
        let Some(index) = self.pending_queue.iter().position(|item| item.id == id) else {
            return;
        };
        let item = self.pending_queue.remove(index);
        let Some(session_id) = self.live_gateway_session_id.clone() else {
            self.last_error = Some(format!(
                "{} session is not ready yet.",
                self.backend_display_name
            ));
            cx.notify();
            return;
        };
        if self.is_sending() {
            let backend = self.backend.clone();
            let join = tokio_spawn(cx, async move {
                backend.lock().await.interrupt(&session_id).await
            });
            let text = item.text.clone();
            let attachments = item.attachments.clone();
            cx.spawn(async move |this, cx| {
                let result = join
                    .await
                    .unwrap_or(Err(anyhow::anyhow!("interrupt task failed")));
                let _ = this.update(cx, |state, cx| {
                    if let Err(error) = result {
                        state.last_error = Some(format!(
                            "Could not interrupt before sending queued: {error}"
                        ));
                        return;
                    }
                    if let Some(session_id) = state.live_gateway_session_id.clone() {
                        state.perform_send(text, attachments, session_id, cx);
                    }
                });
            })
            .detach();
        } else {
            let session_id = self.live_gateway_session_id.clone().unwrap();
            self.perform_send(item.text, item.attachments, session_id, cx);
        }
    }

    pub fn cancel_queued(&mut self, id: &str, cx: &mut gpui::Context<Self>) {
        self.pending_queue.retain(|item| item.id != id);
        log_debug!("queue", "cancel queued queue={}", self.pending_queue.len());
        cx.notify();
    }

    pub fn edit_queued(&mut self, id: &str, cx: &mut gpui::Context<Self>) {
        let Some(index) = self.pending_queue.iter().position(|item| item.id == id) else {
            return;
        };
        let item = self.pending_queue.remove(index);
        self.composer_text = item.text;
        self.composer_attachments = item.attachments;
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Attachments
    // ------------------------------------------------------------------

    pub fn add_attachments(&mut self, paths: Vec<String>, cx: &mut gpui::Context<Self>) {
        let requested = paths.len();
        let mut added = 0;
        for path in paths {
            let path = path.trim().to_string();
            if path.is_empty() {
                continue;
            }
            if self
                .composer_attachments
                .iter()
                .any(|attachment| attachment.path == path)
            {
                continue;
            }
            self.composer_attachments
                .push(ComposerAttachment::new(path));
            added += 1;
        }
        log_debug!(
            "composer",
            "attachments requested={requested} added={added} total={}",
            self.composer_attachments.len()
        );
        cx.notify();
    }

    pub fn remove_attachment(&mut self, id: &str, cx: &mut gpui::Context<Self>) {
        self.composer_attachments
            .retain(|attachment| attachment.id != id);
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Hermes model + permission management
    // ------------------------------------------------------------------

    pub fn refresh_hermes_model_config(&mut self) {
        let (provider, model) = hermes_config::read_model_config();
        self.current_model_provider = provider.clone();
        self.current_model_name = model.clone();
        self.available_models = hermes_config::read_model_options(&provider, &model);
        self.context_window_tokens = hermes_config::read_context_window_tokens(&provider, &model);
        self.rebuild_model_provider_groups();
    }

    fn rebuild_model_provider_groups(&mut self) {
        let current_provider = self.current_model_provider.clone();
        let current_model = self.current_model_name.clone();
        let mut providers: Vec<String> = Vec::new();
        for option in &self.available_models {
            if !providers.contains(&option.provider) {
                providers.push(option.provider.clone());
            }
        }
        providers.sort_by(|lhs, rhs| provider_order(lhs, rhs, &current_provider));
        self.model_provider_groups = providers
            .into_iter()
            .map(|provider| {
                let mut models: Vec<ModelOption> = self
                    .available_models
                    .iter()
                    .filter(|option| option.provider == provider)
                    .cloned()
                    .collect();
                models.sort_by(|lhs, rhs| model_order(&lhs.model, &rhs.model, &current_model));
                (provider, models)
            })
            .collect();
    }

    pub fn select_hermes_model(&mut self, option: ModelOption, cx: &mut gpui::Context<Self>) {
        if self.is_switching_model {
            return;
        }
        self.is_switching_model = true;
        cx.notify();
        let option_for_spawn = option.clone();
        let join = tokio_spawn(cx, async move {
            tokio::task::spawn_blocking(move || hermes_config::select_model(&option_for_spawn))
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))
                .and_then(|result| result)
        });
        let provider = option.provider.clone();
        let model = option.model.clone();
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("model switch failed")));
            let _ = this.update(cx, |state, cx| {
                match result {
                    Ok(()) => {
                        state.current_model_provider = provider.clone();
                        state.current_model_name = model.clone();
                        state.refresh_hermes_model_config();
                        state.is_switching_model = false;
                        log_debug!("app", "model switched provider={provider} model={model}");
                    }
                    Err(error) => {
                        state.last_error = Some(format!("Could not switch Hermes model: {error}"));
                        state.is_switching_model = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn refresh_permission_mode(&mut self) {
        self.permission_mode = hermes_config::read_permission_mode();
    }

    pub fn set_permission_mode(&mut self, mode: PermissionMode, cx: &mut gpui::Context<Self>) {
        if self.is_changing_permission_mode {
            return;
        }
        self.is_changing_permission_mode = true;
        cx.notify();
        let join = tokio_spawn(cx, async move {
            tokio::task::spawn_blocking(move || hermes_config::set_permission_mode(mode))
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))
                .and_then(|result| result)
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("permission change failed")));
            let _ = this.update(cx, |state, cx| {
                match result {
                    Ok(()) => {
                        state.permission_mode = mode;
                        state.is_changing_permission_mode = false;
                        log_debug!("app", "permission mode set {mode:?}");
                    }
                    Err(error) => {
                        state.last_error =
                            Some(format!("Could not change Hermes permissions: {error}"));
                        state.is_changing_permission_mode = false;
                        state.refresh_permission_mode();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ------------------------------------------------------------------
    // Session management
    // ------------------------------------------------------------------

    pub fn archive_session(&mut self, session: &AgentSession, cx: &mut gpui::Context<Self>) {
        log_debug!("cache", "archive local session id={}", session.id);
        self.cached_state
            .archived_session_ids
            .insert(cache_key(session, self.settings.backend_kind.id()));
        self.remove_visible_session(&session.id, false, cx);
    }

    pub fn delete_session(&mut self, session: &AgentSession, cx: &mut gpui::Context<Self>) {
        log_debug!("cache", "delete local session id={}", session.id);
        self.cached_state
            .deleted_session_ids
            .insert(cache_key(session, self.settings.backend_kind.id()));
        self.remove_visible_session(&session.id, true, cx);
    }

    pub fn archive_all(&mut self, cx: &mut gpui::Context<Self>) {
        let ids: Vec<String> = self
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        log_debug!("cache", "archive all local sessions count={}", ids.len());
        for id in &ids {
            self.cached_state
                .archived_session_ids
                .insert(format!("{}::{id}", self.settings.backend_kind.id()));
        }
        self.remove_visible_sessions(&ids.into_iter().collect(), false, cx);
    }

    pub fn delete_all(&mut self, cx: &mut gpui::Context<Self>) {
        let ids: Vec<String> = self
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        log_debug!("cache", "delete all local sessions count={}", ids.len());
        for id in &ids {
            self.cached_state
                .deleted_session_ids
                .insert(format!("{}::{id}", self.settings.backend_kind.id()));
        }
        self.remove_visible_sessions(&ids.into_iter().collect(), true, cx);
    }

    pub fn clear_cache(&mut self, cx: &mut gpui::Context<Self>) {
        self.advance_session_view();
        self.cached_state = CachedState::default();
        self.cache_store.clear();
        self.sessions = Vec::new();
        self.selected_session = None;
        self.conversation = Conversation::new();
        self.update_cache_summary();
        cx.notify();
    }

    /// Make `kind` the active backend. Choosing a backend means "use this one",
    /// so its switch is turned on — which turns every other backend off, since
    /// only one can be connected at a time.
    pub fn switch_backend(&mut self, kind: BackendKind, cx: &mut gpui::Context<Self>) {
        self.advance_session_view();
        self.settings.set_backend_enabled(kind, true);
        let backend = self.backend.clone();
        tokio_spawn(cx, async move {
            backend.lock().await.disconnect();
        });
        self.settings.switch_backend(kind);
        self.settings.save();
        self.backend = Arc::new(AsyncMutex::new(Backend::make(kind)));
        self.backend_id = backend_static_id(kind);
        self.backend_display_name = backend_static_name(kind);
        self.backend_caps = self.backend_caps_for(kind);
        self.transport_ready = false;
        self.gateway_connecting = false;
        self.connection_state = ConnectionState::Disconnected;
        self.selected_session = None;
        let cached_sessions = self.cached_state.sessions.clone();
        self.sessions =
            Self::visible_sessions_from(&cached_sessions, kind.id(), &self.cached_state);
        self.conversation = Conversation::new();
        self.live_gateway_session_id = None;
        self.stored_gateway_session_id = None;
        self.pending_clarify = None;
        self.last_error = None;
        log_debug!("app", "backend switched to={}", kind.id());
        cx.notify();
        if self.settings.is_backend_enabled(kind) {
            self.connect(cx);
        }
    }

    /// Turn a backend's switch on, switching to it if it is not already active.
    pub fn enable_backend(&mut self, kind: BackendKind, cx: &mut gpui::Context<Self>) {
        if self.settings.backend_kind == kind && self.settings.is_backend_enabled(kind) {
            // Already the active backend: this is a plain reconnect.
            self.connect(cx);
            return;
        }
        self.switch_backend(kind, cx);
    }

    /// Turn a backend's switch off. Disconnecting the active backend leaves the
    /// app disconnected until the user turns a backend back on, and the choice
    /// survives a restart.
    pub fn disable_backend(&mut self, kind: BackendKind, cx: &mut gpui::Context<Self>) {
        self.settings.set_backend_enabled(kind, false);
        self.settings.save();
        log_debug!("app", "backend disabled id={}", kind.id());
        if self.settings.backend_kind == kind {
            self.disconnect(cx);
        }
        cx.notify();
    }

    pub fn disconnect(&mut self, cx: &mut gpui::Context<Self>) {
        log_debug!("app", "disconnect requested");
        self.advance_session_view();
        self.cache_save_task = None;
        let backend = self.backend.clone();
        tokio_spawn(cx, async move {
            backend.lock().await.disconnect();
        });
        self.transport_ready = false;
        self.gateway_connecting = false;
        self.connection_state = ConnectionState::Disconnected;
        cx.notify();
    }

    pub fn stop_managed_local(&mut self, cx: &mut gpui::Context<Self>) {
        self.local_server.stop();
        cx.notify();
    }

    /// Put the managed server into the directory the settings now name.
    ///
    /// MiMoCode is not OpenClaw: its project *is* a working directory, and the
    /// server picks it up once, when it starts. Changing the setting therefore
    /// means starting the server again — nothing else re-reads it — so this is
    /// what a committed Workspace field does. Only a server this app started is
    /// moved; one the user started keeps its directory and is left alone.
    pub fn apply_workspace(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(server) = ManagedServer::for_kind(self.settings.backend_kind) else {
            return;
        };
        if !self.settings.is_managed_local_backend() || !self.local_server.is_managing() {
            return;
        }
        // A backend that is switched off gets no server from this: the setting
        // is used the next time it is switched on.
        if !self.settings.is_backend_enabled(self.settings.backend_kind) {
            return;
        }
        let workspace = self.settings.workspace_trimmed();
        if self.local_server.started_in() == workspace {
            return;
        }
        // A turn in flight belongs to the directory being left. Say why nothing
        // moved rather than cutting the reply off, and let the next connect do
        // it — the field and the message below will disagree until then.
        if self.is_sending() {
            self.local_server.set_message(format!(
                "{} is still running in {}; a reply is being written, so the workspace applies at the next connect.",
                server.label(),
                self.local_server.started_in().unwrap_or_else(|| "the app's own directory".into())
            ));
            cx.notify();
            return;
        }

        log_debug!("app", "workspace changed; restarting the managed server");
        let port = self.settings.resolved_port();
        let credential = self.settings.session_token.trim().to_string();
        let local_server = self.local_server.clone();
        let join = tokio_spawn(cx, async move {
            local_server
                .restart_in(server, port, workspace.as_deref(), &credential)
                .await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("restart task failed")));
            let _ = this.update(cx, |state, cx| {
                if let Err(error) = result {
                    state.last_error = Some(error.to_string());
                    log_debug!("app", "restarting the managed server failed");
                }
                // The connection was attached to the server that just stopped,
                // so it is re-established here rather than left pointing at it.
                state.connect(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn backend_caps_for(&self, kind: BackendKind) -> BackendCaps {
        Backend::make(kind).capabilities()
    }

    // ------------------------------------------------------------------
    // Event handling (streaming core)
    // ------------------------------------------------------------------

    pub fn handle_event(&mut self, event: AgentEvent, cx: &mut gpui::Context<Self>) {
        // A turn's traffic is only the open transcript's while a session is
        // subscribed. During a switch the session being left is still streaming
        // into this connection, and its reply would be written into the chat the
        // user just opened — see `accepts_turn_events`.
        if matches!(
            &event,
            AgentEvent::MessageStart
                | AgentEvent::MessageDelta { .. }
                | AgentEvent::MessageComplete(_)
                | AgentEvent::Tool(_)
                | AgentEvent::TurnFailed(_)
        ) && !accepts_turn_events(
            self.live_gateway_session_id.as_deref(),
            self.stored_gateway_session_id.as_deref(),
        ) {
            log_debug!("app", "turn event dropped while a session is opening");
            return;
        }

        // Everything that is not a turn event is session/connection state this
        // struct owns; the turn events go to the shared fold in `Conversation`
        // and only the change it reports is orchestrated here.
        match &event {
            AgentEvent::Connected => {
                self.transport_ready = true;
                self.gateway_connecting = false;
                self.connection_state = ConnectionState::Connected;
                self.last_error = None;
                cx.notify();
            }
            AgentEvent::SessionInfo(session_id) => {
                self.live_gateway_session_id = Some(session_id.clone());
                self.transport_ready = true;
                log_debug!("app", "session info live={session_id}");
                cx.notify();
            }
            AgentEvent::SessionsChanged => self.refresh_sessions_quietly(cx),
            AgentEvent::Clarify {
                question,
                choices,
                request_id,
                session_id,
            } => {
                log_debug!(
                    "gateway",
                    "clarify request id={request_id} choices={}",
                    choices.len()
                );
                self.pending_clarify = Some(PendingClarify {
                    session_id: session_id.clone(),
                    question: question.clone(),
                    choices: choices.clone(),
                    request_id: request_id.clone(),
                });
                cx.notify();
            }
            AgentEvent::Disconnected => {
                self.transport_ready = false;
                self.gateway_connecting = false;
                self.pending_clarify = None;
                if self.connection_state == ConnectionState::Connected {
                    self.connection_state = ConnectionState::Disconnected;
                }
                cx.notify();
            }
            AgentEvent::Failed(message) => {
                self.transport_ready = false;
                self.gateway_connecting = false;
                self.pending_clarify = None;
                self.connection_state = ConnectionState::Failed(message.clone());
                self.last_error = Some(message.clone());
                cx.notify();
            }
            AgentEvent::TurnFailed(message) => {
                self.last_error = Some(message.clone());
                // The fold would end the turn silently; the desktop reports the
                // failure in the transcript, so route it through the completion
                // path with the error as the final text.
                self.conversation
                    .complete_message(Some(&format!("Error: {message}")));
                self.after_turn_finished(cx);
            }
            turn_event => {
                let change = self.conversation.apply(turn_event);
                match change {
                    ConversationChange::None => {}
                    ConversationChange::Streaming { .. } => {
                        self.schedule_current_messages_cache_save(cx);
                        self.notify_streaming(cx);
                    }
                    ConversationChange::Structure => {
                        self.schedule_current_messages_cache_save(cx);
                        cx.notify();
                    }
                }
            }
        }
    }

    /// The turn has ended: everything the end of a turn means beyond the
    /// messages themselves. The pending clarify is stale, the transcript is
    /// worth persisting, and two things only the backend knows have changed:
    /// the name a gateway gives a chat from its first prompt, and the session's
    /// context accounting — which a turn has just changed.
    fn after_turn_finished(&mut self, cx: &mut gpui::Context<Self>) {
        self.pending_clarify = None;
        self.save_current_messages_to_cache();
        self.refresh_sessions_quietly(cx);

        // Auto-dequeue the next waiting prompt.
        if let Some(next) = self.pending_queue.first().cloned() {
            self.pending_queue.remove(0);
            log_debug!(
                "queue",
                "auto dequeue queued queue={}",
                self.pending_queue.len()
            );
            if let Some(session_id) = self.live_gateway_session_id.clone() {
                self.perform_send(next.text, next.attachments, session_id, cx);
            }
        }
        cx.notify();
    }

    /// A streaming delta only moved text inside a bubble that is already on
    /// screen. Redrawing on every delta repaints the whole window for a change
    /// that is itself produced many times a second, so these are coalesced:
    /// one scheduled repaint carries the text to wherever it has got to, the
    /// way the queue that feeds the mobile client folds its deltas.
    fn notify_streaming(&mut self, cx: &mut gpui::Context<Self>) {
        if self.stream_notify_task.is_some() {
            return;
        }
        self.stream_notify_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            let _ = this.update(cx, |state, cx| {
                state.stream_notify_task = None;
                cx.notify();
            });
        }));
    }

    // ------------------------------------------------------------------
    // Cache
    // ------------------------------------------------------------------

    fn schedule_current_messages_cache_save(&mut self, cx: &mut gpui::Context<Self>) {
        self.cache_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(900))
                .await;
            let _ = this.update(cx, |state, _cx| {
                state.save_current_messages_to_cache();
            });
        }));
    }

    fn save_current_messages_to_cache(&mut self) {
        let key = self
            .selected_session
            .as_ref()
            .map(|session| cache_key(session, self.settings.backend_kind.id()))
            .or_else(|| self.stored_gateway_session_id.clone())
            .or_else(|| self.live_gateway_session_id.clone());
        let Some(key) = key else {
            return;
        };
        self.cached_state
            .messages_by_session_id
            .insert(key.clone(), self.conversation.messages().to_vec());
        self.cached_state.selected_session_id = Some(key);
        self.cached_state.updated_at = Some(now_unix());
        self.update_cache_summary();
        let cache = self.cache_store.clone();
        let snapshot = self.cached_state.clone();
        cache.save(snapshot);
    }

    fn update_cache_summary(&mut self) {
        self.cache_summary = format!(
            "{} cached session(s), {} cached transcript(s)",
            self.cached_state.sessions.len(),
            self.cached_state.messages_by_session_id.len()
        );
    }

    fn sync_cached_sessions(&mut self, backend_id: &str) {
        self.cached_state
            .sessions
            .retain(|session| session.backend_id.as_deref().unwrap_or("hermes") != backend_id);
        let current = self.sessions.iter().cloned().collect::<Vec<_>>();
        self.cached_state.sessions.extend(current);
        self.cached_state.updated_at = Some(now_unix());
    }

    fn filter_visible(&self, sessions: Vec<AgentSession>) -> Vec<AgentSession> {
        sessions
            .into_iter()
            .filter(|session| {
                let owner = session.backend_id.as_deref().unwrap_or("hermes");
                let key = cache_key(session, self.settings.backend_kind.id());
                owner == self.settings.backend_kind.id()
                    && session.archived != Some(true)
                    && !self.cached_state.archived_session_ids.contains(&key)
                    && !self.cached_state.deleted_session_ids.contains(&key)
            })
            .collect()
    }

    fn visible_sessions_from(
        sessions: &[AgentSession],
        backend_id: &str,
        cached_state: &CachedState,
    ) -> Vec<AgentSession> {
        sessions
            .iter()
            .filter(|session| {
                let owner = session.backend_id.as_deref().unwrap_or("hermes");
                let key = format!("{backend_id}::{}", session.id);
                owner == backend_id
                    && session.archived != Some(true)
                    && !cached_state.archived_session_ids.contains(&key)
                    && !cached_state.deleted_session_ids.contains(&key)
            })
            .cloned()
            .collect()
    }

    fn visible_messages(messages: &[ChatMessage]) -> Vec<ChatMessage> {
        messages
            .iter()
            .filter_map(|message| {
                if message.role == MessageRole::Tool || looks_like_tool_payload(&message.content) {
                    return None;
                }
                let mut cleaned = message.clone();
                if cleaned.is_streaming && now_unix() - cleaned.timestamp > 300.0 {
                    cleaned.is_streaming = false;
                    cleaned.completed_at = Some(
                        cleaned
                            .tool_calls
                            .last()
                            .map(|call| call.timestamp)
                            .unwrap_or(cleaned.timestamp),
                    );
                }
                if cleaned.is_empty_shell() {
                    return None;
                }
                if !cleaned.is_streaming
                    && cleaned.completed_at.is_none()
                    && (!cleaned.tool_calls.is_empty() || cleaned.content.is_empty())
                {
                    cleaned.completed_at = Some(
                        cleaned
                            .tool_calls
                            .last()
                            .map(|call| call.timestamp)
                            .unwrap_or(cleaned.timestamp),
                    );
                }
                Some(cleaned)
            })
            .collect()
    }

    fn remove_visible_session(
        &mut self,
        id: &str,
        delete_messages: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        self.sessions.retain(|session| session.id != id);
        self.sync_cached_sessions(self.backend_id);
        if delete_messages {
            let key = format!("{}::{id}", self.settings.backend_kind.id());
            self.cached_state.messages_by_session_id.remove(&key);
        }
        if self
            .selected_session
            .as_ref()
            .map(|session| session.id.as_str())
            == Some(id)
        {
            self.selected_session = None;
            self.conversation = Conversation::new();
            self.live_gateway_session_id = None;
            self.stored_gateway_session_id = None;
            self.transport_ready = false;
        }
        self.update_cache_summary();
        let cache = self.cache_store.clone();
        let snapshot = self.cached_state.clone();
        cache.save(snapshot);
        cx.notify();
    }

    fn remove_visible_sessions(
        &mut self,
        ids: &HashSet<String>,
        delete_messages: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        if ids.is_empty() {
            return;
        }
        self.sessions.retain(|session| !ids.contains(&session.id));
        self.sync_cached_sessions(self.backend_id);
        if delete_messages {
            for id in ids {
                let key = format!("{}::{id}", self.settings.backend_kind.id());
                self.cached_state.messages_by_session_id.remove(&key);
            }
        }
        if let Some(selected) = self
            .selected_session
            .as_ref()
            .map(|session| session.id.clone())
        {
            if ids.contains(&selected) {
                self.advance_session_view();
                self.selected_session = None;
                self.conversation = Conversation::new();
                self.live_gateway_session_id = None;
                self.stored_gateway_session_id = None;
                self.transport_ready = false;
            }
        }
        self.update_cache_summary();
        let cache = self.cache_store.clone();
        let snapshot = self.cached_state.clone();
        cache.save(snapshot);
        cx.notify();
    }

    pub fn load_messages(&mut self, session: &AgentSession, cx: &mut gpui::Context<Self>) {
        let backend = self.backend.clone();
        let config = self.backend_config();
        let session_id = session.id.clone();
        let backend_id = self.settings.backend_kind.id().to_string();
        let session_key = session_id.clone();
        let view_generation = self.session_view_generation;
        let join = tokio_spawn(cx, async move {
            backend.lock().await.messages(&config, &session_id).await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("messages task failed")));
            let _ = this.update(cx, |state, cx| {
                if !session_view_is_current(
                    view_generation,
                    state.session_view_generation,
                    state
                        .selected_session
                        .as_ref()
                        .map(|session| session.id.as_str()),
                    Some(session_key.as_str()),
                ) {
                    log_debug!("app", "stale history result ignored session={session_key}");
                    return;
                }
                match result {
                    Ok(fetched) => {
                        let visible = AppState::visible_messages(&fetched);
                        let key = format!("{backend_id}::{session_key}");
                        // Do not clobber messages mid-stream; WS deltas own the
                        // current bubble until completion.
                        if state
                            .conversation
                            .messages()
                            .last()
                            .map(|last| last.is_streaming)
                            .unwrap_or(false)
                        {
                            state
                                .cached_state
                                .messages_by_session_id
                                .insert(key, visible);
                            state.update_cache_summary();
                            let cache = state.cache_store.clone();
                            let snapshot = state.cached_state.clone();
                            cache.save(snapshot);
                            return;
                        }
                        // The backend reports a turn as text alone, so the two
                        // lists are merged rather than substituted: what this
                        // client watched happen — the tool calls — survives the
                        // reload (see `Conversation::merge_transcript`).
                        state.conversation.merge_transcript(visible.clone());
                        state
                            .cached_state
                            .messages_by_session_id
                            .insert(key.clone(), visible);
                        state.cached_state.selected_session_id = Some(key);
                        state.update_cache_summary();
                        let cache = state.cache_store.clone();
                        let snapshot = state.cached_state.clone();
                        cache.save(snapshot);
                    }
                    Err(error) => {
                        state.last_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Invalidate every async result belonging to the conversation previously
    /// on screen and return the token for the new one.
    fn advance_session_view(&mut self) -> u64 {
        self.session_view_generation = self.session_view_generation.wrapping_add(1);
        self.session_view_generation
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn cache_key(session: &AgentSession, backend_id: &str) -> String {
    format!("{backend_id}::{}", session.id)
}

fn session_view_is_current(
    operation_generation: u64,
    current_generation: u64,
    selected_session: Option<&str>,
    expected_session: Option<&str>,
) -> bool {
    operation_generation == current_generation && selected_session == expected_session
}

fn backend_static_id(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Hermes => "hermes",
        BackendKind::OpenCode => "opencode",
        BackendKind::MiMoCode => "mimocode",
        BackendKind::Codex => "codex",
        BackendKind::ClaudeCode => "claudecode",
        BackendKind::Pi => "pi",
        BackendKind::OpenClaw => "openclaw",
    }
}

fn backend_static_name(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Hermes => "Hermes",
        BackendKind::OpenCode => "OpenCode",
        BackendKind::MiMoCode => "MiMoCode",
        BackendKind::Codex => "Codex CLI",
        BackendKind::ClaudeCode => "Claude Code",
        BackendKind::Pi => "Pi",
        BackendKind::OpenClaw => "OpenClaw",
    }
}

fn submitted_prompt(text: &str, attachments: &[ComposerAttachment]) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    let attachment_text = attachments
        .iter()
        .map(|attachment| format!("- @{}", attachment.path))
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        format!("Attached files:\n{attachment_text}")
    } else {
        format!("{text}\n\nAttached files:\n{attachment_text}")
    }
}

fn looks_like_tool_payload(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("{\"output\":")
        || trimmed.starts_with("{\"error\":")
        || trimmed.starts_with("{\"exit_code\":")
}

fn provider_order(lhs: &str, rhs: &str, current: &str) -> std::cmp::Ordering {
    if lhs == current && rhs != current {
        return std::cmp::Ordering::Less;
    }
    if rhs == current && lhs != current {
        return std::cmp::Ordering::Greater;
    }
    lhs.to_lowercase().cmp(&rhs.to_lowercase())
}

fn model_order(lhs: &str, rhs: &str, current: &str) -> std::cmp::Ordering {
    if lhs == current && rhs != current {
        return std::cmp::Ordering::Less;
    }
    if rhs == current && lhs != current {
        return std::cmp::Ordering::Greater;
    }
    lhs.to_lowercase().cmp(&rhs.to_lowercase())
}

/// The context accounting the backend reported for the session on screen, if it
/// reported both halves of it.
///
/// The row is looked up in `sessions` rather than read off `selected`: a refresh
/// replaces the listed rows with the backend's, and the selected session is a
/// clone taken when it was opened, so reading that one would report the
/// conversation as it stood before the turn that just ran — the very thing the
/// number is there to answer. Both halves are required together, because a
/// measured "used" over an estimated window would mislabel one of the two.
fn reported_context_usage(
    selected: Option<&AgentSession>,
    sessions: &[AgentSession],
) -> Option<ContextUsage> {
    let selected = selected?;
    let row = sessions
        .iter()
        .find(|session| session.id == selected.id)
        .unwrap_or(selected);
    let used = row.used_tokens?;
    let max = row.context_tokens.filter(|max| *max > 0)?;
    Some(ContextUsage::measured(used, max))
}

/// Whether an event that belongs to a turn may be folded into the messages on
/// screen.
///
/// A connection is subscribed to one session at a time, and a gateway carries
/// every session down it: without this, the reply of the session being left is
/// written into the transcript the user just opened. The question the rule
/// answers is "does anything on screen belong to the session this connection is
/// subscribed to?":
///
/// * **A live session** — yes, the events are its own.
/// * **No session at all** — a chat that has not been created yet has nothing to
///   mismatch, so its first turn must not be held off.
/// * **A session being opened** — the stored id is known but the subscription is
///   not confirmed, which is exactly the window where the previous session's
///   traffic arrives. Held off until the gateway answers.
fn accepts_turn_events(live: Option<&str>, stored: Option<&str>) -> bool {
    live.is_some() || stored.is_none()
}

/// A user refresh means "make this view current", including restoring a dead
/// transport. Backend-originated quiet refreshes only update session metadata.
fn refresh_needs_reconnect(show_progress: bool, transport_ready: bool) -> bool {
    show_progress && !transport_ready
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscribed_session_takes_its_own_traffic() {
        assert!(accepts_turn_events(Some("live-1"), Some("stored-1")));
    }

    /// The window a switch opens: the id on screen is known, the subscription is
    /// not established yet, and anything arriving belongs to the session being
    /// left.
    #[test]
    fn a_session_being_opened_holds_traffic_off() {
        assert!(!accepts_turn_events(None, Some("stored-2")));
    }

    /// A chat that does not exist yet has nothing to mismatch: holding its first
    /// turn off would leave a fresh chat that never answers.
    #[test]
    fn a_chat_with_no_session_yet_takes_its_first_turn() {
        assert!(accepts_turn_events(None, None));
    }

    /// The live id alone is enough: the stored key is not always known.
    #[test]
    fn a_live_session_without_a_stored_key_still_takes_its_traffic() {
        assert!(accepts_turn_events(Some("live-1"), None));
    }

    #[test]
    fn an_async_session_result_only_mutates_the_view_that_started_it() {
        assert!(session_view_is_current(7, 7, Some("a"), Some("a")));
        assert!(!session_view_is_current(7, 8, Some("b"), Some("a")));
        assert!(!session_view_is_current(7, 7, None, Some("a")));
        assert!(session_view_is_current(9, 9, None, None));
    }

    #[test]
    fn an_explicit_refresh_restores_a_dead_transport() {
        assert!(refresh_needs_reconnect(true, false));
        assert!(!refresh_needs_reconnect(true, true));
    }

    #[test]
    fn a_quiet_refresh_never_starts_a_reconnect() {
        assert!(!refresh_needs_reconnect(false, false));
    }

    fn session_row(id: &str, used: Option<i64>, window: Option<i64>) -> AgentSession {
        AgentSession {
            id: id.to_string(),
            used_tokens: used,
            context_tokens: window,
            ..AgentSession::default()
        }
    }

    /// What the backend reported is what the status bar shows: a Gateway says
    /// what the context is holding and how wide the window is, and neither the
    /// count nor the width should be this client's guess when it can be a
    /// reading.
    #[test]
    fn a_reported_reading_is_used_as_it_stands() {
        let selected = session_row("a", None, None);
        let rows = vec![session_row("a", Some(36_393), Some(1_048_576))];
        let usage = reported_context_usage(Some(&selected), &rows).expect("reported");
        assert!(usage.measured);
        assert_eq!(usage.used_tokens, 36_393);
        assert_eq!(usage.max_tokens, 1_048_576);
    }

    /// The listed row is the fresh one. A turn just ran, the list was re-read,
    /// and the copy the session was opened with is older than the reading.
    #[test]
    fn the_listed_row_wins_over_the_copy_the_session_was_opened_with() {
        let selected = session_row("a", Some(1), Some(100));
        let rows = vec![session_row("a", Some(9_000), Some(1_048_576))];
        let usage = reported_context_usage(Some(&selected), &rows).expect("reported");
        assert_eq!(usage.used_tokens, 9_000);
        assert_eq!(usage.max_tokens, 1_048_576);
    }

    /// A backend that reports nothing leaves the frontends to estimate, and the
    /// estimate has to be marked as one — the difference between a reading and
    /// a guess is the whole reason the flag exists.
    #[test]
    fn a_backend_that_reports_nothing_leaves_the_estimate_to_say_so() {
        let selected = session_row("a", None, None);
        assert!(reported_context_usage(Some(&selected), &[]).is_none());

        // Half a reading is not a reading: a measured "used" over a guessed
        // window would put two different claims in one line.
        let half = vec![session_row("a", Some(500), None)];
        assert!(reported_context_usage(Some(&selected), &half).is_none());

        let zero_window = vec![session_row("a", Some(500), Some(0))];
        assert!(
            reported_context_usage(Some(&selected), &zero_window).is_none(),
            "a window of zero is not a window"
        );
    }

    /// With no session on screen there is nothing to report a reading of.
    #[test]
    fn nothing_on_screen_has_no_reading() {
        assert!(reported_context_usage(None, &[session_row("a", Some(1), Some(2))]).is_none());
    }

    #[test]
    fn an_estimate_is_never_drawn_as_a_reading() {
        let estimated = ContextUsage::estimated(17_400, 258_000);
        assert!(!estimated.measured);
        let measured = ContextUsage::measured(36_393, 1_048_576);
        assert!(measured.measured);
        // Same bar either way: how full it looks is the numbers' business, and
        // whether they were measured is the label's.
        assert!((measured.ratio() - 36_393.0 / 1_048_576.0).abs() < 0.0001);
        assert_eq!(measured.percent(), 3);
    }
}
