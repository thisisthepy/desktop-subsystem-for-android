//! Emptying the working set of a VM that has no app window open.
//!
//! While a user's Android has no app window open, the guest keeps running (push messages,
//! alarms and sync go on as before), but it touches little of its memory. The host still keeps
//! all of what the guest once touched in RAM: an operating system takes a process's pages out of
//! RAM only when it runs short. So every minute while no app window is open, the host empties
//! the VM process's working set. What the guest touches again comes straight back (from RAM when
//! the host has not written it out yet, from the page file when it has); what it does not touch
//! goes to the page file and leaves the RAM to the rest of the host. On the owner's PC
//! (1.0.1-dev.39) this took an idle VM from about 400 MiB to 135 MiB on average over ten minutes,
//! and Settings opened right after a trim showed its first frame no later than without one.
//!
//! - [`IdleVmTrim`] decides for one boot of one VM: when to empty its working set, and what the
//!   log says of it.
//! - [`IdleVmTrims`] follows every user's Android: whether it runs, whether any of its app windows
//!   is open, and which VM it runs in.
//! - [`run`] drives it on a thread of the service's own.
//!
//! The platform supplies [`VmWorkingSet`]: what the VM's process holds in RAM, how much the host
//! has written to its page file, and the call that empties the working set. A platform that has
//! no such call says so, and the log says it once.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use crate::host_api::{UserAndroid, UserAndroids};
use crate::memory::mib;
use crate::session::SessionState;
use crate::tasks::{TaskEvent, TaskWatch};

/// How long Android must have had no app window open, while running and ready, before the first
/// trim. The guest gives back what it can by itself first, and what it frees then reaches the
/// host as free pages, which the host drops without writing anything; a trim before that would
/// write those pages to the page file only for the guest to free them a moment later. After the
/// last window closes, the agent ends the closed app's processes within about 4 s, the control
/// daemon's idle reclaim runs on its next look (every 5 s) and takes up to 15 s, free page
/// reporting hands the freed pages over a second later, the guest's screen goes off and it dozes
/// after 30 s, and the agent ends cached processes that have been idle for 30 s on its next look
/// (every 15 s), so by 45 s. A minute is past all of them.
pub const IDLE_VM_TRIM_SETTLE: Duration = Duration::from_secs(60);

/// How often the VM's working set is emptied while no app window is open: what the guest touched
/// since the trim before is what stays in RAM. On the owner's PC a trim every minute kept an idle
/// VM at 135 MiB on average over ten minutes; one trim alone came back to about 260 MiB within
/// five.
pub const IDLE_VM_TRIM_INTERVAL: Duration = Duration::from_secs(60);

/// How often the VM's memory in RAM is looked at while the trims run, for the average the summary
/// line gives. A look is one process counter.
pub const IDLE_VM_LOOK_INTERVAL: Duration = Duration::from_secs(10);

/// How often the log sums up the trims while they run: as often as the memory log.
pub const IDLE_VM_SUMMARY_INTERVAL: Duration = crate::memory::LOG_INTERVAL;

/// What the platform can tell about a VM's memory at one moment; `None` for what it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VmMemoryLook {
    /// What the VM's process holds in RAM for itself, in KiB: on Windows its private working set
    /// (what Task Manager's Memory column shows), on Linux the resident memory of the VM's
    /// process.
    pub resident_kb: Option<u64>,
    /// How much the host has written to its page file (swap) since it started, in KiB. The
    /// operating systems count this for the whole host, not for one process, so what one trim
    /// wrote is the growth of this count after it; on an idle host the trims are what make it
    /// grow.
    pub paged_out_kb: Option<u64>,
}

/// Why the platform did not empty the VM's working set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmptyRefused {
    /// This host has no way to take another process's memory out of RAM, and why. It is said once
    /// and not tried again.
    Unsupported(String),
    /// It has one, but it did not work this time, and why. It is tried again a minute later.
    Failed(String),
}

