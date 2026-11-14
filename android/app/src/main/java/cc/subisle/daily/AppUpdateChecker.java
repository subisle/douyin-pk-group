package cc.subisle.daily;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ActivityNotFoundException;
import android.content.DialogInterface;
import android.content.Intent;
import android.content.pm.PackageInfo;
import android.net.Uri;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.widget.Toast;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/** Checks the public version endpoint and offers the signed APK through the system browser. */
final class AppUpdateChecker {
    private static final long AUTO_CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000L;
    private final Activity activity;
    private final String appUrl;
    private final Handler main = new Handler(Looper.getMainLooper());
    private final ExecutorService executor = Executors.newSingleThreadExecutor();
    private boolean checking;
    private boolean manualResultWanted;
    private long lastAutoCheckAt;
    private AlertDialog updateDialog;

    AppUpdateChecker(Activity activity, String appUrl) {
        this.activity = activity;
        this.appUrl = appUrl;
    }

    void check(final boolean manual) {
        long now = SystemClock.elapsedRealtime();
        if (!manual && lastAutoCheckAt > 0
                && now - lastAutoCheckAt < AUTO_CHECK_INTERVAL_MS) return;
        if (manual) manualResultWanted = true;
        if (checking) {
            if (manual) toast("正在检查更新，请稍候");
            return;
        }
        checking = true;
        if (!manual) lastAutoCheckAt = now;
        executor.execute(new Runnable() {
            @Override
            public void run() {
                final Result result = fetchLatest();
                main.post(new Runnable() {
                    @Override
                    public void run() {
                        checking = false;
                        if (activity.isFinishing() || activity.isDestroyed()) return;
                        boolean notifyManual = manual || manualResultWanted;
                        manualResultWanted = false;
                        showResult(result, notifyManual);
                    }
                });
            }
        });
    }

    void shutdown() {
        executor.shutdownNow();
        if (updateDialog != null) updateDialog.dismiss();
    }

    private Result fetchLatest() {
        HttpURLConnection connection = null;
        try {
            PackageInfo info = activity.getPackageManager()
                    .getPackageInfo(activity.getPackageName(), 0);
            long currentCode;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                currentCode = info.getLongVersionCode();
            } else {
                currentCode = info.versionCode;
            }
            String currentName = info.versionName == null ? "" : info.versionName;

            connection = (HttpURLConnection) new URL(appUrl + "api/public/app-version")
                    .openConnection();
            connection.setConnectTimeout(5000);
            connection.setReadTimeout(5000);
            connection.setUseCaches(false);
            connection.setRequestProperty("Accept", "application/json");
            if (connection.getResponseCode() != HttpURLConnection.HTTP_OK) {
                return Result.failure(currentCode, currentName);
            }
            JSONObject root = new JSONObject(readLimited(connection.getInputStream()));
            JSONObject android = root.getJSONObject("android");
            long latestCode = android.getLong("versionCode");
            String latestName = android.optString("versionName", "");
            return Result.success(currentCode, currentName, latestCode, latestName);
        } catch (Exception e) {
            return Result.failure(0, "");
        } finally {
            if (connection != null) connection.disconnect();
        }
    }

    private static String readLimited(InputStream input) throws Exception {
        try (InputStream stream = input; ByteArrayOutputStream out = new ByteArrayOutputStream()) {
            byte[] buffer = new byte[2048];
            int total = 0;
            int count;
            while ((count = stream.read(buffer)) != -1) {
                total += count;
                if (total > 64 * 1024) throw new IllegalStateException("version response too large");
                out.write(buffer, 0, count);
            }
            return new String(out.toByteArray(), StandardCharsets.UTF_8);
        }
    }

    private void showResult(Result result, boolean manual) {
        if (!result.ok) {
            if (manual) toast("检查更新失败，请稍后重试");
            return;
        }
        if (result.latestCode <= result.currentCode) {
            if (manual) toast("已是最新版本（" + result.currentName + "）");
            return;
        }
        if (updateDialog != null && updateDialog.isShowing()) return;
        String latest = result.latestName.isEmpty() ? String.valueOf(result.latestCode) : result.latestName;
        updateDialog = new AlertDialog.Builder(activity)
                .setTitle("发现新版本 " + latest)
                .setMessage("当前版本 " + result.currentName + "\n最新版本 " + latest
                        + "\n\n更新后仍保留登录状态。")
                .setNegativeButton("稍后", null)
                .setPositiveButton("立即更新", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        openDownload();
                    }
                })
                .create();
        updateDialog.show();
    }

    private void openDownload() {
        try {
            Intent intent = new Intent(Intent.ACTION_VIEW,
                    Uri.parse(appUrl + "download/daily.apk"));
            intent.addCategory(Intent.CATEGORY_BROWSABLE);
            activity.startActivity(intent);
        } catch (ActivityNotFoundException | SecurityException e) {
            toast("无法打开浏览器，请在系统浏览器中打开本站下载");
        }
    }

    private void toast(String text) {
        Toast.makeText(activity, text, Toast.LENGTH_LONG).show();
    }

    private static final class Result {
        final boolean ok;
        final long currentCode;
        final String currentName;
        final long latestCode;
        final String latestName;

        private Result(boolean ok, long currentCode, String currentName,
                       long latestCode, String latestName) {
            this.ok = ok;
            this.currentCode = currentCode;
            this.currentName = currentName;
            this.latestCode = latestCode;
            this.latestName = latestName;
        }

        static Result success(long currentCode, String currentName,
                              long latestCode, String latestName) {
            return new Result(true, currentCode, currentName, latestCode, latestName);
        }

        static Result failure(long currentCode, String currentName) {
            return new Result(false, currentCode, currentName, 0, "");
        }
    }
}
