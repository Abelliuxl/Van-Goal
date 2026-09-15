//! The headless client behind the C ABI: owns the runtime, drives a
//! [`Backend`], and turns its events into a JSON queue the Flutter side drains.
//!
//! The Flutter side owns the widget state; this side owns the connection, the
//! session ids and the message list. Nothing here knows what a message bubble
//! is, and Dart does not fold events into messages: it draws the snapshot this
//! side hands it, which is why the two cannot drift apart on the questions that
//! matter (does a tool call appear twice, has the reply stopped streaming).
//!
//! ## Reconnecting
//!
//! A phone takes the app away whenever the screen locks, and the socket does not
//! survive it. The connection is therefore owned by a loop that keeps trying for
//! as long as the user wants to be connected, with a backoff, and re-subscribes
//! the session that was open. `ensure_connected` lets the frontend ask after a
//! resume, where the socket may look alive to a frozen process and be long gone.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;

use van_goal_core::agent::Backend;
use van_goal_core::chat::{Conversation, ConversationChange};
use van_goal_core::markdown::{self, MarkdownBlock};
use van_goal_core::models::{AgentEvent, BackendConfig, ChatMessage, MessageRole, SessionIDs};
use van_goal_core::settings::{BackendKind, FontSize, SavedSession, Settings};

/// The one connection the app has. Flutter has a single foreground client, so a
/// handle-passing API would only add a way to get it wrong.
static CLIENT: OnceLock<Client> = OnceLock::new();

pub fn client() -> &'static Client {
    CLIENT.get_or_init(Client::new)
}

/// Cap on the queue, so a Flutter side that stops polling cannot grow it
/// without bound. Assistant deltas coalesce (see [`push`]), so in practice the
/// queue holds a handful of entries.
const MAX_QUEUED_EVENTS: usize = 512;

/// How long to wait before the next attempt, in seconds, from the first failure
/// onwards. The last value is used for every later attempt.
const RETRY_SECONDS: [u64; 6] = [1, 2, 4, 8, 15, 30];

/// How long one attempt may take before it is abandoned and tried again. A phone
/// that lost its network mid-handshake leaves a connect that neither finishes
/// nor fails.
///
/// It has to outlast the handshake the adapters already bound themselves: they
/// wait about twenty seconds for a gateway to answer, and abandoning an attempt
/// before they have given up throws away the only place that knows *why* it
/// failed. The adapters also keep the socket they opened, so an attempt cut
/// short mid-handshake leaves them believing they are still connected.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(45);

/// Probe the address, then open the event stream onto it, from a clean slate.
///
/// The disconnect is not tidiness: an adapter that was left mid-handshake
/// reports itself as open, so the next attempt would skip opening it and wait on
/// a handshake whose events nobody is listening to. The socket stays up, no
/// event ever arrives, and the app sits on "connecting" with nothing left to
/// fail — while a fresh start would simply have worked.
///
/// The locks are separate statements on purpose: a guard held by a `match` or an
/// `if let` outlives the arm it guards, so locking again inside one would wait on
/// the lock the same task is holding — the same silent hang by a different
/// route.
async fn establish(
    backend: &Arc<AsyncMutex<Backend>>,
    config: &BackendConfig,
    sender: UnboundedSender<AgentEvent>,
) -> Result<()> {
    backend.lock().await.disconnect();
    let probed = backend.lock().await.probe(config).await;
    probed?;
    backend.lock().await.connect(config.clone(), sender).await
}

pub struct Client {
    runtime: tokio::runtime::Runtime,
    /// Shared with the background tasks that learn which session is on screen,
    /// so the one this device had open survives a restart — see
    /// [`remember_session`].
    settings: Arc<Mutex<Settings>>,
    /// The backend, together with the kind it was built for: a `Backend` does
    /// not expose its own kind, and switching backends has to build a new one.
    backend: Mutex<Option<(BackendKind, Arc<AsyncMutex<Backend>>)>>,
    events: Arc<Mutex<Vec<Value>>>,
    open: Arc<Mutex<OpenSession>>,
    /// The event stream is up and the pump is draining it.
    live: Arc<AtomicBool>,
    /// The user wants to be connected. Cleared by `disconnect`, so a connection
    /// the user closed stays closed instead of being retried forever.
    wanted: Arc<AtomicBool>,
    /// Bumped by every `connect` and `disconnect`: a retry loop from an older
    /// attempt sees that it is no longer the current one and stops.
    generation: Arc<AtomicU64>,
}

/// State for the session the user is looking at.
#[derive(Default)]
struct OpenSession {
    live_id: Option<String>,
    stored_id: Option<String>,
    /// The messages on screen, folded from events by [`Conversation`].
    conversation: Conversation,
    /// Conversations for sessions that are not on screen. Switching away and
    /// back must not lose the tool calls of the turn that just ran: the backend
    /// reports a turn as text alone (see [`Conversation::merge_transcript`]).
    parked: BTreeMap<String, Conversation>,
}

/// How many sessions' worth of messages to keep while the user is elsewhere.
/// Only the one on screen and the one just left are ever wanted; the cap is
/// there so a day of switching between long sessions cannot grow without bound.
const PARKED_LIMIT: usize = 8;

impl OpenSession {
    /// The key a conversation is filed under: whichever id is known.
    fn key(&self) -> Option<String> {
        self.stored_id.clone().or_else(|| self.live_id.clone())
    }

    /// Whether an event that belongs to a turn may be folded into the messages
    /// on screen. While a session is being opened there is no live connection
    /// yet, and anything that arrives in that window was produced by the session
    /// the user just left.
    fn accepts_turn_events(&self) -> bool {
        self.live_id.is_some() || self.stored_id.is_none()
    }

