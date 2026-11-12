// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

package io.github.thisisthepy.dsa.mediaprobe;

import android.app.Activity;
import android.app.Instrumentation;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioTrack;
import android.media.Image;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.SystemClock;
import android.system.ErrnoException;
import android.system.Os;
import android.system.OsConstants;
import android.util.Log;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;

/**
 * Plays sound and runs codecs in an ordinary app's process, and reports what happened as the
 * instrumentation's results, one key per line of `am instrument -r`:
 *
 * <pre>
 *   am instrument -w -r -e check audio io.github.thisisthepy.dsa.mediaprobe/.MediaProbe
 *   am instrument -w -r -e check decode io.github.thisisthepy.dsa.mediaprobe/.MediaProbe
 * </pre>
 *
 * The "audio" check plays a second of a 440 Hz tone through a streaming track and a static
 * one. A streaming track's control block, and a static track's shared buffer, are memory an
 * app shares with the audio server, which is what an app whose process the policy keeps apart
 * cannot do when the rules are missing.
 *
 * The "decode" check encodes a second of H.264 video and of AAC sound with the device's
 * encoders, then decodes each stream with every decoder the device lists for its type (DSA's
 * decoder on the host's GPU, when the host offers it, and the software ones), to buffers the
 * app reads. A codec's input buffers are memory the framework allocates in the app's process,
 * the way the device's codec store says to: when the store could not say, every start() failed
 * with NO_MEMORY.
 *
 * The "video" check ({@link VideoCheck}) decodes clips of colour bars at sizes that are not a
 * multiple of 16 or 64 to buffers and to surfaces on screen, swapping surfaces as players and
 * feeds of short videos do, and checks the pictures' colours and that they keep reaching the
 * screen.
 *
 * The "https" check ({@link HttpsCheck}) opens the address the "url" argument names (at the
 * "address" argument, when given) and says whether the platform's certificate check let it
 * through, and what the app sees of the certificate file the "certificate" argument names.
 */
public final class MediaProbe extends Instrumentation {
    static final String TAG = "DsaMediaProbe";
    static final int RATE = 48_000;
    static final int CHANNELS = 2;
    private static final int FRAME_BYTES = 2 * CHANNELS;

    private static final String AVC = MediaFormat.MIMETYPE_VIDEO_AVC;
    private static final String AAC = MediaFormat.MIMETYPE_AUDIO_AAC;
    private static final int WIDTH = 320;
    private static final int HEIGHT = 240;
    static final int FRAME_RATE = 30;
    /** How long one codec may take over a second of media. */
    static final long CODEC_WAIT_MS = 20_000;
    static final long DEQUEUE_US = 10_000;

    private String check = "audio";
    /** The address the "https" check opens ({@link HttpsCheck}). */
    private String url = "https://localhost:8443/";
    /** Where the "https" check connects instead of looking the url's host up, if anywhere. */
    private String address;
    /** The certificate file the "https" check looks for in Android's folders, if any. */
    private String certificate;

    /** One access unit an encoder produced. */
    static final class Unit {
        final byte[] data;
        final long timeUs;

        Unit(byte[] data, long timeUs) {
            this.data = data;
            this.timeUs = timeUs;
        }
    }

    /** An encoded stream: its codec configuration and its access units. */
    static final class Stream {
        byte[] config;
        final List<Unit> units = new ArrayList<>();
    }

