//! The headless client behind the C ABI: owns the runtime, drives a
//! [`Backend`], and turns its events into a JSON queue the Flutter side drains.
//!
//! The Flutter side owns the widget state; this side owns the connection, the
//! session ids and the per-stream text buffers. Nothing here knows what a
//! message bubble is.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, Result};
use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;

use van_goal_core::agent::Backend;
use van_goal_core::chat::{best_stream_text, merge_stream_chunk};
use van_goal_core::models::{
    AgentEvent, BackendConfig, ChatMessage, DeltaSource, MessageRole, SessionIDs,
};
use van_goal_core::settings::{BackendKind, Settings};

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

pub struct Client {
    runtime: tokio::runtime::Runtime,
    settings: Mutex<Settings>,
    /// The backend, together with the kind it was built for: a `Backend` does
    /// not expose its own kind, and switching backends has to build a new one.
    backend: Mutex<Option<(BackendKind, Arc<AsyncMutex<Backend>>)>>,
    events: Arc<Mutex<Vec<Value>>>,
    open: Arc<Mutex<OpenSession>>,
}

/// State for the session the user is looking at.
#[derive(Default)]
struct OpenSession {
    live_id: Option<String>,
    stored_id: Option<String>,
    /// Text received per stream for the turn in flight; see
    /// [`van_goal_core::chat::best_stream_text`].
    streams: BTreeMap<DeltaSource, String>,
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
            settings: Mutex::new(Settings::default()),
            backend: Mutex::new(None),
            events: Arc::new(Mutex::new(Vec::new())),
            open: Arc::new(Mutex::new(OpenSession::default())),
        }
    }

    /// Handle one command from Dart.
    pub fn run(&self, command: &str) -> Result<()> {
        let value: Value = serde_json::from_str(command)
            .map_err(|error| anyhow!("command was not JSON: {error}"))?;
        match value.get("cmd").and_then(Value::as_str) {
            Some("settings") => self.report_settings(),
            Some("configure") => self.configure(&value),
            Some("connect") => self.connect(),
            Some("disconnect") => self.disconnect(),
            Some("list_sessions") => self.list_sessions(),
            Some("open_session") => {
                let id = required(&value, "id")?;
                self.open_session(id);
                Ok(())
            }
            Some("new_session") => self.new_session(),
            Some("send") => {
                let body = required(&value, "text")?;
                self.send(body);
                Ok(())
            }
            Some("interrupt") => self.interrupt(),
            Some("answer") => {
                let request = required(&value, "request_id")?;
                let answer = required(&value, "answer")?;
                self.answer(request, answer);
                Ok(())
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
    fn report_settings(&self) -> Result<()> {
        let settings = Settings::load();
        let payload = json!({
            "event": "settings",
            "backend": settings.backend_kind.id(),
            "host": settings.backend_host,
            "port": settings.backend_port,
            "use_tls": settings.backend_use_tls,
            "credential": settings.session_token,
            "workspace": settings.workspace_path,
        });
        *self.settings.lock().unwrap() = settings;
        self.push(payload);
        Ok(())
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
            // Best effort: where there is no writable app directory this fails
            // quietly and the app simply does not remember the connection.
            settings.save();
        }
        Ok(())
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

    // ------------------------------------------------------------- lifecycle

    /// Probe the address, then open the event stream onto the queue.
    fn connect(&self) -> Result<()> {
        let config = self.config();
        let backend = self.backend();
        let events = self.events.clone();
        let open = self.open.clone();
        push(
            &events,
            json!({ "event": "connection", "state": "connecting" }),
        );

        let (sender, mut receiver) = unbounded::<AgentEvent>();
        self.runtime.spawn(async move {
            if let Err(error) = backend.lock().await.probe(&config).await {
                push_error(&events, format!("could not reach the backend: {error}"));
                return;
            }
            if let Err(error) = backend.lock().await.connect(config, sender).await {
                push_error(&events, format!("could not open the event stream: {error}"));
                return;
            }
            // Drain the backend's events for as long as the stream lives.
            while let Some(event) = receiver.next().await {
                apply(&events, &open, event);
            }
            push(
                &events,
                json!({ "event": "connection", "state": "disconnected" }),
            );
        });
        Ok(())
    }

    fn disconnect(&self) -> Result<()> {
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
        let config = self.config();
        let backend = self.backend();
        let events = self.events.clone();
        let open = self.open.clone();
        self.runtime.spawn(async move {
            {
                let mut open = open.lock().unwrap();
                open.live_id = None;
                open.stored_id = Some(id.clone());
                open.streams.clear();
            }
            // The transcript as the backend has it, so there is something to
            // show while the live session is being resumed.
            if let Ok(messages) = backend.lock().await.messages(&config, &id).await {
                push(
                    &events,
                    json!({ "event": "transcript", "items": transcript(&messages) }),
                );
            }
            match backend.lock().await.resume_session(&config, &id).await {
                Ok(ids) => {
                    {
                        let mut open = open.lock().unwrap();
                        open.live_id = Some(ids.live_id.clone());
                        open.stored_id = ids.stored_id.clone().or(Some(id));
                    }
                    push(&events, json!({ "event": "session", "id": ids.live_id }));
                }
                Err(error) => push_error(&events, format!("could not resume the session: {error}")),
            }
        });
    }

    /// Start a chat that has no session on the backend yet. The session itself
    /// is created on the first send, so opening one costs nothing.
    fn new_session(&self) -> Result<()> {
        {
            let mut open = self.open.lock().unwrap();
            open.live_id = None;
            open.stored_id = None;
            open.streams.clear();
        }
        self.push(json!({ "event": "transcript", "items": [] }));
        Ok(())
    }

    // ------------------------------------------------------------------ turn

    fn send(&self, body: String) {
        let config = self.config();
        let backend = self.backend();
        let events = self.events.clone();
        let open = self.open.clone();
        self.runtime.spawn(async move {
            let existing = open.lock().unwrap().live_id.clone();
            let live = match existing {
                Some(live) => live,
                None => match backend.lock().await.create_session(&config).await {
                    Ok(SessionIDs { live_id, stored_id }) => {
                        let mut open = open.lock().unwrap();
                        open.live_id = Some(live_id.clone());
                        open.stored_id = stored_id.or_else(|| open.stored_id.take());
                        live_id
                    }
                    Err(error) => {
                        push_error(&events, format!("could not start a session: {error}"));
                        return;
                    }
                },
            };
            open.lock().unwrap().streams.clear();
            if let Err(error) = backend.lock().await.submit_prompt(&live, &body).await {
                push_error(&events, format!("could not send the prompt: {error}"));
            }
        });
    }

    fn interrupt(&self) -> Result<()> {
        let backend = self.backend();
        let live = self.open.lock().unwrap().live_id.clone();
        let events = self.events.clone();
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
        AgentEvent::MessageStart => push(events, json!({ "event": "assistant_start" })),
        AgentEvent::MessageDelta { text, source } => {
            let best = {
                let mut open = open.lock().unwrap();
                merge_stream_chunk(open.streams.entry(source).or_default(), &text);
                best_stream_text(&open.streams)
            };
            if !best.is_empty() {
                push(
                    events,
                    json!({ "event": "assistant", "text": best, "done": false }),
                );
            }
        }
        AgentEvent::MessageComplete(text) => {
            let best = {
                let mut open = open.lock().unwrap();
                let best = text.unwrap_or_else(|| best_stream_text(&open.streams));
                open.streams.clear();
                best
            };
            push(
                events,
                json!({ "event": "assistant", "text": best, "done": true }),
            );
        }
        AgentEvent::SessionsChanged => push(events, json!({ "event": "sessions_changed" })),
        AgentEvent::TurnFailed(message) => push_error(events, message),
        AgentEvent::Tool(record) => push(
            events,
            json!({
                "event": "tool",
                "name": record.name,
                "status": record.status,
                "detail": record.detail,
            }),
        ),
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
        AgentEvent::Failed(message) => push_error(events, message),
    }
}

fn push(events: &Arc<Mutex<Vec<Value>>>, event: Value) {
    let mut queue = events.lock().unwrap();
    // A streaming delta replaces the previous one instead of queueing behind it,
    // so a long reply costs one slot rather than one slot per token.
    let supersedes_previous = event.get("event").and_then(Value::as_str) == Some("assistant")
        && event.get("done").and_then(Value::as_bool) == Some(false)
        && queue
            .last()
            .is_some_and(|last| last.get("event").and_then(Value::as_str) == Some("assistant"));
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
                json!({ "event": "assistant", "text": text, "done": false }),
            );
        }
        assert_eq!(names(&events), vec!["assistant"]);
        assert_eq!(events.lock().unwrap()[0]["text"], "科研模式已进入");
    }

    /// Coalescing must only ever swallow the *immediately* previous delta: an
    /// error or a tool call that arrived mid-stream has to survive, or the user
    /// never learns about it.
    #[test]
    fn an_event_between_two_deltas_is_kept() {
        let events = queue();
        push(
            &events,
            json!({ "event": "assistant", "text": "一", "done": false }),
        );
        push(
            &events,
            json!({ "event": "tool", "name": "read", "status": "ok", "detail": "" }),
        );
        push(
            &events,
            json!({ "event": "assistant", "text": "一二", "done": false }),
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
            json!({ "event": "assistant", "text": "一", "done": false }),
        );
        push(
            &events,
            json!({ "event": "assistant", "text": "一二", "done": true }),
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
}
