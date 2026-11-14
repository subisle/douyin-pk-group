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

# 版本分散在 Manifest、UA、构建参数和公开接口中；构建前钉死一致性，避免更新提示误报。
python3 - "$ROOT" <<'PY'
from pathlib import Path
import re
import sys

root = Path(sys.argv[1])
manifest = (root / "android/app/src/main/AndroidManifest.xml").read_text()
activity = (root / "android/app/src/main/java/cc/subisle/daily/MainActivity.java").read_text()
build = (root / "scripts/build-android-apk.sh").read_text()
pages = (root / "rust/crates/pk-server/src/public_teams/pages.rs").read_text()

def one(pattern, text, label):
    match = re.search(pattern, text)
    if not match:
        raise SystemExit(f"找不到 {label} 版本配置")
    return match.group(1)

code = one(r'android:versionCode="(\d+)"', manifest, "Manifest")
name = one(r'android:versionName="([^"]+)"', manifest, "Manifest")
checks = [
    (one(r'--version-code (\d+)', build, "构建脚本"), code, "versionCode 构建参数"),
    (one(r'--version-name ([^\s]+)', build, "构建脚本"), name, "versionName 构建参数"),
    (one(r'DailyDataAndroid/([0-9.]+)', activity, "APK UA"), name, "APK UA"),
    (one(r'const ANDROID_VERSION_CODE: u64 = (\d+)', pages, "公开接口"), code, "公开接口 versionCode"),
    (one(r'const ANDROID_VERSION_NAME: &str = "([^"]+)"', pages, "公开接口"), name, "公开接口 versionName"),
]
for actual, expected, label in checks:
    if actual != expected:
        raise SystemExit(f"{label} 不一致：{actual} != {expected}")
if f"每日数据-v{name}-release.apk" not in build:
    raise SystemExit("APK 输出文件名与 versionName 不一致")
print(f"Android version alignment: {name} ({code})")
PY
