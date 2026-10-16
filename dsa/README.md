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
| `host/core/` | Rust | The platform-neutral host: sessions, the app registry, the lifecycle policy | planned |
| `host/platform/windows/` | Rust, C++ | WSL launch, hvsocket, the Win32 and DirectComposition compositor, TSF, Start menu and notifications | planned |
| `host/platform/linux/`, `host/platform/macos/` | | Other hosts | planned, not started |
| `graphics/` | C++ | Vulkan remoting on the host, starting from gfxstream | planned |
| `guest/` | Kotlin, C++ | The agent (a system app), the hwcomposer, HAL clients, the image configuration | planned |
| `kernel/` | config, shell | The WSL kernel module configuration and its build | planned |
