//! Adapter for the 雪小米 gateway: a self-hosted agent behind an HTTPS gateway
//! that also relays WeChat, Feishu and iMessage conversations.
//!
//! The protocol is small and measured against the gateway itself:
//!
//! | Request | What it does |
//! | --- | --- |
//! | `GET api/status` | health, the configured model, the relayed channels |
//! | `GET api/models` | models with their thinking levels, and the defaults |
//! | `GET api/sessions` | every session, newest first; channel and archived ones are `readOnly` |
//! | `GET api/sessions/{id}` | one session's messages (`role`, `text`, ISO `timestamp`); 404 when unknown |
//! | `POST api/chat` | `{session, message, stream: true}` → SSE `delta` / `done` / `error` |
//! | `POST api/abort` | `{session}` → `{stopped}`; the turn's stream then ends with `error` |
//! | `POST api/sessions` | `{from}` → a writable copy of a read-only snapshot |
//!
//! Every request carries `Authorization: Bearer <gateway token>`.
//!
//! **Sessions are named by the client.** The gateway creates a session the
//! first time a prompt is sent under an id it has not seen, so "create" is
//! choosing an id, and nothing exists on the gateway until the first turn.
//!
//! **One stream per turn.** There is no event socket: a turn's reply arrives on
//! the response to the `POST api/chat` that started it, so every event is
//! labelled by the session that request named — and passes the same strict
//! rule as every other adapter's (`session_scope::belongs_to_open_session`).
//! A turn the user left running in another session keeps streaming on the
//! gateway; it just does not write into the chat now on screen. `delta`
//! carries the next piece of text; `done` carries the whole reply.
//!
//! **Channel conversations are read-only snapshots.** A WeChat or Feishu chat is
//! shown with its history, and sending into it is refused by the gateway
//! ("这是历史快照，请先继续为新会话"). Sending into one here continues it as a
//! new session first, which is what the gateway's own web client offers.

use super::session_scope::belongs_to_open_session;
use crate::models::*;
use anyhow::{anyhow, Result};
use futures::channel::mpsc::UnboundedSender;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod files;
pub use files::attachment_bytes;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// A turn can run tools for minutes; only the connect is bounded.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// What the gateway prefixes onto a message relayed from a channel. Only the
/// display drops it: the agent reads it to know who is talking.
const RELAY_PREFIX: &str = "[网关可信来源";

#[derive(Default)]
struct Shared {
    events: Option<UnboundedSender<AgentEvent>>,
    /// The session on screen, set by create/resume and cleared for a blank
    /// chat. A turn's events reach the frontend only while this names it.
    active: Option<String>,
    /// Sessions the gateway reports as read-only snapshots, and the writable
    /// session each one was continued as.
    read_only: HashMap<String, Option<String>>,
    /// The turn being streamed, per session, so a stop can end it.
    running: HashMap<String, tokio::task::JoinHandle<()>>,
}

pub struct XueXiaoMiBackend {
    http: reqwest::Client,
    /// Without an overall timeout: a turn's stream lives as long as the turn.
    stream_http: reqwest::Client,
    config: Option<BackendConfig>,
    shared: Arc<Mutex<Shared>>,
}

