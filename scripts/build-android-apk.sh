#!/usr/bin/env bash
# 构建「每日数据」Android release APK（不依赖 Android Studio / Gradle）。
# SDK 工具必须已缓存在 .android-tools/；本脚本不会联网下载 SDK。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

INIT_KEYSTORE=0
SYNC_WEB=1
for arg in "$@"; do
  case "$arg" in
    --init-keystore) INIT_KEYSTORE=1 ;;
    --no-web-sync) SYNC_WEB=0 ;;
    -h|--help)
      printf '用法: %s [--init-keystore] [--no-web-sync]\n' "$0"
      printf '  --init-keystore  仅在本机缺少签名时显式创建长期 release 签名\n'
      printf '  --no-web-sync    只构建到 android/dist，不更新服务端嵌入 APK（本地校验用）\n'
      exit 0
      ;;
    *) printf '未知参数: %s\n' "$arg" >&2; exit 2 ;;
  esac
done

TOOLS="$ROOT/.android-tools"
BT="$TOOLS/bt35/android-15"
PLATFORM="$TOOLS/platform35/android-35/android.jar"
AAPT="$BT/aapt"
AAPT2="$BT/aapt2"
D8="$BT/d8"
ZIPALIGN="$BT/zipalign"
APKSIGNER="$BT/apksigner"

APP="$ROOT/android"
SRC="$APP/app/src/main"
WEB_ASSETS="$ROOT/rust/crates/pk-server/src/public_teams/assets/pwa"
BUILD="$APP/build"
DIST="$APP/dist"
KEY_DIR="$APP/keystore"
KEYSTORE="$KEY_DIR/daily-release.jks"
KEY_ENV="$KEY_DIR/keystore.env"
OUT_APK="$DIST/每日数据-v1.2-release.apk"

die() { printf '错误: %s\n' "$*" >&2; exit 1; }
require_cmd() { command -v "$1" >/dev/null 2>&1 || die "缺少命令 $1"; }
require_exec() { [ -x "$1" ] || die "缺少可执行工具 $1；请先准备本地 Android 工具链，脚本不会联网下载"; }

for cmd in java javac keytool zip python3 shasum grep sed find sort cmp; do
  require_cmd "$cmd"
done
for tool in "$AAPT" "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"; do
  require_exec "$tool"
done
[ -f "$PLATFORM" ] || die "缺少本地 Android platform: $PLATFORM"

# 0) 先跑无需设备的导航/版本一致性回归。
"$ROOT/scripts/test-android-navigation.sh"

# 版本契约由现有导航测试同时钉住；实际 aapt 参数仍保持可被该测试识别的字面值。
python3 - "$SRC/AndroidManifest.xml" <<'PY'
from pathlib import Path
import re
import sys
manifest = Path(sys.argv[1]).read_text()
code = re.search(r'android:versionCode="(\d+)"', manifest)
name = re.search(r'android:versionName="([^"]+)"', manifest)
if not code or not name:
    raise SystemExit('Manifest 缺少 versionCode/versionName')
if code.group(1) != '3' or name.group(1) != '1.2':
    raise SystemExit(f'版本与构建脚本不一致: {name.group(1)} ({code.group(1)})')
print(f'Android version: {name.group(1)} ({code.group(1)})')
PY

# 1) APP_URL 默认值放资源，构建时可用 PK_APP_URL 覆盖；必须为 HTTP(S) 并规范为目录地址。
DEFAULT_APP_URL="$(python3 - "$SRC/res/values/strings.xml" <<'PY'
from html import unescape
from pathlib import Path
import re
import sys
text = Path(sys.argv[1]).read_text()
match = re.search(r'<string name="app_url"[^>]*>(.*?)</string>', text, re.S)
if not match:
    raise SystemExit('strings.xml 缺少 app_url')
print(unescape(match.group(1).strip()))
PY
)"
APP_URL="${PK_APP_URL:-$DEFAULT_APP_URL}"
APP_URL="$(python3 - "$APP_URL" <<'PY'
from urllib.parse import urlsplit, urlunsplit
import sys
raw = sys.argv[1].strip()
parts = urlsplit(raw)
if parts.scheme.lower() not in ('http', 'https') or not parts.hostname:
    raise SystemExit('PK_APP_URL 必须是完整的 http:// 或 https:// 地址')
if parts.username or parts.password:
    raise SystemExit('PK_APP_URL 不能包含用户名或密码')
if parts.query or parts.fragment:
    raise SystemExit('PK_APP_URL 不能包含 query 或 fragment')
