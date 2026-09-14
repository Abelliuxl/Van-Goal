use crate::agent::Backend;
use crate::cache::SessionCacheStore;
use crate::local_server::LocalHermesServer;
use crate::models::*;
use crate::settings::{BackendKind, Settings};
use crate::{hermes_config, log_debug};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use gpui::Task;
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

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

/// A session is unnamed while it still carries the placeholder the client uses
/// for chats it created itself.
fn needs_session_title(title: Option<&str>) -> bool {
    match title.map(str::trim) {
        None | Some("") => true,
        Some(title) => title == "New Chat",
    }
}

/// Sessions as the gateway reports them, plus the one the user is looking at if
/// the gateway has not listed it yet (a chat created here has no messages to
/// report until its first turn ends).
fn merge_fetched_sessions(
    fetched: Vec<AgentSession>,
    selected: Option<&AgentSession>,
) -> Vec<AgentSession> {
    let mut merged = fetched;
    if let Some(selected) = selected {
        if !merged.iter().any(|session| session.id == selected.id) {
            merged.insert(0, selected.clone());
        }
    }
    merged
}

/// Append a chunk to one stream's buffer. Gateways mix incremental deltas,
/// cumulative snapshots, replayed overlaps and plain repeats on the same stream,
/// so a chunk that already covers (or is covered by) the buffer is merged
/// instead of concatenated.
fn merge_stream_chunk(buffer: &mut String, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    if buffer.is_empty() || chunk.starts_with(buffer.as_str()) {
        // A snapshot of everything this stream has produced so far.
        buffer.clear();
        buffer.push_str(chunk);
        return;
    }
    if buffer.ends_with(chunk) {
        // The same chunk delivered twice.
        return;
    }
    // A replayed or re-chunked chunk usually starts where the buffer ends
    // ("…五处落点" + "落点全部实时…"): keep the overlap once.
    buffer.push_str(&chunk[overlap_with_suffix(buffer, chunk)..]);
}

/// Length of the longest suffix of `buffer` that is also a prefix of `chunk`.
/// Overlaps shorter than `MIN_OVERLAP` are ignored: a single shared character is
/// far more likely to be a coincidence than a replay.
fn overlap_with_suffix(buffer: &str, chunk: &str) -> usize {
    const MIN_OVERLAP: usize = 4;
    const MAX_OVERLAP: usize = 512;
    let limit = buffer.len().min(chunk.len()).min(MAX_OVERLAP);
    let boundaries = chunk
        .char_indices()
        .map(|(index, character)| index + character.len_utf8())
        .rev();
    for end in boundaries {
        if end > limit {
            continue;
        }
        if buffer.ends_with(&chunk[..end]) {
            return if end >= MIN_OVERLAP { end } else { 0 };
        }
    }
    0
}

/// The most complete text seen for the turn, across all streams.
fn best_stream_text(streams: &BTreeMap<DeltaSource, String>) -> String {
    streams
        .values()
        .max_by_key(|text| text.chars().count())
        .cloned()
        .unwrap_or_default()
}

struct ConnectOutcome {
    base_url: String,
    discovered_token: Option<String>,
    error: Option<String>,
}

/// Single source of truth for sessions, messages, streaming and sending —
/// the GPUI port of the SwiftUI AppState.
pub struct AppState {
    pub settings: Settings,
    pub local_server: Arc<LocalHermesServer>,
    pub connection_state: ConnectionState,
    pub sessions: Vec<AgentSession>,
    pub selected_session: Option<AgentSession>,
    pub messages: Vec<ChatMessage>,
    pub composer_text: String,
    pub composer_attachments: Vec<ComposerAttachment>,
    pub is_sending: bool,
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
    /// (message id, tool index) pairs expanded in the activity pill.
    pub expanded_tools: HashSet<(String, usize)>,
    /// Prompt held back because no live session existed when the user hit send.
    pub pending_after_start: Option<(String, Vec<ComposerAttachment>)>,
    /// Text received per event stream for the turn in flight. One reply can
    /// arrive on several streams at once (OpenClaw sends both a `session.message`
    /// transcript and an `agent`/assistant stream); appending every stream into
    /// one buffer interleaves two copies of the message, so each is buffered
    /// separately and the most complete one is displayed.
    streaming_text: BTreeMap<DeltaSource, String>,
    /// A silent session-list refresh is in flight (debounces the gateway's
    /// "sessions changed" hints).
    sessions_refresh_inflight: bool,

