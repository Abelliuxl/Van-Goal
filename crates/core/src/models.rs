use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[macro_export]
macro_rules! bitflags_like {
    ($name:ident($t:ty) { $($variant:ident = $value:expr;)* }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name($t);

        impl $name {
            $(pub const $variant: Self = Self($value);)*

            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }
        }

        impl std::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }
    };
}

/// Connection state of the selected backend, mirroring the SwiftUI version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Degraded(String),
    Failed(String),
}

impl ConnectionState {
    pub fn label(&self) -> String {
        match self {
            ConnectionState::Disconnected => "Disconnected".into(),
            ConnectionState::Connecting => "Connecting".into(),
            ConnectionState::Connected => "Connected".into(),
            ConnectionState::Degraded(m) => format!("Limited: {m}"),
            ConnectionState::Failed(m) => format!("Failed: {m}"),
        }
    }

    pub fn pill_label(&self) -> &'static str {
        match self {
            ConnectionState::Connected => "Online",
            ConnectionState::Connecting => "Connecting",
            ConnectionState::Disconnected => "Offline",
            ConnectionState::Degraded(_) => "Limited",
            ConnectionState::Failed(_) => "Error",
        }
    }
}

pub fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A session as advertised by a backend (or created locally).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentSession {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub started_at: Option<f64>,
    #[serde(default)]
    pub last_active: Option<f64>,
    #[serde(default)]
    pub message_count: Option<i64>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub backend_id: Option<String>,
    /// Tokens this session's context is currently holding, as the backend
    /// reports it. `None` when the backend says nothing, which is not the same
    /// as zero: the frontends fall back to an estimate and mark it as one,
    /// rather than presenting a guess as a measurement.
    #[serde(default)]
    pub used_tokens: Option<i64>,
    /// The context window this session's model has, as the backend reports it.
    #[serde(default)]
    pub context_tokens: Option<i64>,
}

impl AgentSession {
    pub fn display_title(&self) -> String {
        match self.title.as_deref() {
            Some(title) if !title.trim().is_empty() => title.to_string(),
            _ => self.id.clone(),
        }
    }

    pub fn subtitle(&self) -> String {
        [self.model.as_deref(), self.cwd.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// A provider/model pair exposed by the Hermes config cache.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    pub provider: String,
    pub model: String,
}

/// Files attached to the composer or a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComposerAttachment {
    pub id: String,
    pub path: String,
}

impl ComposerAttachment {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            path: path.into(),
        }
    }

    pub fn name(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.clone())
    }

    pub fn kind_label(&self) -> &'static str {
        if std::path::Path::new(&self.path).is_dir() {
            "Folder"
        } else {
            "File"
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used_tokens: i64,
    pub max_tokens: i64,
    /// Whether the backend reported these numbers, or the client estimated them
    /// from the text it holds. A guess is worth showing — it is the only thing
    /// available on a backend that reports nothing — but it must not be drawn
    /// as if it were measured, so the frontends mark it.
    pub measured: bool,
}

impl ContextUsage {
    /// The numbers a backend reported for this session.
    pub fn measured(used_tokens: i64, max_tokens: i64) -> Self {
        Self {
            used_tokens,
            max_tokens,
            measured: true,
        }
    }

    /// The numbers a frontend worked out for itself, which the backend has not
    /// confirmed.
    pub fn estimated(used_tokens: i64, max_tokens: i64) -> Self {
        Self {
            used_tokens,
            max_tokens,
            measured: false,
        }
    }

    pub fn ratio(&self) -> f32 {
        if self.max_tokens <= 0 {
            return 0.0;
        }
        (self.used_tokens as f32 / self.max_tokens as f32).clamp(0.0, 1.0)
    }

    pub fn percent(&self) -> i32 {
        (self.ratio() * 100.0).round() as i32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    FullAccess,
    AskBeforeRisky,
    RestrictedTools,
}

impl PermissionMode {
    pub const ALL: [PermissionMode; 3] = [
        PermissionMode::FullAccess,
        PermissionMode::AskBeforeRisky,
        PermissionMode::RestrictedTools,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            PermissionMode::FullAccess => "Full access",
            PermissionMode::AskBeforeRisky => "Ask first",
            PermissionMode::RestrictedTools => "Restricted",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

impl MessageRole {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(MessageRole::User),
            "assistant" => Some(MessageRole::Assistant),
            "system" => Some(MessageRole::System),
            "tool" => Some(MessageRole::Tool),
            _ => None,
        }
    }
}

/// One chat message. Tool activity is recorded inside the assistant message it
/// belongs to, like the SwiftUI version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub timestamp: f64,
    pub is_streaming: bool,
    #[serde(default)]
    pub completed_at: Option<f64>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
    #[serde(default)]
    pub attachments: Vec<ComposerAttachment>,
}

