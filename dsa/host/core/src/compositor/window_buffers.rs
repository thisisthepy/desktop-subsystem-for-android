//! What one task window holds in buffers of the window's size, and the line the window
//! manager logs when that changes.
//!
//! A task window's picture passes through several buffers as large as the task can be: the
//! guest's ColorBuffers the hwcomposer composes the task into (the renderer's), the texture the
//! renderer copies each frame into (the window manager's, which the renderer imports and so
//! does not hold twice), the window's swapchain, and, while the input trace is on, a copy of the
//! last frame to compare a press with. Each is a whole frame: 24 MiB for a task of 3072x2048.
//! The window manager says what it holds for each window whenever that changes, so the memory a
//! window costs can be read from the log next to the renderer's own line for the same task.

/// Bytes in a MiB.
const MIB: f64 = 1024.0 * 1024.0;

/// The bytes of one buffer of `size` pixels of four bytes each.
pub fn buffer_bytes((width, height): (u32, u32)) -> u64 {
    u64::from(width) * u64::from(height) * 4
}

/// The window-sized buffers the window manager holds for one task window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowBuffers {
    /// The texture the renderer copies the task's frames into.
    pub texture: Option<(u32, u32)>,
    /// The frame swapchain's buffers: their size and how many there are.
    pub swapchain: Option<((u32, u32), u32)>,
    /// The splash screen's swapchain, while the window shows one: two buffers of the window's
    /// size.
    pub splash: Option<(u32, u32)>,
    /// The copy of the last frame the input trace compares a press with.
    pub last_frame: Option<(u32, u32)>,
}

/// How many buffers a splash swapchain has.
const SPLASH_BUFFERS: u32 = 2;

impl WindowBuffers {
    /// Every part, as (what it is, its buffers' size, how many buffers).
    fn parts(&self) -> Vec<(&'static str, (u32, u32), u32)> {
        let mut parts = Vec::new();
        if let Some(size) = self.texture {
            parts.push(("frame texture", size, 1));
        }
        if let Some((size, count)) = self.swapchain {
            parts.push(("swapchain", size, count));
        }
        if let Some(size) = self.splash {
            parts.push(("splash swapchain", size, SPLASH_BUFFERS));
        }
        if let Some(size) = self.last_frame {
            parts.push(("last frame copy for the input trace", size, 1));
        }
        parts
    }

    /// How many window-sized buffers these are.
    pub fn count(&self) -> u32 {
        self.parts().iter().map(|(_, _, count)| count).sum()
    }

    /// What they hold together, in bytes.
    pub fn bytes(&self) -> u64 {
        self.parts()
            .iter()
            .map(|(_, size, count)| buffer_bytes(*size) * u64::from(*count))
            .sum()
    }

    /// The log line for task `task_id`'s window holding these.
    pub fn describe(&self, task_id: i32) -> String {
        let parts = self.parts();
        if parts.is_empty() {
            return format!("task {task_id}: the window manager holds no window-sized buffer");
        }
        let each: Vec<String> = parts
            .iter()
            .map(|(what, (width, height), count)| {
                let mib = (buffer_bytes((*width, *height)) * u64::from(*count)) as f64 / MIB;
                if *count == 1 {
                    format!("{what} {width}x{height} ({mib:.1} MiB)")
                } else {
                    format!("{what} {count} x {width}x{height} ({mib:.1} MiB)")
                }
            })
            .collect();
        let count = self.count();
        format!(
            "task {task_id}: the window manager holds {count} window-sized buffer{} of {:.1} \
             MiB: {}",
            if count == 1 { "" } else { "s" },
            self.bytes() as f64 / MIB,
            each.join(", ")
        )
    }
}

/// Says what a window holds only when it changed since the last time it was said.
#[derive(Debug, Default)]
pub struct BufferReport {
    said: Option<WindowBuffers>,
}

impl BufferReport {
    pub fn new() -> BufferReport {
        BufferReport::default()
    }

    /// The line to log for task `task_id`'s window, now holding `now`, or `None` when that is
    /// what was said last. Nothing is said before the window holds anything.
    pub fn changed(&mut self, task_id: i32, now: WindowBuffers) -> Option<String> {
        if self.said == Some(now) {
            return None;
        }
        if self.said.is_none() && now == WindowBuffers::default() {
            return None;
        }
        self.said = Some(now);
        Some(now.describe(task_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr1_window_buffers_count_each_buffer_the_window_manager_holds() {
        // A YouTube window of 2880x1704 in buffers rounded up to 3072x2048, drawing.
        let buffers = WindowBuffers {
            texture: Some((3072, 2048)),
            swapchain: Some(((3072, 2048), 3)),
            splash: None,
            last_frame: None,
        };
        assert_eq!(buffers.count(), 4);
        assert_eq!(buffers.bytes(), 4 * 3072 * 2048 * 4);
        assert_eq!(
            buffers.describe(12),
            "task 12: the window manager holds 4 window-sized buffers of 96.0 MiB: frame texture \
             3072x2048 (24.0 MiB), swapchain 3 x 3072x2048 (72.0 MiB)"
        );
    }

    #[test]
    fn nfr1_the_last_frame_copy_counts_only_while_the_input_trace_keeps_one() {
        let mut buffers = WindowBuffers {
            texture: Some((1024, 1024)),
            swapchain: Some(((1024, 1024), 2)),
            ..WindowBuffers::default()
        };
        assert_eq!(buffers.count(), 3);
        buffers.last_frame = Some((572, 937));
        assert_eq!(buffers.count(), 4);
        assert_eq!(buffers.bytes(), 3 * 1024 * 1024 * 4 + 572 * 937 * 4);
    }

    #[test]
    fn nfr1_a_window_that_holds_nothing_says_so() {
        assert_eq!(WindowBuffers::default().count(), 0);
        assert_eq!(
            WindowBuffers::default().describe(3),
            "task 3: the window manager holds no window-sized buffer"
        );
    }

    #[test]
    fn nfr1_the_buffers_are_said_only_when_they_change() {
        let mut report = BufferReport::new();
        // Nothing held yet: nothing to say.
        assert_eq!(report.changed(7, WindowBuffers::default()), None);
        let drawing = WindowBuffers {
            texture: Some((1024, 1024)),
            swapchain: Some(((1024, 1024), 3)),
            ..WindowBuffers::default()
        };
        assert!(report.changed(7, drawing).is_some());
        assert_eq!(report.changed(7, drawing), None);
        // Still for a while: the swapchain lets its third buffer go.
        let still = WindowBuffers {
            swapchain: Some(((1024, 1024), 2)),
            ..drawing
        };
        assert!(report
            .changed(7, still)
            .is_some_and(|line| line.contains("holds 3 window-sized buffers")));
        // Minimized: everything but the texture goes, and that is said too.
        let hidden = WindowBuffers {
            texture: Some((1024, 1024)),
            ..WindowBuffers::default()
        };
        assert!(report
            .changed(7, hidden)
            .is_some_and(|line| line.contains("holds 1 window-sized buffer of 4.0 MiB")));
        assert!(report
            .changed(7, WindowBuffers::default())
            .is_some_and(|line| line.contains("holds no window-sized buffer")));
    }
}
