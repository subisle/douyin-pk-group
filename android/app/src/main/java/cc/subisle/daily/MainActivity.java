package cc.subisle.daily;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.graphics.Color;
import android.net.Uri;
import android.net.http.SslError;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.CookieManager;
import android.webkit.RenderProcessGoneDetail;
import android.webkit.SslErrorHandler;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

/**
 * 每日数据轻量版：固定站点 WebView 壳。
 *
 * 安全约束：
 * - 只主动加载构建时配置的站点；
 * - 本站 HTTP(S) 留在 WebView；用户点击的外站 HTTP(S) 交给系统浏览器；
 * - 无点击的外站跳转与非 HTTP(S) scheme 拦截（内部更新指令除外）；
 * - 不启用文件访问、通用内容访问、自动文件下载；
 * - 不暴露 addJavascriptInterface；
 * - 仅保留登录态所需的 DOM storage / Cookies。
 */
public final class MainActivity extends Activity {
    private static final String APK_UA_TOKEN = "DailyDataAndroid/1.2";
    private static final int SPLASH_TIMEOUT_MS = 12_000;
    private static final int RENDERER_RECOVERY_WINDOW_MS = 60_000;
    private static final int MAX_RENDERER_RECOVERIES = 3;

    private FrameLayout root;
    private WebView web;
    private View splash;
    private TextView splashText;
    private View errorView;
    private TextView errorTitle;
    private TextView errorDetail;
    private Handler mainHandler;
    private AppUpdateChecker updateChecker;
    private Runnable updateCheckRunnable;
    private String appUrl;
    private boolean loadFailed;
    private boolean pageCommitted;
    private int rendererRecoveries;
    private long lastRendererRecoveryAt;

    private final Runnable splashTimeout = new Runnable() {
        @Override
        public void run() {
            if (pageCommitted || loadFailed) return;
            showLoadError(
                    "页面加载时间较长",
                    "网络可能较慢，页面仍在尝试加载。请检查网络后点击“重新加载”。");
        }
    };

    @Override
    @SuppressLint("SetJavaScriptEnabled")
    protected void onCreate(Bundle savedInstanceState) {
        setTheme(R.style.AppTheme);
        super.onCreate(savedInstanceState);
        mainHandler = new Handler(Looper.getMainLooper());
        appUrl = getString(R.string.app_url);
        NavigationPolicy.configure(appUrl);
        updateChecker = new AppUpdateChecker(this, appUrl);

        root = new FrameLayout(this);
        root.setBackgroundColor(getResources().getColor(R.color.background, getTheme()));
        setContentView(root);

        installWebView();
        createErrorView();
        showSplash("正在连接每日数据…");

        if (savedInstanceState == null) {
            loadApp(false);
        } else {
            web.restoreState(savedInstanceState);
            if (web.getUrl() == null || !NavigationPolicy.isAppUrl(web.getUrl())) {
                loadApp(false);
            } else {
                mainHandler.postDelayed(splashTimeout, SPLASH_TIMEOUT_MS);
            }
        }
    }

    private void installWebView() {
        WebView view = new WebView(this);
        view.setId(View.generateViewId());
        view.setLayoutParams(new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));
        view.setBackgroundColor(getResources().getColor(R.color.background, getTheme()));

