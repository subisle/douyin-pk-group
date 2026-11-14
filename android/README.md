# 每日数据 Android APK

一个轻量 Android WebView 壳。应用只主动打开构建时配置的展示端地址，登录态保存在 WebView 的 Cookies / localStorage 中。

当前版本：`1.3`（versionCode `4`），`minSdk 26` / `targetSdk 35`。

## 稳定性与安全

- release APK 使用独立长期签名（RSA 3072 / SHA-256，30 年有效期）。
- 只声明 `android.permission.INTERNET`，没有短信、通讯录、定位、存储、相机、电话、通知、后台服务等权限。
- 没有第三方 SDK、广告、统计、推送、动态加载、反射调用、原生库或可执行 dex 下载。
- 只主动加载 `app_url` 对应站点；用户点击的外站 HTTP/HTTPS 交给系统浏览器；无点击的外站跳转及 `tel:`、`sms:`、`mailto:`、`intent:`、`javascript:` 等协议继续拦截。
- 不暴露 `addJavascriptInterface`；`dailyapp://check-update` 只作为页面按钮调用原生更新检查的私有入口，必须带用户点击。
- 禁用文件访问、任意内容 URL 和数据库；保留登录所需的 DOM storage / Cookies。
- 主页面网络错误、HTTP 4xx/5xx、SSL 校验失败都会显示中文错误页和「重新加载」按钮，不再白屏。
- WebView 渲染进程被系统回收时自动重建并重新加载；短时间连续失败后提示关闭其他应用后重试。
- 启动页不会在进度 60% 时提前消失，而是在页面可见或加载完成时退出；异常时显示可重试错误页。
- Android 12+ 使用系统 SplashScreen，Android 8–11 使用统一的 `windowBackground` 启动画面。

## 构建前提

构建不依赖 Android Studio / Gradle，也不会联网下载 Android SDK。需要本机已经准备：

```text
.android-tools/bt35/android-15/{aapt,aapt2,d8,zipalign,apksigner}
.android-tools/platform35/android-35/android.jar
android/keystore/daily-release.jks
android/keystore/keystore.env
```

另外需要 `java`、`javac`、`keytool`、`python3`、`zip`、`shasum`。缺少工具时构建脚本会直接报错退出，不会自动下载大 SDK。

## 构建

在仓库根目录执行：

```bash
./scripts/build-android-apk.sh
```

脚本会：

1. 运行无需设备的导航与版本一致性测试。
2. 校验原生工具链和 release JKS。
3. 清理旧 `classes`、`res-flat`、`gen`、`obj-final` 后重新编译，避免旧文件残留。
4. 使用 Manifest 中的版本契约构建、对齐并签名 APK。
5. 用 `aapt dump badging` 校验实际产物版本，用 `apksigner verify` 校验 v2/v3 签名。
6. 生成：

```text
android/dist/每日数据-v1.3-release.apk
android/dist/每日数据-v1.3-release.apk.sha256
android/dist/aapt-badging.txt
android/dist/apksigner-verify.txt
```

7. 默认把 APK 同步到服务端嵌入资源，并逐字节确认嵌入 APK 与 `android/dist` 产物一致。

只做本地构建、不更新服务端嵌入资源时：

```bash
./scripts/build-android-apk.sh --no-web-sync
```

## 签名保护

默认构建时如果缺少 `daily-release.jks` 或 `keystore.env`，脚本会失败，绝不自动生成新签名。

只有首次初始化签名时才允许：

```bash
./scripts/build-android-apk.sh --init-keystore
```

如果已有 JKS 但口令文件丢失，脚本仍会拒绝生成新签名，因为换签名会导致已安装用户无法覆盖升级。请从离线备份找回 `keystore.env` 和 JKS。

签名口令只保存在权限 `600` 的 `android/keystore/keystore.env`，不得入库或发送到远端。

## 配置 APP_URL

本地默认地址只保存在：

```text
android/app/src/main/res/values/strings.xml  ->  app_url
```

构建时可覆盖，无需改 Java：

```bash
PK_APP_URL='https://example.com/' ./scripts/build-android-apk.sh
```

`PK_APP_URL` 必须是完整的 HTTP(S) 地址，不能带用户名、密码、query 或 fragment。构建脚本会把最终地址写入临时资源，并为 HTTP 地址生成仅限该域名的明文例外；HTTPS 不保留明文例外。

正式分发建议使用已备案域名的 HTTPS。当前网络配置仍为安卓端本地/旧服务器的 HTTP 入口保留单一域名例外。

## 安装与应用内更新

- 启动及回到前台时会检查 `/api/public/app-version`。
- 「我的 → 检查软件更新」可手动触发。
- 更新通过系统浏览器下载；同签名升级会保留登录数据。
- UA 追加 `DailyDataAndroid/1.3`，展示端据此识别 Android App 并关闭安装引导。

## 展示端平台推荐

- iOS Safari：推荐「分享 → 添加到主屏幕」，把每日数据作为 App 使用。
- Android 浏览器：推荐下载同源 `/download/daily.apk`，同时提供 `.sha256` 校验文件。
- Android App 内 WebView：UA 识别为已安装，不再显示安装引导。
- 微信、抖音等内置浏览器：提示切换系统浏览器，并提供复制当前地址。
- 桌面浏览器：不弹安装推荐。

## 安装提示

首次从浏览器、微信或文件管理器安装时，Android 通常会提示“未知来源应用 / 需要允许安装”。这是系统安全机制，不是 APK 有风险。让用户允许当前来源安装即可；后续同签名升级不会再出现签名冲突。若被 Play Protect 拦截，选择“仍要安装 / 更多信息”属于用户侧确认。

## 旧设备说明

本 Activity 已限制为单一 WebView、无原生后台服务，并在页面进入后台时暂停 WebView 定时器。网络异常、低内存导致渲染进程回收、WebView 页面提交延迟时均有明确状态或恢复路径。Android 8/9 低内存设备仍建议在真机上验证冷启动、断网重试和前后台切换。
