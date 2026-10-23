//! The control channel over Hyper-V sockets (`AF_HYPERV`).
//!
//! The WSL VM is a Hyper-V partition. Inside it the guest uses vsock; on the host the same
//! connection is a Hyper-V socket addressed by the VM's ID and a service ID, where vsock
//! port `p` maps to the service ID [`Guid::vsock_service`]`(p)`.
//!
//! Framing is the shared [`FramedConnection`]; this module only opens sockets.

use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::sync::OnceLock;
use std::time::Duration;

use dsa_transport::stream::{
    accept_with_timeout, FramedConnection, RawStreamConnection, StreamSocket,
};
use dsa_transport::{Connection, Connector, Listener, TransportError};
use windows_sys::Win32::Networking::WinSock::{
    accept, bind, closesocket, connect, ioctlsocket, listen, recv, send, setsockopt, shutdown,
    socket, WSAGetLastError, WSAPoll, WSAStartup, FIONBIO, INVALID_SOCKET, POLLRDNORM, SD_BOTH,
    SOCKADDR, SOCKET, SOCKET_ERROR, SOCK_STREAM, SOMAXCONN, WSADATA, WSAEAFNOSUPPORT,
    WSAEPROTONOSUPPORT, WSAESOCKTNOSUPPORT, WSAPOLLFD,
};

use crate::guid::Guid;

/// The Hyper-V socket address family.
const AF_HYPERV: i32 = 34;
/// The only protocol of `AF_HYPERV`.
const HV_PROTOCOL_RAW: i32 = 1;
/// Socket option (level `HV_PROTOCOL_RAW`): connect timeout in milliseconds.
const HVSOCKET_CONNECT_TIMEOUT: i32 = 1;

/// `SOCKADDR_HV` from `hvsocket.h`.
#[repr(C)]
struct SockaddrHv {
    family: u16,
    reserved: u16,
    vm_id: Guid,
    service_id: Guid,
}

fn sockaddr(vm_id: Guid, service_id: Guid) -> SockaddrHv {
    SockaddrHv {
        family: AF_HYPERV as u16,
        reserved: 0,
        vm_id,
        service_id,
    }
}

fn winsock_startup() -> io::Result<()> {
    static RESULT: OnceLock<i32> = OnceLock::new();
    let result = *RESULT.get_or_init(|| {
        // SAFETY: WSADATA is plain data that WSAStartup fills in.
        let mut data: WSADATA = unsafe { zeroed() };
        // SAFETY: no preconditions; the reference count it takes is never released, which
        // is what a process that keeps sockets until exit wants.
        unsafe { WSAStartup(0x0202, &mut data) }
    });
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result))
    }
}

fn last_error() -> io::Error {
    // SAFETY: no preconditions.
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

/// Turns a socket creation failure into `Unsupported` when the machine has no Hyper-V
/// sockets, so callers can tell "not here" from "broken".
fn creation_error(error: io::Error) -> TransportError {
    match error.raw_os_error() {
        Some(code)
            if code == WSAEAFNOSUPPORT
                || code == WSAEPROTONOSUPPORT
                || code == WSAESOCKTNOSUPPORT =>
        {
            TransportError::Unsupported(format!(
                "this machine has no Hyper-V sockets ({error}); the Virtual Machine Platform \
                 feature that WSL 2 needs provides them"
            ))
        }
        _ => error.into(),
    }
}

/// An owned Hyper-V socket.
///
/// Read timeouts wait with `WSAPoll` before each `recv` instead of using `SO_RCVTIMEO`:
/// when an `SO_RCVTIMEO` timeout expires on a Hyper-V socket the connection is aborted,
/// and a timeout has to leave the connection usable.
struct HvSocket {
    raw: SOCKET,
    read_timeout: Option<Duration>,
}

impl HvSocket {
    fn new() -> Result<HvSocket, TransportError> {
        winsock_startup().map_err(creation_error)?;
        // SAFETY: plain socket creation; the result is checked.
        let raw = unsafe { socket(AF_HYPERV, SOCK_STREAM, HV_PROTOCOL_RAW) };
        if raw == INVALID_SOCKET {
            return Err(creation_error(last_error()));
        }
        Ok(HvSocket::from_raw(raw))
    }

    fn from_raw(raw: SOCKET) -> HvSocket {
        HvSocket {
            raw,
            read_timeout: None,
        }
    }

    /// Waits until the socket is readable, for at most `timeout`.
    fn wait_readable(&self, timeout: Duration) -> io::Result<()> {
        let millis = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut poll = WSAPOLLFD {
            fd: self.raw,
            events: POLLRDNORM,
            revents: 0,
        };
        // SAFETY: one valid WSAPOLLFD.
        let ready = unsafe { WSAPoll(&mut poll, 1, millis) };
        match ready {
            SOCKET_ERROR => Err(last_error()),
            0 => Err(io::Error::from(io::ErrorKind::TimedOut)),
            // Readable, or closed or failed, which the recv that follows reports.
            _ => Ok(()),
        }
    }

    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        let mut mode: u32 = nonblocking.into();
        // SAFETY: FIONBIO takes a pointer to a u32 flag.
        if unsafe { ioctlsocket(self.raw, FIONBIO, &mut mode) } == SOCKET_ERROR {
            return Err(last_error());
        }
        Ok(())
    }

    fn set_option_u32(&self, level: i32, name: i32, value: u32) -> io::Result<()> {
        let bytes = value.to_ne_bytes();
        // SAFETY: the option value is a 4-byte buffer that outlives the call.
        let result =
            unsafe { setsockopt(self.raw, level, name, bytes.as_ptr(), bytes.len() as i32) };
        if result == SOCKET_ERROR {
            return Err(last_error());
        }
        Ok(())
    }
}

