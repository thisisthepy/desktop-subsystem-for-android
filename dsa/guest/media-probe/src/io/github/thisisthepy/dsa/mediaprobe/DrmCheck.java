// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

package io.github.thisisthepy.dsa.mediaprobe;

import android.media.MediaDrm;
import android.os.Bundle;

import java.util.UUID;

/**
 * The "drm" check: whether an app sees Widevine, as a streaming app asks before it plays
 * protected video, and at which security level.
 *
 * <pre>
 *   am instrument -w -r -e check drm io.github.thisisthepy.dsa.mediaprobe/.MediaProbe
 * </pre>
 *
 * It reports "drm.widevine.supported" (true or false, from MediaDrm.isCryptoSchemeSupported)
 * and, when supported, "drm.widevine.securityLevel" (the "securityLevel" property, L1 to L3)
 * and "drm.widevine.maxSecurityLevel" (MediaDrm.getMaxSecurityLevel, as its number). Opening a
 * session is not asked for: it would provision the device's certificate over the network.
 */
final class DrmCheck {
    /** Widevine's scheme, as every app names it. */
    static final UUID WIDEVINE = new UUID(0xEDEF8BA979D64ACEL, 0xA3C827DCD51D21EDL);

    private DrmCheck() {}

    static void run(Bundle results) throws Exception {
        boolean supported = MediaDrm.isCryptoSchemeSupported(WIDEVINE);
        results.putString("drm.widevine.supported", Boolean.toString(supported));
        if (!supported) {
            return;
        }
        try (MediaDrm drm = new MediaDrm(WIDEVINE)) {
            results.putString("drm.widevine.securityLevel", drm.getPropertyString("securityLevel"));
            results.putInt("drm.widevine.maxSecurityLevel", MediaDrm.getMaxSecurityLevel());
            results.putString("drm.widevine.vendor", drm.getPropertyString(MediaDrm.PROPERTY_VENDOR));
        }
    }
}