impl ChatMessage {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            content: content.into(),
            timestamp: now_unix(),
            is_streaming: false,
            completed_at: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }

    pub fn streaming(role: MessageRole) -> Self {
        let mut message = Self::new(role, "");
        message.is_streaming = true;
        message
    }

    pub fn is_empty_shell(&self) -> bool {
        self.role == MessageRole::Assistant
            && !self.is_streaming
            && self.content.trim().is_empty()
            && self.tool_calls.is_empty()
            && self.attachments.is_empty()
    }
}

/// A single tool event rendered as an expandable pill inside a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    pub status: String,
    pub detail: String,
    pub timestamp: f64,
}

impl ToolCallRecord {
    pub fn new(
        name: impl Into<String>,
        status: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            status: status.into(),
            detail: detail.into(),
            timestamp: now_unix(),
        }
    }
}

/// A prompt waiting in the visible queue while another turn is running.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueuedPrompt {
    pub id: String,
    pub text: String,
    pub attachments: Vec<ComposerAttachment>,
    pub created_at: f64,
}

impl QueuedPrompt {
    pub fn new(text: impl Into<String>, attachments: Vec<ComposerAttachment>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            text: text.into(),
            attachments,
            created_at: now_unix(),
        }
    }
}

/// A pending clarify / approval / permission question raised by the backend.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingClarify {
    pub session_id: Option<String>,
    pub question: String,
    pub choices: Vec<String>,
    pub request_id: String,
}

/// On-disk snapshot of sessions, transcripts and local archive/delete markers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CachedState {
    #[serde(default)]
    pub sessions: Vec<AgentSession>,
    #[serde(default)]
    pub messages_by_session_id: std::collections::HashMap<String, Vec<ChatMessage>>,
    #[serde(default)]
    pub selected_session_id: Option<String>,
    #[serde(default)]
    pub archived_session_ids: std::collections::HashSet<String>,
    #[serde(default)]
    pub deleted_session_ids: std::collections::HashSet<String>,
    #[serde(default)]
    pub updated_at: Option<f64>,
}

/// IDs reported by a backend after create/resume: the live turn owner and the
/// persistent transcript key.
#[derive(Clone, Debug)]
pub struct SessionIDs {
    pub live_id: String,
    pub stored_id: Option<String>,
}

/// Per-backend connection inputs handed to every backend call.
#[derive(Clone, Debug, Default)]
pub struct BackendConfig {
    pub base_url: String,
    pub credential: String,
    pub profile: Option<String>,
    pub workspace: Option<String>,
}

bitflags_like! {
    BackendCaps(u32) {
        SESSION_HISTORY = 1 << 0;
        SESSION_RESUME  = 1 << 1;
        INTERRUPT       = 1 << 2;
        USER_INTERACTION = 1 << 3;
        TOOL_EVENTS     = 1 << 4;
        ATTACHMENTS     = 1 << 5;
        MODEL_SELECTION = 1 << 6;
        PERMISSION_MODES = 1 << 7;
    }
}

/// Which event stream a streamed chunk of a reply arrived on.
///
/// A gateway can deliver the *same* reply over more than one stream at once
/// (OpenClaw sends both a `session.message` transcript and an `agent`/assistant
/// event stream). Appending all of them into one buffer interleaves two copies
/// of the message, so chunks are tagged and buffered per stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeltaSource {
    /// `session.message` / `chat` frames carrying `deltaText`.
    Transcript,
    /// `agent` frames with `stream: "assistant"` (`data.delta`).
    AgentStream,
    /// Item / event-log streams (`message.delta`, `message.part.updated`,
    /// `item/agentMessage/delta`, …).
    EventStream,
}

/// Events every backend normalizes into; consumed by AppState.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    Connected,
    SessionInfo(String),
    MessageStart,
    MessageDelta {
        text: String,
        source: DeltaSource,
    },
    MessageComplete(Option<String>),
    /// The gateway's session list changed (renamed, created, finished a turn).
    /// Carries no payload: it is a hint to re-list.
    SessionsChanged,
    TurnFailed(String),
    Tool(ToolCallRecord),
    Clarify {
        question: String,
        choices: Vec<String>,
        request_id: String,
        session_id: Option<String>,
    },
    Disconnected,
    Failed(String),
}

pub fn pretty_json(value: &serde_json::Value) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(text) => text,
        Err(_) => value.to_string(),
    }
}

/// Token count formatting shared by the composer ring and tooltip.
pub fn compact_token_count(value: i64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{}k", ((value as f64) / 1_000.0).round() as i64)
    } else {
        value.to_string()
    }
}

pub fn duration_string(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}
