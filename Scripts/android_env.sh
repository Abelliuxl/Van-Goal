#!/usr/bin/env bash
# Environment for building the Android app. Source it, do not run it:
#
#     source Scripts/android_env.sh
#
# Everything lives under the user's own directories, installed by
# `Scripts/setup_android_toolchain.sh` — no Homebrew, no sudo.
#
# Override FLUTTER_HOME to use a Flutter installed somewhere else.

export JAVA_HOME="${JAVA_HOME:-$HOME/development/jdk21}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.0.12077973"
export FLUTTER_HOME="${FLUTTER_HOME:-$HOME/development/flutter}"

export PATH="$FLUTTER_HOME/bin:$JAVA_HOME/bin:$ANDROID_HOME/platform-tools:$PATH"

# The Flutter and pub hosts are slow to unreachable from here; these are the
# Flutter team's own mirrors, and they are several times faster.
export FLUTTER_STORAGE_BASE_URL="${FLUTTER_STORAGE_BASE_URL:-https://storage.flutter-io.cn}"
export PUB_HOSTED_URL="${PUB_HOSTED_URL:-https://pub.flutter-io.cn}"

# Where the Rust side is cross-compiled from, and which ABIs ship.
export VAN_GOAL_RUST_TARGETS="${VAN_GOAL_RUST_TARGETS:-arm64-v8a armeabi-v7a x86_64}"
