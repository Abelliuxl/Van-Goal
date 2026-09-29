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
FLUTTER_TARGETS=()
for abi in "${ABIS[@]}"; do
    TARGET_FLAGS+=(-t "$abi")
    case "$abi" in
        arm64-v8a) FLUTTER_TARGETS+=(android-arm64) ;;
        armeabi-v7a) FLUTTER_TARGETS+=(android-arm) ;;
        x86_64) FLUTTER_TARGETS+=(android-x64) ;;
        *) echo "Unsupported Android ABI: $abi" >&2; exit 1 ;;
    esac
done

echo "==> Cross-compiling van-goal-mobile for: ${ABIS[*]}"
cargo ndk "${TARGET_FLAGS[@]}" -o "$JNI_LIBS" build --release -p van-goal-mobile

echo
echo "==> Packaging the APK"
cd app
flutter build apk --release --split-per-abi \
    --target-platform "$(IFS=,; echo "${FLUTTER_TARGETS[*]}")"

echo
echo "==> Done"
for abi in "${ABIS[@]}"; do
    ls -lh "build/app/outputs/flutter-apk/app-${abi}-release.apk"
done
