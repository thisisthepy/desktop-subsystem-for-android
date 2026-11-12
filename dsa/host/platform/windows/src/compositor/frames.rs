//! Which window each display's frames are copied into.
//!
//! The renderer's presentation thread asks a [`FrameRouter`] where a display's new frame
//! goes; the UI thread tells it which window shows each display, from
//! `TaskWindows::routes`. A display without a window gets no texture, so its frames are
//! dropped before they are copied anywhere.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::adapter::DisplaySink;

/// A window that frames can be copied into.
pub trait FrameTarget: Send + Sync {
    /// A texture the renderer copies a frame into.
    type Texture: Send + Sync;

    /// A texture of exactly `width` x `height` pixels for the next frame, or `None` when
    /// the window cannot show one. The size is the guest buffer's, the largest a frame of
    /// the task can be, so it stays the same while the window is resized.
    fn texture(&self, width: u32, height: u32) -> Option<Arc<Self::Texture>>;

    /// The shared handle the renderer opens `texture` by.
    fn handle(texture: &Self::Texture) -> isize;

    /// Shows the frame that was copied into `texture`, which fills its top-left `content`
    /// pixels.
    fn present(&self, texture: &Self::Texture, content: (u32, u32));
}

type Given<T> = (Arc<T>, Arc<<T as FrameTarget>::Texture>);

/// Routes each display's frames to the window that shows it.
pub struct FrameRouter<T: FrameTarget> {
    routes: Mutex<HashMap<u32, Arc<T>>>,
    /// The window and texture the renderer was last given for each display. The texture is
    /// kept open while the renderer may still write it: until it is given something else for
    /// that display, or the display loses its window.
    given: Mutex<HashMap<u32, Given<T>>>,
    /// Told of each display that lost its window, so the renderer lets go of its copy of the
    /// display's texture at once rather than when a frame that never comes would replace it.
    #[allow(clippy::type_complexity)]
    forget: Mutex<Option<Box<dyn Fn(u32) + Send + Sync>>>,
    /// When the last frame arrived, in milliseconds after `start`: the idle memory trim
    /// gives the GPU driver's spare memory back while shown windows have nothing new to show.
    start: Instant,
    last_frame_ms: AtomicU64,
    /// How many frames were shown in a window: after a GPU reset, a guest that draws none
    /// has a compositor that did not recover.
    frames_shown: AtomicU64,
}

impl<T: FrameTarget> Default for FrameRouter<T> {
    fn default() -> Self {
        FrameRouter {
            routes: Mutex::new(HashMap::new()),
            given: Mutex::new(HashMap::new()),
            forget: Mutex::new(None),
            start: Instant::now(),
            last_frame_ms: AtomicU64::new(0),
            frames_shown: AtomicU64::new(0),
        }
    }
}

impl<T: FrameTarget> FrameRouter<T> {
    pub fn new() -> FrameRouter<T> {
        FrameRouter::default()
    }

    /// How long ago the last frame arrived (since the router was made, before any frame).
    pub fn quiet_for(&self) -> Duration {
        let last = Duration::from_millis(self.last_frame_ms.load(Ordering::Relaxed));
        self.start.elapsed().saturating_sub(last)
    }

    /// How many frames have been shown in a window so far.
    pub fn frames_shown(&self) -> u64 {
        self.frames_shown.load(Ordering::Relaxed)
    }

    /// Sets what is told of each display that lost its window.
    pub fn on_forget(&self, forget: Box<dyn Fn(u32) + Send + Sync>) {
        *self.forget.lock().unwrap_or_else(|e| e.into_inner()) = Some(forget);
    }

    /// Replaces the routes: from now on each display's frames go to its window, and the
    /// frames of every other display are dropped. A display that lost its window lets its
    /// texture go now: a closed task draws no more frames, so waiting for the renderer to
    /// move on would keep the texture (a whole display's pixels) for as long as the window
    /// manager runs.
    pub fn set_routes(&self, routes: impl IntoIterator<Item = (u32, Arc<T>)>) {
        let routes: HashMap<u32, Arc<T>> = routes.into_iter().collect();
        let routed: Vec<u32> = routes.keys().copied().collect();
        *self.routes.lock().unwrap_or_else(|e| e.into_inner()) = routes;
        let gone: Vec<u32> = {
            let mut given = self.given.lock().unwrap_or_else(|e| e.into_inner());
            let gone: Vec<u32> = given
                .keys()
                .copied()
                .filter(|display| !routed.contains(display))
                .collect();
            for display in &gone {
                given.remove(display);
            }
            gone
        };
        if gone.is_empty() {
            return;
        }
        if let Some(forget) = self
            .forget
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            for display in gone {
                forget(display);
            }
        }
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

