package android.app.backup;

import android.os.Binder;
import android.os.IBinder;
import android.os.IInterface;
import android.os.RemoteException;

/** The framework's callbacks for a restore session (a oneway AIDL interface); declarations only. */
public interface IRestoreObserver extends IInterface {
    void restoreSetsAvailable(RestoreSet[] result) throws RemoteException;

    void restoreStarting(int numPackages) throws RemoteException;

    void onUpdate(int nowBeingRestored, String curentPackage) throws RemoteException;

    void restoreFinished(int error) throws RemoteException;

    abstract class Stub extends Binder implements IRestoreObserver {
        public Stub() {
            throw new RuntimeException("Stub!");
        }

        @Override
        public IBinder asBinder() {
            throw new RuntimeException("Stub!");
        }
    }
}
