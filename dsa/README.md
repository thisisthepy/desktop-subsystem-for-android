# dsa

All of the project's code lives under this folder. Each folder below is created by the
first change that puts real code in it.

The Rust crates form one workspace, `dsa/Cargo.toml`. Build and test them from this folder:

```
cargo test --workspace
```

| Folder | Language | What it holds | Status |
|---|---|---|---|
| `protocol/` | definition + generator | The guest-host control messages, defined once, with codecs generated for Rust, Kotlin and C++ | planned; the definition format is not chosen yet |
| `protocol/transport/` | Rust | The control channel's transport: framing, the connection traits, the in-memory fake and the conformance suite every transport passes | in progress |
| `host/core/` | Rust | The platform-neutral host: sessions, the app registry, the lifecycle policy | in progress: the session start and stop |
| `host/platform/windows/` | Rust, C++ | The Android VM, hvsocket, the Win32 and DirectComposition compositor, TSF, Start menu and notifications | in progress: the Android VM launcher (HCS), hvsocket, and a WSL container launcher kept for a later second backend |
| `host/platform/linux/`, `host/platform/macos/` | | Other hosts | planned, not started |
| `graphics/` | C++ | Vulkan remoting on the host, starting from gfxstream | planned |
| `guest/` | Kotlin, C++, Rust | The agent (a system app), the hwcomposer, HAL clients, the image configuration | planned |
| `guest/transport/` | Rust | The guest side of the control channel, over vsock | in progress |
| `kernel/` | config, shell | The guest kernel configuration, with binder built in, and its build | planned |