impl Default for XueXiaoMiBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl XueXiaoMiBackend {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .unwrap_or_default(),
            stream_http: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .unwrap_or_default(),
            config: None,
            shared: Arc::new(Mutex::new(Shared::default())),
        }
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn config(&self) -> Result<&BackendConfig> {
        self.config
            .as_ref()
            .ok_or_else(|| anyhow!("雪小米 is not connected."))
    }

    async fn get(&self, config: &BackendConfig, path: &str) -> Result<serde_json::Value> {
        let request = self.http.get(url(config, path));
        read_json(authorized(request, config).send().await?).await
    }

    async fn post(
        &self,
        config: &BackendConfig,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let request = self.http.post(url(config, path)).json(&body);
        read_json(authorized(request, config).send().await?).await
    }

    pub async fn probe(&self, config: &BackendConfig) -> Result<()> {
        if config.credential.trim().is_empty() {
            return Err(anyhow!("雪小米 needs its gateway token."));
        }
        let status = self.get(config, "api/status").await?;
        if status.get("model").is_none() && status.get("started").is_none() {
            return Err(anyhow!(
                "That address did not answer like a 雪小米 gateway."
            ));
        }
        Ok(())
    }

    pub async fn connect(
        &mut self,
        config: BackendConfig,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        self.disconnect();
        self.probe(&config).await?;
        self.config = Some(config);
        let _ = events.unbounded_send(AgentEvent::Connected);
        self.shared().events = Some(events);
        Ok(())
    }

    pub async fn list_sessions(&mut self, config: &BackendConfig) -> Result<Vec<AgentSession>> {
        let value = self.get(config, "api/sessions").await?;
        let rows = value
            .get("sessions")
            .and_then(|rows| rows.as_array())
            .cloned()
            .unwrap_or_default();
        let sessions: Vec<AgentSession> = rows.iter().filter_map(session_from_row).collect();
        let mut shared = self.shared();
        for row in &rows {
            if let (Some(id), Some(true)) = (
                row.get("id").and_then(|v| v.as_str()),
                row.get("readOnly").and_then(|v| v.as_bool()),
            ) {
                shared.read_only.entry(id.to_string()).or_insert(None);
            }
        }
        Ok(sessions)
    }

    pub async fn messages(
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<Vec<ChatMessage>> {
        let path = format!("api/sessions/{}", encode(session_id));
        let value = match self.get(config, &path).await {
            Ok(value) => value,
            // A session this client named and has not sent to yet does not
            // exist on the gateway, which is not an error worth showing.
            Err(error) if error.to_string().contains("HTTP 404") => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        if value
            .get("session")
            .and_then(|s| s.get("readOnly"))
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            self.shared()
                .read_only
                .entry(session_id.to_string())
                .or_insert(None);
        }
        let rows = value
            .get("messages")
            .and_then(|rows| rows.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(rows.iter().filter_map(message_from_row).collect())
    }

    /// A new session is an id this client chooses; the gateway records it with
    /// the first prompt.
    pub async fn create_session(&mut self, _config: &BackendConfig) -> Result<SessionIDs> {
        let id = format!("van-goal-{}", uuid::Uuid::new_v4().simple());
        self.shared().active = Some(id.clone());
        Ok(SessionIDs {
            live_id: id.clone(),
            stored_id: Some(id),
        })
    }

    pub async fn resume_session(
        &mut self,
        _config: &BackendConfig,
        session_id: &str,
    ) -> Result<SessionIDs> {
        self.shared().active = Some(session_id.to_string());
        Ok(SessionIDs {
            live_id: session_id.to_string(),
            stored_id: Some(session_id.to_string()),
        })
    }

    pub async fn submit_prompt(&mut self, session_id: &str, text: &str) -> Result<()> {
        self.submit_with_attachments(session_id, text, &[]).await
    }

    pub async fn submit_with_attachments(
        &mut self,
        session_id: &str,
        text: &str,
        attachments: &[ComposerAttachment],
    ) -> Result<()> {
        let config = self.config()?.clone();
        let events = self
            .shared()
            .events
            .clone()
            .ok_or_else(|| anyhow!("雪小米 is not connected."))?;

        // Validate every file before uploading any of them or continuing a snapshot.
        let prepared = files::prepare(attachments)?;
        // A channel conversation is a snapshot; it takes new prompts as a
        // writable copy, continued once and reused after that.
        let target = self.writable_session(&config, session_id).await?;

        let mut images = Vec::new();
        let mut paths = Vec::new();
        for file in prepared {
            if let Some(mime) = file.mime {
                images.push(serde_json::json!({ "data": file.data, "mimeType": mime }));
            } else {
                let uploaded = self
                    .post(
                        &config,
                        "api/upload",
                        serde_json::json!({
                            "session": target, "name": file.name, "data": file.data,
                        }),
                    )
                    .await?;
                paths.push(
                    uploaded
                        .get("path")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            anyhow!("The gateway did not return the uploaded file's path.")
                        })?
                        .to_string(),
                );
            }
        }
        let mut message = if text.is_empty() && !attachments.is_empty() {
            "（见附件）".to_string()
        } else {
            text.to_string()
        };
        if !paths.is_empty() {
            message.push_str(&format!("\n\n[附件：{}]", paths.join("、")));
        }

        let request = self
            .stream_http
            .post(url(&config, "api/chat"))
            .json(&serde_json::json!({
                "session": target,
                "message": message,
                "images": images,
                "stream": true,
            }));
        let response = authorized(request, &config)
            .header("Accept", "text/event-stream")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(error_from(response).await);
        }

        let _ = events.unbounded_send(AgentEvent::MessageStart);
        let shared = self.shared.clone();
        let key = session_id.to_string();
        let task = tokio::spawn(stream_turn(
            response,
            events,
            shared.clone(),
            key.clone(),
            config,
        ));
        let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = guard.running.insert(key, task) {
            previous.abort();
        }
        Ok(())
    }

    async fn writable_session(&self, config: &BackendConfig, session_id: &str) -> Result<String> {
        let continued = match self.shared().read_only.get(session_id) {
            None => return Ok(session_id.to_string()),
            Some(continued) => continued.clone(),
        };
        if let Some(continued) = continued {
            return Ok(continued);
        }
        let created = self
            .post(
                config,
                "api/sessions",
                serde_json::json!({ "from": session_id }),
            )
            .await
            .map_err(|error| {
                anyhow!("Could not continue the snapshot as a new session: {error}")
            })?;
        let id = created
            .get("session")
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("The gateway did not say which session it continued as."))?
            .to_string();
        self.shared()
            .read_only
            .insert(session_id.to_string(), Some(id.clone()));
        if let Some(events) = self.shared().events.clone() {
            let _ = events.unbounded_send(AgentEvent::SessionsChanged);
        }
        Ok(id)
    }

    /// The gateway has no questions to ask mid-turn.
    pub async fn respond_to_interaction(&mut self, _request_id: &str, _answer: &str) -> Result<()> {
        Ok(())
    }

    pub async fn interrupt(&mut self, session_id: &str) -> Result<()> {
        let config = self.config()?.clone();
        let target = self
            .shared()
            .read_only
            .get(session_id)
            .cloned()
            .flatten()
            .unwrap_or_else(|| session_id.to_string());
        self.post(
            &config,
            "api/abort",
            serde_json::json!({ "session": target }),
        )
        .await?;
        Ok(())
    }

    pub fn clear_session_scope(&mut self) {
        self.shared().active = None;
    }

    pub fn disconnect(&mut self) {
        let mut shared = self.shared();
        for (_, task) in shared.running.drain() {
            task.abort();
        }
        shared.events = None;
        shared.active = None;
        drop(shared);
        self.config = None;
    }
}

