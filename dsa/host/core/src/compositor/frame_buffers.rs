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

#[cfg(test)]
mod tests {
    use super::*;

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