/// The platform's part: looking at the VM's memory and emptying its working set. `vm_id` is the
/// VM's runtime ID, as the session reports it.
pub trait VmWorkingSet: Send + Sync {
    /// What the VM's process holds now, and the host's page file writes so far.
    fn look(&self, vm_id: &str) -> VmMemoryLook;
    /// Takes every page of the VM's process out of RAM that the operating system can take: what
    /// the guest touches again comes back on its own. The guest keeps running.
    fn empty(&self, vm_id: &str) -> Result<(), EmptyRefused>;
}

/// Where a VM stands, as the trims see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmActivity {
    /// Android is running and ready, and none of its app windows is open.
    Idle,
    /// An app window is open, shown or minimized, or the windows cannot be told.
    WindowOpen,
    /// Android is not running and ready: it is starting, stalled, stopping or stopped.
    NotReady,
}

impl VmActivity {
    /// What ended a trim's stretch, for its log line.
    fn ended_by(self) -> &'static str {
        match self {
            VmActivity::Idle => "the next trim",
            VmActivity::WindowOpen => "an app window opened",
            VmActivity::NotReady => "Android left its idle state",
        }
    }
}

/// The last trim of a VM: when, and what the VM held in RAM just before and just after, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastTrim {
    pub at: Instant,
    pub before_kb: Option<u64>,
    pub after_kb: Option<u64>,
}

/// A trim whose log line waits for the end of its stretch, when what came back and what was
/// written are known.
#[derive(Debug, Clone, Copy)]
struct OpenTrim {
    at: Instant,
    before_kb: Option<u64>,
    after_kb: Option<u64>,
    paged_out_kb: Option<u64>,
}

fn show_mib(kb: Option<u64>) -> String {
    kb.map_or("?".to_string(), |kb| mib(kb).to_string())
}

/// What a count that only grows grew by from `from` to `to`; unknown when either is, or when it
/// went back (a counter that wrapped or was reset).
fn growth(from: Option<u64>, to: Option<u64>) -> Option<u64> {
    let (from, to) = (from?, to?);
    to.checked_sub(from)
}

impl OpenTrim {
    /// The trim's log line, written at `now`, when its stretch ended for `ended_by` with the VM
    /// as `look` found it.
    fn line(&self, now: Instant, look: VmMemoryLook, ended_by: &str) -> String {
        format!(
            "idle VM trim: emptying the VM's working set took its memory in RAM from {} to {} \
             MiB; in the {} s until {ended_by}, it came back to {} MiB and the host wrote {} MiB \
             to its page file (counted for all of the host's processes)",
            show_mib(self.before_kb),
            show_mib(self.after_kb),
            now.saturating_duration_since(self.at).as_secs(),
            show_mib(look.resident_kb),
            show_mib(growth(self.paged_out_kb, look.paged_out_kb)),
        )
    }
}

/// What the trims did since the last summary line.
#[derive(Debug, Clone, Default)]
struct Period {
    started: Option<Instant>,
    first_paged_out_kb: Option<u64>,
    last_paged_out_kb: Option<u64>,
    trims: u32,
    failures: u32,
    looks: u32,
    resident_sum_kb: u64,
    least_kb: Option<u64>,
    most_kb: Option<u64>,
}

impl Period {
    /// Takes in a look at the VM while the trims run.
    fn look(&mut self, now: Instant, look: VmMemoryLook) {
        if self.started.is_none() {
            self.started = Some(now);
            self.first_paged_out_kb = look.paged_out_kb;
        }
        if look.paged_out_kb.is_some() {
            self.last_paged_out_kb = look.paged_out_kb;
        }
        if let Some(kb) = look.resident_kb {
            self.looks += 1;
            self.resident_sum_kb += kb;
            self.least_kb = Some(self.least_kb.map_or(kb, |least| least.min(kb)));
            self.most_kb = Some(self.most_kb.map_or(kb, |most| most.max(kb)));
        }
    }

    /// Takes in only the page file's count from a look taken after the trims stopped, so the
    /// writes the last trim caused are in the summary and the average is not.
    fn look_counters(&mut self, look: VmMemoryLook) {
        if self.started.is_some() && look.paged_out_kb.is_some() {
            self.last_paged_out_kb = look.paged_out_kb;
        }
    }

