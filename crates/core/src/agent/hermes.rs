use super::{json_str, pretty_json};
use crate::log_debug;
use crate::models::*;
use anyhow::{anyhow, Result};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub struct HermesBackend {
    http: reqwest::Client,
    gateway: GatewayHandle,
}

impl Default for HermesBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl HermesBackend {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .unwrap_or_default(),
            gateway: GatewayHandle::new(),
        }
    }

    pub async fn probe(&mut self, config: &BackendConfig) -> Result<()> {
        let status: serde_json::Value = self
            .http
            .get(format!("{}/api/status", config.base_url))
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let _ = status;
        Ok(())
    }

    pub async fn discover_credential(&mut self, base_url: &str) -> Result<String> {
        log_debug!("http", "discover session token from dashboard html");
        let html = self
            .http
            .get(base_url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let re = regex::Regex::new(r#"window\.__HERMES_SESSION_TOKEN__="([^"]+)""#)?;
        if let Some(captures) = re.captures(&html) {
            if let Some(token) = captures.get(1) {
                log_debug!(
                    "http",
                    "session token discovered length={}",
                    token.as_str().len()
                );
                return Ok(token.as_str().to_string());
            }
        }
        Err(anyhow!(
            "Could not discover a loopback session token. Open settings and paste a token, or start a local loopback Hermes server."
        ))
    }

    pub async fn list_sessions(&mut self, config: &BackendConfig) -> Result<Vec<AgentSession>> {
        #[derive(Deserialize)]
        struct SessionListResponse {
            #[serde(default)]
            sessions: Vec<WireSession>,
        }
        #[derive(Deserialize)]
        struct WireSession {
            id: String,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            cwd: Option<String>,
            #[serde(default)]
            model: Option<String>,
            #[serde(default)]
            provider: Option<String>,
            #[serde(rename = "started_at", default)]
            started_at: Option<f64>,
            #[serde(rename = "last_active", default)]
            last_active: Option<f64>,
            #[serde(rename = "message_count", default)]
            message_count: Option<i64>,
            #[serde(rename = "is_active", default)]
            is_active: Option<bool>,
            #[serde(default)]
            archived: Option<bool>,
            #[serde(default)]
            profile: Option<String>,
        }

        let mut request = self
            .http
            .get(format!("{}/api/sessions", config.base_url))
            .query(&[
                ("limit", "60"),
                ("offset", "0"),
                ("min_messages", "0"),
                ("order", "recent"),
            ])
            .header("Accept", "application/json");
        if let Some(profile) = config.profile.as_deref().filter(|p| !p.is_empty()) {
            request = request.query(&[("profile", profile)]);
        }
        if !config.credential.is_empty() {
            request = request.header("X-Hermes-Session-Token", &config.credential);
        }
        let response: SessionListResponse =
            request.send().await?.error_for_status()?.json().await?;
        log_debug!("http", "sessions fetched count={}", response.sessions.len());
        Ok(response
            .sessions
            .into_iter()
            .map(|session| AgentSession {
                id: session.id,
                title: session.title,
                cwd: session.cwd,
                model: session.model,
                provider: session.provider,
                started_at: session.started_at,
                last_active: session.last_active,
                message_count: session.message_count,
                is_active: session.is_active,
                archived: session.archived,
                profile: session.profile,
                backend_id: None,
            })
            .collect())
    }

    pub async fn messages(
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<Vec<ChatMessage>> {
        #[derive(Deserialize)]
        struct WireMessage {
            #[serde(default)]
            role: String,
            #[serde(default)]
            content: serde_json::Value,
        }
        #[derive(Deserialize)]
        struct MessagesResponse {
            #[serde(default)]
            messages: Vec<WireMessage>,
        }

        let mut request = self
            .http
            .get(format!(
                "{}/api/sessions/{}/messages",
                config.base_url,
                urlencode(session_id)
            ))
            .header("Accept", "application/json");
        if let Some(profile) = config.profile.as_deref().filter(|p| !p.is_empty()) {
            request = request.query(&[("profile", profile)]);
        }
        if !config.credential.is_empty() {
            request = request.header("X-Hermes-Session-Token", &config.credential);
        }
        let response: MessagesResponse = request.send().await?.error_for_status()?.json().await?;
        Ok(response
            .messages
            .into_iter()
            .filter_map(|message| {
                let role = MessageRole::parse(&message.role).unwrap_or(MessageRole::Assistant);
                if role == MessageRole::Tool {
                    return None;
                }
                let content = json_plain_text(&message.content);
                if content.is_empty() {
                    None
                } else {
                    Some(ChatMessage::new(role, content))
                }
            })
            .collect())
    }

    pub async fn connect(
        &mut self,
        config: BackendConfig,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        self.gateway.close();
        self.gateway
            .open(&config.base_url, &config.credential, events)
    }

    pub async fn create_session(&mut self, config: &BackendConfig) -> Result<SessionIDs> {
        let mut params = serde_json::json!({
            "source": "van-goal",
            "cols": 110,
            "close_on_disconnect": false
        });
        if let Some(profile) = config.profile.as_deref().filter(|p| !p.is_empty()) {
            params["profile"] = serde_json::Value::String(profile.to_string());
        }
        let result = self.gateway.request("session.create", params).await?;
        let session_id = json_str(&result, "session_id")
            .ok_or_else(|| anyhow!("session.create returned no session_id"))?;
        Ok(SessionIDs {
            live_id: session_id,
            stored_id: json_str(&result, "stored_session_id"),
        })
    }

    pub async fn resume_session(
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<SessionIDs> {
        let mut params = serde_json::json!({
            "session_id": session_id,
            "source": "van-goal",
            "cols": 110,
            "close_on_disconnect": false
        });
        if let Some(profile) = config.profile.as_deref().filter(|p| !p.is_empty()) {
            params["profile"] = serde_json::Value::String(profile.to_string());
        }
        let result = self.gateway.request("session.resume", params).await?;
        Ok(SessionIDs {
            live_id: json_str(&result, "session_id").unwrap_or_else(|| session_id.to_string()),
            stored_id: Some(
                json_str(&result, "session_key")
                    .or_else(|| json_str(&result, "resumed"))
                    .unwrap_or_else(|| session_id.to_string()),
            ),
        })
    }

    pub async fn submit_prompt(&mut self, session_id: &str, text: &str) -> Result<()> {
        self.gateway
            .request(
                "prompt.submit",
                serde_json::json!({ "session_id": session_id, "text": text }),
            )
            .await?;
        Ok(())
    }

    pub async fn respond_to_interaction(&mut self, request_id: &str, answer: &str) -> Result<()> {
        self.gateway
            .request(
                "clarify.respond",
                serde_json::json!({ "request_id": request_id, "answer": answer }),
            )
            .await?;
        Ok(())
    }

    pub async fn interrupt(&mut self, session_id: &str) -> Result<()> {
        self.gateway
            .request(
                "session.interrupt",
                serde_json::json!({ "session_id": session_id }),
            )
            .await?;
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.gateway.close();
    }
}

/// Extract readable text from the polymorphic `content` field of stored
/// messages (string | object | array), mirroring Swift's JSONValue.plainText.
pub(crate) fn json_plain_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(map) => {
            if let Some(text) = map.get("text") {
                return json_plain_text(text);
            }
            if let Some(content) = map.get("content") {
                return json_plain_text(content);
            }
            let mut pairs: Vec<String> = map
                .iter()
                .map(|(key, value)| format!("{key}: {}", json_plain_text(value)))
                .collect();
            pairs.sort();
            pairs.join("\n")
        }
        serde_json::Value::Array(items) => items
            .iter()
            .map(json_plain_text)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

pub(crate) fn urlencode(value: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("v", value)
        .finish()
        .trim_start_matches("v=")
        .to_string()
}

// ---------------------------------------------------------------------------
// Gateway WebSocket transport (JSON-RPC over /api/ws)
// ---------------------------------------------------------------------------

enum GatewayCommand {
    Request {
        method: String,
        params: serde_json::Value,
        responder: oneshot::Sender<Result<serde_json::Value>>,
    },
    Close,
}

#[derive(Default)]
struct GatewayHandle {
    command_tx: Arc<Mutex<Option<UnboundedSender<GatewayCommand>>>>,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl GatewayHandle {
    fn new() -> Self {
        Self::default()
    }

    fn is_open(&self) -> bool {
        self.command_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    fn open(
        &mut self,
        base_url: &str,
        token: &str,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        if self.is_open() {
            return Ok(());
        }
        let ws_url = build_ws_url(base_url, "/api/ws", token)?;
        let (command_tx, command_rx) = unbounded::<GatewayCommand>();
        *self.command_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(command_tx.clone());
        let task = tokio::spawn(run_gateway(ws_url, command_rx, events));
        *self.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        Ok(())
    }

    async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let (responder, receiver) = oneshot::channel();
        {
            let guard = self.command_tx.lock().unwrap_or_else(|e| e.into_inner());
            let sender = guard
                .as_ref()
                .ok_or_else(|| anyhow!("Hermes gateway socket is not connected."))?;
            sender
                .unbounded_send(GatewayCommand::Request {
                    method: method.to_string(),
                    params,
                    responder,
                })
                .map_err(|_| anyhow!("Hermes gateway socket is not connected."))?;
        }
        match tokio::time::timeout(Duration::from_secs(120), receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("Hermes gateway request dropped.")),
            Err(_) => Err(anyhow!("Hermes gateway request timed out.")),
        }
    }

    fn close(&mut self) {
        log_debug!("gateway", "disconnect gateway");
        if let Some(sender) = self
            .command_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = sender.unbounded_send(GatewayCommand::Close);
        }
        if let Some(task) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            task.abort();
        }
    }
}

fn build_ws_url(base_url: &str, path: &str, token: &str) -> Result<url::Url> {
    let mut parsed = url::Url::parse(base_url)?;
    parsed
        .set_scheme(if parsed.scheme() == "https" {
            "wss"
        } else {
            "ws"
        })
        .map_err(|_| anyhow!("Could not build Hermes gateway WebSocket URL."))?;
    parsed.set_path(path);
    parsed.query_pairs_mut().append_pair("token", token);
    Ok(parsed)
}

async fn run_gateway(
    ws_url: url::Url,
    mut command_rx: UnboundedReceiver<GatewayCommand>,
    events: UnboundedSender<AgentEvent>,
) {
    let connect = tokio_tungstenite::connect_async(ws_url.as_str()).await;
    let (ws_stream, _response) = match connect {
        Ok(pair) => pair,
        Err(error) => {
            let _ = events.unbounded_send(AgentEvent::Failed(format!(
                "Could not open Hermes gateway: {error}"
            )));
            return;
        }
    };
    log_debug!("gateway", "gateway ws opened");
    let (mut sink, mut stream) = ws_stream.split();
    let _ = events.unbounded_send(AgentEvent::Connected);

    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value>>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    loop {
        tokio::select! {
            command = command_rx.next() => {
                match command {
                    Some(GatewayCommand::Request { method, params, responder }) => {
                        let id = format!("h{}", uuid::Uuid::new_v4());
                        log_debug!("gateway", "rpc request method={method} id={id}");
                        pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(id.clone(), responder);
                        let payload = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "method": method,
                            "params": params
                        });
                        if sink.send(Message::Text(payload.to_string())).await.is_err() {
                            if let Some(responder) =
                                pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id)
                            {
                                let _ = responder.send(Err(anyhow!("Hermes gateway send failed.")));
                            }
                        }
                    }
                    Some(GatewayCommand::Close) | None => {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
            frame = stream.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        handle_gateway_frame(&text, &pending, &events);
                    }
                    Some(Ok(Message::Close(frame))) => {
                        log_debug!("gateway", "gateway ws closed frame={frame:?}");
                        let _ = events.unbounded_send(AgentEvent::Disconnected);
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        log_debug!("gateway", "gateway receive failed: {error}");
                        let _ = events.unbounded_send(AgentEvent::Failed(error.to_string()));
                        break;
                    }
                    None => {
                        let _ = events.unbounded_send(AgentEvent::Disconnected);
                        break;
                    }
                }
            }
        }
    }

    for (_, responder) in pending.lock().unwrap_or_else(|e| e.into_inner()).drain() {
        let _ = responder.send(Err(anyhow!("Hermes gateway socket is not connected.")));
    }
}

