// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
package io.github.thisisthepy.dsa.mediaprobe;

import android.app.Instrumentation;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.graphics.Rect;
import android.media.AudioFormat;
import android.media.AudioTrack;
import android.media.Image;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.SystemClock;
import android.util.Log;
import android.view.PixelCopy;
import android.view.SurfaceView;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;

/**
 * The "video" check: clips of four colour bars, at sizes that are not a multiple of 16 or 64
 * (426x240, 360x640), encoded as H.264 and VP9 and decoded with every decoder the device lists
 * for them, three ways:
 *
 * <ul>
 *   <li>to buffers the app reads, whose pictures must hold the bars' colours to their right
 *       and bottom edges (a picture whose chroma was lost is green, and padding shown past the
 *       picture is a green band);
 *   <li>to a surface on screen while a sound plays, as a player does, moving to a second
 *       surface halfway: pictures must keep reaching the screen on both, and the sound must
 *       play;
 *   <li>to one surface for a first clip and another for a second, as a feed of short videos
 *       does, which starts its sound only once the second clip's first picture is shown.
 * </ul>
 *
 * Pictures reaching the screen are counted with the codec's rendered-frame callback, and what
 * the surface shows is copied back (PixelCopy) and its colours checked as well. The display is
 * then captured as SurfaceFlinger composes it, and the bars checked where the surface sits on
 * it: PixelCopy reads the surface in the probe's own process, whose graphics driver copies the
 * decoder's picture up to the host by itself, so a picture the composer never copied up
 * (green on screen, as YouTube showed it) passes PixelCopy and fails only here.
 *
 * <p>Results, per clip ({@code avc_426x240} and so on) and decoder:
 * {@code video.<clip>.<decoder>.buffer.out}, {@code .buffer.colour} ("ok" or what is wrong),
 * {@code .surface.rendered}, {@code .surface.presented}, {@code .surface.after_swap},
 * {@code .surface.colour}, {@code .surface.screen}, {@code .surface.audio},
 * {@code .shorts.first}, {@code .shorts.second}, {@code .shorts.first_frame_ms},
 * {@code .shorts.colour}, {@code .shorts.screen}, and {@code .<way>.error} for a way that
 * threw.
 */
final class VideoCheck {
    private static final String TAG = MediaProbe.TAG;
    private static final String AVC = MediaFormat.MIMETYPE_VIDEO_AVC;
    private static final String VP9 = MediaFormat.MIMETYPE_VIDEO_VP9;
    private static final int[][] SIZES = {{426, 240}, {360, 640}};
    /** The bars, left to right, as BT.601 limited-range Y, Cb, Cr: grey, red, yellow, blue. */
    static final int[][] BARS = {{180, 128, 128}, {81, 90, 240}, {210, 16, 146}, {41, 240, 110}};
    /** The same bars as they show on screen. */
    private static final int[] BAR_COLOURS = {
        Color.rgb(193, 193, 193), Color.rgb(255, 0, 0), Color.rgb(255, 255, 0), Color.rgb(0, 0, 255)
    };
    /** How far a decoded sample may be from its bar: the clips are lossy. */
    private static final int YUV_TOLERANCE = 32;
    private static final int RGB_TOLERANCE = 72;
    /** The second clip of a feed is told apart by its timestamps, this far on. */
    private static final long SECOND_CLIP_US = 10_000_000L;
    /** How long a feed's second clip may take to show its first picture. */
    private static final long FIRST_FRAME_WAIT_MS = 5_000;

    private final Instrumentation instrumentation;

    VideoCheck(Instrumentation instrumentation) {
        this.instrumentation = instrumentation;
    }

    static int bar(int x, int width) {
        return Math.min(3, x * 4 / width);
    }

