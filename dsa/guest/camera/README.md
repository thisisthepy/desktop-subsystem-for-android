# guest/camera

`dsa-camera` is the guest's camera provider HAL (`android.hardware.camera.provider`,
version 1, `ICameraProvider/internal/0`). It lends the host computer's cameras to Android:
each camera Windows has becomes an Android camera, so camera apps, video calls and any app
that takes a picture use the computer's own camera. It replaces Cuttlefish's emulated
cameras.

Apps reach it through Android's camera service (`cameraserver`), as they reach any camera,
so Android's camera permission applies as usual. Windows applies its own privacy settings on
the host: a camera the user has blocked there is not offered to Android at all.

## How a picture travels

1. The provider asks the host's camera port (vsock 5572) for the host's cameras when it
   starts, and every 3 seconds after. The host's window manager answers with each camera's
   name, where its enclosure places it, how it is turned, and the sizes and frame rates it
   delivers (`dsa/host/platform/windows/src/camera_capture.rs`). A camera that comes or goes
   (plugged in or out, blocked or allowed in the privacy settings, the window manager
   started or stopped) is reported to the camera service.
2. Each camera is described at Android's EXTERNAL hardware level: a camera on the front of
   the screen faces front (`LENS_FACING_FRONT`), one on the back faces back, and any other
   (a USB camera, or one Windows does not place) is external. Its stream sizes and rates are
   the modes the host camera has, and the sizes apps ask for without looking (320x240 to
   1920x1080, CameraX's defaults among them) that one of those modes is at least as large as.
   A preview the screen shows (a SurfaceView or TextureView) is RGBA, because the guest's
   allocator (minigbm on vkms, its dumb driver) makes no YUV buffer the composer may show;
   video and the streams apps read are YUV, stills are JPEG. The HAL writes every stream by
   CPU, and asks for `CAMERA_OUTPUT` only on YUV and JPEG: the allocator makes no RGBA buffer
   with it, so a preview with it got no buffers.
3. When an app configures its streams, the session opens its own connection and starts the
   host camera at the smallest mode its largest picture stream fits in, one of the stream's
   shape first. A stream of another shape is cropped at the centre, not stretched. The host holds the camera open
   from then until the session closes, so the camera light and the taskbar's in-use
   indicator show only while an app uses it.
4. Each picture comes as a `CameraFrame` (NV12). For each capture request the session takes
   the newest picture and scales it into every buffer of the request; a still asks the host
   for a JPEG of the newest picture (`CameraStill`), which the host encodes, and puts it in
   the still buffer with the trailer Android reads its length from.

The camera metadata buffers are built with the platform's own camera metadata code
(`system/media/camera`), built from the fetched sources (`fetch.sh`). `shim/` stands in for
the two platform headers that code names. The apps' buffers arrive as graphics buffer
handles, which the HAL imports with the platform's `AHardwareBuffer_createFromHandle`; the
NDK leaves that call out, so the build links against a stand-in library and the HAL loads
the platform's own `libnativewindow.so` on the device.

## Building and testing

- `build.sh [scratch] [out]` fetches the pinned sources and builds `dsa-camera` with the NDK.
- `test.sh` builds and runs the rules on the build machine: the camera port's messages
  against the protocol vectors, the sizes, rates and stream combinations a camera offers,
  the copy of a picture into a buffer, and the still's trailer.

`make-image.py --camera <out>/dsa-camera` installs it with its init script, VINTF fragment,
label (`hal_camera_default`), service label, the camera features, and
`sepolicy/camera.cil`, and removes Cuttlefish's emulated camera APEX.

## Checking on a running guest

- `logcat -s dsa-camera` shows `serving android.hardware.camera.provider.ICameraProvider/internal/0`,
  then `camera 0 is Integrated Camera, facing 1, turned 0 degrees` for each host camera.
- `dumpsys media.camera` lists each camera with its facing and stream configurations.
- Opening the camera app logs `Integrated Camera runs at 1280x720, 30 frames a second`, and
  closing it `Integrated Camera closed; the host camera is let go`.
- The window manager's log on the host shows `camera: ...` for a camera it cannot offer.