    backend: Arc<AsyncMutex<Backend>>,
    backend_id: &'static str,
    backend_display_name: &'static str,
    backend_caps: BackendCaps,
    cache_store: SessionCacheStore,
    cached_state: CachedState,
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

        crate::logger::global_logger().set_enabled(settings.debug_logging_enabled);

        let kind = settings.backend_kind;
        let mut state = Self {
            settings: settings.clone(),
            local_server: Arc::new(LocalHermesServer::default()),
            connection_state: ConnectionState::Disconnected,
            sessions,
            selected_session: selected,
            messages: Self::visible_messages(&messages),
            composer_text: String::new(),
            composer_attachments: Vec::new(),
            is_sending: false,
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
            expanded_tools: HashSet::new(),
            pending_after_start: None,
            streaming_text: BTreeMap::new(),
            sessions_refresh_inflight: false,
            backend: Arc::new(AsyncMutex::new(Backend::make(kind))),
            backend_id: backend_static_id(kind),
            backend_display_name: backend_static_name(kind),
            backend_caps: Backend::make(kind).capabilities(),
            cache_store,
            cached_state,
            live_gateway_session_id: None,
            stored_gateway_session_id: None,
            event_tx,
            cache_save_task: None,
        };
        state.update_cache_summary();
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

    pub fn backend_caps(&self) -> BackendCaps {
        self.backend_caps
    }

