use super::session_scope::belongs_to_open_session;
use super::{json_str, normalize_seconds, pretty_json};
use crate::models::*;
use anyhow::{anyhow, Result};
use base64::Engine;
use futures::channel::mpsc::UnboundedSender;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// The username each server pairs with the configured password in its
/// `Authorization: Basic` header. They are not interchangeable: a
/// password-protected `mimo serve` answers `200` to `mimocode:<password>` and
/// `401` to `opencode:<password>`, so a shared name authenticates against
/// neither server's other credentials.
pub const OPENCODE_AUTH_USER: &str = "opencode";
pub const MIMOCODE_AUTH_USER: &str = "mimocode";

#[derive(Default)]
struct SharedState {
    active_session_id: Option<String>,
    permission_sessions: HashMap<String, String>,
    assistant_message_ids: HashSet<String>,
    text_by_part_id: HashMap<String, String>,
}

/// Adapter for the structured HTTP + SSE interface exposed by `opencode serve`
/// (and the OpenCode-compatible `mimo serve`).
pub struct OpenCodeBackend {
    display_name: &'static str,
    /// The username this server expects beside the configured password.
    auth_username: &'static str,
    http: reqwest::Client,
    shared: Arc<Mutex<SharedState>>,
    event_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    abort_flag: Arc<std::sync::atomic::AtomicBool>,
    current_config: Option<BackendConfig>,
}

impl OpenCodeBackend {
    pub fn new(display_name: &'static str, auth_username: &'static str) -> Self {
        Self {
            display_name,
            auth_username,
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .unwrap_or_default(),
            shared: Arc::new(Mutex::new(SharedState::default())),
            event_task: Arc::new(Mutex::new(None)),
            abort_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            current_config: None,
        }
    }

