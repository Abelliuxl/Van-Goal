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

There are two rules for this, in one place, and they answer two different
questions:

```rust
// crates/core/src/agent/session_scope.rs
pub fn belongs_to_session<'a>(subscribed, event_session) -> bool       // lenient
pub fn belongs_to_open_session<'a>(subscribed, event_session) -> bool  // strict
```

The lenient form keeps everything when nothing is subscribed yet, because there
is nothing to compare against and one of those events may be how the client
learns which session it is on. The strict form says a *labelled* frame belongs
to the session on screen or to nobody. **Turn traffic uses the strict form.**

## What the Gateway actually does

An earlier version of this document assumed `sessions.messages.subscribe` scoped
delivery, and that the lenient rule was therefore safe for turn traffic once the
subscription existed. Both assumptions are wrong, and
`cargo run -p van-goal-core --example gateway_probe -- wss://host:18789` is how
that was measured:

* A socket that had **subscribed to nothing** received turn frames for **two
  different sessions** inside thirty seconds. Subscribing does not filter
  delivery, and neither does unsubscribing: the Gateway pushes every session to
  every connected client.
* **Every** turn frame carried its session at `payload.sessionKey`, at the top
  of the payload — 0 unlabelled frames out of 77 captured. `chat` and `agent`
  both use it.
* Scoring one capture with both rules, for a client that was on no session:

  | session the client is on | lenient admits | strict admits |
  | --- | --- | --- |
  | none — the window after a reconnect | 32 / 32 | 0 / 32 |
  | the correct one | 32 / 32 | 32 / 32 |

So the client-side filter is the *only* thing standing between the user and
somebody else's conversation, and the lenient form applied to turn traffic is
not a safety net — it is the leak. "Nothing subscribed yet" is not a rare state:
it is where a client sits for the whole window after every reconnect, which on a
phone (a socket per screen-lock, per network change, per frozen process) is most
of the time.

## The rule

1. **A labelled frame → keep it only when it names the session on screen.** For
   turn traffic this holds even when nothing is on screen: a frame for a session
   the client is not in belongs to somebody else.
2. **An unlabelled frame → keep it.** A backend that labels the traffic it fans
   out may leave its own single-session stream unlabelled, so dropping these
   would lose the active session's own reply — the worse of the two failures. No
   unlabelled turn frame has been observed on a Gateway, so nothing is lost in
   practice by requiring the label to match.
3. **Bookkeeping is exempt.** `session.info`, `sessions.changed`, … carry the
   announcement of which session the client is on, so they are read with the
   lenient rule — or not filtered at all, as in OpenClaw, which emits none of
   them.

`subscribed` holds *every* id the client knows the session by. Backends disagree
about whether their events name the live id or the stored key, so accepting only
one of them would look exactly like a session that never answers.

## Applying it

| Adapter | Where the session is named | Rule |
| --- | --- | --- |
| `agent/openclaw.rs` | `sessionKey` / `key` on the frame payload, read by `payload_session_key` | strict, for `chat` / `session.message` / `session.tool` / `agent` |
| `agent/opencode.rs` | `sessionID` on the properties, the part or the info | strict, in the prologue of `handle_event_json` |
| `agent/hermes.rs` | `session_id` beside the event type | strict, in `handle_gateway_frame`, everything except the session bookkeeping |

An adapter that gains a session-labelled stream **must** call the rule. That is
the whole point of it living in one place: the rule is not re-derived per
backend, and a backend that forgets it has one function to look for.

## The other half: where a prompt goes

Filtering decides which replies are *drawn*. It does not decide which session a
prompt is *sent to*, and a mistake there looks the same to the user — they are
reading one conversation and the answer comes back in another.

Both clients used to treat "there is no live session" as "start a new chat". On
a connection that has just been re-established, or one whose `resume_session` is
still in flight, that is wrong: the session on screen exists on the backend, it
is only the live connection to it that is missing. Creating a session there sent
the prompt into a conversation the user had never opened, leaving the one they
were reading untouched. A Gateway's own journal shows it: two prompts three
seconds apart, `sessions.create` between them, and the two messages landing in
two different sessions.

The rule is now:

* a live id → send to it;
* no live id but a stored one → **resume it, then send**;
* neither → a genuine new chat, and the only case that creates a session.

That is `send_target` in `crates/mobile/src/client.rs`, and the same decision in
`AppState::send_composer`. There it is carried by a `HeldPrompt`, which records
the session the prompt belongs to: one held for a conversation is flushed into
that conversation and returned to the composer — not sent — when it cannot be
reached, while one held for a genuinely new chat is flushed once the session
exists.

## What the frontends add

Filtering at the adapter is not enough on its own, because the window between
"the user opened a session" and "the gateway confirmed the subscription" belongs
to neither session. During that window the adapter may still be scoped to the
*previous* session, so its frames pass the filter legitimately.

Both clients therefore hold turn traffic off until a subscription is confirmed:

* **Desktop** — `accepts_turn_events` in `crates/desktop/src/state.rs`; the live
  session id is dropped when a switch starts (`resume_session`) and restored when
  the gateway answers.
* **Mobile** — `OpenSession::accepts_turn_events` in `crates/mobile/src/client.rs`;
  the same rule, expressed with the stored and live ids the bridge keeps.

The mobile bridge goes one step further, because a phone switches sessions often:

* Each session's conversation is parked while the user is elsewhere, and
  reopening one merges what the client watched happen back onto what the backend
  reports (`Conversation::merge_transcript`). That is about *keeping* messages,
  not about deciding whose they are — the rules above decide that.
* The session on screen is **remembered** (`Settings::last_session`) and reopened
  on the next launch, showing its transcript *before* resuming it. An app that
  came up on an empty chat made the first message typed create yet another
  session, which is how one conversation's context goes missing. `new_session`
  forgets it, because a new chat is what the user asked for.

`open_on` in `crates/mobile/src/client.rs` is the one place that shows a session
and attaches to it, shared by "the user opened this session" and "bring back the
one that was open" — they are the same operation, and doing only half of it is
what leaves the screen and the connection disagreeing about which session is
current.

## Two frontends, one session list

Sessions are told apart by the **whole** key, never by a prefix, so a desktop
session and a phone session under the same namespace can never exchange
traffic — the filter compares full strings and there is no prefix matching
anywhere. But they still have to be *nameable*, and that is a separate thing:

* The Gateway titles a session after the client that created it, and it reads
  that name from the handshake's `displayName`, **not** from the key. Measured:
  a session created under `agent:main:van-goal:isolation-probe` by a client
  whose `displayName` was "Van-Goal probe" came back titled "Van-Goal probe".
  Changing only the key would therefore have changed nothing about the titles.
* With one shared name the list read "Van-Goal", "Van-Goal", "Van-Goal" — ten of
  them on the Gateway this was measured against. Picking the wrong entry looks
  exactly like a client that crossed two conversations, which is why
  `OPENCLAW_DISPLAY_NAME` and `OPENCLAW_SESSION_NAMESPACE` are per-frontend.

The remaining shared thing is the *device* identity (`darwin`/`desktop`), which
is still one set of compile-time constants: a phone reports itself as a desktop
in `openclaw devices list`. That one is part of the signed device payload, so
changing it would be an authentication change rather than a naming one, and it
has not been verified against a Gateway.
