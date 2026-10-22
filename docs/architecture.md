# How Van-Goal is put together

Van-Goal is a client for coding agents, not an agent. The backend the user
selects stays the runtime, the model gateway, the tool executor and the owner of
every session; Van-Goal is a window onto it that happens to be pleasant to use.
Nothing in this repository decides what an agent does or remembers what it said —
it translates a backend's native protocol into one event model and draws the
result.

That framing explains most of the decisions below. When in doubt: the question is
never "should the client do this for the user", it is "which backend owns this,
and what is the smallest thing the client can do to show it faithfully".

## The four layers

```
crates/core        no UI of any kind — protocols, models, settings, markdown
crates/desktop     the C ABI the desktop window loads    ─┐
desktop/           the desktop window (Go, MyGo)          │
crates/mobile      the C ABI a phone links against       ├─ two consumers of core
app/               the Flutter client                     ─┘
```

| Crate | What it owns |
| --- | --- |
| `core::agent` | The eight adapters (Hermes, OpenCode, MiMoCode, Codex CLI, Claude Code, Pi, OpenClaw, 雪小米) behind one `Backend` facade, plus the rules they all share |
| `core::models` | `AgentEvent` (what an adapter emits) and `ChatMessage` / `AgentSession` (what a frontend draws) |
| `core::chat` | `Conversation`, the folding of events into a message list, and the stream merging that keeps a two-stream reply from interleaving |
| `core::markdown` | The block and inline parser both renderers use |
| `core::settings` / `cache` / `secret_store` | Everything that survives a restart, in one app-data directory |
| `core::jsonl_process` / `local_server` | Subprocess backends, and the managed local servers (`hermes serve`, `mimo serve`) |
| `crates/desktop` | The desktop's C ABI (`vgd_command` / `vgd_wait` / `vgd_poll` / `vgd_free`): connection and reconnect loop, which session is on screen, where a prompt goes, the message list |
| `desktop/` | The window, in Go: MyGo native UI, menus, the draft being typed. Loads the core through purego, without cgo |
| `crates/mobile` | The C ABI, and the connection/reconnect/parking logic a phone needs |
| `app` | Widgets. Dart folds no events and owns no message list |

**`crates/core` may not import a UI toolkit.** That constraint is what keeps the
door open for a third frontend, and the mobile build is what breaks first if it
is ever violated.

## The event model

雪小米 advertises its model IDs, names, thinking levels, upstream level mappings
and defaults through `GET api/models`. The adapter loads that catalog on connect
and sends the chosen `model` and `thinking` on each `POST api/chat`; changing a
choice never mutates the gateway's global settings. `ModelSelection` events feed
both frontends, with per-gateway preferences in `Settings::xuexiaomi_models`.
Changed gateway defaults supersede saved choices, matching the web client.
History metadata restores a resumed session's choices; session-tagged selection
events are ignored once the user has switched away. Background history refreshes
do not override a choice waiting for the next prompt. Thinking menus only offer
the selected model's advertised levels and disclose any upstream remapping.

Every adapter translates its protocol into `AgentEvent`. The variants that carry
a turn:

```rust
MessageStart
MessageDelta { text, source }   // a chunk, possibly on more than one stream
MessageComplete(Option<String>) // the authoritative text, when the backend sends it
Attachment(ComposerAttachment) // a file returned by the backend
Tool(ToolCallRecord)
TurnFailed(String) / Failed(String) / Disconnected / Connected
SessionInfo(String) / SessionsChanged / Clarify { .. }
```

Note what is **not** in there: a session id on the message events. That is why
session filtering happens inside the adapters rather than in the frontends — see
[`sessions.md`](sessions.md).

`DeltaSource` exists because a gateway can deliver the *same* reply over two
streams at once (OpenClaw sends both a transcript frame and an assistant stream).
Chunks are buffered per stream and the most complete one is shown; appending them
into one buffer interleaves two copies of the message, which breaks the markdown
inside it.

## Folding events into a conversation

`core::chat::Conversation` owns the message list and the rules for changing it:

* a placeholder bubble appears only while a turn is in flight and the last
  message is not already a streaming assistant one;
* a tool call joins the streaming bubble that has no text yet, so a turn that
  runs twenty tools reads as one block rather than twenty;
* identical `(name, status, detail)` tool events are dropped — gateways report
  each call twice, once started and once finished;
* completing a turn stops *every* streaming message and prunes empty shells, so a
  failed turn cannot leave a spinner turning forever;
* a completion with no text never wipes what already streamed in.

These are subtle enough that two implementations drift: the mobile client used to
have a second one and it produced one "Thinking…" bubble per event. The desktop
`AppState` and the mobile bridge both fold through `Conversation` now.

## Attachments

`Backend::submit_with_attachments` sends local agents their existing `@path`
references. 雪小米 receives PNG/JPEG/GIF/WebP bytes as base64 `images` entries
with `mimeType`; other files go through `POST api/upload` with `{session, name,
data}`, then the returned server path is appended to the chat request. Validation
happens before uploading: six files maximum, 8 MB per image, 24 MB per other file.
The reverse proxy must also allow the base64 JSON bodies. On 2026-10-21 the
public gateway rejected an 871 KB PNG with nginx HTTP 413; that request exceeds
1 MB after base64 encoding. A deployment limit of 80 MB accommodates six 8 MB
images in one chat request. A refused upload restores the composer draft and
its attachments; the client does not silently resend or change the image.