/// Read one turn's SSE stream into events.
async fn stream_turn(
    response: reqwest::Response,
    events: UnboundedSender<AgentEvent>,
    shared: Arc<Mutex<Shared>>,
    session: String,
    config: BackendConfig,
) {
    let mut body = response.bytes_stream();
    let mut parser = SseParser::default();
    let mut ended = false;
    'read: while let Some(chunk) = body.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                if on_screen(&shared, &session) {
                    let _ = events.unbounded_send(AgentEvent::TurnFailed(format!(
                        "The reply stream broke: {error}"
                    )));
                }
                ended = true;
                break;
            }
        };
        for frame in parser.push(&chunk) {
            if let Some(event) = event_from_frame(&frame) {
                let last = matches!(
                    event,
                    AgentEvent::MessageComplete(_) | AgentEvent::TurnFailed(_)
                );
                if on_screen(&shared, &session) {
                    if let AgentEvent::Attachment(attachment) = &event {
                        let config = config.clone();
                        let source = attachment.path.clone();
                        // Keep short-lived, single-use links usable after this
                        // turn. A preview or Save joins the same cached fetch.
                        tokio::spawn(async move {
                            let _ = attachment_bytes(&config, &source, false).await;
                        });
                    }
                    let _ = events.unbounded_send(event);
                }
                if last {
                    ended = true;
                    break 'read;
                }
            }
        }
    }
    if !ended && on_screen(&shared, &session) {
        // The gateway closed the stream without saying how the turn ended.
        let _ = events.unbounded_send(AgentEvent::MessageComplete(None));
    }
    let _ = events.unbounded_send(AgentEvent::SessionsChanged);
    shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .running
        .remove(&session);
}

