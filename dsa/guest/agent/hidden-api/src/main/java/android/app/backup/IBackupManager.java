package android.app.backup;

import android.os.Binder;
import android.os.IBinder;
import android.os.IInterface;
import android.os.RemoteException;

/**
 * The backup manager's binder interface (frameworks/base,
 * core/java/android/app/backup/IBackupManager.aidl); declarations only, of the methods the
 * agent calls.
 */
public interface IBackupManager extends IInterface {
    void setBackupEnabled(boolean isEnabled) throws RemoteException;

    void setAutoRestore(boolean doAutoRestore) throws RemoteException;

    boolean isBackupEnabled() throws RemoteException;

    String getCurrentTransport() throws RemoteException;

    String[] listAllTransports() throws RemoteException;

    String selectBackupTransport(String transport) throws RemoteException;

    IRestoreSession beginRestoreSessionForUser(int userId, String packageName, String transportID)
            throws RemoteException;

    void setBackupServiceActive(int whichUser, boolean makeActive) throws RemoteException;

    boolean isBackupServiceActive(int whichUser) throws RemoteException;

    String[] filterAppsEligibleForBackupForUser(int userId, String[] packages)
            throws RemoteException;

    int requestBackup(String[] packages, IBackupObserver observer, IBackupManagerMonitor monitor,
            int flags) throws RemoteException;

    abstract class Stub extends Binder implements IBackupManager {
        public Stub() {
            throw new RuntimeException("Stub!");
        }

        public static IBackupManager asInterface(IBinder obj) {
            throw new RuntimeException("Stub!");
        }

        @Override
        public IBinder asBinder() {
            throw new RuntimeException("Stub!");
        }
    }
}