雪小米's SSE `attachment` event carries `{name, url}`. It passes the same
session filter as text, and `Conversation` folds and deduplicates it, including
replies with files and no text. The desktop bridge starts caching returned files
as soon as they arrive: gateway download links may expire or be single-use, so
preview and Save must share one fetch. The cache is in `ReceivedAttachments/`;
clipboard images are retained in `Attachments/` so a cached transcript can still
preview them after a restart. Downloads are limited to 256 MB and the gateway's
own origin; redirects are refused. Preview decoding is bounded separately.
The native Save dialog starts at the absolute `~/Downloads/<filename>` path so
it does not open at the filesystem root when the app launches from Finder.

Measured history currently contains text and `[图片]` / upload-path markers,
without attachment metadata. Reopening merges files recorded by this client;
it cannot recover files from a turn it never watched. Matching the transport
markers recovers the original user text and local attachments without exposing
upload paths for prompts sent here.

The opt-in desktop bridge media check uses a caller-supplied image and text
containing `VG_MEDIA_INPUT_OK` (it creates one real gateway session):

```bash
VG_XXM_URL=https://host:port/ VG_XXM_TOKEN_FILE=path/to/token \
VG_XXM_IMAGE_FILE=path/to/image.png VG_XXM_ATTACHMENT_FILE=path/to/verification.txt \
  cargo run -p van-goal-desktop --example xuexiaomi_e2e
```

## Threading

On the desktop the core owns a multi-threaded tokio runtime; every backend
operation runs on it, and the state sits behind one `std::sync::Mutex` that is
never held across an `.await` — an async result locks again to apply itself,
after checking it still belongs to the view that asked for it. Each event
stream gets a channel and an epoch of its own, so a stream that was replaced
cannot change the state. The Go side waits in `vgd_wait` on a goroutine, polls
what changed (the small state, the messages only when their structure changed,
deltas otherwise) and hands it to MyGo's main thread with `Window.Update`.

The mobile bridge owns its own two-worker tokio runtime, because Dart has
neither. Every `vg_` call returns immediately; a command is queued onto the
runtime and its result arrives as an event.

## Storage

One app-data directory per device holds all of it:

```
settings.json               shared by every client on that device
SessionCache.json           sessions and transcripts
openclaw-device-identity.json  the Ed25519 key the Gateway knows this device by
van-goal.log                the debug log
```

On macOS that directory is `~/Library/Application Support/VanGoal`; on Windows
it is `%LOCALAPPDATA%/VanGoal`. Both paths are resolved once, with a migration
from the pre-rename `HermitGPUI` directory.

Android and iOS do not hand an app a home directory, so the host names one
first — `vg_set_data_dir`, before any other call. The choice is made once, and a
second caller is refused rather than silently ignored.

`Settings` is written by the desktop and the phone alike: it is the same file
format, and sharing it means the two clients cannot disagree about what a
"backend switch" or a "text size" is. It is *not* synced between devices.

## Sessions

A session has two ids: the **live** id a running backend knows it by, and the
**stored** key it is filed under (OpenClaw's `sessionKey`, a Hermes stored
session, a Codex thread id). Adapters return both as `SessionIDs`, and the
frontends keep both — what a backend accepts for a resume is not always what it
reports in a list, and vice versa.

Which session the user is on is the one piece of state that decides where a
turn's events are allowed to land, and every layer has a version of it. The
adapter filters by it, the frontend holds traffic off while it is being
established, and the mobile bridge parks one conversation per session. See
[`sessions.md`](sessions.md).

## Mobile

`crates/mobile` is a C ABI of four functions — `vg_set_data_dir`, `vg_command`,
`vg_poll`, `vg_free` — speaking JSON in both directions. Dart drains the event
queue on a timer and redraws; it never blocks on the network, and it never folds
an event into a message.

The phone build offers the network backends only (Hermes, OpenCode, MiMoCode,
OpenClaw). Codex CLI, Claude Code and Pi are local subprocesses, and the managed
servers (`hermes serve`, `mimo serve`) are launched as ones; none of those
binaries exist on a phone, so `crates/mobile` refuses them by name rather than
offering a connection that fails for a reason the user cannot act on.

A phone takes the app away whenever the screen locks, so the connection is owned
by a retry loop rather than by a single attempt, the backend is reconnected when
the app returns to the foreground, and the backend the user left switched on is
reconnected at launch. [`mobile.md`](mobile.md) has the details.

## Building

See the README for the commands. Two environment notes that cost time if
forgotten:

* `ring`'s C build needs `SDKROOT` exported on macOS unless the shell already has
  it.
* Xcode 26+ ships the Metal shader compiler separately:
  `xcodebuild -downloadComponent MetalToolchain`.
* The Windows desktop builds with either Rust toolchain (MSVC or GNU). The
  window is native MyGo UI, so there is no WebView2 runtime to install.
  `Scripts/package_windows.ps1` puts `VanGoal.exe`, `van_goal_desktop.dll` and
  `AppIcon.png` in one folder, embeds the icon and manifest into the exe when
  mingw-w64's `windres` is available, and writes a portable x64 zip from a
  release build.
