//! Unix socket listener and connector for the local API.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dsa_transport::local::{LocalListener, Peer};
use dsa_transport::stream::{accept_with_timeout, FramedConnection, StreamSocket};
use dsa_transport::{Connection, Connector, TransportError};

struct Socket(UnixStream);

impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Socket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl StreamSocket for Socket {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }
    fn shutdown(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// The user ID of the process on the other end of `stream`, as the kernel reports it.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "linux")]
    {
        // SAFETY: zero is a valid ucred; getsockopt fills it in.
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: SO_PEERCRED writes one ucred into a buffer of the size given.
        let result = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: getpeereid writes the two IDs.
        if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// The user ID of this process.
pub fn current_uid() -> u32 {
    // SAFETY: no preconditions.
    unsafe { libc::getuid() }
}

/// Listens on a socket file. Dropping it removes the file.
pub struct UnixLocalListener {
    listener: UnixListener,
    path: PathBuf,
}

impl UnixLocalListener {
    /// Listens at `path` and sets the file's permissions to `mode` (0o600 for one user,
    /// 0o666 when the server checks each peer itself). A socket file left by a previous run
    /// is replaced; any other file there is an error, so a mistyped path cannot delete it.
    pub fn bind(path: &Path, mode: u32) -> Result<UnixLocalListener, TransportError> {
        use std::os::unix::fs::FileTypeExt;
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_socket() => fs::remove_file(path)?,
            Ok(_) => {
                return Err(TransportError::Io {
                    kind: io::ErrorKind::AlreadyExists,
                    message: format!("{} exists and is not a socket", path.display()),
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        listener.set_nonblocking(true)?;
        Ok(UnixLocalListener {
            listener,
            path: path.to_path_buf(),
        })
    }
}

impl Drop for UnixLocalListener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl LocalListener for UnixLocalListener {
    fn accept(&mut self, timeout: Duration) -> Result<(Box<dyn Connection>, Peer), TransportError> {
        let (stream, _) = accept_with_timeout(timeout, || self.listener.accept())?;
        stream.set_nonblocking(false)?;
        let uid = peer_uid(&stream)?;
        let peer = Peer {
            user: format!("uid:{uid}"),
            administrator: uid == 0,
        };
        Ok((Box::new(FramedConnection::new(Socket(stream))), peer))
    }
}

/// Connects to a [`UnixLocalListener`].
pub struct UnixLocalConnector {
    pub path: PathBuf,
}

impl Connector for UnixLocalConnector {
    fn connect(&mut self, _timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        // A local connect either succeeds or fails at once; there is nothing to wait for.
        let stream = UnixStream::connect(&self.path)?;
        Ok(Box::new(FramedConnection::new(Socket(stream))))
    }
}
