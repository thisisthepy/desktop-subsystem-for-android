package android.app.backup;

/** How far a backup of one package has got (a system API); declarations only. */
public class BackupProgress {
    public final long bytesExpected;
    public final long bytesTransferred;

    public BackupProgress(long bytesExpected, long bytesTransferred) {
        throw new RuntimeException("Stub!");
    }
}