        WebSettings settings = view.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setJavaScriptCanOpenWindowsAutomatically(false);
        settings.setDomStorageEnabled(true);
        settings.setDatabaseEnabled(false);
        settings.setAllowFileAccess(false);
        settings.setAllowContentAccess(false);
        settings.setAllowFileAccessFromFileURLs(false);
        settings.setAllowUniversalAccessFromFileURLs(false);
        settings.setLoadWithOverviewMode(false);
        settings.setUseWideViewPort(false);
        settings.setSupportZoom(false);
        settings.setBuiltInZoomControls(false);
        settings.setDisplayZoomControls(false);
        settings.setCacheMode(WebSettings.LOAD_DEFAULT);
        settings.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);

        String ua = settings.getUserAgentString();
        String cleanUa = ua == null ? "" : ua;
        if (cleanUa.contains("; wv") || cleanUa.contains("MicroMessenger")) {
            cleanUa = cleanUa.replaceAll("(?i)MicroMessenger/[^ ]+", "")
                    .replace("; wv", "")
                    .replaceAll("\\s+", " ")
                    .trim();
        }
        settings.setUserAgentString(
                (cleanUa.isEmpty() ? "Mozilla/5.0" : cleanUa) + " " + APK_UA_TOKEN);

        CookieManager.getInstance().setAcceptCookie(true);
        CookieManager.getInstance().setAcceptThirdPartyCookies(view, false);

        view.setWebChromeClient(new WebChromeClient());

        view.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView current, WebResourceRequest request) {
                return handleNavigation(request.getUrl(), request.hasGesture());
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView current, String url) {
                return handleNavigation(Uri.parse(url), true);
            }

            @Override
            public void onPageStarted(WebView current, String url, android.graphics.Bitmap favicon) {
                super.onPageStarted(current, url, favicon);
                loadFailed = false;
                pageCommitted = false;
            }

            @Override
            public void onPageCommitVisible(WebView current, String url) {
                super.onPageCommitVisible(current, url);
                pageCommitted = true;
                loadFailed = false;
                hideSplash();
                clearLoadError();
            }

            @Override
            public void onPageFinished(WebView current, String url) {
                super.onPageFinished(current, url);
                if (loadFailed) return;
                pageCommitted = true;
                hideSplash();
                clearLoadError();
            }

            @Override
            public void onReceivedError(WebView current, WebResourceRequest request,
                                        WebResourceError error) {
                if (!request.isForMainFrame()) return;
                String description = error == null || error.getDescription() == null
                        ? "未知网络错误"
                        : error.getDescription().toString();
                loadFailed = true;
                showLoadError(
                        "无法连接服务器",
                        "请检查手机网络后重试。\n\n" + description);
            }

            @Override
            public void onReceivedHttpError(WebView current, WebResourceRequest request,
                                            WebResourceResponse response) {
                if (!request.isForMainFrame()) return;
                int status = response == null ? 0 : response.getStatusCode();
                if (status > 0 && status < 400) return;
                String reason = response == null ? "服务器返回异常响应"
                        : "服务器返回 " + status + " " + safeReason(response.getReasonPhrase());
                loadFailed = true;
                showLoadError("页面暂时无法打开", reason + "\n\n请稍后点击“重新加载”。");
            }

            @Override
            public void onReceivedSslError(WebView current, SslErrorHandler handler,
                                           SslError error) {
                if (handler != null) handler.cancel();
                loadFailed = true;
                showLoadError(
                        "安全连接失败",
                        "服务器证书校验失败，已停止加载以保护账号安全。请稍后重试。");
            }

            @Override
            public boolean onRenderProcessGone(WebView current, RenderProcessGoneDetail detail) {
                recoverWebView(current);
                return true;
            }
        });

        root.addView(view, 0);
        web = view;
    }

    private void createErrorView() {
        LinearLayout box = new LinearLayout(this);
        box.setOrientation(LinearLayout.VERTICAL);
        box.setGravity(Gravity.CENTER_HORIZONTAL | Gravity.CENTER_VERTICAL);
        box.setPadding(dp(28), dp(32), dp(28), dp(32));
        box.setBackgroundColor(getResources().getColor(R.color.background, getTheme()));
        box.setVisibility(View.GONE);

        errorTitle = new TextView(this);
        errorTitle.setTextColor(getResources().getColor(R.color.primary, getTheme()));
        errorTitle.setTextSize(22f);
        errorTitle.setGravity(Gravity.CENTER);
        box.addView(errorTitle);

        errorDetail = new TextView(this);
        errorDetail.setTextColor(Color.rgb(91, 85, 96));
        errorDetail.setTextSize(15f);
        errorDetail.setGravity(Gravity.CENTER);
        errorDetail.setLineSpacing(dp(4), 1f);
        LinearLayout.LayoutParams detailParams = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT);
        detailParams.topMargin = dp(14);
        box.addView(errorDetail, detailParams);

        Button retry = new Button(this);
        retry.setText("重新加载");
        retry.setAllCaps(false);
        retry.setTextSize(16f);
        retry.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                retryLoad();
            }
        });
        LinearLayout.LayoutParams retryParams = new LinearLayout.LayoutParams(
                Math.min(dp(260), getResources().getDisplayMetrics().widthPixels - dp(56)),
                dp(48));
        retryParams.topMargin = dp(24);
        box.addView(retry, retryParams);

        root.addView(box, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));
        errorView = box;
    }

    private void showLoadError(String title, String detail) {
        if (errorView == null) return;
        hideSplash();
        errorTitle.setText(title);
        errorDetail.setText(detail);
        errorView.setVisibility(View.VISIBLE);
        errorView.bringToFront();
    }

    private void clearLoadError() {
        if (errorView != null) errorView.setVisibility(View.GONE);
    }

    private void retryLoad() {
        clearLoadError();
        ensureWebView();
        loadApp(true);
    }

    private void loadApp(boolean fromRetry) {
        loadFailed = false;
        pageCommitted = false;
        showSplash(fromRetry ? "正在重新连接…" : "正在连接每日数据…");
        mainHandler.removeCallbacks(splashTimeout);
        web.loadUrl(appUrl);
        mainHandler.postDelayed(splashTimeout, SPLASH_TIMEOUT_MS);
    }

    private void ensureWebView() {
        if (web != null) return;
        installWebView();
    }

    private void recoverWebView(WebView failed) {
        long now = android.os.SystemClock.elapsedRealtime();
        if (lastRendererRecoveryAt == 0
                || now - lastRendererRecoveryAt > RENDERER_RECOVERY_WINDOW_MS) {
            rendererRecoveries = 0;
        }
        lastRendererRecoveryAt = now;
        rendererRecoveries++;

        if (failed != null) {
            if (failed.getParent() instanceof ViewGroup) {
                ((ViewGroup) failed.getParent()).removeView(failed);
            }
            failed.destroy();
        }
        if (failed == web) web = null;

        loadFailed = true;
        pageCommitted = false;
        if (rendererRecoveries > MAX_RENDERER_RECOVERIES) {
            showLoadError(
                    "页面进程反复退出",
                    "手机可用内存可能不足。请关闭其他应用后点击“重新加载”。");
            return;
        }

        installWebView();
        loadApp(true);
    }

    private void showSplash(String message) {
        if (splash == null) {
            splash = createSplash();
            root.addView(splash, new FrameLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.MATCH_PARENT));
        }
        if (splashText != null) splashText.setText(message);
        splash.setVisibility(View.VISIBLE);
        splash.bringToFront();
    }

    private void hideSplash() {
        mainHandler.removeCallbacks(splashTimeout);
        if (splash == null) return;
        if (splash.getParent() instanceof ViewGroup) {
            ((ViewGroup) splash.getParent()).removeView(splash);
        }
        splash = null;
        splashText = null;
    }

    private View createSplash() {
        FrameLayout box = new FrameLayout(this);
        box.setBackgroundColor(getResources().getColor(R.color.splash, getTheme()));
        splashText = new TextView(this);
        splashText.setText("正在连接每日数据…");
        splashText.setTextColor(Color.WHITE);
        splashText.setTextSize(22f);
        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.gravity = Gravity.CENTER;
        box.addView(splashText, lp);
        return box;
    }

    private String safeReason(String reason) {
        return reason == null || reason.trim().isEmpty() ? "请求失败" : reason;
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private boolean handleNavigation(Uri uri, boolean userGesture) {
        if (userGesture && uri != null && "dailyapp".equalsIgnoreCase(uri.getScheme())
                && "check-update".equalsIgnoreCase(uri.getHost())) {
            updateChecker.check(true);
            return true;
        }
        NavigationPolicy.Action action = NavigationPolicy.decide(
                uri == null ? null : uri.toString(), userGesture);
        if (action == NavigationPolicy.Action.LOAD_IN_WEB_VIEW) return false;
        if (action == NavigationPolicy.Action.OPEN_EXTERNAL_BROWSER) {
            openExternalBrowser(uri);
        }
        return true;
    }

    private void openExternalBrowser(Uri uri) {
        try {
            Intent intent = new Intent(Intent.ACTION_VIEW, uri);
            intent.addCategory(Intent.CATEGORY_BROWSABLE);
            startActivity(intent);
        } catch (ActivityNotFoundException | SecurityException e) {
            Toast.makeText(this, "无法打开浏览器，请使用“复制链接”", Toast.LENGTH_LONG).show();
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        if (web != null) {
            web.onResume();
            web.resumeTimers();
        }
        if (mainHandler != null && updateChecker != null) {
            if (updateCheckRunnable != null) mainHandler.removeCallbacks(updateCheckRunnable);
            updateCheckRunnable = new Runnable() {
                @Override
                public void run() {
                    updateChecker.check(false);
                }
            };
            mainHandler.postDelayed(updateCheckRunnable, 2000);
        }
    }

    @Override
    protected void onPause() {
        if (web != null) {
            web.onPause();
            web.pauseTimers();
        }
        CookieManager.getInstance().flush();
        super.onPause();
    }

    @Override
    protected void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        if (web != null) web.saveState(outState);
    }

    @Override
    public void onBackPressed() {
        if (web != null && web.canGoBack()) {
            clearLoadError();
            web.goBack();
        } else {
            super.onBackPressed();
        }
    }

    @Override
    protected void onDestroy() {
        if (mainHandler != null) mainHandler.removeCallbacksAndMessages(null);
        if (updateChecker != null) updateChecker.shutdown();
        if (web != null) {
            if (web.getParent() instanceof ViewGroup) {
                ((ViewGroup) web.getParent()).removeView(web);
            }
            web.destroy();
            web = null;
        }
        super.onDestroy();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
    }
}
