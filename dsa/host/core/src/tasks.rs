//! The guest's app tasks as the host knows them.
//!
//! The agent in the guest reports each app task the host shows as a window (`TaskUpdated`,
//! `TaskRemoved`), and the hwcomposer reports where each task's frames arrive on the
//! graphics stream (`TaskFrames`). The session keeps the latest of these in a [`TaskTable`]
//! and hands every change on as a [`TaskEvent`], so a compositor that connects late starts
//! from the tasks that exist now rather than from nothing.

use std::collections::BTreeMap;

/// A rectangle in guest display pixels: left and top inclusive, right and bottom exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Bounds {
    /// The width in pixels; zero for a rectangle whose right edge is not past its left.
    pub fn width(&self) -> u32 {
        (i64::from(self.right) - i64::from(self.left)).clamp(0, i64::from(u32::MAX)) as u32
    }

    /// The height in pixels; zero for a rectangle whose bottom edge is not below its top.
    pub fn height(&self) -> u32 {
        (i64::from(self.bottom) - i64::from(self.top)).clamp(0, i64::from(u32::MAX)) as u32
    }
}

/// One app task, as the agent last described it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Task {
    /// The guest's task ID, unique while the task lives.
    pub task_id: i32,
    /// Where the task's window is on the guest's freeform display.
    pub bounds: Bounds,
    /// The package of the task's root activity.
    pub package_name: String,
    /// The title the task's window shows.
    pub label: String,
    /// Whether the task has input focus in the guest.
    pub focused: bool,
}

/// A change to the guest's tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskEvent {
    /// A task appeared, or something its window shows changed. It carries the whole task.
    Updated(Task),
    /// The task ended.
    Removed { task_id: i32 },
    /// The task's frames now arrive on the graphics stream as posts to `display_id`.
    Frames { task_id: i32, display_id: u32 },
}

impl TaskEvent {
    /// The task the event is about.
    pub fn task_id(&self) -> i32 {
        match self {
            TaskEvent::Updated(task) => task.task_id,
            TaskEvent::Removed { task_id } | TaskEvent::Frames { task_id, .. } => *task_id,
        }
    }
}

/// What the host asks of the guest about its tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRequest {
    /// The user closed the task's window; the agent ends the task.
    Close { task_id: i32 },
    /// Start the app's launcher activity as a new task in its own window.
    Launch { package_name: String },
}

/// The most tasks the table keeps. A guest that reports more is broken or hostile; the
/// tasks past this are dropped rather than allowed to grow the host's memory without end.
pub const MAX_TASKS: usize = 1024;

/// The latest state of each task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskTable {
    tasks: BTreeMap<i32, Task>,
    frames: BTreeMap<i32, u32>,
}

impl TaskTable {
    pub fn new() -> TaskTable {
        TaskTable::default()
    }

    /// Records `event`. Returns false when the event changes nothing, or when it would take
    /// the table past [`MAX_TASKS`] and was dropped.
    pub fn apply(&mut self, event: &TaskEvent) -> bool {
        match event {
            TaskEvent::Updated(task) => {
                if self.tasks.get(&task.task_id) == Some(task) {
                    return false;
                }
                if !self.knows(task.task_id) && self.len() >= MAX_TASKS {
                    return false;
                }
                self.tasks.insert(task.task_id, task.clone());
                true
            }
            TaskEvent::Removed { task_id } => {
                let had_task = self.tasks.remove(task_id).is_some();
                let had_frames = self.frames.remove(task_id).is_some();
                had_task || had_frames
            }
            TaskEvent::Frames {
                task_id,
                display_id,
            } => {
                if self.frames.get(task_id) == Some(display_id) {
                    return false;
                }
                if !self.knows(*task_id) && self.len() >= MAX_TASKS {
                    return false;
                }
                self.frames.insert(*task_id, *display_id);
                true
            }
        }
    }

    /// Whether the table has heard of `task_id`, from the agent or from the hwcomposer.
    pub fn knows(&self, task_id: i32) -> bool {
        self.tasks.contains_key(&task_id) || self.frames.contains_key(&task_id)
    }

    /// How many tasks the table has heard of.
    pub fn len(&self) -> usize {
        self.tasks.len()
            + self
                .frames
                .keys()
                .filter(|task_id| !self.tasks.contains_key(task_id))
                .count()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty() && self.frames.is_empty()
    }

    /// The tasks the agent has described, in task ID order.
    pub fn tasks(&self) -> Vec<Task> {
        self.tasks.values().cloned().collect()
    }

