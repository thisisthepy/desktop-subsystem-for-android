//! The VM's network: NAT by default, the host's LAN with advanced networking.
//!
//! NAT is user-mode networking through passt, so it needs no privileges and changes nothing
//! on the host. passt (BSD-3-Clause and GPL-2.0-or-later) runs as its own program, as the
//! user, and serves the VM's adapter as a vhost-user device on a Unix socket. It gives the
//! guest the host's own address and routes by DHCP, forwards DNS to the host's resolvers,
//! carries IPv6 when the host has it, and follows the host's routes, so a VPN on the host
//! carries the guest's traffic too. crosvm has no user-mode network of its own on Linux, and
//! the passt of older distributions has no vhost-user, so DSA ships a passt of its own beside
//! crosvm and uses it first.
//!
//! Advanced networking puts the VM on the host's LAN as a device of its own, through a macvtap
//! link on the interface of the host's default route. That needs the service's privileges to
//! make the link; the VM gets the open tap as a file descriptor. The host itself cannot reach
//! the guest through a macvtap in bridge mode (a property of macvtap); every other machine on
//! the LAN can.
//!
//! The guest always sees one virtio adapter, eth0, which its image turns into Wi-Fi, and takes
//! its address by DHCP, as on Windows.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use dsa_host_core::host_api::NetworkStatus;

/// The host's default route: its interface and gateway, from `/proc/net/route`.
pub fn default_route(route_table: &str) -> Option<(String, Ipv4Addr)> {
    route_table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 || fields[1] != "00000000" {
            return None;
        }
        let gateway = u32::from_str_radix(fields[2], 16).ok()?;
        // The table is in the host's byte order, which is little endian on every host DSA runs on.
        Some((fields[0].to_string(), Ipv4Addr::from(gateway.swap_bytes())))
    })
}

/// The DNS servers `resolv.conf` names.
pub fn nameservers(resolv_conf: &str) -> Vec<String> {
    resolv_conf
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            match words.next() {
                Some("nameserver") => words.next().map(str::to_string),
                _ => None,
            }
        })
        .collect()
}

/// The host's IPv4 addresses on `interface`, from `ip -o -4 addr show` output.
pub fn interface_addresses(ip_output: &str) -> Vec<String> {
    ip_output
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            words.find(|w| *w == "inet" || *w == "inet6")?;
            let address = words.next()?;
            Some(address.split('/').next().unwrap_or(address).to_string())
        })
        .collect()
}

/// A macvtap link's name for user `uid`: at most 15 characters, as the kernel allows.
pub fn macvtap_name(uid: u32) -> String {
    format!("dsatap{uid:x}")
}

/// Where DSA's package puts the VM programs (crosvm and passt) and its own host programs.
pub const PACKAGE_PROGRAMS: &str = "/usr/lib/dsa/bin";

