# The Android client

`app/` is a Flutter application; `crates/mobile` is a C ABI over `crates/core`.
Dart owns the widgets, Rust owns the connection, and neither knows much about the
other.

## Why a C ABI and not generated bindings

Four functions, JSON in both directions:

| Function | What it does |
| --- | --- |
| `vg_set_data_dir(path)` | Names the private directory the app may write to. **Must be first.** |
| `vg_command(json)` | Runs one command; returns `{"ok":true, …}` or `{"ok":false,"error":…}` |
| `vg_poll()` | Drains everything that has happened since the last call, as a JSON array |
| `vg_free(ptr)` | Releases a string the library handed out |

Hand-written FFI over JSON is a smaller thing to depend on than a code generator
whose version has to match the crate's, and the payloads here are chat messages —
the encoding cost is nothing next to the network round trip. It also means the
wire format is inspectable in a log.

Unwinding across `extern "C"` is undefined behaviour, so every entry point wraps
its body in `catch_unwind` and reports a panic as an error string instead of
aborting the process.

## Why nothing blocks

Dart calls these on its platform thread. Listing sessions, connecting or sending
a prompt all talk to the network, so a command is queued onto the bridge's own
tokio runtime (two worker threads — one turn at a time plus the event pump) and
its result comes back through `vg_poll` as an event. The Flutter side is a loop
that drains events and rebuilds, which is how it wants to work anyway.

The few commands whose answer is a *value* rather than an event — the saved
preferences, the markdown blocks of a message — return it in the reply. Those are
computed locally and instantly, so routing them through the queue would mean
matching a request to an event for no gain.

## The queue

`vg_poll` returns everything queued since the previous call, and the queue is
capped so a Dart side that stops polling (a backgrounded app) cannot grow it
without bound.

Streaming deltas **coalesce**: each delta carries the whole text so far, so a new
one replaces the previous one *for the same message* rather than queueing behind
it. A long reply therefore costs one slot rather than one per token. A delta for a
different message is a different message's first words and is kept — as is
anything that arrived in between, because an error or a tool call that lands
mid-stream must not be swallowed.

Text that merely streamed further is sent as a small `assistant` event carrying
the message id. Anything *structural* — a message added, completed, or given a
tool call — is sent as a fresh `transcript` snapshot. That split is why the
payload stays small while the widget tree stays dumb.

## Reconnecting

The normal case on a phone is that the app is taken away: the screen locks, the
process is frozen, the socket dies and nothing on this side notices, because a
frozen process runs no timeouts.

The connection is therefore owned by a retry loop rather than by a single
attempt:

* It keeps trying for as long as the user wants the connection, with a backoff
  (1, 2, 4, 8, 15, then 30 seconds). The switch the user last left on is saved, so
  launching the app connects on its own; a backend they explicitly disconnected
  stays off.
* Every attempt starts from a **clean adapter**. An adapter abandoned mid
  handshake still reports itself as open, and the next attempt would then wait on
  a handshake whose events nobody is listening to: socket up, no events ever,
  the header on "connecting" with nothing left to fail.
* The attempt timeout (45s) deliberately outlasts the ~20s the adapters wait for
  a gateway to answer, so that the adapter is the one to give up and say why.
* After reconnecting, the session that was open is subscribed again — a gateway
  only delivers a session's frames to a connection that asked for it.
* Returning to the foreground after more than five seconds away forces a fresh
  connection, because a socket that outlived a locked screen may never error on
  its own. Regaining focus *without* an absence (a keyboard opening, a dialog
  closing) does not, since that would throw away a reply being written.

## What the phone does not offer

| Backend | Why not |
| --- | --- |
| Codex CLI, Claude Code, Pi | Local subprocesses; the binaries do not exist on a phone and an app cannot spawn them |
| A managed `hermes serve` | Launched as a local subprocess for the same reason |

Hermes itself *is* offered — as a remote server the user points at. Only the
"Van-Goal starts one for you" part is desktop-only.

## Settings shared with the desktop

`font_size` and `show_tool_calls` are the same fields the desktop writes, in the
same `settings.json` (per device, not synced). The text size scales every font in
the app through Flutter's `TextScaler`, which is the mobile equivalent of the
desktop's "multiply every design size by one number".

`show_tool_calls` exists because a long turn can run dozens of calls and, on a
phone, they take more room than the reply they belong to. Off draws the replies
alone.