    /// Each task's display, in task ID order.
    pub fn frames(&self) -> Vec<(i32, u32)> {
        self.frames
            .iter()
            .map(|(task_id, display_id)| (*task_id, *display_id))
            .collect()
    }

    /// The events that bring an empty table to this one: every task, then every pairing of
    /// a task with its display.
    pub fn snapshot(&self) -> Vec<TaskEvent> {
        let tasks = self.tasks.values().cloned().map(TaskEvent::Updated);
        let frames = self
            .frames
            .iter()
            .map(|(task_id, display_id)| TaskEvent::Frames {
                task_id: *task_id,
                display_id: *display_id,
            });
        tasks.chain(frames).collect()
    }

    /// Forgets every task, and returns the removals that say so.
    pub fn clear(&mut self) -> Vec<TaskEvent> {
        let mut ids: Vec<i32> = self
            .tasks
            .keys()
            .chain(self.frames.keys())
            .copied()
            .collect();
        ids.sort_unstable();
        ids.dedup();
        self.tasks.clear();
        self.frames.clear();
        ids.into_iter()
            .map(|task_id| TaskEvent::Removed { task_id })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(task_id: i32, label: &str) -> Task {
        Task {
            task_id,
            bounds: Bounds {
                left: 0,
                top: 0,
                right: 400,
                bottom: 300,
            },
            package_name: "com.example".into(),
            label: label.into(),
            focused: false,
        }
    }

    #[test]
    fn fr3_the_table_keeps_only_the_latest_state_of_each_task() {
        let mut table = TaskTable::new();
        assert!(table.apply(&TaskEvent::Updated(task(7, "Clock"))));
        assert!(table.apply(&TaskEvent::Updated(task(7, "Alarm"))));
        assert!(!table.apply(&TaskEvent::Updated(task(7, "Alarm"))));
        assert!(table.apply(&TaskEvent::Frames {
            task_id: 7,
            display_id: 3
        }));
        assert_eq!(
            table.snapshot(),
            vec![
                TaskEvent::Updated(task(7, "Alarm")),
                TaskEvent::Frames {
                    task_id: 7,
                    display_id: 3
                }
            ]
        );
        assert!(table.apply(&TaskEvent::Removed { task_id: 7 }));
        assert!(table.is_empty());
        assert!(!table.apply(&TaskEvent::Removed { task_id: 7 }));
    }

    #[test]
    fn fr3_frames_may_arrive_before_the_task_they_belong_to() {
        let mut table = TaskTable::new();
        table.apply(&TaskEvent::Frames {
            task_id: 9,
            display_id: 4,
        });
        assert!(table.knows(9));
        assert_eq!(table.len(), 1);
        table.apply(&TaskEvent::Updated(task(9, "Maps")));
        assert_eq!(table.len(), 1);
        assert_eq!(table.frames(), vec![(9, 4)]);
    }

    #[test]
    fn fr3_clearing_the_table_removes_every_task_it_knew() {
        let mut table = TaskTable::new();
        table.apply(&TaskEvent::Updated(task(2, "a")));
        table.apply(&TaskEvent::Frames {
            task_id: 2,
            display_id: 1,
        });
        table.apply(&TaskEvent::Frames {
            task_id: 5,
            display_id: 2,
        });
        assert_eq!(
            table.clear(),
            vec![
                TaskEvent::Removed { task_id: 2 },
                TaskEvent::Removed { task_id: 5 }
            ]
        );
        assert!(table.is_empty());
    }

    #[test]
    fn nfr5_a_guest_cannot_grow_the_table_without_end() {
        let mut table = TaskTable::new();
        for task_id in 0..MAX_TASKS as i32 {
            assert!(table.apply(&TaskEvent::Updated(task(task_id, "t"))));
        }
        assert!(!table.apply(&TaskEvent::Updated(task(-1, "one too many"))));
        assert!(!table.apply(&TaskEvent::Frames {
            task_id: -1,
            display_id: 1
        }));
        // A task it already knows still changes.
        assert!(table.apply(&TaskEvent::Updated(task(0, "renamed"))));
        assert_eq!(table.len(), MAX_TASKS);
    }

    #[test]
    fn fr3_bounds_never_give_a_negative_size() {
        let bounds = Bounds {
            left: 10,
            top: 10,
            right: 5,
            bottom: 30,
        };
        assert_eq!(bounds.width(), 0);
        assert_eq!(bounds.height(), 20);
    }
}
