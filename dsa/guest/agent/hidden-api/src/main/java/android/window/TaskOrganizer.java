package android.window;

import android.app.ActivityManager;
import android.view.SurfaceControl;
import java.util.List;
import java.util.concurrent.Executor;

/**
 * The framework's task organizer: the window manager hands it every task and asks it to draw
 * each task's starting window. Every callback does nothing until overridden. Declarations
 * only.
 */
public class TaskOrganizer extends WindowOrganizer {
    public TaskOrganizer(ITaskOrganizerController taskOrganizerController, Executor executor) {
        throw new RuntimeException("Stub!");
    }

    public List<TaskAppearedInfo> registerOrganizer() {
        throw new RuntimeException("Stub!");
    }

    public void unregisterOrganizer() {
        throw new RuntimeException("Stub!");
    }

    public void addStartingWindow(StartingWindowInfo info) {
        throw new RuntimeException("Stub!");
    }

    public void removeStartingWindow(StartingWindowRemovalInfo removalInfo) {
        throw new RuntimeException("Stub!");
    }

    public void copySplashScreenView(int taskId) {
        throw new RuntimeException("Stub!");
    }

    public void onAppSplashScreenViewRemoved(int taskId) {
        throw new RuntimeException("Stub!");
    }

    public void onTaskAppeared(ActivityManager.RunningTaskInfo taskInfo, SurfaceControl leash) {
        throw new RuntimeException("Stub!");
    }

    public void onTaskVanished(ActivityManager.RunningTaskInfo taskInfo) {
        throw new RuntimeException("Stub!");
    }

    public void onTaskInfoChanged(ActivityManager.RunningTaskInfo taskInfo) {
        throw new RuntimeException("Stub!");
    }
}