/// Whether the turn streaming for `session` belongs to the chat on screen.
fn on_screen(shared: &Mutex<Shared>, session: &str) -> bool {
    let shared = shared.lock().unwrap_or_else(|e| e.into_inner());
    belongs_to_open_session(shared.active.as_deref(), Some(session))
}

/// One server-sent event: its name and its (joined) data lines.
#[derive(Debug, PartialEq)]
struct SseFrame {
    event: String,
    data: String,
}

/// Incremental SSE parser. Frames end at a blank line; a chunk boundary can
/// fall anywhere, including inside a UTF-8 character, so bytes are buffered
/// until a whole frame is there.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some((end, separator)) = find_frame_end(&self.buffer) {
            let raw: Vec<u8> = self.buffer.drain(..end + separator).collect();
            let text = String::from_utf8_lossy(&raw[..end]);
            let mut event = String::from("message");
            let mut data = Vec::new();
            for line in text.lines() {
                if let Some(value) = line.strip_prefix("event:") {
                    event = value.trim().to_string();
                } else if let Some(value) = line.strip_prefix("data:") {
                    data.push(value.strip_prefix(' ').unwrap_or(value).to_string());
                }
            }
            if !data.is_empty() {
                frames.push(SseFrame {
                    event,
                    data: data.join("\n"),
                });
            }
        }
        frames
    }
}

fn find_frame_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn event_from_frame(frame: &SseFrame) -> Option<AgentEvent> {
    let payload: serde_json::Value = serde_json::from_str(&frame.data).ok()?;
    let text = payload.get("text").and_then(|v| v.as_str());
    match frame.event.as_str() {
        // Each delta is the next piece of the reply; `merge_stream_chunk`
        // accumulates them on the one stream this backend has.
        "delta" => Some(AgentEvent::MessageDelta {
            text: text?.to_string(),
            source: DeltaSource::EventStream,
        }),
        "done" => Some(AgentEvent::MessageComplete(text.map(str::to_string))),
        "attachment" => files::from_row(&payload).map(AgentEvent::Attachment),
        "error" => Some(AgentEvent::TurnFailed(
            payload
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("The gateway reported an error.")
                .to_string(),
        )),
        _ => None,
    }
}

fn session_from_row(row: &serde_json::Value) -> Option<AgentSession> {
    let id = row.get("id")?.as_str()?.to_string();
    let read_only = row.get("readOnly").and_then(|v| v.as_bool()) == Some(true);
    let source = row
        .get("source")
        .and_then(|v| v.as_str())
        .map(source_label)
        .filter(|label| !label.is_empty());
    let title = row
        .get("title")
        .and_then(|v| v.as_str())
        .map(strip_relay_prefix)
        .filter(|title| !title.trim().is_empty() && !title.starts_with(RELAY_PREFIX));
    Some(AgentSession {
        id,
        title: title.or_else(|| {
            Some(
                if read_only {
                    "历史对话"
                } else {
                    "新对话"
                }
                .to_string(),
            )
        }),
        // Where the conversation came from, as the sidebar's second line.
        cwd: if read_only {
            source.map(|s| format!("{s} · 只读快照"))
        } else {
            None
        },
        provider: Some("雪小米".into()),
        started_at: iso_seconds(row.get("createdAt")),
        last_active: iso_seconds(row.get("updatedAt")),
        is_active: row.get("running").and_then(|v| v.as_bool()),
        archived: Some(false),
        ..AgentSession::default()
    })
}

fn source_label(source: &str) -> String {
    match source {
        "weixin" => "微信".into(),
        "feishu" => "飞书".into(),
        "imessage" => "iMessage".into(),
        "网页 / API" => String::new(),
        other => other.to_string(),
    }
}

