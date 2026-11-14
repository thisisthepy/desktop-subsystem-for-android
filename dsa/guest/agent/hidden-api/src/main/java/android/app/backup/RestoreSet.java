package android.app.backup;

/** A restore set a backup transport offers (a system API); declarations only. */
public class RestoreSet {
    public String name;
    public String device;
    public long token;

    public RestoreSet() {
        throw new RuntimeException("Stub!");
    }

    public RestoreSet(String name, String device, long token) {
        throw new RuntimeException("Stub!");
    }
}