    /// Put the conversation on screen away and bring back the one for `id`, if
    /// this client has seen it before.
    fn switch_to(&mut self, id: Option<&str>) {
        if let Some(key) = self.key() {
            let parked = std::mem::take(&mut self.conversation);
            self.parked.insert(key, parked);
            while self.parked.len() > PARKED_LIMIT {
                let oldest = match self.parked.keys().next() {
                    Some(key) => key.clone(),
                    None => break,
                };
                self.parked.remove(&oldest);
            }
        }
        self.conversation = id.and_then(|id| self.parked.remove(id)).unwrap_or_default();
    }
}

/// Where a prompt is about to be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SendTarget {
    /// The connection already has a live session.
    Live(String),
    /// The session on screen has no live connection, but it exists on the
    /// backend under this key: attach to it again.
    Resume(String),
    /// No session on screen at all, so this really is a new chat.
    Create,
}

/// Decide where a prompt goes, from the two ids this client holds.
///
/// The distinction between [`SendTarget::Resume`] and [`SendTarget::Create`] is
/// the whole of it. A session whose live connection is missing — the normal
/// state after a reconnect, and the state a phone is in most of the time — used
/// to be treated as "no session", which created a new one and sent the prompt
/// there. The user was reading one conversation and the prompt went into
/// another, which is exactly what makes a client look like it is crossing
/// sessions. Only a chat with no session on screen at all needs one created.
fn send_target(live: Option<&str>, stored: Option<&str>) -> SendTarget {
    match (live, stored) {
        (Some(live), _) => SendTarget::Live(live.to_string()),
        (None, Some(stored)) => SendTarget::Resume(stored.to_string()),
        (None, None) => SendTarget::Create,
    }
}

/// Remember the session on screen, so the next launch comes back to it.
///
/// A phone is closed and reopened constantly, and an app that comes up on an
/// empty chat makes the user's first message create *another* session: the
/// conversation they were having is still on the backend, but the client is no
/// longer in it, which reads as the session having changed underneath them.
fn remember_session(settings: &Mutex<Settings>, id: &str) {
    let mut settings = settings.lock().unwrap();
    let saved = SavedSession {
        backend: settings.backend_kind.id().to_string(),
        id: id.to_string(),
    };
    if settings.last_session.as_ref() == Some(&saved) {
        return;
    }
    settings.last_session = Some(saved);
    settings.save();
}

/// The session this device had open, if it was open on the configured backend.
/// A key means nothing to a backend that did not issue it.
fn saved_session(settings: &Mutex<Settings>) -> Option<String> {
    let settings = settings.lock().unwrap();
    settings
        .last_session
        .as_ref()
        .filter(|saved| saved.backend == settings.backend_kind.id())
        .map(|saved| saved.id.clone())
}

/// Stop coming back to a session, because the user asked for a new chat.
fn forget_session(settings: &Mutex<Settings>) {
    let mut settings = settings.lock().unwrap();
    if settings.last_session.take().is_some() {
        settings.save();
    }
}

/// Show a session's transcript and attach this connection to it.
///
/// Both halves matter and the order is the point: the messages are shown *before*
/// the session is resumed, so the conversation on screen and the session the next
/// prompt goes to are the same one. Resuming without showing leaves an empty
/// transcript sending into a live session; showing without resuming leaves the
/// screen on a session the connection is not attached to. Either way the user
/// sees one chat and writes into another.
///
/// Shared by the user opening a session and by the reconnect that brings back
/// the one that was open, which are the same operation.
async fn open_on(
    backend: &Arc<AsyncMutex<Backend>>,
    config: &BackendConfig,
    events: &Arc<Mutex<Vec<Value>>>,
    open: &Arc<Mutex<OpenSession>>,
    id: &str,
) {
    {
        let mut session = open.lock().unwrap();
        session.switch_to(Some(id));
        // Not attached to it yet: `accepts_turn_events` holds turn traffic off
        // until the backend confirms, which is why the live id is cleared here.
        session.live_id = None;
        session.stored_id = Some(id.to_string());
    }
    // What this client already drew for the session, tool calls and all, before
    // the backend has said anything.
    push_transcript(events, open);

    if let Ok(messages) = backend.lock().await.messages(config, id).await {
        // A turn the client watched is richer than what the backend reports, so
        // the two are merged rather than replaced.
        open.lock().unwrap().conversation.merge_transcript(messages);
        push_transcript(events, open);
    }
    match backend.lock().await.resume_session(config, id).await {
        Ok(ids) => {
            {
                let mut session = open.lock().unwrap();
                session.live_id = Some(ids.live_id.clone());
                session.stored_id = ids.stored_id.clone().or(Some(id.to_string()));
            }
            push(events, json!({ "event": "session", "id": ids.live_id }));
        }
        Err(error) => push_error(events, format!("could not resume the session: {error}")),
    }
}