    pub fn backend_display_name(&self) -> &'static str {
        self.backend_display_name
    }

    pub fn backend_id(&self) -> &'static str {
        self.backend_id
    }

    pub fn context_usage(&self) -> ContextUsage {
        ContextUsage {
            used_tokens: self.estimated_context_tokens(),
            max_tokens: self.context_window_tokens,
        }
    }

    fn estimated_context_tokens(&self) -> i64 {
        const FIXED_PROMPT_TOKENS: i64 = 17_400;
        let message_chars: usize = self
            .messages
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
        log_debug!(
            "app",
            "bootstrap autoConnect={}",
            self.settings.auto_connect
        );
        self.refresh_hermes_model_config();
        self.refresh_permission_mode();
        if self.settings.auto_connect {
            self.connect(cx);
        }
    }

    pub fn connect(&mut self, cx: &mut gpui::Context<Self>) {
        log_debug!("app", "connect requested");
        self.connection_state = ConnectionState::Connecting;
        self.transport_ready = false;
        cx.notify();

        let settings = self.settings.clone();
        let local_server = self.local_server.clone();
        let backend = self.backend.clone();
        let managed = settings.is_managed_local_backend();
        let port = settings.resolved_port();
        let fallback_url = settings.active_backend_url();

        let join = tokio_spawn(cx, async move {
            let base_url = if managed {
                match local_server.ensure_running(port).await {
                    Ok(url) => url,
                    Err(error) => {
                        return ConnectOutcome {
                            base_url: fallback_url,
                            discovered_token: None,
                            error: Some(error.to_string()),
                        };
                    }
                }
            } else {
                fallback_url.clone()
            };
            let config = BackendConfig {
                base_url: base_url.clone(),
                credential: String::new(),
                profile: settings.normalized_profile(),
                workspace: settings.workspace_trimmed(),
            };
            if let Err(error) = backend.lock().await.probe(&config).await {
                return ConnectOutcome {
                    base_url,
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
                            base_url,
                            discovered_token: None,
                            error: Some(error.to_string()),
                        };
                    }
                }
            } else {
                None
            };
            ConnectOutcome {
                base_url,
                discovered_token,
                error: None,
            }
        });

        cx.spawn(async move |this, cx| {
            let outcome = join.await.unwrap_or(ConnectOutcome {
                base_url: String::new(),
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
        self.selected_session = Some(session.clone());
        self.pending_clarify = None;
        let key = cache_key(&session, self.settings.backend_kind.id());
        self.cached_state.selected_session_id = Some(key.clone());
        if let Some(cached) = self
            .cached_state
            .messages_by_session_id
            .get(&key)
            .cloned()
            .filter(|cached| !cached.is_empty())
        {
            self.messages = Self::visible_messages(&cached);
        } else {
            self.messages = Vec::new();
        }
        cx.notify();

        self.load_messages(&session, cx);
        self.connect_gateway(cx);

        let backend = self.backend.clone();
        let config = self.backend_config();
        let session_id = session.id.clone();
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
            let _ = this.update(cx, |state, _cx| match result {
                Ok(ids) => {
                    state.live_gateway_session_id = Some(ids.live_id.clone());
                    state.stored_gateway_session_id =
                        Some(ids.stored_id.unwrap_or_else(|| ids.live_id.clone()));
                    state.transport_ready = true;
                    let cache = state.cache_store.clone();
                    let snapshot = state.cached_state.clone();
                    cache.save(snapshot);
                }
                Err(error) => {
                    state.last_error = Some(error.to_string());
                    log_debug!("app", "resume gateway failed: {error}");
                }
            });
        })
        .detach();
    }

    pub fn start_fresh_chat(&mut self, cx: &mut gpui::Context<Self>) {
        self.selected_session = None;
        self.messages = Vec::new();
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
            backend.lock().await.create_session(&config).await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("create task failed")));
            let _ = this.update(cx, |state, cx| {
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
                        // session existed.
                        if let Some((text, attachments)) = state.pending_after_start.take() {
                            if let Some(session_id) = state.live_gateway_session_id.clone() {
                                state.composer_text = String::new();
                                state.composer_attachments = Vec::new();
                                state.perform_send(text, attachments, session_id, cx);
                            }
                        }
                    }
                    Err(error) => {
                        state.connection_state = ConnectionState::Failed(error.to_string());
                        state.last_error = Some(error.to_string());
                        state.transport_ready = false;
                        // Give the held-back prompt back to the composer so
                        // nothing the user typed is lost.
                        if let Some((text, attachments)) = state.pending_after_start.take() {
                            state.composer_text = text;
                            state.composer_attachments = attachments;
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
            // No live session yet: create one first, then submit automatically.
            // Keep the composer content visible until the prompt actually
            // submits, so a failed session setup never eats the user's text.
            self.pending_after_start = Some((text.clone(), attachments.clone()));
            log_debug!("app", "no live session on send; creating fresh chat first");
            self.start_fresh_chat(cx);
            return;
        }

        if self.is_sending {
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
        self.is_sending = true;
        self.streaming_text.clear();
        let submitted = submitted_prompt(&text, &attachments);
        self.messages
            .push(ChatMessage::new(MessageRole::User, text));
        self.messages
            .push(ChatMessage::streaming(MessageRole::Assistant));
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
                    state.is_sending = false;
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
        if !self.is_sending {
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

        let now = now_unix();
        for message in self.messages.iter_mut() {
            if message.role == MessageRole::Assistant && message.is_streaming {
                message.is_streaming = false;
                message.completed_at = Some(now);
            }
        }
        self.prune_empty_assistant_messages();
        self.messages
            .push(ChatMessage::new(MessageRole::User, trimmed.clone()));
        self.messages
            .push(ChatMessage::streaming(MessageRole::Assistant));
        self.is_sending = true;
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
                    state.is_sending = false;
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
        if self.is_sending {
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

    pub fn reload_model_config(&mut self, cx: &mut gpui::Context<Self>) {
        self.refresh_hermes_model_config();
        cx.notify();
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
        self.cached_state = CachedState::default();
        self.cache_store.clear();
        self.sessions = Vec::new();
        self.selected_session = None;
        self.messages = Vec::new();
        self.update_cache_summary();
        cx.notify();
    }

    pub fn switch_backend(&mut self, kind: BackendKind, cx: &mut gpui::Context<Self>) {
        let backend = self.backend.clone();
        tokio_spawn(cx, async move {
            backend.lock().await.disconnect();
        });
        self.settings.switch_backend(kind);
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
        self.messages = Vec::new();
        self.live_gateway_session_id = None;
        self.stored_gateway_session_id = None;
        self.pending_clarify = None;
        self.last_error = None;
        log_debug!("app", "backend switched to={}", kind.id());
        cx.notify();
        if self.settings.auto_connect {
            self.connect(cx);
        }
    }

    pub fn disconnect(&mut self, cx: &mut gpui::Context<Self>) {
        log_debug!("app", "disconnect requested");
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
        let server = self.local_server.clone();
        tokio_spawn(cx, async move {
            server.stop().await;
        });
    }

    fn backend_caps_for(&self, kind: BackendKind) -> BackendCaps {
        Backend::make(kind).capabilities()
    }

    // ------------------------------------------------------------------
    // Event handling (streaming core)
    // ------------------------------------------------------------------

    pub fn handle_event(&mut self, event: AgentEvent, cx: &mut gpui::Context<Self>) {
        match event {
            AgentEvent::Connected => {
                self.transport_ready = true;
                self.gateway_connecting = false;
                self.connection_state = ConnectionState::Connected;
                self.last_error = None;
            }
            AgentEvent::SessionInfo(session_id) => {
                self.live_gateway_session_id = Some(session_id.clone());
                self.transport_ready = true;
                log_debug!("app", "session info live={session_id}");
            }
            AgentEvent::MessageStart => {
                let needs_placeholder = self.is_sending
                    && (self
                        .messages
                        .last()
                        .map(|last| last.role != MessageRole::Assistant || !last.is_streaming)
                        .unwrap_or(true));
                if needs_placeholder {
                    self.messages
                        .push(ChatMessage::streaming(MessageRole::Assistant));
                    self.schedule_current_messages_cache_save(cx);
                }
            }
            AgentEvent::MessageDelta { text, source } => {
                self.append_assistant_delta(text, source, cx)
            }
            AgentEvent::SessionsChanged => self.refresh_sessions_quietly(cx),
            AgentEvent::MessageComplete(text) => self.complete_assistant_message(text, cx),
            AgentEvent::TurnFailed(message) => {
                self.last_error = Some(message.clone());
                self.complete_assistant_message(Some(format!("Error: {message}")), cx);
            }
            AgentEvent::Tool(record) => {
                log_debug!(
                    "gateway",
                    "tool event name={} status={}",
                    record.name,
                    record.status
                );
                self.append_tool_call(record, cx);
            }
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
                    session_id,
                    question,
                    choices,
                    request_id,
                });
            }
            AgentEvent::Disconnected => {
                self.transport_ready = false;
                self.gateway_connecting = false;
                self.pending_clarify = None;
                if self.connection_state == ConnectionState::Connected {
                    self.connection_state = ConnectionState::Disconnected;
                }
            }
            AgentEvent::Failed(message) => {
                self.transport_ready = false;
                self.gateway_connecting = false;
                self.pending_clarify = None;
                self.connection_state = ConnectionState::Failed(message.clone());
                self.last_error = Some(message);
            }
        }
        cx.notify();
    }

    fn append_assistant_delta(
        &mut self,
        text: String,
        source: DeltaSource,
        cx: &mut gpui::Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }
        merge_stream_chunk(self.streaming_text.entry(source).or_default(), &text);
        let streamed = best_stream_text(&self.streaming_text);
        if streamed.is_empty() {
            return;
        }
        if self.streaming_text.len() > 1 {
            log_debug!(
                "gateway",
                "reply arriving on {} streams; showing the most complete one",
                self.streaming_text.len()
            );
        }
        if let Some(index) = self.messages.iter().rposition(|message| {
            message.role == MessageRole::Assistant
                && message.is_streaming
                && message.tool_calls.is_empty()
        }) {
            self.messages[index].content = streamed;
        } else {
            let mut message = ChatMessage::streaming(MessageRole::Assistant);
            message.content = streamed;
            self.messages.push(message);
        }
        self.schedule_current_messages_cache_save(cx);
    }

    fn append_tool_call(&mut self, record: ToolCallRecord, cx: &mut gpui::Context<Self>) {
        // Deduplicate within the current turn only (after the last user message).
        let turn_start = self
            .messages
            .iter()
            .rposition(|message| message.role == MessageRole::User)
            .map(|index| index + 1)
            .unwrap_or(0);
        let is_duplicate = self.messages[turn_start..].iter().any(|message| {
            message.tool_calls.iter().any(|call| {
                call.name == record.name
                    && call.status == record.status
                    && call.detail == record.detail
            })
        });
        if is_duplicate {
            return;
        }

        if let Some(index) = self.messages.iter().rposition(|message| {
            message.role == MessageRole::Assistant
                && message.is_streaming
                && message.content.trim().is_empty()
        }) {
            self.messages[index].tool_calls.push(record);
        } else {
            let mut message = ChatMessage::streaming(MessageRole::Assistant);
            message.tool_calls.push(record);
            self.messages.push(message);
        }
        self.schedule_current_messages_cache_save(cx);
    }

    fn complete_assistant_message(
        &mut self,
        final_text: Option<String>,
        cx: &mut gpui::Context<Self>,
    ) {
        // Any pending clarify is stale once the turn ends.
        self.pending_clarify = None;
        let completed_at = now_unix();
        let active_index = self
            .messages
            .iter()
            .rposition(|message| message.role == MessageRole::Assistant && message.is_streaming);

        for message in self.messages.iter_mut() {
            if message.role == MessageRole::Assistant && message.is_streaming {
                message.is_streaming = false;
                message.completed_at = Some(completed_at);
            }
        }

        if let Some(index) = active_index {
            let trimmed_final = final_text
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            if !trimmed_final.is_empty() {
                if self.messages[index].tool_calls.is_empty() {
                    if self.messages[index].content != trimmed_final {
                        self.messages[index].content = trimmed_final;
                    }
                } else {
                    self.messages
                        .push(ChatMessage::new(MessageRole::Assistant, trimmed_final));
                }
            }
        } else if let Some(final_text) = final_text.filter(|text| !text.is_empty()) {
            self.messages
                .push(ChatMessage::new(MessageRole::Assistant, final_text));
        }
        self.prune_empty_assistant_messages();
        self.streaming_text.clear();
        self.is_sending = false;
        self.save_current_messages_to_cache();
        // A chat created here is named by the gateway from its first prompt, and
        // the name only shows up once the session list is re-read.
        if self
            .selected_session
            .as_ref()
            .is_some_and(|session| needs_session_title(session.title.as_deref()))
        {
            self.refresh_sessions_quietly(cx);
        }

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

    fn prune_empty_assistant_messages(&mut self) {
        self.messages.retain(|message| !message.is_empty_shell());
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
            .insert(key.clone(), self.messages.clone());
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
            self.messages = Vec::new();
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
                self.selected_session = None;
                self.messages = Vec::new();
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
        let join = tokio_spawn(cx, async move {
            backend.lock().await.messages(&config, &session_id).await
        });
        cx.spawn(async move |this, cx| {
            let result = join
                .await
                .unwrap_or(Err(anyhow::anyhow!("messages task failed")));
            let _ = this.update(cx, |state, cx| {
                match result {
                    Ok(fetched) => {
                        let fetched = AppState::visible_messages(&fetched);
                        let key = format!("{backend_id}::{session_key}");
                        // Do not clobber messages mid-stream; WS deltas own the
                        // current bubble until completion.
                        if state
                            .messages
                            .last()
                            .map(|last| last.is_streaming)
                            .unwrap_or(false)
                        {
                            state
                                .cached_state
                                .messages_by_session_id
                                .insert(key, fetched);
                            state.update_cache_summary();
                            let cache = state.cache_store.clone();
                            let snapshot = state.cached_state.clone();
                            cache.save(snapshot);
                            return;
                        }
                        if !state.messages.is_empty() && fetched.len() <= state.messages.len() {
                            return;
                        }
                        state.messages = fetched.clone();
                        state
                            .cached_state
                            .messages_by_session_id
                            .insert(key.clone(), fetched);
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
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn cache_key(session: &AgentSession, backend_id: &str) -> String {
    format!("{backend_id}::{}", session.id)
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

#[cfg(test)]
mod streaming_tests {
    use super::*;

    #[test]
    fn unnamed_sessions_are_recognised() {
        assert!(needs_session_title(None));
        assert!(needs_session_title(Some("")));
        assert!(needs_session_title(Some("  ")));
        assert!(needs_session_title(Some("New Chat")));
        assert!(!needs_session_title(Some("Hermit GPUI")));
    }

    #[test]
    fn the_open_chat_survives_a_session_list_refresh() {
        let mut selected = AgentSession::default();
        selected.id = "agent:main:hermit:brand-new".into();
        selected.title = Some("New Chat".into());

        let listed = AgentSession {
            id: "agent:main:hermit:older".into(),
            title: Some("Hermit GPUI".into()),
            ..Default::default()
        };

        // The gateway has not listed the brand new chat yet: it must not vanish
        // from the sidebar while the user is looking at it.
        let merged = merge_fetched_sessions(vec![listed.clone()], Some(&selected));
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, selected.id);
        assert!(merged.iter().any(|session| session.id == listed.id));

        // Once the gateway lists it, the fetched (named) copy wins.
        let named = AgentSession {
            id: selected.id.clone(),
            title: Some("Hermit GPUI".into()),
            ..Default::default()
        };
        let merged = merge_fetched_sessions(vec![named], Some(&selected));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].title.as_deref(), Some("Hermit GPUI"));
    }

    #[test]
    fn incremental_chunks_accumulate() {
        let mut buffer = String::new();
        for chunk in ["科", "研", "模式已进入"] {
            merge_stream_chunk(&mut buffer, chunk);
        }
        assert_eq!(buffer, "科研模式已进入");
    }

    #[test]
    fn cumulative_snapshots_replace_instead_of_duplicating() {
        let mut buffer = String::new();
        merge_stream_chunk(&mut buffer, "科研");
        merge_stream_chunk(&mut buffer, "科研模式");
        merge_stream_chunk(&mut buffer, "科研模式已进入");
        assert_eq!(buffer, "科研模式已进入");
    }

    #[test]
    fn a_repeated_chunk_is_ignored() {
        let mut buffer = String::new();
        merge_stream_chunk(&mut buffer, "ab");
        merge_stream_chunk(&mut buffer, "ab");
        assert_eq!(buffer, "ab");
    }

    fn chunked(text: &str, size: usize) -> Vec<String> {
        let characters: Vec<char> = text.chars().collect();
        characters
            .chunks(size)
            .map(|chunk| chunk.iter().collect())
            .collect()
    }

    /// The OpenClaw regression: one reply is delivered by two streams at the same
    /// time with different chunk boundaries. Appending them into one buffer
    /// interleaved two copies of the text and broke the markdown tables inside
    /// it, because the separator row ended up split across the two copies.
    #[test]
    fn two_streams_of_one_reply_do_not_interleave() {
        let text = "刚把五处落点全部实时拉了一遍，当前状态如下：\n\n\
                    | 项目 | 进度 | 进行中 |\n|---|---|---|\n\
                    | **nano-AgFe-抗氧化** | 6/9 | 测试ROS检测手段 |\n\
                    | PEEK亲水改性 | 4/4 ✅ | — |\n";
        let first = chunked(text, 3);
        let second = chunked(text, 7);
        let mut streams: BTreeMap<DeltaSource, String> = BTreeMap::new();
        for index in 0..first.len().max(second.len()) {
            if let Some(chunk) = first.get(index) {
                merge_stream_chunk(streams.entry(DeltaSource::Transcript).or_default(), chunk);
            }
            if let Some(chunk) = second.get(index) {
                merge_stream_chunk(streams.entry(DeltaSource::AgentStream).or_default(), chunk);
            }
        }

        // What the old code did: every stream appended into one buffer.
        let mut naive = String::new();
        for index in 0..first.len().max(second.len()) {
            if let Some(chunk) = first.get(index) {
                naive.push_str(chunk);
            }
            if let Some(chunk) = second.get(index) {
                naive.push_str(chunk);
            }
        }
        assert_ne!(naive, text, "the naive merge should reproduce the garbling");
        assert!(
            !crate::markdown::parse(&naive)
                .iter()
                .any(|block| matches!(block, crate::markdown::MarkdownBlock::Table(..))),
            "the naive merge is what stopped tables from parsing"
        );

        let shown = best_stream_text(&streams);
        assert_eq!(shown, text, "interleaved streams garbled the reply");
        assert!(
            shown.contains("|---|---|---|"),
            "the table separator was split"
        );
        // The parser turns that separator into a real table.
        let tables = crate::markdown::parse(&shown)
            .into_iter()
            .filter(|block| matches!(block, crate::markdown::MarkdownBlock::Table(..)))
            .count();
        assert_eq!(tables, 1, "table was not recognised after merging streams");
    }

    #[test]
    fn replayed_overlap_is_merged_once() {
        let mut buffer = String::new();
        merge_stream_chunk(&mut buffer, "刚把五处落点");
        merge_stream_chunk(&mut buffer, "落点全部实时拉了一遍");
        assert_eq!(buffer, "刚把五处落点全部实时拉了一遍");
    }

    #[test]
    fn a_one_character_coincidence_is_not_treated_as_an_overlap() {
        let mut buffer = String::new();
        merge_stream_chunk(&mut buffer, "好的");
        merge_stream_chunk(&mut buffer, "的的确");
        assert_eq!(buffer, "好的的的确");
    }

    #[test]
    fn normal_chunks_are_not_trimmed() {
        let mut buffer = String::new();
        for chunk in ["项目", "总览：", "nano-AgFe", "-抗氧化"] {
            merge_stream_chunk(&mut buffer, chunk);
        }
        assert_eq!(buffer, "项目总览：nano-AgFe-抗氧化");
    }

    #[test]
    fn the_most_complete_stream_wins() {
        let mut streams: BTreeMap<DeltaSource, String> = BTreeMap::new();
        merge_stream_chunk(
            streams.entry(DeltaSource::Transcript).or_default(),
            "完整的一段话",
        );
        merge_stream_chunk(streams.entry(DeltaSource::AgentStream).or_default(), "完整");
        assert_eq!(best_stream_text(&streams), "完整的一段话");
    }
}
