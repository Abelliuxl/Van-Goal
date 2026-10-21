# Van-Goal

**A native desktop client for local and remote coding agents — a window drawn by [MyGo](https://github.com/egoist/mygo) (Go) over a Rust core.**

Van-Goal is a thin, fast frontend: the selected agent backend stays the runtime, model gateway, tool executor, memory system, and session owner. The Rust core (`crates/core`, behind the C ABI in `crates/desktop`) speaks the agents' protocols and owns the conversation; the window in `desktop/` is native UI that MyGo draws on the GPU, in pure Go without cgo.

![Platform](https://img.shields.io/badge/platform-macOS%2012%2B%20%7C%20Windows%2010%2B-000000?style=flat-square)
![Language](https://img.shields.io/badge/language-Rust%20%2B%20Go-DEA584?style=flat-square&logo=rust)
![UI](https://img.shields.io/badge/UI-MyGo-084CCF?style=flat-square)
![License](https://img.shields.io/badge/license-MIT-4c3d2e?style=flat-square)

## Highlights

- **Streaming chat** — replies fade in word by word, the list follows the end until you scroll up, and a turn's tool calls fold into one line that opens to the list.
- **Markdown rendering** — CommonMark with tables, task lists and highlighted code, selectable and copyable, through MyGo's streaming markdown view.
- **Auto-managed backend** — probes for a local Hermes service, starts `hermes serve` when needed, and discovers the session token on its own. No URL to type for daily local use.
- **Sessions sidebar** — resume, archive, or delete past conversations; one-click archive-all / delete-all with confirmation.
- **Queue + clarify** — queue follow-up prompts while a turn is running (send now / edit / cancel), and answer Hermes clarify prompts from a tappable card or the composer.
- **Model switcher** — pick any provider/model your Hermes config exposes, with a context-window meter.
- **Permission modes** — Full access / Ask first / Restricted tools, applied through `hermes config set`.
- **One switch per backend** — each backend has its own on/off switch in Settings. Switching one on connects it and switches the others off; switching it off disconnects it and keeps it off after a restart. The backend you were last using is the one that comes back on launch.
- **Text size** — Small / Default / Large / Extra Large in Settings scales every font in the app at once: transcript, markdown, sidebar, composer and the settings window itself.
- **Interface zoom** — View → Zoom In / Zoom Out / Actual Size (⌘+ / ⌘− / ⌘0) magnifies the whole window, text and chrome together, and the level survives a restart. It is independent of Text size, so you can enlarge type without inflating the layout around it.
- **Comes back on its own** — a dropped connection is retried with a backoff and the open chat is re-attached; a prompt that cannot be delivered goes back to the composer with the reason, never into another chat.
- **Native integration** — system/light/dark appearance, app-local credentials, Ed25519 device identity for OpenClaw, drag-and-drop attachments, and a native menu with Command shortcuts on macOS and Ctrl shortcuts on Windows (N / R / B / , / + / − / 0).
- **雪小米 attachments** — send images as multimodal input, upload documents, paste screenshots or copied files, preview images, and save files returned by the agent. Saving starts in your Downloads folder. Up to six attachments per message: 8 MB per image and 24 MB per other file.
- **Android files** — pick images or documents, insert keyboard images, preview returned images, and save received files to `Download/Van-Goal`. Unsent text and attachments return to the composer when delivery fails.

## Build

### macOS

Requires macOS 12+, stable Rust and Go (any recent Go fetches the 1.27 toolchain MyGo needs).

```bash
export SDKROOT="$(xcrun --show-sdk-path)"   # ring's C build needs it
cargo build -p van-goal-desktop             # the core, as a dylib
cd desktop && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 go run .
```

Package a local app bundle (the Go binary, with the core in `Contents/Resources`):

```bash
Scripts/package_app.sh
open Build/VanGoal.app
```

Run the tests:

```bash
cargo test --workspace
cd desktop && GOTOOLCHAIN=go1.27.3 CGO_ENABLED=0 go test ./...   # the view, headless, against the real core
```

### Windows

Requires Windows 10 or later, Rust (either the MSVC or the GNU toolchain), and Go. The window is native MyGo UI, so it needs no WebView2 runtime.

```powershell
cargo build -p van-goal-desktop             # the core, as van_goal_desktop.dll
cd desktop
$env:GOTOOLCHAIN = "go1.27.3"; $env:CGO_ENABLED = "0"
go run .
```

```powershell
Scripts/package_windows.ps1                 # -> Build\VanGoal-windows-x86_64 and its zip
```

The package holds `VanGoal.exe`, `van_goal_desktop.dll` and `AppIcon.png`; keep the three together. The script embeds the app icon, a per-monitor-DPI manifest and version information into the executable with `windres` (mingw-w64) when it is installed; without it the exe builds anyway and the window still takes its icon from `AppIcon.png`. Settings and the local cache live in `%LOCALAPPDATA%\VanGoal`.

Run the tests the same way:

```powershell
cargo test --workspace
cd desktop; $env:GOTOOLCHAIN = "go1.27.3"; $env:CGO_ENABLED = "0"; go test ./...
```

## Backends

Backends are selectable in Settings; each adapter translates its native protocol into Van-Goal's shared event model.

| Backend | Transport | Session support |
| --- | --- | --- |
| Hermes | REST + WebSocket JSON-RPC | List, create, resume, stream, clarify |
| OpenCode | `opencode serve` HTTP + SSE | List, create, resume, stream, permissions |
| MiMoCode | `mimo serve` OpenCode-compatible HTTP + SSE, managed locally | List, create, resume, stream |
| Codex CLI | Local `codex app-server` JSON-RPC | List, create, resume, stream, approvals |
| Claude Code | Local bidirectional `stream-json` process | Create, resume, stream, tools |
| Pi | Local `pi --mode rpc` JSONL process | Create, resume, stream, extension UI |
| OpenClaw | Gateway protocol v4 over WebSocket | List, create, resume, stream, approvals |
| 雪小米 | HTTPS gateway: REST + SSE per turn, bearer token | List (incl. WeChat / Feishu / iMessage snapshots), create, resume, stream, stop |

Local CLI backends use the workspace directory configured in Settings and reuse the CLI's existing login. Hermes defaults to port `9119`, OpenCode/MiMoCode to `4096`, OpenClaw to `18789`. At a loopback address Van-Goal starts and manages `hermes serve` and `mimo serve` itself, so a switch to either needs nothing running first; a server already listening on the port is used as it is. `mimo serve` scopes every session to the directory it was started in, so the Workspace field decides which project its sessions and its agent work in.

OpenClaw connections create a stable Ed25519 device identity in Van-Goal's local app-data directory and sign the Gateway challenge nonce. A new remote device may appear as pending in OpenClaw and must be approved once.

### Connect to OpenClaw

1. Start the OpenClaw Gateway and note its host, port, TLS setting, and `gateway.auth.token`.
2. Open Van-Goal Settings and switch **OpenClaw** on, entering either a host/port or a complete `ws://` / `wss://` Gateway URL. Complete URLs preserve reverse-proxy paths and override the separate Port and TLS fields.
3. On the first connection, run `openclaw devices list` on the Gateway host and approve Van-Goal's exact pending request with `openclaw devices approve <requestId>`.
4. Switch OpenClaw on again. Van-Goal stores the issued device token in its local app-data directory and keeps each backend's connection settings separate; Settings reports that stored token, which is why the gateway-token field can stay empty once the device is paired.

## Documentation

| Document | What it covers |
| --- | --- |
| [`AGENTS.md`](AGENTS.md) | The rules of the codebase, how to build and test each target, and the traps |
| [`docs/architecture.md`](docs/architecture.md) | The whole system: layers, the event model, storage, threading |
| [`docs/sessions.md`](docs/sessions.md) | One connection carrying many sessions, and the rule that keeps them apart |
| [`docs/mobile.md`](docs/mobile.md) | The Android client: the C ABI, the queue, reconnecting, building |

## Architecture

Van-Goal is deliberately a thin frontend — all agent capability lives in the backend.

| Part | Language | Responsibility |
| --- | --- | --- |
| `crates/core` | Rust | The protocol adapters, the event model, `chat::Conversation`, markdown, settings, cache. No UI of any kind |
| `crates/desktop` | Rust | A cdylib with a JSON C ABI (`vgd_command`, `vgd_wait`, `vgd_poll`, `vgd_free`): the desktop's connection, reconnect loop, session state machine and message list |
| `desktop/` | Go | The window, drawn by MyGo: sidebar, transcript, composer, settings. Loads the core with purego, so it builds without cgo |
| `crates/mobile` + `app/` | Rust + Dart | The Android client |

The Go side folds no events and decides nothing about sessions: it draws the state the core reports and sends it commands. A goroutine blocks in `vgd_wait` and redraws the moment something changes, so a streaming delta is on screen within a frame and an idle app costs nothing.

| Crate | Layer | Responsibility |
| --- | --- | --- |
| `core` | `agent/` | Backend-neutral facade + protocol adapters (Hermes, OpenCode, CLI, OpenClaw, 雪小米) |
| `core` | `models.rs` | Normalized event model every adapter translates into |
| `core` | `jsonl_process.rs` | Codex / Claude Code / Pi subprocess lifecycle and JSONL streaming |
| `core` | `local_server.rs` | Discovers/launches the local `hermes serve` process |
| `core` | `hermes_config.rs` | Reads/writes `~/.hermes` config, model cache, permission modes |
| `core` | `markdown.rs` | Block-level markdown parser, shared with the renderer |
| `core` | `cache.rs` / `secret_store.rs` / `settings.rs` / `logger.rs` | On-disk cache, local credentials, persisted settings, debug log |
| `crates/desktop` | `connect.rs` / `sessions.rs` / `send.rs` / `events.rs` | Connecting and reconnecting, which session is on screen, where a prompt goes, folding events |
| `crates/desktop` | `payload.rs` / `commands.rs` | The JSON the window draws from, and the commands it sends |
| `desktop/` | `chat.go` / `view.go` / `settings.go` / `theme.go` | Transcript and composer, sidebar and header, settings, the palette |
| `desktop/` | `bridge/` | Loading the core and its C ABI |

`crates/core` has a `testing` feature that redirects the app-data directory to a
throwaway temp directory, and the Go tests do the same through
`vgd_set_data_dir`: a test run never reads or rewrites the developer's real
settings, session cache or OpenClaw device identity.

### Threading model

The core owns a multi-threaded tokio runtime; every backend operation runs there, and its state sits behind one lock that is never held across an `.await`. The window's view runs on MyGo's main thread; a goroutine waits in `vgd_wait`, polls what changed and hands it to the view with `Window.Update`.

## Android

`app/` is a Flutter client for the same Rust core, reached through
`crates/mobile` — a small C ABI that owns a tokio runtime and drives a
`Backend`, so Dart never blocks on the network. Dart owns the widgets; Rust owns
the connection.

The interface is three calls: a command goes in as JSON, an acknowledgement
comes back, and everything the command produced arrives from `vg_poll()` as
events. Streaming deltas coalesce in that queue, so a long reply costs one slot
rather than one per token. See the module comment in `crates/mobile/src/lib.rs`
for why the surface is shaped that way.

### Only the network backends

The phone build offers **Hermes, OpenCode, MiMoCode and OpenClaw**. Codex CLI,
Claude Code and Pi are local subprocesses, and a managed `hermes serve` is
launched as one — none of those binaries exist on a phone and an Android app
cannot spawn them. `crates/mobile` refuses them at the edge rather than offering
a connection that fails for a reason the user cannot act on.

### Build

The toolchain installs without `sudo`, under your own home directory:

```bash
Scripts/setup_android_toolchain.sh   # JDK, Flutter, Android SDK + NDK, Rust targets
Scripts/build_android.sh             # cross-compile the core, then package the APK
```

`Scripts/android_env.sh` holds the environment both scripts use; source it to
run `flutter` or `adb` by hand. It points `FLUTTER_STORAGE_BASE_URL` and
`PUB_HOSTED_URL` at the Flutter team's mirrors, which are several times faster
than the origin from here.

The APK lands in `app/build/app/outputs/flutter-apk/app-release.apk`. Install it
with `adb install -r` or by copying it to the phone.

## License

MIT
