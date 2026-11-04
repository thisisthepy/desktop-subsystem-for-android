# guest/power

`dsa-power` and `dsa-health` are the guest's power HAL (`android.hardware.power`, version 5)
and health HAL (`android.hardware.health`, version 3). They replace Cuttlefish's example power
HAL and its made-up battery. Both reach the host through `dsa-channeld`'s binder service, as
the hwcomposer does.

## Power

Android's power manager sets power modes (an app launching, a game, low power, the display
off) and asks for short boosts when the user touches the screen. `dsa-power` sends each mode
change and each boost to the host as a `PowerHint`. The host keeps them and runs the guest's
virtual processors to match: at full speed while a performance mode or a boost is on,
frugally while the display is off, the guest is in Doze or low power is on, and otherwise as
any program. On Windows that is the power throttling and priority of the VM's `vmmem`
process, whose threads are the guest's processors.

Only the modes and boosts the host acts on are reported as supported, so the framework sends
no others. Hint sessions are not offered: the host steers the guest's processors as a whole,
not single guest threads. The modes that are on are sent again whenever the channel comes
back, and `dsa-channeld` tells a host that connects again.

## Health

The guest's battery is the host computer's. The host sends its power (`PowerState`) when the
guest connects and on each change; `dsa-channeld` keeps the latest and hands it to a listener
that subscribes later, so a restarted `dsa-health` knows it at once. A host without a battery,
and the time before the host has said, read as a full battery on AC power. Each change reaches
the battery service through its callback.

## Building and testing

- `build.sh [scratch] [out]` fetches the pinned AIDL definitions, binder headers and protobuf
  runtime (`fetch.sh`) and builds both programs with the NDK.
- `test.sh` builds and runs the rules on the build machine: the messages against the protocol
  vectors, the battery for each host power, and the modes and boosts taken.

`make-image.py --power-hal <out>` installs them, with their init scripts, VINTF fragments and
labels (`hal_power_default`, `hal_health_default`), and removes Cuttlefish's power and health
APEXes. `sepolicy/power.cil` lets both reach the daemon.

## Checking on a running guest

- `logcat -s dsa-power dsa-health` shows `serving android.hardware.power.IPower/default` and
  `serving android.hardware.health.IHealth/default`, then `subscribed to the control channel`.
- `dumpsys battery` shows the host's charge and whether it runs on AC power.
- The service's log on the host shows `power: running Android's processors at full speed`
  the first time an app launches.