    /// The username the readiness check and this adapter have to agree on.
    pub fn auth_username(&self) -> &'static str {
        self.auth_username
    }

    fn auth_header(&self, credential: &str) -> Option<String> {
        if credential.is_empty() {
            None
        } else {
            Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD
                    .encode(format!("{}:{credential}", self.auth_username))
            ))
        }
    }

    async fn request_json(
        &self,
        config: &BackendConfig,
        path: &str,
        method: reqwest::Method,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let mut request = self
            .http
            .request(method, format!("{}{}", config.base_url, path))
            .header("Accept", "application/json");
        if let Some(auth) = self.auth_header(&config.credential) {
            request = request.header("Authorization", auth);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let data = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "{} returned HTTP {}{}",
                self.display_name,
                status.as_u16(),
                if data.is_empty() {
                    String::new()
                } else {
                    format!(": {}", data.chars().take(300).collect::<String>())
                }
            ));
        }
        if data.trim().is_empty() {
            return Ok(serde_json::Value::Null);
        }
        Ok(serde_json::from_str(&data)?)
    }

    pub async fn probe(&self, config: &BackendConfig) -> Result<()> {
        let value = self
            .request_json(config, "/global/health", reqwest::Method::GET, None)
            .await?;
        if value.get("healthy").and_then(|h| h.as_bool()) != Some(true) {
            return Err(anyhow!(
                "{} returned an invalid health response.",
                self.display_name
            ));
        }
        Ok(())
    }

    pub async fn list_sessions(&self, config: &BackendConfig) -> Result<Vec<AgentSession>> {
        let value = self
            .request_json(config, "/session", reqwest::Method::GET, None)
            .await?;
        let rows = value
            .as_array()
            .ok_or_else(|| anyhow!("{} returned an invalid session list.", self.display_name))?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let id = json_str(row, "id")?;
                let time = row.get("time").cloned().unwrap_or_default();
                Some(AgentSession {
                    id,
                    title: json_str(row, "title"),
                    cwd: json_str(row, "directory"),
                    model: None,
                    provider: Some(self.display_name.to_string()),
                    started_at: normalize_seconds(super::json_f64(&time, "created")),
                    last_active: normalize_seconds(super::json_f64(&time, "updated")),
                    message_count: None,
                    is_active: None,
                    archived: Some(false),
                    profile: None,
                    backend_id: None,
                    // Neither backend reports a session's token accounting yet.
                    used_tokens: None,
                    context_tokens: None,
                })
            })
            .collect())
    }

    pub async fn messages(
        &self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<Vec<ChatMessage>> {
        let value = self
            .request_json(
                config,
                &format!("/session/{}/message", urlencode(session_id)),
                reqwest::Method::GET,
                None,
            )
            .await?;
        messages_from_value(&value, self.display_name)
    }

    pub async fn connect(
        &mut self,
        config: BackendConfig,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        self.disconnect();
        self.probe(&config).await?;
        self.current_config = Some(config.clone());
        self.abort_flag
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let task = tokio::spawn(run_event_stream(
            config,
            events,
            self.shared.clone(),
            self.abort_flag.clone(),
            self.display_name,
            self.auth_username,
        ));
        *self.event_task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        Ok(())
    }

    pub async fn create_session(&mut self, config: &BackendConfig) -> Result<SessionIDs> {
        let value = self
            .request_json(
                config,
                "/session",
                reqwest::Method::POST,
                Some(serde_json::json!({ "title": "New Chat" })),
            )
            .await?;
        let session_id = json_str(&value, "id").ok_or_else(|| {
            anyhow!(
                "{} returned an invalid session creation.",
                self.display_name
            )
        })?;
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_session_id = Some(session_id.clone());
        Ok(SessionIDs {
            live_id: session_id.clone(),
            stored_id: Some(session_id),
        })
    }

    pub async fn resume_session(
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<SessionIDs> {
        self.request_json(
            config,
            &format!("/session/{}", urlencode(session_id)),
            reqwest::Method::GET,
            None,
        )
        .await?;
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_session_id = Some(session_id.to_string());
        Ok(SessionIDs {
            live_id: session_id.to_string(),
            stored_id: Some(session_id.to_string()),
        })
    }

    pub async fn submit_prompt(&mut self, session_id: &str, text: &str) -> Result<()> {
        let config = self
            .current_config
            .clone()
            .ok_or_else(|| anyhow!("No {} session is active.", self.display_name))?;
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_session_id = Some(session_id.to_string());
        self.request_json(
            &config,
            &format!("/session/{}/prompt_async", urlencode(session_id)),
            reqwest::Method::POST,
            Some(serde_json::json!({
                "parts": [{ "type": "text", "text": text }]
            })),
        )
        .await?;
        Ok(())
    }

    pub async fn respond_to_interaction(&mut self, request_id: &str, answer: &str) -> Result<()> {
        let config = self
            .current_config
            .clone()
            .ok_or_else(|| anyhow!("No {} session is active.", self.display_name))?;
        let session_id = {
            let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            shared
                .permission_sessions
                .remove(request_id)
                .or_else(|| shared.active_session_id.clone())
        }
        .ok_or_else(|| anyhow!("No {} session is active.", self.display_name))?;
        self.request_json(
            &config,
            &format!(
                "/session/{}/permissions/{}",
                urlencode(&session_id),
                urlencode(request_id)
            ),
            reqwest::Method::POST,
            Some(serde_json::json!({
                "response": if answer.is_empty() { "reject" } else { answer }
            })),
        )
        .await?;
        Ok(())
    }

    pub async fn interrupt(&mut self, session_id: &str) -> Result<()> {
        let config = self
            .current_config
            .clone()
            .ok_or_else(|| anyhow!("No {} session is active.", self.display_name))?;
        self.request_json(
            &config,
            &format!("/session/{}/abort", urlencode(session_id)),
            reqwest::Method::POST,
            None,
        )
        .await?;
        Ok(())
    }

    pub fn clear_session_scope(&mut self) {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_session_id = None;
    }

    pub fn disconnect(&mut self) {
        if let Some(task) = self
            .event_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            task.abort();
        }
        self.abort_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.current_config = None;
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.active_session_id = None;
        shared.permission_sessions.clear();
        shared.assistant_message_ids.clear();
        shared.text_by_part_id.clear();
    }
}

