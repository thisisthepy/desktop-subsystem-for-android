//! `dsa-hostd` on macOS: the per-user agent that runs the user's Android.
//!
//! ```text
//! dsa-hostd serve [config.json]          serve the local API until stopped (launchd runs this)
//! dsa-hostd install-agent [config.json]  install and start the launchd agent for this user
//! dsa-hostd uninstall-agent              stop and remove it
//! dsa-hostd check [config.json]          check the image and the VM with the framework,
//!                                        without starting it
//! dsa-hostd version
//! ```
//!
//! Without a configuration file named, the agent reads the user's, then the machine's
//! (`dsa-hostd.json` in the DSA folder of each). The binary must be signed with the
//! `com.apple.security.virtualization` entitlement (`dsa-hostd.entitlements`).

#[cfg(target_os = "macos")]
fn main() {
    std::process::exit(match macos::run(std::env::args().skip(1).collect()) {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("dsa-hostd: {why}");
            1
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("dsa-hostd: this build is the macOS host and runs on macOS only");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use dsa_host_macos::host_config::MacHostConfig;

    fn home() -> Result<PathBuf, String> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is not set".to_string())
    }

    fn config_path(arg: Option<&String>, home: &Path) -> Result<PathBuf, String> {
        match arg {
            Some(path) => Ok(PathBuf::from(path)),
            None => MacHostConfig::find(home).ok_or_else(|| {
                format!(
                    "no configuration: neither {} nor {} exists",
                    dsa_host_macos::host_config::user_root_in(home)
                        .join(dsa_host_macos::host_config::FILE_NAME)
                        .display(),
                    Path::new(dsa_host_macos::host_config::MACHINE_ROOT)
                        .join(dsa_host_macos::host_config::FILE_NAME)
                        .display()
                )
            }),
        }
    }

    /// Sets `stop` on SIGTERM and SIGINT, which launchd and a terminal send.
    fn stop_on_signals(stop: Arc<AtomicBool>) {
        static STOP: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
        let _ = STOP.set(stop);
        extern "C" fn on_signal(_: libc::c_int) {
            if let Some(stop) = STOP.get() {
                stop.store(true, Ordering::Relaxed);
            }
        }
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(
                libc::SIGTERM,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
            libc::signal(
                libc::SIGINT,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }

    pub fn run(args: Vec<String>) -> Result<(), String> {
        let build = format!("dsa-hostd {}", dsa_host_macos::VERSION);
        let home = home()?;
        match args.first().map(String::as_str) {
            Some("serve") => {
                let path = config_path(args.get(1), &home)?;
                let config = MacHostConfig::read(&path)?;
                let stop = Arc::new(AtomicBool::new(false));
                stop_on_signals(Arc::clone(&stop));
                dsa_host_macos::service::serve(config, &home, &build, stop)
            }
            Some("install-agent") => {
                let program =
                    std::env::current_exe().map_err(|e| format!("finding this program: {e}"))?;
                let config = args.get(1).map(PathBuf::from);
                let uid = dsa_host_unix::socket::current_uid();
                let plist =
                    dsa_host_macos::launchd::install(&home, uid, &program, config.as_deref())?;
                println!("installed and started {}", plist.display());
                Ok(())
            }
            Some("uninstall-agent") => {
                let uid = dsa_host_unix::socket::current_uid();
                dsa_host_macos::launchd::uninstall(&home, uid)?;
                println!("removed the agent");
                Ok(())
            }
            Some("check") => {
                let path = config_path(args.get(1), &home)?;
                let config = MacHostConfig::read(&path)?;
                check(&config, &home)
            }
            Some("version") => {
                println!("{build}");
                Ok(())
            }
            _ => Err(
                "usage: dsa-hostd serve|install-agent [config.json] | uninstall-agent | \
                 check [config.json] | version"
                    .into(),
            ),
        }
    }

    /// Reads the image and has the framework validate a VM made from it.
    fn check(config: &MacHostConfig, home: &Path) -> Result<(), String> {
        use dsa_host_macos::vm_config::{image_files, Disk, GuestArch, MacVmConfig};
        let dir = config
            .image_dir
            .clone()
            .or_else(|| {
                let (root, first) = (config.images_dir.as_ref()?, config.first_image.as_ref()?);
                Some(root.join("images").join(&first.version))
            })
            .ok_or("the configuration names no image")?;
        let files = image_files(&dir, GuestArch::HOST)?;
        println!("image: {}", dir.display());
        println!("kernel: {}", files.kernel.display());
        println!(
            "virtualization supported on this Mac: {}",
            dsa_host_macos::vz::supported()
        );
        let user = config.user(home);
        let mut disks: Vec<Disk> = files
            .system_disk
            .iter()
            .map(|path| Disk {
                path: path.clone(),
                read_only: true,
            })
            .collect();
        disks.push(Disk {
            path: files.disk_template.clone(),
            read_only: true,
        });
        let vm = MacVmConfig {
            kernel: files.kernel.clone(),
            initrd: files.initrd.clone(),
            cmdline: dsa_host_macos::cmdline::for_virtualization_framework(
                &files.kernel_cmdline,
                &config.kernel_cmdline,
                false,
            ),
            memory_max_mb: 4096,
            memory_start_mb: 2048,
            processors: config.processors,
            disks,
            console_log: Some(user.logs().join("check-console.log")),
            network: config.network,
            mac_address: None,
            listen_ports: vec![config.control_port],
        };
        println!("kernel command line: {}", vm.cmdline);
        match dsa_host_macos::vz::validate(&vm) {
            Ok(()) => {
                println!("the Virtualization framework accepts the VM");
                Ok(())
            }
            // A Mac that is itself a VM without nested virtualization refuses every VM; the
            // image and the configuration were still read.
            Err(why) if !dsa_host_macos::vz::supported() => {
                println!("this Mac runs no VMs, so the framework only says: {why}");
                Ok(())
            }
            Err(why) => Err(why),
        }
    }
}
