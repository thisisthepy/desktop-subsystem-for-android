package android.media.audiopolicy;

import android.content.Context;
import java.util.List;

/**
 * A policy of the audio framework's routing, which a privileged app registers through
 * AudioManager.registerAudioPolicy; declarations only. The agent's policy routes each app with a
 * window to an output of its own.
 */
public class AudioPolicy {
    private AudioPolicy() {
        throw new RuntimeException("Stub!");
    }

    /** Adds the mixes to the registered policy. AudioManager.SUCCESS (0) when it did. */
    public int attachMixes(List<AudioMix> mixes) {
        throw new RuntimeException("Stub!");
    }

    /** Takes mixes the policy has out of it. AudioManager.SUCCESS (0) when it did. */
    public int detachMixes(List<AudioMix> mixes) {
        throw new RuntimeException("Stub!");
    }

    /** Makes a policy. */
    public static class Builder {
        public Builder(Context context) {
            throw new RuntimeException("Stub!");
        }

        public Builder addMix(AudioMix mix) {
            throw new RuntimeException("Stub!");
        }

        public AudioPolicy build() {
            throw new RuntimeException("Stub!");
        }
    }
}
