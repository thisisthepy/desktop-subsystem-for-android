# guest/codec2

`dsa-codec2` is the guest's Codec 2.0 HAL (`android.hardware.media.c2`, version 1,
`IComponentStore/default`). Its video decoders decode on the host's GPU: H.264, HEVC, VP9
and AV1, each offered only when the host's GPU decodes it.

## How a picture travels

1. An app creates a decoder (`MediaCodec`). The framework picks `c2.dsa.<format>.decoder`
   ahead of the guest's software decoder (rank 128 against 512), when the store lists it.
2. The component opens a connection to the host's video port (vsock 5569). The host's
   window manager answers: it decodes with Media Foundation on a Direct3D 11 device, so
   the GPU's video engine does the work (`dsa/host/platform/windows/src/video_decoder.rs`).
3. Each access unit the app queues is copied out of the framework's input buffer, sent as a
   `VideoPacket`, and the input buffer goes back to the framework at once.
4. Each picture comes back as a `VideoFrame` (NV12, its visible size only), in presentation
   order. The component copies it into a graphics buffer from the framework's output pool: a
   buffer of the app's surface (through `IGraphicBufferAllocator`), or a buffer of its own when
   the app reads the pictures itself, and finishes the work the picture came from with the
   picture's rectangle as the block's crop.

The buffers are YV12 (`src/output_buffer.h`), as the platform's software decoders allocate
for a surface, at the picture's size rounded up to 2; samples past the picture repeat its
edge. The guest's gralloc has no NV12 buffer the composer may use, so a dequeue of an NV12
buffer from an app's surface failed (`C2_CORRUPTED`) on every picture and no video reached
the screen. While every buffer of the surface is shown or queued the allocator answers
`C2_BLOCKING`, and the component waits on its waitable descriptor, up to 2 s, before it drops
the picture; a flush or a stop ends the wait. A buffer that comes back is written only once its
release fence signals (up to 1 s), so a picture never lands in a buffer the screen still shows.
A picture goes out only in a buffer that holds all of it: a buffer that is too small, cannot
be locked or unlocked, or a picture shorter than its size, ends the work with no output and
gives the buffer back to the surface (`src/picture_copy.h`), rather than showing zeros (green)
or an older picture. A failure that repeats is logged once in a hundred.

Packets go to the host through a queue that a thread of the link's own writes
(`src/send_queue.h`), so the framework's `queue()` call never waits on the host. The host
decodes on one thread and reads no packet while the guest has not taken its last picture, and
the app gives the surface's buffers back from the thread that calls `queue()`: when
`queue()` wrote to the socket itself, each picture waited its full 2 s for a buffer and
`queue()` took 8 s. A flush drops every picture that arrives before the host's answer without
waiting for a buffer (`src/output_gate.h`), so the flush returns as soon as the host answers.

The host's pictures wait for a thread of the component's own in a short queue
(`src/arrival_queue.h`: four pictures, 32 MiB at most). The thread that reads the socket only
puts each picture there, so the host decodes the next ones while a picture waits for a buffer
of the app's surface; when the reader wrote each picture itself, the host's decoder stood
still for every such wait, and the stream played at the pace of its slowest buffer. A full
queue holds the host back, and a flush's answer is taken at once: the pictures before it are
dropped as the thread comes to them.

Every 5 s while a video plays, each decoder logs its numbers (`src/picture_stats.h`): the
pictures from the host and those shown, each a second; the pictures dropped and why (flushed,
no buffer came back, not written); and the average time a picture spent queued, waiting for a
buffer and its release fence, being written, and being handed to the framework. The host logs
its side the same way (`video: AV1 640x360 over 5.0 s: ...` in the window manager's log): the
packets decoded, the copy out of its GPU, the wait for the guest to take a picture, the send,
and how the decoder's surface was laid out.

The store asks the host which formats it decodes when it starts, and lists only those. The
framework builds its list of codecs once a boot, so the first list waits up to 20 s for the
host's answer. A format the host does not decode stays with the guest's own decoder.

The component describes its parameters with the platform's own Codec 2.0 helpers, built from
the fetched sources (`fetch.sh`), so the framework configures it as it configures the
platform's decoders. The output pixel format is settable rather than a constant
(`src/config_rules.h`): the framework sets it to the surface's format for an app that shows
its video, and the helper answers a constant set to another value with `C2_CORRUPTED`, which
failed every such configure. `shim/` stands in for the parts of libbase, libutils and libcutils those
sources name, which an NDK build cannot link.

The image sets `debug.stagefright.c2-poolmask` to keep the framework's codec buffers out of
buffer pools: the HAL takes its input as handles. Output to a surface keeps its pool, which
reaches the HAL as the surface's allocator.

As the device's default store, it is the store every process that uses a codec asks how to
allocate linear memory: the input of every decoder, the software ones included, and the
output of audio decoders. It answers as the platform's own store does (`dmabuf-usage`: the
`system` heap, no flags; `ion-usage`: any heap). A store that says it knows those parameters
must also answer them, because the framework then asks it for each allocation and fails the
allocation when it refuses: before the store answered, every `MediaCodec.start()` in an app
failed with NO_MEMORY. The image workflow's boot check decodes H.264 and AAC with every
decoder in an ordinary app (`dsa/host/platform/linux/tests/media.rs`). It also serves the
video port with a host decoder of its own, so DSA's H.264 and VP9 decoders are listed, and
decodes colour bars at 426x240 and 360x640 with them and the guest's own decoders, to buffers
and to surfaces on screen that change partway, as a player and a feed of short videos do.

## Building and testing

- `build.sh [scratch] [out]` fetches the pinned sources and builds `dsa-codec2` with the NDK.
- `test.sh` builds and runs the rules on the build machine: the video port's messages against
  the protocol vectors, the copy of a picture into a buffer's planes, the output buffers'
  format, size, crop and allocation failures, the pictures that never go out, the waits a
  flush ends, the queue of packets to the host, the queue of pictures from it, and the numbers
  the log reports.

`make-image.py --codec2 <out>/dsa-codec2` installs it with its init script, VINTF fragment,
label (`mediacodec`), service label, `media_codecs_c2.xml` and `sepolicy/codec2.cil`.

## Checking on a running guest

- `logcat -s dsa-codec2` shows `serving android.hardware.media.c2.IComponentStore/default`,
  then `the host decodes format 1 up to 4096x2304 on its GPU` for each format.
- Playing an H.264 video logs `c2.dsa.avc.decoder: decoding WxH on the host's GPU`, then a
  line every 5 s such as `c2.dsa.avc.decoder 640x360 over 5.0 s: 150 pictures from the host
  (30.0/s), 150 shown (30.0/s), ...`.
- The window manager's log on the host shows `video: H.264 decodes on the GPU` at start.
