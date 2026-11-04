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
4. Each picture comes back as a `VideoFrame` (NV12), in presentation order. The component
   copies it into a graphics buffer from the framework's output pool: a buffer of the app's
   surface (through `IGraphicBufferAllocator`), or a buffer of its own when the app reads the
   pictures itself, and finishes the work the picture came from.

The store asks the host which formats it decodes when it starts, and lists only those. The
framework builds its list of codecs once a boot, so the first list waits up to 20 s for the
host's answer. A format the host does not decode stays with the guest's own decoder.

The component describes its parameters with the platform's own Codec 2.0 helpers, built from
the fetched sources (`fetch.sh`), so the framework configures it as it configures the
platform's decoders. `shim/` stands in for the parts of libbase, libutils and libcutils those
sources name, which an NDK build cannot link.

The image sets `debug.stagefright.c2-poolmask` to keep the framework's codec buffers out of
buffer pools: the HAL takes its input as handles. Output to a surface keeps its pool, which
reaches the HAL as the surface's allocator.

## Building and testing

- `build.sh [scratch] [out]` fetches the pinned sources and builds `dsa-codec2` with the NDK.
- `test.sh` builds and runs the rules on the build machine: the video port's messages against
  the protocol vectors and the copy of a picture into a buffer's planes.

`make-image.py --codec2 <out>/dsa-codec2` installs it with its init script, VINTF fragment,
label (`mediacodec`), service label, `media_codecs_c2.xml` and `sepolicy/codec2.cil`.

## Checking on a running guest

- `logcat -s dsa-codec2` shows `serving android.hardware.media.c2.IComponentStore/default`,
  then `the host decodes format 1 up to 4096x2304 on its GPU` for each format.
- Playing an H.264 video logs `c2.dsa.avc.decoder: decoding WxH on the host's GPU`.
- The window manager's log on the host shows `video: H.264 decodes on the GPU` at start.
