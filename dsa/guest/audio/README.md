# guest/audio

`dsa-audio` is the guest's primary audio module (`android.hardware.audio.core.IModule/default`,
version 4). audioserver mixes the sound of each app with a window into an output of the app's
own, and every other sound into the shared output, and the module sends each output to the
host, which plays it on the user's default output device as that app, in the host's own
volume mixer. Its input is the host's microphone: what the user's default capture device
records reaches apps that record, through the same port.

## How sound travels

1. audioserver asks the module for its ports and routes. The shared output is the mix port
   `primary output` (48 kHz, stereo, 16-bit) and the device port `Speaker`, the default
   output device. Each app output (eight, `src/app_outputs.h`) is a mix port of the same
   format and a bus device of its own (`dsa_app_0` to `dsa_app_7`). The mixer converts every
   app's sound to that format before it reaches the module.
2. audioserver opens a stream on each mix port. The module answers with three shared
   memory queues, laid out the way the platform's fast message queue lays them out
   (`src/shared_queue.h`): commands, replies and sound.
3. A thread of the stream takes each command (`src/output_stream_logic.cpp`). A burst's
   sound goes to the host at once, and the burst returns once the sound may be handed on
   without the host's queue growing. The pace comes from the guest's own clock, never from
   the host, so a slow or absent host never holds up the mixer.
4. Each stream has its own connection to the host's audio port, vsock port 5568
   (`src/host_link.cpp`). It says `Hello` as the guest's audio HAL, opens the stream with
   `AudioOutputOpen`, named `primary` or `app 0` to `app 7`, and then sends `AudioPcm` and
   `AudioOutputControl`. The messages are
   the ones `dsa/protocol/control.proto` defines, through the generated codec.
5. On the host, the window manager plays each stream in the user's session (see
   `dsa/host/platform/windows/src/compositor/audio.rs`), after a 40 ms queue that absorbs
   late deliveries.

While the host is not there (the user signed out, the window manager restarts), the
stream's sound is dropped and the module connects again each second. The guest plays on,
silent, and nothing in it waits.

## Volume

The host's volume is the only one the guest's sound has, so the sound is never turned down
twice. Every output device has a gain for volume (`AudioGain.useForVolume`). Audio policy then
sets each stream's volume as the device's gain, through `setAudioPortConfig`, and leaves the
mixer at full scale: an app's sound leaves the guest as the app made it, whatever Android's
media volume is. The module keeps the gain it is given and never applies it. It has no master
volume or mute.

Android's media volume still works for apps: `setStreamVolume`, `getStreamVolume` and the
volume keys behave as on a phone. The agent routes each app with a window to an app output
(an audio policy that matches the app's uid to the output's bus device), tells the host which
app each output plays and the volume Android gives it (`AppAudio`), and makes a volume the
user sets in the host's mixer Android's media volume (`SetAppVolume`). See the agent's README.

The module has no external devices and no direct or offloaded outputs.

## How the microphone travels

1. The module has a second mix port, primary input (48 kHz, stereo, 16-bit), and a
   device port, Built-In Mic, routed into it. audioserver converts to whatever an app
   asked for.
2. An app records only with the RECORD_AUDIO permission, which the framework checks
   before it opens a stream; nothing here changes that.
3. When audioserver opens an input stream, the module connects to the host's audio port
   (src/input_link.cpp), says Hello and sends AudioInputOpen. The host opens the
   user's default capture device and answers AudioInputOpened. When the host lets no
   program record (the user turned the microphone off for desktop apps in the privacy
   settings) or has no microphone, the answer carries the reason, the open fails, and the
   app's recording fails with it.
4. While the stream is open, the host sends what it records as AudioPcm, and the link
   queues it (src/capture_queue.cpp). The stream's thread
   (src/input_stream_logic.cpp) hands it to the framework at the guest's own pace, and
   gives silence for what has not arrived, so a slow host never holds up the recording.
   setMicMute silences it.
5. Closing the stream closes the connection, and the host closes its capture device, so
   the device is open only while an app records.

## In the image

The image's audio APEX keeps serving the remote submix and Bluetooth modules, the audio
configuration and the effects. It makes a module for each one its policy file
(`/vendor/etc/audio_policy_configuration.xml`) lists, so `make-image.py` removes the
primary module from that file, and `dsa-audio` alone registers `IModule/default`, which the
APEX's manifest already declares. It runs in the audio HAL's domain
(`hal_audio_default`), and `dsa/guest/image/sepolicy/audio.cil` lets that domain reach the
host over vsock.

## Building

```
dsa/guest/audio/build.sh .scratch .scratch/build-audio
```

It fetches the pinned AIDL definitions and headers (`fetch.sh`), generates the binder code
with the Android SDK's `aidl`, and builds with the NDK, as the hwcomposer does. The tests of
the parts that run without binder build on the build machine:

```
dsa/guest/audio/test.sh
```
