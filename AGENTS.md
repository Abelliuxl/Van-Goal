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

`crates/core/examples/gateway_probe.rs` is the wire-level diagnostic for the
Gateway. Session isolation is decided by what a frame says about *which* session
it belongs to, and that cannot be reasoned about from the source — every fact in
[docs/sessions.md](docs/sessions.md) about what a Gateway sends was measured with
this. It connects with the same device identity the app uses and prints each
frame with the label the adapter would read off it, then scores the whole
capture with both session rules:

```bash
cargo run -p van-goal-core --example gateway_probe -- --list
cargo run -p van-goal-core --example gateway_probe -- --history agent:main:some-session
cargo run -p van-goal-core --example gateway_probe -- --poke     # traffic in a session of its own
```

`--poke` creates `agent:main:van-goal-probe:isolation-probe` and puts one turn in
it, which is how a leak is reproduced on purpose. Removing it needs the
`operator.admin` scope, which this client is not granted, so a poked session has
to be deleted from the Gateway's own side.

`crates/core/examples/mimocode_probe.rs` is the same kind of instrument for the
OpenCode family. Its two servers look alike and are not: `mimo serve` picks a
random port unless it is told which one, scopes every session to the directory it
was started in, pairs its password with the `mimocode` user rather than
`opencode`, and reports a text part as the whole text so far with no `delta`
field. This runs the app's own managed-server path — start, wait for health,
list, create, prompt, stream — in one command:

```bash
cargo run -p van-goal-core --example mimocode_probe
cargo run -p van-goal-core --example mimocode_probe -- --prompt "reply with exactly: PROBE-OK"
cargo run -p van-goal-core --example mimocode_probe -- --keep-running
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
`session_scope::belongs_to_open_session` for turn traffic.** One connection
carries many sessions; the failure mode is another session's reply appearing in
the open chat, which does not look like a failure. A Gateway pushes every
session to every client — measured, and `sessions.messages.subscribe` does not
change it — so "nothing subscribed yet" must not mean "accept everything": that
is the state a client is in for the whole window after a reconnect. See
[docs/sessions.md](docs/sessions.md).

**A prompt goes to the session on screen, or nowhere.** Having no live
connection to it is not the same as having no session: it is the state after
every reconnect. Resume it and send, rather than creating a session the user
never opened — that is the difference between a chat that keeps its context and
one that looks like messages are landing in the wrong conversation.

**A client comes back to the session it had open.** An app that starts on an
empty chat makes the first thing typed create another session, and on a phone,
whose socket drops constantly, that is the normal case rather than the
exception. `Settings::last_session` is that memory on mobile; `new_session`
clears it.

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

`app/pubspec.yaml` says `0.1.0+1` while the installed app reports a version code
in the thousands, so a plain `flutter build apk --release` is **refused as a
downgrade** over an install made with `--build-number`. Pass a higher one:
`flutter build apk --release --build-number=2003`.

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
  `openclaw devices list`. The name it *calls itself* is separate and **is**
  per-frontend (`OPENCLAW_DISPLAY_NAME`, `OPENCLAW_SESSION_NAMESPACE`): the
  Gateway titles a session after the client that created it, so one shared name
  fills the session list with identical entries and picking the wrong one is
  indistinguishable from a client that crossed two conversations. Measured:
  ten sessions all titled "Van-Goal".
* **`chat.history` carries no tool calls.** Tool activity is only known for turns
  this client watched; reopening a session merges back what the client recorded
  (`Conversation::merge_transcript`) and nothing more.
* **The desktop opens one connection and switches sessions on it**, so the window
  between switching and the gateway confirming the subscription is where another
  session's traffic gets in — the live session id is cleared for exactly that
  window, on both clients.
