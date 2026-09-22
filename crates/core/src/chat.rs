//! Client-side chat state that every frontend needs, regardless of toolkit.
//!
//! Three things live here because they are subtle enough that a second frontend
//! must not reimplement them:
//!
//! * merging a reply that arrives as a stream of chunks, possibly on several
//!   streams at once ([`merge_stream_chunk`], [`best_stream_text`]);
//! * keeping the chat the user is looking at in the session list while the
//!   gateway still has not listed it ([`merge_fetched_sessions`]);
//! * folding the events of a turn into the message list a frontend draws
//!   ([`Conversation`]).

use crate::models::{now_unix, AgentEvent, AgentSession, ChatMessage, DeltaSource, MessageRole};
use std::collections::BTreeMap;

/// A session is unnamed while it still carries the placeholder the client uses
/// for chats it created itself.
pub fn needs_session_title(title: Option<&str>) -> bool {
    match title.map(str::trim) {
        None | Some("") => true,
        Some(title) => title == "New Chat",
    }
}

/// Sessions as the gateway reports them, plus the one the user is looking at if
/// the gateway has not listed it yet (a chat created here has no messages to
/// report until its first turn ends).
pub fn merge_fetched_sessions(
    fetched: Vec<AgentSession>,
    selected: Option<&AgentSession>,
) -> Vec<AgentSession> {
    let mut merged = fetched;
    if let Some(selected) = selected {
        if !merged.iter().any(|session| session.id == selected.id) {
            merged.insert(0, selected.clone());
        }
    }
    merged
}

/// Append a chunk to one stream's buffer. Gateways mix incremental deltas,
/// cumulative snapshots, replayed overlaps and plain repeats on the same stream,
/// so a chunk that already covers (or is covered by) the buffer is merged
/// instead of concatenated.
pub fn merge_stream_chunk(buffer: &mut String, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    if buffer.is_empty() || chunk.starts_with(buffer.as_str()) {
        // A snapshot of everything this stream has produced so far.
        buffer.clear();
        buffer.push_str(chunk);
        return;
    }
    if buffer.ends_with(chunk) {
        // The same chunk delivered twice.
        return;
    }
    // A replayed or re-chunked chunk usually starts where the buffer ends
    // ("…五处落点" + "落点全部实时…"): keep the overlap once.
    buffer.push_str(&chunk[overlap_with_suffix(buffer, chunk)..]);
}

/// Length of the longest suffix of `buffer` that is also a prefix of `chunk`.
/// Overlaps shorter than `MIN_OVERLAP` are ignored: a single shared character is
/// far more likely to be a coincidence than a replay.
fn overlap_with_suffix(buffer: &str, chunk: &str) -> usize {
    const MIN_OVERLAP: usize = 4;
    const MAX_OVERLAP: usize = 512;
    let limit = buffer.len().min(chunk.len()).min(MAX_OVERLAP);
    let boundaries = chunk
        .char_indices()
        .map(|(index, character)| index + character.len_utf8())
        .rev();
    for end in boundaries {
        if end > limit {
            continue;
        }
        if buffer.ends_with(&chunk[..end]) {
            return if end >= MIN_OVERLAP { end } else { 0 };
        }
    }
    0
}

/// The most complete text seen for the turn, across all streams.
///
/// A gateway can deliver the *same* reply over more than one stream at once
/// (OpenClaw sends both a `session.message` transcript and an `agent`/assistant
/// event stream). Appending all of them into one buffer interleaves two copies
/// of the message, so chunks are buffered per stream and the most complete one
/// is what gets shown.
pub fn best_stream_text(streams: &BTreeMap<DeltaSource, String>) -> String {
    streams
        .values()
        .max_by_key(|text| text.chars().count())
        .cloned()
        .unwrap_or_default()
}

/// What folding one event did to the message list.
///
/// A frontend that draws the list itself wants to know whether only the text
/// being streamed moved (cheap to redraw) or something was added, removed or
/// gained a tool call (the whole list has to be re-read).
#[derive(Clone, Debug, PartialEq)]
pub enum ConversationChange {
    None,
    /// The text of one streaming message moved.
    Streaming {
        id: String,
        text: String,
    },
    /// Messages were added, removed, completed or gained a tool call.
    Structure,
}