    @Override
    public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        if (arguments != null && arguments.getString("check") != null) {
            check = arguments.getString("check");
        }
        if (arguments != null && arguments.getString("url") != null) {
            url = arguments.getString("url");
        }
        if (arguments != null) {
            address = arguments.getString("address");
            certificate = arguments.getString("certificate");
        }
        start();
    }

    @Override
    public void onStart() {
        Bundle results = new Bundle();
        try {
            if (check.equals("audio")) {
                audio(results);
            } else if (check.equals("decode")) {
                decode(results);
            } else if (check.equals("video")) {
                new VideoCheck(this).run(results);
            } else if (check.equals("root_absent")) {
                rootAbsent(results);
            } else if (check.equals("root_visible")) {
                rootVisible(results);
            } else if (check.equals("root_request")) {
                rootRequest(results);
            } else if (check.equals("https")) {
                HttpsCheck.run(url, address, certificate, results);
            } else {
                results.putString("error", "no check named " + check);
            }
        } catch (Throwable t) {
            Log.e(TAG, "the " + check + " check failed", t);
            results.putString("error", t.toString());
        }
        finish(Activity.RESULT_OK, results);
    }

    private void rootVisible(Bundle results) {
        try {
            Os.stat("/system/bin/su");
            results.putString("root.visible", "yes");
        } catch (ErrnoException e) {
            results.putString("root.visible", e.errno == OsConstants.ENOENT ? "ENOENT" : e.toString());
        }
    }

    private void rootRequest(Bundle results) throws Exception {
        Process process = new ProcessBuilder("/system/bin/su", "-c", "id")
                .redirectErrorStream(true).start();
        String output = new String(process.getInputStream().readAllBytes(), java.nio.charset.StandardCharsets.UTF_8);
        results.putString("root.output", output.trim());
        results.putInt("root.exit", process.waitFor());
    }

    /** Checks su from the app's own uid while root elevation is off. */
    private void rootAbsent(Bundle results) throws Exception {
        String[] paths = {"/system/bin/su", "/system/xbin/su", "/sbin/su"};
        for (String path : paths) {
            String key = "root." + path;
            try {
                Os.stat(path);
                results.putString(key + ".stat", "visible");
            } catch (ErrnoException e) {
                results.putString(key + ".stat", e.errno == OsConstants.ENOENT ? "ENOENT" : e.toString());
            }
            try {
                Os.stat("/proc/self/root" + path);
                results.putString(key + ".procstat", "visible");
            } catch (ErrnoException e) {
                results.putString(key + ".procstat", e.errno == OsConstants.ENOENT ? "ENOENT" : e.toString());
            }
            try {
                results.putString(key + ".access", Os.access(path, OsConstants.X_OK) ? "visible" : "false");
            } catch (ErrnoException e) {
                results.putString(key + ".access", e.errno == OsConstants.ENOENT ? "ENOENT" : e.toString());
            }
            try {
                Process process = new ProcessBuilder(path, "-c", "id").start();
                results.putString(key + ".exec", "started:" + process.waitFor());
            } catch (IOException e) {
                results.putString(key + ".exec", e.getMessage() != null
                        && (e.getMessage().contains("ENOENT") || e.getMessage().contains("error=2"))
                        ? "ENOENT" : e.toString());
            }
        }
        Process lookup = new ProcessBuilder("/system/bin/sh", "-c", "command -v su").start();
        results.putInt("root.path_lookup_exit", lookup.waitFor());
    }

    /** One second of a 440 Hz tone, 16-bit stereo at 48 kHz. */
    static short[] tone() {
        short[] pcm = new short[RATE * CHANNELS];
        for (int i = 0; i < RATE; i++) {
            short sample = (short) (Math.sin(2 * Math.PI * 440 * i / RATE) * 8000);
            pcm[CHANNELS * i] = sample;
            pcm[CHANNELS * i + 1] = sample;
        }
        return pcm;
    }

    static AudioTrack track(int mode, int bytes) {
        return new AudioTrack.Builder()
                .setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build())
                .setAudioFormat(new AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(RATE)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                        .build())
                .setTransferMode(mode)
                .setBufferSizeInBytes(bytes)
                .build();
    }

    /** Waits up to `wait` ms for the track to have played at least `frames` frames. */
    private static int playedFrames(AudioTrack track, int frames, long wait) {
        long deadline = SystemClock.uptimeMillis() + wait;
        int played = track.getPlaybackHeadPosition();
        while (played < frames && SystemClock.uptimeMillis() < deadline) {
            SystemClock.sleep(50);
            played = track.getPlaybackHeadPosition();
        }
        return played;
    }

    private void audio(Bundle results) {
        short[] pcm = tone();

        // A streaming track: the app writes into the control block's ring as it plays.
        AudioTrack stream = null;
        try {
            int min = AudioTrack.getMinBufferSize(RATE, AudioFormat.CHANNEL_OUT_STEREO,
                    AudioFormat.ENCODING_PCM_16BIT);
            stream = track(AudioTrack.MODE_STREAM, Math.max(min, RATE / 10 * FRAME_BYTES));
            results.putInt("audio.stream.state", stream.getState());
            stream.play();
            int written = stream.write(pcm, 0, pcm.length, AudioTrack.WRITE_BLOCKING);
            results.putInt("audio.stream.written", written < 0 ? written : written / CHANNELS);
            results.putInt("audio.stream.played", playedFrames(stream, RATE / 2, 3000));
            stream.stop();
        } catch (RuntimeException e) {
            Log.e(TAG, "the streaming track failed", e);
            results.putString("audio.stream.error", e.toString());
        } finally {
            if (stream != null) {
                stream.release();
            }
        }

        // A static track: the app's whole sound in a buffer it shares with the audio server.
        AudioTrack fixed = null;
        try {
            fixed = track(AudioTrack.MODE_STATIC, pcm.length * 2);
            int written = fixed.write(pcm, 0, pcm.length);
            results.putInt("audio.static.written", written < 0 ? written : written / CHANNELS);
            results.putInt("audio.static.state", fixed.getState());
            fixed.play();
            results.putInt("audio.static.played", playedFrames(fixed, RATE / 2, 3000));
            fixed.stop();
        } catch (RuntimeException e) {
            Log.e(TAG, "the static track failed", e);
            results.putString("audio.static.error", e.toString());
        } finally {
            if (fixed != null) {
                fixed.release();
            }
        }
    }

    private static byte[] bytes(ByteBuffer buffer, MediaCodec.BufferInfo info) {
        byte[] data = new byte[info.size];
        buffer.position(info.offset);
        buffer.limit(info.offset + info.size);
        buffer.get(data);
        return data;
    }

    /** Takes what an encoder has finished, until its end of stream; true once it ended. */
    static boolean drainEncoder(MediaCodec codec, Stream stream) {
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        while (true) {
            int index = codec.dequeueOutputBuffer(info, DEQUEUE_US);
            if (index < 0) {
                return false;
            }
            ByteBuffer buffer = codec.getOutputBuffer(index);
            if (info.size > 0 && buffer != null) {
                byte[] data = bytes(buffer, info);
                if ((info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0) {
                    stream.config = data;
                } else {
                    stream.units.add(new Unit(data, info.presentationTimeUs));
                }
            }
            codec.releaseOutputBuffer(index, false);
            if ((info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0) {
                return true;
            }
        }
    }

    /** A picture of a moving gradient in a flexible YUV 4:2:0 image. */
    private static void picture(Image image, int frame) {
        Image.Plane[] planes = image.getPlanes();
        for (int plane = 0; plane < 3; plane++) {
            ByteBuffer data = planes[plane].getBuffer();
            int rowStride = planes[plane].getRowStride();
            int pixelStride = planes[plane].getPixelStride();
            int width = plane == 0 ? WIDTH : WIDTH / 2;
            int height = plane == 0 ? HEIGHT : HEIGHT / 2;
            for (int y = 0; y < height; y++) {
                for (int x = 0; x < width; x++) {
                    int value = plane == 0 ? (x + y + 4 * frame) & 0xff : 128;
                    data.put(y * rowStride + x * pixelStride, (byte) value);
                }
            }
        }
    }

    static String encoderFor(MediaFormat format) throws IOException {
        String name = new MediaCodecList(MediaCodecList.REGULAR_CODECS).findEncoderForFormat(format);
        if (name == null) {
            throw new IOException("no encoder for " + format);
        }
        return name;
    }

    /** A second of the moving gradient, encoded as H.264. */
    private static Stream encodeVideo(Bundle results) throws IOException {
        MediaFormat format = MediaFormat.createVideoFormat(AVC, WIDTH, HEIGHT);
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible);
        format.setInteger(MediaFormat.KEY_BIT_RATE, 1_000_000);
        format.setInteger(MediaFormat.KEY_FRAME_RATE, FRAME_RATE);
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1);
        String name = encoderFor(format);
        results.putString("encode.avc.codec", name);
        MediaCodec codec = MediaCodec.createByCodecName(name);
        Stream stream = new Stream();
        try {
            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            codec.start();
            long deadline = SystemClock.uptimeMillis() + CODEC_WAIT_MS;
            int queued = 0;
            boolean ended = false;
            while (!ended && SystemClock.uptimeMillis() < deadline) {
                if (queued <= FRAME_RATE) {
                    int index = codec.dequeueInputBuffer(DEQUEUE_US);
                    if (index >= 0) {
                        long timeUs = queued * 1_000_000L / FRAME_RATE;
                        if (queued == FRAME_RATE) {
                            codec.queueInputBuffer(index, 0, 0, timeUs,
                                    MediaCodec.BUFFER_FLAG_END_OF_STREAM);
                        } else {
                            picture(codec.getInputImage(index), queued);
                            codec.queueInputBuffer(index, 0, WIDTH * HEIGHT * 3 / 2, timeUs, 0);
                        }
                        queued++;
                    }
                }
                ended = drainEncoder(codec, stream);
            }
            codec.stop();
        } finally {
            codec.release();
        }
        results.putInt("encode.avc.units", stream.units.size());
        return stream;
    }

    /** A second of the 440 Hz tone, encoded as AAC (LC). */
    private static Stream encodeAudio(Bundle results) throws IOException {
        MediaFormat format = MediaFormat.createAudioFormat(AAC, RATE, CHANNELS);
        format.setInteger(MediaFormat.KEY_AAC_PROFILE,
                MediaCodecInfo.CodecProfileLevel.AACObjectLC);
        format.setInteger(MediaFormat.KEY_BIT_RATE, 128_000);
        String name = encoderFor(format);
        results.putString("encode.aac.codec", name);
        MediaCodec codec = MediaCodec.createByCodecName(name);
        Stream stream = new Stream();
        short[] pcm = tone();
        try {
            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            codec.start();
            long deadline = SystemClock.uptimeMillis() + CODEC_WAIT_MS;
            int sample = 0;
            boolean sentEnd = false;
            boolean ended = false;
            while (!ended && SystemClock.uptimeMillis() < deadline) {
                if (!sentEnd) {
                    int index = codec.dequeueInputBuffer(DEQUEUE_US);
                    if (index >= 0) {
                        ByteBuffer buffer = codec.getInputBuffer(index);
                        long timeUs = (sample / CHANNELS) * 1_000_000L / RATE;
                        if (sample >= pcm.length) {
                            codec.queueInputBuffer(index, 0, 0, timeUs,
                                    MediaCodec.BUFFER_FLAG_END_OF_STREAM);
                            sentEnd = true;
                        } else {
                            buffer.clear();
                            buffer.order(java.nio.ByteOrder.LITTLE_ENDIAN);
                            int count = Math.min(pcm.length - sample, 1024 * CHANNELS);
                            count = Math.min(count, buffer.capacity() / 2 / CHANNELS * CHANNELS);
                            for (int i = 0; i < count; i++) {
                                buffer.putShort(pcm[sample + i]);
                            }
                            codec.queueInputBuffer(index, 0, count * 2, timeUs, 0);
                            sample += count;
                        }
                    }
                }
                ended = drainEncoder(codec, stream);
            }
            codec.stop();
        } finally {
            codec.release();
        }
        results.putInt("encode.aac.units", stream.units.size());
        return stream;
    }

    /** The decoders the device lists for a type, aliases left out. */
    static List<String> decodersFor(String type) {
        List<String> names = new ArrayList<>();
        for (MediaCodecInfo info : new MediaCodecList(MediaCodecList.REGULAR_CODECS).getCodecInfos()) {
            if (info.isEncoder() || info.isAlias()) {
                continue;
            }
            for (String supported : info.getSupportedTypes()) {
                if (supported.equalsIgnoreCase(type)) {
                    names.add(info.getName());
                }
            }
        }
        return names;
    }

    /**
     * Decodes `stream` with the decoder `name` to buffers the app reads, and returns what came
     * out: pictures for video, sample frames for sound.
     */
    private static int decodeWith(String name, MediaFormat format, Stream stream, boolean video)
            throws IOException {
        MediaCodec codec = MediaCodec.createByCodecName(name);
        int out = 0;
        try {
            format.setByteBuffer("csd-0", ByteBuffer.wrap(stream.config));
            codec.configure(format, null, null, 0);
            codec.start();
            MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
            long deadline = SystemClock.uptimeMillis() + CODEC_WAIT_MS;
            int queued = 0;
            boolean ended = false;
            while (!ended && SystemClock.uptimeMillis() < deadline) {
                if (queued <= stream.units.size()) {
                    int index = codec.dequeueInputBuffer(DEQUEUE_US);
                    if (index >= 0) {
                        if (queued == stream.units.size()) {
                            long timeUs = queued == 0 ? 0 : stream.units.get(queued - 1).timeUs + 1;
                            codec.queueInputBuffer(index, 0, 0, timeUs,
                                    MediaCodec.BUFFER_FLAG_END_OF_STREAM);
                        } else {
                            Unit unit = stream.units.get(queued);
                            ByteBuffer buffer = codec.getInputBuffer(index);
                            buffer.clear();
                            buffer.put(unit.data);
                            codec.queueInputBuffer(index, 0, unit.data.length, unit.timeUs, 0);
                        }
                        queued++;
                    }
                }
                int index = codec.dequeueOutputBuffer(info, DEQUEUE_US);
                if (index >= 0) {
                    if (info.size > 0 && (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) == 0) {
                        out += video ? 1 : info.size / FRAME_BYTES;
                    }
                    codec.releaseOutputBuffer(index, false);
                    ended = (info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
                }
            }
            codec.stop();
        } finally {
            codec.release();
        }
        return out;
    }

    private static void decodeAll(Bundle results, String key, String type, Stream stream,
            boolean video) {
        List<String> names = decodersFor(type);
        results.putString("decode." + key + ".decoders", String.join(",", names));
        for (String name : names) {
            MediaFormat format = video
                    ? MediaFormat.createVideoFormat(type, WIDTH, HEIGHT)
                    : MediaFormat.createAudioFormat(type, RATE, CHANNELS);
            try {
                results.putInt("decode." + name + ".out", decodeWith(name, format, stream, video));
            } catch (IOException | RuntimeException e) {
                Log.e(TAG, name + " failed", e);
                results.putString("decode." + name + ".error", e.toString());
            }
        }
    }

    private void decode(Bundle results) {
        Stream video = null;
        try {
            video = encodeVideo(results);
        } catch (IOException | RuntimeException e) {
            Log.e(TAG, "the H.264 encoder failed", e);
            results.putString("encode.avc.error", e.toString());
        }
        if (video != null && video.config != null && !video.units.isEmpty()) {
            decodeAll(results, "avc", AVC, video, true);
        }
        Stream sound = null;
        try {
            sound = encodeAudio(results);
        } catch (IOException | RuntimeException e) {
            Log.e(TAG, "the AAC encoder failed", e);
            results.putString("encode.aac.error", e.toString());
        }
        if (sound != null && sound.config != null && !sound.units.isEmpty()) {
            decodeAll(results, "aac", AAC, sound, false);
        }
    }
}
