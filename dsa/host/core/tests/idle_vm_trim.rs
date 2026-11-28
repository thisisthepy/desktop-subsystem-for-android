//! Emptying an idle VM's working set: every minute while no app window is open, never while one
//! is, through the core's API with a fake VM and a fake Android.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::host_api::{PerUser, UserAndroid, UserAndroids};
use dsa_host_core::idle_vm_trim::{
    EmptyRefused, IdleVmTrim, IdleVmTrims, VmActivity, VmMemoryLook, VmWorkingSet,
    IDLE_VM_SUMMARY_INTERVAL, IDLE_VM_TRIM_INTERVAL, IDLE_VM_TRIM_SETTLE,
};
use dsa_host_core::ime::ImeEvent;
use dsa_host_core::input::PointerEvent;
use dsa_host_core::keys::KeyEvent;
use dsa_host_core::session::SessionState;
use dsa_host_core::tasks::{Task, TaskEvent, TaskRequestError, TaskWatch};

const MIB: u64 = 1024;

/// A VM whose memory the test sets, and that records every time its working set is emptied.
struct FakeVm {
    state: Mutex<VmState>,
}

struct VmState {
    resident_kb: u64,
    paged_out_kb: u64,
    /// What the VM holds in RAM right after a trim.
    after_trim_kb: u64,
    refuse: Option<EmptyRefused>,
    emptied: Vec<String>,
}

impl FakeVm {
    fn new(resident_mib: u64, after_trim_mib: u64) -> FakeVm {
        FakeVm {
            state: Mutex::new(VmState {
                resident_kb: resident_mib * MIB,
                paged_out_kb: 0,
                after_trim_kb: after_trim_mib * MIB,
                refuse: None,
                emptied: Vec::new(),
            }),
        }
    }

    fn set(&self, resident_mib: u64, paged_out_mib: u64) {
        let mut state = self.state.lock().unwrap();
        state.resident_kb = resident_mib * MIB;
        state.paged_out_kb = paged_out_mib * MIB;
    }

    fn refuse(&self, refused: Option<EmptyRefused>) {
        self.state.lock().unwrap().refuse = refused;
    }

    fn trims(&self) -> usize {
        self.state.lock().unwrap().emptied.len()
    }
}

impl VmWorkingSet for FakeVm {
    fn look(&self, _vm_id: &str) -> VmMemoryLook {
        let state = self.state.lock().unwrap();
        VmMemoryLook {
            resident_kb: Some(state.resident_kb),
            paged_out_kb: Some(state.paged_out_kb),
        }
    }

    fn empty(&self, vm_id: &str) -> Result<(), EmptyRefused> {
        let mut state = self.state.lock().unwrap();
        if let Some(refused) = state.refuse.clone() {
            return Err(refused);
        }
        state.emptied.push(vm_id.to_string());
        state.resident_kb = state.after_trim_kb;
        Ok(())
    }
}

/// An Android whose state and app windows the test sets.
struct FakeAndroid {
    state: Mutex<SessionState>,
    vm_id: String,
    tasks: Mutex<Vec<i32>>,
    watchers: Mutex<Vec<Sender<TaskEvent>>>,
}

impl FakeAndroid {
    fn new(state: SessionState, vm_id: &str) -> Arc<FakeAndroid> {
        Arc::new(FakeAndroid {
            state: Mutex::new(state),
            vm_id: vm_id.to_string(),
            tasks: Mutex::new(Vec::new()),
            watchers: Mutex::new(Vec::new()),
        })
    }

    fn set_state(&self, state: SessionState) {
        *self.state.lock().unwrap() = state;
    }

    fn tell(&self, event: TaskEvent) {
        self.watchers
            .lock()
            .unwrap()
            .retain(|watcher| watcher.send(event.clone()).is_ok());
    }

    /// A task appears: its window opens, whether it is then shown or minimized.
    fn open_window(&self, task_id: i32) {
        self.tasks.lock().unwrap().push(task_id);
        self.tell(TaskEvent::Updated(Task {
            task_id,
            ..Task::default()
        }));
    }

    fn close_window(&self, task_id: i32) {
        self.tasks.lock().unwrap().retain(|id| *id != task_id);
        self.tell(TaskEvent::Removed { task_id });
    }
}

