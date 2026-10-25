//! Which window each display's frames are copied into.
//!
//! The renderer's presentation thread asks a [`FrameRouter`] where a display's new frame
//! goes; the UI thread tells it which window shows each display, from
//! `TaskWindows::routes`. A display without a window gets no texture, so its frames are
//! dropped before they are copied anywhere.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::adapter::DisplaySink;

/// A window that frames can be copied into.
pub trait FrameTarget: Send + Sync {
    /// A texture the renderer copies a frame into.
    type Texture: Send + Sync;

    /// A texture of exactly `width` x `height` pixels for the next frame, or `None` when
    /// the window cannot show one.
    fn texture(&self, width: u32, height: u32) -> Option<Arc<Self::Texture>>;

    /// The shared handle the renderer opens `texture` by.
    fn handle(texture: &Self::Texture) -> isize;

    /// Shows the frame that was copied into `texture`.
    fn present(&self, texture: &Self::Texture);
}

type Given<T> = (Arc<T>, Arc<<T as FrameTarget>::Texture>);

/// Routes each display's frames to the window that shows it.
pub struct FrameRouter<T: FrameTarget> {
    routes: Mutex<HashMap<u32, Arc<T>>>,
    /// The window and texture the renderer was last given for each display. The texture is
    /// kept open while the renderer may still write it: until it is given something else for
    /// that display.
    given: Mutex<HashMap<u32, Given<T>>>,
}

impl<T: FrameTarget> Default for FrameRouter<T> {
    fn default() -> Self {
        FrameRouter {
            routes: Mutex::new(HashMap::new()),
            given: Mutex::new(HashMap::new()),
        }
    }
}

impl<T: FrameTarget> FrameRouter<T> {
    pub fn new() -> FrameRouter<T> {
        FrameRouter::default()
    }

    /// Replaces the routes: from now on each display's frames go to its window, and the
    /// frames of every other display are dropped.
    pub fn set_routes(&self, routes: impl IntoIterator<Item = (u32, Arc<T>)>) {
        *self.routes.lock().unwrap_or_else(|e| e.into_inner()) = routes.into_iter().collect();
    }

    /// The displays that have a window, in order.
    pub fn routed_displays(&self) -> Vec<u32> {
        let mut displays: Vec<u32> = self
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect();
        displays.sort_unstable();
        displays
    }
}

impl<T: FrameTarget + 'static> DisplaySink for FrameRouter<T> {
    fn target(&self, display_id: u32, width: u32, height: u32) -> Option<isize> {
        let window = self
            .routes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&display_id)
            .cloned();
        let given = window.and_then(|window| {
            let texture = window.texture(width, height)?;
            Some((window, texture))
        });
        let handle = given.as_ref().map(|(_, texture)| T::handle(texture));
        let mut all = self.given.lock().unwrap_or_else(|e| e.into_inner());
        match given {
            Some(given) => {
                all.insert(display_id, given);
            }
            None => {
                all.remove(&display_id);
            }
        }
        handle
    }

    fn frame(&self, display_id: u32) {
        let given = self
            .given
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&display_id)
            .cloned();
        if let Some((window, texture)) = given {
            window.present(&texture);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Texture {
        handle: isize,
        size: (u32, u32),
        dropped: Arc<AtomicUsize>,
    }

    impl Drop for Texture {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A window that hands out a new texture for each frame size and records what it shows.
    struct Window {
        base: isize,
        current: Mutex<Option<Arc<Texture>>>,
        shown: Mutex<Vec<(isize, (u32, u32))>>,
        dropped: Arc<AtomicUsize>,
    }

    impl Window {
        fn new(base: isize) -> Arc<Window> {
            Arc::new(Window {
                base,
                current: Mutex::new(None),
                shown: Mutex::new(Vec::new()),
                dropped: Arc::new(AtomicUsize::new(0)),
            })
        }
    }

    impl FrameTarget for Window {
        type Texture = Texture;
        fn texture(&self, width: u32, height: u32) -> Option<Arc<Texture>> {
            let mut current = self.current.lock().unwrap();
            if current.as_ref().map(|t| t.size) != Some((width, height)) {
                *current = Some(Arc::new(Texture {
                    handle: self.base + width as isize,
                    size: (width, height),
                    dropped: Arc::clone(&self.dropped),
                }));
            }
            current.clone()
        }
        fn handle(texture: &Texture) -> isize {
            texture.handle
        }
        fn present(&self, texture: &Texture) {
            self.shown
                .lock()
                .unwrap()
                .push((texture.handle, texture.size));
        }
    }

    #[test]
    fn fr3_a_frame_goes_only_to_the_window_of_its_display() {
        let router = FrameRouter::new();
        let clock = Window::new(1000);
        let maps = Window::new(2000);
        router.set_routes([(3, Arc::clone(&clock)), (4, Arc::clone(&maps))]);

        assert_eq!(router.target(3, 10, 20), Some(1010));
        router.frame(3);
        assert_eq!(router.target(4, 30, 40), Some(2030));
        router.frame(4);
        assert_eq!(*clock.shown.lock().unwrap(), vec![(1010, (10, 20))]);
        assert_eq!(*maps.shown.lock().unwrap(), vec![(2030, (30, 40))]);
    }

    #[test]
    fn fr3_frames_of_a_display_without_a_window_are_dropped() {
        let router = FrameRouter::new();
        let clock = Window::new(1000);
        router.set_routes([(3, Arc::clone(&clock))]);
        assert_eq!(router.target(0, 1920, 1080), None);
        router.frame(0);
        assert_eq!(router.target(9, 10, 10), None);
        router.frame(9);
        assert!(clock.shown.lock().unwrap().is_empty());
        assert_eq!(router.routed_displays(), vec![3]);
    }

    #[test]
    fn fr3_a_closed_windows_texture_stays_open_until_the_renderer_moves_on() {
        let router = FrameRouter::new();
        let clock = Window::new(1000);
        router.set_routes([(3, Arc::clone(&clock))]);
        router.target(3, 10, 20);
        router.frame(3);
        // The window closes: its own reference goes, the renderer may still hold the texture.
        *clock.current.lock().unwrap() = None;
        router.set_routes([]);
        assert_eq!(clock.dropped.load(Ordering::Relaxed), 0);
        // The renderer's next frame for the display gets nothing, and the texture goes.
        assert_eq!(router.target(3, 10, 20), None);
        assert_eq!(clock.dropped.load(Ordering::Relaxed), 1);
        router.frame(3);
        assert_eq!(clock.shown.lock().unwrap().len(), 1);
    }

    #[test]
    fn fr3_a_frame_is_shown_in_the_window_it_was_copied_for() {
        let router = FrameRouter::new();
        let clock = Window::new(1000);
        let maps = Window::new(2000);
        router.set_routes([(3, Arc::clone(&clock))]);
        router.target(3, 10, 20);
        // The display moves to another task between the copy and the frame.
        router.set_routes([(3, Arc::clone(&maps))]);
        router.frame(3);
        assert_eq!(clock.shown.lock().unwrap().len(), 1);
        assert!(maps.shown.lock().unwrap().is_empty());
    }
}
