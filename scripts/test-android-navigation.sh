#!/usr/bin/env bash
# 在宿主机验证 APK 链接路由，不依赖 Android 设备或 Gradle。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$(mktemp -d "${TMPDIR:-/tmp}/pk-android-navigation.XXXXXX")"
trap 'rm -rf "$OUT"' EXIT

javac -source 8 -target 8 -Xlint:-options -d "$OUT" \
  "$ROOT/android/app/src/main/java/cc/subisle/daily/NavigationPolicy.java" \
  "$ROOT/android/app/src/test/java/cc/subisle/daily/NavigationPolicyTest.java"
java -cp "$OUT" cc.subisle.daily.NavigationPolicyTest