    fn due(&self, now: Instant) -> bool {
        self.started
            .is_some_and(|at| now.saturating_duration_since(at) >= IDLE_VM_SUMMARY_INTERVAL)
    }

    /// The summary line, if anything was tried since the last; the period starts again after it.
    fn close(&mut self, now: Instant) -> Option<String> {
        let started = self.started?;
        let line = (self.trims > 0 || self.failures > 0).then(|| {
            let average = (self.looks > 0).then(|| self.resident_sum_kb / u64::from(self.looks));
            let mut line = format!(
                "idle VM trim: {} {} in {} s; the VM's memory in RAM averaged {} MiB ({} to {} \
                 MiB, {} looks every {} s), and the host wrote {} MiB to its page file (counted \
                 for all of the host's processes)",
                self.trims,
                if self.trims == 1 { "trim" } else { "trims" },
                now.saturating_duration_since(started).as_secs(),
                show_mib(average),
                show_mib(self.least_kb),
                show_mib(self.most_kb),
                self.looks,
                IDLE_VM_LOOK_INTERVAL.as_secs(),
                show_mib(growth(self.first_paged_out_kb, self.last_paged_out_kb)),
            );
            if self.failures > 0 {
                line.push_str(&format!("; {} failed", self.failures));
            }
            line
        });
        let last = self.last_paged_out_kb;
        *self = Period::default();
        // The next period counts the page file's writes on from where this one stopped.
        self.last_paged_out_kb = last;
        line
    }
}

/// Decides when the host empties the working set of one boot of one VM, and what the log says of
/// it.
///
/// Once Android has had no app window open for [`IDLE_VM_TRIM_SETTLE`], running and ready, the
/// VM's working set is emptied, and again every [`IDLE_VM_TRIM_INTERVAL`] while that lasts. An
/// app window that opens, shown or minimized, stops it at once, and so does Android leaving its
/// running states; the wait starts again from the next time it is idle. Never while a window is
/// open.
///
/// Each trim has one log line: what the VM held in RAM just before and just after, and at the end
/// of its stretch (the next trim, or what stopped the trims) what had come back and how much the
/// host wrote to its page file meanwhile. Every [`IDLE_VM_SUMMARY_INTERVAL`] while the trims run,
/// and when they stop, a summary line gives the average the VM held in RAM.
#[derive(Debug, Clone)]
pub struct IdleVmTrim {
    activity: VmActivity,
    idle_since: Option<Instant>,
    last_trim: Option<Instant>,
    next_look: Option<Instant>,
    open: Option<OpenTrim>,
    period: Period,
    unsupported: Option<String>,
    /// A failure was logged in this idle stretch; the rest are counted in the summary.
    failure_told: bool,
    last: Option<LastTrim>,
}

impl Default for IdleVmTrim {
    fn default() -> Self {
        IdleVmTrim::new()
    }
}

impl IdleVmTrim {
    /// For a VM that has just been seen, not yet idle.
    pub fn new() -> IdleVmTrim {
        IdleVmTrim {
            activity: VmActivity::NotReady,
            idle_since: None,
            last_trim: None,
            next_look: None,
            open: None,
            period: Period::default(),
            unsupported: None,
            failure_told: false,
            last: None,
        }
    }

    /// Takes in where the VM stands at `now`. Idle starts the wait for the first trim, unless it
    /// already runs; anything else ends the trims at once.
    pub fn set_activity(&mut self, activity: VmActivity, now: Instant) {
        if activity == VmActivity::Idle {
            if self.activity != VmActivity::Idle || self.idle_since.is_none() {
                self.idle_since = Some(now);
            }
        } else {
            self.idle_since = None;
            self.last_trim = None;
            self.next_look = None;
            self.failure_told = false;
        }
        self.activity = activity;
    }

    /// The last trim, if this VM has had one.
    pub fn last_trim(&self) -> Option<LastTrim> {
        self.last
    }

