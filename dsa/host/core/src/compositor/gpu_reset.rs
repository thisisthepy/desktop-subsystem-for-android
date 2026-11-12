//! Keeping an app's window through a GPU reset.
//!
//! A GPU reset (Windows' timeout detection and recovery, a driver update, a GPU that is
//! taken away) loses every graphics device on the GPU. The host's own devices are made
//! again, but an app in the guest may not survive it: its renderer thread can crash on the
//! lost device, and Android then ends the app's task. Its window would close with it, and with
//! the app's last window its window program, although the user did nothing.
//!
//! So a task that ends within [`RESET_GRACE`] of a reset keeps its window: the window is not
//! closed, the app is started again, and the first new task of the same app takes the kept
//! window over. The window keeps its place, its size and its window program, and shows the
//! app again as soon as the new task draws. A window whose app does not come back within
//! [`RELAUNCH_WAIT`] closes as it would have.
//!
//! Inside, the window keeps the ID of the task it opened for (its window ID), which the window
//! hosts know it by; the guest knows only the new task. [`GpuResetWindows`] turns one into the
//! other: [`WindowChange`]s from the task rules go to the window hosts with window IDs, and
//! what a window host reports comes back with the task's.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::task_windows::WindowChange;
use super::window_hosts::FromHost;

/// A task that ends this soon after a GPU reset is taken to have ended because of it.
pub const RESET_GRACE: Duration = Duration::from_secs(60);

/// How long a kept window waits for its app's new task.
pub const RELAUNCH_WAIT: Duration = Duration::from_secs(30);

/// A window whose task ended after a GPU reset, waiting for its app's next task.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Kept {
    package: String,
    since: Instant,
}

/// What the task rules' changes after a guest event come to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GuestOutcome {
    /// The changes for the window hosts, by window ID.
    pub changes: Vec<WindowChange>,
    /// The apps to start again, for a window each kept.
    pub relaunch: Vec<String>,
    /// The new tasks that took a kept window over, and the window's client size when it is
    /// known: the task is asked for that size, so it fits the window it took.
    pub adopted: Vec<(i32, Option<(u32, u32)>)>,
    /// The windows kept for their app's next task.
    pub kept: Vec<i32>,
}

/// Where a window host's report goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// To the task rules, with the task's ID.
    Task(FromHost),
    /// The user closed a kept window: it closes now (this change goes to the window hosts).
    CloseKept(WindowChange),
    /// About a kept window, which has no task to tell.
    Dropped,
}

/// The windows kept through a GPU reset, and which task each window shows now.
#[derive(Debug, Default)]
pub struct GpuResetWindows {
    reset_at: Option<Instant>,
    /// The package of each window, by window ID.
    packages: BTreeMap<i32, String>,
    /// Each window's last client size, by window ID.
    sizes: BTreeMap<i32, (u32, u32)>,
    /// The windows waiting for a new task, by window ID.
    kept: BTreeMap<i32, Kept>,
    /// The window each task that took a kept window over shows, by task ID.
    windows: BTreeMap<i32, i32>,
}

impl GpuResetWindows {
    pub fn new() -> GpuResetWindows {
        GpuResetWindows::default()
    }

    /// The GPU was reset at `now`.
    pub fn gpu_reset(&mut self, now: Instant) {
        self.reset_at = Some(now);
    }

    fn reset_recently(&self, now: Instant) -> bool {
        self.reset_at
            .is_some_and(|at| now.saturating_duration_since(at) <= RESET_GRACE)
    }

    /// The window a task shows: its own, unless it took a kept one over.
    pub fn window_of(&self, task_id: i32) -> i32 {
        self.windows.get(&task_id).copied().unwrap_or(task_id)
    }

    /// The task a window shows, or `None` for a kept window that waits for one.
    pub fn task_of(&self, window: i32) -> Option<i32> {
        if self.kept.contains_key(&window) {
            return None;
        }
        Some(
            self.windows
                .iter()
                .find(|(_, shown_in)| **shown_in == window)
                .map(|(task, _)| *task)
                .unwrap_or(window),
        )
    }

    /// Whether a window waits for its app's next task.
    pub fn is_kept(&self, window: i32) -> bool {
        self.kept.contains_key(&window)
    }

    /// The apps whose kept windows wait for their next task, each once.
    pub fn kept_packages(&self) -> Vec<String> {
        let mut packages: Vec<String> = Vec::new();
        for kept in self.kept.values() {
            if !packages.contains(&kept.package) {
                packages.push(kept.package.clone());
            }
        }
        packages
    }

