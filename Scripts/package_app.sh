#!/usr/bin/env bash
# Package the desktop app into a local VanGoal.app bundle: the Rust core as a
# dylib (crates/desktop) and the MyGo window as a Go binary (desktop/).
set -euo pipefail

cd "$(dirname "$0")/.."

# Make cargo available when launched outside a Rust-enabled shell.
if [ -d "$HOME/.cargo/bin" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

# Some shells do not export the active macOS SDK; the C parts of the
# dependency tree need it.
if [ -z "${SDKROOT:-}" ]; then
    export SDKROOT="$(xcrun --show-sdk-path)"
fi

APP_NAME="VanGoal"
BUNDLE_ID="com.abelliuxl.VanGoal"
# This is a Cargo workspace, so `cargo metadata` lists every member and the
# order is not guaranteed — ask for the desktop package by name instead of
# taking packages[0], which would silently stamp the wrong version (or fail).
# `/usr/local/bin/python3` may be an Intel-only Homebrew shim on Apple Silicon.
# The system interpreter is sufficient for this small JSON query and avoids a
# packaging failure before the release build even starts.
PYTHON_BIN="${PYTHON_BIN:-/usr/bin/python3}"
VERSION="$(cargo metadata --no-deps --format-version 1 | "$PYTHON_BIN" -c '
import json, sys
packages = json.load(sys.stdin)["packages"]
app = next(p for p in packages if p["name"] == "van-goal-desktop")
print(app["version"])
')"
BUILD_DIR="Build"
APP_DIR="$BUILD_DIR/$APP_NAME.app"
CONTENTS="$APP_DIR/Contents"

echo "Building the core..."
cargo build --release -p van-goal-desktop

# MyGo needs Go 1.27 and no cgo. An older Go fetches the right toolchain.
GO_BIN="$(command -v go || true)"
if [ -z "$GO_BIN" ] && [ -x /opt/homebrew/bin/go ]; then GO_BIN=/opt/homebrew/bin/go; fi
if [ -z "$GO_BIN" ]; then GO_BIN="$(ls -d /opt/homebrew/Cellar/go/*/bin/go 2>/dev/null | tail -1)"; fi
if [ -z "$GO_BIN" ]; then echo "Go is not installed (https://go.dev/dl/)" >&2; exit 1; fi
export GOTOOLCHAIN="${GOTOOLCHAIN:-go1.27.3}" CGO_ENABLED=0

echo "Building the window..."
(cd desktop && "$GO_BIN" build -trimpath -tags mygo_noinspector \
    -ldflags "-s -w -X github.com/egoist/mygo.production=1" -o "../target/release/$APP_NAME" .)

echo "Creating $APP_DIR..."
rm -rf "$APP_DIR"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"

cp target/release/$APP_NAME "$CONTENTS/MacOS/$APP_NAME"
cp target/release/libvan_goal_desktop.dylib "$CONTENTS/Resources/"

# Compile the macOS app icon and asset catalog into the bundle.
xcrun actool \
    --compile "$CONTENTS/Resources" \
    --platform macosx \
    --minimum-deployment-target 12.0 \
    --app-icon AppIcon \
    --output-partial-info-plist "$BUILD_DIR/AppIcon-Info.plist" \
    assets/Assets.xcassets

cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Van-Goal</string>
    <key>CFBundleDisplayName</key>
    <string>Van-Goal</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIconName</key>
    <string>AppIcon</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHumanReadableCopyright</key>
    <string>MIT License</string>
</dict>
</plist>
PLIST

# Ad-hoc signature so Gatekeeper's local checks pass.
codesign --force --deep --sign - "$APP_DIR" || true

echo "Packaged: $APP_DIR (version $VERSION)"
