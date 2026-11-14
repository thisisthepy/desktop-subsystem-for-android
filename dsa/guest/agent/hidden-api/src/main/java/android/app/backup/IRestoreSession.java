package android.app.backup;

import android.os.IInterface;
import android.os.RemoteException;

/** A restore session of the backup manager (an AIDL interface); declarations only. */
public interface IRestoreSession extends IInterface {
    int getAvailableRestoreSets(IRestoreObserver observer, IBackupManagerMonitor monitor)
            throws RemoteException;

    int restoreAll(long token, IRestoreObserver observer, IBackupManagerMonitor monitor)
            throws RemoteException;

    int restorePackages(long token, IRestoreObserver observer, String[] packages,
            IBackupManagerMonitor monitor) throws RemoteException;

    void endRestoreSession() throws RemoteException;
}
