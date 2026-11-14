//! The DNS port a session serves its guest ([`crate::dns`]): the guest's DNS queries,
//! answered by the host's resolver.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_protocol::control;
use dsa_transport::{Listener, TransportError};

use super::{Session, SessionState, Shared};
use crate::dns::{self, DnsService, HostResolver};

impl Session {
    /// Serves the DNS port on `listener` for the life of the session: each connection that
    /// proves it knows the running boot's secret may send DNS queries, which `resolver`
    /// answers. `build` names this host in the handshake. The guest's control daemon may hold
    /// more than one connection over time (it connects again after a host restart); each is
    /// served on a thread of its own until it closes.
    pub fn serve_dns_port(
        &self,
        listener: Box<dyn Listener>,
        resolver: Arc<dyn HostResolver>,
        build: &str,
    ) {
        let shared = Arc::clone(&self.shared);
        let hello = dsa_protocol::handshake::local_hello(control::Role::Host, build);
        let service = Arc::new(DnsService::new(resolver));
        let spawned = thread::Builder::new()
            .name("dsa-dns".into())
            .spawn(move || accept_dns(listener, shared, service, hello));
        match spawned {
            Ok(_) => self.shared.dns_port_served.store(true, Ordering::Relaxed),
            Err(error) => eprintln!(
                "dns: the DNS port cannot be served, so the guest asks its network's DNS servers: {error}"
            ),
        }
    }

    /// Whether this host's resolver answers the guest's DNS queries now: `Some(true)` while the
    /// guest's control daemon holds a connection on the DNS port, `Some(false)` while Android
    /// runs without one (it then asks its network's DNS servers itself), and `None` while
    /// Android is not running or still booting, or when this host does not serve the DNS port.
    pub fn dns_answered_by_host(&self) -> Option<bool> {
        if !self.shared.dns_port_served.load(Ordering::Relaxed) {
            return None;
        }
        match self.state() {
            SessionState::Active
            | SessionState::Dozing
            | SessionState::Frozen
            | SessionState::Stalled => {
                Some(self.shared.dns_connections.load(Ordering::Relaxed) > 0)
            }
            SessionState::Stopped | SessionState::Starting | SessionState::Stopping => None,
        }
    }
}

/// Counts one open DNS connection for as long as it lives, a failed one included.
struct OpenConnection<'a>(&'a Shared);

impl<'a> OpenConnection<'a> {
    fn count(shared: &'a Shared) -> OpenConnection<'a> {
        shared.dns_connections.fetch_add(1, Ordering::Relaxed);
        OpenConnection(shared)
    }
}

impl Drop for OpenConnection<'_> {
    fn drop(&mut self) {
        self.0.dns_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

fn accept_dns(
    mut listener: Box<dyn Listener>,
    shared: Arc<Shared>,
    service: Arc<DnsService>,
    hello: control::Hello,
) {
    let mut reported = false;
    while !shared.log_port_stop.load(Ordering::Relaxed) {
        let mut connection = match listener.accept(Duration::from_millis(200)) {
            Ok(connection) => connection,
            Err(TransportError::Timeout) => continue,
            Err(error) => {
                if !reported {
                    eprintln!("dns: accepting on the DNS port failed: {error}");
                    reported = true;
                }
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        reported = false;
        let secret = shared
            .boot_secret
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(secret) = secret else {
            connection.close();
            continue;
        };
        let (shared, service, hello) = (Arc::clone(&shared), Arc::clone(&service), hello.clone());
        let spawned = thread::Builder::new()
            .name("dsa-dns-connection".into())
            .spawn(move || {
                let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let Err(error) = dsa_protocol::handshake::host_accept(
                        connection.as_mut(),
                        &hello,
                        &secret,
                        dns::HANDSHAKE_WAIT,
                    ) {
                        eprintln!("dns: refused a connection on the DNS port: {error}");
                        return;
                    }
                    eprintln!("dns: the guest's DNS queries are answered by this host's resolver");
                    let _open = OpenConnection::count(&shared);
                    let ended =
                        dns::serve_connection(connection.as_mut(), &service, &shared.log_port_stop);
                    eprintln!("dns: the guest's DNS connection ended ({ended})");
                }));
                if served.is_err() {
                    eprintln!("dns: a DNS connection failed; the guest connects again");
                }
                connection.close();
            });
        if let Err(error) = spawned {
            eprintln!(
                "dns: a connection on the DNS port is dropped, no thread could take it: {error}"
            );
        }
    }
}