## Phone and tablet layout

One Android package adapts to the app window's current logical width through
`LayoutBuilder`, including split-screen and freely resized windows:

| Available width | Layout |
| --- | --- |
| Below 840 logical pixels | A single conversation with the session list in a drawer |
| 840 and above | A permanent 280-pixel session sidebar beside the conversation |

The conversation and composer share a centered column capped at 820 logical
pixels. Bubbles use that column's constraints rather than the full display
width. Extremely narrow windows put secondary toolbar actions in a menu. The
composer can scroll within 60% of the pane's available height when attachments
or a keyboard leave little room. Empty-state content is scrollable too.

The conversation pane retains its widget identity, text controller and scroll
controller when the sidebar appears or disappears. The sidebar uses the same
session-list component as the drawer, but selecting a session there does not
pop a route. This is a presentation change; the Rust session/event model is
shared and unchanged.

Settings use a centered dialog capped at 560 pixels in windows at least 600
pixels wide, and a bottom sheet on smaller windows. Image previews fit the
message column and their zoom dialog is capped at 1000 pixels. Ctrl+Enter and
Cmd+Enter send from an external keyboard; ordinary Enter remains a newline.
The Android activity is explicitly resizable and does not lock orientation.

Reference: [Flutter's adaptive layout approach](https://docs.flutter.dev/ui/adaptive-responsive/general).
Functional verification of rotation, resizing, keyboard interaction, drafts and
session switching is handed to the user; do not run UI/device tests without
their explicit request.

## Images and files on Android

雪小米 supports six attachments per message, with an 8 MB limit per supported
image and 24 MB per other file. The composer offers separate image and file
pickers and accepts images inserted by the keyboard. Android's document picker
grants access only to the selected content URIs; Kotlin copies their bytes into
the app's private `Attachments/` directory before handing paths to Rust. This
does not require access to the user's entire storage or photo library.

`send` accepts an `attachments` array of private paths. The mobile bridge adds
the prompt and placeholder through `Conversation`, then calls the same
`Backend::submit_with_attachments` as the desktop. Failed delivery removes the
unsent prompt and emits `composer_restore` with the original text and files.
The view keeps any newer draft and puts the returned text before it. A session
switch while delivery is pending cannot redirect the prompt to another chat.

Transcript snapshots include `{path, name, remote}` attachments. Received images
load through asynchronous `load_attachment` commands, whose `attachment_ready`
events carry a request id and private cached path. File bytes never travel
through large base64 FFI payloads. Previews and saving share the core's durable
cache so they do not consume a single-use gateway link twice. Local history is
stored through an ordered background writer in `SessionCache.json`, with keys
qualified by backend; restarting can recover attachment metadata this client
watched even when the gateway's history only contains text.

Saving on Android 10 and newer writes to `Download/Van-Goal` through MediaStore,
then offers to open the saved content URI. Android 7–9 uses the system Save As
picker (starting in Downloads where the OS supports an initial location).
No broad storage permission is added. Other mobile backends keep text-only
sending until they have a native upload protocol.

The public gateway's nginx request-body limit is independent of the app's
8/24 MB limits; HTTP 413 still returns the unsent draft and files. See the
measured deployment limit in [architecture.md](architecture.md#attachments).

## Building

```bash
Scripts/setup_android_toolchain.sh   # JDK, Flutter, Android SDK + NDK, Rust targets
Scripts/build_android.sh             # cross-compile the core, then package the APK
```

The toolchain installs without `sudo`, under the user's own home directory —
`/opt/homebrew` is not writable by the account this was built for, so Homebrew
is deliberately not involved. `Scripts/android_env.sh` holds the environment both
scripts use; source it to run `flutter` or `adb` by hand.

`build_android.sh` builds `arm64-v8a` only by default (every modern phone);
`VAN_GOAL_ABIS="arm64-v8a armeabi-v7a x86_64"` builds the rest. It writes the
`.so` files into `app/android/app/src/main/jniLibs`, which is a build product and
is not committed.

Two things about the release APK that are worth knowing before shipping one:

* It is signed with the **debug key** (see the `TODO` in `app/android/app/build.gradle.kts`),
  which is fine for installing over an existing copy but not for publishing.
* Flutter only adds the `INTERNET` permission to its debug and profile
  manifests, so the release manifest carries it explicitly. Without that line the
  APK installs, launches and never reaches a backend.
