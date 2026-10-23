//! Integration adapter point for the native gfxstream host renderer backend.
//!
//! The gfxstream host renderer (`gfxstream_backend.dll`) remotes guest Vulkan and
//! OpenGL ES calls onto the host Vulkan driver. This adapter implements [`GraphicsRenderer`]
//! and serves as the integration point between accepted Hyper-V socket streams and the native
//! graphics backend.

use std::path::{Path, PathBuf};

use dsa_host_core::compositor::{GraphicsRenderer, GraphicsStreamKind};
use dsa_transport::Connection;
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

/// Default filename of the native gfxstream host renderer DLL.
pub const GFXSTREAM_BACKEND_DLL: &str = "gfxstream_backend.dll";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Adapter connecting guest graphics streams to the native gfxstream host renderer.
pub struct GfxstreamAdapter {
    dll_path: Option<PathBuf>,
}

impl GfxstreamAdapter {
    /// Creates a new adapter, searching for `gfxstream_backend.dll` at `custom_path`, beside
    /// the current executable, or in the current directory.
    pub fn new(custom_path: Option<PathBuf>) -> GfxstreamAdapter {
        let dll_path = custom_path.or_else(Self::find_backend_dll);
        GfxstreamAdapter { dll_path }
    }

    /// Returns the resolved path to `gfxstream_backend.dll` if found.
    pub fn backend_path(&self) -> Option<&Path> {
        self.dll_path.as_deref()
    }

    /// Whether the native backend DLL was found on the system.
    pub fn is_available(&self) -> bool {
        self.dll_path.as_ref().map(|p| p.is_file()).unwrap_or(false)
    }

    fn find_backend_dll() -> Option<PathBuf> {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join(GFXSTREAM_BACKEND_DLL);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        let local_candidate = PathBuf::from(GFXSTREAM_BACKEND_DLL);
        if local_candidate.is_file() {
            Some(local_candidate)
        } else {
            None
        }
    }
}

impl GraphicsRenderer for GfxstreamAdapter {
    fn serve_stream(
        &self,
        kind: GraphicsStreamKind,
        mut connection: Box<dyn Connection>,
    ) -> Result<(), String> {
        if let Some(path) = &self.dll_path {
            let path_wide = wide(&path.to_string_lossy());
            // SAFETY: attempting to load the native backend library.
            let handle = unsafe { LoadLibraryW(path_wide.as_ptr()) };
            if handle.is_null() {
                return Err(format!(
                    "could not load graphics renderer library {}: os error {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            // Once the native gfxstream library is loaded, accepted streams will be
            // forwarded to the exported gfxstream stream handler function.
            Ok(())
        } else {
            // When the backend DLL has not been deployed yet, the stream is acknowledged
            // and maintained so that guest connections do not immediately crash.
            let buf = [0u8; 1024];
            while let Ok(n) = connection.recv(None) {
                if n.is_empty() {
                    break;
                }
            }
            let _ = kind;
            let _ = buf;
            Ok(())
        }
    }
}