/// The messages of one chat, folded from the events of a turn.
///
/// The rules are not obvious — when a placeholder appears, which tool event is
/// a repeat of one already shown, when a reply stops streaming — and a frontend
/// that guesses them ends up with the failure modes the mobile client had: one
/// "Thinking…" bubble per event, a spinner that never stops because the turn
/// failed, and the same tool call listed once per status update. They are ported
/// from the desktop `AppState` and live here so both frontends share them.
#[derive(Default)]
pub struct Conversation {
    messages: Vec<ChatMessage>,
    streams: BTreeMap<DeltaSource, String>,
    /// Whether a prompt has been submitted and its turn has not finished. Only
    /// used to decide whether an empty placeholder is worth creating: an event
    /// that arrives with no turn in flight is still drawn, it just does not get
    /// a bubble of its own to live in.
    sending: bool,
}

impl Conversation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// The messages on screen, mutable. Rarely wanted: the fold owns the list
    /// during a turn. A test or a repair that must reach into an existing
    /// message uses this instead of the frontend keeping its own copy.
    pub fn messages_mut(&mut self) -> &mut Vec<ChatMessage> {
        &mut self.messages
    }

    pub fn is_sending(&self) -> bool {
        self.sending
    }

    /// A prompt was submitted: the next assistant message gets a placeholder
    /// straight away, so the user sees the turn start before any text arrives.
    ///
    /// The prompt itself is recorded here rather than left to the frontend to
    /// draw, because the message list is what a transcript reload replaces: a
    /// message the client only drew locally would vanish on the next one.
    pub fn begin_turn_with(&mut self, prompt: &str) {
        self.messages
            .push(ChatMessage::new(MessageRole::User, prompt));
        self.sending = true;
        self.streams.clear();
    }

    /// Like [`begin_turn_with`], plus the assistant placeholder right away.
    ///
    /// The desktop sends this way: on a slow handshake the placeholder arriving
    /// with `MessageStart` would leave the prompt sitting alone in the chat for
    /// the whole round trip, and the composer would look like it did nothing.
    /// The backend's own `MessageStart` finds the placeholder already there and
    /// adds nothing (see `start_message`).
    pub fn begin_turn_with_placeholder(&mut self, prompt: &str) {
        self.begin_turn_with(prompt);
        self.messages
            .push(ChatMessage::streaming(MessageRole::Assistant));
    }

    pub fn begin_turn(&mut self) {
        self.sending = true;
        self.streams.clear();
    }

    /// Replace the transcript with what the backend reports.
    pub fn set_transcript(&mut self, messages: Vec<ChatMessage>) {
        self.messages = messages;
        self.streams.clear();
    }

    /// Replace the transcript, keeping tool activity this client watched happen.
    ///
    /// `chat.history` reports a turn as one block of text and carries no tool
    /// calls, so a session reopened in the app would otherwise lose them. The two
    /// lists do not line up message for message: the client draws a turn's tool
    /// calls as a bubble of their own, while the backend reports the turn as the
    /// text that followed them. So each reported message is matched to the local
    /// message with the same role and text, and collects the tool calls of every
    /// assistant message the client drew between them.
    pub fn merge_transcript(&mut self, messages: Vec<ChatMessage>) {
        let local = std::mem::take(&mut self.messages);
        let mut cursor = 0usize;
        let mut merged = Vec::with_capacity(messages.len());

        for mut message in messages {
            let matched = local[cursor..]
                .iter()
                .position(|candidate| {
                    candidate.role == message.role
                        && candidate.content.trim() == message.content.trim()
                })
                .map(|offset| cursor + offset);

            if let Some(index) = matched {
                if message.role == MessageRole::Assistant {
                    for candidate in &local[cursor..=index] {
                        for call in &candidate.tool_calls {
                            if !message.tool_calls.iter().any(|kept| kept.id == call.id) {
                                message.tool_calls.push(call.clone());
                            }
                        }
                    }
                }
                cursor = index + 1;
            }
            merged.push(message);
        }

        self.set_transcript(merged);
    }

    /// End the turn: nothing is streaming any more and the empty shells some
    /// events leave behind are removed.
    pub fn finish_turn(&mut self) {
        let completed_at = now_unix();
        for message in self.messages.iter_mut() {
            if message.role == MessageRole::Assistant && message.is_streaming {
                message.is_streaming = false;
                message.completed_at = Some(completed_at);
            }
        }
        self.prune_empty_assistant_messages();
        self.streams.clear();
        self.sending = false;
    }

    pub fn apply(&mut self, event: &AgentEvent) -> ConversationChange {
        match event {
            AgentEvent::MessageStart => self.start_message(),
            AgentEvent::MessageDelta { text, source } => self.apply_delta(text, *source),
            AgentEvent::MessageComplete(text) => self.complete_message(text.as_deref()),
            AgentEvent::Tool(record) => self.append_tool_call(record),
            AgentEvent::TurnFailed(_) => {
                self.finish_turn();
                ConversationChange::Structure
            }
            _ => ConversationChange::None,
        }
    }

    fn start_message(&mut self) -> ConversationChange {
        // A turn that opens a tool call and then a second message would
        // otherwise leave one placeholder spinning for every event.
        let needs_placeholder = self.sending
            && self
                .messages
                .last()
                .map(|last| last.role != MessageRole::Assistant || !last.is_streaming)
                .unwrap_or(true);
        if !needs_placeholder {
            return ConversationChange::None;
        }
        self.messages
            .push(ChatMessage::streaming(MessageRole::Assistant));
        ConversationChange::Structure
    }

    fn apply_delta(&mut self, text: &str, source: DeltaSource) -> ConversationChange {
        if text.is_empty() {
            return ConversationChange::None;
        }
        merge_stream_chunk(self.streams.entry(source).or_default(), text);
        let streamed = best_stream_text(&self.streams);
        if streamed.is_empty() {
            return ConversationChange::None;
        }
        // The reply belongs to the message that is streaming *text*: a message
        // holding tool calls is a separate bubble and must not swallow it.
        let index = match self.messages.iter().rposition(|message| {
            message.role == MessageRole::Assistant
                && message.is_streaming
                && message.tool_calls.is_empty()
        }) {
            Some(index) => {
                self.messages[index].content = streamed.clone();
                index
            }
            None => {
                let mut message = ChatMessage::streaming(MessageRole::Assistant);
                message.content = streamed.clone();
                self.messages.push(message);
                self.messages.len() - 1
            }
        };
        ConversationChange::Streaming {
            id: self.messages[index].id.clone(),
            text: streamed,
        }
    }

    /// The turn's reply is complete. The mobile bridge reaches this through
    /// [`Self::apply`]; the desktop calls it directly for a turn the backend
    /// reported as failed, with the error as the final text.
    pub fn complete_message(&mut self, text: Option<&str>) -> ConversationChange {
        let final_text = text
            .map(str::to_string)
            .unwrap_or_else(|| best_stream_text(&self.streams));
        let final_text = final_text.trim().to_string();

        let active = self
            .messages
            .iter()
            .rposition(|message| message.role == MessageRole::Assistant && message.is_streaming);
        let completed_at = now_unix();
        for message in self.messages.iter_mut() {
            if message.role == MessageRole::Assistant && message.is_streaming {
                message.is_streaming = false;
                message.completed_at = Some(completed_at);
            }
        }

        match active {
            Some(index) => {
                // An empty final text must not wipe what already streamed in.
                if !final_text.is_empty() {
                    if self.messages[index].tool_calls.is_empty() {
                        self.messages[index].content = final_text;
                    } else {
                        self.messages
                            .push(ChatMessage::new(MessageRole::Assistant, final_text));
                    }
                }
            }
            None if !final_text.is_empty() => {
                self.messages
                    .push(ChatMessage::new(MessageRole::Assistant, final_text));
            }
            None => {}
        }

        self.finish_turn();
        ConversationChange::Structure
    }

    fn append_tool_call(&mut self, record: &crate::models::ToolCallRecord) -> ConversationChange {
        // Deduplicate within the current turn: a gateway reports a tool once when
        // it starts and again when it finishes, and re-sends the same status on
        // reconnect. Repeats are dropped, a changed status is kept.
        let turn_start = self
            .messages
            .iter()
            .rposition(|message| message.role == MessageRole::User)
            .map(|index| index + 1)
            .unwrap_or(0);
        let duplicate = self.messages[turn_start..].iter().any(|message| {
            message.tool_calls.iter().any(|call| {
                call.name == record.name
                    && call.status == record.status
                    && call.detail == record.detail
            })
        });
        if duplicate {
            return ConversationChange::None;
        }

        // Tool activity joins the streaming bubble that has no text yet, so a
        // run of calls reads as one block instead of one bubble per call.
        let target = self.messages.iter().rposition(|message| {
            message.role == MessageRole::Assistant
                && message.is_streaming
                && message.content.trim().is_empty()
        });
        match target {
            Some(index) => self.messages[index].tool_calls.push(record.clone()),
            None => {
                let mut message = ChatMessage::streaming(MessageRole::Assistant);
                message.tool_calls.push(record.clone());
                self.messages.push(message);
            }
        }
        ConversationChange::Structure
    }

    /// Drop assistant bubbles that hold nothing: they are placeholders that no
    /// text and no tool call ever arrived for.
    fn prune_empty_assistant_messages(&mut self) {
        self.messages.retain(|message| !message.is_empty_shell());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unnamed_sessions_are_recognised() {
        assert!(needs_session_title(None));
        assert!(needs_session_title(Some("")));
        assert!(needs_session_title(Some("  ")));
        assert!(needs_session_title(Some("New Chat")));
        assert!(!needs_session_title(Some("Van-Goal")));
    }

    /// The desktop sends with the placeholder already in place, so a slow
    /// handshake leaves something moving on screen. The backend's own
    /// `MessageStart` must not add a second placeholder on top of it.
    #[test]
    fn sending_with_a_placeholder_shows_one_bubble_per_turn() {
        let mut conversation = Conversation::new();
        conversation.begin_turn_with_placeholder("第一句");
        assert_eq!(conversation.messages().len(), 2);
        assert_eq!(conversation.messages()[0].role, MessageRole::User);
        assert!(conversation.messages()[1].is_streaming);

        assert_eq!(
            conversation.apply(&AgentEvent::MessageStart),
            ConversationChange::None,
            "the placeholder is already on screen"
        );
        assert_eq!(conversation.messages().len(), 2);
    }

    #[test]
    fn the_open_chat_survives_a_session_list_refresh() {
        let mut selected = AgentSession::default();
        selected.id = "agent:main:van-goal:brand-new".into();
        selected.title = Some("New Chat".into());

        let listed = AgentSession {
            id: "agent:main:van-goal:older".into(),
            title: Some("Van-Goal".into()),
            ..Default::default()
        };

        // The gateway has not listed the brand new chat yet: it must not vanish
        // from the sidebar while the user is looking at it.
        let merged = merge_fetched_sessions(vec![listed.clone()], Some(&selected));
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, selected.id);
        assert!(merged.iter().any(|session| session.id == listed.id));

        // Once the gateway lists it, the fetched (named) copy wins.
        let named = AgentSession {
            id: selected.id.clone(),
            title: Some("Van-Goal".into()),
            ..Default::default()
        };
        let merged = merge_fetched_sessions(vec![named], Some(&selected));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].title.as_deref(), Some("Van-Goal"));
    }

    #[test]
    fn incremental_chunks_accumulate() {
        let mut buffer = String::new();
        for chunk in ["科", "研", "模式已进入"] {
            merge_stream_chunk(&mut buffer, chunk);
        }
        assert_eq!(buffer, "科研模式已进入");
    }

    #[test]
    fn cumulative_snapshots_replace_instead_of_duplicating() {
        let mut buffer = String::new();
        merge_stream_chunk(&mut buffer, "科研");
        merge_stream_chunk(&mut buffer, "科研模式");
        merge_stream_chunk(&mut buffer, "科研模式已进入");
        assert_eq!(buffer, "科研模式已进入");
    }

    #[test]
    fn a_repeated_chunk_is_ignored() {
        let mut buffer = String::new();
        merge_stream_chunk(&mut buffer, "ab");
        merge_stream_chunk(&mut buffer, "ab");
        assert_eq!(buffer, "ab");
    }

    fn chunked(text: &str, size: usize) -> Vec<String> {
        let characters: Vec<char> = text.chars().collect();
        characters
            .chunks(size)
            .map(|chunk| chunk.iter().collect())
            .collect()
    }

    /// The OpenClaw regression: one reply is delivered by two streams at the same
    /// time with different chunk boundaries. Appending them into one buffer
    /// interleaved two copies of the text and broke the markdown tables inside
    /// it, because the separator row ended up split across the two copies.
    #[test]
    fn two_streams_of_one_reply_do_not_interleave() {
        let text = "刚把五处落点全部实时拉了一遍，当前状态如下：\n\n\
                    | 项目 | 进度 | 进行中 |\n|---|---|---|\n\
                    | **nano-AgFe-抗氧化** | 6/9 | 测试ROS检测手段 |\n\
                    | PEEK亲水改性 | 4/4 ✅ | — |\n";
        let first = chunked(text, 3);
        let second = chunked(text, 7);
        let mut streams: BTreeMap<DeltaSource, String> = BTreeMap::new();
        for index in 0..first.len().max(second.len()) {
            if let Some(chunk) = first.get(index) {
                merge_stream_chunk(streams.entry(DeltaSource::Transcript).or_default(), chunk);
            }
            if let Some(chunk) = second.get(index) {
                merge_stream_chunk(streams.entry(DeltaSource::AgentStream).or_default(), chunk);
            }
        }

        // What the old code did: every stream appended into one buffer.
        let mut naive = String::new();
        for index in 0..first.len().max(second.len()) {
            if let Some(chunk) = first.get(index) {
                naive.push_str(chunk);
            }
            if let Some(chunk) = second.get(index) {
                naive.push_str(chunk);
            }
        }
        assert_ne!(naive, text, "the naive merge should reproduce the garbling");
        assert!(
            !crate::markdown::parse(&naive)
                .iter()
                .any(|block| matches!(block, crate::markdown::MarkdownBlock::Table(..))),
            "the naive merge is what stopped tables from parsing"
        );

        let shown = best_stream_text(&streams);
        assert_eq!(shown, text, "interleaved streams garbled the reply");
        assert!(
            shown.contains("|---|---|---|"),
            "the table separator was split"
        );
        // The parser turns that separator into a real table.
        let tables = crate::markdown::parse(&shown)
            .into_iter()
            .filter(|block| matches!(block, crate::markdown::MarkdownBlock::Table(..)))
            .count();
        assert_eq!(tables, 1, "table was not recognised after merging streams");
    }

    #[test]
    fn replayed_overlap_is_merged_once() {
        let mut buffer = String::new();
        merge_stream_chunk(&mut buffer, "刚把五处落点");
        merge_stream_chunk(&mut buffer, "落点全部实时拉了一遍");
        assert_eq!(buffer, "刚把五处落点全部实时拉了一遍");
    }

    #[test]
    fn a_one_character_coincidence_is_not_treated_as_an_overlap() {
        let mut buffer = String::new();
        merge_stream_chunk(&mut buffer, "好的");
        merge_stream_chunk(&mut buffer, "的的确");
        assert_eq!(buffer, "好的的的确");
    }

    #[test]
    fn normal_chunks_are_not_trimmed() {
        let mut buffer = String::new();
        for chunk in ["项目", "总览：", "nano-AgFe", "-抗氧化"] {
            merge_stream_chunk(&mut buffer, chunk);
        }
        assert_eq!(buffer, "项目总览：nano-AgFe-抗氧化");
    }

    // ------------------------------------------------------------ conversation

    fn tool(name: &str, status: &str) -> AgentEvent {
        AgentEvent::Tool(crate::models::ToolCallRecord::new(name, status, ""))
    }

    /// Adapters put the call's own JSON in `detail`, so two calls to the same
    /// tool are told apart by it.
    fn tool_on(name: &str, status: &str, target: &str) -> AgentEvent {
        AgentEvent::Tool(crate::models::ToolCallRecord::new(
            name,
            status,
            format!(r#"{{"path":"{target}"}}"#),
        ))
    }

    fn delta(text: &str) -> AgentEvent {
        AgentEvent::MessageDelta {
            text: text.to_string(),
            source: DeltaSource::AgentStream,
        }
    }

    fn user_message(text: &str) -> ChatMessage {
        ChatMessage::new(MessageRole::User, text)
    }

    /// A turn opens a placeholder, streams text and finishes. One bubble, and it
    /// is not left streaming.
    #[test]
    fn a_plain_turn_leaves_one_finished_message() {
        let mut conversation = Conversation::new();
        conversation.set_transcript(vec![user_message("你好")]);
        conversation.begin_turn();

        conversation.apply(&AgentEvent::MessageStart);
        conversation.apply(&delta("你好呀"));
        conversation.apply(&AgentEvent::MessageComplete(Some("你好呀".into())));

        let messages = conversation.messages();
        assert_eq!(messages.len(), 2, "{messages:#?}");
        assert_eq!(messages[1].content, "你好呀");
        assert!(
            !messages[1].is_streaming,
            "the reply never stopped streaming"
        );
        assert!(!conversation.is_sending());
    }

    /// MiMoCode ends a turn by reporting the session idle, and a `mimo serve`
    /// turn was measured reporting it twice. The second report must not leave a
    /// second bubble behind, or every mimo reply would come with an empty one.
    #[test]
    fn a_turn_that_reports_its_end_twice_leaves_one_reply() {
        let mut conversation = Conversation::new();
        // The desktop's own shape: the prompt and its placeholder are added
        // together, and the backend's events arrive afterwards.
        conversation.begin_turn_with_placeholder("说点什么");

        // The exact sequence measured from `mimo serve` 0.1.8: the assistant
        // message opens, one part carries the whole text so far, then idle.
        conversation.apply(&AgentEvent::MessageStart);
        conversation.apply(&delta("STUB-OK"));
        conversation.apply(&AgentEvent::MessageComplete(None));
        conversation.apply(&AgentEvent::MessageComplete(None));

        let messages = conversation.messages();
        assert_eq!(messages.len(), 2, "a second bubble appeared: {messages:#?}");
        assert_eq!(messages[1].content, "STUB-OK");
        assert!(!messages[1].is_streaming);
        assert!(!conversation.is_sending());
    }

    /// A failed mimo turn is followed by the same idle pair. The desktop sends
    /// the failure down the completion path so it lands in the transcript, and
    /// the report that follows it must not add a second bubble or leave the
    /// composer blocked — the failure a user is most likely to meet here is
    /// "unknown certificate verification error", which says nothing about
    /// whether the turn ended.
    #[test]
    fn a_failed_turn_tells_the_user_once_and_ends_the_turn() {
        let mut conversation = Conversation::new();
        // The desktop's own shape: the prompt and its placeholder are added
        // together, and the backend's events arrive afterwards.
        conversation.begin_turn_with_placeholder("说点什么");

        conversation.apply(&AgentEvent::MessageStart);
        conversation.complete_message(Some(
            "Error: unknown certificate verification error",
        ));
        conversation.apply(&AgentEvent::MessageComplete(None));
        conversation.apply(&AgentEvent::MessageComplete(None));

        let messages = conversation.messages();
        assert_eq!(messages.len(), 2, "{messages:#?}");
        assert_eq!(messages[1].role, MessageRole::Assistant);
        assert!(
            messages[1]
                .content
                .contains("unknown certificate verification error"),
            "{messages:#?}"
        );
        assert!(
            !conversation.is_sending(),
            "the turn is over, so the composer must not stay blocked"
        );
        assert!(
            messages.iter().all(|message| !message.is_streaming),
            "a spinner was left running: {messages:#?}"
        );
    }

    /// The regression the mobile client was rebuilt for: a turn that runs four
    /// tools used to draw four bubbles and four "Thinking…" spinners, because
    /// every event started a placeholder of its own.
    #[test]
    fn a_turn_with_tool_calls_draws_one_bubble_for_the_calls() {
        let mut conversation = Conversation::new();
        conversation.set_transcript(vec![user_message("看看目录")]);
        conversation.begin_turn();

        for (name, target) in [("read", "a.rs"), ("bash", "-"), ("read", "b.rs")] {
            conversation.apply(&AgentEvent::MessageStart);
            conversation.apply(&tool_on(name, "running", target));
            conversation.apply(&tool_on(name, "ok", target));
        }
        conversation.apply(&AgentEvent::MessageStart);
        conversation.apply(&delta("目录里有三个文件。"));
        conversation.apply(&AgentEvent::MessageComplete(None));

        let messages = conversation.messages();
        assert_eq!(messages.len(), 3, "{messages:#?}");
        // Three calls, each reported once when it started and once when it
        // finished: six records, in one bubble.
        assert_eq!(messages[1].tool_calls.len(), 6, "tool calls were split up");
        assert!(messages[1].content.is_empty());
        assert_eq!(messages[2].content, "目录里有三个文件。");
        assert!(
            messages.iter().all(|message| !message.is_streaming),
            "a spinner was left running: {messages:#?}"
        );
    }

    /// A gateway reports a tool when it starts and again when it finishes. The
    /// status change is worth showing; the identical repeat is not.
    #[test]
    fn a_repeated_tool_event_is_dropped_but_a_status_change_is_kept() {
        let mut conversation = Conversation::new();
        conversation.set_transcript(vec![user_message("看看目录")]);
        conversation.begin_turn();

        conversation.apply(&tool("read", "running"));
        conversation.apply(&tool("read", "running"));
        conversation.apply(&tool("read", "ok"));

        let calls = &conversation.messages()[1].tool_calls;
        assert_eq!(calls.len(), 2, "{calls:#?}");
        assert_eq!(calls[0].status, "running");
        assert_eq!(calls[1].status, "ok");
    }

    /// A failed turn used to leave the last bubble streaming forever, which is
    /// the spinner that never stopped.
    #[test]
    fn a_failed_turn_stops_the_spinner() {
        let mut conversation = Conversation::new();
        conversation.set_transcript(vec![user_message("你好")]);
        conversation.begin_turn();

        conversation.apply(&AgentEvent::MessageStart);
        conversation.apply(&AgentEvent::TurnFailed("gateway closed".into()));

        assert!(
            conversation
                .messages()
                .iter()
                .all(|message| !message.is_streaming),
            "a failed turn left a message streaming"
        );
        assert!(!conversation.is_sending());
    }

    /// The final text of a turn arrives with the completion event, and a turn
    /// that produced tool calls reports it separately from the calls.
    #[test]
    fn a_completion_without_text_keeps_what_streamed() {
        let mut conversation = Conversation::new();
        conversation.set_transcript(vec![user_message("你好")]);
        conversation.begin_turn();

        conversation.apply(&delta("已经流出来的正文"));
        conversation.apply(&AgentEvent::MessageComplete(None));

        assert_eq!(conversation.messages()[1].content, "已经流出来的正文");
    }

    /// `chat.history` carries no tool calls, so reopening a session must not
    /// throw away the ones the client watched happen.
    #[test]
    fn reopening_a_session_keeps_the_tool_calls_the_client_saw() {
        let mut conversation = Conversation::new();
        conversation.set_transcript(vec![user_message("看看目录")]);
        conversation.begin_turn();
        conversation.apply(&tool("read", "ok"));
        conversation.apply(&delta("目录里有三个文件。"));
        conversation.apply(&AgentEvent::MessageComplete(None));

        // What the backend reports for the same turn: text only.
        let fetched = vec![
            user_message("看看目录"),
            ChatMessage::new(MessageRole::Assistant, "目录里有三个文件。"),
        ];
        conversation.merge_transcript(fetched);

        let messages = conversation.messages();
        assert_eq!(messages.len(), 2, "{messages:#?}");
        assert_eq!(
            messages[1].tool_calls.len(),
            1,
            "the tool call was dropped when the session was re-read"
        );
    }

    /// A session the client has never seen loads as the backend reports it.
    #[test]
    fn a_transcript_the_client_never_saw_loads_unchanged() {
        let mut conversation = Conversation::new();
        conversation.merge_transcript(vec![
            user_message("你好"),
            ChatMessage::new(MessageRole::Assistant, "你好呀"),
        ]);
        assert_eq!(conversation.messages().len(), 2);
        assert!(conversation.messages()[1].tool_calls.is_empty());
    }

    /// Only a turn in flight gets a placeholder: an event that arrives with no
    /// prompt outstanding must not push an empty bubble that never fills.
    #[test]
    fn no_placeholder_appears_without_a_turn_in_flight() {
        let mut conversation = Conversation::new();
        conversation.set_transcript(vec![user_message("你好")]);
        conversation.apply(&AgentEvent::MessageStart);
        assert_eq!(conversation.messages().len(), 1);

        // Text that arrives anyway is still drawn.
        conversation.apply(&delta("迟到的回复"));
        assert_eq!(conversation.messages().len(), 2);
        assert_eq!(conversation.messages()[1].content, "迟到的回复");
    }

    #[test]
    fn the_most_complete_stream_wins() {
        let mut streams: BTreeMap<DeltaSource, String> = BTreeMap::new();
        merge_stream_chunk(
            streams.entry(DeltaSource::Transcript).or_default(),
            "完整的一段话",
        );
        merge_stream_chunk(streams.entry(DeltaSource::AgentStream).or_default(), "完整");
        assert_eq!(best_stream_text(&streams), "完整的一段话");
    }
}