    /// The task rules' changes that did not come from the guest ending a task (the user closed
    /// a window, a window's size settled): they go to the window hosts as they are, with
    /// window IDs.
    pub fn changes(&mut self, changes: Vec<WindowChange>) -> Vec<WindowChange> {
        let mut out = Vec::with_capacity(changes.len());
        for change in changes {
            let change = self.to_window(change);
            match &change {
                WindowChange::Open {
                    task_id,
                    package_name,
                    ..
                } => {
                    self.packages.insert(*task_id, package_name.clone());
                }
                WindowChange::Close { task_id } => self.forget_window(*task_id),
                _ => {}
            }
            out.push(change);
        }
        out
    }

    /// The task rules' changes after the guest's tasks changed at `now`. A window whose task
    /// ended soon after a GPU reset is kept instead of closed and its app started again; a new
    /// task of an app with a kept window takes that window over.
    pub fn guest_changes(&mut self, changes: Vec<WindowChange>, now: Instant) -> GuestOutcome {
        let mut outcome = GuestOutcome::default();
        for change in changes {
            match change {
                WindowChange::Close { task_id } => {
                    let window = self.window_of(task_id);
                    self.windows.remove(&task_id);
                    match self.packages.get(&window).cloned() {
                        Some(package) if self.reset_recently(now) => {
                            self.kept.insert(
                                window,
                                Kept {
                                    package: package.clone(),
                                    since: now,
                                },
                            );
                            outcome.kept.push(window);
                            if !outcome.relaunch.contains(&package) {
                                outcome.relaunch.push(package);
                            }
                        }
                        _ => {
                            self.forget_window(window);
                            outcome
                                .changes
                                .push(WindowChange::Close { task_id: window });
                        }
                    }
                }
                WindowChange::Open {
                    task_id,
                    package_name,
                    title,
                    width,
                    height,
                    focused,
                } => {
                    let kept = self
                        .kept
                        .iter()
                        .find(|(_, kept)| kept.package == package_name)
                        .map(|(window, _)| *window);
                    match kept {
                        Some(window) => {
                            self.kept.remove(&window);
                            self.windows.insert(task_id, window);
                            outcome
                                .adopted
                                .push((task_id, self.sizes.get(&window).copied()));
                            // The window is there already; only its title may be new.
                            outcome.changes.push(WindowChange::Retitle {
                                task_id: window,
                                title,
                            });
                        }
                        None => {
                            self.packages.insert(task_id, package_name.clone());
                            outcome.changes.push(WindowChange::Open {
                                task_id,
                                package_name,
                                title,
                                width,
                                height,
                                focused,
                            });
                        }
                    }
                }
                other => outcome.changes.push(self.to_window(other)),
            }
        }
        outcome
    }

    /// A window host reported `report` on one of its windows (by window ID).
    pub fn report(&mut self, report: FromHost) -> Report {
        let window = report.task_id();
        if let FromHost::Resized { width, height, .. } = &report {
            self.sizes.insert(window, (*width, *height));
        }
        match self.task_of(window) {
            Some(task_id) => Report::Task(with_task_id(report, task_id)),
            None => match report {
                FromHost::Closed { .. } => {
                    self.forget_window(window);
                    Report::CloseKept(WindowChange::Close { task_id: window })
                }
                _ => Report::Dropped,
            },
        }
    }

    /// The windows whose app did not come back in time: they close now.
    pub fn tick(&mut self, now: Instant) -> Vec<WindowChange> {
        let late: Vec<i32> = self
            .kept
            .iter()
            .filter(|(_, kept)| now.saturating_duration_since(kept.since) >= RELAUNCH_WAIT)
            .map(|(window, _)| *window)
            .collect();
        late.into_iter()
            .map(|window| {
                self.forget_window(window);
                WindowChange::Close { task_id: window }
            })
            .collect()
    }

    /// Every task is gone (the guest cannot be reached): the kept windows close too.
    pub fn close_all(&mut self) -> Vec<WindowChange> {
        let kept: Vec<i32> = self.kept.keys().copied().collect();
        let closes = kept
            .into_iter()
            .map(|window| WindowChange::Close { task_id: window })
            .collect();
        *self = GpuResetWindows {
            reset_at: self.reset_at,
            ..GpuResetWindows::default()
        };
        closes
    }

    /// A window went with its window program. Returns the task it showed, which the task rules
    /// then close; `None` for a kept window, which has none.
    pub fn window_gone(&mut self, window: i32) -> Option<i32> {
        let task = self.task_of(window);
        if let Some(task) = task {
            self.windows.remove(&task);
        }
        self.forget_window(window);
        task
    }

    fn forget_window(&mut self, window: i32) {
        self.kept.remove(&window);
        self.packages.remove(&window);
        self.sizes.remove(&window);
        self.windows.retain(|_, shown_in| *shown_in != window);
    }