    fn frame(&self, display_id: u32, content: (u32, u32)) {
        self.last_frame_ms
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
        let given = self
            .given
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&display_id)
            .cloned();
        if let Some((window, texture)) = given {
            window.present(&texture, content);
            self.frames_shown.fetch_add(1, Ordering::Relaxed);
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

    /// A frame a test window showed: the texture's handle, its size, and the frame's size.
    type Shown = (isize, (u32, u32), (u32, u32));

    /// A window that hands out a new texture for each frame size and records what it shows.
    struct Window {
        base: isize,
        current: Mutex<Option<Arc<Texture>>>,
        /// Each frame shown: the texture's handle, its size, and the frame's content size.
        shown: Mutex<Vec<Shown>>,
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
        fn present(&self, texture: &Texture, content: (u32, u32)) {
            self.shown
                .lock()
                .unwrap()
                .push((texture.handle, texture.size, content));
        }
    }

    #[test]
    fn fr3_a_frame_goes_only_to_the_window_of_its_display() {
        let router = FrameRouter::new();
        let clock = Window::new(1000);
        let maps = Window::new(2000);
        router.set_routes([(3, Arc::clone(&clock)), (4, Arc::clone(&maps))]);

        assert_eq!(router.target(3, 10, 20), Some(1010));
        router.frame(3, (10, 20));
        assert_eq!(router.target(4, 30, 40), Some(2030));
        router.frame(4, (30, 40));
        assert_eq!(
            *clock.shown.lock().unwrap(),
            vec![(1010, (10, 20), (10, 20))]
        );
        assert_eq!(
            *maps.shown.lock().unwrap(),
            vec![(2030, (30, 40), (30, 40))]
        );
    }

    #[test]
    fn nfr5_only_frames_shown_in_a_window_count_as_the_guest_drawing() {
        let router = FrameRouter::new();
        let clock = Window::new(1000);
        router.set_routes([(3, Arc::clone(&clock))]);
        assert_eq!(router.frames_shown(), 0);
        // A frame of a display without a window shows nowhere.
        router.frame(9, (10, 20));
        assert_eq!(router.frames_shown(), 0);
        assert_eq!(router.target(3, 10, 20), Some(1010));
        router.frame(3, (10, 20));
        router.frame(3, (10, 20));
        assert_eq!(router.frames_shown(), 2);
    }

    #[test]
    fn fr3_frames_of_a_display_without_a_window_are_dropped() {
        let router = FrameRouter::new();
        let clock = Window::new(1000);
        router.set_routes([(3, Arc::clone(&clock))]);
        assert_eq!(router.target(0, 1920, 1080), None);
        router.frame(0, (1920, 1080));
        assert_eq!(router.target(9, 10, 10), None);
        router.frame(9, (10, 10));
        assert!(clock.shown.lock().unwrap().is_empty());
        assert_eq!(router.routed_displays(), vec![3]);
    }

    #[test]
    fn nfr2_a_closed_windows_texture_goes_at_once_and_the_renderer_is_told() {
        let router = FrameRouter::new();
        let forgotten = Arc::new(Mutex::new(Vec::new()));
        let told = Arc::clone(&forgotten);
        router.on_forget(Box::new(move |display| told.lock().unwrap().push(display)));
        let clock = Window::new(1000);
        let maps = Window::new(2000);
        router.set_routes([(3, Arc::clone(&clock)), (4, Arc::clone(&maps))]);
        router.target(3, 10, 20);
        router.frame(3, (10, 20));
        router.target(4, 10, 20);
        // The clock's window closes: its own reference goes, and so does the router's, with no
        // frame for the display ever coming again.
        *clock.current.lock().unwrap() = None;
        router.set_routes([(4, Arc::clone(&maps))]);
        assert_eq!(clock.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(*forgotten.lock().unwrap(), vec![3]);
        // The window that stays keeps its texture, and nothing is told again.
        assert_eq!(maps.dropped.load(Ordering::Relaxed), 0);
        router.set_routes([(4, Arc::clone(&maps))]);
        assert_eq!(*forgotten.lock().unwrap(), vec![3]);
        // A late frame of the closed display reaches no window.
        assert_eq!(router.target(3, 10, 20), None);
        router.frame(3, (10, 20));
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
        router.frame(3, (10, 20));
        assert_eq!(clock.shown.lock().unwrap().len(), 1);
        assert!(maps.shown.lock().unwrap().is_empty());
    }

    #[test]
    fn fr4_frames_of_a_resizing_task_share_one_texture_and_carry_their_size() {
        let router = FrameRouter::new();
        let clock = Window::new(1000);
        router.set_routes([(3, Arc::clone(&clock))]);
        // The guest's buffer stays at its display's size; only the content changes.
        for content in [(800, 600), (812, 606), (830, 615)] {
            assert_eq!(router.target(3, 1920, 1080), Some(2920));
            router.frame(3, content);
        }
        assert_eq!(
            *clock.shown.lock().unwrap(),
            vec![
                (2920, (1920, 1080), (800, 600)),
                (2920, (1920, 1080), (812, 606)),
                (2920, (1920, 1080), (830, 615)),
            ]
        );
        assert_eq!(clock.dropped.load(Ordering::Relaxed), 0);
    }
}