    /// Why this host cannot empty a VM's working set, once the platform has said so.
    pub fn unsupported(&self) -> Option<&str> {
        self.unsupported.as_deref()
    }

    /// Does what is due at `now` to the VM `vm_id` through `vm`, and returns the log lines it
    /// wrote. Called often (every second); it looks at the VM only when something is due.
    pub fn tick(&mut self, now: Instant, vm_id: &str, vm: &dyn VmWorkingSet) -> Vec<String> {
        let mut lines = Vec::new();
        if self.activity != VmActivity::Idle {
            let ended_by = self.activity.ended_by();
            if let Some(open) = self.open.take() {
                let look = vm.look(vm_id);
                self.period.look_counters(look);
                lines.push(open.line(now, look, ended_by));
            }
            lines.extend(self.period.close(now));
            return lines;
        }
        let settled = self
            .idle_since
            .is_some_and(|since| now.saturating_duration_since(since) >= IDLE_VM_TRIM_SETTLE);
        if !settled || self.unsupported.is_some() {
            return lines;
        }
        // A summary that is due covers the trims before this moment, not the one due with it.
        if self.period.due(now) {
            lines.extend(self.period.close(now));
        }
        // The regular look comes before the trim, so the average is not taken just after one.
        if self.next_look.is_none_or(|at| now >= at) {
            self.next_look = Some(now + IDLE_VM_LOOK_INTERVAL);
            self.period.look(now, vm.look(vm_id));
        }
        let due = self
            .last_trim
            .is_none_or(|at| now.saturating_duration_since(at) >= IDLE_VM_TRIM_INTERVAL);
        if due {
            self.last_trim = Some(now);
            let before = vm.look(vm_id);
            if let Some(open) = self.open.take() {
                lines.push(open.line(now, before, VmActivity::Idle.ended_by()));
            }
            match vm.empty(vm_id) {
                Ok(()) => {
                    let after = vm.look(vm_id);
                    self.period.trims += 1;
                    self.open = Some(OpenTrim {
                        at: now,
                        before_kb: before.resident_kb,
                        after_kb: after.resident_kb,
                        paged_out_kb: before.paged_out_kb.or(after.paged_out_kb),
                    });
                    self.last = Some(LastTrim {
                        at: now,
                        before_kb: before.resident_kb,
                        after_kb: after.resident_kb,
                    });
                }
                Err(EmptyRefused::Unsupported(why)) => {
                    self.unsupported = Some(why);
                }
                Err(EmptyRefused::Failed(why)) => {
                    self.period.failures += 1;
                    if !self.failure_told {
                        self.failure_told = true;
                        lines.push(format!(
                            "idle VM trim: emptying the VM's working set failed ({why}); it is \
                             tried again every minute while no app window is open, and the \
                             summary counts the failures"
                        ));
                    }
                }
            }
        }
        lines
    }
}

/// Which app windows of one user's Android are open, from its task watch.
struct OpenTasks {
    watch: Option<TaskWatch>,
    open: BTreeSet<i32>,
}

impl OpenTasks {
    fn new(android: &dyn UserAndroid) -> OpenTasks {
        let mut tasks = OpenTasks {
            watch: None,
            open: BTreeSet::new(),
        };
        tasks.rewatch(android);
        tasks
    }

    fn rewatch(&mut self, android: &dyn UserAndroid) {
        self.open.clear();
        self.watch = android.watch_tasks();
        let current: Vec<TaskEvent> = self
            .watch
            .as_ref()
            .map(|watch| watch.current.clone())
            .unwrap_or_default();
        for event in &current {
            self.apply(event);
        }
    }

    fn apply(&mut self, event: &TaskEvent) {
        match event {
            TaskEvent::Updated(task) => {
                self.open.insert(task.task_id);
            }
            TaskEvent::Removed { task_id } => {
                self.open.remove(task_id);
            }
            _ => {}
        }
    }