impl UserAndroid for FakeAndroid {
    fn state(&self) -> SessionState {
        *self.state.lock().unwrap()
    }
    fn last_reason(&self) -> Option<String> {
        None
    }
    fn start(&self) {}
    fn stop(&self) {}
    fn vm_id(&self) -> Option<String> {
        (self.state() != SessionState::Stopped).then(|| self.vm_id.clone())
    }
    fn watch_tasks(&self) -> Option<TaskWatch> {
        let (tx, events) = mpsc::channel();
        self.watchers.lock().unwrap().push(tx);
        let current = self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .map(|task_id| {
                TaskEvent::Updated(Task {
                    task_id: *task_id,
                    ..Task::default()
                })
            })
            .collect();
        Some(TaskWatch { current, events })
    }
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Ok(())
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Ok(())
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Ok(())
    }
    fn share_clipboard(&self, _: bool) {}
    fn host_clipboard(&self, _: String) {}
    fn watch_clipboard(&self) -> Option<Receiver<String>> {
        None
    }
}

/// The users' Androids, each made on first use.
fn androids(users: Vec<(&str, Arc<FakeAndroid>)>) -> Arc<dyn UserAndroids> {
    let list: Vec<(String, Arc<FakeAndroid>)> = users
        .into_iter()
        .map(|(user, android)| (user.to_string(), android))
        .collect();
    let names: Vec<String> = list.iter().map(|(user, _)| user.clone()).collect();
    let androids = Arc::new(PerUser::new(move |user: &str| {
        list.iter()
            .find(|(name, _)| name == user)
            .map(|(_, android)| Arc::clone(android) as Arc<dyn UserAndroid>)
            .ok_or_else(|| format!("no Android for {user}"))
    }));
    for user in names {
        androids.android(&user).unwrap();
    }
    androids
}

/// Ticks `trims` once a second from `from` to `to` seconds after `start`, both included, and
/// returns the log lines.
fn tick_seconds(
    trims: &mut IdleVmTrims,
    androids: &dyn UserAndroids,
    vm: &FakeVm,
    start: Instant,
    from: u64,
    to: u64,
) -> Vec<String> {
    (from..=to)
        .flat_map(|second| trims.tick(androids, vm, start + Duration::from_secs(second)))
        .collect()
}

fn at(start: Instant, seconds: u64) -> Instant {
    start + Duration::from_secs(seconds)
}

#[test]
fn nfr1_an_idle_vm_working_set_is_emptied_every_minute_while_no_window_is_open() {
    let start = Instant::now();
    let android = FakeAndroid::new(SessionState::Dozing, "vm-1");
    let androids = androids(vec![("alice", Arc::clone(&android))]);
    let vm = FakeVm::new(400, 150);
    let mut trims = IdleVmTrims::new();

    // Ten minutes and a little more without an app window: idle from the first look (1 s), so
    // the first trim at 61 s and one a minute after it, the last at 601 s.
    tick_seconds(&mut trims, &*androids, &vm, start, 1, 660);
    assert_eq!(vm.trims(), 10);
    assert_eq!(vm.state.lock().unwrap().emptied[0], "vm-1");

    // A window open (shown or minimized): never, however long.
    android.open_window(7);
    tick_seconds(&mut trims, &*androids, &vm, start, 661, 1260);
    assert_eq!(vm.trims(), 10);

    // An Android whose window was open all along is never trimmed.
    let busy = FakeAndroid::new(SessionState::Active, "vm-2");
    busy.open_window(3);
    let androids = androids_of_one("bob", &busy);
    let vm = FakeVm::new(400, 150);
    let mut trims = IdleVmTrims::new();
    tick_seconds(&mut trims, &*androids, &vm, start, 1, 1200);
    assert_eq!(vm.trims(), 0);
}

fn androids_of_one(user: &str, android: &Arc<FakeAndroid>) -> Arc<dyn UserAndroids> {
    androids(vec![(user, Arc::clone(android))])
}

