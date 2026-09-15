# One connection, many sessions

Most of the backends Van-Goal talks to carry **more than one session down a
single connection**. A Gateway fans out every session it knows about; OpenCode's
`/event` stream is global by definition; a Hermes server labels each event with
the session it came from. The client opens one chat at a time.

That combination has a failure mode that does not look like a failure: another
session's reply is appended to whatever chat happens to be open, and the user
reads a cron job's output as if it were the answer to their question. It is how
the run of `NO_REPLY` messages appeared in the desktop client: a cron job in
another session, arriving on the same socket as the chat being typed.

There is one rule for this, in one place:

```rust
// crates/core/src/agent/session_scope.rs
pub fn belongs_to_session<'a>(
    subscribed: impl IntoIterator<Item = &'a str>,
    event_session: Option<&str>,
) -> bool
```

## The rule

1. **Nothing subscribed yet → keep everything.** There is nothing to compare
   against, and the event that announces the session is itself one of these.
2. **An unlabelled event → keep it.** A backend that labels the traffic it fans
   out may leave its own single-session stream unlabelled, so dropping these
   would lose the active session's own reply — the worse of the two failures.
3. **A labelled event → keep it only when it names the session on screen.**

`subscribed` holds *every* id the client knows the session by. Backends disagree
about whether their events name the live id or the stored key, so accepting only
one of them would look exactly like a session that never answers.

## Applying it

Every adapter whose events carry a session id must call the rule before it emits
anything into the event stream:

| Adapter | Where the session is named | Where the rule is applied |
| --- | --- | --- |
| `agent/openclaw.rs` | `sessionKey` / `key` / `sessionId` on the frame payload | `is_for_active_session`, called for `chat` / `session.message` / `session.tool` / `agent` frames |
| `agent/opencode.rs` | `sessionID` on the properties, the part or the info | the prologue of `handle_event_json`, before any event type is matched |
| `agent/hermes.rs` | `session_id` beside the event type | `handle_gateway_frame`, for everything except the session bookkeeping |

The bookkeeping (`session.info`, `sessions.changed`, …) is deliberately exempt:
one of those events is how the client learns which session it is on, so
filtering it would leave the filter with nothing to compare against.

An adapter that gains a session-labelled stream **must** call this rule. That is
the whole point of it living in one place: the rule is not re-derived per
backend, and a backend that forgets it has one function to look for.

## What the frontends add

Filtering at the adapter is not enough on its own, because the window between
"the user opened a session" and "the gateway confirmed the subscription" belongs
to neither session. During that window the adapter is still scoped to the
*previous* session — so its frames pass the filter legitimately.

Both clients therefore hold turn traffic off until a subscription is confirmed:

* **Desktop** — `accepts_turn_events` in `crates/desktop/src/state.rs`; the live
  session id is dropped when a switch starts (`resume_session`) and restored when
  the gateway answers.
* **Mobile** — `OpenSession::accepts_turn_events` in `crates/mobile/src/client.rs`;
  the same rule, expressed with the stored and live ids the bridge keeps.

The mobile bridge goes one step further, because a phone switches sessions often:
each session's conversation is parked while the user is elsewhere, and reopening
one merges what the client watched happen back onto what the backend reports
(`Conversation::merge_transcript`). That is about *keeping* messages, not about
deciding whose they are — the rule above decides that.