    void run(Bundle results) throws Exception {
        HandlerThread callbacks = new HandlerThread("probe-callbacks");
        callbacks.start();
        Handler handler = new Handler(callbacks.getLooper());
        Intent intent = new Intent(Intent.ACTION_MAIN)
                .setClassName(instrumentation.getTargetContext(), VideoActivity.class.getName())
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        VideoActivity activity = (VideoActivity) instrumentation.startActivitySync(intent);
        try {
            instrumentation.waitForIdleSync();
            if (!activity.awaitSurfaces(10_000)) {
                results.putString("video.error", "the probe's surfaces were never made");
                return;
            }
            for (String type : new String[] {AVC, VP9}) {
                for (int[] size : SIZES) {
                    clip(results, type, size[0], size[1], activity, handler);
                }
            }
        } finally {
            activity.finish();
            callbacks.quitSafely();
        }
    }

    private void clip(Bundle results, String type, int width, int height, VideoActivity activity,
            Handler handler) {
        String key = "video." + (type.equals(AVC) ? "avc" : "vp9") + "_" + width + "x" + height;
        MediaProbe.Stream stream;
        try {
            stream = encode(type, width, height);
        } catch (IOException | RuntimeException e) {
            Log.e(TAG, key + ": the encoder failed", e);
            results.putString(key + ".encode_error", e.toString());
            return;
        }
        results.putInt(key + ".units", stream.units.size());
        if (stream.units.isEmpty()) {
            return;
        }
        List<String> names = MediaProbe.decodersFor(type);
        results.putString(key + ".decoders", String.join(",", names));
        for (String name : names) {
            String at = key + "." + name;
            try {
                toBuffers(results, at + ".buffer", name, type, stream, width, height);
            } catch (Exception e) {
                Log.e(TAG, at + ": decoding to buffers failed", e);
                results.putString(at + ".buffer.error", e.toString());
            }
            try {
                toSurfaces(results, at + ".surface", name, type, stream, width, height, activity, handler);
            } catch (Exception e) {
                Log.e(TAG, at + ": decoding to a surface failed", e);
                results.putString(at + ".surface.error", e.toString());
            }
            try {
                feed(results, at + ".shorts", name, type, stream, width, height, activity, handler);
            } catch (Exception e) {
                Log.e(TAG, at + ": the feed of two clips failed", e);
                results.putString(at + ".shorts.error", e.toString());
            }
        }
    }

    /** Fills a flexible YUV 4:2:0 image with the bars. */
    private static void bars(Image image, int width, int height) {
        Image.Plane[] planes = image.getPlanes();
        for (int plane = 0; plane < 3; plane++) {
            ByteBuffer data = planes[plane].getBuffer();
            int rowStride = planes[plane].getRowStride();
            int pixelStride = planes[plane].getPixelStride();
            int w = plane == 0 ? width : (width + 1) / 2;
            int h = plane == 0 ? height : (height + 1) / 2;
            for (int y = 0; y < h; y++) {
                for (int x = 0; x < w; x++) {
                    int[] colour = BARS[bar(plane == 0 ? x : 2 * x, width)];
                    data.put(y * rowStride + x * pixelStride, (byte) colour[plane]);
                }
            }
        }
    }

