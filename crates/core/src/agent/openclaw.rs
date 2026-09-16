use super::session_scope;
use super::{json_str, pretty_json};
use crate::agent::device_identity::{
    OpenClawDeviceIdentity, OPENCLAW_CLIENT_ID, OPENCLAW_CLIENT_MODE, OPENCLAW_DEVICE_FAMILY,
    OPENCLAW_DISPLAY_NAME, OPENCLAW_PLATFORM, OPENCLAW_ROLE, OPENCLAW_SCOPES,
    OPENCLAW_SESSION_NAMESPACE,
};
use crate::log_debug;
use crate::models::*;
use anyhow::{anyhow, Result};
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

pub struct OpenClawBackend {
    http: reqwest::Client,
    gateway: GatewayHandle,
}

impl Default for OpenClawBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenClawBackend {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
            gateway: GatewayHandle::default(),
        }
    }

    pub async fn probe(&self, config: &BackendConfig) -> Result<()> {
        let mut request = self
            .http
            .get(format!("{}/healthz", config.base_url.trim_end_matches('/')))
            .timeout(Duration::from_secs(5));
        if !config.credential.is_empty() {
            request = request.bearer_auth(&config.credential);
        }
        let response = request.send().await?;
        let status = response.status();
        if !(status.is_success() || status.is_client_error()) {
            return Err(anyhow!(
                "OpenClaw returned HTTP {} for health probe",
                status.as_u16()
            ));
        }
        Ok(())
    }

    pub async fn list_sessions(&mut self, config: &BackendConfig) -> Result<Vec<AgentSession>> {
        self.ensure_connected(config).await?;
        let result = self
            .gateway
            .request("sessions.list", serde_json::json!({ "limit": 100 }))
            .await?;
        let rows = result
            .get("sessions")
            .and_then(|s| s.as_array())
            .or_else(|| result.get("items").and_then(|i| i.as_array()))
            .cloned()
            .unwrap_or_default();
        Ok(rows.iter().filter_map(session_from_row).collect())
    }

    pub async fn messages(
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<Vec<ChatMessage>> {
        self.ensure_connected(config).await?;
        let result = self
            .gateway
            .request(
                "chat.history",
                serde_json::json!({ "sessionKey": session_id, "limit": 200 }),
            )
            .await?;
        let rows = result
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(rows
            .iter()
            .filter_map(|row| {
                let role = MessageRole::parse(json_str(row, "role").as_deref()?)?;
                let text = if let Some(direct) =
                    json_str(row, "text").or_else(|| json_str(row, "content"))
                {
                    direct
                } else if let Some(blocks) = row.get("content").and_then(|c| c.as_array()) {
                    blocks
                        .iter()
                        .filter_map(|block| {
                            json_str(block, "text").or_else(|| json_str(block, "content"))
                        })
                        .collect::<String>()
                } else {
                    return None;
                };
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
        self.gateway.disconnect();
        self.ensure_connected_with_events(&config, Some(events))
            .await
    }

    pub async fn create_session(&mut self, config: &BackendConfig) -> Result<SessionIDs> {
        self.ensure_connected(config).await?;
        let proposed = proposed_session_key();
        let result = self
            .gateway
            .request("sessions.create", sessions_create_params(&proposed))
            .await?;
        let key = json_str(&result, "key")
            .or_else(|| json_str(&result, "sessionKey"))
            .unwrap_or(proposed);
        self.gateway.set_active_session(key.clone());
        self.subscribe_to_session(&key).await;
        Ok(SessionIDs {
            live_id: key.clone(),
            stored_id: Some(key),
        })
    }

    pub async fn resume_session(
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<SessionIDs> {
        self.ensure_connected(config).await?;
        self.gateway.set_active_session(session_id.to_string());
        self.subscribe_to_session(session_id).await;
        Ok(SessionIDs {
            live_id: session_id.to_string(),
            stored_id: Some(session_id.to_string()),
        })
    }

    pub async fn submit_prompt(&mut self, session_id: &str, text: &str) -> Result<()> {
        let idempotency_key = uuid::Uuid::new_v4().to_string();
        if let Err(current_error) = self
            .gateway
            .request(
                "chat.send",
                chat_send_params(session_id, text, &idempotency_key),
            )
            .await
        {
            if !is_unexpected_property_error(&current_error, "idempotencyKey") {
                return Err(current_error);
            }

            log_debug!(
                "openclaw",
                "chat.send does not accept idempotencyKey; retrying legacy params"
            );
            self.gateway
                .request("chat.send", legacy_chat_send_params(session_id, text))
                .await?;
        }
        Ok(())
    }

    pub async fn respond_to_interaction(&mut self, request_id: &str, answer: &str) -> Result<()> {
        let method = if request_id.starts_with("plugin:") {
            "plugin.approval.resolve"
        } else {
            "exec.approval.resolve"
        };
        self.gateway
            .request(
                method,
                serde_json::json!({
                    "id": request_id,
                    "decision": if answer.is_empty() { "deny" } else { answer }
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn interrupt(&mut self, session_id: &str) -> Result<()> {
        self.gateway
            .request(
                "chat.abort",
                serde_json::json!({ "sessionKey": session_id }),
            )
            .await?;
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.gateway.disconnect();
    }

    async fn subscribe_to_session(&self, session_id: &str) {
        if let Err(current_error) = self
            .gateway
            .request(
                "sessions.messages.subscribe",
                session_messages_subscribe_params(session_id),
            )
            .await
        {
            let legacy_result = self
                .gateway
                .request(
                    "sessions.messages.subscribe",
                    legacy_session_messages_subscribe_params(session_id),
                )
                .await;
            if let Err(legacy_error) = legacy_result {
                log_debug!(
                    "openclaw",
                    "session message subscribe failed current={current_error}; legacy={legacy_error}"
                );
            }
        }
    }

    async fn ensure_connected(&mut self, config: &BackendConfig) -> Result<()> {
        if self.gateway.is_connected() {
            return Ok(());
        }
        self.ensure_connected_with_events(config, None).await
    }

    async fn ensure_connected_with_events(
        &mut self,
        config: &BackendConfig,
        events: Option<UnboundedSender<AgentEvent>>,
    ) -> Result<()> {
        if self.gateway.is_connected() {
            return Ok(());
        }
        if !self.gateway.is_open() {
            self.gateway
                .open(&config.base_url, &config.credential, events)?;
        }
        self.gateway.wait_connected(Duration::from_secs(20)).await
    }
}

/// The Gateway WebSocket URL behind an HTTP(S) base URL. Shared by the
/// handshake and by the stored-credential lookup below, so the key a device
/// token is filed under cannot drift between writing and reading it.
fn gateway_url(base_url: &str) -> Result<url::Url> {
    let mut parsed = url::Url::parse(base_url)?;
    parsed
        .set_scheme(if parsed.scheme() == "https" {
            "wss"
        } else {
            "ws"
        })
        .map_err(|_| anyhow!("Could not build OpenClaw gateway WebSocket URL."))?;
    Ok(parsed)
}

/// Characters of the paired-device token the Gateway issued for this address.
/// That token lives in the app-data secret store rather than in settings, which
/// is why the gateway-token field can be empty while connecting still works.
/// Settings reads it through here so the stored credential is visible.
pub fn stored_device_token_characters(base_url: &str) -> usize {
    let Ok(url) = gateway_url(base_url) else {
        return 0;
    };
    OpenClawDeviceIdentity::load_device_token(&url.to_string())
        .ok()
        .flatten()
        .map(|token| token.trim().chars().count())
        .unwrap_or(0)
}

/// Forget the paired-device token held for this address, so the next connect
/// has to authenticate with the gateway token again. Returns whether a token
/// was actually removed.
pub fn forget_device_token(base_url: &str) -> bool {
    let Ok(url) = gateway_url(base_url) else {
        return false;
    };
    OpenClawDeviceIdentity::forget_device_token(&url.to_string()).unwrap_or(false)
}

/// One row of `sessions.list` as a session, or nothing when it names no session.
///
/// A row carries the session's context accounting, so the client does not have
/// to guess at it: `totalTokens` is what the context is holding and
/// `contextTokens` is the width of the window its model has. Both are optional
/// because a Gateway is free to leave them out, and "not reported" has to stay
/// distinguishable from zero — a frontend shows an estimate for the first and
/// must not present it as the second.
fn session_from_row(row: &serde_json::Value) -> Option<AgentSession> {
    let key = json_str(row, "key").or_else(|| json_str(row, "sessionKey"))?;
    Some(AgentSession {
        id: key,
        title: json_str(row, "title").or_else(|| json_str(row, "displayName")),
        cwd: None,
        model: json_str(row, "model"),
        provider: Some("OpenClaw".into()),
        started_at: None,
        last_active: None,
        message_count: super::json_i64(row, "messageCount"),
        is_active: None,
        archived: Some(false),
        profile: None,
        backend_id: None,
        used_tokens: super::json_i64(row, "totalTokens"),
        context_tokens: super::json_i64(row, "contextTokens"),
    })
}

fn sessions_create_params(key: &str) -> serde_json::Value {
    serde_json::json!({ "key": key })
}

/// The key a session this client starts is filed under.
///
/// The namespace names the frontend, so a desktop session and a phone session
/// are told apart in the Gateway's own records even before either has a title —
/// see [`OPENCLAW_SESSION_NAMESPACE`]. The id is random per session: two
/// sessions under one namespace are still two different conversations, which is
/// why the filter compares the whole key and never the namespace.
fn proposed_session_key() -> String {
    format!(
        "agent:main:{}:{}",
        OPENCLAW_SESSION_NAMESPACE,
        uuid::Uuid::new_v4()
    )
}

fn session_messages_subscribe_params(key: &str) -> serde_json::Value {
    serde_json::json!({ "key": key })
}

fn legacy_session_messages_subscribe_params(key: &str) -> serde_json::Value {
    serde_json::json!({ "sessionKey": key })
}

fn chat_send_params(session_id: &str, text: &str, idempotency_key: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionKey": session_id,
        "message": text,
        "idempotencyKey": idempotency_key
    })
}

fn legacy_chat_send_params(session_id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionKey": session_id,
        "message": text
    })
}

fn is_unexpected_property_error(error: &anyhow::Error, property: &str) -> bool {
    let message = error.to_string();
    message.contains("unexpected property") && message.contains(&format!("'{property}'"))
}

// ---------------------------------------------------------------------------
// Gateway protocol v4 over WebSocket
// ---------------------------------------------------------------------------

enum GatewayCommand {
    Request {
        method: String,
        params: serde_json::Value,
        responder: oneshot::Sender<Result<serde_json::Value>>,
    },
    SetActive(String),
    Close,
}

#[derive(Clone, Debug, Default)]
enum GatewayHandshakeState {
    #[default]
    Pending,
    Connected,
    Failed(String),
}

#[derive(Default)]
struct GatewayHandle {
    command_tx: Arc<Mutex<Option<UnboundedSender<GatewayCommand>>>>,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    connected_rx: Arc<Mutex<Option<watch::Receiver<GatewayHandshakeState>>>>,
}

impl GatewayHandle {
    fn is_connected(&self) -> bool {
        self.connected_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|rx| matches!(&*rx.borrow(), GatewayHandshakeState::Connected))
            .unwrap_or(false)
    }

    fn is_open(&self) -> bool {
        self.task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|task| !task.is_finished())
            .unwrap_or(false)
    }

    fn set_active_session(&self, key: String) {
        if let Some(sender) = self
            .command_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = sender.unbounded_send(GatewayCommand::SetActive(key));
        }
    }

    fn open(
        &mut self,
        base_url: &str,
        token: &str,
        events: Option<UnboundedSender<AgentEvent>>,
    ) -> Result<()> {
        if self.is_connected() {
            return Ok(());
        }
        let parsed = gateway_url(base_url)?;
        let (command_tx, command_rx) = unbounded::<GatewayCommand>();
        let (connected_tx, connected_rx) = watch::channel(GatewayHandshakeState::Pending);
        *self.command_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(command_tx);
        *self.connected_rx.lock().unwrap_or_else(|e| e.into_inner()) = Some(connected_rx);
        let task = tokio::spawn(run_gateway(
            parsed,
            command_rx,
            connected_tx,
            events,
            token.to_string(),
        ));
        *self.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        Ok(())
    }

    async fn wait_connected(&self, timeout: Duration) -> Result<()> {
        let mut rx = self
            .connected_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| anyhow!("OpenClaw gateway is not open."))?;
        tokio::time::timeout(timeout, async move {
            loop {
                let state = rx.borrow().clone();
                match state {
                    GatewayHandshakeState::Pending => {}
                    GatewayHandshakeState::Connected => return Ok(()),
                    GatewayHandshakeState::Failed(message) => return Err(anyhow!(message)),
                }
                rx.changed()
                    .await
                    .map_err(|_| anyhow!("OpenClaw gateway is not connected."))?;
            }
        })
        .await
        .map_err(|_| anyhow!("OpenClaw gateway handshake timed out."))?
    }

    async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let (responder, receiver) = oneshot::channel();
        {
            let guard = self.command_tx.lock().unwrap_or_else(|e| e.into_inner());
            let sender = guard
                .as_ref()
                .ok_or_else(|| anyhow!("OpenClaw gateway socket is not connected."))?;
            sender
                .unbounded_send(GatewayCommand::Request {
                    method: method.to_string(),
                    params,
                    responder,
                })
                .map_err(|_| anyhow!("OpenClaw gateway socket is not connected."))?;
        }
        match tokio::time::timeout(Duration::from_secs(60), receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("OpenClaw gateway request dropped.")),
            Err(_) => Err(anyhow!("OpenClaw gateway request timed out.")),
        }
    }

    fn disconnect(&mut self) {
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
        *self.connected_rx.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

async fn run_gateway(
    ws_url: url::Url,
    mut command_rx: UnboundedReceiver<GatewayCommand>,
    connected_tx: watch::Sender<GatewayHandshakeState>,
    events: Option<UnboundedSender<AgentEvent>>,
    token: String,
) {
    let gateway_scope = ws_url.to_string();
    let (ws_stream, _response) = match tokio_tungstenite::connect_async(ws_url.as_str()).await {
        Ok(pair) => pair,
        Err(error) => {
            let message = format!("Could not open OpenClaw gateway: {error}");
            if let Some(events) = &events {
                let _ = events.unbounded_send(AgentEvent::Failed(message.clone()));
            }
            log_debug!(
                "openclaw",
                "gateway socket open failed url={ws_url}: {message}"
            );
            connected_tx.send_replace(GatewayHandshakeState::Failed(message));
            return;
        }
    };
    let (mut sink, mut stream) = ws_stream.split();

    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let handshake_request_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let challenge_nonce: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let active_session: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let emit = |events: &Option<UnboundedSender<AgentEvent>>, event: AgentEvent| {
        if let Some(events) = events {
            let _ = events.unbounded_send(event);
        }
    };

    loop {
        tokio::select! {
            command = command_rx.next() => {
                match command {
                    Some(GatewayCommand::Request { method, params, responder }) => {
                        let id = uuid::Uuid::new_v4().to_string();
                        pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(id.clone(), responder);
                        let payload = serde_json::json!({
                            "type": "req",
                            "id": id,
                            "method": method,
                            "params": params
                        });
                        if sink.send(Message::Text(payload.to_string())).await.is_err() {
                            if let Some(responder) =
                                pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id)
                            {
                                let _ = responder.send(Err(anyhow!("OpenClaw gateway send failed.")));
                            }
                        }
                    }
                    Some(GatewayCommand::SetActive(key)) => {
                        *active_session.lock().unwrap_or_else(|e| e.into_inner()) = Some(key);
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
                        handle_frame(
                            &text,
                            &pending,
                            &handshake_request_id,
                            &challenge_nonce,
                            &active_session,
                            &connected_tx,
                            &events,
                            &mut sink,
                            &token,
                            &gateway_scope,
                        )
                        .await;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        if matches!(&*connected_tx.borrow(), GatewayHandshakeState::Pending) {
                            connected_tx.send_replace(GatewayHandshakeState::Failed(
                                "OpenClaw gateway closed the connection during handshake.".into(),
                            ));
                        }
                        emit(&events, AgentEvent::Disconnected);
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        let message = error.to_string();
                        connected_tx.send_replace(GatewayHandshakeState::Failed(message.clone()));
                        emit(&events, AgentEvent::Failed(message));
                        break;
                    }
                }
            }
        }
    }

    for (_, responder) in pending.lock().unwrap_or_else(|e| e.into_inner()).drain() {
        let _ = responder.send(Err(anyhow!("OpenClaw gateway socket is not connected.")));
    }
}

