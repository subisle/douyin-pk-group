package cc.subisle.daily;

/** Host-side regression tests; no Android device or test framework required. */
public final class NavigationPolicyTest {
    private static int assertions;

    public static void main(String[] args) {
        expect(NavigationPolicy.Action.LOAD_IN_WEB_VIEW,
                "http://43.142.30.113:81/", true, "app entry");
        expect(NavigationPolicy.Action.LOAD_IN_WEB_VIEW,
                "https://43.142.30.113/teams/st", false, "app HTTPS entry");
        expect(NavigationPolicy.Action.OPEN_EXTERNAL_BROWSER,
                "https://drive.uc.cn/s/abc123", true, "clicked UC netdisk link");
        expect(NavigationPolicy.Action.OPEN_EXTERNAL_BROWSER,
                "HTTP://PAN.EXAMPLE.COM/resource", true, "case-insensitive HTTP URL");
        expect(NavigationPolicy.Action.BLOCK,
                "https://drive.uc.cn/s/abc123", false, "external redirect without a click");
        expect(NavigationPolicy.Action.OPEN_EXTERNAL_BROWSER,
                "http://43.142.30.113/download", true, "same host on another port is external");
        expect(NavigationPolicy.Action.BLOCK,
                "intent://scan/#Intent;scheme=zxing;end", true, "intent scheme");
        expect(NavigationPolicy.Action.BLOCK,
                "javascript:alert(1)", true, "JavaScript URL");
        expect(NavigationPolicy.Action.BLOCK,
                "file:///sdcard/private.txt", true, "file URL");
        expect(NavigationPolicy.Action.BLOCK,
                "https:///missing-host", true, "missing host");
        expect(NavigationPolicy.Action.BLOCK,
                "https://example.com/%zz", true, "malformed URL");
        expect(NavigationPolicy.Action.BLOCK,
                null, true, "null URL");

        System.out.println("NavigationPolicyTest: " + assertions + " assertions passed");
    }

    private static void expect(NavigationPolicy.Action expected, String url,
                               boolean userGesture, String label) {
        NavigationPolicy.Action actual = NavigationPolicy.decide(url, userGesture);
        assertions++;
        if (actual != expected) {
            throw new AssertionError(label + ": expected " + expected + ", got " + actual);
        }
    }
}