    /// Whether any app window is open now (every task has one, shown or minimized); `None` when
    /// the tasks cannot be told.
    fn any_open(&mut self, android: &dyn UserAndroid) -> Option<bool> {
        let mut events = Vec::new();
        let mut lost = false;
        if let Some(watch) = &self.watch {
            loop {
                match watch.events.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        lost = true;
                        break;
                    }
                }
            }
        }
        for event in &events {
            self.apply(event);
        }
        if lost || self.watch.is_none() {
            self.rewatch(android);
        }
        self.watch.as_ref()?;
        Some(!self.open.is_empty())
    }
}

/// Follows every user's Android and empties the working set of each VM that has no app window
/// open, as [`IdleVmTrim`] decides.
#[derive(Default)]
pub struct IdleVmTrims {
    vms: HashMap<(String, String), IdleVmTrim>,
    tasks: HashMap<String, OpenTasks>,
    /// The platform said it cannot, and the log has said so.
    unsupported: bool,
}

impl IdleVmTrims {
    pub fn new() -> IdleVmTrims {
        IdleVmTrims::default()
    }

    /// Whether the platform has said it cannot empty a VM's working set; nothing is tried after.
    pub fn unsupported(&self) -> bool {
        self.unsupported
    }

    /// Does what is due at `now` for every user's Android in `androids`, through `vm`, and
    /// returns the log lines, each after the user it is about.
    pub fn tick(
        &mut self,
        androids: &dyn UserAndroids,
        vm: &dyn VmWorkingSet,
        now: Instant,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let mut running = HashSet::new();
        for (user, state) in androids.running() {
            let Ok(android) = androids.android(&user) else {
                continue;
            };
            let tasks = self
                .tasks
                .entry(user.clone())
                .or_insert_with(|| OpenTasks::new(&*android));
            let any_open = tasks.any_open(&*android);
            let Some(vm_id) = android.vm_id() else {
                continue;
            };
            let ready = matches!(
                state,
                SessionState::Active | SessionState::Dozing | SessionState::Frozen
            );
            let activity = match (ready, any_open) {
                (true, Some(false)) => VmActivity::Idle,
                (true, _) => VmActivity::WindowOpen,
                (false, _) => VmActivity::NotReady,
            };
            let key = (user.clone(), vm_id.clone());
            running.insert(key.clone());
            let trim = self.vms.entry(key).or_default();
            trim.set_activity(activity, now);
            if self.unsupported {
                continue;
            }
            lines.extend(
                trim.tick(now, &vm_id, vm)
                    .into_iter()
                    .map(|line| format!("{user}: {line}")),
            );
            if let Some(why) = trim.unsupported() {
                self.unsupported = true;
                lines.push(format!(
                    "{user}: idle VM trim: this host cannot empty a VM's working set ({why}); \
                     an idle VM keeps in RAM what it holds while no app window is open"
                ));
            }
        }
        // A VM that stopped, or that a new boot replaced: its last trim's line, and the summary.
        let gone: Vec<(String, String)> = self
            .vms
            .keys()
            .filter(|key| !running.contains(*key))
            .cloned()
            .collect();
        for key in gone {
            if let Some(mut trim) = self.vms.remove(&key) {
                trim.set_activity(VmActivity::NotReady, now);
                let (user, vm_id) = key;
                lines.extend(
                    trim.tick(now, &vm_id, vm)
                        .into_iter()
                        .map(|line| format!("{user}: {line}")),
                );
            }
        }
        lines
    }
}

/// Empties the working set of every idle VM of `androids` through `vm` until `stop` is set, as
/// [`IdleVmTrims`] decides, and writes what it did to the service's log (standard error) after
/// `build`. It runs on a thread of its own and looks every second, until the platform says it
/// cannot empty a working set at all.
pub fn run(androids: &dyn UserAndroids, vm: &dyn VmWorkingSet, build: &str, stop: &AtomicBool) {
    let mut trims = IdleVmTrims::new();
    while !stop.load(Ordering::Relaxed) && !trims.unsupported() {
        std::thread::sleep(Duration::from_secs(1));
        for line in trims.tick(androids, vm, Instant::now()) {
            eprintln!("{build}: {line}");
        }
    }
}
