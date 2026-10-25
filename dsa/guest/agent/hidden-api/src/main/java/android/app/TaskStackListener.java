package android.app;

import android.content.ComponentName;
import android.os.RemoteException;

/**
 * The framework's base class for task change listeners: every callback does nothing until
 * overridden. Declarations only.
 */
public abstract class TaskStackListener extends ITaskStackListener.Stub {
    public TaskStackListener() {
        throw new RuntimeException("Stub!");
    }

    @Override
    public void onTaskStackChanged() throws RemoteException {
        throw new RuntimeException("Stub!");
    }

    @Override
    public void onTaskCreated(int taskId, ComponentName componentName) throws RemoteException {
        throw new RuntimeException("Stub!");
    }

    @Override
    public void onTaskRemoved(int taskId) throws RemoteException {
        throw new RuntimeException("Stub!");
    }

    @Override
    public void onTaskMovedToFront(ActivityManager.RunningTaskInfo taskInfo) throws RemoteException {
        throw new RuntimeException("Stub!");
    }

    @Override
    public void onTaskDescriptionChanged(ActivityManager.RunningTaskInfo taskInfo) throws RemoteException {
        throw new RuntimeException("Stub!");
    }

    @Override
    public void onTaskDisplayChanged(int taskId, int newDisplayId) throws RemoteException {
        throw new RuntimeException("Stub!");
    }

    @Override
    public void onTaskFocusChanged(int taskId, boolean focused) throws RemoteException {
        throw new RuntimeException("Stub!");
    }
}
