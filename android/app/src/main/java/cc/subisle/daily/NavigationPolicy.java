package cc.subisle.daily;

import java.net.URI;
import java.net.URISyntaxException;

/** Decides which URLs may stay in the app and which require an external browser. */
final class NavigationPolicy {
    private static final String APP_HOST = "43.142.30.113";
    private static final int APP_PORT = 81;

    enum Action {
        LOAD_IN_WEB_VIEW,
        OPEN_EXTERNAL_BROWSER,
        BLOCK
    }

    private NavigationPolicy() {}

    static Action decide(String rawUrl, boolean userGesture) {
        if (rawUrl == null) return Action.BLOCK;

        final URI uri;
        try {
            uri = new URI(rawUrl);
        } catch (URISyntaxException e) {
            return Action.BLOCK;
        }

        String scheme = uri.getScheme();
        if (scheme == null
                || (!"http".equalsIgnoreCase(scheme) && !"https".equalsIgnoreCase(scheme))
                || uri.getHost() == null) {
            return Action.BLOCK;
        }

        if (isAppUrl(uri)) return Action.LOAD_IN_WEB_VIEW;
        return userGesture ? Action.OPEN_EXTERNAL_BROWSER : Action.BLOCK;
    }

    static boolean isAppUrl(String rawUrl) {
        return decide(rawUrl, false) == Action.LOAD_IN_WEB_VIEW;
    }

    private static boolean isAppUrl(URI uri) {
        if (!APP_HOST.equalsIgnoreCase(uri.getHost())) return false;
        int port = uri.getPort();
        if (port == -1) return "https".equalsIgnoreCase(uri.getScheme());
        return port == APP_PORT;
    }
}
