use super::session_scope::belongs_to_session;
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
    http: reqwest::Client,
    shared: Arc<Mutex<SharedState>>,
    event_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    abort_flag: Arc<std::sync::atomic::AtomicBool>,
    current_config: Option<BackendConfig>,
}

impl OpenCodeBackend {
    pub fn new(display_name: &'static str) -> Self {
        Self {
            display_name,
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

    fn auth_header(&self, credential: &str) -> Option<String> {
        if credential.is_empty() {
            None
        } else {
            Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("opencode:{credential}"))
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
        let rows = value
            .as_array()
            .ok_or_else(|| anyhow!("{} returned an invalid message list.", self.display_name))?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let info = row.get("info")?;
                let role = MessageRole::parse(json_str(info, "role").as_deref()?)?;
                let parts = row.get("parts")?.as_array()?;
                let text = parts
                    .iter()
                    .filter(|part| json_str(part, "type").as_deref() == Some("text"))
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

async fn run_event_stream(
    config: BackendConfig,
    events: UnboundedSender<AgentEvent>,
    shared: Arc<Mutex<SharedState>>,
    abort_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let client = reqwest::Client::builder().build().unwrap_or_default();
    let mut request = client
        .get(format!("{}/event", config.base_url))
        .header("Accept", "text/event-stream");
    if !config.credential.is_empty() {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("opencode:{}", config.credential));
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
            "OpenCode returned HTTP {} for event stream",
            response.status().as_u16()
        )));
        return;
    }

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
    // into the open transcript. See `session_scope`.
    {
        let state = shared.lock().unwrap_or_else(|e| e.into_inner());
        if !belongs_to_session(
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