impl Client {
    fn new() -> Self {
        // Two worker threads: one turn at a time plus the event pump, which is
        // all a chat client needs and keeps the thread count down on a phone.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build the tokio runtime");
        Self {
            runtime,
            settings: Arc::new(Mutex::new(Settings::default())),
            backend: Mutex::new(None),
            events: Arc::new(Mutex::new(Vec::new())),
            open: Arc::new(Mutex::new(OpenSession::default())),
            live: Arc::new(AtomicBool::new(false)),
            wanted: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }
    /// Handle one command from Dart. The reply carries a payload for the few
    /// commands whose answer is a value rather than an event.
    pub fn run(&self, command: &str) -> Result<Option<Value>> {
        let value: Value = serde_json::from_str(command)
            .map_err(|error| anyhow!("command was not JSON: {error}"))?;
        match value.get("cmd").and_then(Value::as_str) {
            Some("settings") => self.report_settings().map(Some),
            Some("configure") => self.configure(&value).map(|()| None),
            Some("set_ui") => self.set_ui(&value).map(Some),
            Some("connect") => {
                self.connect(false);
                Ok(None)
            }
            Some("ensure_connected") => {
                // `force` comes from the app returning to the foreground: a
                // socket that outlived a locked screen is not necessarily alive.
                let force = value.get("force").and_then(Value::as_bool).unwrap_or(false);
                self.connect(force);
                Ok(None)
            }
            Some("disconnect") => self.disconnect().map(|()| None),
            Some("list_sessions") => self.list_sessions().map(|()| None),
            Some("open_session") => {
                let id = required(&value, "id")?;
                self.open_session(id);
                Ok(None)
            }
            Some("new_session") => self.new_session().map(|()| None),
            Some("send") => {
                let body = required(&value, "text")?;
                self.send(body);
                Ok(None)
            }
            Some("interrupt") => self.interrupt().map(|()| None),
            Some("answer") => {
                let request = required(&value, "request_id")?;
                let answer = required(&value, "answer")?;
                self.answer(request, answer);
                Ok(None)
            }
            Some("markdown") => {
                let text = required(&value, "text")?;
                Ok(Some(json!({ "blocks": blocks(&text) })))
            }
            other => Err(anyhow!("unknown command {other:?}")),
        }
    }

    /// Take everything queued since the last call.
    pub fn poll(&self) -> Vec<Value> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    // ---------------------------------------------------------------- set-up

    /// Load what was saved last time and hand it to the UI.
    ///
    /// A command rather than something `new` does, because the directory to
    /// load from is named by the host and the host calls `vg_set_data_dir`
    /// after this object already exists.
    fn report_settings(&self) -> Result<Value> {
        let settings = Settings::load();
        let payload = settings_payload(&settings);
        *self.settings.lock().unwrap() = settings;
        self.push(payload.clone());
        // A backend the user left switched on is connected again here, before
        // the first screen is drawn. The app being closed, the screen locking and
        // the phone dropping the network are the normal case on mobile, not an
        // error the user should have to undo by hand every time.
        if self.wants_connection() {
            self.connect(false);
        }
        Ok(payload)
    }

    fn configure(&self, value: &Value) -> Result<()> {
        let kind = match value.get("backend").and_then(Value::as_str) {
            Some(id) => parse_kind(id)?,
            None => return Err(anyhow!("configure needs a backend")),
        };
        {
            let mut settings = self.settings.lock().unwrap();
            settings.backend_kind = kind;
            if let Some(host) = value.get("host").and_then(Value::as_str) {
                settings.backend_host = host.to_string();
            }
            if let Some(port) = value.get("port").and_then(Value::as_u64) {
                settings.backend_port = port as u16;
            }
            if let Some(tls) = value.get("use_tls").and_then(Value::as_bool) {
                settings.backend_use_tls = tls;
            }
            if let Some(credential) = value.get("credential").and_then(Value::as_str) {
                settings.session_token = credential.to_string();
            }
            if let Some(workspace) = value.get("workspace").and_then(Value::as_str) {
                settings.workspace_path = workspace.to_string();
            }
            // The switch records that this backend is the one to come back to:
            // the app connects on launch when it is on, and stays off when the
            // user disconnected before closing it.
            settings.set_backend_enabled(kind, true);
            // Best effort: where there is no writable app directory this fails
            // quietly and the app simply does not remember the connection.
            settings.save();
        }
        Ok(())
    }

    /// Persist one of the interface preferences and hand the whole set back, so
    /// the frontend and the file cannot disagree about what was saved.
    fn set_ui(&self, value: &Value) -> Result<Value> {
        let payload = {
            let mut settings = self.settings.lock().unwrap();
            if let Some(id) = value.get("font_size").and_then(Value::as_str) {
                settings.font_size =
                    FontSize::from_id(id).ok_or_else(|| anyhow!("unknown font size {id:?}"))?;
            }
            if let Some(show) = value.get("show_tool_calls").and_then(Value::as_bool) {
                settings.show_tool_calls = show;
            }
            settings.save();
            settings_payload(&settings)
        };
        self.push(payload.clone());
        Ok(payload)
    }

    fn config(&self) -> BackendConfig {
        let settings = self.settings.lock().unwrap();
        BackendConfig {
            base_url: settings.active_backend_url(),
            credential: settings.session_token.clone(),
            profile: settings.normalized_profile(),
            workspace: settings.workspace_trimmed(),
        }
    }

    fn kind(&self) -> BackendKind {
        self.settings.lock().unwrap().backend_kind
    }

    /// Whether the saved settings say this app should be connected: on for a
    /// backend the user configured, off after they disconnected.
    fn wants_connection(&self) -> bool {
        let settings = self.settings.lock().unwrap();
        let kind = settings.backend_kind;
        settings.is_backend_enabled(kind)
    }

    /// The backend for the configured kind, rebuilt when the kind changed.
    fn backend(&self) -> Arc<AsyncMutex<Backend>> {
        let kind = self.kind();
        let mut slot = self.backend.lock().unwrap();
        if let Some((built_for, backend)) = slot.as_ref() {
            if *built_for == kind {
                return backend.clone();
            }
        }
        let backend = Arc::new(AsyncMutex::new(Backend::make(kind)));
        *slot = Some((kind, backend.clone()));
        backend
    }

    /// A turn is running: dropping the stream would lose the reply being written.
    fn busy(&self) -> bool {
        self.open.lock().unwrap().conversation.is_sending()
    }

    // ------------------------------------------------------------- lifecycle

    /// Open the connection, or leave the running one alone.
    ///
    /// `force` is for the app coming back to the foreground: a socket that
    /// outlived a locked screen never errors (the phone may have changed network
    /// underneath it), so it has to be allowed to start over. It is skipped
    /// while a turn is in flight, where the stream is worth more than the risk.
    fn connect(&self, force: bool) {
        let live = self.live.load(Ordering::SeqCst);
        if live && !force {
            return;
        }
        if live && self.busy() {
            return;
        }
        self.wanted.store(true, Ordering::SeqCst);
        // The pump that owns the current connection is being replaced: it exits
        // when its stream closes, which the new attempt does before anything
        // else. Until then the app is not connected, whatever the old socket
        // still thinks.
        self.live.store(false, Ordering::SeqCst);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.pump(generation);
    }

    /// Ask for the connection and keep asking until it is there or the user
    /// closes it.
    fn pump(&self, generation: u64) {
        let config = self.config();
        let backend = self.backend();
        let events = self.events.clone();
        let open = self.open.clone();
        let live = self.live.clone();
        let wanted = self.wanted.clone();
        let current = self.generation.clone();
        let saved = saved_session(&self.settings);

        self.runtime.spawn(async move {
            let mut attempt = 0usize;
            loop {
                if !wanted.load(Ordering::SeqCst) || current.load(Ordering::SeqCst) != generation {
                    return;
                }
                // Reported per attempt, not once: a client that is retrying every
                // half minute looks frozen otherwise, and the header would say
                // "offline" while the app is in fact working on it.
                push(
                    &events,
                    json!({ "event": "connection", "state": "connecting" }),
                );

                let (sender, mut receiver) = unbounded::<AgentEvent>();
                let opened = match tokio::time::timeout(
                    ATTEMPT_TIMEOUT,
                    establish(&backend, &config, sender),
                )
                .await
                {
                    Ok(outcome) => outcome,
                    // A connection that is accepted and then never answers would
                    // otherwise hold the only attempt there is, with the header
                    // saying "connecting" until the app is restarted.
                    Err(_) => Err(anyhow!("no answer within {}s", ATTEMPT_TIMEOUT.as_secs())),
                };
                if let Err(error) = opened {
                    // A newer pump may have taken over while this attempt was in
                    // flight; its state is the one the user sees.
                    if current.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    push_error(&events, format!("could not reach the backend: {error}"));
                } else {
                    live.store(true, Ordering::SeqCst);
                    attempt = 0;
                    let reopened = { open.lock().unwrap().stored_id.clone() };
                    match reopened {
                        // A session is already on screen: put the subscription
                        // back. A gateway only delivers a session's frames to a
                        // connection that subscribed to it.
                        Some(id) => match backend.lock().await.resume_session(&config, &id).await {
                            Ok(ids) => {
                                open.lock().unwrap().live_id = Some(ids.live_id.clone());
                                push(&events, json!({ "event": "session", "id": ids.live_id }));
                            }
                            Err(error) => push_error(
                                &events,
                                format!("could not resume the session: {error}"),
                            ),
                        },
                        // Nothing on screen: this is the first connection of a
                        // launch, and the session this device had open is the
                        // one to come back to. Showing it, rather than
                        // subscribing quietly, is what keeps the transcript and
                        // the session the next prompt goes to in agreement.
                        None => {
                            if let Some(id) = saved.clone() {
                                open_on(&backend, &config, &events, &open, &id).await;
                            }
                        }
                    }
                    // Drain the backend's events for as long as the stream lives.
                    while let Some(event) = receiver.next().await {
                        apply(&events, &open, event);
                    }
                    // The stream ending is only worth reporting if this is still
                    // the connection the app is on: a newer pump has already
                    // said "connecting", and its "connected" must not be undone
                    // by this one going away. Checked before the flag is cleared
                    // too, so an outgoing pump cannot mark the incoming one down.
                    if current.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    live.store(false, Ordering::SeqCst);
                    push(
                        &events,
                        json!({ "event": "connection", "state": "disconnected" }),
                    );
                    // A turn that was running when the stream died would leave
                    // its spinner turning forever.
                    let changed = {
                        let mut open = open.lock().unwrap();
                        if open.conversation.is_sending() {
                            open.conversation.finish_turn();
                            true
                        } else {
                            false
                        }
                    };
                    if changed {
                        push_transcript(&events, &open);
                    }
                }

                if !wanted.load(Ordering::SeqCst) || current.load(Ordering::SeqCst) != generation {
                    return;
                }
                let wait = RETRY_SECONDS[attempt.min(RETRY_SECONDS.len() - 1)];
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(wait)).await;
            }
        });
    }

