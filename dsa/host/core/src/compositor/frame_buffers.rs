//! How a task window's buffers are sized, so that resizing the window does not remake them.
//!
//! Every buffer on a task's frame path (the guest's ColorBuffers, the window manager's shared
//! texture and the window host's swapchain) is made once at the largest size the task can
//! have, and each frame fills only its top-left region. A frame carries its content size
//! next to the buffer, and the window shows only that region. Making a buffer is the slow
//! part of a resize (tens of milliseconds for a swapchain's `ResizeBuffers` alone), so a size
//! step that fits the buffers makes none.

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
/// which the guest does only when its display does.
pub fn texture_fits(current: (u32, u32), wanted: (u32, u32)) -> bool {
    current == wanted
}

/// The scale that makes a frame fill a client area of `view` pixels when the swapchain's
/// buffers are `buffers` pixels. DirectComposition stretches a swapchain's source region
/// over the whole of its buffers, so a frame of any size first takes the buffers' size, and
/// the visual scales from there.
pub fn visual_scale(buffers: (u32, u32), view: (u32, u32)) -> (f32, f32) {
    let scale = |buffer: u32, view: u32| {
        if buffer == 0 || view == 0 {
            1.0
        } else {
            view as f32 / buffer as f32
        }
    };
    (scale(buffers.0, view.0), scale(buffers.1, view.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr5_a_frame_never_waits_for_the_buffer_the_desktop_is_showing() {
        assert!(swapchain_buffers() >= 3);
    }

    #[test]
    fn fr4_a_frame_smaller_than_its_buffers_is_shown_at_the_window_size_not_stretched() {
        // A 607x1080 frame in 1920x1080 buffers is stretched to 1920 wide first; the visual
        // brings it back to the 607-pixel client area, so it shows 1:1.
        let (x, y) = visual_scale((1920, 1080), (607, 1080));
        assert!((1920.0 * x - 607.0).abs() < 0.01);
        assert!((y - 1.0).abs() < f32::EPSILON);
        assert_eq!(visual_scale((0, 0), (607, 1080)), (1.0, 1.0));
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
