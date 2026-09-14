use crate::models::*;
use crate::settings::BackendKind;
use anyhow::Result;
use futures::channel::mpsc::UnboundedSender;

pub mod cli;
pub mod device_identity;
pub mod hermes;
pub mod openclaw;
pub mod opencode;

/// Backend-neutral facade consumed by AppState. Each variant translates its
/// native protocol (REST, WebSocket, SSE, subprocess JSONL) into AgentEvent.
pub enum Backend {
    Hermes(hermes::HermesBackend),
    OpenCode(opencode::OpenCodeBackend),
    MiMoCode(opencode::OpenCodeBackend),
    Codex(cli::LocalCliBackend),
    ClaudeCode(cli::LocalCliBackend),
    Pi(cli::LocalCliBackend),
    OpenClaw(openclaw::OpenClawBackend),
}

impl Backend {
    pub fn make(kind: BackendKind) -> Self {
        match kind {
            BackendKind::Hermes => Backend::Hermes(hermes::HermesBackend::new()),
            BackendKind::OpenCode => Backend::OpenCode(opencode::OpenCodeBackend::new("OpenCode")),
            BackendKind::MiMoCode => Backend::MiMoCode(opencode::OpenCodeBackend::new("MiMoCode")),
            BackendKind::Codex => Backend::Codex(cli::LocalCliBackend::new(BackendKind::Codex)),
            BackendKind::ClaudeCode => {
                Backend::ClaudeCode(cli::LocalCliBackend::new(BackendKind::ClaudeCode))
            }
            BackendKind::Pi => Backend::Pi(cli::LocalCliBackend::new(BackendKind::Pi)),
            BackendKind::OpenClaw => Backend::OpenClaw(openclaw::OpenClawBackend::new()),
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Backend::Hermes(_) => "hermes",
            Backend::OpenCode(_) => "opencode",
            Backend::MiMoCode(_) => "mimocode",
            Backend::Codex(_) => "codex",
            Backend::ClaudeCode(_) => "claudecode",
            Backend::Pi(_) => "pi",
            Backend::OpenClaw(_) => "openclaw",
        }
    }

    pub fn capabilities(&self) -> BackendCaps {
        match self {
            Backend::Hermes(_) => {
                BackendCaps::SESSION_HISTORY
                    | BackendCaps::SESSION_RESUME
                    | BackendCaps::INTERRUPT
                    | BackendCaps::USER_INTERACTION
                    | BackendCaps::TOOL_EVENTS
                    | BackendCaps::ATTACHMENTS
                    | BackendCaps::MODEL_SELECTION
                    | BackendCaps::PERMISSION_MODES
            }
            Backend::OpenCode(_) | Backend::MiMoCode(_) => {
                BackendCaps::SESSION_HISTORY
                    | BackendCaps::SESSION_RESUME
                    | BackendCaps::INTERRUPT
                    | BackendCaps::USER_INTERACTION
                    | BackendCaps::TOOL_EVENTS
                    | BackendCaps::ATTACHMENTS
            }
            Backend::Codex(_) => {
                BackendCaps::SESSION_HISTORY
                    | BackendCaps::SESSION_RESUME
                    | BackendCaps::INTERRUPT
                    | BackendCaps::USER_INTERACTION
                    | BackendCaps::TOOL_EVENTS
                    | BackendCaps::ATTACHMENTS
            }
            Backend::ClaudeCode(_) => {
                BackendCaps::SESSION_RESUME
                    | BackendCaps::INTERRUPT
                    | BackendCaps::TOOL_EVENTS
                    | BackendCaps::ATTACHMENTS
            }
            Backend::Pi(_) => {
                BackendCaps::SESSION_RESUME
                    | BackendCaps::INTERRUPT
                    | BackendCaps::USER_INTERACTION
                    | BackendCaps::TOOL_EVENTS
                    | BackendCaps::ATTACHMENTS
            }
            Backend::OpenClaw(_) => {
                BackendCaps::SESSION_HISTORY
                    | BackendCaps::SESSION_RESUME
                    | BackendCaps::INTERRUPT
                    | BackendCaps::USER_INTERACTION
                    | BackendCaps::TOOL_EVENTS
                    | BackendCaps::ATTACHMENTS
            }
        }
    }

    pub async fn probe(&mut self, config: &BackendConfig) -> Result<()> {
        match self {
            Backend::Hermes(b) => b.probe(config).await,
            Backend::OpenCode(b) | Backend::MiMoCode(b) => b.probe(config).await,
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => b.probe(config).await,
            Backend::OpenClaw(b) => b.probe(config).await,
        }
    }

