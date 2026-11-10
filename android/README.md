# 每日数据 Android APK

一个极小的 Android WebView 壳，固定访问当前线上地址：

```text
http://43.142.30.113:81/
```

设计上尽量降低安装风险提示：

- release APK 使用独立长期签名（RSA 3072 / SHA-256，30 年有效期）。
- 只声明 `android.permission.INTERNET`，没有短信、通讯录、定位、存储、相机、电话、通知、后台服务等敏感权限。
- 没有第三方 SDK、广告、统计、推送、动态加载、反射调用、原生库或可执行 dex 下载。
- 只加载一个固定入口；`shouldOverrideUrlLoading` 会拦截非本站 HTTP/HTTPS、`tel:`、`sms:`、`mailto:`、`intent:`、`javascript:` 等外部 URL。
- 禁用文件访问、任意内容 URL、数据库、DOM storage 以外的本地存储接口通过 WebView 配置收紧；登录态所需的 `localStorage` 开启。
- `minSdk 26` / `targetSdk 35`，不申请 `QUERY_ALL_PACKAGES`，导出组件只有启动 Activity。
- 未使用调试证书签名；`android:debuggable` 由构建命令显式设为 false。

> 说明：Android 对“未知来源 / 未识别开发者”的提示由系统、应用商店或 Play Protect 决定，APK 本身不能绕过。能做的是保持签名稳定、权限最小、代码透明、站点固定；后续如果接入正式域名 HTTPS 与应用商店分发，提示会进一步减少。

## 构建

在本仓库根目录执行：

```bash
./scripts/build-android-apk.sh
```

脚本会：

1. 在 `.android-tools/` 下准备 Android SDK build-tools 35 与 platform 35（不入库）。
2. 在 `android/keystore/daily-release.jks` 下生成/复用 release 签名（不入库）。
3. 用 Java 源码编译出极小 classes.dex，再用 aapt2 / zipalign / apksigner 产出：

```text
android/dist/每日数据-v1.0-release.apk
android/dist/每日数据-v1.0-release.apk.sha256
```

签名口令保存在 `android/keystore/keystore.env`（权限 600，不入库）。请离线妥善保管这个文件和 JKS；更换签名会导致用户必须卸载旧版才能升级。

## 线上地址切换

修改 `android/app/src/main/java/cc/subisle/daily/MainActivity.java` 的 `APP_URL`，重新构建即可。
后续建议把 43 的 HTTP 入口换成已备案域名的 HTTPS，并同步把 `APP_URL` 改成 HTTPS；届时可以把网络安全配置里的明文例外彻底去掉。

## 展示端平台推荐

Web 展示页已内置平台分流：

- 微信 / QQ / 抖音 / 通用 Android WebView 等内置浏览器：顶部常驻「请在浏览器中打开」，完整引导弹层提示切换系统浏览器后下载 App / 添加 App 到主屏幕，并提供复制当前地址。
- iOS Safari：推荐「分享 → 添加到主屏幕」，把每日数据作为 App 使用。
- Android 浏览器：推荐下载同源 `/download/daily.apk` 的「每日数据 App」；同时提供 `/download/daily.apk.sha256` 校验文件。
- Android App 内 WebView：UA 追加 `DailyDataAndroid/1.0`，页面识别为已安装，永久关闭安装引导。
- 桌面浏览器：不弹安装推荐，避免干扰。

`scripts/build-android-apk.sh` 会把签名后的 APK 与校验文件同步到 `rust/crates/pk-server/src/public_teams/assets/pwa/`，由 Rust 服务作为同源公开资源下发。重新构建后需重新发布服务端代码，线上下载入口才会更新。

## 安装提示说明

首次从浏览器、微信或文件管理器安装时，Android 通常会提示“未知来源应用 / 需要允许安装”。这是系统安全机制，不是 APK 有风险。让用户允许当前来源安装即可；后续同签名升级不会再出现签名冲突。若仍被 Play Protect 拦截，选择“仍要安装 / 更多信息”属于用户侧确认，应用本身不依赖任何绕过逻辑。
