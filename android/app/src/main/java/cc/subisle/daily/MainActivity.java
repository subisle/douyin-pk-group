package cc.subisle.daily;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.Intent;
import android.graphics.Color;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.CookieManager;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.FrameLayout;
import android.widget.TextView;

/**
 * 每日数据轻量版：固定站点 WebView 壳。
 *
 * 安全约束：
 * - 只主动加载 APP_URL；
 * - 非本站 HTTP(S) 与所有非 HTTP(S) scheme 一律不交给 WebView / 外部应用；
 * - 不启用文件访问、通用内容访问、自动文件下载；
 * - 不暴露 addJavascriptInterface；
 * - 仅保留登录态所需的 DOM storage / Cookies。
 */
public final class MainActivity extends Activity {
    private static final String APP_URL = "http://43.142.30.113:81/";
    private static final String APP_HOST = "43.142.30.113";
    private static final int APP_PORT = 81;
    private static final String APK_UA_TOKEN = "DailyDataAndroid/1.0";

    private WebView web;
    private View splash;

    @Override
    @SuppressLint("SetJavaScriptEnabled")
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setTheme(R.style.AppTheme);

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
                if (newProgress >= 60 && splash != null) {
                    ((ViewGroup) splash.getParent()).removeView(splash);
                    splash = null;
                }
            }
        });

        web.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
                return !isAllowed(request.getUrl());
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView view, String url) {
                return !isAllowed(Uri.parse(url));
            }

            @Override
            public void onPageFinished(WebView view, String url) {
                if (splash != null) {
                    ((ViewGroup) splash.getParent()).removeView(splash);
                    splash = null;
                }
            }
        });

        if (savedInstanceState == null) {
            web.loadUrl(APP_URL);
        } else {
            web.restoreState(savedInstanceState);
            if (web.getUrl() == null || !isAllowed(Uri.parse(web.getUrl()))) {
                web.loadUrl(APP_URL);
            }
        }
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

    private static boolean isAllowed(Uri uri) {
        if (uri == null) return false;
        String scheme = uri.getScheme();
        if (scheme == null || (!"http".equalsIgnoreCase(scheme) && !"https".equalsIgnoreCase(scheme))) {
            return false;
        }
        if (!APP_HOST.equalsIgnoreCase(uri.getHost())) return false;
        int port = uri.getPort();
        if (port == -1) return "https".equalsIgnoreCase(scheme);
        return port == APP_PORT;
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
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
    }
}