path = parts.path or '/'
if not path.endswith('/'):
    path += '/'
print(urlunsplit((parts.scheme.lower(), parts.netloc, path, '', '')))
PY
)"
APP_SCHEME="$(python3 -c 'from urllib.parse import urlsplit; import sys; print(urlsplit(sys.argv[1]).scheme)' "$APP_URL")"
APP_HOST="$(python3 -c 'from urllib.parse import urlsplit; import sys; print(urlsplit(sys.argv[1]).hostname)' "$APP_URL")"
printf 'APP_URL: %s\n' "$APP_URL"

# 2) 默认绝不在缺签名时自动生成；只有 --init-keystore 才允许初始化长期签名。
mkdir -p "$KEY_DIR"
if [ "$INIT_KEYSTORE" -eq 1 ]; then
  if [ -f "$KEYSTORE" ] && [ ! -f "$KEY_ENV" ]; then
    die "已有 JKS 但缺少 keystore.env，无法恢复原口令；请找回原口令，不要生成新签名"
  fi
  if [ ! -f "$KEY_ENV" ]; then
    umask 077
    python3 -c "import secrets,string; print('PK_KEYSTORE_PASSWORD=' + ''.join(secrets.choice(string.ascii_letters+string.digits) for _ in range(32))); print('PK_KEY_ALIAS=daily-release')" > "$KEY_ENV"
    chmod 600 "$KEY_ENV"
  fi
else
  [ -f "$KEY_ENV" ] || die "缺少 $KEY_ENV；如确认首次初始化签名，请显式使用 --init-keystore"
  [ -f "$KEYSTORE" ] || die "缺少 $KEYSTORE；如确认首次初始化签名，请显式使用 --init-keystore"
fi

# shellcheck disable=SC1090
source "$KEY_ENV"
: "${PK_KEYSTORE_PASSWORD:?missing PK_KEYSTORE_PASSWORD}"
: "${PK_KEY_ALIAS:=daily-release}"

if [ ! -f "$KEYSTORE" ]; then
  [ "$INIT_KEYSTORE" -eq 1 ] || die "缺少 release JKS"
  keytool -genkeypair \
    -alias "$PK_KEY_ALIAS" \
    -keyalg RSA -keysize 3072 -sigalg SHA256withRSA \
    -validity 10950 \
    -dname "CN=每日数据, OU=Subisle, O=Subisle, L=Hefei, ST=Anhui, C=CN" \
    -keystore "$KEYSTORE" \
    -storepass "$PK_KEYSTORE_PASSWORD" \
    -keypass "$PK_KEYSTORE_PASSWORD" \
    -storetype PKCS12 >/dev/null
fi
chmod 600 "$KEYSTORE" "$KEY_ENV"

# 3) 每次从空目录构建，避免旧 class、旧资源或已删除源码残留进 APK。
mkdir -p "$BUILD" "$DIST"
rm -rf "$BUILD/classes" "$BUILD/res-flat" "$BUILD/gen" "$BUILD/obj-final" "$BUILD/res"
mkdir -p "$BUILD/classes" "$BUILD/res-flat" "$BUILD/gen" "$BUILD/obj-final" "$BUILD/res"
cp -R "$SRC/res/." "$BUILD/res/"

# 将构建配置写入临时资源，不在源码里改动本地默认地址。
python3 - "$BUILD/res/values/strings.xml" "$APP_URL" <<'PY'
from html import escape
from pathlib import Path
import re
import sys
path = Path(sys.argv[1])
text = path.read_text()
text, count = re.subn(
    r'(<string name="app_url"[^>]*>).*?(</string>)',
    lambda m: m.group(1) + escape(sys.argv[2]) + m.group(2),
    text,
    count=1,
    flags=re.S,
)
if count != 1:
    raise SystemExit('无法写入构建期 app_url')
path.write_text(text)
PY

python3 - "$BUILD/res/xml/network_security_config.xml" "$APP_SCHEME" "$APP_HOST" <<'PY'
from html import escape
from pathlib import Path
import sys
path, scheme, host = Path(sys.argv[1]), sys.argv[2].lower(), sys.argv[3]
domain = ''
if scheme == 'http':
    domain = f'''\n    <domain-config cleartextTrafficPermitted="true">\n        <domain includeSubdomains="false">{escape(host)}</domain>\n    </domain-config>'''
path.write_text(f'''<?xml version="1.0" encoding="utf-8"?>
<network-security-config>
    <!-- 默认禁止明文流量；仅为构建配置中的 HTTP 入口保留窄例外。 -->
    <base-config cleartextTrafficPermitted="false">
        <trust-anchors>
            <certificates src="system" />
        </trust-anchors>
    </base-config>{domain}
</network-security-config>
''')
PY

