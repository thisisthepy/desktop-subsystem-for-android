//! How a task window's buffers are sized, so that most steps of a resize do not remake them.
//!
//! Every buffer on a task's frame path (the guest's ColorBuffers, the window manager's shared
//! texture and the window host's swapchain) follows the size the guest gives its ColorBuffers,
//! which is the task's size rounded up in steps, and each frame fills only its top-left
//! region. A frame carries its content size next to the buffer, and the window shows only that
//! region. Making a buffer is the slow part of a resize (tens of milliseconds for a
//! swapchain's `ResizeBuffers` alone), so a size step that fits the buffers makes none.

/// The part of a buffer of `buffer` pixels that a frame of `content` pixels fills: the
/// content clipped to the buffer. A content size of 0 in either direction comes from a
/// sender that does not say it, whose frames fill the whole buffer.
pub fn content_region(content: (u32, u32), buffer: (u32, u32)) -> (u32, u32) {
    if content.0 == 0 || content.1 == 0 {
        return buffer;
    }
    (content.0.min(buffer.0), content.1.min(buffer.1))
}

/// What a window's swapchain needs before it shows a frame from a texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapchainChange {
    /// There is none yet: make one of this size.
    Create((u32, u32)),
    /// Its buffers are the wrong size for the texture: resize them to this.
    Resize((u32, u32)),
    /// It fits: only the region shown changes.
    Keep,
}

/// How many buffers a window's swapchain has. With two, the buffer a frame is copied into is
/// the one the desktop compositor is still showing, and the copy waits for its next vertical
/// blank; the renderer waits for the copy. The third is always free.
pub fn swapchain_buffers() -> u32 {
    3
}

/// How long a shown window goes without a new frame before its swapchain lets its third
/// buffer go. A still window needs no free buffer to copy into, and the buffer holds a whole
/// frame of the task's size; the next frame brings it back.
pub const IDLE_SWAPCHAIN_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// How many buffers a shown window's swapchain keeps when its last frame came `quiet_for`
/// ago: the fewest a flip-model swapchain can have once the window is still.
pub fn wanted_swapchain_buffers(quiet_for: std::time::Duration) -> u32 {
    if quiet_for >= IDLE_SWAPCHAIN_DELAY {
        2
    } else {
        swapchain_buffers()
    }
}

/// How many buffers a window's swapchain is to have once a frame arrives, `since_last` after
/// the one before (`None` for its first frame), when it was to have `wanted`. A lone frame of
/// a still window is shown with the two buffers it has: taking the third back means resizing
/// the swapchain, which takes longer than waiting for the next vertical blank. A second frame
/// soon after says the window is drawing again, and the third buffer comes back.
pub fn buffers_after_frame(wanted: u32, since_last: Option<std::time::Duration>) -> u32 {
    match since_last {
        Some(gap) if gap >= IDLE_SWAPCHAIN_DELAY => wanted,
        _ => swapchain_buffers(),
    }
}

/// What a swapchain whose buffers are `current` pixels (`None` when there is none) needs to
/// show frames from a texture of `texture` pixels. The buffers follow the texture, which is
/// the largest a frame can be, never a frame's own size, so a window resize changes nothing
/// here.
pub fn swapchain_change(current: Option<(u32, u32)>, texture: (u32, u32)) -> SwapchainChange {
    match current {
        None => SwapchainChange::Create(texture),
        Some(size) if size != texture => SwapchainChange::Resize(texture),
        Some(_) => SwapchainChange::Keep,
    }
}

/// Whether a shared texture of `current` pixels can take the frames of a ColorBuffer of
/// `wanted` pixels. The texture is remade only when the ColorBuffer itself changes size,
/// which the guest does only when the task passes a step of its size.
pub fn texture_fits(current: (u32, u32), wanted: (u32, u32)) -> bool {
    current == wanted
}

/// The visual's scale that shows a frame of `frame` pixels 1:1 when the swapchain's buffers
/// are `buffers` pixels. DirectComposition stretches a swapchain's source region over the
/// whole of its buffers, so this brings the frame back to its own size. A frame is never
/// scaled to its window: the window takes the size of the frame it shows.
pub fn frame_scale(buffers: (u32, u32), frame: (u32, u32)) -> (f32, f32) {
    let scale = |frame: u32, buffer: u32| {
        if frame == 0 || buffer == 0 {
            1.0
        } else {
            frame as f32 / buffer as f32
        }
    };
    (scale(frame.0, buffers.0), scale(frame.1, buffers.1))
}

