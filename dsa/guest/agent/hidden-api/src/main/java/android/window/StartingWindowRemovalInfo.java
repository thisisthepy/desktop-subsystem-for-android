package android.window;

import android.view.SurfaceControl;

/** The window manager's request to remove a task's starting window. Declarations only. */
public final class StartingWindowRemovalInfo {
    public int taskId;
    public SurfaceControl windowAnimationLeash;
    public boolean playRevealAnimation;
    public boolean windowlessSurface;
    public boolean removeImmediately;
}