fn handle_gateway_frame(
    text: &str,
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value>>>>>,
    events: &UnboundedSender<AgentEvent>,
) {
    log_debug!("gateway", "gateway frame sample={}", frame_sample(text));
    let Ok(object) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };

    // RPC responses
    if let Some(id) = object.get("id").and_then(|v| v.as_str()) {
        let responder = pending.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        if let Some(responder) = responder {
            if let Some(error) = object.get("error") {
                let message = error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Hermes RPC failed")
                    .to_string();
                let _ = responder.send(Err(anyhow!(message)));
            } else {
                let result = object.get("result").cloned().unwrap_or_default();
                let _ = responder.send(Ok(if result.is_null() {
                    serde_json::Value::Object(Default::default())
                } else {
                    result
                }));
            }
            return;
        }
    }

    // Events
    if json_str(&object, "method").as_deref() != Some("event") {
        return;
    }
    let Some(params) = object.get("params") else {
        return;
    };
    let event_type = json_str(params, "type").unwrap_or_else(|| "event".into());
    let session_id = json_str(params, "session_id");
    let payload = params.get("payload").cloned().unwrap_or_default();

    match event_type.as_str() {
        "session.info" => {
            if let Some(session_id) = session_id {
                log_debug!("gateway", "session info live={session_id}");
                let _ = events.unbounded_send(AgentEvent::SessionInfo(session_id));
            }
        }
        "sessions.changed" | "session.created" | "session.updated" => {
            let _ = events.unbounded_send(AgentEvent::SessionsChanged);
        }
        "message.start" => {
            let _ = events.unbounded_send(AgentEvent::MessageStart);
        }
        "message.delta" => {
            if let Some(text) = json_str(&payload, "text") {
                let _ = events.unbounded_send(AgentEvent::MessageDelta {
                    text,
                    source: DeltaSource::EventStream,
                });
            }
        }
        "message.complete" => {
            let _ = events.unbounded_send(AgentEvent::MessageComplete(json_str(&payload, "text")));
        }
        "clarify.request" => {
            let question = json_str(&payload, "question").unwrap_or_default();
            let choices: Vec<String> = payload
                .get("choices")
                .and_then(|c| c.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let request_id = json_str(&payload, "request_id").unwrap_or_default();
            log_debug!(
                "gateway",
                "clarify request id={request_id} choices={}",
                choices.len()
            );
            let _ = events.unbounded_send(AgentEvent::Clarify {
                question,
                choices,
                request_id,
                session_id,
            });
        }
        _ => {
            if is_tool_event(&event_type, &payload) {
                let _ = events.unbounded_send(AgentEvent::Tool(tool_record(
                    &event_type,
                    &payload,
                    params,
                )));
            } else {
                log_debug!(
                    "gateway",
                    "gateway non-message event ignored type={event_type}"
                );
            }
        }
    }
}

fn frame_sample(value: &str) -> String {
    let prefix: String = value.chars().take(320).collect();
    prefix.replace('\n', "\\n")
}

fn is_tool_event(event_type: &str, payload: &serde_json::Value) -> bool {
    if event_type.starts_with("tool.") {
        return true;
    }
    if payload.get("tool").is_some() || payload.get("tool_name").is_some() {
        return true;
    }
    if let Some(name) = payload.get("name").and_then(|n| n.as_str()) {
        return name.to_lowercase().contains("tool");
    }
    false
}

fn tool_record(
    event_type: &str,
    payload: &serde_json::Value,
    fallback: &serde_json::Value,
) -> ToolCallRecord {
    let name = json_str(payload, "tool")
        .or_else(|| json_str(payload, "name"))
        .or_else(|| json_str(payload, "tool_name"))
        .unwrap_or_else(|| event_type.to_string());
    let status = event_type.replace("tool.", "");
    let source = if payload.is_null() || payload.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        fallback
    } else {
        payload
    };
    ToolCallRecord::new(name, status, pretty_json(source))
}