/// The session a Gateway frame is about, when the Gateway labels one. Versions
/// spell this differently per method, so both known spellings are accepted.
fn payload_session_key(payload: &serde_json::Value) -> Option<String> {
    ["sessionKey", "key", "session_id", "sessionId"]
        .iter()
        .find_map(|field| json_str(payload, field))
        .filter(|key| !key.is_empty())
}

/// Whether a frame belongs in the transcript the user is looking at.
///
/// One Gateway connection carries every session's traffic, and subscribing to a
/// session does not unsubscribe from the ones opened before it. This was
/// measured against a live Gateway rather than assumed: a socket that had
/// subscribed to nothing received turn frames for *two* different sessions
/// inside thirty seconds, every one of them labelled with its `sessionKey` at
/// the top of the payload. `sessions.messages.subscribe` therefore does not
/// scope delivery, and filtering here is the only thing that does.
///
/// That makes the empty case the dangerous one. A client that has not resumed a
/// session yet — the whole window after a reconnect, which on a phone is most
/// of the time — used to accept everything, so every session's reply was
/// appended to whatever chat was open. Turn traffic uses the strict rule: see
/// [`session_scope::belongs_to_open_session`].
fn is_for_active_session(
    payload: &serde_json::Value,
    active_session: &Arc<Mutex<Option<String>>>,
) -> bool {
    let active = active_session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    session_scope::belongs_to_open_session(
        active.as_deref(),
        payload_session_key(payload).as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
async fn handle_frame(
    text: &str,
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value>>>>>,
    handshake_request_id: &Arc<Mutex<Option<String>>>,
    challenge_nonce: &Arc<Mutex<Option<String>>>,
    active_session: &Arc<Mutex<Option<String>>>,
    connected_tx: &watch::Sender<GatewayHandshakeState>,
    events: &Option<UnboundedSender<AgentEvent>>,
    sink: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    token: &str,
    gateway_scope: &str,
) {
    let Ok(object) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };

    let frame_type = json_str(&object, "type").unwrap_or_default();

    // RPC responses
    if frame_type == "res" {
        if let Some(id) = json_str(&object, "id") {
            let responder = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            if let Some(responder) = responder {
                if object.get("ok").and_then(|ok| ok.as_bool()) == Some(false) {
                    let message = format!(
                        "OpenClaw RPC failed: {}",
                        object.get("error").map(pretty_json).unwrap_or_default()
                    );
                    let is_handshake = handshake_request_id
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_deref()
                        == Some(id.as_str());
                    if is_handshake {
                        connected_tx.send_replace(GatewayHandshakeState::Failed(message.clone()));
                    }
                    let _ = responder.send(Err(anyhow!(message)));
                } else {
                    let payload = object.get("payload").cloned().unwrap_or_default();
                    let _ = responder.send(Ok(if payload.is_null() {
                        serde_json::Value::Object(Default::default())
                    } else {
                        payload
                    }));
                }
            }
        }
        return;
    }

    if frame_type != "event" {
        return;
    }
    let Some(event) = json_str(&object, "event") else {
        return;
    };
    let payload = object.get("payload").cloned().unwrap_or_default();

    // Message traffic for another session must not reach the open transcript.
    if matches!(
        event.as_str(),
        "chat" | "session.message" | "session.tool" | "agent"
    ) && !is_for_active_session(&payload, active_session)
    {
        log_debug!("openclaw", "event dropped for another session name={event}");
        return;
    }

    let events_for_emit = events.clone();
    let emit_event = move |event: AgentEvent| {
        if let Some(events) = events_for_emit.as_ref() {
            let _ = events.unbounded_send(event);
        }
    };

    match event.as_str() {
        "connect.challenge" => {
            let nonce = json_str(&payload, "nonce").unwrap_or_default();
            let Some(signed_at) = payload
                .get("ts")
                .and_then(|value| value.as_i64())
                .filter(|ts| *ts >= 0)
            else {
                connected_tx.send_replace(GatewayHandshakeState::Failed(
                    "OpenClaw sent an invalid gateway challenge timestamp.".into(),
                ));
                emit_event(AgentEvent::Failed(
                    "OpenClaw sent an invalid gateway challenge timestamp.".into(),
                ));
                return;
            };
            *challenge_nonce.lock().unwrap_or_else(|e| e.into_inner()) = Some(nonce.clone());

            // Complete the handshake immediately: sign and send connect.
            let (device, device_token) = match OpenClawDeviceIdentity::load_or_create() {
                Ok(identity) => {
                    let device_token = OpenClawDeviceIdentity::load_device_token(gateway_scope)
                        .ok()
                        .flatten();
                    let signature_token = if token.is_empty() {
                        device_token.as_deref().unwrap_or_default()
                    } else {
                        token
                    };
                    (
                        identity.signed_connect_device(&nonce, signed_at, signature_token),
                        device_token,
                    )
                }
                Err(error) => {
                    connected_tx.send_replace(GatewayHandshakeState::Failed(format!(
                        "Could not load OpenClaw device identity: {error}"
                    )));
                    emit_event(AgentEvent::Failed(format!(
                        "Could not load OpenClaw device identity: {error}"
                    )));
                    return;
                }
            };
            let id = uuid::Uuid::new_v4().to_string();
            *handshake_request_id
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
            let (responder, receiver) = oneshot::channel();
            pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.clone(), responder);
            let connect_payload = serde_json::json!({
                "minProtocol": 4,
                "maxProtocol": 4,
                "client": {
                    "id": OPENCLAW_CLIENT_ID,
                    "displayName": OPENCLAW_DISPLAY_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                    "platform": OPENCLAW_PLATFORM,
                    "deviceFamily": OPENCLAW_DEVICE_FAMILY,
                    "mode": OPENCLAW_CLIENT_MODE
                },
                "role": OPENCLAW_ROLE,
                "scopes": OPENCLAW_SCOPES,
                "caps": ["tool-events"],
                "commands": [],
                "permissions": {},
                "auth": match (token.is_empty(), device_token.as_deref()) {
                    (false, Some(device_token)) => serde_json::json!({
                        "token": token,
                        "deviceToken": device_token
                    }),
                    (false, None) => serde_json::json!({ "token": token }),
                    (true, Some(device_token)) => serde_json::json!({
                        "deviceToken": device_token
                    }),
                    (true, None) => serde_json::json!({}),
                },
                "locale": "en-US",
                "userAgent": format!("van-goal/{}", env!("CARGO_PKG_VERSION")),
                "device": device
            });
            let frame = serde_json::json!({
                "type": "req",
                "id": id,
                "method": "connect",
                "params": connect_payload
            });
            let send_result = sink.send(Message::Text(frame.to_string())).await;
            let events_out = events.clone();
            let connected_tx = connected_tx.clone();
            let gateway_scope = gateway_scope.to_string();
            tokio::spawn(async move {
                let result = if send_result.is_err() {
                    Err(anyhow!("Could not send the OpenClaw gateway handshake."))
                } else {
                    match tokio::time::timeout(Duration::from_secs(15), receiver).await {
                        Ok(Ok(result)) => result,
                        Ok(Err(_)) => Err(anyhow!("OpenClaw dropped the gateway handshake.")),
                        Err(_) => Err(anyhow!("OpenClaw gateway handshake timed out.")),
                    }
                };
                match result {
                    Ok(result) => {
                        let accepted = result.get("type").and_then(|t| t.as_str())
                            == Some("hello-ok")
                            || result.get("protocol").is_some();
                        connected_tx.send_replace(if accepted {
                            GatewayHandshakeState::Connected
                        } else {
                            GatewayHandshakeState::Failed(
                                "OpenClaw returned an invalid handshake response.".into(),
                            )
                        });
                        if accepted {
                            if let Some(device_token) = result
                                .pointer("/auth/deviceToken")
                                .and_then(|value| value.as_str())
                            {
                                if let Err(error) = OpenClawDeviceIdentity::save_device_token(
                                    &gateway_scope,
                                    device_token,
                                ) {
                                    log_debug!("openclaw", "could not save device token: {error}");
                                }
                            }
                            log_debug!("openclaw", "gateway handshake ok");
                            if let Some(events) = events_out {
                                let _ = events.unbounded_send(AgentEvent::Connected);
                            }
                        } else if let Some(events) = events_out {
                            let _ = events.unbounded_send(AgentEvent::Failed(
                                "OpenClaw returned an invalid handshake response.".into(),
                            ));
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        log_debug!("openclaw", "gateway handshake failed: {message}");
                        connected_tx.send_replace(GatewayHandshakeState::Failed(message.clone()));
                        if let Some(events) = events_out {
                            let _ = events.unbounded_send(AgentEvent::Failed(message));
                        }
                    }
                }
            });
        }
        "chat" | "session.message" => {
            let state = json_str(&payload, "state")
                .or_else(|| json_str(&payload, "status"))
                .unwrap_or_default();
            if state == "started" {
                emit_event(AgentEvent::MessageStart);
            }
            if let Some(delta) = json_str(&payload, "deltaText").filter(|d| !d.is_empty()) {
                emit_event(AgentEvent::MessageDelta {
                    text: delta,
                    source: DeltaSource::Transcript,
                });
            }
            if matches!(state.as_str(), "final" | "completed" | "done") {
                emit_event(AgentEvent::MessageComplete(None));
            }
            if state == "error" {
                emit_event(AgentEvent::TurnFailed(
                    payload
                        .get("error")
                        .map(pretty_json)
                        .unwrap_or_else(|| "OpenClaw turn failed".into()),
                ));
            }
        }
        "sessions.changed" | "session.created" | "session.updated" => {
            emit_event(AgentEvent::SessionsChanged);
        }
        "session.tool" => {
            emit_event(AgentEvent::Tool(ToolCallRecord::new(
                json_str(&payload, "name")
                    .or_else(|| json_str(&payload, "tool"))
                    .unwrap_or_else(|| "tool".into()),
                json_str(&payload, "status").unwrap_or_else(|| "updated".into()),
                payload.to_string(),
            )));
        }
        "agent" => {
            let stream = json_str(&payload, "stream").unwrap_or_default();
            let data = payload.get("data").cloned().unwrap_or_default();
            match stream.as_str() {
                "lifecycle" if json_str(&data, "phase").as_deref() == Some("start") => {
                    emit_event(AgentEvent::MessageStart);
                }
                "assistant" => {
                    if let Some(delta) = json_str(&data, "delta") {
                        emit_event(AgentEvent::MessageDelta {
                            text: delta,
                            source: DeltaSource::AgentStream,
                        });
                    }
                }
                "tool" => {
                    emit_event(AgentEvent::Tool(ToolCallRecord::new(
                        json_str(&data, "name")
                            .or_else(|| json_str(&data, "tool"))
                            .unwrap_or_else(|| "tool".into()),
                        json_str(&data, "status").unwrap_or_else(|| "updated".into()),
                        data.to_string(),
                    )));
                }
                _ => {}
            }
        }
        "exec.approval.requested" | "plugin.approval.requested" => {
            let Some(request_id) = json_str(&payload, "id") else {
                return;
            };
            let command = json_str(&payload, "command")
                .or_else(|| json_str(&payload, "rawCommand"))
                .or_else(|| json_str(&payload, "title"))
                .unwrap_or_else(|| "OpenClaw requests approval".into());
            emit_event(AgentEvent::Clarify {
                question: command,
                choices: vec!["allow-once".into(), "allow-always".into(), "deny".into()],
                request_id,
                session_id: json_str(&payload, "sessionKey"),
            });
        }
        _ => {
            // Worth knowing about: the gateway announces session list changes
            // with its own event names.
            log_debug!("openclaw", "event ignored name={event}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        chat_send_params, is_for_active_session, is_unexpected_property_error,
        legacy_chat_send_params, legacy_session_messages_subscribe_params, payload_session_key,
        proposed_session_key, session_from_row, session_messages_subscribe_params,
        sessions_create_params,
    };
    use crate::agent::device_identity::{OPENCLAW_DISPLAY_NAME, OPENCLAW_SESSION_NAMESPACE};
    use std::sync::{Arc, Mutex};

    fn active(key: Option<&str>) -> Arc<Mutex<Option<String>>> {
        Arc::new(Mutex::new(key.map(str::to_string)))
    }

    #[test]
    fn session_rpc_params_match_openclaw_v4_schema() {
        assert_eq!(
            sessions_create_params("agent:main:van-goal:test"),
            serde_json::json!({ "key": "agent:main:van-goal:test" })
        );
        assert_eq!(
            session_messages_subscribe_params("agent:main:van-goal:test"),
            serde_json::json!({ "key": "agent:main:van-goal:test" })
        );
        assert_eq!(
            legacy_session_messages_subscribe_params("agent:main:van-goal:test"),
            serde_json::json!({ "sessionKey": "agent:main:van-goal:test" })
        );
    }

    #[test]
    fn chat_send_can_fall_back_for_older_gateways() {
        assert_eq!(
            chat_send_params("agent:main:test", "hello", "request-1"),
            serde_json::json!({
                "sessionKey": "agent:main:test",
                "message": "hello",
                "idempotencyKey": "request-1"
            })
        );
        assert_eq!(
            legacy_chat_send_params("agent:main:test", "hello"),
            serde_json::json!({
                "sessionKey": "agent:main:test",
                "message": "hello"
            })
        );
        assert!(is_unexpected_property_error(
            &anyhow::anyhow!(
                "OpenClaw RPC failed: invalid chat.send params: unexpected property 'idempotencyKey'"
            ),
            "idempotencyKey"
        ));
    }

    /// The Gateway carries every session down one connection, so a cron job in
    /// another session used to land in whatever chat was open.
    #[test]
    fn a_frame_for_another_session_is_rejected() {
        let active = active(Some("agent:main:van-goal:mine"));

        assert!(is_for_active_session(
            &serde_json::json!({ "sessionKey": "agent:main:van-goal:mine" }),
            &active
        ));
        assert!(!is_for_active_session(
            &serde_json::json!({ "sessionKey": "agent:main:van-goal:cron" }),
            &active
        ));
        assert!(!is_for_active_session(
            &serde_json::json!({ "key": "agent:main:van-goal:cron" }),
            &active
        ));
    }

    /// Gateway versions disagree on the field name, so both are honoured.
    #[test]
    fn both_session_key_spellings_are_understood() {
        let active = active(Some("key-a"));
        for payload in [
            serde_json::json!({ "key": "key-a" }),
            serde_json::json!({ "sessionKey": "key-a" }),
        ] {
            assert_eq!(payload_session_key(&payload).as_deref(), Some("key-a"));
            assert!(is_for_active_session(&payload, &active));
        }
    }

    /// A frame that names no session is kept: dropping it could lose the active
    /// session's own stream.
    #[test]
    fn an_unlabelled_frame_is_kept() {
        let active = active(Some("key-a"));
        let payload = serde_json::json!({ "deltaText": "hello" });
        assert_eq!(payload_session_key(&payload), None);
        assert!(is_for_active_session(&payload, &active));
    }

    /// The window after a reconnect is the one that leaks, because that is when
    /// the client has no active session and the Gateway is still pushing every
    /// session to it. "Nothing subscribed yet" must not mean "accept everybody".
    #[test]
    fn a_frame_for_another_session_is_rejected_before_anything_is_subscribed() {
        let active = active(None);
        assert!(!is_for_active_session(
            &serde_json::json!({ "sessionKey": "agent:main:van-goal:someone-else" }),
            &active
        ));
        assert!(!is_for_active_session(
            &serde_json::json!({ "sessionKey": "agent:main:van-goal:cron" }),
            &active
        ));
    }

    /// An unlabelled frame is still kept when no session is open, so a Gateway
    /// that leaves its own stream unlabelled does not lose the reply.
    #[test]
    fn an_unlabelled_frame_is_kept_before_anything_is_subscribed() {
        let active = active(None);
        assert!(is_for_active_session(
            &serde_json::json!({ "deltaText": "hello" }),
            &active
        ));
    }

    /// The namespace names the frontend; the id must still be random, because
    /// two sessions under one namespace are two conversations and the filter
    /// compares the whole key.
    #[test]
    fn a_proposed_key_names_the_frontend_and_is_unique_per_session() {
        let key = proposed_session_key();
        let prefix = format!("agent:main:{OPENCLAW_SESSION_NAMESPACE}:");
        assert!(
            key.starts_with(&prefix),
            "{key} should be filed under {prefix}"
        );
        assert!(
            key.len() > prefix.len(),
            "{key} carries no session id after the namespace"
        );
        assert_ne!(
            proposed_session_key(),
            key,
            "two sessions must not share a key"
        );
    }

    /// The two frontends must not present themselves by the same name, or the
    /// session list they share fills with identical entries and picking the
    /// wrong one is indistinguishable from a client that crossed two
    /// conversations. This is the whole reason the constants are per-platform,
    /// so the test states it even though it can only see one side of it.
    #[test]
    fn the_client_names_itself_distinctly_from_the_other_frontend() {
        assert!(!OPENCLAW_DISPLAY_NAME.trim().is_empty());
        assert!(
            OPENCLAW_SESSION_NAMESPACE.starts_with("van-goal"),
            "{OPENCLAW_SESSION_NAMESPACE} should still say which app it is"
        );
        // The name each frontend used before this was split, and the reason ten
        // sessions on the measured Gateway were all called "Van-Goal".
        assert_ne!(OPENCLAW_DISPLAY_NAME, "Van-Goal");
        if cfg!(any(target_os = "android", target_os = "ios")) {
            assert_eq!(OPENCLAW_DISPLAY_NAME, "Van-Goal Mobile");
            assert_eq!(OPENCLAW_SESSION_NAMESPACE, "van-goal-mobile");
        } else {
            assert_eq!(OPENCLAW_DISPLAY_NAME, "Van-Goal Desktop");
            assert_eq!(OPENCLAW_SESSION_NAMESPACE, "van-goal-desktop");
        }
    }

    /// A `sessions.list` row as captured off a live Gateway, trimmed to the
    /// fields the client reads. The context accounting is the part that matters
    /// here: it is what the status bar shows instead of a guess, and the two
    /// numbers are the row's, not the client's.
    #[test]
    fn a_session_row_carries_its_own_context_accounting() {
        let row = serde_json::json!({
            "key": "agent:main:van-goal-desktop:0ec3c05b",
            "displayName": "Van-Goal Desktop",
            "model": "deepseek/deepseek-v4.1-flash",
            "messageCount": 12,
            "totalTokens": 36393,
            "contextTokens": 1048576,
            "status": "done"
        });
        let session = session_from_row(&row).expect("the row names a session");
        assert_eq!(session.id, "agent:main:van-goal-desktop:0ec3c05b");
        assert_eq!(
            session.model.as_deref(),
            Some("deepseek/deepseek-v4.1-flash")
        );
        assert_eq!(session.used_tokens, Some(36_393));
        assert_eq!(session.context_tokens, Some(1_048_576));
    }

    /// A Gateway that reports nothing must leave both unset rather than zero:
    /// the frontends show an estimate for an unset value and must never draw it
    /// as a measured zero.
    #[test]
    fn a_row_that_reports_no_accounting_leaves_it_unset() {
        let row = serde_json::json!({ "key": "agent:main:van-goal-desktop:1", "model": "m" });
        let session = session_from_row(&row).expect("the row names a session");
        assert_eq!(session.used_tokens, None);
        assert_eq!(session.context_tokens, None);
    }

    /// A row with no key names no session and is skipped, so a Gateway that
    /// changes shape cannot produce a session with an empty id that every other
    /// session would then be compared against.
    #[test]
    fn a_row_without_a_key_is_not_a_session() {
        assert!(session_from_row(&serde_json::json!({ "model": "m" })).is_none());
    }
}
