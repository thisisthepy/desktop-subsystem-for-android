//! How adb on this computer finds a user's developer mode port, the Linux part: the user's own
//! adb server, and the port's mDNS advertisement through Avahi on the system bus.
//!
//! The service runs as root for every user, so its own environment says nothing of a user's
//! adb. The user's adb server is the process of theirs that adb started with `fork-server`;
//! its command line says where it listens (`-L tcp:5037`), and its environment carries the
//! `ANDROID_ADB_SERVER_PORT` it was started with. Only the user's own server is attached, so
//! one user's Android never shows in another user's adb.

use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::adb_discovery::{
    adb_server_port, mdns_instance_name, AdbAdvertise, AdbServerLink, ADB_SERVER_PORT_VARIABLE,
    MDNS_SERVICE_TYPE,
};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

use crate::devices::dbus::proxy;

/// How long a found adb server port is used before `/proc` is read again.
const LOOK_AGAIN: Duration = Duration::from_secs(10);

const AVAHI: &str = "org.freedesktop.Avahi";
const AVAHI_SERVER: &str = "org.freedesktop.Avahi.Server";
const AVAHI_ENTRY_GROUP: &str = "org.freedesktop.Avahi.EntryGroup";
/// `AVAHI_IF_UNSPEC`: every interface Avahi publishes on.
const AVAHI_IF_UNSPEC: i32 = -1;
/// `AVAHI_PROTO_INET`: the record is an IPv4 one.
const AVAHI_PROTO_INET: i32 = 0;
/// `AVAHI_PUBLISH_NO_REVERSE`: no PTR record for 127.0.0.1, which is localhost's own.
const AVAHI_PUBLISH_NO_REVERSE: u32 = 16;

/// The port an adb server's command line says it listens on: `-L tcp:5037`,
/// `-L tcp:localhost:5037` or `-P 5037`. `None` when the command line is not an adb server's.
pub fn server_port_from_args(args: &[String]) -> Option<u16> {
    let program = args.first()?;
    let name = program.rsplit('/').next().unwrap_or(program);
    if !name.starts_with("adb") || !args.iter().any(|arg| arg == "fork-server") {
        return None;
    }
    let mut found = None;
    for pair in args.windows(2) {
        match pair[0].as_str() {
            "-L" => {
                found = pair[1]
                    .strip_prefix("tcp:")
                    .and_then(|spec| spec.rsplit(':').next())
                    .and_then(|port| port.parse::<u16>().ok())
                    .or(found)
            }
            "-P" => found = pair[1].parse::<u16>().ok().or(found),
            _ => {}
        }
    }
    found
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

/// The port of the adb server `uid` runs, or `None` when they run none.
pub fn user_server_port(uid: u32) -> Option<u16> {
    let processes = std::fs::read_dir("/proc").ok()?;
    for process in processes.flatten() {
        let path = process.path();
        let is_pid = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()));
        if !is_pid || std::fs::metadata(&path).map(|m| m.uid()).ok() != Some(uid) {
            continue;
        }
        let Ok(cmdline) = std::fs::read(path.join("cmdline")) else {
            continue;
        };
        let args = split_nul(&cmdline);
        if let Some(port) = server_port_from_args(&args) {
            return Some(port);
        }
        if args
            .first()
            .and_then(|program| program.rsplit('/').next())
            .is_some_and(|name| name.starts_with("adb"))
            && args.iter().any(|arg| arg == "fork-server")
        {
            // A server started without a port on its command line: it took the variable.
            let environment = std::fs::read(path.join("environ")).unwrap_or_default();
            let variable = split_nul(&environment).into_iter().find_map(|entry| {
                entry
                    .strip_prefix(ADB_SERVER_PORT_VARIABLE)
                    .and_then(|rest| rest.strip_prefix('='))
                    .map(str::to_string)
            });
            return Some(adb_server_port(variable.as_deref()));
        }
    }
    None
}

