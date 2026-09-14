use super::{json_str, pretty_json};
use crate::agent::device_identity::{
    OpenClawDeviceIdentity, OPENCLAW_CLIENT_ID, OPENCLAW_CLIENT_MODE, OPENCLAW_DEVICE_FAMILY,
    OPENCLAW_PLATFORM, OPENCLAW_ROLE, OPENCLAW_SCOPES,
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
        Ok(rows
            .iter()
            .filter_map(|row| {
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
                })
            })
            .collect())
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
        let proposed = format!("agent:main:hermit:{}", uuid::Uuid::new_v4());
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

fn sessions_create_params(key: &str) -> serde_json::Value {
    serde_json::json!({ "key": key })
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
        let mut parsed = url::Url::parse(base_url)?;
        parsed
            .set_scheme(if parsed.scheme() == "https" {
                "wss"
            } else {
                "ws"
            })
            .map_err(|_| anyhow!("Could not build OpenClaw gateway WebSocket URL."))?;
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
                    "displayName": "Hermit GPUI",
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
                "userAgent": format!("hermit-gpui/{}", env!("CARGO_PKG_VERSION")),
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
    let _ = active_session;
}

#[cfg(test)]
mod tests {
    use super::{
        chat_send_params, is_unexpected_property_error, legacy_chat_send_params,
        legacy_session_messages_subscribe_params, session_messages_subscribe_params,
        sessions_create_params,
    };

    #[test]
    fn session_rpc_params_match_openclaw_v4_schema() {
        assert_eq!(
            sessions_create_params("agent:main:hermit:test"),
            serde_json::json!({ "key": "agent:main:hermit:test" })
        );
        assert_eq!(
            session_messages_subscribe_params("agent:main:hermit:test"),
            serde_json::json!({ "key": "agent:main:hermit:test" })
        );
        assert_eq!(
            legacy_session_messages_subscribe_params("agent:main:hermit:test"),
            serde_json::json!({ "sessionKey": "agent:main:hermit:test" })
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
}
