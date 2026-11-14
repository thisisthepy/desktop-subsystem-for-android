package android.os;

/** The framework's service registry (hidden); declarations only. */
public final class ServiceManager {
    private ServiceManager() {
    }

    public static IBinder getService(String name) {
        throw new RuntimeException("Stub!");
    }
}
