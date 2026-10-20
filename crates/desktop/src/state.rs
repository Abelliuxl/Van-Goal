//! The desktop's state: what is on screen, which session it belongs to, and the
//! rules that decide where a turn's events and a prompt are allowed to go.
//!
//! Everything here is plain data and pure functions. The async orchestration
//! that changes it lives in [`crate::client`] and the modules beside it; the
//! split is what lets the rules be tested without a runtime or a backend.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;
use van_goal_core::agent::Backend;
use van_goal_core::chat::Conversation;
use van_goal_core::models::{
    AgentSession, BackendCaps, CachedState, ChatMessage, ComposerAttachment, ConnectionState,
    ContextUsage, MessageRole, ModelOption, PendingClarify, PermissionMode, QueuedPrompt,
};
use van_goal_core::settings::{BackendKind, Settings};

/// What the composer should do with the draft the Go side holds.
#[derive(Clone, Debug, PartialEq)]
pub enum ComposerAction {
    /// The draft was taken: empty the composer.
    Clear,
    /// Give a draft back to the composer, after whatever the user has typed
    /// since — a prompt that could not be sent is never thrown away.
    Restore {
        text: String,
        attachments: Vec<ComposerAttachment>,
    },
}

/// What changed since the Go side last polled. Kept apart so a streaming delta
/// does not re-send the whole transcript, and a sidebar change does not re-send
/// the messages.
#[derive(Default)]
pub struct Dirty {
    pub state: bool,
    pub messages: bool,
    pub settings: bool,
}

pub struct State {
    pub settings: Settings,
    pub connection: ConnectionState,
    pub sessions: Vec<AgentSession>,
    pub selected: Option<AgentSession>,
    /// The conversation on screen. The fold rules live in `chat::Conversation`
    /// so this frontend and the mobile bridge cannot disagree about them.
    pub conversation: Conversation,
    pub is_refreshing: bool,
    pub refresh_inflight: bool,
    pub transport_ready: bool,
    pub gateway_connecting: bool,
    pub queue: Vec<QueuedPrompt>,
    pub clarify: Option<PendingClarify>,
    pub last_error: Option<String>,
    /// The user asked to stop the turn and the backend has not ended it yet.
    pub stopping: bool,

    pub backend: Arc<AsyncMutex<Backend>>,
    pub caps: BackendCaps,
    pub cached: CachedState,
    /// Identifies the most recent operation that changed which conversation is
    /// on screen. Async history/resume/create results captured under an older
    /// value must not mutate the new conversation.
    pub view_generation: u64,
    pub live_id: Option<String>,
    pub stored_id: Option<String>,

    // Hermes model + permission state, read from Hermes' own config files.
    pub models: Vec<ModelOption>,
    pub current_provider: String,
    pub current_model: String,
    pub switching_model: bool,
    pub context_window: i64,
    pub permission_mode: PermissionMode,
    pub changing_permission: bool,

    /// Draft instructions for the composer, drained by the next poll.
    pub composer: Vec<ComposerAction>,
    /// Ids of messages whose text streamed further since the last poll.
    pub streamed: Vec<String>,
    /// Bumped by every cache-save request; a delayed save only runs if no newer
    /// one was asked for in the meantime.
    pub cache_generation: u64,
    /// Bumped by every message-list change, so the Go side can tell a transcript
    /// it already drew from a new one.
    pub revision: u64,
    /// Identifies the event stream whose events may change this state. Bumped
    /// whenever a stream is opened, the backend is switched or the user
    /// disconnects: a stream that is being replaced still delivers its last
    /// `Disconnected`, and that must not mark the new connection down.
    pub connection_epoch: u64,
    /// Bumped by every connect attempt, disconnect and backend switch, so a
    /// connect or a retry that was overtaken does not apply its result.
    pub connect_generation: u64,
    /// How many reconnects in a row have failed; picks the next backoff.
    pub retry_attempt: usize,
    /// A reconnect is scheduled after the connection dropped on its own.
    pub reconnecting: bool,
    /// Bumped by every turn this client starts, so a deferred "stop the
    /// spinner" after an interrupt only ends the turn it was meant for.
    pub turn_seq: u64,
    pub dirty: Dirty,
}

impl State {
    pub fn kind(&self) -> BackendKind {
        self.settings.backend_kind
    }