    pub async fn discover_credential(&mut self, base_url: &str) -> Result<String> {
        match self {
            Backend::Hermes(b) => b.discover_credential(base_url).await,
            _ => Ok(String::new()),
        }
    }

    pub async fn list_sessions(&mut self, config: &BackendConfig) -> Result<Vec<AgentSession>> {
        match self {
            Backend::Hermes(b) => b.list_sessions(config).await,
            Backend::OpenCode(b) | Backend::MiMoCode(b) => b.list_sessions(config).await,
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => {
                b.list_sessions(config).await
            }
            Backend::OpenClaw(b) => b.list_sessions(config).await,
        }
    }

    pub async fn messages(
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<Vec<ChatMessage>> {
        match self {
            Backend::Hermes(b) => b.messages(config, session_id).await,
            Backend::OpenCode(b) | Backend::MiMoCode(b) => b.messages(config, session_id).await,
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => {
                b.messages(config, session_id).await
            }
            Backend::OpenClaw(b) => b.messages(config, session_id).await,
        }
    }

    pub async fn connect(
        &mut self,
        config: BackendConfig,
        events: UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        match self {
            Backend::Hermes(b) => b.connect(config, events).await,
            Backend::OpenCode(b) | Backend::MiMoCode(b) => b.connect(config, events).await,
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => {
                b.connect(config, events).await
            }
            Backend::OpenClaw(b) => b.connect(config, events).await,
        }
    }

    pub async fn create_session(&mut self, config: &BackendConfig) -> Result<SessionIDs> {
        match self {
            Backend::Hermes(b) => b.create_session(config).await,
            Backend::OpenCode(b) | Backend::MiMoCode(b) => b.create_session(config).await,
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => {
                b.create_session(config).await
            }
            Backend::OpenClaw(b) => b.create_session(config).await,
        }
    }

    pub async fn resume_session(
        &mut self,
        config: &BackendConfig,
        session_id: &str,
    ) -> Result<SessionIDs> {
        match self {
            Backend::Hermes(b) => b.resume_session(config, session_id).await,
            Backend::OpenCode(b) | Backend::MiMoCode(b) => {
                b.resume_session(config, session_id).await
            }
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => {
                b.resume_session(config, session_id).await
            }
            Backend::OpenClaw(b) => b.resume_session(config, session_id).await,
        }
    }

    pub async fn submit_prompt(&mut self, session_id: &str, text: &str) -> Result<()> {
        match self {
            Backend::Hermes(b) => b.submit_prompt(session_id, text).await,
            Backend::OpenCode(b) | Backend::MiMoCode(b) => b.submit_prompt(session_id, text).await,
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => {
                b.submit_prompt(session_id, text).await
            }
            Backend::OpenClaw(b) => b.submit_prompt(session_id, text).await,
        }
    }

    pub async fn respond_to_interaction(&mut self, request_id: &str, answer: &str) -> Result<()> {
        match self {
            Backend::Hermes(b) => b.respond_to_interaction(request_id, answer).await,
            Backend::OpenCode(b) | Backend::MiMoCode(b) => {
                b.respond_to_interaction(request_id, answer).await
            }
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => {
                b.respond_to_interaction(request_id, answer).await
            }
            Backend::OpenClaw(b) => b.respond_to_interaction(request_id, answer).await,
        }
    }

    pub async fn interrupt(&mut self, session_id: &str) -> Result<()> {
        match self {
            Backend::Hermes(b) => b.interrupt(session_id).await,
            Backend::OpenCode(b) | Backend::MiMoCode(b) => b.interrupt(session_id).await,
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => {
                b.interrupt(session_id).await
            }
            Backend::OpenClaw(b) => b.interrupt(session_id).await,
        }
    }

    pub fn disconnect(&mut self) {
        match self {
            Backend::Hermes(b) => b.disconnect(),
            Backend::OpenCode(b) | Backend::MiMoCode(b) => b.disconnect(),
            Backend::Codex(b) | Backend::ClaudeCode(b) | Backend::Pi(b) => b.disconnect(),
            Backend::OpenClaw(b) => b.disconnect(),
        }
    }
}

// JSON helpers shared by the protocol adapters.
pub(crate) fn json_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

pub(crate) fn json_f64(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(|v| v.as_f64())
}

pub(crate) fn json_i64(value: &serde_json::Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|v| v.as_i64())
}

pub(crate) fn normalize_seconds(value: Option<f64>) -> Option<f64> {
    match value {
        Some(raw) if raw > 10_000_000_000.0 => Some(raw / 1_000.0),
        other => other,
    }
}
