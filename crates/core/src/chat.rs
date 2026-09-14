//! Client-side chat state that every frontend needs, regardless of toolkit.
//!
//! Two things live here because they are subtle enough that a second frontend
//! must not reimplement them:
//!
//! * merging a reply that arrives as a stream of chunks, possibly on several
//!   streams at once ([`merge_stream_chunk`], [`best_stream_text`]);
//! * keeping the chat the user is looking at in the session list while the
//!   gateway still has not listed it ([`merge_fetched_sessions`]).

use crate::models::{AgentSession, DeltaSource};
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