    pub fn backend_id(&self) -> &'static str {
        self.settings.backend_kind.id()
    }

    pub fn messages(&self) -> &[ChatMessage] {
        self.conversation.messages()
    }

    pub fn is_sending(&self) -> bool {
        self.conversation.is_sending()
    }

    /// Invalidate every async result belonging to the conversation previously
    /// on screen and return the token for the new one.
    pub fn advance_view(&mut self) -> u64 {
        self.view_generation = self.view_generation.wrapping_add(1);
        self.view_generation
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.selected.as_ref().map(|session| session.id.as_str())
    }

    pub fn view_is_current(&self, generation: u64, expected: Option<&str>) -> bool {
        view_is_current(
            generation,
            self.view_generation,
            self.selected_id(),
            expected,
        )
    }

    pub fn changed(&mut self) {
        self.dirty.state = true;
    }

    pub fn messages_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.dirty.messages = true;
        self.dirty.state = true;
    }

    pub fn settings_changed(&mut self) {
        self.dirty.settings = true;
        self.dirty.state = true;
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.last_error = Some(message.into());
        self.changed();
    }

    /// How full the context of the session on screen is: the backend's own
    /// reading when it reported both halves of one, else an estimate that is
    /// marked as such.
    pub fn context_usage(&self) -> ContextUsage {
        reported_context_usage(self.selected.as_ref(), &self.sessions).unwrap_or_else(|| {
            ContextUsage::estimated(estimated_tokens(self.messages()), self.context_window)
        })
    }

    pub fn filter_visible(&self, sessions: Vec<AgentSession>) -> Vec<AgentSession> {
        visible_sessions(&sessions, self.backend_id(), &self.cached)
    }
}

pub fn cache_key(backend_id: &str, session_id: &str) -> String {
    format!("{backend_id}::{session_id}")
}

pub fn view_is_current(
    operation: u64,
    current: u64,
    selected: Option<&str>,
    expected: Option<&str>,
) -> bool {
    operation == current && selected == expected
}

/// Whether an event that belongs to a turn may be folded into the messages on
/// screen.
///
/// A connection is subscribed to one session at a time, and a gateway carries
/// every session down it:
///
/// * **A live session** — the events are its own.
/// * **No session at all** — a chat that has not been created yet has nothing to
///   mismatch, so its first turn must not be held off.
/// * **A session being opened** — the stored id is known but the subscription is
///   not confirmed, which is exactly the window where the previous session's
///   traffic arrives. Held off until the gateway answers.
pub fn accepts_turn_events(live: Option<&str>, stored: Option<&str>) -> bool {
    live.is_some() || stored.is_none()
}

/// A user refresh means "make this view current", including restoring a dead
/// transport. Backend-originated quiet refreshes only update session metadata.
pub fn refresh_needs_reconnect(show_progress: bool, transport_ready: bool) -> bool {
    show_progress && !transport_ready
}

pub fn visible_sessions(
    sessions: &[AgentSession],
    backend_id: &str,
    cached: &CachedState,
) -> Vec<AgentSession> {
    sessions
        .iter()
        .filter(|session| {
            let owner = session.backend_id.as_deref().unwrap_or("hermes");
            let key = cache_key(backend_id, &session.id);
            owner == backend_id
                && session.archived != Some(true)
                && !cached.archived_session_ids.contains(&key)
                && !cached.deleted_session_ids.contains(&key)
        })
        .cloned()
        .collect()
}

/// The messages worth drawing from a stored or fetched transcript.
pub fn visible_messages(messages: &[ChatMessage], now: f64) -> Vec<ChatMessage> {
    messages
        .iter()
        .filter_map(|message| {
            if message.role == MessageRole::Tool || looks_like_tool_payload(&message.content) {
                return None;
            }
            let mut cleaned = message.clone();
            let finished_at = cleaned
                .tool_calls
                .last()
                .map(|call| call.timestamp)
                .unwrap_or(cleaned.timestamp);
            // A message cached mid-stream by a run that never saw the end of
            // the turn would otherwise spin forever.
            if cleaned.is_streaming && now - cleaned.timestamp > 300.0 {
                cleaned.is_streaming = false;
                cleaned.completed_at = Some(finished_at);
            }
            if cleaned.is_empty_shell() {
                return None;
            }
            if !cleaned.is_streaming
                && cleaned.completed_at.is_none()
                && (!cleaned.tool_calls.is_empty() || cleaned.content.is_empty())
            {
                cleaned.completed_at = Some(finished_at);
            }
            Some(cleaned)
        })
        .collect()
}