fn message_from_row(row: &serde_json::Value) -> Option<ChatMessage> {
    let role = MessageRole::parse(row.get("role")?.as_str()?)?;
    let text = row.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let text = if role == MessageRole::User {
        strip_relay_prefix(text)
    } else {
        text.to_string()
    };
    let mut message = ChatMessage::new(role, text);
    if role == MessageRole::User {
        // History only retains text markers, not the image bytes or upload
        // metadata. Match the original prompt to recover what this client kept.
        let mut plain = message.content.trim_end();
        while let Some(rest) = plain.strip_suffix("\n[图片]") {
            plain = rest.trim_end();
        }
        if plain.ends_with(']') {
            if let Some((prefix, note)) = plain.rsplit_once("\n\n[附件：") {
                if note[..note.len() - 1]
                    .split('、')
                    .all(|path| path.starts_with("uploads/"))
                {
                    plain = prefix;
                }
            }
        }
        if plain != message.content {
            message.attachment_match_text = Some(if plain == "（见附件）" {
                "".into()
            } else {
                plain.to_string()
            });
        }
    }
    message.attachments = row
        .get("attachments")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(files::from_row)
        .collect();
    if let Some(timestamp) = iso_seconds(row.get("timestamp")) {
        message.timestamp = timestamp;
        message.completed_at = Some(timestamp);
    }
    Some(message)
}

/// Drop the relay's provenance line from the front of a channel message.
fn strip_relay_prefix(text: &str) -> String {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix(RELAY_PREFIX) {
        if let Some(end) = rest.find(']') {
            return rest[end + 1..].trim_start().to_string();
        }
    }
    text.to_string()
}

fn iso_seconds(value: Option<&serde_json::Value>) -> Option<f64> {
    let text = value?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis() as f64 / 1000.0)
}

fn url(config: &BackendConfig, path: &str) -> String {
    format!("{}/{}", config.base_url.trim_end_matches('/'), path)
}

fn authorized(request: reqwest::RequestBuilder, config: &BackendConfig) -> reqwest::RequestBuilder {
    request.header(
        "Authorization",
        format!("Bearer {}", config.credential.trim()),
    )
}

fn encode(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

async fn read_json(response: reqwest::Response) -> Result<serde_json::Value> {
    if !response.status().is_success() {
        return Err(error_from(response).await);
    }
    let text = response.text().await?;
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_str(&text)?)
}

