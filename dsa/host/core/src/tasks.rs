//! The guest's app tasks as the host knows them.
//!
//! The agent in the guest reports each app task the host shows as a window (`TaskUpdated`,
//! `TaskRemoved`), and the hwcomposer reports where each task's frames arrive on the
//! graphics stream (`TaskFrames`). The session keeps the latest of these in a [`TaskTable`]
//! and hands every change on as a [`TaskEvent`], so a compositor that connects late starts
//! from the tasks that exist now rather than from nothing.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::Receiver;

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
    /// The density the task's app draws at, in dots per inch (160 at a scale of 100%); 0
    /// when the agent could not read it.
    pub density_dpi: u32,
    /// The largest size the guest can give the task, in its display pixels; 0 when unknown.
    pub max_width: u32,
    pub max_height: u32,
}

impl Task {
    /// The largest size the guest can give the task, when the agent said.
    pub fn max_size(&self) -> Option<(u32, u32)> {
        (self.max_width > 0 && self.max_height > 0).then_some((self.max_width, self.max_height))
    }
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

/// The guest's tasks as they are now, and every change to them from then on.
pub struct TaskWatch {
    /// The events that bring an empty list of tasks to the current one.
    pub current: Vec<TaskEvent>,
    /// The changes after `current`, in order.
    pub events: Receiver<TaskEvent>,
}

/// What the host asks of the guest about its tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRequest {
    /// The user closed the task's window; the agent ends the task.
    Close { task_id: i32 },
    /// Start the app's launcher activity as a new task in its own window.
    Launch { package_name: String },
    /// The user resized the task's window: the task takes its client size, in guest
    /// display pixels.
    Resize {
        task_id: i32,
        width: u32,
        height: u32,
    },
    /// The task's window is on a monitor of another scale: the task draws at this density.
    SetDensity { task_id: i32, density_dpi: u32 },
}

impl TaskRequest {
    /// What the request asks for, in words for a log line.
    pub fn describe(&self) -> String {
        match self {
            TaskRequest::Close { task_id } => format!("end task {task_id}"),
            TaskRequest::Launch { package_name } => format!("launch {package_name}"),
            TaskRequest::Resize {
                task_id,
                width,
                height,
            } => format!("resize task {task_id} to {width}x{height}"),
            TaskRequest::SetDensity {
                task_id,
                density_dpi,
            } => format!("draw task {task_id} at {density_dpi} dpi"),
        }
    }
}

/// Folds a run of requests waiting to be sent into the ones still worth sending.
///
/// Only a task's latest size and latest density count: while the user drags a window's
/// edge, the sizes it passed through are dropped rather than queued, so the guest never
/// works through a backlog. The latest of each keeps its place, so a density followed by
/// the size that goes with it still arrives in that order. Closes and launches are kept, in
/// order.
pub fn coalesce(requests: Vec<TaskRequest>) -> Vec<TaskRequest> {
    let superseded = |index: usize, request: &TaskRequest| {
        requests[index + 1..]
            .iter()
            .any(|later| match (request, later) {
                (
                    TaskRequest::Resize { task_id: a, .. },
                    TaskRequest::Resize { task_id: b, .. },
                ) => a == b,
                (
                    TaskRequest::SetDensity { task_id: a, .. },
                    TaskRequest::SetDensity { task_id: b, .. },
                ) => a == b,
                _ => false,
            })
    };
    requests
        .iter()
        .enumerate()
        .filter(|(index, request)| !superseded(*index, request))
        .map(|(_, request)| request.clone())
        .collect()
}

/// Why a [`TaskRequest`] was not carried out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRequestError {
    /// The Android is not running, so there is nothing to act on; the detail says so in
    /// words a person can act on.
    NotRunning(String),
    /// The request could not be delivered; the detail says why.
    Failed(String),
}

impl fmt::Display for TaskRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskRequestError::NotRunning(why) | TaskRequestError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for TaskRequestError {}

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
            ..Task::default()
        }
    }

    fn resize(task_id: i32, width: u32) -> TaskRequest {
        TaskRequest::Resize {
            task_id,
            width,
            height: 600,
        }
    }

    #[test]
    fn fr4_only_the_latest_size_of_each_task_is_sent() {
        let requests = vec![
            resize(1, 500),
            resize(2, 300),
            resize(1, 520),
            resize(1, 540),
        ];
        assert_eq!(coalesce(requests), vec![resize(2, 300), resize(1, 540)]);
    }

    #[test]
    fn fr4_a_density_and_its_size_keep_their_order() {
        let density = TaskRequest::SetDensity {
            task_id: 1,
            density_dpi: 320,
        };
        let requests = vec![
            resize(1, 500),
            TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 240,
            },
            density.clone(),
            resize(1, 1000),
        ];
        assert_eq!(coalesce(requests), vec![density, resize(1, 1000)]);
    }

    #[test]
    fn fr4_closes_and_launches_are_never_dropped() {
        let launch = TaskRequest::Launch {
            package_name: "com.example".into(),
        };
        let close = TaskRequest::Close { task_id: 1 };
        let requests = vec![close.clone(), launch.clone(), close.clone(), resize(1, 500)];
        assert_eq!(
            coalesce(requests),
            vec![close.clone(), launch, close, resize(1, 500)]
        );
    }

    #[test]
    fn fr4_a_task_reports_its_size_limit_only_when_it_knows_it() {
        let mut known = task(1, "a");
        assert_eq!(known.max_size(), None);
        known.max_width = 1920;
        assert_eq!(known.max_size(), None);
        known.max_height = 1080;
        assert_eq!(known.max_size(), Some((1920, 1080)));
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