# 4) 编译资源并以干净的 classes 目录生成 DEX。
"$AAPT2" compile --dir "$BUILD/res" -o "$BUILD/res-flat"
"$AAPT2" link \
  -o "$BUILD/app-unsigned.apk" \
  -I "$PLATFORM" \
  --manifest "$SRC/AndroidManifest.xml" \
  --java "$BUILD/gen" \
  --min-sdk-version 26 \
  --target-sdk-version 35 \
  --version-code 3 \
  --version-name 1.2 \
  $(find "$BUILD/res-flat" -name '*.flat' | sort)

javac \
  -source 8 -target 8 -Xlint:-options \
  -bootclasspath "$PLATFORM" \
  -classpath "$PLATFORM" \
  -d "$BUILD/classes" \
  "$SRC/java/cc/subisle/daily/MainActivity.java" \
  "$SRC/java/cc/subisle/daily/NavigationPolicy.java" \
  "$SRC/java/cc/subisle/daily/AppUpdateChecker.java" \
  $(find "$BUILD/gen" -name '*.java' | sort)
"$D8" --min-api 26 --lib "$PLATFORM" --output "$BUILD/obj-final" \
  $(find "$BUILD/classes" -name '*.class' | sort)

UNSIGNED="$BUILD/app-unsigned.apk"
ALIGNED="$BUILD/app-aligned.apk"
(cd "$BUILD/obj-final" && zip -q -u "$UNSIGNED" classes.dex)
"$ZIPALIGN" -f -p 4 "$UNSIGNED" "$ALIGNED"
"$ZIPALIGN" -c -p 4 "$ALIGNED"

# 5) 签名后分别用 aapt 和 apksigner 校验实际产物，而不只校验源码参数。
cp "$ALIGNED" "$OUT_APK"
"$APKSIGNER" sign \
  --ks "$KEYSTORE" \
  --ks-key-alias "$PK_KEY_ALIAS" \
  --ks-pass "pass:$PK_KEYSTORE_PASSWORD" \
  --key-pass "pass:$PK_KEYSTORE_PASSWORD" \
  --v1-signing-enabled true \
  --v2-signing-enabled true \
  --v3-signing-enabled true \
  --min-sdk-version 26 \
  "$OUT_APK"
"$APKSIGNER" verify --verbose --min-sdk-version 26 "$OUT_APK" > "$DIST/apksigner-verify.txt"
"$AAPT" dump badging "$OUT_APK" > "$DIST/aapt-badging.txt"

python3 - "$DIST/aapt-badging.txt" <<'PY'
from pathlib import Path
import re
import sys
text = Path(sys.argv[1]).read_text()
code = re.search(r"versionCode='([^']+)'", text)
name = re.search(r"versionName='([^']+)'", text)
if not code or code.group(1) != '3':
    raise SystemExit(f'aapt versionCode 校验失败: {code.group(1) if code else "missing"}')
if not name or name.group(1) != '1.2':
    raise SystemExit(f'aapt versionName 校验失败: {name.group(1) if name else "missing"}')
print(f'aapt verified: versionCode={code.group(1)} versionName={name.group(1)}')
PY

shasum -a 256 "$OUT_APK" > "$OUT_APK.sha256"
OUT_SHA="$(shasum -a 256 "$OUT_APK" | awk '{print $1}')"

# 6) 默认同步一份到展示端嵌入资源，并逐字节校验 dist 与嵌入 APK 一致。
if [ "$SYNC_WEB" -eq 1 ]; then
  mkdir -p "$WEB_ASSETS"
  cp "$OUT_APK" "$WEB_ASSETS/daily-release.apk"
  printf '%s  daily-release.apk\n' "$OUT_SHA" > "$WEB_ASSETS/daily-release.apk.sha256"
  cmp -s "$OUT_APK" "$WEB_ASSETS/daily-release.apk" \
    || die "嵌入 APK 与 android/dist 产物不一致"
else
  printf '跳过 Web 嵌入同步（--no-web-sync）\n'
fi

printf '\nAPK: %s\n' "$OUT_APK"
printf 'SHA256: %s\n' "$OUT_SHA"
printf '签名信息:\n'
keytool -list -v -keystore "$KEYSTORE" -storepass "$PK_KEYSTORE_PASSWORD" -alias "$PK_KEY_ALIAS" 2>/dev/null \
  | grep -E 'Alias|Owner|Valid from|SHA256:' | sed -n '1,12p'
