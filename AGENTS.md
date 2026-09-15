# Working in this repository

Van-Goal is a client for coding agents: a macOS app (GPUI) and an Android app
(Flutter), both drawing on one Rust core that speaks seven agent protocols. The
backend stays the runtime, the model gateway, the tool executor and the owner of
every session. The client translates a protocol into one event model and draws
it. [docs/architecture.md](docs/architecture.md) is the full picture;
[docs/sessions.md](docs/sessions.md) and [docs/mobile.md](docs/mobile.md) cover
the two parts that are easiest to get subtly wrong.

## Layout

```
crates/core        protocols, models, settings, markdown — no UI of any kind
crates/desktop     the macOS client (GPUI)
crates/mobile      the C ABI the Flutter app links against
app/               the Flutter client
assets/            the app icon
Build/             packaged output (ignored by git)
Scripts/           toolchain setup and packaging
docs/              architecture, sessions, mobile
```

## Rules that are not negotiable

**`crates/core` must not import a UI toolkit.** That is the whole reason the
crates are split, and the mobile build is what breaks first if it is violated.
Anything a second frontend would have to get right belongs in core: the
conversation folding (`chat::Conversation`), the markdown parser, the session
filter (`agent::session_scope`), settings.

**A frontend does not fold events into messages.** Both clients hand their
message list to `chat::Conversation` and draw what it gives back. The mobile
client once had its own version and it produced one "Thinking…" bubble per event
plus a spinner that never stopped.

**Every adapter whose events carry a session id calls
`session_scope::belongs_to_session`.** One connection carries many sessions; the
failure mode is another session's reply appearing in the open chat, which does
not look like a failure. See [docs/sessions.md](docs/sessions.md) for the rule
and for what the frontends add on top.

**Tests must never touch real app data.** `crates/core` has a `testing` feature
that redirects the app-data directory to a throwaway temp directory; a crate that
tests against core turns it on from `[dev-dependencies]`, because `cfg(test)`
does not reach a dependency. Without it a test run rewrites the developer's
settings, session cache and OpenClaw device identity.

**Nothing user-visible is invented client-side.** The client does not summarize,
soften or retry a backend's answer, and an error the user cannot act on is
reported as it came.

## Behaviour worth matching

* **A turn's shape is decided once, in `Conversation`**: a placeholder only while
  a turn is in flight, tool calls folded into one bubble, duplicates within a
  turn dropped, every streaming message stopped when the turn ends (including
  when it fails).
* **A phone loses its connection routinely** (screen lock, process freeze) and
  the socket will not error on its own. The mobile bridge owns a retry loop with
  a backoff, reconnects on return to the foreground, and connects on launch when
  the saved switch is on. Every attempt starts from a clean adapter — an adapter
  abandoned mid-handshake claims to be open and will hang the next attempt
  silently.
* **Streaming deltas coalesce in the mobile queue** but only against the
  immediately previous delta for the *same* message; anything in between is kept.
* **The CLI backends are desktop-only.** Codex CLI, Claude Code, Pi and a managed
  `hermes serve` are local subprocesses; `crates/mobile` refuses them by name
  rather than offering a connection that fails for a reason the user cannot act
  on.

## Building and testing

```bash
cargo test --workspace          # core + desktop + mobile + the C ABI
cargo clippy --workspace --all-targets
cargo fmt --all

cd app && flutter test && flutter analyze     # needs Scripts/android_env.sh sourced
```

Desktop:

```bash
export SDKROOT="$(xcrun --show-sdk-path)"     # ring's C build needs it
xcodebuild -downloadComponent MetalToolchain  # Xcode 26+ ships it separately
cargo run --release -p van-goal
Scripts/package_app.sh                        # -> Build/VanGoal.app
```

Android (the toolchain installs without `sudo`, into the user's own directories):

```bash
Scripts/setup_android_toolchain.sh
Scripts/build_android.sh
VAN_GOAL_ABIS="arm64-v8a armeabi-v7a x86_64" Scripts/build_android.sh

source Scripts/android_env.sh                 # flutter, adb, cargo-ndk on PATH
adb install -r app/build/app/outputs/flutter-apk/app-arm64-v8a-release.apk
```

`flutter build apk --release` alone produces a **universal** APK, which is a trap
after a single-ABI build: the other ABIs still hold the previous version's
`.so` files. Build the split APKs (`--split-per-abi`) or rebuild every ABI.

## Things that surprise people

* **`vg_set_data_dir` must be the first call** on mobile. The app-data directory
  is resolved once, and a second caller is refused rather than obeyed.
* **`settings.json` is shared by every client on a device** (not synced between
  them). A new preference is usually a field on `Settings` and a line in the
  mobile bridge's `settings` payload, not a new file.
* **The release APK is debug-signed** (`TODO` in `app/android/app/build.gradle.kts`)
  and Flutter does not put `INTERNET` in the release manifest — it is added
  explicitly there.
* **OpenClaw identifies the device as `darwin`/`desktop`** from compile-time
  constants (`agent/device_identity.rs`), so a phone looks like a Mac in
  `openclaw devices list`.
* **`chat.history` carries no tool calls.** Tool activity is only known for turns
  this client watched; reopening a session merges back what the client recorded
  (`Conversation::merge_transcript`) and nothing more.
* **The desktop opens one connection and switches sessions on it**, so the window
  between switching and the gateway confirming the subscription is where another
  session's traffic gets in — the live session id is cleared for exactly that
  window, on both clients.
