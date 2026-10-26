package android.window;

import android.app.ActivityManager;
import android.view.SurfaceControl;

/** A task an organizer took over when it registered, with its surface. Declarations only. */
public final class TaskAppearedInfo {
    public ActivityManager.RunningTaskInfo getTaskInfo() {
        throw new RuntimeException("Stub!");
    }

    public SurfaceControl getLeash() {
        throw new RuntimeException("Stub!");
    }
}
