package android.app.backup;

/** What a backup transport restores next (a system API); declarations only. */
public class RestoreDescription {
    public static final RestoreDescription NO_MORE_PACKAGES = null;
    public static final int TYPE_KEY_VALUE = 1;
    public static final int TYPE_FULL_STREAM = 2;

    public RestoreDescription(String packageName, int dataType) {
        throw new RuntimeException("Stub!");
    }

    public String getPackageName() {
        throw new RuntimeException("Stub!");
    }

    public int getDataType() {
        throw new RuntimeException("Stub!");
    }
}