fn looks_like_tool_payload(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("{\"output\":")
        || trimmed.starts_with("{\"error\":")
        || trimmed.starts_with("{\"exit_code\":")
}

/// The context accounting the backend reported for the session on screen, if it
/// reported both halves of it. The listed row wins over the copy the session was
/// opened with: a refresh after a turn replaces the rows, not the selection.
pub fn reported_context_usage(
    selected: Option<&AgentSession>,
    sessions: &[AgentSession],
) -> Option<ContextUsage> {
    let selected = selected?;
    let row = sessions
        .iter()
        .find(|session| session.id == selected.id)
        .unwrap_or(selected);
    let used = row.used_tokens?;
    let max = row.context_tokens.filter(|max| *max > 0)?;
    Some(ContextUsage::measured(used, max))
}

fn estimated_tokens(messages: &[ChatMessage]) -> i64 {
    const FIXED_PROMPT_TOKENS: i64 = 17_400;
    let chars: usize = messages
        .iter()
        .map(|message| {
            message.content.len()
                + message
                    .attachments
                    .iter()
                    .map(|a| a.path.len() + 8)
                    .sum::<usize>()
                + message
                    .tool_calls
                    .iter()
                    .map(|call| call.detail.len().min(2_000))
                    .sum::<usize>()
        })
        .sum();
    FIXED_PROMPT_TOKENS + (chars as i64) / 4
}

/// The prompt as the backend receives it: attachments are named as `@path`
/// references, which every supported agent resolves itself.
pub fn submitted_prompt(text: &str, attachments: &[ComposerAttachment]) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    let list = attachments
        .iter()
        .map(|attachment| format!("- @{}", attachment.path))
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        format!("Attached files:\n{list}")
    } else {
        format!("{text}\n\nAttached files:\n{list}")
    }
}

/// Attachments from paths, trimmed, without empties or duplicates.
pub fn attachments_from(paths: &[String]) -> Vec<ComposerAttachment> {
    let mut seen = HashSet::new();
    paths
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty() && seen.insert(path.to_string()))
        .map(ComposerAttachment::new)
        .collect()
}

