//! How a process and a thread on a touch's path are scheduled.
#![cfg(windows)]

use dsa_host_windows::scheduling::{raise_this_process, raise_this_thread};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetPriorityClass, GetThreadPriority,
    ABOVE_NORMAL_PRIORITY_CLASS, THREAD_PRIORITY_HIGHEST,
};

#[test]
fn fr5_a_process_on_the_touch_path_runs_ahead_of_normal_programs() {
    raise_this_process().expect("raises");
    // SAFETY: the pseudo handle of this process.
    let class = unsafe { GetPriorityClass(GetCurrentProcess()) };
    assert_eq!(class, ABOVE_NORMAL_PRIORITY_CLASS);
}

#[test]
fn fr5_a_thread_on_the_touch_path_runs_at_high_priority() {
    std::thread::spawn(|| {
        raise_this_thread();
        // SAFETY: the pseudo handle of this thread.
        let priority = unsafe { GetThreadPriority(GetCurrentThread()) };
        assert_eq!(priority, THREAD_PRIORITY_HIGHEST);
    })
    .join()
    .unwrap();
}