fn urlencode(value: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("v", value)
        .finish()
        .trim_start_matches("v=")
        .to_string()
}

/// Read a session's transcript out of the message list both servers return:
/// `info.role` plus the message's parts, of which only the text parts are the
/// conversation.
fn messages_from_value(
    value: &serde_json::Value,
    display_name: &str,
) -> Result<Vec<ChatMessage>> {
    let rows = value
        .as_array()
        .ok_or_else(|| anyhow!("{display_name} returned an invalid message list."))?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let info = row.get("info")?;
            let role = MessageRole::parse(json_str(info, "role").as_deref()?)?;
            let parts = row.get("parts")?.as_array()?;
            let text = parts
                .iter()
                .filter(|part| json_str(part, "type").as_deref() == Some("text"))
                .filter(|part| !is_injected_reminder(part, role))
                .filter_map(|part| json_str(part, "text"))
                .collect::<String>();
            if text.is_empty() {
                None
            } else {
                Some(ChatMessage::new(role, text))
            }
        })
        .collect())
}

/// Whether a part is the server talking to the model rather than the user
/// talking to it.
///
/// `mimo serve` appends its own instructions to the user's own message and
/// marks them `"synthetic": true` — a turn was measured carrying a
/// `<system-reminder>` that told the model to search its skills first. Reopening
/// that session would otherwise show those instructions in the user's bubble, as
/// though they had been typed. Parts of the model's own reply are left alone:
/// nothing measured marks those synthetic, and dropping one would lose an answer.
fn is_injected_reminder(part: &serde_json::Value, role: MessageRole) -> bool {
    role == MessageRole::User
        && part
            .get("synthetic")
            .and_then(|flag| flag.as_bool())
            .unwrap_or(false)
}

async fn run_event_stream(
    config: BackendConfig,
    events: UnboundedSender<AgentEvent>,
    shared: Arc<Mutex<SharedState>>,
    abort_flag: Arc<std::sync::atomic::AtomicBool>,
    display_name: &'static str,
    auth_username: &'static str,
) {
    let client = reqwest::Client::builder().build().unwrap_or_default();
    let mut request = client
        .get(format!("{}/event", config.base_url))
        .header("Accept", "text/event-stream");
    if !config.credential.is_empty() {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("{auth_username}:{}", config.credential));
        request = request.header("Authorization", format!("Basic {encoded}"));
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            let _ = events.unbounded_send(AgentEvent::Failed(error.to_string()));
            return;
        }
    };
    if !response.status().is_success() {
        let _ = events.unbounded_send(AgentEvent::Failed(format!(
            "{} returned HTTP {} for event stream",
            display_name,
            response.status().as_u16()
        )));
        return;
    }

    // The stream is open, so the transport is up. Without this the frontends
    // never leave "Connecting": this is the only point at which the adapter
    // knows the connection it was asked for is actually there.
    let _ = events.unbounded_send(AgentEvent::Connected);

    let mut byte_stream = response.bytes_stream();
    let mut buffer = String::new();
    loop {
        if abort_flag.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        tokio::select! {
            chunk = byte_stream.next() => {
                let Some(chunk) = chunk else { break };
                let Ok(bytes) = chunk else { break };
                buffer.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(newline) = buffer.find('\n') {
                    let line: String = buffer.drain(..=newline).collect();
                    let line = line.trim_end();
                    if let Some(payload) = line.strip_prefix("data:") {
                        let payload = payload.trim();
                        if !payload.is_empty() {
                            handle_event_json(payload, &events, &shared);
                        }
                    }
                }
            }
        }
    }
    let _ = events.unbounded_send(AgentEvent::Disconnected);
}

