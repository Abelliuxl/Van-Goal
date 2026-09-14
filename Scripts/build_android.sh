#!/usr/bin/env bash
# Build the Android APK: cross-compile the Rust client into the app's jniLibs,
# then let Flutter package it together with the Dart UI.
#
#     Scripts/build_android.sh                 # arm64 only (every modern phone)
#     VAN_GOAL_ABIS="arm64-v8a armeabi-v7a x86_64" Scripts/build_android.sh
#
# Requires the toolchain from Scripts/setup_android_toolchain.sh.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=android_env.sh
source Scripts/android_env.sh

# arm64-v8a covers every phone sold for years; the others are only worth the
# extra compile when you need an old device or an emulator.
read -r -a ABIS <<<"${VAN_GOAL_ABIS:-arm64-v8a}"
JNI_LIBS="app/android/app/src/main/jniLibs"

TARGET_FLAGS=()
for abi in "${ABIS[@]}"; do
    TARGET_FLAGS+=(-t "$abi")
done

echo "==> Cross-compiling van-goal-mobile for: ${ABIS[*]}"
cargo ndk "${TARGET_FLAGS[@]}" -o "$JNI_LIBS" build --release -p van-goal-mobile

echo
echo "==> Packaging the APK"
cd app
flutter build apk --release

echo
echo "==> Done"
ls -lh build/app/outputs/flutter-apk/*.apk
