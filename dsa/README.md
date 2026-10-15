# dsa

All of the project's code lives under this folder. Nothing is implemented yet; each folder
below is created by the first change that puts real code in it.

| Folder | Language | What it holds | Status |
|---|---|---|---|
| `protocol/` | definition + generator | The guest-host control messages, defined once, with codecs generated for Rust, Kotlin and C++ | planned |
| `host/core/` | Rust | The platform-neutral host: sessions, the app registry, the lifecycle policy | planned |
| `host/platform/windows/` | Rust, C++ | WSL launch, hvsocket, the Win32 and DirectComposition compositor, TSF, Start menu and notifications | planned |
| `host/platform/linux/`, `host/platform/macos/` | | Other hosts | planned, not started |
| `graphics/` | C++ | Vulkan remoting on the host, starting from gfxstream | planned |
| `guest/` | Kotlin, C++ | The agent (a system app), the hwcomposer, HAL clients, the image configuration | planned |
| `kernel/` | config, shell | The WSL kernel module configuration and its build | planned |
