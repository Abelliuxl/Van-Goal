use super::{json_str, pretty_json};
use crate::jsonl_process::{JsonlProcessTransport, ProcessOutput};
use crate::models::*;
use crate::settings::BackendKind;
use anyhow::{anyhow, Result};
use futures::channel::mpsc::UnboundedSender;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

#[derive(Default)]
struct InteractionState {
    approval_response_ids: HashMap<String, serde_json::Value>,
    interaction_methods: HashMap<String, String>,
    interaction_question_ids: HashMap<String, Vec<String>>,
    interaction_payloads: HashMap<String, serde_json::Value>,
    live_session_id: Option<String>,
    active_turn_ids: HashMap<String, String>,
}

/// Shared core between the backend facade and the reader task consuming
/// subprocess output.
struct CliCore {
    kind: BackendKind,
    transport: Arc<JsonlProcessTransport>,
    events: Mutex<Option<UnboundedSender<AgentEvent>>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value>>>>,
    interactions: Mutex<InteractionState>,
}

impl CliCore {
    fn emit(&self, event: AgentEvent) {
        if let Some(sender) = self
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = sender.unbounded_send(event);
        }
    }
}

/// Adapter for Codex app-server, Claude Code stream-json, and Pi RPC CLIs,
/// each run as a local subprocess in the configured workspace.
pub struct LocalCliBackend {
    kind: BackendKind,
    core: Arc<CliCore>,
    config: Option<BackendConfig>,
    reader_task: Option<tokio::task::JoinHandle<()>>,
    output_tx: Option<mpsc::UnboundedSender<ProcessOutput>>,
}

impl LocalCliBackend {
    pub fn new(kind: BackendKind) -> Self {
        Self {
            kind,
            core: Arc::new(CliCore {
                kind,
                transport: Arc::new(JsonlProcessTransport::new()),
                events: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                interactions: Mutex::new(InteractionState::default()),
            }),
            config: None,
            reader_task: None,
            output_tx: None,
        }
    }

