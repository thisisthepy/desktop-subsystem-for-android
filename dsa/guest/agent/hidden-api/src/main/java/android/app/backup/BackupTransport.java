package android.app.backup;

import android.content.Intent;
import android.content.pm.PackageInfo;
import android.os.IBinder;
import android.os.ParcelFileDescriptor;

/**
 * The framework's base class of a backup transport (a system API); declarations only. The
 * signatures and constants are the framework's (frameworks/base,
 * core/java/android/app/backup/BackupTransport.java).
 */
public class BackupTransport {
    public static final int TRANSPORT_OK = 0;
    public static final int NO_MORE_DATA = -1;
    public static final int TRANSPORT_ERROR = -1000;
    public static final int TRANSPORT_NOT_INITIALIZED = -1001;
    public static final int TRANSPORT_PACKAGE_REJECTED = -1002;
    public static final int AGENT_ERROR = -1003;
    public static final int AGENT_UNKNOWN = -1004;
    public static final int TRANSPORT_QUOTA_EXCEEDED = -1005;
    public static final int TRANSPORT_NON_INCREMENTAL_BACKUP_REQUIRED = -1006;
    public static final int FLAG_USER_INITIATED = 1;
    public static final int FLAG_INCREMENTAL = 1 << 1;
    public static final int FLAG_NON_INCREMENTAL = 1 << 2;
    public static final int FLAG_DATA_NOT_CHANGED = 1 << 3;

    public BackupTransport() {
        throw new RuntimeException("Stub!");
    }

    public IBinder getBinder() {
        throw new RuntimeException("Stub!");
    }

    public String name() {
        throw new RuntimeException("Stub!");
    }

    public Intent configurationIntent() {
        throw new RuntimeException("Stub!");
    }

    public String currentDestinationString() {
        throw new RuntimeException("Stub!");
    }

    public Intent dataManagementIntent() {
        throw new RuntimeException("Stub!");
    }

    public String dataManagementLabel() {
        throw new RuntimeException("Stub!");
    }

    public CharSequence dataManagementIntentLabel() {
        throw new RuntimeException("Stub!");
    }

    public String transportDirName() {
        throw new RuntimeException("Stub!");
    }

    public int initializeDevice() {
        throw new RuntimeException("Stub!");
    }

    public int clearBackupData(PackageInfo packageInfo) {
        throw new RuntimeException("Stub!");
    }

    public int finishBackup() {
        throw new RuntimeException("Stub!");
    }

    public long requestBackupTime() {
        throw new RuntimeException("Stub!");
    }

    public int performBackup(PackageInfo packageInfo, ParcelFileDescriptor inFd, int flags) {
        throw new RuntimeException("Stub!");
    }

    public int performBackup(PackageInfo packageInfo, ParcelFileDescriptor inFd) {
        throw new RuntimeException("Stub!");
    }

    public RestoreSet[] getAvailableRestoreSets() {
        throw new RuntimeException("Stub!");
    }

    public long getCurrentRestoreSet() {
        throw new RuntimeException("Stub!");
    }

    public int startRestore(long token, PackageInfo[] packages) {
        throw new RuntimeException("Stub!");
    }

    public RestoreDescription nextRestorePackage() {
        throw new RuntimeException("Stub!");
    }

    public int getRestoreData(ParcelFileDescriptor outFd) {
        throw new RuntimeException("Stub!");
    }

    public void finishRestore() {
        throw new RuntimeException("Stub!");
    }

    public long requestFullBackupTime() {
        throw new RuntimeException("Stub!");
    }

    public int performFullBackup(PackageInfo targetPackage, ParcelFileDescriptor socket, int flags) {
        throw new RuntimeException("Stub!");
    }

    public int performFullBackup(PackageInfo targetPackage, ParcelFileDescriptor socket) {
        throw new RuntimeException("Stub!");
    }

    public int checkFullBackupSize(long size) {
        throw new RuntimeException("Stub!");
    }

    public int sendBackupData(int numBytes) {
        throw new RuntimeException("Stub!");
    }

    public void cancelFullBackup() {
        throw new RuntimeException("Stub!");
    }

    public boolean isAppEligibleForBackup(PackageInfo targetPackage, boolean isFullBackup) {
        throw new RuntimeException("Stub!");
    }

    public long getBackupQuota(String packageName, boolean isFullBackup) {
        throw new RuntimeException("Stub!");
    }

    public int getNextFullRestoreDataChunk(ParcelFileDescriptor socket) {
        throw new RuntimeException("Stub!");
    }

    public int abortFullRestore() {
        throw new RuntimeException("Stub!");
    }

    public int getTransportFlags() {
        throw new RuntimeException("Stub!");
    }
}