impl Drop for HvSocket {
    fn drop(&mut self) {
        // SAFETY: the socket is owned and closed exactly once.
        unsafe { closesocket(self.raw) };
    }
}

impl Read for HvSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(timeout) = self.read_timeout {
            self.wait_readable(timeout)?;
        }
        let len = buf.len().min(i32::MAX as usize) as i32;
        // SAFETY: buf is valid for len bytes.
        let read = unsafe { recv(self.raw, buf.as_mut_ptr(), len, 0) };
        if read == SOCKET_ERROR {
            return Err(last_error());
        }
        Ok(read as usize)
    }
}

impl Write for HvSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(i32::MAX as usize) as i32;
        // SAFETY: buf is valid for len bytes.
        let written = unsafe { send(self.raw, buf.as_ptr(), len, 0) };
        if written == SOCKET_ERROR {
            return Err(last_error());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StreamSocket for HvSocket {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout = timeout;
        Ok(())
    }

    fn shutdown(&mut self) {
        // SAFETY: shutting down an owned socket.
        unsafe { shutdown(self.raw, SD_BOTH) };
    }
}

/// Listens for connections from a VM (or from the host, with [`Guid::LOOPBACK`]).
pub struct HvSocketListener {
    socket: HvSocket,
}

impl HvSocketListener {
    /// Listens on `service_id` for connections from the partition `vm_id`.
    pub fn bind(vm_id: Guid, service_id: Guid) -> Result<HvSocketListener, TransportError> {
        let socket = HvSocket::new()?;
        let address = sockaddr(vm_id, service_id);
        // SAFETY: address is a valid SOCKADDR_HV of the size passed.
        let bound = unsafe {
            bind(
                socket.raw,
                &address as *const SockaddrHv as *const SOCKADDR,
                size_of::<SockaddrHv>() as i32,
            )
        };
        if bound == SOCKET_ERROR {
            return Err(last_error().into());
        }
        // SAFETY: listening on a bound socket.
        if unsafe { listen(socket.raw, SOMAXCONN as i32) } == SOCKET_ERROR {
            return Err(last_error().into());
        }
        // Non-blocking so that accept can time out.
        socket.set_nonblocking(true)?;
        Ok(HvSocketListener { socket })
    }
}

impl Listener for HvSocketListener {
    fn accept(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let accepted = accept_with_timeout(timeout, || {
            // SAFETY: the peer address is not requested.
            let raw =
                unsafe { accept(self.socket.raw, std::ptr::null_mut(), std::ptr::null_mut()) };
            if raw == INVALID_SOCKET {
                Err(last_error())
            } else {
                Ok(HvSocket::from_raw(raw))
            }
        })?;
        // An accepted socket inherits non-blocking mode; reads use timeouts instead.
        accepted.set_nonblocking(false)?;
        Ok(Box::new(FramedConnection::new(accepted)))
    }
}

/// Listens on Hyper-V sockets and yields [RawStreamConnection]s without framing.
pub struct RawHvSocketListener {
    socket: HvSocket,
}

impl RawHvSocketListener {
    /// Listens on service_id for connections from the partition m_id.
    pub fn bind(vm_id: Guid, service_id: Guid) -> Result<RawHvSocketListener, TransportError> {
        let listener = HvSocketListener::bind(vm_id, service_id)?;
        Ok(RawHvSocketListener {
            socket: listener.socket,
        })
    }
}

impl Listener for RawHvSocketListener {
    fn accept(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let accepted = accept_with_timeout(timeout, || {
            // SAFETY: the peer address is not requested.
            let raw =
                unsafe { accept(self.socket.raw, std::ptr::null_mut(), std::ptr::null_mut()) };
            if raw == INVALID_SOCKET {
                Err(last_error())
            } else {
                Ok(HvSocket::from_raw(raw))
            }
        })?;
        // An accepted socket inherits non-blocking mode; reads use timeouts instead.
        accepted.set_nonblocking(false)?;
        Ok(Box::new(RawStreamConnection::new(accepted)))
    }
}

/// Connects to a service in a VM (or on the host, with [`Guid::LOOPBACK`]).
pub struct HvSocketConnector {
    pub vm_id: Guid,
    pub service_id: Guid,
}

impl Connector for HvSocketConnector {
    fn connect(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let socket = HvSocket::new()?;
        let millis = timeout.as_millis().clamp(1, u32::MAX as u128) as u32;
        socket.set_option_u32(HV_PROTOCOL_RAW, HVSOCKET_CONNECT_TIMEOUT, millis)?;
        let address = sockaddr(self.vm_id, self.service_id);
        // SAFETY: address is a valid SOCKADDR_HV of the size passed.
        let connected = unsafe {
            connect(
                socket.raw,
                &address as *const SockaddrHv as *const SOCKADDR,
                size_of::<SockaddrHv>() as i32,
            )
        };
        if connected == SOCKET_ERROR {
            return Err(last_error().into());
        }
        Ok(Box::new(FramedConnection::new(socket)))
    }
}