    fn to_window(&self, change: WindowChange) -> WindowChange {
        let window = |task_id: i32| self.window_of(task_id);
        match change {
            WindowChange::Open {
                task_id,
                package_name,
                title,
                width,
                height,
                focused,
            } => WindowChange::Open {
                task_id: window(task_id),
                package_name,
                title,
                width,
                height,
                focused,
            },
            WindowChange::Retitle { task_id, title } => WindowChange::Retitle {
                task_id: window(task_id),
                title,
            },
            WindowChange::Resize {
                task_id,
                width,
                height,
            } => WindowChange::Resize {
                task_id: window(task_id),
                width,
                height,
            },
            WindowChange::Close { task_id } => WindowChange::Close {
                task_id: window(task_id),
            },
            WindowChange::TaskSize {
                task_id,
                width,
                height,
                max,
                min,
                scale,
            } => WindowChange::TaskSize {
                task_id: window(task_id),
                width,
                height,
                max,
                min,
                scale,
            },
            WindowChange::ImeCursor { task_id, cursor } => WindowChange::ImeCursor {
                task_id: window(task_id),
                cursor,
            },
            WindowChange::ImeEditor { task_id, editor } => WindowChange::ImeEditor {
                task_id: window(task_id),
                editor,
            },
            WindowChange::CaptionTone { task_id, tone } => WindowChange::CaptionTone {
                task_id: window(task_id),
                tone,
            },
            WindowChange::Mode {
                task_id,
                mode,
                resizable,
            } => WindowChange::Mode {
                task_id: window(task_id),
                mode,
                resizable,
            },
            WindowChange::CaptionMode {
                task_id,
                app_drawn,
                interactive,
            } => WindowChange::CaptionMode {
                task_id: window(task_id),
                app_drawn,
                interactive,
            },
            WindowChange::CaptionBack { task_id, back } => WindowChange::CaptionBack {
                task_id: window(task_id),
                back,
            },
        }
    }
}

