package io.github.thisisthepy.dsa.agent.channel;

import android.os.Binder;
import android.util.Log;

/**
 * Marks a binder the agent serves as part of a stable (VINTF) interface.
 *
 * The control daemon runs on the vendor side, and the binder driver lets it call only
 * binders marked stable. The generated stubs of the channel interface call this instead of
 * Binder.markVintfStability(), which the SDK does not publish; platform apps may call it.
 */
public final class VintfStability {
    private VintfStability() {}

    public static void mark(Binder binder) {
        try {
            Binder.class.getMethod("markVintfStability").invoke(binder);
        } catch (ReflectiveOperationException e) {
            Log.e("DsaAgent", "could not mark a binder stable; the control daemon will refuse to call it", e);
        }
    }
}
