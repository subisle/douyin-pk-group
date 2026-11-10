#!/usr/bin/env bash
# 构建「每日数据」Android release APK（不依赖 Android Studio / Gradle）。
# 所有 SDK 工具、签名库与产物都落在仓库忽略目录，不会入库。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TOOLS="$ROOT/.android-tools"
BT="$TOOLS/bt35/android-15"
PLATFORM="$TOOLS/platform35/android-35/android.jar"
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
OUT_APK="$DIST/每日数据-v1.0-release.apk"

mkdir -p "$TOOLS" "$BUILD" "$DIST" "$KEY_DIR"

# 1) 准备 Android SDK build-tools 35 / platform 35（仅本地缓存，不入库）。
if [ ! -x "$AAPT2" ]; then
  curl -fL --retry 2 -o "$TOOLS/bt35.zip" "https://dl.google.com/android/repository/build-tools_r35_macosx.zip"
  unzip -q -o "$TOOLS/bt35.zip" -d "$TOOLS/bt35"
fi
if [ ! -f "$PLATFORM" ]; then
  curl -fL --retry 2 -o "$TOOLS/platform35.zip" "https://dl.google.com/android/repository/platform-35_r02.zip"
  unzip -q -o "$TOOLS/platform35.zip" -d "$TOOLS/platform35"
fi
chmod +x "$AAPT2" "$D8" "$ZIPALIGN" "$APKSIGNER"

# 2) 生成/复用长期 release 签名。口令只写在 600 权限的本地文件，不入库。
if [ ! -f "$KEY_ENV" ]; then
  umask 077
  python3 -c "import secrets,string; print('PK_KEYSTORE_PASSWORD=' + ''.join(secrets.choice(string.ascii_letters+string.digits) for _ in range(32))); print('PK_KEY_ALIAS=daily-release')" > "$KEY_ENV"
  chmod 600 "$KEY_ENV"
fi
# shellcheck disable=SC1090
source "$KEY_ENV"
: "${PK_KEYSTORE_PASSWORD:?missing PK_KEYSTORE_PASSWORD}"
: "${PK_KEY_ALIAS:=daily-release}"

if [ ! -f "$KEYSTORE" ]; then
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

# 3) aapt2 先编译资源并生成 R.java；再一次性编译 MainActivity + R.java -> DEX。
CLASSES="$BUILD/classes"
RES_FLAT="$BUILD/res-flat"
mkdir -p "$CLASSES" "$RES_FLAT" "$BUILD/gen" "$BUILD/obj-final"
"$AAPT2" compile --dir "$SRC/res" -o "$RES_FLAT"
"$AAPT2" link \
  -o "$BUILD/app-unsigned.apk" \
  -I "$PLATFORM" \
  --manifest "$SRC/AndroidManifest.xml" \
  --java "$BUILD/gen" \
  --min-sdk-version 26 \
  --target-sdk-version 35 \
  --version-code 1 \
  --version-name 1.0 \
  $(find "$RES_FLAT" -name '*.flat' | sort)

javac \
  -source 8 -target 8 -Xlint:-options \
  -bootclasspath "$PLATFORM" \
  -classpath "$PLATFORM" \
  -d "$CLASSES" \
  "$SRC/java/cc/subisle/daily/MainActivity.java" \
  $(find "$BUILD/gen" -name '*.java' | sort)
"$D8" --min-api 26 --lib "$PLATFORM" --output "$BUILD/obj-final" $(find "$CLASSES" -name '*.class' | sort)

UNSIGNED="$BUILD/app-unsigned.apk"
ALIGNED="$BUILD/app-aligned.apk"
(cd "$BUILD/obj-final" && zip -q -u "$UNSIGNED" classes.dex)
"$ZIPALIGN" -f -p 4 "$UNSIGNED" "$ALIGNED"

# 5) apksigner v2 + v3 签名，并验证。
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
shasum -a 256 "$OUT_APK" > "$OUT_APK.sha256"

# 6) 同步一份到 Web 端公开资源，Android 浏览器可直接下载安装。
# 这是 release 签名产物，不是签名私钥；换签名后需重新构建并重新发布。
mkdir -p "$WEB_ASSETS"
cp "$OUT_APK" "$WEB_ASSETS/daily-release.apk"
sed 's|  .*/每日数据-v1\.0-release\.apk$|  daily-release.apk|' "$OUT_APK.sha256" \
  > "$WEB_ASSETS/daily-release.apk.sha256"

printf '\nAPK: %s\n' "$OUT_APK"
printf 'SHA256: %s\n' "$(cat "$OUT_APK.sha256")"
printf '签名信息:\n'
keytool -list -v -keystore "$KEYSTORE" -storepass "$PK_KEYSTORE_PASSWORD" -alias "$PK_KEY_ALIAS" 2>/dev/null \
  | grep -E 'Alias|Owner|Valid from|SHA256:' | sed -n '1,12p'