/// `report` about task `task_id` instead of the window it named.
fn with_task_id(report: FromHost, task_id: i32) -> FromHost {
    match report {
        FromHost::Closed { .. } => FromHost::Closed { task_id },
        FromHost::Resized { width, height, .. } => FromHost::Resized {
            task_id,
            width,
            height,
        },
        FromHost::Dpi { dpi, .. } => FromHost::Dpi { task_id, dpi },
        FromHost::Focus { focused, .. } => FromHost::Focus { task_id, focused },
        FromHost::Dragging { dragging, .. } => FromHost::Dragging { task_id, dragging },
        FromHost::Shown { shown, .. } => FromHost::Shown { task_id, shown },
        FromHost::Caption { band, .. } => FromHost::Caption { task_id, band },
        FromHost::MenuChosen { action, .. } => FromHost::MenuChosen { task_id, action },
        FromHost::MediaButton { button, .. } => FromHost::MediaButton { task_id, button },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::window_hosts::{HostAction, HostId, WindowHosts};
    use crate::tasks::AppInfo;

    const PLAY: &str = "com.android.vending";

    fn open(task_id: i32, package: &str) -> WindowChange {
        WindowChange::Open {
            task_id,
            package_name: package.into(),
            title: "Google Play Store".into(),
            width: 513,
            height: 914,
            focused: true,
        }
    }

    /// Window hosts with Play's window 9 open and its window program attached.
    fn attached(now: Instant) -> (WindowHosts, GpuResetWindows) {
        let mut hosts = WindowHosts::new();
        hosts.app(
            &AppInfo {
                package_name: PLAY.into(),
                label: "Google Play Store".into(),
                ..Default::default()
            },
            now,
        );
        let mut resets = GpuResetWindows::new();
        for change in resets.changes(vec![open(9, PLAY)]) {
            hosts.change(&change, now);
        }
        hosts.attached(HostId(1), PLAY, false, now);
        (hosts, resets)
    }

    fn carry_out(
        hosts: &mut WindowHosts,
        changes: &[WindowChange],
        now: Instant,
    ) -> Vec<HostAction> {
        changes
            .iter()
            .flat_map(|change| hosts.change(change, now))
            .collect()
    }

    #[test]
    fn nfr5_a_task_that_ends_after_a_gpu_reset_keeps_its_window_and_its_program() {
        let now = Instant::now();
        let (mut hosts, mut resets) = attached(now);
        resets.gpu_reset(now);
        let later = now + Duration::from_secs(16);
        let outcome = resets.guest_changes(vec![WindowChange::Close { task_id: 9 }], later);
        assert!(outcome.changes.is_empty());
        assert_eq!(outcome.relaunch, vec![PLAY.to_string()]);
        assert_eq!(outcome.kept, vec![9]);
        // Nothing reaches the window program: no close, and no end of its connection.
        assert!(carry_out(&mut hosts, &outcome.changes, later).is_empty());
        assert_eq!(hosts.tasks_of(HostId(1)), vec![9]);
        assert!(resets.is_kept(9));
    }

    #[test]
    fn nfr5_the_apps_new_task_takes_the_kept_window_over() {
        let now = Instant::now();
        let (mut hosts, mut resets) = attached(now);
        resets.report(FromHost::Resized {
            task_id: 9,
            width: 1166,
            height: 1033,
        });
        resets.gpu_reset(now);
        resets.guest_changes(vec![WindowChange::Close { task_id: 9 }], now);
        let outcome = resets.guest_changes(
            vec![
                open(14, PLAY),
                WindowChange::Retitle {
                    task_id: 14,
                    title: "Play".into(),
                },
            ],
            now + Duration::from_secs(3),
        );
        assert_eq!(outcome.adopted, vec![(14, Some((1166, 1033)))]);
        // No second window: the kept one takes the new task's title.
        assert_eq!(
            outcome.changes,
            vec![
                WindowChange::Retitle {
                    task_id: 9,
                    title: "Google Play Store".into()
                },
                WindowChange::Retitle {
                    task_id: 9,
                    title: "Play".into()
                },
            ]
        );
        let actions = carry_out(&mut hosts, &outcome.changes, now);
        assert!(actions
            .iter()
            .all(|action| !matches!(action, HostAction::Disconnect { .. })));
        assert_eq!(hosts.tasks_of(HostId(1)), vec![9]);
        // The window's reports reach the new task, and the task's changes the window.
        assert_eq!(
            resets.report(FromHost::Focus {
                task_id: 9,
                focused: true
            }),
            Report::Task(FromHost::Focus {
                task_id: 14,
                focused: true
            })
        );
        assert_eq!(resets.window_of(14), 9);
        assert_eq!(
            resets.changes(vec![WindowChange::Close { task_id: 14 }]),
            vec![WindowChange::Close { task_id: 9 }]
        );
    }

    #[test]
    fn nfr5_a_task_that_ends_long_after_a_reset_closes_its_window() {
        let now = Instant::now();
        let (_, mut resets) = attached(now);
        let outcome = resets.guest_changes(vec![WindowChange::Close { task_id: 9 }], now);
        assert_eq!(outcome.changes, vec![WindowChange::Close { task_id: 9 }]);
        let (_, mut resets) = attached(now);
        resets.gpu_reset(now);
        let outcome = resets.guest_changes(
            vec![WindowChange::Close { task_id: 9 }],
            now + RESET_GRACE + Duration::from_secs(1),
        );
        assert_eq!(outcome.changes, vec![WindowChange::Close { task_id: 9 }]);
        assert!(outcome.relaunch.is_empty());
    }

    #[test]
    fn nfr5_a_kept_window_closes_when_its_app_does_not_come_back() {
        let now = Instant::now();
        let (_, mut resets) = attached(now);
        resets.gpu_reset(now);
        resets.guest_changes(vec![WindowChange::Close { task_id: 9 }], now);
        assert!(resets.tick(now + RELAUNCH_WAIT / 2).is_empty());
        assert_eq!(
            resets.tick(now + RELAUNCH_WAIT),
            vec![WindowChange::Close { task_id: 9 }]
        );
        assert!(!resets.is_kept(9));
    }

    #[test]
    fn nfr5_the_user_can_close_a_kept_window() {
        let now = Instant::now();
        let (_, mut resets) = attached(now);
        resets.gpu_reset(now);
        resets.guest_changes(vec![WindowChange::Close { task_id: 9 }], now);
        assert_eq!(
            resets.report(FromHost::Dragging {
                task_id: 9,
                dragging: true
            }),
            Report::Dropped
        );
        assert_eq!(
            resets.report(FromHost::Closed { task_id: 9 }),
            Report::CloseKept(WindowChange::Close { task_id: 9 })
        );
        assert!(resets.tick(now + RELAUNCH_WAIT).is_empty());
    }

    #[test]
    fn nfr5_losing_the_guest_closes_the_kept_windows_too() {
        let now = Instant::now();
        let (_, mut resets) = attached(now);
        resets.gpu_reset(now);
        resets.guest_changes(vec![WindowChange::Close { task_id: 9 }], now);
        assert_eq!(resets.close_all(), vec![WindowChange::Close { task_id: 9 }]);
        assert!(!resets.is_kept(9));
    }
}