/// Finds `program` beside this program, in DSA's package folder, on the `PATH`, or in the
/// system's program folders.
pub fn find_program(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    program_folders(beside, std::env::split_paths(&path).collect())
        .into_iter()
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// The folders `find_program` looks in, in order. DSA's package folder comes before the `PATH`
/// and the system's folders: the passt of a distribution's own packages has no vhost-user on
/// older releases, and a service that does not run from the package folder would otherwise
/// start that passt instead of DSA's own.
pub fn program_folders(beside: Option<PathBuf>, path: Vec<PathBuf>) -> Vec<PathBuf> {
    beside
        .into_iter()
        .chain(std::iter::once(PathBuf::from(PACKAGE_PROGRAMS)))
        .chain(path)
        .chain(
            [
                "/usr/bin",
                "/usr/sbin",
                "/usr/local/bin",
                "/usr/libexec",
                "/bin",
                "/sbin",
            ]
            .into_iter()
            .map(PathBuf::from),
        )
        .collect()
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// The network status of a VM in `mode` (`NetworkStatus::NAT` or `ADVANCED`), as the host
/// sees it now.
pub fn host_status(mode: &str, problem: Option<String>) -> NetworkStatus {
    let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
    let resolv = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let (interface, gateway) = default_route(&route)
        .map(|(interface, gateway)| (Some(interface), gateway.to_string()))
        .unwrap_or((None, String::new()));
    let addresses = if mode == NetworkStatus::NAT {
        // passt gives the guest the host's own address on the default route.
        interface
            .and_then(|interface| {
                std::process::Command::new("ip")
                    .args(["-o", "addr", "show", "dev", &interface])
                    .output()
                    .ok()
            })
            .map(|out| interface_addresses(&String::from_utf8_lossy(&out.stdout)))
            .unwrap_or_default()
    } else {
        // On the LAN the guest's address comes from the LAN's DHCP server, which the host
        // does not see.
        Vec::new()
    };
    NetworkStatus {
        mode: mode.to_string(),
        addresses,
        dns_servers: nameservers(&resolv),
        gateway,
        problem,
        // The service fills it from the session, which knows whether the guest reaches the
        // DNS port.
        dns_resolver: String::new(),
    }
}

#[cfg(target_os = "linux")]
pub use running::{NetworkChoice, VmNetwork};

#[cfg(target_os = "linux")]
mod running {
    use super::*;
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use crate::crosvm::NetworkBackend;
    use crate::users::Account;

    /// The file descriptor crosvm finds the macvtap's tap at.
    pub const TAP_FD: i32 = 3;

    /// What the VM's network is for one boot.
    pub struct NetworkChoice {
        pub backend: NetworkBackend,
        /// The adapter's MAC address, when the network decides it (a macvtap has its own).
        pub mac_address: Option<String>,
        /// The tap to hand crosvm as [`TAP_FD`].
        pub tap: Option<File>,
        pub status: NetworkStatus,
    }

    /// The network of one running VM, removed when it stops.
    #[derive(Default)]
    pub struct VmNetwork {
        passt: Option<Child>,
        macvtap: Option<String>,
    }

    impl VmNetwork {
        /// Makes the network of the next boot of `account`'s VM, in `advanced` mode or NAT,
        /// with its sockets in `dir`.
        pub fn attach(
            &mut self,
            advanced: bool,
            account: &Account,
            dir: &Path,
        ) -> Result<NetworkChoice, String> {
            self.detach();
            let mut problem = None;
            if advanced {
                match self.macvtap(account.uid) {
                    Ok(choice) => return Ok(choice),
                    Err(why) => {
                        problem = Some(format!(
                            "advanced networking is on but could not be set up ({why}); \
                             Android uses NAT"
                        ))
                    }
                }
            }
            let backend = match find_program("passt") {
                Some(passt) => {
                    let socket = dir.join("passt.sock");
                    let _ = std::fs::remove_file(&socket);
                    let mut command = Command::new(passt);
                    command
                        .args(passt_args())
                        .arg(&socket)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::inherit());
                    account.run_as(&mut command, None);
                    let child = command
                        .spawn()
                        .map_err(|e| format!("starting passt for the VM's network: {e}"))?;
                    self.passt = Some(child);
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !socket.exists() {
                        if Instant::now() >= deadline {
                            self.detach();
                            return Err("passt did not open its socket within 5 s".into());
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    NetworkBackend::Passt { socket }
                }
                None => {
                    problem = Some(
                        "passt, which gives Android its network, is missing: reinstall DSA \
                         (the desktop-subsystem-for-android package)"
                            .to_string(),
                    );
                    NetworkBackend::None
                }
            };
            Ok(NetworkChoice {
                backend,
                mac_address: None,
                tap: None,
                status: host_status(NetworkStatus::NAT, problem),
            })
        }

        fn macvtap(&mut self, uid: u32) -> Result<NetworkChoice, String> {
            let route = std::fs::read_to_string("/proc/net/route")
                .map_err(|e| format!("reading the host's routes: {e}"))?;
            let (interface, _) = default_route(&route)
                .ok_or("the host has no default route to put Android's adapter on")?;
            let name = macvtap_name(uid);
            let ip = |args: &[&str]| -> Result<(), String> {
                let out = Command::new("ip")
                    .args(args)
                    .output()
                    .map_err(|e| format!("running ip: {e}"))?;
                if out.status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "ip {}: {}",
                        args.join(" "),
                        String::from_utf8_lossy(&out.stderr).trim()
                    ))
                }
            };
            // One a stopped service left behind goes first.
            let _ = ip(&["link", "delete", &name]);
            ip(&[
                "link", "add", "link", &interface, "name", &name, "type", "macvtap", "mode",
                "bridge",
            ])?;
            self.macvtap = Some(name.clone());
            ip(&["link", "set", &name, "up"])?;
            let sys = PathBuf::from("/sys/class/net").join(&name);
            let index = std::fs::read_to_string(sys.join("ifindex"))
                .map_err(|e| format!("reading {name}'s index: {e}"))?;
            let mac = std::fs::read_to_string(sys.join("address"))
                .map_err(|e| format!("reading {name}'s address: {e}"))?;
            let tap_path = format!("/dev/tap{}", index.trim());
            let tap = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&tap_path)
                .map_err(|e| format!("opening {tap_path}: {e}"))?;
            Ok(NetworkChoice {
                backend: NetworkBackend::Tap { fd: TAP_FD },
                mac_address: Some(mac.trim().to_string()),
                tap: Some(tap),
                status: host_status(NetworkStatus::ADVANCED, None),
            })
        }

        /// Removes the network: passt ends, and the macvtap link goes.
        pub fn detach(&mut self) {
            if let Some(mut passt) = self.passt.take() {
                let _ = passt.kill();
                let _ = passt.wait();
            }
            if let Some(name) = self.macvtap.take() {
                let _ = Command::new("ip").args(["link", "delete", &name]).output();
            }
        }
    }

    impl Drop for VmNetwork {
        fn drop(&mut self) {
            self.detach();
        }
    }

    /// Puts `tap` at [`TAP_FD`] in the child, for its pre-exec step: `dup2` leaves the copy
    /// open across `exec`.
    pub fn place_tap(tap: &File) -> impl Fn() -> std::io::Result<()> + Send + Sync + 'static {
        let fd = tap.as_raw_fd();
        move || {
            // SAFETY: dup2 is async-signal-safe; both descriptors are valid in the child.
            if unsafe { libc::dup2(fd, TAP_FD) } < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
pub use running::{place_tap, TAP_FD};

/// passt's arguments, before the socket path: the adapter as a vhost-user device, for one VM
/// that connects once.
pub fn passt_args() -> [&'static str; 5] {
    [
        "--vhost-user",
        "--foreground",
        "--quiet",
        "--one-off",
        "--socket",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr28_the_default_route_names_the_lan_interface_and_gateway() {
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
                     wlp2s0\t0000A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\n\
                     wlp2s0\t00000000\t0100A8C0\t0003\t0\t0\t600\t00000000\n";
        assert_eq!(
            default_route(table),
            Some(("wlp2s0".to_string(), Ipv4Addr::new(192, 168, 0, 1)))
        );
        assert_eq!(default_route("Iface\tDestination\n"), None);
    }

    #[test]
    fn fr28_the_guest_is_told_the_hosts_dns_servers() {
        let resolv = "# generated\nnameserver 127.0.0.53\noptions edns0\nnameserver ::1\n";
        assert_eq!(nameservers(resolv), ["127.0.0.53", "::1"]);
    }

    #[test]
    fn fr28_the_hosts_addresses_are_read_from_ip() {
        let out = "2: wlp2s0    inet 192.168.0.23/24 brd 192.168.0.255 scope global wlp2s0\n\
                   2: wlp2s0    inet6 fe80::1/64 scope link\n";
        assert_eq!(interface_addresses(out), ["192.168.0.23", "fe80::1"]);
    }

    #[test]
    fn fr28_passt_serves_the_adapter_as_a_vhost_user_device() {
        let args = passt_args();
        assert_eq!(args[0], "--vhost-user");
        assert_eq!(args.last(), Some(&"--socket"));
    }

    #[test]
    fn fr28_dsas_own_passt_comes_before_a_distributions() {
        let folders = program_folders(
            Some(PathBuf::from("/work/target/debug")),
            vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")],
        );
        assert_eq!(folders[0], PathBuf::from("/work/target/debug"));
        assert_eq!(folders[1], PathBuf::from(PACKAGE_PROGRAMS));
        let package = folders
            .iter()
            .position(|d| d == Path::new(PACKAGE_PROGRAMS));
        let system = folders.iter().position(|d| d == Path::new("/usr/bin"));
        assert!(package < system, "{folders:?}");
        assert_eq!(
            program_folders(None, Vec::new())[0],
            PathBuf::from(PACKAGE_PROGRAMS)
        );
    }

    #[test]
    fn fr28_each_users_lan_link_has_a_name_the_kernel_takes() {
        assert_eq!(macvtap_name(1000), "dsatap3e8");
        assert!(macvtap_name(u32::MAX).len() <= 15);
    }
}