/// How many pixels a side the resize trace samples of a frame to see if it is all one colour.
pub const SAMPLE_GRID: u32 = 8;

/// The pixels of a frame of `region` pixels the trace samples: the centres of an even grid,
/// row by row, [`SAMPLE_GRID`] to a side.
pub fn sample_points((width, height): (u32, u32)) -> Vec<(u32, u32)> {
    let along = |size: u32, i: u32| {
        ((2 * u64::from(i) + 1) * u64::from(size) / (2 * u64::from(SAMPLE_GRID))) as u32
    };
    (0..SAMPLE_GRID)
        .flat_map(|row| {
            (0..SAMPLE_GRID).map(move |column| (along(width, column), along(height, row)))
        })
        .collect()
}

/// The colour every one of `pixels` has, or `None` when they differ. An app's frame is hardly
/// ever one colour across a whole grid, and a frame nothing was drawn into always is.
pub fn uniform_colour(pixels: &[[u8; 4]]) -> Option<[u8; 4]> {
    let first = *pixels.first()?;
    pixels.iter().all(|pixel| *pixel == first).then_some(first)
}

/// Whether a task window's swapchain is held, by whether the window is shown.
///
/// A minimized window shows nothing, but its swapchain still holds [`swapchain_buffers`]
/// buffers of the task's full size, on the GPU or, on a GPU that shares the computer's memory,
/// in it: tens of MiB a window. While the window is hidden they are let go, and frames that
/// arrive are not copied anywhere; the shared texture keeps the newest. When the window is
/// shown again the swapchain is made again and the newest frame shown from the texture at once,
/// so the window never comes back empty.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HiddenBuffers {
    hidden: bool,
    /// A frame has come since the window was made, so there is one to show again.
    seen: bool,
}

/// What to do with a frame that arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiddenFrame {
    /// The window is shown: present it.
    Present,
    /// The window is hidden: leave it in the texture, present nothing.
    Hold,
}

impl HiddenBuffers {
    pub fn new() -> HiddenBuffers {
        HiddenBuffers::default()
    }

    /// Whether the window is hidden now.
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// The window was hidden. Returns whether its swapchain is to be let go now: only the
    /// first time, while it was shown.
    pub fn hide(&mut self) -> bool {
        !std::mem::replace(&mut self.hidden, true)
    }

    /// A frame arrived.
    pub fn frame(&mut self) -> HiddenFrame {
        self.seen = true;
        if self.hidden {
            HiddenFrame::Hold
        } else {
            HiddenFrame::Present
        }
    }