    /// Close the connection and keep it closed.
    fn disconnect(&self) -> Result<()> {
        self.wanted.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        {
            // The switch records that this backend is *not* the one to come back
            // to, so the next launch starts disconnected like the user left it.
            let mut settings = self.settings.lock().unwrap();
            let kind = settings.backend_kind;
            settings.set_backend_enabled(kind, false);
            settings.save();
        }
        self.live.store(false, Ordering::SeqCst);

        let backend = self.backend();
        let events = self.events.clone();
        self.runtime.spawn(async move {
            backend.lock().await.disconnect();
            push(
                &events,
                json!({ "event": "connection", "state": "disconnected" }),
            );
        });
        Ok(())
    }

    // -------------------------------------------------------------- sessions

    fn list_sessions(&self) -> Result<()> {
        let config = self.config();
        let backend = self.backend();
        let events = self.events.clone();
        self.runtime.spawn(async move {
            match backend.lock().await.list_sessions(&config).await {
                Ok(sessions) => {
                    let items: Vec<Value> = sessions
                        .into_iter()
                        .map(|session| {
                            json!({
                                "id": session.id,
                                "title": session.display_title(),
                                "model": session.model,
                                "cwd": session.cwd,
                                "last_active": session.last_active,
                                "messages": session.message_count,
                            })
                        })
                        .collect();
                    push(&events, json!({ "event": "sessions", "items": items }));
                }
                Err(error) => push_error(&events, format!("could not list sessions: {error}")),
            }
        });
        Ok(())
    }

