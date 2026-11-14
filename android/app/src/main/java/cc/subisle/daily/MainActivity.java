package cc.subisle.daily;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.graphics.Color;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.CookieManager;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.FrameLayout;
import android.widget.TextView;
import android.widget.Toast;

/**
 * 每日数据轻量版：固定站点 WebView 壳。
 *
 * 安全约束：
 * - 只主动加载 APP_URL；
 * - 本站 HTTP(S) 留在 WebView；用户点击的外站 HTTP(S) 交给系统浏览器；
 * - 无点击的外站跳转与非 HTTP(S) scheme 拦截（内部更新指令除外）；
 * - 不启用文件访问、通用内容访问、自动文件下载；
 * - 不暴露 addJavascriptInterface；
 * - 仅保留登录态所需的 DOM storage / Cookies。
 */
public final class MainActivity extends Activity {
    private static final String APP_URL = "http://43.142.30.113:81/";
    private static final String APK_UA_TOKEN = "DailyDataAndroid/1.2";

    private WebView web;
    private View splash;
    private Handler mainHandler;
    private AppUpdateChecker updateChecker;
    private final Runnable splashTimeout = new Runnable() {
        @Override
        public void run() {
            hideSplash();
        }
    };

    @Override
    @SuppressLint("SetJavaScriptEnabled")
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setTheme(R.style.AppTheme);
        mainHandler = new Handler(Looper.getMainLooper());
        updateChecker = new AppUpdateChecker(this, APP_URL);

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(getResources().getColor(R.color.background, getTheme()));

        web = new WebView(this);
        web.setId(View.generateViewId());
        web.setLayoutParams(new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));
        web.setBackgroundColor(getResources().getColor(R.color.background, getTheme()));
        root.addView(web);

        splash = createSplash();
        root.addView(splash, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));

        setContentView(root);

        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setJavaScriptCanOpenWindowsAutomatically(false);
        s.setDomStorageEnabled(true);
        s.setDatabaseEnabled(false);
        s.setAllowFileAccess(false);
        s.setAllowContentAccess(false);
        s.setAllowFileAccessFromFileURLs(false);
        s.setAllowUniversalAccessFromFileURLs(false);
        s.setLoadWithOverviewMode(false);
        s.setUseWideViewPort(false);
        s.setSupportZoom(false);
        s.setBuiltInZoomControls(false);
        s.setDisplayZoomControls(false);
        s.setCacheMode(WebSettings.LOAD_DEFAULT);
        s.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);
        String ua = s.getUserAgentString();
        String cleanUa = ua == null ? "" : ua;
        if (cleanUa.contains("; wv") || cleanUa.contains("MicroMessenger")) {
            cleanUa = cleanUa.replaceAll("(?i)MicroMessenger/[^ ]+", "")
                    .replace("; wv", "")
                    .replaceAll("\\s+", " ")
                    .trim();
        }
        s.setUserAgentString((cleanUa.isEmpty() ? "Mozilla/5.0" : cleanUa) + " " + APK_UA_TOKEN);

        CookieManager.getInstance().setAcceptCookie(true);
        CookieManager.getInstance().setAcceptThirdPartyCookies(web, false);

        web.setWebChromeClient(new WebChromeClient() {
            @Override
            public void onProgressChanged(WebView view, int newProgress) {
                if (newProgress >= 60) hideSplash();
            }
        });

        web.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
                return handleNavigation(request.getUrl(), request.hasGesture());
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView view, String url) {
                // minSdk 26 normally uses the WebResourceRequest overload above.
                return handleNavigation(Uri.parse(url), true);
            }

            @Override
            public void onPageFinished(WebView view, String url) {
                hideSplash();
            }

            @Override
            public void onReceivedError(WebView view, WebResourceRequest request,
                                        WebResourceError error) {
                if (request.isForMainFrame()) hideSplash();
            }
        });

        if (savedInstanceState == null) {
            web.loadUrl(APP_URL);
        } else {
            web.restoreState(savedInstanceState);
            if (web.getUrl() == null || !NavigationPolicy.isAppUrl(web.getUrl())) {
                web.loadUrl(APP_URL);
            }
        }
        mainHandler.postDelayed(splashTimeout, 8000);
    }

    private void hideSplash() {
        if (splash == null) return;
        if (splash.getParent() instanceof ViewGroup) {
            ((ViewGroup) splash.getParent()).removeView(splash);
        }
        splash = null;
    }

    private View createSplash() {
        FrameLayout box = new FrameLayout(this);
        box.setBackgroundColor(getResources().getColor(R.color.splash, getTheme()));
        TextView tv = new TextView(this);
        tv.setText(R.string.app_name);
        tv.setTextColor(Color.WHITE);
        tv.setTextSize(24f);
        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.gravity = android.view.Gravity.CENTER;
        box.addView(tv, lp);
        return box;
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
        mainHandler.postDelayed(new Runnable() {
            @Override
            public void run() {
                updateChecker.check(false);
            }
        }, 2000);
    }

    @Override
    protected void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        web.saveState(outState);
    }

    @Override
    public void onBackPressed() {
        if (web.canGoBack()) {
            web.goBack();
        } else {
            super.onBackPressed();
        }
    }

    @Override
    protected void onDestroy() {
        mainHandler.removeCallbacksAndMessages(null);
        updateChecker.shutdown();
        if (web != null) {
            if (web.getParent() instanceof ViewGroup) {
                ((ViewGroup) web.getParent()).removeView(web);
            }
            web.destroy();
        }
        super.onDestroy();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
    }
}
