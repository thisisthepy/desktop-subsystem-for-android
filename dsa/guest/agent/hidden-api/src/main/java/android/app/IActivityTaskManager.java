package android.app;

import android.os.IInterface;
import android.os.RemoteException;

/** The activity task manager's binder interface, the part the agent calls; declarations only. */
public interface IActivityTaskManager extends IInterface {
    void registerTaskStackListener(ITaskStackListener listener) throws RemoteException;

    void unregisterTaskStackListener(ITaskStackListener listener) throws RemoteException;

    boolean removeTask(int taskId) throws RemoteException;
}