    /** A second of the bars at 30 pictures a second. */
    private static MediaProbe.Stream encode(String type, int width, int height) throws IOException {
        MediaFormat format = MediaFormat.createVideoFormat(type, width, height);
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible);
        format.setInteger(MediaFormat.KEY_BIT_RATE, 2_000_000);
        format.setInteger(MediaFormat.KEY_FRAME_RATE, MediaProbe.FRAME_RATE);
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1);
        MediaCodec codec = MediaCodec.createByCodecName(MediaProbe.encoderFor(format));
        MediaProbe.Stream stream = new MediaProbe.Stream();
        try {
            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            codec.start();
            long deadline = SystemClock.uptimeMillis() + MediaProbe.CODEC_WAIT_MS;
            int queued = 0;
            boolean ended = false;
            int frames = MediaProbe.FRAME_RATE;
            while (!ended && SystemClock.uptimeMillis() < deadline) {
                if (queued <= frames) {
                    int index = codec.dequeueInputBuffer(MediaProbe.DEQUEUE_US);
                    if (index >= 0) {
                        long timeUs = queued * 1_000_000L / frames;
                        if (queued == frames) {
                            codec.queueInputBuffer(index, 0, 0, timeUs, MediaCodec.BUFFER_FLAG_END_OF_STREAM);
                        } else {
                            bars(codec.getInputImage(index), width, height);
                            codec.queueInputBuffer(index, 0, width * height * 3 / 2, timeUs, 0);
                        }
                        queued++;
                    }
                }
                ended = MediaProbe.drainEncoder(codec, stream);
            }
            codec.stop();
        } finally {
            codec.release();
        }
        return stream;
    }

    private static MediaFormat formatFor(String type, int width, int height, MediaProbe.Stream stream) {
        MediaFormat format = MediaFormat.createVideoFormat(type, width, height);
        if (stream.config != null) {
            format.setByteBuffer("csd-0", ByteBuffer.wrap(stream.config));
        }
        return format;
    }

    /** One sample of an image's plane, at a picture position inside its crop. */
    private static int sample(Image image, int plane, int x, int y) {
        Rect crop = image.getCropRect();
        Image.Plane p = image.getPlanes()[plane];
        int px = crop.left + x;
        int py = crop.top + y;
        if (plane > 0) {
            px /= 2;
            py /= 2;
        }
        return p.getBuffer().get(py * p.getRowStride() + px * p.getPixelStride()) & 0xff;
    }

    /** The positions checked: each bar's middle at the top, middle and bottom rows, and the right edge. */
    private static int[][] positions(int width, int height) {
        int[] rows = {1, height / 2, height - 1};
        int[][] out = new int[4 * rows.length + rows.length][];
        int n = 0;
        for (int b = 0; b < 4; b++) {
            for (int y : rows) {
                out[n++] = new int[] {(2 * b + 1) * width / 8, y};
            }
        }
        for (int y : rows) {
            out[n++] = new int[] {width - 1, y};
        }
        return out;
    }

    /** "ok", or what is wrong with a decoded picture's colours. */
    static String imageColours(Image image, int width, int height) {
        Rect crop = image.getCropRect();
        if (crop.width() < width || crop.height() < height) {
            return "the picture is " + crop.width() + "x" + crop.height() + ", not " + width + "x" + height;
        }
        for (int[] at : positions(width, height)) {
            int[] want = BARS[bar(at[0], width)];
            int y = sample(image, 0, at[0], at[1]);
            int u = sample(image, 1, at[0], at[1]);
            int v = sample(image, 2, at[0], at[1]);
            if (u < 16 && v < 16) {
                return "no chroma (green) at " + at[0] + "," + at[1] + ": Y" + y + " Cb" + u + " Cr" + v;
            }
            if (Math.abs(y - want[0]) > YUV_TOLERANCE || Math.abs(u - want[1]) > YUV_TOLERANCE
                    || Math.abs(v - want[2]) > YUV_TOLERANCE) {
                return "wrong colour at " + at[0] + "," + at[1] + ": Y" + y + " Cb" + u + " Cr" + v
                        + ", not Y" + want[0] + " Cb" + want[1] + " Cr" + want[2];
            }
        }
        return "ok";
    }

    /** "ok", or what is wrong with what a surface shows, copied back at the picture's size. */
    private static String surfaceColours(SurfaceView view, int width, int height, Handler handler)
            throws InterruptedException {
        Bitmap bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888);
        CountDownLatch done = new CountDownLatch(1);
        int[] result = {-1};
        PixelCopy.request(view, bitmap, copied -> {
            result[0] = copied;
            done.countDown();
        }, handler);
        if (!done.await(5, TimeUnit.SECONDS)) {
            return "the copy of the surface did not finish";
        }
        if (result[0] != PixelCopy.SUCCESS) {
            return "the surface could not be copied (PixelCopy " + result[0] + ")";
        }
        for (int[] at : positions(width, height)) {
            int want = BAR_COLOURS[bar(at[0], width)];
            int got = bitmap.getPixel(at[0], at[1]);
            if (Math.abs(Color.red(got) - Color.red(want)) > RGB_TOLERANCE
                    || Math.abs(Color.green(got) - Color.green(want)) > RGB_TOLERANCE
                    || Math.abs(Color.blue(got) - Color.blue(want)) > RGB_TOLERANCE) {
                return String.format("wrong colour on screen at %d,%d: #%06x, not #%06x", at[0], at[1],
                        got & 0xffffff, want & 0xffffff);
            }
        }
        return "ok";
    }

    /**
     * "ok", or what is wrong with the bars where a surface sits on the display, in a capture
     * of the whole display as SurfaceFlinger composes it. The surface scales the picture to
     * its own size, and the bars are upright stripes, so each is looked for at the same
     * fraction of the surface's width.
     */
    private String screenColours(SurfaceView view) {
        int[] origin = new int[2];
        view.getLocationOnScreen(origin);
        int width = view.getWidth();
        int height = view.getHeight();
        if (width < 8 || height < 8) {
            return "the surface is " + width + "x" + height + " on screen";
        }
        Bitmap shot = instrumentation.getUiAutomation().takeScreenshot();
        if (shot == null) {
            return "the display could not be captured";
        }
        Bitmap screen = shot.getConfig() == Bitmap.Config.HARDWARE
                ? shot.copy(Bitmap.Config.ARGB_8888, false)
                : shot;
        if (screen == null) {
            return "the display's capture could not be read";
        }
        int[] rows = {height / 4, height / 2, height * 3 / 4};
        for (int b = 0; b < 4; b++) {
            int want = BAR_COLOURS[b];
            for (int row : rows) {
                int x = origin[0] + (2 * b + 1) * width / 8;
                int y = origin[1] + row;
                if (x < 0 || y < 0 || x >= screen.getWidth() || y >= screen.getHeight()) {
                    return String.format("the surface at %d,%d (%dx%d) is off the %dx%d display",
                            origin[0], origin[1], width, height, screen.getWidth(), screen.getHeight());
                }
                int got = screen.getPixel(x, y);
                if (Math.abs(Color.red(got) - Color.red(want)) > RGB_TOLERANCE
                        || Math.abs(Color.green(got) - Color.green(want)) > RGB_TOLERANCE
                        || Math.abs(Color.blue(got) - Color.blue(want)) > RGB_TOLERANCE) {
                    return String.format("wrong colour on screen at %d,%d: #%06x, not #%06x", x, y,
                            got & 0xffffff, want & 0xffffff);
                }
            }
        }
        return "ok";
    }

    /** Queues the clip's next unit, or its end; returns how many are queued. */
    private static int queueNext(MediaCodec codec, MediaProbe.Stream stream, int queued, long offsetUs,
            boolean configFirst) {
        if (queued > stream.units.size()) {
            return queued;
        }
        int index = codec.dequeueInputBuffer(MediaProbe.DEQUEUE_US);
        if (index < 0) {
            return queued;
        }
        ByteBuffer buffer = codec.getInputBuffer(index);
        buffer.clear();
        if (configFirst && queued == 0 && stream.config != null) {
            buffer.put(stream.config);
            codec.queueInputBuffer(index, 0, stream.config.length, offsetUs, MediaCodec.BUFFER_FLAG_CODEC_CONFIG);
            return -1;
        }
        int at = Math.max(queued, 0);
        if (at == stream.units.size()) {
            long timeUs = offsetUs + (at == 0 ? 0 : stream.units.get(at - 1).timeUs + 1);
            codec.queueInputBuffer(index, 0, 0, timeUs, MediaCodec.BUFFER_FLAG_END_OF_STREAM);
        } else {
            MediaProbe.Unit unit = stream.units.get(at);
            buffer.put(unit.data);
            codec.queueInputBuffer(index, 0, unit.data.length, offsetUs + unit.timeUs, 0);
        }
        return at + 1;
    }

    private void toBuffers(Bundle results, String key, String name, String type, MediaProbe.Stream stream,
            int width, int height) throws IOException {
        MediaCodec codec = MediaCodec.createByCodecName(name);
        int out = 0;
        String colour = "no picture came out";
        try {
            codec.configure(formatFor(type, width, height, stream), null, null, 0);
            codec.start();
            MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
            long deadline = SystemClock.uptimeMillis() + MediaProbe.CODEC_WAIT_MS;
            int queued = 0;
            boolean ended = false;
            while (!ended && SystemClock.uptimeMillis() < deadline) {
                queued = queueNext(codec, stream, queued, 0, false);
                int index = codec.dequeueOutputBuffer(info, MediaProbe.DEQUEUE_US);
                if (index < 0) {
                    continue;
                }
                if (info.size > 0 && (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) == 0) {
                    out++;
                    // The first picture, and every tenth: a later one must hold its colours too.
                    if (out == 1 || out % 10 == 0) {
                        Image image = codec.getOutputImage(index);
                        String seen = image == null ? "the decoder gave no image" : imageColours(image, width, height);
                        if (out == 1 || !seen.equals("ok")) {
                            colour = out == 1 ? seen : "picture " + out + ": " + seen;
                        }
                        if (image != null) {
                            image.close();
                        }
                    }
                }
                codec.releaseOutputBuffer(index, false);
                ended = (info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
            }
            codec.stop();
        } finally {
            codec.release();
        }
        results.putInt(key + ".out", out);
        results.putString(key + ".colour", colour);
    }

    /** Counts the pictures the codec says reached the screen, before and after a timestamp. */
    private static final class Presented implements MediaCodec.OnFrameRenderedListener {
        final AtomicInteger before = new AtomicInteger();
        final AtomicInteger after = new AtomicInteger();
        final AtomicLong splitUs = new AtomicLong(Long.MAX_VALUE);
        final AtomicLong firstAfterMs = new AtomicLong(-1);

        @Override
        public void onFrameRendered(MediaCodec codec, long presentationTimeUs, long nanoTime) {
            if (presentationTimeUs >= splitUs.get()) {
                after.incrementAndGet();
                firstAfterMs.compareAndSet(-1, SystemClock.uptimeMillis());
            } else {
                before.incrementAndGet();
            }
        }
    }

    /**
     * Plays the clip to the first surface while a tone plays, moving to the second surface
     * halfway, as a player does.
     */
    private void toSurfaces(Bundle results, String key, String name, String type, MediaProbe.Stream stream,
            int width, int height, VideoActivity activity, Handler handler) throws Exception {
        MediaCodec codec = MediaCodec.createByCodecName(name);
        Presented presented = new Presented();
        AudioTrack track = null;
        Thread sound = null;
        int rendered = 0;
        try {
            int min = AudioTrack.getMinBufferSize(MediaProbe.RATE, AudioFormat.CHANNEL_OUT_STEREO,
                    AudioFormat.ENCODING_PCM_16BIT);
            track = MediaProbe.track(AudioTrack.MODE_STREAM, Math.max(min, MediaProbe.RATE / 10 * 4));
            final AudioTrack playing = track;
            playing.play();
            sound = new Thread(() -> {
                short[] pcm = MediaProbe.tone();
                playing.write(pcm, 0, pcm.length, AudioTrack.WRITE_BLOCKING);
            }, "probe-sound");
            sound.start();

            codec.setOnFrameRenderedListener(presented, handler);
            codec.configure(formatFor(type, width, height, stream), activity.surface(0), null, 0);
            codec.start();
            MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
            long deadline = SystemClock.uptimeMillis() + MediaProbe.CODEC_WAIT_MS;
            int queued = 0;
            boolean ended = false;
            boolean swapped = false;
            while (!ended && SystemClock.uptimeMillis() < deadline) {
                queued = queueNext(codec, stream, queued, 0, false);
                int index = codec.dequeueOutputBuffer(info, MediaProbe.DEQUEUE_US);
                if (index < 0) {
                    continue;
                }
                boolean picture = info.size > 0 && (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) == 0;
                if (picture && !swapped && rendered >= stream.units.size() / 2) {
                    // The player's view changes: the rest goes to the other surface.
                    codec.setOutputSurface(activity.surface(1));
                    presented.splitUs.set(info.presentationTimeUs);
                    swapped = true;
                }
                codec.releaseOutputBuffer(index, picture);
                if (picture) {
                    rendered++;
                }
                ended = (info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
            }
            // The last pictures reach the screen, and their callbacks come.
            SystemClock.sleep(1000);
            results.putString(key + ".colour", surfaceColours(activity.view(1), width, height, handler));
            results.putString(key + ".screen", screenColours(activity.view(1)));
            codec.stop();
        } finally {
            codec.release();
            if (sound != null) {
                sound.join(3000);
            }
            if (track != null) {
                results.putInt(key + ".audio", track.getPlaybackHeadPosition());
                track.stop();
                track.release();
            }
        }
        results.putInt(key + ".rendered", rendered);
        results.putInt(key + ".presented", presented.before.get() + presented.after.get());
        results.putInt(key + ".after_swap", presented.after.get());
    }

    /**
     * Plays the clip to the first surface, then, after a flush, again to the second, as a feed
     * of short videos moves from one item to the next.
     */
    private void feed(Bundle results, String key, String name, String type, MediaProbe.Stream stream,
            int width, int height, VideoActivity activity, Handler handler) throws Exception {
        MediaCodec codec = MediaCodec.createByCodecName(name);
        Presented presented = new Presented();
        presented.splitUs.set(SECOND_CLIP_US);
        long secondStart = 0;
        try {
            codec.setOnFrameRenderedListener(presented, handler);
            codec.configure(formatFor(type, width, height, stream), activity.surface(0), null, 0);
            codec.start();
            for (int clip = 0; clip < 2; clip++) {
                long offset = clip == 0 ? 0 : SECOND_CLIP_US;
                if (clip == 1) {
                    codec.flush();
                    codec.setOutputSurface(activity.surface(1));
                    secondStart = SystemClock.uptimeMillis();
                }
                MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
                long deadline = SystemClock.uptimeMillis() + MediaProbe.CODEC_WAIT_MS;
                int queued = 0;
                boolean ended = false;
                while (!ended && SystemClock.uptimeMillis() < deadline) {
                    // After a flush the parameter sets go again, ahead of the clip.
                    queued = queueNext(codec, stream, queued, offset, clip == 1);
                    int index = codec.dequeueOutputBuffer(info, MediaProbe.DEQUEUE_US);
                    if (index < 0) {
                        continue;
                    }
                    boolean picture = info.size > 0 && (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) == 0;
                    codec.releaseOutputBuffer(index, picture);
                    ended = (info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
                }
                SystemClock.sleep(clip == 0 ? 300 : 1000);
            }
            // The feed's sound would start with the second clip's first picture.
            long deadline = secondStart + FIRST_FRAME_WAIT_MS;
            while (presented.firstAfterMs.get() < 0 && SystemClock.uptimeMillis() < deadline) {
                SystemClock.sleep(50);
            }
            results.putString(key + ".colour", surfaceColours(activity.view(1), width, height, handler));
            results.putString(key + ".screen", screenColours(activity.view(1)));
            codec.stop();
        } finally {
            codec.release();
        }
        long first = presented.firstAfterMs.get();
        results.putInt(key + ".first", presented.before.get());
        results.putInt(key + ".second", presented.after.get());
        results.putLong(key + ".first_frame_ms", first < 0 ? -1 : first - secondStart);
    }
}
