//! The control channel over vsock (`AF_VSOCK`), as the guest opens it.
//!
//! The guest runs inside the WSL VM, so it reaches the host as CID
//! [`VMADDR_CID_HOST`]. Framing is the shared [`FramedConnection`]; this module only opens
//! sockets.

use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

use dsa_transport::stream::{accept_with_timeout, FramedConnection, StreamSocket};
use dsa_transport::{Connection, Connector, Listener, TransportError};

/// Any CID: a listener bound to it accepts from the host and from local peers.
pub const VMADDR_CID_ANY: u32 = libc::VMADDR_CID_ANY;
/// This machine, through the vsock loopback transport.
pub const VMADDR_CID_LOCAL: u32 = libc::VMADDR_CID_LOCAL;
/// The host, as seen from a VM.
pub const VMADDR_CID_HOST: u32 = libc::VMADDR_CID_HOST;

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(last_error())
    } else {
        Ok(result)
    }
}

fn address(cid: u32, port: u32) -> libc::sockaddr_vm {
    // SAFETY: sockaddr_vm is plain data; zero is a valid starting value.
    let mut address: libc::sockaddr_vm = unsafe { zeroed() };
    address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    address.svm_cid = cid;
    address.svm_port = port;
    address
}

/// Turns a socket creation failure into `Unsupported` when the kernel has no vsock.
fn creation_error(error: io::Error) -> TransportError {
    match error.raw_os_error() {
        Some(libc::EAFNOSUPPORT) | Some(libc::EPROTONOSUPPORT) => {
            TransportError::Unsupported(format!("this kernel has no vsock support ({error})"))
        }
        _ => error.into(),
    }
}

/// An owned vsock socket.
struct VsockSocket(OwnedFd);

impl VsockSocket {
    fn new() -> Result<VsockSocket, TransportError> {
        // SAFETY: plain socket creation; the result is checked.
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        let fd = check(fd).map_err(creation_error)?;
        // SAFETY: fd is a freshly created descriptor that nothing else owns.
        Ok(VsockSocket(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        // SAFETY: F_GETFL/F_SETFL on an owned descriptor.
        let flags = check(unsafe { libc::fcntl(self.fd(), libc::F_GETFL) })?;
        let flags = if nonblocking {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        // SAFETY: as above.
        check(unsafe { libc::fcntl(self.fd(), libc::F_SETFL, flags) })?;
        Ok(())
    }
}

impl Read for VsockSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: buf is valid for buf.len() bytes.
        let read = unsafe { libc::recv(self.fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if read < 0 {
            return Err(last_error());
        }
        Ok(read as usize)
    }
}

impl Write for VsockSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // MSG_NOSIGNAL: a closed peer is an error to report, not a SIGPIPE.
        // SAFETY: buf is valid for buf.len() bytes.
        let written = unsafe {
            libc::send(
                self.fd(),
                buf.as_ptr().cast(),
                buf.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if written < 0 {
            return Err(last_error());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StreamSocket for VsockSocket {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        // A zero timeval means "no timeout"; FramedConnection never passes a zero timeout.
        let timeout = timeout.unwrap_or(Duration::ZERO);
        let value = libc::timeval {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_usec: timeout.subsec_micros() as libc::suseconds_t,
        };
        // SAFETY: the option value is a timeval that outlives the call.
        check(unsafe {
            libc::setsockopt(
                self.fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&value as *const libc::timeval).cast(),
                size_of::<libc::timeval>() as libc::socklen_t,
            )
        })?;
        Ok(())
    }

    fn shutdown(&mut self) {
        // SAFETY: shutting down an owned descriptor.
        unsafe { libc::shutdown(self.fd(), libc::SHUT_RDWR) };
    }

    fn try_clone(&self) -> io::Result<VsockSocket> {
        Ok(VsockSocket(self.0.try_clone()?))
    }
}

/// Listens for vsock connections.
pub struct VsockListener {
    socket: VsockSocket,
}

impl VsockListener {
    /// Listens on `port` for connections addressed to `cid` (usually [`VMADDR_CID_ANY`]).
    pub fn bind(cid: u32, port: u32) -> Result<VsockListener, TransportError> {
        let socket = VsockSocket::new()?;
        let address = address(cid, port);
        // SAFETY: address is a valid sockaddr_vm of the size passed.
        check(unsafe {
            libc::bind(
                socket.fd(),
                (&address as *const libc::sockaddr_vm).cast(),
                size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        })?;
        // SAFETY: listening on a bound socket.
        check(unsafe { libc::listen(socket.fd(), libc::SOMAXCONN) })?;
        socket.set_nonblocking(true)?;
        Ok(VsockListener { socket })
    }
}

impl Listener for VsockListener {
    fn accept(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let accepted = accept_with_timeout(timeout, || {
            // SAFETY: the peer address is not requested. accept4 does not pass O_NONBLOCK
            // on to the new socket, so it starts blocking.
            let fd = unsafe {
                libc::accept4(
                    self.socket.fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC,
                )
            };
            let fd = check(fd)?;
            // SAFETY: fd was just returned by accept4 and nothing else owns it.
            Ok(VsockSocket(unsafe { OwnedFd::from_raw_fd(fd) }))
        })?;
        Ok(Box::new(FramedConnection::new(accepted)))
    }
}

/// Connects to a vsock listener, normally the host's at [`VMADDR_CID_HOST`].
pub struct VsockConnector {
    pub cid: u32,
    pub port: u32,
}

impl Connector for VsockConnector {
    fn connect(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let socket = VsockSocket::new()?;
        socket.set_nonblocking(true)?;
        let address = address(self.cid, self.port);
        // SAFETY: address is a valid sockaddr_vm of the size passed.
        let result = unsafe {
            libc::connect(
                socket.fd(),
                (&address as *const libc::sockaddr_vm).cast(),
                size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };
        if result < 0 {
            let error = last_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(error.into());
            }
            let mut poll = libc::pollfd {
                fd: socket.fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            let millis = timeout.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
            // SAFETY: one valid pollfd.
            let ready = check(unsafe { libc::poll(&mut poll, 1, millis) })?;
            if ready == 0 {
                return Err(TransportError::Timeout);
            }
            let mut status: libc::c_int = 0;
            let mut len = size_of::<libc::c_int>() as libc::socklen_t;
            // SAFETY: SO_ERROR writes one c_int.
            check(unsafe {
                libc::getsockopt(
                    socket.fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    (&mut status as *mut libc::c_int).cast(),
                    &mut len,
                )
            })?;
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status).into());
            }
        }
        socket.set_nonblocking(false)?;
        Ok(Box::new(FramedConnection::new(socket)))
    }
}