    fn open_session(&self, id: String) {
        // Opened on purpose: this is the conversation to come back to.
        remember_session(&self.settings, &id);
        let config = self.config();
        let backend = self.backend();
        let events = self.events.clone();
        let open = self.open.clone();
        self.runtime.spawn(async move {
            open_on(&backend, &config, &events, &open, &id).await;
        });
    }

    /// Start a chat that has no session on the backend yet. The session itself
    /// is created on the first send, so opening one costs nothing.
    fn new_session(&self) -> Result<()> {
        {
            let mut open = self.open.lock().unwrap();
            open.switch_to(None);
            open.live_id = None;
            open.stored_id = None;
        }
        // The user asked for a new chat, so that — not the conversation they
        // were in — is what a relaunch should come back to.
        forget_session(&self.settings);
        let events = self.events.clone();
        let open = self.open.clone();
        push_transcript(&events, &open);
        Ok(())
    }

    // ------------------------------------------------------------------ turn

    fn send(&self, body: String) {
        // The prompt goes into the message list here, not in the frontend: the
        // next snapshot replaces what Dart holds, and a message only the
        // frontend drew would be the one that disappeared.
        {
            let mut open = self.open.lock().unwrap();
            open.conversation.begin_turn_with(&body);
        }
        let events = self.events.clone();
        let open = self.open.clone();
        push_transcript(&events, &open);

        let config = self.config();
        let backend = self.backend();
        let settings = self.settings.clone();
        self.runtime.spawn(async move {
            let (existing, stored) = {
                let open = open.lock().unwrap();
                (open.live_id.clone(), open.stored_id.clone())
            };
            let live = match send_target(existing.as_deref(), stored.as_deref()) {
                SendTarget::Live(live) => live,
                // There is no live connection to the session on screen. It still
                // exists on the backend, so resume it: creating one here is how a
                // prompt ended up in a conversation the user had never opened,
                // which is indistinguishable from one session's messages
                // appearing in another.
                SendTarget::Resume(stored) => {
                    match backend.lock().await.resume_session(&config, &stored).await {
                        Ok(ids) => {
                            let mut open = open.lock().unwrap();
                            open.live_id = Some(ids.live_id.clone());
                            open.stored_id = ids.stored_id.clone().or(Some(stored));
                            ids.live_id
                        }
                        Err(error) => {
                            push_error(&events, format!("could not resume the session: {error}"));
                            open.lock().unwrap().conversation.finish_turn();
                            push_transcript(&events, &open);
                            return;
                        }
                    }
                }
                // A chat with no session at all: the only case that needs one
                // created.
                SendTarget::Create => match backend.lock().await.create_session(&config).await {
                    Ok(SessionIDs { live_id, stored_id }) => {
                        let mut open = open.lock().unwrap();
                        open.live_id = Some(live_id.clone());
                        open.stored_id = stored_id.or_else(|| open.stored_id.take());
                        live_id
                    }
                    Err(error) => {
                        push_error(&events, format!("could not start a session: {error}"));
                        open.lock().unwrap().conversation.finish_turn();
                        push_transcript(&events, &open);
                        return;
                    }
                },
            };
            // The prompt goes to this session, so this is the conversation a
            // relaunch has to come back to.
            remember_session(&settings, &live);
            if let Err(error) = backend.lock().await.submit_prompt(&live, &body).await {
                push_error(&events, format!("could not send the prompt: {error}"));
                open.lock().unwrap().conversation.finish_turn();
                push_transcript(&events, &open);
            }
        });
    }

    fn interrupt(&self) -> Result<()> {
        let backend = self.backend();
        let live = self.open.lock().unwrap().live_id.clone();
        let events = self.events.clone();
        let open = self.open.clone();
        // Stopping is the user ending the turn: waiting for the backend to
        // confirm would leave the spinner running on a turn nobody is watching.
        self.open.lock().unwrap().conversation.finish_turn();
        push_transcript(&events, &open);
        self.runtime.spawn(async move {
            let Some(live) = live else {
                return;
            };
            if let Err(error) = backend.lock().await.interrupt(&live).await {
                push_error(&events, format!("could not interrupt: {error}"));
            }
        });
        Ok(())
    }

    fn answer(&self, request_id: String, answer: String) {
        let backend = self.backend();
        let events = self.events.clone();
        self.runtime.spawn(async move {
            if let Err(error) = backend
                .lock()
                .await
                .respond_to_interaction(&request_id, &answer)
                .await
            {
                push_error(&events, format!("could not answer: {error}"));
            }
        });
    }

    fn push(&self, event: Value) {
        push(&self.events, event);
    }
}

/// The preferences the frontend draws from, as one payload.
fn settings_payload(settings: &Settings) -> Value {
    json!({
        "event": "settings",
        "backend": settings.backend_kind.id(),
        "host": settings.backend_host,
        "port": settings.backend_port,
        "use_tls": settings.backend_use_tls,
        "credential": settings.session_token,
        "workspace": settings.workspace_path,
        "font_size": settings.font_size.id(),
        "show_tool_calls": settings.show_tool_calls,
    })
}

/// Which backends exist on a phone.
///
/// Codex CLI, Claude Code and Pi are local subprocesses, and `hermes serve` is
/// launched as one too. None of those binaries exist on Android and an app
/// cannot spawn them, so the mobile client offers the network backends only —
/// which is why this list is shorter than `BackendKind::ALL`.
fn parse_kind(id: &str) -> Result<BackendKind> {
    match id {
        "hermes" => Ok(BackendKind::Hermes),
        "opencode" => Ok(BackendKind::OpenCode),
        "mimocode" => Ok(BackendKind::MiMoCode),
        "openclaw" => Ok(BackendKind::OpenClaw),
        other => Err(anyhow!(
            "{other} runs as a local process and is not available on mobile"
        )),
    }
}

fn required(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("command needs a {key}"))
}

