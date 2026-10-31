package android.window;

import android.app.ActivityManager;
import android.content.pm.ActivityInfo;
import android.os.IBinder;

/** What the window manager says about a starting window it asks for. Declarations only. */
public final class StartingWindowInfo {
    public ActivityManager.RunningTaskInfo taskInfo;
    public ActivityInfo targetActivityInfo;
    public int startingWindowTypeParameter;
    public int splashScreenThemeResId;
    public IBinder appToken;
}
