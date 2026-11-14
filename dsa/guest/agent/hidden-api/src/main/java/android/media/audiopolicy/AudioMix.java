package android.media.audiopolicy;

import android.media.AudioDeviceInfo;
import android.media.AudioFormat;

/**
 * What an audio policy does with the sound its rule matches; declarations only. The flags
 * (ROUTE_FLAG_RENDER) are read from the framework's class at run time, so no value is copied
 * here.
 */
public class AudioMix {
    private AudioMix() {
        throw new RuntimeException("Stub!");
    }

    /** Makes a mix. */
    public static class Builder {
        public Builder(AudioMixingRule rule) {
            throw new RuntimeException("Stub!");
        }

        public Builder setFormat(AudioFormat format) {
            throw new RuntimeException("Stub!");
        }

        public Builder setRouteFlags(int routeFlags) {
            throw new RuntimeException("Stub!");
        }

        public Builder setDevice(AudioDeviceInfo device) {
            throw new RuntimeException("Stub!");
        }

        public AudioMix build() {
            throw new RuntimeException("Stub!");
        }
    }
}