fn transcript(messages: &[ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .filter(|message| !message.is_empty_shell())
        .map(|message| {
            json!({
                "id": message.id,
                "role": role_name(message.role),
                "content": message.content,
                "streaming": message.is_streaming,
                "tools": message.tool_calls.iter().map(|tool| json!({
                    "name": tool.name,
                    "status": tool.status,
                    "detail": tool.detail,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn role_name(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool => "tool",
    }
}

/// Hand the whole message list over. Sent when something was added, removed,
/// completed or gained a tool call; text that merely streamed further is sent as
/// a [`delta`] instead, so a long reply costs one small event per poll rather
/// than a copy of the transcript per token.
fn push_transcript(events: &Arc<Mutex<Vec<Value>>>, open: &Arc<Mutex<OpenSession>>) {
    let items = {
        let open = open.lock().unwrap();
        transcript(open.conversation.messages())
    };
    push(events, json!({ "event": "transcript", "items": items }));
}

/// Fold one backend event into the queue and the open-session state.
fn apply(events: &Arc<Mutex<Vec<Value>>>, open: &Arc<Mutex<OpenSession>>, event: AgentEvent) {
    match event {
        AgentEvent::Connected => push(
            events,
            json!({ "event": "connection", "state": "connected" }),
        ),
        AgentEvent::SessionInfo(id) => {
            open.lock().unwrap().live_id = Some(id.clone());
            push(events, json!({ "event": "session", "id": id }));
        }
        AgentEvent::SessionsChanged => push(events, json!({ "event": "sessions_changed" })),
        AgentEvent::Clarify {
            question,
            choices,
            request_id,
            ..
        } => push(
            events,
            json!({
                "event": "clarify",
                "request_id": request_id,
                "question": question,
                "choices": choices,
            }),
        ),
        AgentEvent::Disconnected => push(
            events,
            json!({ "event": "connection", "state": "disconnected" }),
        ),
        // A transport failure mid-turn has to stop the spinner too: the reply is
        // never coming, and the reconnect that follows starts a new turn.
        AgentEvent::Failed(message) => {
            let finished = {
                let mut open = open.lock().unwrap();
                if open.conversation.is_sending() {
                    open.conversation.finish_turn();
                    true
                } else {
                    false
                }
            };
            if finished {
                push_transcript(events, open);
            }
            push_error(events, message);
        }
        other => {
            let change = {
                let mut open = open.lock().unwrap();
                // A session being opened has no live connection yet, and until it
                // has one any event still in flight belongs to the session the
                // user just left. Folding it in would write one chat's reply into
                // another's transcript.
                if !open.accepts_turn_events() {
                    return;
                }
                open.conversation.apply(&other)
            };
            match change {
                ConversationChange::None => {}
                ConversationChange::Streaming { id, text } => push(
                    events,
                    json!({ "event": "assistant", "id": id, "text": text, "done": false }),
                ),
                ConversationChange::Structure => push_transcript(events, open),
            }
        }
    }
}

fn push(events: &Arc<Mutex<Vec<Value>>>, event: Value) {
    let mut queue = events.lock().unwrap();
    // A streaming delta replaces the previous one for the same message instead of
    // queueing behind it, so a long reply costs one slot rather than one slot per
    // token. A delta for a *different* message is a new message's first words and
    // must not swallow the one before it.
    let supersedes_previous = event.get("event").and_then(Value::as_str) == Some("assistant")
        && event.get("done").and_then(Value::as_bool) == Some(false)
        && queue.last().is_some_and(|last| {
            last.get("event").and_then(Value::as_str) == Some("assistant")
                && last.get("done").and_then(Value::as_bool) == Some(false)
                && last.get("id") == event.get("id")
        });
    if supersedes_previous {
        queue.pop();
    }
    queue.push(event);
    if queue.len() > MAX_QUEUED_EVENTS {
        let overflow = queue.len() - MAX_QUEUED_EVENTS;
        queue.drain(0..overflow);
    }
}

fn push_error(events: &Arc<Mutex<Vec<Value>>>, message: String) {
    push(events, json!({ "event": "error", "message": message }));
}

// --------------------------------------------------------------- markdown

/// Parse the markdown of one message into blocks the frontend draws.
///
/// The frontend asks for this rather than receiving it with every transcript:
/// parsing is instant and local, while sending the parsed form of every message
/// would put a second copy of the whole conversation in every snapshot.
fn blocks(text: &str) -> Vec<Value> {
    markdown::parse(text)
        .into_iter()
        .map(|block| match block {
            MarkdownBlock::Heading(level, text) => {
                json!({ "kind": "heading", "level": level, "runs": runs(&text) })
            }
            MarkdownBlock::Paragraph(text) => json!({ "kind": "paragraph", "runs": runs(&text) }),
            MarkdownBlock::Bullet(text) => json!({ "kind": "bullet", "runs": runs(&text) }),
            MarkdownBlock::Numbered(number, text) => {
                json!({ "kind": "numbered", "number": number, "runs": runs(&text) })
            }
            MarkdownBlock::Quote(text) => json!({ "kind": "quote", "runs": runs(&text) }),
            MarkdownBlock::Code(text) => json!({ "kind": "code", "text": text }),
            MarkdownBlock::Table(headers, rows) => json!({
                "kind": "table",
                "headers": headers.iter().map(|cell| runs(cell)).collect::<Vec<_>>(),
                "rows": rows
                    .iter()
                    .map(|row| row.iter().map(|cell| runs(cell)).collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
            }),
            MarkdownBlock::Separator => json!({ "kind": "separator" }),
        })
        .collect()
}

/// Inline markdown as a list of styled runs.
///
/// The parser reports styles as byte ranges into the text with the delimiters
/// removed. Byte offsets are the wrong currency to hand Dart — a `String` is
/// indexed in UTF-16 code units, so one emoji shifts every offset after it — so
/// each run carries its own text instead and the frontend only has to append.
fn runs(text: &str) -> Vec<Value> {
    let inline = markdown::parse_inline(text);
    let visible = inline.text.as_str();
    let mut cuts: Vec<usize> = vec![0, visible.len()];
    for style in &inline.spans {
        cuts.push(style.range.start);
        cuts.push(style.range.end);
    }
    for link in &inline.links {
        cuts.push(link.range.start);
        cuts.push(link.range.end);
    }
    cuts.retain(|cut| *cut <= visible.len() && visible.is_char_boundary(*cut));
    cuts.sort_unstable();
    cuts.dedup();

    let mut out = Vec::with_capacity(cuts.len().saturating_sub(1));
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        let styles: Vec<&str> = inline
            .spans
            .iter()
            .filter(|style| style.range.start <= start && style.range.end >= end)
            .map(|style| style_name(style.style))
            .collect();
        let url = inline
            .links
            .iter()
            .find(|link| link.range.start <= start && link.range.end >= end)
            .map(|link| link.url.clone());
        out.push(json!({
            "text": &visible[start..end],
            "styles": styles,
            "url": url,
        }));
    }
    out
}

fn style_name(style: markdown::InlineStyle) -> &'static str {
    match style {
        markdown::InlineStyle::Strong => "strong",
        markdown::InlineStyle::Emphasis => "emphasis",
        markdown::InlineStyle::Code => "code",
        markdown::InlineStyle::Strikethrough => "strikethrough",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> Arc<Mutex<Vec<Value>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn names(events: &Arc<Mutex<Vec<Value>>>) -> Vec<String> {
        events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event["event"].as_str().unwrap_or("?").to_string())
            .collect()
    }

    /// The three CLI backends are local subprocesses and `hermes serve` is
    /// launched as one, so none of them can exist on a phone. Offering them
    /// would produce a connection that fails for a reason the user cannot act
    /// on, so they are rejected at the edge.
    #[test]
    fn only_the_network_backends_are_offered_on_mobile() {
        for id in ["hermes", "opencode", "mimocode", "openclaw"] {
            assert!(parse_kind(id).is_ok(), "{id} should be available");
        }
        for id in ["codex", "claudecode", "pi"] {
            let error = parse_kind(id).unwrap_err().to_string();
            assert!(error.contains("local process"), "{id}: {error}");
        }
        assert!(parse_kind("nonsense").is_err());
    }

    /// A reply arrives as one delta per token, and each delta carries the whole
    /// text so far. Queueing them all would grow the queue by a full copy of the
    /// reply per token and hand Flutter a backlog of stale partial replies.
    #[test]
    fn streaming_deltas_do_not_pile_up_in_the_queue() {
        let events = queue();
        for text in ["科", "科研", "科研模式", "科研模式已进入"] {
            push(
                &events,
                json!({ "event": "assistant", "id": "m1", "text": text, "done": false }),
            );
        }
        assert_eq!(names(&events), vec!["assistant"]);
        assert_eq!(events.lock().unwrap()[0]["text"], "科研模式已进入");
    }

    /// Two messages streaming one after the other are two messages: the second
    /// must not fold itself into the first one's delta.
    #[test]
    fn a_delta_for_another_message_is_a_separate_event() {
        let events = queue();
        push(
            &events,
            json!({ "event": "assistant", "id": "m1", "text": "第一段", "done": false }),
        );
        push(
            &events,
            json!({ "event": "assistant", "id": "m2", "text": "第二段", "done": false }),
        );
        assert_eq!(names(&events), vec!["assistant", "assistant"]);
    }

    /// Coalescing must only ever swallow the *immediately* previous delta: an
    /// error or a tool call that arrived mid-stream has to survive, or the user
    /// never learns about it.
    #[test]
    fn an_event_between_two_deltas_is_kept() {
        let events = queue();
        push(
            &events,
            json!({ "event": "assistant", "id": "m1", "text": "一", "done": false }),
        );
        push(
            &events,
            json!({ "event": "tool", "name": "read", "status": "ok", "detail": "" }),
        );
        push(
            &events,
            json!({ "event": "assistant", "id": "m1", "text": "一二", "done": false }),
        );
        assert_eq!(names(&events), vec!["assistant", "tool", "assistant"]);
    }

    /// The final delta of a turn is the authoritative text and must not be
    /// folded into the one before it, or a short reply could lose its last
    /// chunk.
    #[test]
    fn the_finished_reply_is_kept_next_to_the_last_delta() {
        let events = queue();
        push(
            &events,
            json!({ "event": "assistant", "id": "m1", "text": "一", "done": false }),
        );
        push(
            &events,
            json!({ "event": "assistant", "id": "m1", "text": "一二", "done": true }),
        );
        assert_eq!(names(&events), vec!["assistant", "assistant"]);
    }

    /// A Flutter side that stops polling (backgrounded app) must not let the
    /// queue grow without bound.
    #[test]
    fn the_queue_is_capped() {
        let events = queue();
        for index in 0..(MAX_QUEUED_EVENTS + 50) {
            push(
                &events,
                json!({ "event": "tool", "name": index.to_string() }),
            );
        }
        assert_eq!(events.lock().unwrap().len(), MAX_QUEUED_EVENTS);
    }

    #[test]
    fn a_command_without_its_argument_is_rejected() {
        assert!(required(&json!({ "cmd": "send" }), "text").is_err());
        assert_eq!(required(&json!({ "text": "hi" }), "text").unwrap(), "hi");
    }

    /// Byte offsets from the parser would land in the middle of a character once
    /// Dart — which indexes UTF-16 — walked past an emoji. The runs carry their
    /// own text so nothing has to be counted on the other side.
    #[test]
    fn inline_runs_carry_their_own_text_and_nothing_is_lost() {
        let parsed = runs("你好 👋 **粗体** 和 `代码` 还有 [链接](https://example.com)");
        let text: String = parsed
            .iter()
            .map(|run| run["text"].as_str().unwrap())
            .collect();
        assert!(
            text.contains("你好 👋 粗体 和 代码 还有 链接"),
            "delimiters should be gone and text intact: {text:?}"
        );
        assert!(
            parsed.iter().any(|run| run["styles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "strong")),
            "no run was marked strong: {parsed:#?}"
        );
        assert!(
            parsed.iter().any(|run| run["styles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "code")),
            "no run was marked code: {parsed:#?}"
        );
        assert!(
            parsed
                .iter()
                .any(|run| run["url"].as_str() == Some("https://example.com")),
            "the link was lost: {parsed:#?}"
        );
    }

    /// Plain text is one run, so the frontend has a single path for everything.
    #[test]
    fn plain_text_is_a_single_run() {
        let parsed = runs("就是一句话");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["text"], "就是一句话");
        assert!(parsed[0]["styles"].as_array().unwrap().is_empty());
    }

    /// While a session is being opened there is no live connection yet, so
    /// whatever is still in flight belongs to the session that was just left.
    #[test]
    fn turn_events_are_dropped_while_a_session_is_being_opened() {
        let mut open = OpenSession::default();
        assert!(
            open.accepts_turn_events(),
            "a brand new chat accepts events"
        );

        open.stored_id = Some("b".into());
        assert!(
            !open.accepts_turn_events(),
            "events arriving while a session is opening belong to the old one"
        );

        open.live_id = Some("b".into());
        assert!(open.accepts_turn_events(), "a resumed session accepts them");
    }

    /// Switching away and back must not lose what the client already drew.
    #[test]
    fn switching_sessions_keeps_each_conversation() {
        let mut open = OpenSession {
            stored_id: Some("a".into()),
            live_id: Some("a".into()),
            ..Default::default()
        };
        open.conversation.begin_turn_with("在 A 里说的话");

        open.switch_to(Some("b"));
        open.stored_id = Some("b".into());
        open.live_id = None;
        assert!(open.conversation.messages().is_empty(), "B starts empty");

        open.conversation.begin_turn_with("在 B 里说的话");
        open.live_id = Some("b".into());
        open.switch_to(Some("a"));
        assert_eq!(open.conversation.messages().len(), 1);
        assert_eq!(open.conversation.messages()[0].content, "在 A 里说的话");
    }

    /// Only the session on screen and the one just left are ever wanted, so the
    /// map of parked conversations does not grow with every switch.
    #[test]
    fn the_parked_conversations_are_capped() {
        let mut open = OpenSession::default();
        for index in 0..(PARKED_LIMIT + 4) {
            open.stored_id = Some(format!("s{index}"));
            open.switch_to(Some(&format!("next{index}")));
        }
        assert!(
            open.parked.len() <= PARKED_LIMIT,
            "{} conversations were kept",
            open.parked.len()
        );
    }

    /// A message is rendered from blocks, and the ones with text inside them
    /// keep their structure.
    #[test]
    fn markdown_blocks_keep_their_shape() {
        let parsed = blocks("# 标题\n\n- 一项\n\n```\n代码\n```\n");
        let kinds: Vec<&str> = parsed
            .iter()
            .map(|block| block["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, vec!["heading", "bullet", "code"]);
        assert_eq!(parsed[0]["level"], 1);
        assert_eq!(parsed[2]["text"], "代码");
    }

    /// The bug that made one chat's messages appear in another: with no live
    /// connection to the session on screen, the bridge started a *new* session
    /// and sent the prompt there. The user was reading one conversation and the
    /// prompt went into a different one — on a phone, whose socket drops
    /// constantly, this happened often enough to look like sessions crossing.
    #[test]
    fn a_prompt_goes_to_the_session_on_screen_not_to_a_new_one() {
        assert_eq!(
            send_target(None, Some("agent:main:van-goal:mine")),
            SendTarget::Resume("agent:main:van-goal:mine".into()),
            "a session that exists on the backend must be resumed, never replaced"
        );
        assert_eq!(
            send_target(Some("live-9"), Some("stored-9")),
            SendTarget::Live("live-9".into())
        );
        assert_eq!(
            send_target(None, None),
            SendTarget::Create,
            "only a chat with nothing on screen needs a session created"
        );
    }

    /// Coming back to the conversation the user was in, rather than to an empty
    /// chat whose first message would create yet another session.
    #[test]
    fn the_open_session_is_remembered_and_forgotten() {
        let dir = std::env::temp_dir().join(format!("van-goal-remember-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let settings = Mutex::new(Settings {
            backend_kind: BackendKind::OpenClaw,
            ..Settings::default()
        });

        assert_eq!(saved_session(&settings), None, "nothing is open yet");

        remember_session(&settings, "agent:main:van-goal:a");
        assert_eq!(
            saved_session(&settings).as_deref(),
            Some("agent:main:van-goal:a")
        );

        remember_session(&settings, "agent:main:van-goal:b");
        assert_eq!(
            saved_session(&settings).as_deref(),
            Some("agent:main:van-goal:b")
        );

        forget_session(&settings);
        assert_eq!(
            saved_session(&settings),
            None,
            "a new chat forgets the old one"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session key is only meaningful to the backend that issued it, so a key
    /// left over from another backend must not be resumed here.
    #[test]
    fn a_saved_session_from_another_backend_is_not_reopened() {
        let settings = Mutex::new(Settings {
            backend_kind: BackendKind::OpenClaw,
            last_session: Some(SavedSession {
                backend: "hermes".into(),
                id: "live-7".into(),
            }),
            ..Settings::default()
        });

        assert_eq!(saved_session(&settings), None);
    }
}