#[test]
fn nfr1_the_idle_vm_trim_waits_for_the_settle_time() {
    let start = Instant::now();
    let android = FakeAndroid::new(SessionState::Active, "vm-1");
    android.open_window(1);
    let androids = androids_of_one("alice", &android);
    let vm = FakeVm::new(400, 150);
    let mut trims = IdleVmTrims::new();
    tick_seconds(&mut trims, &*androids, &vm, start, 0, 100);
    assert_eq!(vm.trims(), 0);

    // The last window closes at 101 s: the guest gives back what it can by itself first.
    android.close_window(1);
    let settle = IDLE_VM_TRIM_SETTLE.as_secs();
    tick_seconds(&mut trims, &*androids, &vm, start, 101, 101 + settle - 1);
    assert_eq!(vm.trims(), 0);
    tick_seconds(
        &mut trims,
        &*androids,
        &vm,
        start,
        101 + settle,
        101 + settle,
    );
    assert_eq!(vm.trims(), 1);

    // A window that opens and closes again starts the wait again.
    android.open_window(2);
    tick_seconds(&mut trims, &*androids, &vm, start, 162, 165);
    android.close_window(2);
    tick_seconds(&mut trims, &*androids, &vm, start, 166, 166 + settle - 1);
    assert_eq!(vm.trims(), 1);
    tick_seconds(
        &mut trims,
        &*androids,
        &vm,
        start,
        166 + settle,
        166 + settle,
    );
    assert_eq!(vm.trims(), 2);
}

#[test]
fn nfr1_the_idle_vm_trim_stops_at_once_when_a_window_opens() {
    let start = Instant::now();
    let android = FakeAndroid::new(SessionState::Dozing, "vm-1");
    let androids = androids_of_one("alice", &android);
    let vm = FakeVm::new(400, 150);
    let mut trims = IdleVmTrims::new();
    tick_seconds(&mut trims, &*androids, &vm, start, 0, 60);
    assert_eq!(vm.trims(), 1);

    // An app opens at 90 s; the session is still Dozing for a moment (the screen comes on
    // after), but the window is what counts.
    android.open_window(5);
    let lines = tick_seconds(&mut trims, &*androids, &vm, start, 90, 90);
    assert_eq!(vm.trims(), 1);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("alice: idle VM trim: emptying")
                && line.contains("in the 30 s until an app window opened")),
        "{lines:?}"
    );
    tick_seconds(&mut trims, &*androids, &vm, start, 91, 400);
    assert_eq!(vm.trims(), 1);
}

#[test]
fn nfr1_the_idle_vm_trim_waits_while_android_is_not_ready() {
    let start = Instant::now();
    let android = FakeAndroid::new(SessionState::Starting, "vm-1");
    let androids = androids_of_one("alice", &android);
    let vm = FakeVm::new(400, 150);
    let mut trims = IdleVmTrims::new();
    tick_seconds(&mut trims, &*androids, &vm, start, 0, 300);
    assert_eq!(vm.trims(), 0);

    // Ready with no window, whether the guest dozes yet or not.
    android.set_state(SessionState::Active);
    tick_seconds(&mut trims, &*androids, &vm, start, 301, 361);
    assert_eq!(vm.trims(), 1);

    // A guest whose agent is unreachable is not trimmed.
    android.set_state(SessionState::Stalled);
    tick_seconds(&mut trims, &*androids, &vm, start, 362, 600);
    assert_eq!(vm.trims(), 1);
}

#[test]
fn nfr1_each_idle_vm_trim_logs_its_working_set_and_page_file_writes() {
    let start = Instant::now();
    let vm = FakeVm::new(396, 154);
    vm.set(396, 1000);
    let mut trim = IdleVmTrim::new();
    trim.set_activity(VmActivity::Idle, start);
    assert!(trim.tick(at(start, 59), "vm", &vm).is_empty());
    assert!(trim.tick(at(start, 60), "vm", &vm).is_empty());
    assert_eq!(vm.trims(), 1);
    let last = trim.last_trim().unwrap();
    assert_eq!(last.at, at(start, 60));
    assert_eq!(
        (last.before_kb, last.after_kb),
        (Some(396 * MIB), Some(154 * MIB))
    );

    // What the guest touched came back, and the host wrote to its page file meanwhile.
    vm.set(262, 1180);
    let lines = trim.tick(at(start, 120), "vm", &vm);
    assert_eq!(
        lines,
        [
            "idle VM trim: emptying the VM's working set took its memory in RAM from 396 to 154 \
             MiB; in the 60 s until the next trim, it came back to 262 MiB and the host wrote \
             180 MiB to its page file (counted for all of the host's processes)"
        ]
    );
    assert_eq!(vm.trims(), 2);

    // A window opens: the last trim's line, then the summary of the stretch.
    vm.set(300, 1200);
    trim.set_activity(VmActivity::WindowOpen, at(start, 150));
    let lines = trim.tick(at(start, 150), "vm", &vm);
    assert_eq!(
        lines,
        [
            "idle VM trim: emptying the VM's working set took its memory in RAM from 262 to 154 \
             MiB; in the 30 s until an app window opened, it came back to 300 MiB and the host \
             wrote 20 MiB to its page file (counted for all of the host's processes)",
            "idle VM trim: 2 trims in 90 s; the VM's memory in RAM averaged 329 MiB (262 to 396 \
             MiB, 2 looks every 10 s), and the host wrote 200 MiB to its page file (counted for \
             all of the host's processes)",
        ]
    );
    // Nothing more until it is idle again.
    assert!(trim.tick(at(start, 400), "vm", &vm).is_empty());
    assert_eq!(vm.trims(), 2);
}