/// A link to the adb server of the user `uid`. While they run none it is refused, so
/// auto-connect waits for one.
pub fn adb_server_of(uid: u32) -> AdbServerLink {
    let found: Arc<Mutex<Option<(Instant, Option<u16>)>>> = Arc::default();
    let inner = dsa_host_core::adb_discovery::loopback_link({
        let found = Arc::clone(&found);
        move || {
            let mut found = found.lock().unwrap_or_else(|e| e.into_inner());
            let current: Option<(Instant, Option<u16>)> = *found;
            let port = match current {
                Some((at, Some(port))) if at.elapsed() < LOOK_AGAIN => Some(port),
                _ => {
                    let port = user_server_port(uid);
                    *found = Some((Instant::now(), port));
                    port
                }
            };
            // Port 0 is never listened on, so no server means a refused connection.
            port.unwrap_or(0)
        }
    });
    Arc::new(move || {
        let stream = inner();
        if stream.is_err() {
            // The server went away or moved: look again next time.
            *found.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        stream
    })
}

/// An entry group Avahi publishes until it is freed.
struct Published {
    group: Proxy<'static>,
}

impl Drop for Published {
    fn drop(&mut self) {
        let _ = self.group.call::<_, _, ()>("Free", &());
    }
}

fn publish(port: u16) -> Result<Published, String> {
    let connection = Connection::system().map_err(|e| format!("the system bus: {e}"))?;
    let server = proxy(&connection, AVAHI, "/".into(), AVAHI_SERVER)?;
    let path: OwnedObjectPath = server
        .call("EntryGroupNew", &())
        .map_err(|e| format!("Avahi gives no entry group: {e}"))?;
    let group: Proxy<'static> = proxy(
        &connection,
        AVAHI,
        path.as_str().to_string(),
        AVAHI_ENTRY_GROUP,
    )?;
    let host = format!("dsa-adb-{port}.local");
    group
        .call::<_, _, ()>(
            "AddAddress",
            &(
                AVAHI_IF_UNSPEC,
                AVAHI_PROTO_INET,
                AVAHI_PUBLISH_NO_REVERSE,
                host.as_str(),
                "127.0.0.1",
            ),
        )
        .map_err(|e| format!("Avahi refuses the loopback address: {e}"))?;
    let txt: Vec<Vec<u8>> = Vec::new();
    group
        .call::<_, _, ()>(
            "AddService",
            &(
                AVAHI_IF_UNSPEC,
                AVAHI_PROTO_INET,
                0u32,
                mdns_instance_name(port),
                MDNS_SERVICE_TYPE,
                "",
                host.as_str(),
                port,
                txt,
            ),
        )
        .map_err(|e| format!("Avahi refuses the service: {e}"))?;
    group
        .call::<_, _, ()>("Commit", &())
        .map_err(|e| format!("Avahi does not publish: {e}"))?;
    Ok(Published { group })
}

/// Advertises developer mode's port as an `_adb._tcp` service whose only address is
/// `127.0.0.1`, through Avahi when it runs. Without it, nothing is advertised and the log
/// says so; auto-connect and `adb connect` still work.
pub fn advertise(port: u16) -> Option<Box<dyn Send>> {
    match publish(port) {
        Ok(published) => Some(Box::new(published)),
        Err(why) => {
            eprintln!("developer mode: port {port} is not advertised over mDNS: {why}");
            None
        }
    }
}

/// The advertiser developer mode uses on Linux.
pub fn advertiser() -> AdbAdvertise {
    Arc::new(advertise)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn fr20_the_adb_server_port_is_read_from_its_command_line() {
        assert_eq!(
            server_port_from_args(&args("adb -L tcp:5037 fork-server server --reply-fd 4")),
            Some(5037)
        );
        assert_eq!(
            server_port_from_args(&args(
                "/opt/android/platform-tools/adb -L tcp:localhost:5038 fork-server server"
            )),
            Some(5038)
        );
        assert_eq!(
            server_port_from_args(&args("adb -P 6000 fork-server server")),
            Some(6000)
        );
        assert_eq!(server_port_from_args(&args("adb fork-server server")), None);
        assert_eq!(server_port_from_args(&args("adb devices")), None);
        assert_eq!(
            server_port_from_args(&args("bash -L tcp:5037 fork-server")),
            None
        );
    }
}
