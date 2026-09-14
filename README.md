# Hermit GPUI

**A native macOS client for local and remote coding agents — rebuilt on [GPUI](https://gpui.rs) (Rust).**

Hermit GPUI is the GPUI sibling of [Hermit](https://github.com/Abelliuxl/Hermit) (SwiftUI). It is a thin, fast frontend: the selected agent backend stays the runtime, model gateway, tool executor, memory system, and session owner — Hermit just gives it a beautiful home on your Mac, now rendered by Zed's GPU-accelerated UI framework.

![Platform](https://img.shields.io/badge/platform-macOS%2012%2B-000000?style=flat-square&logo=apple)
![Language](https://img.shields.io/badge/language-Rust-DEA584?style=flat-square&logo=rust)
![UI](https://img.shields.io/badge/UI-GPUI%200.2-084CCF?style=flat-square)
![License](https://img.shields.io/badge/license-MIT-4c3d2e?style=flat-square)

## Highlights

- **Streaming chat** — token-by-token responses with a live "Thinking" pill and expandable tool-call activity.
- **Markdown rendering** — headings, lists, quotes, fenced code blocks, and tables rendered natively from GPUI primitives.
- **Auto-managed backend** — probes for a local Hermes service, starts `hermes serve` when needed, and discovers the session token on its own. No URL to type for daily local use.
- **Sessions sidebar** — resume, archive, or delete past conversations; one-click archive-all / delete-all with confirmation.
- **Queue + clarify** — queue follow-up prompts while a turn is running (send now / edit / cancel), and answer Hermes clarify prompts from a tappable card or the composer.
- **Model switcher** — pick any provider/model your Hermes config exposes, with a context-window meter.
- **Permission modes** — Full access / Ask first / Restricted tools, applied through `hermes config set`.
- **Native integration** — system/light/dark appearance, app-local credentials, Ed25519 device identity for OpenClaw, close-to-minimize window behavior, native menu bar with ⌘N / ⌘R / ⌘, shortcuts.

## Build

Requires macOS 12+ and a recent stable Rust toolchain.

```bash
rustup default stable
# If the C toolchain cannot find macOS SDK headers (ring's build):
export SDKROOT="$(xcrun --show-sdk-path)"

# Xcode 26+ ships the Metal shader compiler as a separate component:
xcodebuild -downloadComponent MetalToolchain

cargo build --release
```

Run the app:

```bash
cargo run --release
```

Package a local app bundle:

```bash
Scripts/package_app.sh
open Build/HermitGPUI.app
```

## Backends

Backends are selectable in Settings; each adapter translates its native protocol into Hermit's shared event model.

| Backend | Transport | Session support |
| --- | --- | --- |
| Hermes | REST + WebSocket JSON-RPC | List, create, resume, stream, clarify |
| OpenCode | `opencode serve` HTTP + SSE | List, create, resume, stream, permissions |
| MiMoCode | `mimo serve` OpenCode-compatible HTTP + SSE | List, create, resume, stream |
| Codex CLI | Local `codex app-server` JSON-RPC | List, create, resume, stream, approvals |
| Claude Code | Local bidirectional `stream-json` process | Create, resume, stream, tools |
| Pi | Local `pi --mode rpc` JSONL process | Create, resume, stream, extension UI |
| OpenClaw | Gateway protocol v4 over WebSocket | List, create, resume, stream, approvals |

Local CLI backends use the workspace directory configured in Settings and reuse the CLI's existing login. Hermes defaults to port `9119`, OpenCode/MiMoCode to `4096`, OpenClaw to `18789`.

OpenClaw connections create a stable Ed25519 device identity in Hermit's local app-data directory and sign the Gateway challenge nonce. A new remote device may appear as pending in OpenClaw and must be approved once.

### Connect to OpenClaw

1. Start the OpenClaw Gateway and note its host, port, TLS setting, and `gateway.auth.token`.
2. Open Hermit Settings, choose **OpenClaw**, enter either a host/port or a complete `ws://` / `wss://` Gateway URL, and click **Connect to OpenClaw**. Complete URLs preserve reverse-proxy paths and override the separate Port and TLS fields.
3. On the first connection, run `openclaw devices list` on the Gateway host and approve Hermit's exact pending request with `openclaw devices approve <requestId>`.
4. Connect again. Hermit stores the issued device token in its local app-data directory and keeps Hermes and OpenClaw connection settings separate.

## Architecture

Hermit GPUI is deliberately a thin frontend — all agent capability lives in the backend.

| Layer | Responsibility |
| --- | --- |
| `main.rs` | App entry, tokio runtime, menus, actions, windows |
| `state.rs` | Single source of truth: sessions, messages, streaming, sending, queue |
| `agent/` | Backend-neutral facade + protocol adapters (Hermes, OpenCode, CLI, OpenClaw) |
| `jsonl_process.rs` | Codex / Claude Code / Pi subprocess lifecycle and JSONL streaming |
| `local_server.rs` | Discovers/launches the local `hermes serve` process |
| `hermes_config.rs` | Reads/writes `~/.hermes` config, model cache, permission modes |
| `ui/` | GPUI views: root shell, sidebar, chat, composer, editor, settings |
| `editor.rs` | Multi-line text editor element built on GPUI text shaping |
| `markdown.rs` | Block-level markdown parser shared with the renderer |
| `cache.rs` / `secret_store.rs` / `settings.rs` | On-disk cache, local credentials, persisted settings |

### Threading model

GPUI owns the main-thread UI executor. A multi-threaded tokio runtime is installed as a GPUI global; every backend operation runs on it and marshals results back onto the UI through GPUI entities. Backend events arrive on a single `futures::channel::mpsc` channel and are pumped into the state machine, mirroring the SwiftUI version's actor-based design.

## Differences from the SwiftUI Hermit

- System, light, and dark appearance modes are available in Settings.
- File attachments are added via the native open panel (the `+` button); drag-and-drop is not wired yet.
- Inline markdown (bold/links) is not styled per-span yet; block-level structure is.

Everything else — session management, streaming, queueing, clarify, model switching, permission modes, and all seven backends — matches the SwiftUI app's behavior.

## License

MIT