/// The gateway's own words for a refused request, which are what the user can
/// act on ("需要有效的网关令牌", "这是历史快照…").
async fn error_from(response: reqwest::Response) -> anyhow::Error {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    match message {
        Some(message) => anyhow!("{message} (HTTP {status})"),
        None => anyhow!("雪小米 returned HTTP {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn files_upload_and_images_arrive_as_bytes_in_the_chat_request() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = BackendConfig {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            credential: "fixture".into(),
            ..Default::default()
        };
        let server = tokio::spawn(async move {
            for step in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut raw = Vec::new();
                let (header_end, length) = loop {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(index) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&raw[..index]).to_lowercase();
                        let length = header
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .map(|n| n.parse::<usize>().unwrap())
                            .unwrap_or(0);
                        assert!(header.contains("authorization: bearer fixture"));
                        break (index + 4, length);
                    }
                };
                while raw.len() < header_end + length {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    raw.extend_from_slice(&buf[..n]);
                }
                let header = String::from_utf8_lossy(&raw[..header_end]);
                let body = &raw[header_end..header_end + length];
                let response = match step {
                    0 => {
                        assert!(header.starts_with("GET /api/status "));
                        "{\"model\":\"fixture\"}".to_string()
                    }
                    1 => {
                        assert!(header.starts_with("POST /api/upload "));
                        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
                        assert_eq!(value["name"], "input.txt");
                        assert_eq!(
                            STANDARD.decode(value["data"].as_str().unwrap()).unwrap(),
                            b"input bytes"
                        );
                        "{\"path\":\"uploads/session/input.txt\"}".to_string()
                    }
                    _ => {
                        assert!(header.starts_with("POST /api/chat "));
                        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
                        assert_eq!(
                            value["message"],
                            "inspect\n\n[附件：uploads/session/input.txt]"
                        );
                        assert_eq!(value["images"][0]["mimeType"], "image/png");
                        assert_eq!(
                            STANDARD
                                .decode(value["images"][0]["data"].as_str().unwrap())
                                .unwrap(),
                            b"\x89PNG\r\n\x1a\nfixture"
                        );
                        "event: attachment\ndata: {\"url\":\"/api/download?token=fixture\",\"name\":\"result.txt\"}\n\nevent: done\ndata: {\"text\":\"ok\"}\n\n".to_string()
                    }
                };
                let kind = if step == 2 {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
            }
        });
        let dir = std::env::temp_dir().join(format!("vg-wire-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("input.txt"), b"input bytes").unwrap();
        std::fs::write(dir.join("picture.png"), b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let (sender, mut receiver) = futures::channel::mpsc::unbounded();
        let mut backend = XueXiaoMiBackend::new();
        backend.connect(config.clone(), sender).await.unwrap();
        let session = backend.create_session(&config).await.unwrap();
        backend
            .submit_with_attachments(
                &session.live_id,
                "inspect",
                &[
                    ComposerAttachment::new(dir.join("input.txt").to_string_lossy()),
                    ComposerAttachment::new(dir.join("picture.png").to_string_lossy()),
                ],
            )
            .await
            .unwrap();
        let mut attachment = false;
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), receiver.next())
                .await
                .unwrap()
                .unwrap();
            if let AgentEvent::Attachment(a) = &event {
                assert_eq!(a.name(), "result.txt");
                attachment = true;
            }
            if matches!(event, AgentEvent::MessageComplete(_)) {
                break;
            }
        }
        assert!(attachment);
        server.await.unwrap();
        backend.disconnect();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn attachment_events_and_history_keep_the_filename_and_download_url() {
        let payload = serde_json::json!({"name": "报告.pdf", "url": "/api/files/report"});
        let event = event_from_frame(&SseFrame {
            event: "attachment".into(),
            data: payload.to_string(),
        })
        .unwrap();
        assert!(
            matches!(event, AgentEvent::Attachment(a) if a.name() == "报告.pdf" && a.path == "/api/files/report")
        );
        let message = message_from_row(
            &serde_json::json!({"role": "assistant", "text": "", "attachments": [payload]}),
        )
        .unwrap();
        assert_eq!(message.attachments.len(), 1);
        assert!(!message.is_empty_shell());
    }

    #[test]
    fn history_recovers_local_files_and_images_without_showing_upload_paths() {
        let mut conversation = crate::chat::Conversation::new();
        conversation.begin_turn_with_placeholder("inspect");
        conversation.messages_mut()[0].attachments = vec![
            ComposerAttachment::new("/tmp/report.pdf"),
            ComposerAttachment::new("/tmp/picture.png"),
        ];
        conversation.complete_message(Some("done"));
        let fetched = message_from_row(&serde_json::json!({"role": "user", "text": "inspect\n\n[附件：uploads/session/123-report.pdf]\n[图片]"})).unwrap();
        conversation.merge_transcript(vec![
            fetched,
            ChatMessage::new(MessageRole::Assistant, "done"),
        ]);
        assert_eq!(conversation.messages()[0].content, "inspect");
        assert_eq!(conversation.messages()[0].attachments.len(), 2);
        // An unfamiliar history message keeps exactly what the backend sent.
        let text = "other\n[图片]";
        conversation.merge_transcript(vec![message_from_row(
            &serde_json::json!({"role": "user", "text": text}),
        )
        .unwrap()]);
        assert_eq!(conversation.messages()[0].content, text);
    }

    /// A chunk boundary can fall inside a frame, and inside a character: the
    /// stream measured from the gateway sends CJK text eight bytes at a time.
    #[test]
    fn frames_survive_any_chunking() {
        let raw = "event: delta\ndata: {\"text\":\"雪落在\"}\n\nevent: delta\ndata: {\"text\":\"夜里\"}\n\nevent: done\ndata: {\"text\":\"雪落在夜里\",\"model\":\"m\"}\n\n";
        for size in [1, 3, 7, raw.len()] {
            let mut parser = SseParser::default();
            let mut frames = Vec::new();
            for chunk in raw.as_bytes().chunks(size) {
                frames.extend(parser.push(chunk));
            }
            let events: Vec<_> = frames.iter().filter_map(event_from_frame).collect();
            assert_eq!(events.len(), 3, "chunk size {size}");
            assert!(
                matches!(&events[0], AgentEvent::MessageDelta { text, .. } if text == "雪落在")
            );
            assert!(
                matches!(&events[2], AgentEvent::MessageComplete(Some(text)) if text == "雪落在夜里")
            );
        }
    }

    #[test]
    fn a_stopped_turn_ends_with_the_gateways_reason() {
        let mut parser = SseParser::default();
        let frames =
            parser.push("event: error\r\ndata: {\"error\":\"请求已停止\"}\r\n\r\n".as_bytes());
        assert!(matches!(
            event_from_frame(&frames[0]),
            Some(AgentEvent::TurnFailed(message)) if message == "请求已停止"
        ));
    }

    /// The deltas the gateway sends are pieces, not snapshots, and they fold
    /// into one reply through the same merge every backend uses.
    #[test]
    fn deltas_fold_into_one_reply() {
        let mut conversation = crate::chat::Conversation::new();
        conversation.begin_turn_with_placeholder("数到五");
        conversation.apply(&AgentEvent::MessageStart);
        for piece in ["一，", "二，三", "，四，", "五"] {
            conversation.apply(&AgentEvent::MessageDelta {
                text: piece.into(),
                source: DeltaSource::EventStream,
            });
        }
        conversation.apply(&AgentEvent::MessageComplete(Some(
            "一，二，三，四，五".into(),
        )));
        let messages = conversation.messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "一，二，三，四，五");
    }

    #[test]
    fn channel_sessions_are_listed_as_read_only_snapshots() {
        let row = serde_json::json!({
            "id": "history-945b",
            "readOnly": true,
            "source": "weixin",
            "title": "[网关可信来源：weixin 私聊；身份：亮仔]\n\n你好",
            "createdAt": "2026-10-19T16:12:34.706Z",
            "updatedAt": "2026-10-20T03:14:52.579Z",
            "running": false
        });
        let session = session_from_row(&row).unwrap();
        assert_eq!(session.title.as_deref(), Some("你好"));
        assert_eq!(session.cwd.as_deref(), Some("微信 · 只读快照"));
        assert!(session.last_active.unwrap() > session.started_at.unwrap());

        let web = serde_json::json!({ "id": "a", "readOnly": false, "source": "网页 / API", "title": "" });
        let session = session_from_row(&web).unwrap();
        assert_eq!(session.title.as_deref(), Some("新对话"));
        assert_eq!(session.cwd, None);
    }

    #[test]
    fn the_relay_prefix_is_hidden_from_the_transcript() {
        let row = serde_json::json!({
            "role": "user",
            "text": "[网关可信来源：weixin 私聊；身份：亮仔；时区 Asia/Shanghai。]\n\n你好",
            "timestamp": "2026-10-19T16:12:34.708Z"
        });
        assert_eq!(message_from_row(&row).unwrap().content, "你好");
        assert_eq!(strip_relay_prefix("普通消息"), "普通消息");
    }

    /// The turn the user left keeps streaming; it must not write into the
    /// chat now on screen, and it does reach its own.
    #[test]
    fn a_turn_only_reaches_the_session_on_screen() {
        let shared = Mutex::new(Shared::default());
        assert!(
            !on_screen(&shared, "a"),
            "a blank chat takes no labelled turn"
        );
        shared.lock().unwrap().active = Some("a".into());
        assert!(on_screen(&shared, "a"));
        shared.lock().unwrap().active = Some("b".into());
        assert!(!on_screen(&shared, "a"));
    }

    #[test]
    fn session_ids_are_encoded_into_the_path() {
        assert_eq!(encode("abc-123"), "abc-123");
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
    }
}