    fn executable_name(&self) -> &'static str {
        match self.kind {
            BackendKind::Codex => "codex",
            BackendKind::ClaudeCode => "claude",
            BackendKind::Pi => "pi",
            _ => "unknown",
        }
    }

    pub async fn probe(&self, _config: &BackendConfig) -> Result<()> {
        if crate::jsonl_process::find_executable(self.executable_name()).is_none() {
            return Err(anyhow!(
                "Could not find the {} executable.",
                self.executable_name()
            ));
        }
        Ok(())
    }

    pub async fn list_sessions(&mut self, config: &BackendConfig) -> Result<Vec<AgentSession>> {
        if self.kind != BackendKind::Codex {
            return Ok(Vec::new());
        }
        self.ensure_codex_started(config).await?;
        let result = self
            .request("thread/list", serde_json::json!({ "limit": 100 }))
            .await?;
        let rows = result
            .get("data")
            .and_then(|d| d.as_array())
            .or_else(|| result.get("threads").and_then(|t| t.as_array()))
            .cloned()
            .unwrap_or_default();
        Ok(rows
            .iter()
            .filter_map(|row| {
                let id = json_str(row, "id")?;
                Some(AgentSession {
                    id,
                    title: json_str(row, "name")
                        .or_else(|| json_str(row, "title"))
                        .or_else(|| json_str(row, "preview")),
                    cwd: json_str(row, "cwd"),
                    model: json_str(row, "model"),
                    provider: Some("Codex".into()),
                    started_at: super::json_f64(row, "createdAt"),
                    last_active: super::json_f64(row, "updatedAt"),
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
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<Vec<ChatMessage>> {
        if self.kind != BackendKind::Codex {
            return Ok(Vec::new());
        }
        self.ensure_codex_started(config).await?;
        let result = self
            .request(
                "thread/read",
                serde_json::json!({ "threadId": session_id, "includeTurns": true }),
            )
            .await?;
        Ok(codex_messages(&result))
    }

    pub async fn connect(
        &mut self,
        config: BackendConfig,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        self.disconnect();
        self.config = Some(config.clone());
        self.probe(&config).await?;
        *self.core.events.lock().unwrap_or_else(|e| e.into_inner()) = Some(events.clone());
        if self.kind == BackendKind::Codex {
            self.ensure_codex_started(&config).await?;
        }
        self.ensure_reader();
        let _ = events.unbounded_send(AgentEvent::Connected);
        Ok(())
    }

    /// Spawns the persistent output reader once; it survives process
    /// restarts because the sender side stays with the backend.
    fn ensure_reader(&mut self) {
        if self.reader_task.is_some() {
            return;
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<ProcessOutput>();
        self.output_tx = Some(tx);
        let core = self.core.clone();
        self.reader_task = Some(tokio::spawn(async move {
            loop {
                let Some(output) = rx.recv().await else {
                    break;
                };
                match output {
                    ProcessOutput::Line(line) => handle_line(&core, &line).await,
                    ProcessOutput::Exit(code) => {
                        if !core.transport.is_running() {
                            {
                                let mut pending =
                                    core.pending.lock().unwrap_or_else(|e| e.into_inner());
                                for (_, responder) in pending.drain() {
                                    let _ = responder
                                        .send(Err(anyhow!("The agent process is not running.")));
                                }
                            }
                            if code == 0 {
                                core.emit(AgentEvent::Disconnected);
                            } else {
                                core.emit(AgentEvent::Failed(format!(
                                    "{} exited with code {code}.",
                                    core.kind.display_name()
                                )));
                            }
                        }
                    }
                }
            }
        }));
    }

    pub async fn create_session(&mut self, config: &BackendConfig) -> Result<SessionIDs> {
        self.config = Some(config.clone());
        match self.kind {
            BackendKind::Codex => {
                self.ensure_codex_started(config).await?;
                let mut params = serde_json::json!({});
                if let Some(workspace) = normalized_workspace(config) {
                    params["cwd"] = serde_json::Value::String(workspace);
                }
                let result = self.request("thread/start", params).await?;
                let id = result
                    .get("thread")
                    .and_then(|thread| json_str(thread, "id"))
                    .ok_or_else(|| anyhow!("Codex returned an invalid thread/start response."))?;
                self.core
                    .interactions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .live_session_id = Some(id.clone());
                Ok(SessionIDs {
                    live_id: id.clone(),
                    stored_id: Some(id),
                })
            }
            BackendKind::ClaudeCode | BackendKind::Pi => {
                let live_id = uuid::Uuid::new_v4().to_string();
                self.core
                    .interactions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .live_session_id = Some(live_id.clone());
                self.start_interactive_process(None, config).await?;
                Ok(SessionIDs {
                    live_id: live_id.clone(),
                    stored_id: Some(live_id),
                })
            }
            _ => Err(anyhow!("unsupported")),
        }
    }

    pub async fn resume_session(
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<SessionIDs> {
        self.config = Some(config.clone());
        match self.kind {
            BackendKind::Codex => {
                self.ensure_codex_started(config).await?;
                self.request(
                    "thread/resume",
                    serde_json::json!({ "threadId": session_id }),
                )
                .await?;
            }
            BackendKind::ClaudeCode | BackendKind::Pi => {
                self.start_interactive_process(Some(session_id), config)
                    .await?;
            }
            _ => {}
        }
        self.core
            .interactions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .live_session_id = Some(session_id.to_string());
        Ok(SessionIDs {
            live_id: session_id.to_string(),
            stored_id: Some(session_id.to_string()),
        })
    }

    pub async fn submit_prompt(&mut self, session_id: &str, text: &str) -> Result<()> {
        match self.kind {
            BackendKind::Codex => {
                let result = self
                    .request(
                        "turn/start",
                        serde_json::json!({
                            "threadId": session_id,
                            "input": [{ "type": "text", "text": text }]
                        }),
                    )
                    .await?;
                if let Some(turn_id) = result.get("turn").and_then(|turn| json_str(turn, "id")) {
                    self.core
                        .interactions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .active_turn_ids
                        .insert(session_id.to_string(), turn_id);
                }
            }
            BackendKind::ClaudeCode => {
                self.core
                    .transport
                    .send(&serde_json::json!({
                        "type": "user",
                        "message": {
                            "role": "user",
                            "content": [{ "type": "text", "text": text }]
                        }
                    }))
                    .await?;
            }
            BackendKind::Pi => {
                self.core
                    .transport
                    .send(&serde_json::json!({ "type": "prompt", "message": text }))
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn respond_to_interaction(&mut self, request_id: &str, answer: &str) -> Result<()> {
        if self.kind == BackendKind::Pi {
            let confirmed = ["yes", "allow", "true"].contains(&answer.to_lowercase().as_str());
            self.core
                .transport
                .send(&serde_json::json!({
                    "type": "extension_ui_response",
                    "id": request_id,
                    "value": answer,
                    "confirmed": confirmed,
                    "cancelled": answer.is_empty()
                }))
                .await?;
            return Ok(());
        }
        let (response_id, method, question_ids, requested) = {
            let mut interactions = self
                .core
                .interactions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            (
                interactions.approval_response_ids.remove(request_id),
                interactions.interaction_methods.remove(request_id),
                interactions.interaction_question_ids.remove(request_id),
                interactions.interaction_payloads.remove(request_id),
            )
        };
        let Some(response_id) = response_id else {
            return Ok(());
        };
        let method = method.unwrap_or_default();
        if method == "item/tool/requestUserInput" {
            let ids = question_ids.unwrap_or_default();
            let mut answers = serde_json::Map::new();
            for (index, id) in ids.iter().enumerate() {
                answers.insert(
                    id.clone(),
                    serde_json::json!({
                        "answers": if index == 0 && !answer.is_empty() { vec![answer] } else { vec![] }
                    }),
                );
            }
            self.core
                .transport
                .send(&serde_json::json!({ "id": response_id, "result": { "answers": answers } }))
                .await?;
        } else if method == "item/permissions/requestApproval" {
            let requested = requested.unwrap_or_else(|| serde_json::json!({}));
            self.core
                .transport
                .send(&serde_json::json!({
                    "id": response_id,
                    "result": { "permissions": if answer == "decline" { serde_json::json!({}) } else { requested } }
                }))
                .await?;
        } else if method == "mcpServer/elicitation/request" {
            let content = if answer == "decline" {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(answer.to_string())
            };
            self.core
                .transport
                .send(&serde_json::json!({
                    "id": response_id,
                    "result": {
                        "action": if answer == "decline" { "decline" } else { "accept" },
                        "content": content
                    }
                }))
                .await?;
        } else {
            self.core
                .transport
                .send(&serde_json::json!({
                    "id": response_id,
                    "result": { "decision": if answer == "decline" { "decline" } else { "accept" } }
                }))
                .await?;
        }
        Ok(())
    }

    pub async fn interrupt(&mut self, session_id: &str) -> Result<()> {
        match self.kind {
            BackendKind::Codex => {
                let mut params = serde_json::json!({ "threadId": session_id });
                if let Some(turn_id) = self
                    .core
                    .interactions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .active_turn_ids
                    .get(session_id)
                    .cloned()
                {
                    params["turnId"] = serde_json::Value::String(turn_id);
                }
                self.request("turn/interrupt", params).await?;
            }
            BackendKind::Pi => {
                self.core
                    .transport
                    .send(&serde_json::json!({ "type": "abort" }))
                    .await?;
            }
            BackendKind::ClaudeCode => {
                self.core.transport.interrupt();
                self.core.emit(AgentEvent::MessageComplete(None));
            }
            _ => {}
        }
        Ok(())
    }

    pub fn disconnect(&mut self) {
        *self.core.events.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        self.output_tx = None;
        {
            let mut pending = self.core.pending.lock().unwrap_or_else(|e| e.into_inner());
            for (_, responder) in pending.drain() {
                let _ = responder.send(Err(anyhow!("The agent process is not running.")));
            }
        }
        {
            let mut interactions = self
                .core
                .interactions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            interactions.approval_response_ids.clear();
            interactions.interaction_methods.clear();
            interactions.interaction_question_ids.clear();
            interactions.interaction_payloads.clear();
            interactions.active_turn_ids.clear();
            interactions.live_session_id = None;
        }
        let transport = self.core.transport.clone();
        tokio::spawn(async move {
            transport.stop().await;
        });
    }

    async fn ensure_codex_started(&self, config: &BackendConfig) -> Result<()> {
        if self.core.transport.is_running() {
            return Ok(());
        }
        self.start_process(&["app-server".to_string()], config)
            .await?;
        self.request(
            "initialize",
            serde_json::json!({
                "clientInfo": { "name": "hermit", "title": "Hermit", "version": "0.1.0" }
            }),
        )
        .await?;
        self.core
            .transport
            .send(&serde_json::json!({ "method": "initialized", "params": {} }))
            .await?;
        Ok(())
    }

    async fn start_interactive_process(
        &self,
        resume_id: Option<&str>,
        config: &BackendConfig,
    ) -> Result<()> {
        let live_session_id = self
            .core
            .interactions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .live_session_id
            .clone();
        let arguments: Vec<String> = match self.kind {
            BackendKind::ClaudeCode => {
                let mut args: Vec<String> = [
                    "-p",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--include-partial-messages",
                    "--verbose",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
                if let Some(resume_id) = resume_id {
                    args.push("--resume".into());
                    args.push(resume_id.to_string());
                } else if let Some(live) = live_session_id {
                    args.push("--session-id".into());
                    args.push(live);
                }
                args
            }
            BackendKind::Pi => {
                let mut args: Vec<String> =
                    ["--mode", "rpc"].iter().map(|s| s.to_string()).collect();
                if let Some(resume_id) = resume_id {
                    args.push("--session".into());
                    args.push(resume_id.to_string());
                } else if let Some(live) = live_session_id {
                    args.push("--session-id".into());
                    args.push(live);
                }
                args
            }
            _ => Vec::new(),
        };
        self.start_process(&arguments, config).await
    }

    async fn start_process(&self, arguments: &[String], config: &BackendConfig) -> Result<()> {
        let Some(output_tx) = self.output_tx.clone() else {
            return Err(anyhow!("The agent process reader is not ready."));
        };
        self.core
            .transport
            .start(
                self.executable_name(),
                arguments,
                normalized_workspace(config).as_deref(),
                output_tx,
            )
            .await
    }

    async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let (responder, receiver) = oneshot::channel();
        self.core
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), responder);
        if let Err(error) = self
            .core
            .transport
            .send(&serde_json::json!({ "id": id, "method": method, "params": params }))
            .await
        {
            self.core
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(std::time::Duration::from_secs(60), receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("The agent process request was dropped.")),
            Err(_) => Err(anyhow!("The agent process request timed out.")),
        }
    }
}

fn normalized_workspace(config: &BackendConfig) -> Option<String> {
    config
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|s| s.to_string())
}

fn codex_messages(value: &serde_json::Value) -> Vec<ChatMessage> {
    let thread = value.get("thread").unwrap_or(value);
    let turns = thread
        .get("turns")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for turn in turns {
        let items = turn
            .get("items")
            .and_then(|items| items.as_array())
            .cloned()
            .unwrap_or_default();
        for item in items {
            let item_type = json_str(&item, "type").unwrap_or_default();
            if !matches!(item_type.as_str(), "userMessage" | "agentMessage") {
                continue;
            }
            let text = if let Some(direct) = json_str(&item, "text") {
                direct
            } else if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                content
                    .iter()
                    .filter_map(|part| json_str(part, "text"))
                    .collect::<String>()
            } else {
                json_str(&item, "content").unwrap_or_default()
            };
            if text.is_empty() {
                continue;
            }
            let role = if item_type == "userMessage" {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
            out.push(ChatMessage::new(role, text));
        }
    }
    out
}

async fn handle_line(core: &Arc<CliCore>, line: &str) {
    let Ok(object) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };

    // Pending request responses
    if let Some(id) = object.get("id").map(|v| match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }) {
        let responder = core
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        if let Some(responder) = responder {
            if object.get("error").is_some() {
                let _ = responder.send(Err(anyhow!(
                    "The agent process returned an error: {}",
                    object.get("error").map(pretty_json).unwrap_or_default()
                )));
            } else {
                let result = object
                    .get("result")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let _ = responder.send(Ok(result));
            }
            return;
        }
    }

    match core.kind {
        BackendKind::Codex => handle_codex(core, &object).await,
        BackendKind::ClaudeCode => handle_claude(core, &object),
        BackendKind::Pi => handle_pi(core, &object),
        _ => {}
    }
}

async fn handle_codex(core: &Arc<CliCore>, object: &serde_json::Value) {
    let method = json_str(object, "method").unwrap_or_default();
    let params = object.get("params").cloned().unwrap_or_default();

    // Server -> client request: surface as clarify/approval.
    if object.get("id").is_some() && !method.is_empty() {
        if method == "currentTime/read" {
            let response_id = object.get("id").cloned().unwrap_or_default();
            let _ = core
                .transport
                .send(&serde_json::json!({
                    "id": response_id,
                    "result": { "currentTimeAt": now_millis() }
                }))
                .await;
            return;
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        {
            let mut interactions = core.interactions.lock().unwrap_or_else(|e| e.into_inner());
            interactions.approval_response_ids.insert(
                request_id.clone(),
                object.get("id").cloned().unwrap_or_default(),
            );
            interactions
                .interaction_methods
                .insert(request_id.clone(), method.clone());
            if method == "item/permissions/requestApproval" {
                interactions.interaction_payloads.insert(
                    request_id.clone(),
                    params.get("permissions").cloned().unwrap_or_default(),
                );
            }
        }
        let mut question = json_str(&params, "command")
            .or_else(|| json_str(&params, "reason"))
            .unwrap_or_else(|| method.clone());
        let mut choices = vec!["accept".to_string(), "decline".to_string()];
        if method == "item/tool/requestUserInput" {
            if let Some(questions) = params.get("questions").and_then(|q| q.as_array()) {
                if let Some(first) = questions.first() {
                    question = json_str(first, "question").unwrap_or(question);
                    choices = first
                        .get("options")
                        .and_then(|options| options.as_array())
                        .map(|options| {
                            options
                                .iter()
                                .filter_map(|option| json_str(option, "label"))
                                .collect()
                        })
                        .unwrap_or(choices);
                    let ids: Vec<String> =
                        questions.iter().filter_map(|q| json_str(q, "id")).collect();
                    core.interactions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .interaction_question_ids
                        .insert(request_id.clone(), ids);
                }
            }
        }
        core.emit(AgentEvent::Clarify {
            question,
            choices,
            request_id,
            session_id: json_str(&params, "threadId"),
        });
        return;
    }

    match method.as_str() {
        "turn/started" => core.emit(AgentEvent::MessageStart),
        "item/agentMessage/delta" => {
            if let Some(delta) = json_str(&params, "delta") {
                core.emit(AgentEvent::MessageDelta {
                    text: delta,
                    source: DeltaSource::EventStream,
                });
            }
        }
        "item/started" | "item/completed" => {
            if let Some(item) = params.get("item") {
                if json_str(item, "type").as_deref() != Some("agentMessage") {
                    core.emit(AgentEvent::Tool(ToolCallRecord::new(
                        json_str(item, "type").unwrap_or_else(|| "Codex item".into()),
                        if method.ends_with("completed") {
                            "completed"
                        } else {
                            "started"
                        },
                        pretty_json(item),
                    )));
                }
            }
        }
        "turn/completed" => {
            if let Some(thread_id) = json_str(&params, "threadId") {
                core.interactions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .active_turn_ids
                    .remove(&thread_id);
            }
            let turn = params.get("turn").cloned().unwrap_or_default();
            if json_str(&turn, "status").as_deref() == Some("failed") {
                core.emit(AgentEvent::TurnFailed(pretty_json(
                    turn.get("error").unwrap_or(&turn),
                )));
            } else {
                core.emit(AgentEvent::MessageComplete(None));
            }
        }
        _ => {}
    }
}

fn handle_claude(core: &Arc<CliCore>, object: &serde_json::Value) {
    match json_str(object, "type").as_deref() {
        Some("system") => {
            if let Some(session_id) = json_str(object, "session_id") {
                core.interactions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .live_session_id = Some(session_id);
            }
        }
        Some("stream_event") => {
            let event = object.get("event").cloned().unwrap_or_default();
            if json_str(&event, "type").as_deref() == Some("content_block_delta") {
                if let Some(delta) = event.get("delta") {
                    if let Some(text) = json_str(delta, "text") {
                        core.emit(AgentEvent::MessageDelta {
                            text,
                            source: DeltaSource::EventStream,
                        });
                    }
                }
            }
        }
        Some("assistant") => {
            core.emit(AgentEvent::MessageStart);
            let message = object.get("message").cloned().unwrap_or_default();
            if let Some(blocks) = message.get("content").and_then(|c| c.as_array()) {
                for block in blocks {
                    if json_str(block, "type").as_deref() == Some("tool_use") {
                        core.emit(AgentEvent::Tool(ToolCallRecord::new(
                            json_str(block, "name").unwrap_or_else(|| "tool".into()),
                            "started",
                            pretty_json(block),
                        )));
                    }
                }
            }
        }
        Some("result") => {
            if object.get("is_error").and_then(|e| e.as_bool()) == Some(true) {
                core.emit(AgentEvent::TurnFailed(
                    json_str(object, "result").unwrap_or_else(|| "Claude Code turn failed.".into()),
                ));
            } else {
                core.emit(AgentEvent::MessageComplete(json_str(object, "result")));
            }
        }
        _ => {}
    }
}

fn handle_pi(core: &Arc<CliCore>, object: &serde_json::Value) {
    match json_str(object, "type").as_deref() {
        Some("agent_start") => core.emit(AgentEvent::MessageStart),
        Some("message_update") => {
            let update = object
                .get("assistantMessageEvent")
                .cloned()
                .unwrap_or_default();
            if json_str(&update, "type").as_deref() == Some("text_delta") {
                if let Some(delta) = json_str(&update, "delta") {
                    core.emit(AgentEvent::MessageDelta {
                        text: delta,
                        source: DeltaSource::EventStream,
                    });
                }
            }
        }
        Some(event_type)
            if event_type == "tool_execution_start" || event_type == "tool_execution_end" =>
        {
            core.emit(AgentEvent::Tool(ToolCallRecord::new(
                json_str(object, "toolName").unwrap_or_else(|| "tool".into()),
                if event_type == "tool_execution_end" {
                    "completed"
                } else {
                    "started"
                },
                pretty_json(object),
            )));
        }
        Some("agent_end") => core.emit(AgentEvent::MessageComplete(None)),
        Some("extension_ui_request") => {
            let request_id =
                json_str(object, "id").unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let live_session_id = core
                .interactions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .live_session_id
                .clone();
            core.emit(AgentEvent::Clarify {
                question: json_str(object, "title")
                    .or_else(|| json_str(object, "message"))
                    .unwrap_or_else(|| "Pi needs input".into()),
                choices: object
                    .get("options")
                    .and_then(|options| options.as_array())
                    .map(|options| {
                        options
                            .iter()
                            .filter_map(|o| o.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                request_id,
                session_id: live_session_id,
            });
        }
        _ => {}
    }
}