#[test]
fn nfr1_the_idle_vm_trims_are_summed_up_every_five_minutes() {
    let start = Instant::now();
    let vm = FakeVm::new(200, 100);
    let mut trim = IdleVmTrim::new();
    trim.set_activity(VmActivity::Idle, start);
    let settle = IDLE_VM_TRIM_SETTLE.as_secs();
    let summary = IDLE_VM_SUMMARY_INTERVAL.as_secs();
    let mut lines = Vec::new();
    for second in 0..=settle + summary {
        lines.extend(trim.tick(at(start, second), "vm", &vm));
    }
    let summaries: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains(" trims in "))
        .collect();
    assert_eq!(summaries.len(), 1, "{lines:?}");
    // One trim a minute: at the settle time and every minute after it, five in five minutes.
    let trims = summary / IDLE_VM_TRIM_INTERVAL.as_secs();
    assert!(
        summaries[0].starts_with(&format!("idle VM trim: {trims} trims in {summary} s;")),
        "{}",
        summaries[0]
    );
}

#[test]
fn nfr1_a_host_that_cannot_empty_a_working_set_says_so_once() {
    let start = Instant::now();
    let alice = FakeAndroid::new(SessionState::Dozing, "vm-1");
    let bob = FakeAndroid::new(SessionState::Dozing, "vm-2");
    let androids = androids(vec![("alice", alice), ("bob", bob)]);
    let vm = FakeVm::new(400, 150);
    vm.refuse(Some(EmptyRefused::Unsupported(
        "the operating system has no such call".into(),
    )));
    let mut trims = IdleVmTrims::new();
    let lines = tick_seconds(&mut trims, &*androids, &vm, start, 0, 1200);
    let said: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("this host cannot empty a VM's working set"))
        .collect();
    assert_eq!(said.len(), 1, "{lines:?}");
    assert!(said[0].contains("(the operating system has no such call)"));
    assert_eq!(vm.trims(), 0);
    // The service's thread stops there.
    assert!(trims.unsupported());
}

#[test]
fn nfr1_a_failed_idle_vm_trim_is_logged_once_and_tried_again_a_minute_later() {
    let start = Instant::now();
    let vm = FakeVm::new(400, 150);
    vm.refuse(Some(EmptyRefused::Failed("access denied".into())));
    let mut trim = IdleVmTrim::new();
    trim.set_activity(VmActivity::Idle, start);
    let mut lines = Vec::new();
    for second in 0..=180 {
        lines.extend(trim.tick(at(start, second), "vm", &vm));
    }
    assert_eq!(
        lines,
        ["idle VM trim: emptying the VM's working set failed (access denied); it is tried again \
          every minute while no app window is open, and the summary counts the failures"]
    );
    // It works on the next try.
    vm.refuse(None);
    trim.tick(at(start, 239), "vm", &vm);
    assert_eq!(vm.trims(), 0);
    trim.tick(at(start, 240), "vm", &vm);
    assert_eq!(vm.trims(), 1);
}

#[test]
fn nfr1_a_vm_that_stops_gets_the_line_of_its_last_trim() {
    let start = Instant::now();
    let android = FakeAndroid::new(SessionState::Dozing, "vm-1");
    let androids = androids_of_one("alice", &android);
    let vm = FakeVm::new(400, 150);
    let mut trims = IdleVmTrims::new();
    tick_seconds(&mut trims, &*androids, &vm, start, 0, 60);
    assert_eq!(vm.trims(), 1);
    android.set_state(SessionState::Stopped);
    let lines = tick_seconds(&mut trims, &*androids, &vm, start, 70, 70);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("until Android left its idle state")),
        "{lines:?}"
    );
    tick_seconds(&mut trims, &*androids, &vm, start, 71, 400);
    assert_eq!(vm.trims(), 1);
}
