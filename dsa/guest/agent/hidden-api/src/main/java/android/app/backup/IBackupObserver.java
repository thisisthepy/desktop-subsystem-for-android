package android.app.backup;

import android.os.Binder;
import android.os.IBinder;
import android.os.IInterface;
import android.os.RemoteException;

/** The framework's callbacks for a requested backup (a oneway AIDL interface); declarations only. */
public interface IBackupObserver extends IInterface {
    void onUpdate(String currentPackage, BackupProgress backupProgress) throws RemoteException;

    void onResult(String target, int status) throws RemoteException;

    void backupFinished(int status) throws RemoteException;

    abstract class Stub extends Binder implements IBackupObserver {
        public Stub() {
            throw new RuntimeException("Stub!");
        }

        @Override
        public IBinder asBinder() {
            throw new RuntimeException("Stub!");
        }
    }
}