/// Cut `text` to at most `max` bytes without splitting a character.
pub fn truncate_on_char(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Model groups for the picker: the current provider first, the rest
/// alphabetical; inside each, the current model first.
pub fn model_groups(
    models: &[ModelOption],
    current_provider: &str,
    current_model: &str,
) -> Vec<(String, Vec<ModelOption>)> {
    fn current_first(lhs: &str, rhs: &str, current: &str) -> std::cmp::Ordering {
        (lhs != current)
            .cmp(&(rhs != current))
            .then_with(|| lhs.to_lowercase().cmp(&rhs.to_lowercase()))
    }
    let mut providers: Vec<String> = Vec::new();
    for option in models {
        if !providers.contains(&option.provider) {
            providers.push(option.provider.clone());
        }
    }
    providers.sort_by(|a, b| current_first(a, b, current_provider));
    providers
        .into_iter()
        .map(|provider| {
            let mut group: Vec<ModelOption> = models
                .iter()
                .filter(|option| option.provider == provider)
                .cloned()
                .collect();
            group.sort_by(|a, b| current_first(&a.model, &b.model, current_model));
            (provider, group)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscribed_session_takes_its_own_traffic() {
        assert!(accepts_turn_events(Some("live-1"), Some("stored-1")));
        assert!(accepts_turn_events(Some("live-1"), None));
    }

    /// The window a switch opens: the id on screen is known, the subscription is
    /// not established yet, and anything arriving belongs to the session being
    /// left.
    #[test]
    fn a_session_being_opened_holds_traffic_off() {
        assert!(!accepts_turn_events(None, Some("stored-2")));
    }

    /// A chat that does not exist yet has nothing to mismatch: holding its first
    /// turn off would leave a fresh chat that never answers.
    #[test]
    fn a_chat_with_no_session_yet_takes_its_first_turn() {
        assert!(accepts_turn_events(None, None));
    }

    #[test]
    fn an_async_session_result_only_mutates_the_view_that_started_it() {
        assert!(view_is_current(7, 7, Some("a"), Some("a")));
        assert!(!view_is_current(7, 8, Some("b"), Some("a")));
        assert!(!view_is_current(7, 7, None, Some("a")));
        assert!(view_is_current(9, 9, None, None));
    }

    #[test]
    fn only_an_explicit_refresh_restores_a_dead_transport() {
        assert!(refresh_needs_reconnect(true, false));
        assert!(!refresh_needs_reconnect(true, true));
        assert!(!refresh_needs_reconnect(false, false));
    }

    /// The GPUI client sliced tool detail at byte 3000, which panics on a
    /// multi-byte character — a CJK tool output was enough to crash it.
    #[test]
    fn truncation_never_splits_a_character() {
        let text = "工具输出".repeat(1000);
        let cut = truncate_on_char(&text, 3000);
        assert!(cut.len() <= 3000);
        assert!(text.starts_with(cut));
        assert_eq!(truncate_on_char("short", 3000), "short");
    }

    fn row(id: &str, used: Option<i64>, window: Option<i64>) -> AgentSession {
        AgentSession {
            id: id.to_string(),
            used_tokens: used,
            context_tokens: window,
            ..AgentSession::default()
        }
    }

    #[test]
    fn the_listed_row_wins_over_the_copy_the_session_was_opened_with() {
        let selected = row("a", Some(1), Some(100));
        let rows = vec![row("a", Some(9_000), Some(1_048_576))];
        let usage = reported_context_usage(Some(&selected), &rows).expect("reported");
        assert!(usage.measured);
        assert_eq!(usage.used_tokens, 9_000);
    }

    #[test]
    fn half_a_reading_is_not_a_reading() {
        let selected = row("a", None, None);
        assert!(reported_context_usage(Some(&selected), &[]).is_none());
        assert!(reported_context_usage(Some(&selected), &[row("a", Some(5), None)]).is_none());
        assert!(reported_context_usage(Some(&selected), &[row("a", Some(5), Some(0))]).is_none());
        assert!(reported_context_usage(None, &[row("a", Some(1), Some(2))]).is_none());
    }

    #[test]
    fn archived_deleted_and_foreign_sessions_are_not_listed() {
        let mut cached = CachedState::default();
        cached
            .archived_session_ids
            .insert(cache_key("openclaw", "b"));
        cached
            .deleted_session_ids
            .insert(cache_key("openclaw", "c"));
        let owned = |id: &str, backend: &str| AgentSession {
            id: id.into(),
            backend_id: Some(backend.into()),
            ..AgentSession::default()
        };
        let listed = visible_sessions(
            &[
                owned("a", "openclaw"),
                owned("b", "openclaw"),
                owned("c", "openclaw"),
                owned("d", "hermes"),
            ],
            "openclaw",
            &cached,
        );
        let ids: Vec<&str> = listed.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);
    }

    #[test]
    fn stale_streaming_and_tool_payloads_are_cleaned_up() {
        let mut stale = ChatMessage::streaming(MessageRole::Assistant);
        stale.content = "half a reply".into();
        stale.timestamp = 1_000.0;
        let payload = ChatMessage::new(MessageRole::Assistant, "{\"output\": \"x\"}");
        let tool = ChatMessage::new(MessageRole::Tool, "ran");
        let visible = visible_messages(&[stale, payload, tool], 2_000.0);
        assert_eq!(visible.len(), 1);
        assert!(!visible[0].is_streaming);
        assert_eq!(visible[0].completed_at, Some(1_000.0));
    }

    #[test]
    fn attachments_are_named_as_references() {
        let attachments = attachments_from(&["/a/b.rs".into(), " /a/b.rs ".into(), "".into()]);
        assert_eq!(attachments.len(), 1);
        assert_eq!(
            submitted_prompt("", &attachments),
            "Attached files:\n- @/a/b.rs"
        );
        assert_eq!(
            submitted_prompt("look", &attachments),
            "look\n\nAttached files:\n- @/a/b.rs"
        );
        assert_eq!(submitted_prompt("plain", &[]), "plain");
    }

    #[test]
    fn the_current_provider_and_model_come_first() {
        let option = |p: &str, m: &str| ModelOption {
            provider: p.into(),
            model: m.into(),
        };
        let groups = model_groups(
            &[
                option("b", "x"),
                option("a", "y"),
                option("b", "z"),
                option("c", "w"),
            ],
            "b",
            "z",
        );
        let providers: Vec<&str> = groups.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(providers, vec!["b", "a", "c"]);
        assert_eq!(groups[0].1[0].model, "z");
    }
}