fn handle_event_json(
    payload: &str,
    events: &UnboundedSender<AgentEvent>,
    shared: &Arc<Mutex<SharedState>>,
) {
    let Ok(event) = serde_json::from_str::<serde_json::Value>(payload) else {
        return;
    };
    let Some(event_type) = json_str(&event, "type") else {
        return;
    };
    let Some(properties) = event.get("properties").cloned() else {
        return;
    };

    let event_session_id = super::json_str(&properties, "sessionID")
        .or_else(|| {
            properties
                .get("part")
                .and_then(|part| json_str(part, "sessionID"))
        })
        .or_else(|| {
            properties
                .get("info")
                .and_then(|info| json_str(info, "sessionID"))
        });

    // The server's `/event` stream carries every session, including the ones the
    // user is not looking at: without this, another session's reply is written
    // into the open transcript. See `session_scope` — the strict form, because
    // "no session subscribed yet" is the state of a client that has just
    // reconnected, not a licence to accept everything.
    {
        let state = shared.lock().unwrap_or_else(|e| e.into_inner());
        if !belongs_to_open_session(
            state.active_session_id.as_deref(),
            event_session_id.as_deref(),
        ) {
            return;
        }
    }

    match event_type.as_str() {
        "message.updated" => {
            let Some(info) = properties.get("info") else {
                return;
            };
            if json_str(info, "role").as_deref() != Some("assistant") {
                return;
            }
            let Some(message_id) = json_str(info, "id") else {
                return;
            };
            let inserted = shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .assistant_message_ids
                .insert(message_id);
            if inserted {
                let _ = events.unbounded_send(AgentEvent::MessageStart);
            }
        }
        "message.part.updated" => {
            let Some(part) = properties.get("part") else {
                return;
            };
            if let Some(message_id) = json_str(part, "messageID") {
                let known = shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .assistant_message_ids
                    .contains(&message_id);
                if !known {
                    return;
                }
            }
            match json_str(part, "type").as_deref() {
                Some("text") => {
                    if let Some(delta) = json_str(&properties, "delta").filter(|d| !d.is_empty()) {
                        let _ = events.unbounded_send(AgentEvent::MessageDelta {
                            text: delta,
                            source: DeltaSource::EventStream,
                        });
                    } else if let (Some(part_id), Some(full_text)) =
                        (json_str(part, "id"), json_str(part, "text"))
                    {
                        let previous = {
                            let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                            state
                                .text_by_part_id
                                .insert(part_id.clone(), full_text.clone())
                                .unwrap_or_default()
                        };
                        if let Some(delta) = full_text.strip_prefix(&previous) {
                            if !delta.is_empty() {
                                let _ = events.unbounded_send(AgentEvent::MessageDelta {
                                    text: delta.to_string(),
                                    source: DeltaSource::EventStream,
                                });
                            }
                        }
                    }
                }
                Some("tool") => {
                    let name = json_str(part, "tool").unwrap_or_else(|| "tool".into());
                    let state = part.get("state").cloned().unwrap_or_default();
                    let status = json_str(&state, "status").unwrap_or_else(|| "updated".into());
                    let _ = events.unbounded_send(AgentEvent::Tool(ToolCallRecord::new(
                        name,
                        status,
                        pretty_json(part),
                    )));
                }
                _ => {}
            }
        }
        "session.idle" => {
            let _ = events.unbounded_send(AgentEvent::MessageComplete(None));
            shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .text_by_part_id
                .clear();
        }
        "session.error" => {
            let _ = events.unbounded_send(AgentEvent::TurnFailed(pretty_json(&properties)));
        }
        "permission.updated" => {
            let Some(request_id) = json_str(&properties, "id") else {
                return;
            };
            let session_id = json_str(&properties, "sessionID");
            if let Some(session_id) = &session_id {
                shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .permission_sessions
                    .insert(request_id.clone(), session_id.clone());
            }
            let _ = events.unbounded_send(AgentEvent::Clarify {
                question: json_str(&properties, "title")
                    .unwrap_or_else(|| "OpenCode requests permission".into()),
                choices: vec!["once".into(), "always".into(), "reject".into()],
                request_id,
                session_id,
            });
        }
        _ => {}
    }
}


