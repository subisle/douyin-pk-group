package cc.subisle.daily;

import java.net.URI;
import java.net.URISyntaxException;
import java.util.Locale;

/** Decides which URLs may stay in the app and which require an external browser. */
final class NavigationPolicy {
    private static volatile String appScheme;
    private static volatile String appHost;
    private static volatile int appPort;

    enum Action {
        LOAD_IN_WEB_VIEW,
        OPEN_EXTERNAL_BROWSER,
        BLOCK
    }

    private NavigationPolicy() {}

    static void configure(String rawAppUrl) {
        try {
            URI uri = new URI(rawAppUrl);
            String scheme = normalizeScheme(uri.getScheme());
            String host = uri.getHost();
            if (scheme == null || host == null || host.isEmpty()) return;
            appScheme = scheme;
            appHost = host.toLowerCase(Locale.ROOT);
            appPort = effectivePort(uri);
        } catch (URISyntaxException ignored) {
            // The build script rejects invalid URLs. Keep the previous safe configuration.
        }
    }

    static Action decide(String rawUrl, boolean userGesture) {
        if (rawUrl == null || appHost == null) return Action.BLOCK;

        final URI uri;
        try {
            uri = new URI(rawUrl);
        } catch (URISyntaxException e) {
            return Action.BLOCK;
        }

        String scheme = normalizeScheme(uri.getScheme());
        if (scheme == null || uri.getHost() == null) return Action.BLOCK;

        if (isAppUrl(uri, scheme)) return Action.LOAD_IN_WEB_VIEW;
        return userGesture ? Action.OPEN_EXTERNAL_BROWSER : Action.BLOCK;
    }

    static boolean isAppUrl(String rawUrl) {
        return decide(rawUrl, false) == Action.LOAD_IN_WEB_VIEW;
    }

    private static boolean isAppUrl(URI uri, String scheme) {
        String host = uri.getHost().toLowerCase(Locale.ROOT);
        if (!appHost.equals(host)) return false;

        int port = effectivePort(uri);
        if (appScheme.equals(scheme) && appPort == port) return true;

        // Preserve the existing secure upgrade behavior for the same host.
        return "https".equals(scheme) && port == 443;
    }

    private static int effectivePort(URI uri) {
        if (uri.getPort() != -1) return uri.getPort();
        return "https".equalsIgnoreCase(uri.getScheme()) ? 443 : 80;
    }

    private static String normalizeScheme(String scheme) {
        if (scheme == null) return null;
        String lower = scheme.toLowerCase(Locale.ROOT);
        if ("http".equals(lower) || "https".equals(lower)) return lower;
        return null;
    }
}