    /// The window is shown again. Returns whether the newest frame is to be shown again from
    /// the texture: when the window was hidden and a frame has ever come.
    pub fn show(&mut self) -> bool {
        std::mem::replace(&mut self.hidden, false) && self.seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr2_a_still_window_keeps_two_buffers_and_a_drawing_one_three() {
        use std::time::Duration;
        assert_eq!(
            wanted_swapchain_buffers(Duration::ZERO),
            swapchain_buffers()
        );
        assert_eq!(wanted_swapchain_buffers(Duration::from_millis(500)), 3);
        assert_eq!(wanted_swapchain_buffers(IDLE_SWAPCHAIN_DELAY), 2);
        assert_eq!(wanted_swapchain_buffers(Duration::from_secs(60)), 2);
        assert!(IDLE_SWAPCHAIN_DELAY <= Duration::from_secs(2));
    }

    #[test]
    fn nfr2_a_lone_frame_of_a_still_window_does_not_take_its_third_buffer_back() {
        use std::time::Duration;
        // The first frame, and frames in quick succession, get all three buffers.
        assert_eq!(buffers_after_frame(3, None), 3);
        assert_eq!(buffers_after_frame(2, Some(Duration::from_millis(16))), 3);
        // A frame after a still spell keeps the two buffers the window has.
        assert_eq!(buffers_after_frame(2, Some(IDLE_SWAPCHAIN_DELAY)), 2);
        assert_eq!(buffers_after_frame(3, Some(Duration::from_secs(10))), 3);
    }

    #[test]
    fn nfr1_a_minimized_window_lets_its_buffers_go_once_and_holds_its_frames() {
        let mut buffers = HiddenBuffers::new();
        assert_eq!(buffers.frame(), HiddenFrame::Present);
        assert!(buffers.hide());
        assert!(buffers.hidden());
        // A second minimize message lets nothing go twice.
        assert!(!buffers.hide());
        assert_eq!(buffers.frame(), HiddenFrame::Hold);
        assert_eq!(buffers.frame(), HiddenFrame::Hold);
    }

    #[test]
    fn nfr1_a_restored_window_shows_its_newest_frame_again_at_once() {
        let mut buffers = HiddenBuffers::new();
        buffers.frame();
        buffers.hide();
        assert!(buffers.show());
        assert!(!buffers.hidden());
        assert_eq!(buffers.frame(), HiddenFrame::Present);
        // A restore message for a window that was not hidden shows nothing again.
        assert!(!buffers.show());
    }

    #[test]
    fn nfr1_a_window_hidden_before_its_first_frame_waits_for_one() {
        let mut buffers = HiddenBuffers::new();
        assert!(buffers.hide());
        assert!(!buffers.show());
        assert_eq!(buffers.frame(), HiddenFrame::Present);
    }

    #[test]
    fn fr4_the_samples_of_a_frame_spread_over_it_and_stay_inside() {
        let points = sample_points((900, 747));
        assert_eq!(points.len(), (SAMPLE_GRID * SAMPLE_GRID) as usize);
        assert!(points.iter().all(|&(x, y)| x < 900 && y < 747));
        assert!(points.iter().any(|&(x, _)| x < 100));
        assert!(points.iter().any(|&(x, _)| x > 800));
        assert!(points.iter().any(|&(_, y)| y < 100));
        assert!(points.iter().any(|&(_, y)| y > 650));
        // The smallest frame still names pixels it has.
        assert!(sample_points((1, 1)).iter().all(|&point| point == (0, 0)));
    }

    #[test]
    fn fr4_a_frame_all_one_colour_is_told_apart_from_a_drawn_one() {
        let black = [0, 0, 0, 255];
        assert_eq!(uniform_colour(&[black; 64]), Some(black));
        let mut drawn = [black; 64];
        drawn[63] = [255, 255, 255, 255];
        assert_eq!(uniform_colour(&drawn), None);
        assert_eq!(uniform_colour(&[]), None);
    }

    #[test]
    fn fr5_a_frame_never_waits_for_the_buffer_the_desktop_is_showing() {
        assert!(swapchain_buffers() >= 3);
    }

    #[test]
    fn fr4_a_frame_is_shown_1_to_1_whatever_its_buffers() {
        let buffers = (1920, 1088);
        for frame in [(900, 700), (607, 1080), (1920, 1088), (1, 1)] {
            let (x, y) = frame_scale(buffers, frame);
            // One frame pixel is one window pixel in each direction.
            assert!((x * buffers.0 as f32 / frame.0 as f32 - 1.0).abs() < 1e-4);
            assert!((y * buffers.1 as f32 / frame.1 as f32 - 1.0).abs() < 1e-4);
        }
        assert_eq!(frame_scale((0, 0), (607, 1080)), (1.0, 1.0));
    }

    #[test]
    fn fr4_a_frame_shows_only_the_region_its_content_fills() {
        assert_eq!(content_region((800, 600), (1920, 1080)), (800, 600));
        assert_eq!(content_region((1920, 1080), (1920, 1080)), (1920, 1080));
    }

    #[test]
    fn fr4_content_larger_than_the_buffer_is_clipped_to_it() {
        assert_eq!(content_region((2000, 600), (1920, 1080)), (1920, 600));
        assert_eq!(content_region((800, 1200), (1920, 1080)), (800, 1080));
    }

    #[test]
    fn fr4_a_frame_without_a_content_size_fills_its_buffer() {
        assert_eq!(content_region((0, 0), (1280, 720)), (1280, 720));
        assert_eq!(content_region((640, 0), (1280, 720)), (1280, 720));
    }

    #[test]
    fn fr4_resizing_a_window_keeps_its_swapchain() {
        let texture = (1920, 1080);
        assert_eq!(
            swapchain_change(None, texture),
            SwapchainChange::Create(texture)
        );
        // Frames of every size come from the same texture: nothing is resized.
        assert_eq!(
            swapchain_change(Some(texture), texture),
            SwapchainChange::Keep
        );
    }

    #[test]
    fn fr4_a_new_texture_size_resizes_the_swapchain_once() {
        assert_eq!(
            swapchain_change(Some((1920, 1080)), (2560, 1440)),
            SwapchainChange::Resize((2560, 1440))
        );
        assert_eq!(
            swapchain_change(Some((2560, 1440)), (2560, 1440)),
            SwapchainChange::Keep
        );
    }

    #[test]
    fn fr4_the_shared_texture_is_kept_while_the_color_buffer_keeps_its_size() {
        assert!(texture_fits((1920, 1080), (1920, 1080)));
        assert!(!texture_fits((1920, 1080), (2560, 1440)));
    }
}