#[cfg(test)]
mod tests {
    use super::{handle_event_json, SharedState};
    use crate::models::{AgentEvent, DeltaSource};
    use futures::channel::mpsc::{unbounded, UnboundedSender};
    use std::sync::{Arc, Mutex};

    const SESSION: &str = "ses_f38608c86ffen4dUyi7sKmrq5x";
    const OTHER_SESSION: &str = "ses_070fe86f0ffe7El4nrN45x5eYg";
    const USER_MESSAGE: &str = "msg_0c79f7fe9001Xx39w41s5ud1r1";
    const ASSISTANT_MESSAGE: &str = "msg_0c79f8012001t5E39aNf3cJOsF";
    const ASSISTANT_PART: &str = "prt_0c79f8006001rD9BP85NWJPX4K";

    /// A shared state scoped to the session on screen — the state a client is
    /// in right after opening a session — plus the channel the adapter would
    /// hand its events to.
    struct Probe {
        shared: Arc<Mutex<SharedState>>,
        tx: UnboundedSender<AgentEvent>,
        rx: futures::channel::mpsc::UnboundedReceiver<AgentEvent>,
    }

    impl Probe {
        fn new() -> Self {
            let (tx, rx) = unbounded();
            Self {
                shared: Arc::new(Mutex::new(SharedState {
                    active_session_id: Some(SESSION.to_string()),
                    ..SharedState::default()
                })),
                tx,
                rx,
            }
        }

        /// Feed the captured payloads through the adapter in order.
        fn feed(&mut self, payloads: &[String]) {
            for payload in payloads {
                handle_event_json(payload, &self.tx, &self.shared);
            }
        }

        fn events(&mut self) -> Vec<AgentEvent> {
            let mut out = Vec::new();
            while let Ok(event) = self.rx.try_recv() {
                out.push(event);
            }
            out
        }
    }

