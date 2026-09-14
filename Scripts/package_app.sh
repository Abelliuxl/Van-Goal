#!/usr/bin/env bash
# Package the release binary into a local HermitGPUI.app bundle.
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

APP_NAME="HermitGPUI"
BUNDLE_ID="com.abelliuxl.HermitGPUI"
VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
BUILD_DIR="Build"
APP_DIR="$BUILD_DIR/$APP_NAME.app"
CONTENTS="$APP_DIR/Contents"

echo "Building release binary..."
cargo build --release

echo "Creating $APP_DIR..."
rm -rf "$APP_DIR"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"

cp target/release/$APP_NAME "$CONTENTS/MacOS/$APP_NAME"

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
    <string>Hermit</string>
    <key>CFBundleDisplayName</key>
    <string>Hermit</string>
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
