//! Which Android task each of a window host's windows shows.
//!
//! A window host knows each window by the ID of the task it opened for, and so does every
//! message between it and the window manager. After a GPU reset an app's task can end and the
//! app start again; its new task then takes the old window over (see [`super::gpu_reset`]),
//! and the window manager says so with [`ToHost::ShownTask`]. What the window sends Android
//! itself, past the window manager, must name the task it shows now, or it reaches a task that
//! has ended: its pointer, touch, pen, wheel, key and text input (the caption's back button
//! among them), its accessibility watch and actions, and a drag out of it. What it reports to
//! the window manager (a close, a resize, a maximise) keeps the window's ID, which the window
//! manager turns into the task's.
//!
//! [`WindowTasks`] keeps that for a window host, shared by its threads.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::window_hosts::ToHost;
use crate::drag::DragOut;
use crate::input::InputEvent;

/// The task each window shows, by window ID, where it is not the task the window opened for.
/// Cloning it shares it.
#[derive(Debug, Clone, Default)]
pub struct WindowTasks {
    shown: Arc<Mutex<BTreeMap<i32, i32>>>,
}

impl WindowTasks {
    pub fn new() -> WindowTasks {
        WindowTasks::default()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<i32, i32>> {
        self.shown.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes a message from the window manager: a window that shows another task now, or one
    /// that closes, whose ID a later window may have again.
    pub fn follow(&self, message: &ToHost) {
        match message {
            ToHost::ShownTask {
                task_id,
                shown_task_id,
            } => {
                let mut shown = self.lock();
                if task_id == shown_task_id {
                    shown.remove(task_id);
                } else {
                    shown.insert(*task_id, *shown_task_id);
                }
            }
            ToHost::Close { task_id } => {
                self.lock().remove(task_id);
            }
            _ => {}
        }
    }

    /// The task window `window` shows.
    pub fn task_of(&self, window: i32) -> i32 {
        self.lock().get(&window).copied().unwrap_or(window)
    }

    /// `event`, made in window `event.task_id()`, for the task that window shows.
    pub fn input(&self, event: InputEvent) -> InputEvent {
        let window = event.task_id();
        let task_id = self.task_of(window);
        if task_id == window {
            event
        } else {
            event.for_task(task_id)
        }
    }

    /// `out`, a drag out of window `out.task_id`, for the task that window shows.
    pub fn drag_out(&self, out: DragOut) -> DragOut {
        DragOut {
            task_id: self.task_of(out.task_id),
            ..out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ime::ImeEvent;
    use crate::input::{PointerAction, PointerEvent, PointerTool};
    use crate::keys::{KeyAction, KeyEvent};

    fn pointer(task_id: i32) -> InputEvent {
        InputEvent::Pointer(PointerEvent::new(
            task_id,
            PointerAction::Down,
            PointerTool::Touch,
            (12.0, 34.0),
        ))
    }

    fn key(task_id: i32) -> InputEvent {
        InputEvent::Key(KeyEvent {
            task_id,
            action: KeyAction::Down,
            key_code: 4,
            repeat_count: 0,
            meta_state: 0,
            event_time_ms: 5,
            canceled: false,
        })
    }

    #[test]
    fn nfr5_a_window_taken_over_after_a_gpu_reset_sends_its_input_to_the_new_task() {
        let tasks = WindowTasks::new();
        tasks.follow(&ToHost::ShownTask {
            task_id: 9,
            shown_task_id: 14,
        });
        assert_eq!(tasks.task_of(9), 14);
        assert_eq!(tasks.input(pointer(9)).task_id(), 14);
        assert_eq!(tasks.input(key(9)).task_id(), 14);
        let commit = InputEvent::Ime(ImeEvent::Commit {
            task_id: 9,
            text: "\u{d55c}".into(),
        });
        assert_eq!(tasks.input(commit).task_id(), 14);
        let out = tasks.drag_out(DragOut {
            task_id: 9,
            x: 1.0,
            y: 2.0,
            tool: 1,
            pointer_id: 0,
        });
        assert_eq!((out.task_id, out.x, out.y), (14, 1.0, 2.0));
        // Another window of the app still sends to its own task.
        assert_eq!(tasks.input(pointer(10)).task_id(), 10);
    }

    #[test]
    fn nfr5_a_window_taken_over_again_sends_to_the_latest_task() {
        let tasks = WindowTasks::new();
        let clone = tasks.clone();
        tasks.follow(&ToHost::ShownTask {
            task_id: 9,
            shown_task_id: 14,
        });
        tasks.follow(&ToHost::ShownTask {
            task_id: 9,
            shown_task_id: 20,
        });
        // The window host's threads share one map.
        assert_eq!(clone.task_of(9), 20);
    }

    #[test]
    fn nfr5_a_closed_window_forgets_the_task_it_showed() {
        let tasks = WindowTasks::new();
        tasks.follow(&ToHost::ShownTask {
            task_id: 9,
            shown_task_id: 14,
        });
        tasks.follow(&ToHost::Close { task_id: 9 });
        // A later window that opens for task 9 is task 9's.
        assert_eq!(tasks.task_of(9), 9);
        assert_eq!(tasks.input(key(9)).task_id(), 9);
    }
}
