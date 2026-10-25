# dsa

All of the project's code lives under this folder. Each folder below is created by the
first change that puts real code in it.

The Rust crates form one workspace, `dsa/Cargo.toml`. Build and test them from this folder:

```
cargo test --workspace
```

| Folder | Language | What it holds | Status |
|---|---|---|---|
| `protocol/` | proto3 + Rust generator | The guest-host control messages, defined once in `control.proto`, with codecs generated for Rust, Java and C++, checked against shared vectors | in progress: the handshake |
| `protocol/transport/` | Rust | The control channel's transport: framing, the connection traits, the in-memory fake and the conformance suite every transport passes | in progress |
| `host/core/` | Rust | The platform-neutral host: sessions, the app registry, the lifecycle policy | in progress: the session start and stop, the guest's tasks, and which window each task and each frame goes to |
| `host/platform/windows/` | Rust, C++ | The Android VM, hvsocket, the Win32 and DirectComposition compositor, TSF, Start menu and notifications | in progress: the Android VM launcher (HCS), hvsocket, the compositor (one window per Android task, with its frames through Direct3D 11 and DirectComposition), and a WSL container launcher kept for a later second backend |
| `host/platform/linux/`, `host/platform/macos/` | | Other hosts | planned, not started |
| `graphics/` | C++ | Vulkan remoting on the host, starting from gfxstream | in progress: gfxstream over Hyper-V sockets, and the host renderer copying each task's frames into its window |
| `guest/` | Kotlin, C++, Rust | The agent (a system app), the hwcomposer, HAL clients, the image configuration | planned |
| `guest/transport/` | Rust | The guest side of the control channel, over vsock, for the guest's channel daemon | in progress |
| `guest/channeld/` | Rust | The guest's control channel daemon: reads the boot's secret, connects to the host, passes the handshake, carries frames, reconnects | in progress: runs in the image and holds the connection; serving the agent and hwcomposer over binder is not done yet |
| `kernel/` | config, shell | The guest kernel configuration, with binder built in, and its build | planned |
