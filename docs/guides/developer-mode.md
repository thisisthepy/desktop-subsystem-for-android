# Developer mode and adb

Developer mode lets the Android Debug Bridge (adb) on your PC connect to your Android in
Desktop Subsystem for Android™ (DSA). It is off by default.

adb is not part of DSA. Get it from the Android SDK Platform-Tools.

## Connect adb

1. Open the **Advanced** page of DSA Settings and turn on **Developer mode**. Or run
   `dsa settings set developer-mode on`.
2. Start Android, if it is not running: `dsa start --wait`.
3. Connect:

   ```
   adb connect 127.0.0.1:58526
   adb devices
   ```

   Android does not ask you to allow the connection. The port answers only on this PC, so
   other computers cannot connect.
4. When you are done, turn **Developer mode** off again, or run
   `dsa settings set developer-mode off`.

**Connect adb** on the **Advanced** page shows the exact command, and `dsa settings` prints
it on the `Developer mode` line.

## Use another port

The default port is 58526. To change it, edit **Port** under **Connect adb**, or run:

```
dsa settings set adb-port 5555
```

## What developer mode does not change

DSA never uses adb for its own work. Windows, input and installs use DSA's own control
channel, so everything works the same with developer mode off.

To read Android's log you do not need adb. See [Troubleshooting](troubleshooting.md).

---

Android is a trademark of Google LLC. DSA is not affiliated with, endorsed by or sponsored
by Google.
