//! Platform-neutral graphics stream bookkeeping, renderer interfaces, and the rules for
//! task windows.
//!
//! The compositor receives guest graphics streams over the transport and delegates them
//! to a [`GraphicsRenderer`]. [`task_windows`] decides which window each task has and
//! which window each display's frames go to, [`window_sizing`] how a window's size and
//! scale reach its task, and [`resize_latency`] measures how long a resized window waits
//! for a frame of its new size; the platform only carries it out.

pub mod resize_latency;
pub mod task_windows;
pub mod window_sizing;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use dsa_transport::Connection;

/// The graphics command stream port (vsock port 5566).
pub const GRAPHICS_COMMAND_PORT: u32 = 5566;

/// The graphics process pipe port (vsock port 5567).
pub const GRAPHICS_PROCESS_PIPE_PORT: u32 = 5567;

/// The kind of graphics stream received from the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphicsStreamKind {
    /// Command stream for OpenGL ES and Vulkan commands (port 5566).
    Command,
    /// Process pipe for buffer sharing and protocol synchronisation (port 5567).
    ProcessPipe,
}

impl GraphicsStreamKind {
    /// Returns the vsock port corresponding to this stream kind.
    pub const fn port(&self) -> u32 {
        match self {
            GraphicsStreamKind::Command => GRAPHICS_COMMAND_PORT,
            GraphicsStreamKind::ProcessPipe => GRAPHICS_PROCESS_PIPE_PORT,
        }
    }

    /// Returns the stream kind matching the given vsock port, if known.
    pub const fn from_port(port: u32) -> Option<GraphicsStreamKind> {
        match port {
            GRAPHICS_COMMAND_PORT => Some(GraphicsStreamKind::Command),
            GRAPHICS_PROCESS_PIPE_PORT => Some(GraphicsStreamKind::ProcessPipe),
            _ => None,
        }
    }
}

/// The platform-neutral interface for rendering or handling guest graphics streams.
pub trait GraphicsRenderer: Send + Sync + 'static {
    /// Serves an accepted graphics connection of the given kind until the peer disconnects
    /// or an error occurs.
    fn serve_stream(
        &self,
        kind: GraphicsStreamKind,
        connection: Box<dyn Connection>,
    ) -> Result<(), String>;
}

/// A fake renderer for testing that records accepted streams.
#[derive(Default)]
pub struct FakeRenderer {
    command_streams: AtomicUsize,
    process_pipe_streams: AtomicUsize,
    active_streams: AtomicUsize,
    record: Mutex<Vec<GraphicsStreamKind>>,
}

impl FakeRenderer {
    pub fn new() -> FakeRenderer {
        FakeRenderer::default()
    }

    /// The number of command stream connections served.
    pub fn command_stream_count(&self) -> usize {
        self.command_streams.load(Ordering::Relaxed)
    }

    /// The number of process pipe connections served.
    pub fn process_pipe_stream_count(&self) -> usize {
        self.process_pipe_streams.load(Ordering::Relaxed)
    }

    /// The total number of accepted streams recorded in order of arrival.
    pub fn recorded_streams(&self) -> Vec<GraphicsStreamKind> {
        self.record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The current number of active streams.
    pub fn active_stream_count(&self) -> usize {
        self.active_streams.load(Ordering::Relaxed)
    }
}

impl GraphicsRenderer for FakeRenderer {
    fn serve_stream(
        &self,
        kind: GraphicsStreamKind,
        _connection: Box<dyn Connection>,
    ) -> Result<(), String> {
        match kind {
            GraphicsStreamKind::Command => {
                self.command_streams.fetch_add(1, Ordering::Relaxed);
            }
            GraphicsStreamKind::ProcessPipe => {
                self.process_pipe_streams.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.active_streams.fetch_add(1, Ordering::Relaxed);
        self.record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(kind);
        self.active_streams.fetch_sub(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Bookkeeping for active and total graphics streams.
#[derive(Debug, Default)]
pub struct StreamTracker {
    command_active: AtomicUsize,
    command_total: AtomicUsize,
    process_pipe_active: AtomicUsize,
    process_pipe_total: AtomicUsize,
}

impl StreamTracker {
    pub fn new() -> StreamTracker {
        StreamTracker::default()
    }

    /// Increments counts when a stream connects and returns an active stream guard.
    pub fn stream_started(&self, kind: GraphicsStreamKind) {
        match kind {
            GraphicsStreamKind::Command => {
                self.command_active.fetch_add(1, Ordering::Relaxed);
                self.command_total.fetch_add(1, Ordering::Relaxed);
            }
            GraphicsStreamKind::ProcessPipe => {
                self.process_pipe_active.fetch_add(1, Ordering::Relaxed);
                self.process_pipe_total.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Decrements the active count when a stream closes.
    pub fn stream_ended(&self, kind: GraphicsStreamKind) {
        match kind {
            GraphicsStreamKind::Command => {
                self.command_active.fetch_sub(1, Ordering::Relaxed);
            }
            GraphicsStreamKind::ProcessPipe => {
                self.process_pipe_active.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    pub fn command_active(&self) -> usize {
        self.command_active.load(Ordering::Relaxed)
    }

    pub fn command_total(&self) -> usize {
        self.command_total.load(Ordering::Relaxed)
    }

    pub fn process_pipe_active(&self) -> usize {
        self.process_pipe_active.load(Ordering::Relaxed)
    }

    pub fn process_pipe_total(&self) -> usize {
        self.process_pipe_total.load(Ordering::Relaxed)
    }

    pub fn total_active(&self) -> usize {
        self.command_active() + self.process_pipe_active()
    }
}