    /// The end of a turn exactly as `mimo serve` 0.1.8 streams it, copied from a
    /// live capture. The user's own message and prompt part arrive first and
    /// must not read as the reply; the reply's text part carries the whole text
    /// so far with no `delta` field, which is where mimo differs from OpenCode
    /// and where a delta-only reader would show nothing at all.
    #[test]
    fn a_mimo_reply_streams_as_the_whole_part_it_reports() {
        let mut probe = Probe::new();
        probe.feed(&[
            // The user's message, then the prompt as its own text part.
            format!(
                r#"{{"type":"message.updated","properties":{{"sessionID":"{SESSION}","info":{{"id":"{USER_MESSAGE}","role":"user","sessionID":"{SESSION}","time":{{"created":1790055514089}},"agent":"build","model":{{"providerID":"mimo","modelID":"mimo-auto"}}}}}}}}"#
            ),
            format!(
                r#"{{"type":"message.part.updated","properties":{{"sessionID":"{SESSION}","part":{{"type":"text","text":"Reply with exactly one word: PONG. Do not use any tools.","messageID":"{USER_MESSAGE}","sessionID":"{SESSION}","id":"prt_0c79f7fea001aRRQn5TLKQeaU2"}},"time":1790055514095}}}}"#
            ),
            // MimoCode's own additions to the stream. None of them is a turn's
            // traffic, and all of them are interleaved with it.
            format!(
                r#"{{"type":"session.status","properties":{{"sessionID":"{SESSION}","status":{{"type":"busy"}}}}}}"#
            ),
            r#"{"type":"server.heartbeat","properties":{}}"#.to_string(),
            r#"{"type":"server.connected","properties":{}}"#.to_string(),
            r#"{"type":"metrics.agent_request","properties":{"providerID":"mimo"}}"#.to_string(),
            format!(r#"{{"type":"session.diff","properties":{{"sessionID":"{SESSION}","diff":[]}}}}"#),
            // The assistant message, then its text part growing.
            format!(
                r#"{{"type":"message.updated","properties":{{"sessionID":"{SESSION}","info":{{"id":"{ASSISTANT_MESSAGE}","parentID":"{USER_MESSAGE}","role":"assistant","agentID":"main","mode":"build","agent":"build","cost":0,"tokens":{{"input":0,"output":0,"reasoning":0,"cache":{{"read":0,"write":0}}}},"modelID":"mimo-auto","providerID":"mimo","time":{{"created":1790055514130}},"sessionID":"{SESSION}"}}}}}}"#
            ),
            format!(
                r#"{{"type":"message.part.updated","properties":{{"sessionID":"{SESSION}","part":{{"id":"{ASSISTANT_PART}","messageID":"{ASSISTANT_MESSAGE}","sessionID":"{SESSION}","type":"text","text":"PONG"}},"time":1790055514400}}}}"#
            ),
            format!(
                r#"{{"type":"message.part.updated","properties":{{"sessionID":"{SESSION}","part":{{"id":"{ASSISTANT_PART}","messageID":"{ASSISTANT_MESSAGE}","sessionID":"{SESSION}","type":"text","text":"PONG, as asked."}},"time":1790055514500}}}}"#
            ),
            format!(r#"{{"type":"session.idle","properties":{{"sessionID":"{SESSION}"}}}}"#),
        ]);

        match probe.events().as_slice() {
            [
                AgentEvent::MessageStart,
                AgentEvent::MessageDelta {
                    text: first,
                    source: DeltaSource::EventStream,
                },
                AgentEvent::MessageDelta {
                    text: second,
                    source: DeltaSource::EventStream,
                },
                AgentEvent::MessageComplete(None),
            ] => {
                assert_eq!(first, "PONG", "the reply's first report is its whole text");
                assert_eq!(
                    second, ", as asked.",
                    "a later report holds the whole part, so only the new tail \
                     belongs in the bubble"
                );
            }
            other => panic!("unexpected events from a mimo turn: {other:#?}"),
        }
    }

    /// Opening a session the server has already finished shows the same shape:
    /// the first report of a part is the whole text it holds.
    #[test]
    fn a_part_first_seen_whole_is_not_a_delta_against_nothing() {
        let mut probe = Probe::new();
        probe.feed(&[
            format!(
                r#"{{"type":"message.updated","properties":{{"sessionID":"{SESSION}","info":{{"id":"{ASSISTANT_MESSAGE}","role":"assistant","sessionID":"{SESSION}","time":{{"created":1790055514130}}}}}}}}"#
            ),
            format!(
                r#"{{"type":"message.part.updated","properties":{{"sessionID":"{SESSION}","part":{{"type":"text","text":"already finished","messageID":"{ASSISTANT_MESSAGE}","sessionID":"{SESSION}","id":"{ASSISTANT_PART}"}},"time":1790055514400}}}}"#
            ),
        ]);

        match probe.events().as_slice() {
            [AgentEvent::MessageStart, AgentEvent::MessageDelta { text, .. }] => {
                assert_eq!(text, "already finished");
            }
            other => panic!("unexpected events: {other:#?}"),
        }
    }

    /// A tool part is folded into the turn's one tool bubble, with the status
    /// the server reports for it.
    #[test]
    fn a_tool_part_arrives_as_a_tool_record() {
        let mut probe = Probe::new();
        probe.feed(&[
            format!(
                r#"{{"type":"message.updated","properties":{{"sessionID":"{SESSION}","info":{{"id":"{ASSISTANT_MESSAGE}","role":"assistant","sessionID":"{SESSION}"}}}}}}"#
            ),
            format!(
                r#"{{"type":"message.part.updated","properties":{{"sessionID":"{SESSION}","part":{{"id":"prt_tool","messageID":"{ASSISTANT_MESSAGE}","sessionID":"{SESSION}","type":"tool","callID":"call_1","tool":"Read","state":{{"status":"completed","title":"Read a file"}}}}}}}}"#
            ),
        ]);

        let events = probe.events();
        let [AgentEvent::MessageStart, AgentEvent::Tool(record)] = events.as_slice() else {
            panic!("unexpected events: {events:#?}");
        };
        assert_eq!(record.name, "Read");
        assert_eq!(record.status, "completed");
    }

    /// A failed turn reaches the transcript as the server's own report, not as a
    /// summary of it.
    #[test]
    fn a_turn_failure_carries_the_servers_own_error() {
        let mut probe = Probe::new();
        probe.feed(&[format!(
            r#"{{"type":"session.error","properties":{{"sessionID":"{SESSION}","error":{{"name":"UnknownError","data":{{"message":"unknown certificate verification error"}}}}}}}}"#
        )]);

        let events = probe.events();
        let [AgentEvent::TurnFailed(detail)] = events.as_slice() else {
            panic!("unexpected events: {events:#?}");
        };
        assert!(detail.contains("unknown certificate verification error"));
    }

    /// The stream carries every session the server has, so a second conversation
    /// must never be able to write into the open transcript.
    #[test]
    fn another_sessions_traffic_never_reaches_the_transcript() {
        let mut probe = Probe::new();
        probe.feed(&[
            format!(
                r#"{{"type":"message.updated","properties":{{"sessionID":"{OTHER_SESSION}","info":{{"id":"msg_elsewhere","role":"assistant","sessionID":"{OTHER_SESSION}"}}}}}}"#
            ),
            format!(
                r#"{{"type":"message.part.updated","properties":{{"sessionID":"{OTHER_SESSION}","part":{{"type":"text","text":"someone else's reply","messageID":"msg_elsewhere","sessionID":"{OTHER_SESSION}","id":"prt_elsewhere"}}}}}}"#
            ),
            format!(r#"{{"type":"session.idle","properties":{{"sessionID":"{OTHER_SESSION}"}}}}"#),
        ]);

        let events = probe.events();
        assert!(events.is_empty(), "another session leaked in: {events:#?}");
    }
}

#[cfg(test)]
mod history_tests {
    use super::messages_from_value;
    use crate::models::MessageRole;
    use serde_json::json;

    /// A `mimo serve` history, copied from a live capture: the user's message
    /// carries their own part and, after it, one the server injected and marked
    /// `synthetic`.
    #[test]
    fn an_injected_reminder_is_not_shown_as_the_users_own_words() {
        let value = json!([
            {
                "info": { "id": "msg_0c79f7fe9001", "role": "user", "sessionID": "ses_1" },
                "parts": [
                    {
                        "id": "prt_1",
                        "messageID": "msg_0c79f7fe9001",
                        "sessionID": "ses_1",
                        "type": "text",
                        "text": "reply with exactly: PROBE-OK"
                    },
                    {
                        "id": "prt_2",
                        "messageID": "msg_0c79f7fe9001",
                        "sessionID": "ses_1",
                        "type": "text",
                        "synthetic": true,
                        "text": "<system-reminder>\nSkill search trigger: this is the first user query in the session."
                    }
                ]
            }
        ]);

        let messages = messages_from_value(&value, "MiMoCode").expect("a message list");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(
            messages[0].content, "reply with exactly: PROBE-OK",
            "the server's own instructions must not be read back as the prompt"
        );
    }

    /// A message whose parts the server replaced entirely leaves nothing to
    /// show, and an empty bubble is worse than none.
    #[test]
    fn a_message_with_nothing_but_an_injected_part_is_dropped() {
        let value = json!([
            {
                "info": { "id": "msg_1", "role": "user", "sessionID": "ses_1" },
                "parts": [
                    { "id": "prt_1", "type": "text", "synthetic": true, "text": "<system-reminder>" }
                ]
            }
        ]);

        let messages = messages_from_value(&value, "MiMoCode").expect("a message list");
        assert!(messages.is_empty(), "an empty bubble was left behind: {messages:#?}");
    }

    /// The model's own reply is never filtered: a reasoning part and a tool part
    /// are not text, and the text it produced is what the user came to read.
    #[test]
    fn the_models_own_reply_is_kept_whole() {
        let value = json!([
            {
                "info": { "id": "msg_2", "role": "assistant", "sessionID": "ses_1" },
                "parts": [
                    { "id": "prt_1", "type": "reasoning", "text": "let me think" },
                    { "id": "prt_2", "type": "tool", "tool": "Read", "state": { "status": "completed" } },
                    { "id": "prt_3", "type": "text", "text": "Two files, read." },
                    { "id": "prt_4", "type": "text", "synthetic": false, "text": " Anything else?" }
                ]
            }
        ]);

        let messages = messages_from_value(&value, "MiMoCode").expect("a message list");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "Two files, read. Anything else?");
    }
}

#[cfg(test)]
mod stream_tests {
    use super::{run_event_stream, SharedState, MIMOCODE_AUTH_USER};
    use crate::models::{AgentEvent, BackendConfig};
    use base64::Engine;
    use futures::channel::mpsc::unbounded;
    use futures::StreamExt;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A server that answers the event stream, so the adapter's own request can
    /// be read back off the wire.
    async fn one_event_stream() -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let address = listener.local_addr().expect("an address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("a connection");
            let mut request = vec![0_u8; 4096];
            let read = socket.read(&mut request).await.expect("a request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                .await
                .expect("headers");
            socket
                .write_all(b"data: {\"type\":\"server.connected\",\"properties\":{}}\n\n")
                .await
                .expect("one event");
            socket.flush().await.expect("flush");
            String::from_utf8_lossy(&request[..read]).to_string()
        });
        (format!("http://{address}"), server)
    }

    /// Opening the stream is what tells a frontend the transport is up. Without
    /// it the status stays "Connecting" for the life of the connection, which is
    /// exactly what a user reads as the backend never having connected.
    #[tokio::test]
    async fn an_open_stream_reports_the_transport_up() {
        let (base_url, server) = one_event_stream().await;
        let (tx, mut rx) = unbounded();
        let config = BackendConfig {
            base_url,
            credential: "pw".into(),
            profile: None,
            workspace: None,
        };
        let task = tokio::spawn(run_event_stream(
            config,
            tx,
            Arc::new(Mutex::new(SharedState::default())),
            Arc::new(AtomicBool::new(false)),
            "MiMoCode",
            MIMOCODE_AUTH_USER,
        ));

        assert!(
            matches!(rx.next().await, Some(AgentEvent::Connected)),
            "the first thing the frontend hears must be that it is connected"
        );

        // The request itself, so the stream's own credentials are pinned to the
        // user the password is paired with.
        let request = server.await.expect("the server's capture");
        assert!(request.starts_with("GET /event "), "{request}");
        let expected = base64::engine::general_purpose::STANDARD.encode("mimocode:pw");
        // Header names arrive lowercased, so compare the whole request that way.
        assert!(
            request
                .to_lowercase()
                .contains(&format!("authorization: basic {}", expected.to_lowercase())),
            "{request}"
        );

        // The server closed the stream, which ends the task and says so.
        assert!(matches!(rx.next().await, Some(AgentEvent::Disconnected)));
        task.await.expect("the stream task");
    }

    /// A stream the server refuses is a failure, not a connection that is up.
    #[tokio::test]
    async fn a_refused_stream_is_reported_as_a_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let address = listener.local_addr().expect("an address");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("a connection");
            let mut request = vec![0_u8; 4096];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                .await;
        });

        let (tx, mut rx) = unbounded();
        let config = BackendConfig {
            base_url: format!("http://{address}"),
            credential: String::new(),
            profile: None,
            workspace: None,
        };
        tokio::spawn(run_event_stream(
            config,
            tx,
            Arc::new(Mutex::new(SharedState::default())),
            Arc::new(AtomicBool::new(false)),
            "MiMoCode",
            MIMOCODE_AUTH_USER,
        ));

        match rx.next().await {
            Some(AgentEvent::Failed(detail)) => {
                assert!(detail.contains("MiMoCode"), "{detail}");
                assert!(detail.contains("401"), "{detail}");
            }
            other => panic!("a refused stream must be a failure, got {other:?}"),
        }
    }
}
