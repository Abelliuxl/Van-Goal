#!/usr/bin/env bash
# Bootstrap the Android build toolchain for Van-Goal on macOS.
#
# Everything installs without sudo, into the user's own directories:
#   JDK            ~/development/jdk21            (Amazon Corretto 21)
#   Flutter        ~/development/flutter
#   Android SDK    ~/Library/Android/sdk
#   Rust targets   ~/.rustup
#
# Homebrew is deliberately not used: /opt/homebrew is not writable by the
# account here, so `brew install` would need a `sudo chown` first. Downloading
# the archives directly keeps the whole bootstrap password-free.
#
# Idempotent: each step is skipped when its result is already present, so it is
# safe to re-run after a failure part-way through.
set -uo pipefail

ROOT="$HOME/development"
SDK="$HOME/Library/Android/sdk"
JDK="$ROOT/jdk21"
JDK_URL="https://corretto.aws/downloads/latest/amazon-corretto-21-aarch64-macos-jdk.tar.gz"
CMDLINE_TOOLS_VERSION="11076708"
NDK_VERSION="27.0.12077973"
COMPILE_SDK="35"
BUILD_TOOLS="35.0.0"

mkdir -p "$ROOT"

step() { printf '\n========== %s ==========\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------- 1. JDK
step "1/7  JDK 21 (Amazon Corretto)"
if [ -x "$JDK/bin/java" ]; then
    echo "already installed"
else
    curl -fL --http1.1 --retry 3 -sS -w "downloaded %{size_download} bytes\n" \
        "$JDK_URL" -o "$ROOT/jdk21.tar.gz" || echo "!! JDK download failed"
    # The macOS archive wraps the JDK in `<name>.jdk/Contents/Home`, so it is
    # unpacked aside and then flattened into $JDK.
    rm -rf "$ROOT/jdk21-bundle" "$JDK"
    mkdir -p "$ROOT/jdk21-bundle" "$JDK"
    tar -xzf "$ROOT/jdk21.tar.gz" -C "$ROOT/jdk21-bundle" && rm -f "$ROOT/jdk21.tar.gz"
    mv "$ROOT"/jdk21-bundle/*/Contents/Home/* "$JDK"/ 2>/dev/null
    rm -rf "$ROOT/jdk21-bundle"
fi
export JAVA_HOME="$JDK"
export PATH="$JDK/bin:$PATH"
java -version 2>&1 | head -3

# ------------------------------------------------------------- 2. Flutter
step "2/7  Flutter SDK"
if [ -x "$ROOT/flutter/bin/flutter" ]; then
    echo "already installed"
else
    URL=$(curl -fsSL https://storage.googleapis.com/flutter_infra_release/releases/releases_macos.json |
        python3 -c '
import json, sys
d = json.load(sys.stdin)
hash_ = d["current_release"]["stable"]
rel = next(r for r in d["releases"] if r["hash"] == hash_)
print(d["base_url"] + "/" + rel["archive"])
')
    echo "downloading $URL"
    curl -fL --progress-bar "$URL" -o "$ROOT/flutter.zip" || echo "!! flutter download failed"
    rm -rf "$ROOT/flutter"
    unzip -q "$ROOT/flutter.zip" -d "$ROOT" && rm -f "$ROOT/flutter.zip"
fi
export PATH="$ROOT/flutter/bin:$PATH"
"$ROOT/flutter/bin/flutter" --version 2>&1 | head -4

# --------------------------------------------------------- 3. Android SDK
step "3/7  Android command-line tools"
CMDLINE="$SDK/cmdline-tools/latest/bin/sdkmanager"
if [ -x "$CMDLINE" ]; then
    echo "already installed"
else
    mkdir -p "$SDK/cmdline-tools"
    curl -fL --progress-bar \
        "https://dl.google.com/android/repository/commandlinetools-mac-${CMDLINE_TOOLS_VERSION}_latest.zip" \
        -o "$ROOT/cmdline-tools.zip" || echo "!! cmdline-tools download failed"
    rm -rf "$SDK/cmdline-tools/latest" "$SDK/cmdline-tools/cmdline-tools"
    unzip -q "$ROOT/cmdline-tools.zip" -d "$SDK/cmdline-tools" && rm -f "$ROOT/cmdline-tools.zip"
    mv "$SDK/cmdline-tools/cmdline-tools" "$SDK/cmdline-tools/latest"
fi
export ANDROID_HOME="$SDK"
export ANDROID_SDK_ROOT="$SDK"

step "4/7  Android SDK licences"
# `sdkmanager` refuses to install anything until every licence is accepted, and
# it asks interactively — hence the `yes`.
yes | "$CMDLINE" --sdk_root="$SDK" --licenses >/dev/null 2>&1
echo "licences accepted"

step "5/7  Platform, build tools and NDK (this is the large download)"
"$CMDLINE" --sdk_root="$SDK" \
    "platform-tools" \
    "platforms;android-${COMPILE_SDK}" \
    "build-tools;${BUILD_TOOLS}" \
    "ndk;${NDK_VERSION}" || echo "!! sdkmanager install failed"
"$CMDLINE" --sdk_root="$SDK" --list_installed 2>/dev/null | head -20

# --------------------------------------------------------- 6. Rust targets
step "6/7  Rust Android targets"
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
rustup target list --installed

# ------------------------------------------------------------ 7. Tooling
step "7/7  cargo-ndk and flutter_rust_bridge_codegen"
have cargo-ndk || cargo install cargo-ndk
have flutter_rust_bridge_codegen || cargo install flutter_rust_bridge_codegen

step "versions"
{
    echo "JAVA_HOME=$JAVA_HOME"
    "$JAVA_HOME/bin/java" -version 2>&1 | head -1
    "$ROOT/flutter/bin/flutter" --version 2>&1 | head -1
    echo "ANDROID_HOME=$SDK"
    ls "$SDK"
    echo "NDK: $(ls "$SDK/ndk" 2>/dev/null)"
    cargo ndk --version 2>&1 | head -1
    flutter_rust_bridge_codegen --version 2>&1 | head -1
} 2>&1

echo
echo "========== done =========="
