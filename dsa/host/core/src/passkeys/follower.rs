//! The follower that performs the passkey ceremonies of the user's Android on the host, the
//! same on every host.
//!
//! It watches the ceremonies over the local API and reconnects whenever the connection ends.
//! Each ceremony runs on a thread of its own, so the watch goes on while the host's prompt is
//! up. What became of it goes back to Android over a new connection: the credential made or
//! the signature, or why there is none. A ceremony Android withdraws (the app gave up, Android
//! stopped) is canceled in the host's prompt too, and has nobody waiting for its result.
//! Nothing about a ceremony ends the follower. What performs one is the host's
//! [`PasskeyRun`].

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_transport::Connector;

use super::{Ceremony, CeremonyResult, PasskeyEvent};
use crate::host_api::HostApiClient;

type Hook = Box<dyn Fn() + Send>;

/// Whether a ceremony is still wanted, and how to stop it while it runs.
#[derive(Default)]
pub struct Cancel {
    withdrawn: Arc<AtomicBool>,
    hook: Mutex<Option<Hook>>,
}

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Android no longer wants the ceremony: stops it if it runs.
    pub fn withdraw(&self) {
        self.withdrawn.store(true, Ordering::SeqCst);
        if let Some(hook) = self.hook.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            hook();
        }
    }

    pub fn is_withdrawn(&self) -> bool {
        self.withdrawn.load(Ordering::SeqCst)
    }

    /// The flag [`withdraw`](Cancel::withdraw) sets, for a host that polls it.
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.withdrawn)
    }

    /// Sets what stops the ceremony while it runs. False when it was withdrawn already, so it
    /// is not started; a withdrawal from now on calls `hook`.
    pub fn while_running(&self, hook: Hook) -> bool {
        *self.hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
        !self.is_withdrawn()
    }

    /// The ceremony is over; nothing is left to stop.
    pub fn finished(&self) {
        *self.hook.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Performs `ceremony` with `run`, unless it was withdrawn first; a panic in the authenticator's
/// code is a failure of this ceremony only, and a ceremony withdrawn while it ran is canceled
/// whatever `run` answered.
pub fn perform_guarded(cancel: &Cancel, run: impl FnOnce() -> CeremonyResult) -> CeremonyResult {
    if cancel.is_withdrawn() {
        return CeremonyResult::Canceled("Android withdrew the request".into());
    }
    let performed = catch_unwind(AssertUnwindSafe(run));
    cancel.finished();
    let result =
        performed.unwrap_or_else(|_| CeremonyResult::Failed("the passkey code failed".into()));
    if cancel.is_withdrawn() {
        return CeremonyResult::Canceled("Android withdrew the request".into());
    }
    result
}

/// What a ceremony is, for the log: never the user's name or any secret.
pub fn describe(ceremony: &Ceremony) -> String {
    let what = if ceremony.create {
        "making a passkey"
    } else {
        "signing in with a passkey"
    };
    let who = if ceremony.app_label.is_empty() {
        &ceremony.package
    } else {
        &ceremony.app_label
    };
    format!("{what} for {} ({who})", ceremony.rp_id)
}

/// Performs one ceremony and says what became of it. It may block until the user answers.
pub type PasskeyRun = Arc<dyn Fn(u64, &Ceremony, &Cancel) -> CeremonyResult + Send + Sync>;

/// Where a message to the log goes.
pub type PasskeyLog = Arc<dyn Fn(&str) + Send + Sync>;

/// How a host's follower talks to the service.
pub struct PasskeyFollowing {
    /// What the follower calls itself to the service.
    pub build: &'static str,
    /// How long a request to the service is waited for.
    pub answer: Duration,
    /// How long to wait before connecting again after the service could not be reached.
    pub retry: Duration,
    /// How long the watch waits for the service before it looks for finished ceremonies again.
    pub poll: Duration,
    /// The name of each ceremony's thread.
    pub thread_name: &'static str,
}

/// Performs the passkey ceremonies of the user's Android until `stop`. `wait` is how the host
/// waits out the retry time (it may end early when `stop` is set).
pub fn follow_passkeys(
    connector: &mut dyn Connector,
    following: &PasskeyFollowing,
    run: &PasskeyRun,
    stop: &AtomicBool,
    wait: &dyn Fn(Duration),
    log: &PasskeyLog,
) {
    let (done_tx, done) = mpsc::channel();
    let mut running: HashMap<u64, Arc<Cancel>> = HashMap::new();
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        let watch = HostApiClient::connect(&mut *connector, following.build, following.answer)
            .and_then(|client| client.watch_passkeys());
        let mut watch = match watch {
            Ok(watch) => watch,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    log(&format!("passkeys cannot reach the service: {error}"));
                    last_error = error;
                }
                report(connector, following, &done, &mut running, log);
                wait(following.retry);
                continue;
            }
        };
        last_error.clear();
        while !stop.load(Ordering::Relaxed) {
            report(connector, following, &done, &mut running, log);
            match watch.next(following.poll) {
                Ok(Some(PasskeyEvent::Ceremony { id, ceremony })) => {
                    let cancel = Arc::new(Cancel::new());
                    running.insert(id, Arc::clone(&cancel));
                    start(
                        following.thread_name,
                        id,
                        *ceremony,
                        Arc::clone(run),
                        cancel,
                        done_tx.clone(),
                        Arc::clone(log),
                    );
                }
                Ok(Some(PasskeyEvent::Withdrawn { id })) => {
                    if let Some(cancel) = running.get(&id) {
                        cancel.withdraw();
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    log(&format!(
                        "the passkeys' connection to the service ended: {error}"
                    ));
                    break;
                }
            }
        }
    }
    // The follower is ending: what still runs is canceled in the host's prompt.
    for cancel in running.values() {
        cancel.withdraw();
    }
}

/// Passes what became of each finished ceremony to the service. A withdrawn one has nobody
/// waiting for it, and is not reported.
fn report(
    connector: &mut dyn Connector,
    following: &PasskeyFollowing,
    done: &Receiver<(u64, CeremonyResult)>,
    running: &mut HashMap<u64, Arc<Cancel>>,
    log: &PasskeyLog,
) {
    while let Ok((id, result)) = done.try_recv() {
        let withdrawn = running
            .remove(&id)
            .is_some_and(|cancel| cancel.is_withdrawn());
        if withdrawn {
            continue;
        }
        let sent = HostApiClient::connect(connector, following.build, following.answer)
            .and_then(|mut client| client.report_passkey_ceremony(id, &result));
        if let Err(error) = sent {
            log(&format!(
                "what became of passkey ceremony {id} did not reach Android: {error}"
            ));
        }
    }
}

/// Performs one ceremony on a thread of its own, and sends what became of it on `done`.
fn start(
    thread_name: &'static str,
    id: u64,
    ceremony: Ceremony,
    run: PasskeyRun,
    cancel: Arc<Cancel>,
    done: Sender<(u64, CeremonyResult)>,
    log: PasskeyLog,
) {
    let failed = done.clone();
    let spawned = thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            let result = perform_guarded(&cancel, || run(id, &ceremony, &cancel));
            match &result {
                CeremonyResult::Created { .. } | CeremonyResult::Asserted { .. } => {}
                CeremonyResult::Canceled(why)
                | CeremonyResult::Unavailable(why)
                | CeremonyResult::NotAllowed(why)
                | CeremonyResult::Failed(why) => log(&format!("{}: {why}", describe(&ceremony))),
            }
            let _ = done.send((id, result));
        });
    if let Err(error) = spawned {
        let why = format!("the ceremony could not start: {error}");
        let _ = failed.send((id, CeremonyResult::Failed(why)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr36_a_ceremony_withdrawn_before_it_starts_is_canceled_without_running() {
        let cancel = Cancel::new();
        cancel.withdraw();
        let result = perform_guarded(&cancel, || panic!("not performed"));
        assert_eq!(
            result,
            CeremonyResult::Canceled("Android withdrew the request".into())
        );
    }

    #[test]
    fn fr36_a_panic_in_the_authenticator_fails_only_that_ceremony() {
        let cancel = Cancel::new();
        let result = perform_guarded(&cancel, || panic!("authenticator bug"));
        assert_eq!(
            result,
            CeremonyResult::Failed("the passkey code failed".into())
        );
    }

    #[test]
    fn fr36_a_ceremony_withdrawn_while_it_runs_is_canceled_whatever_it_answered() {
        let cancel = Cancel::new();
        let result = perform_guarded(&cancel, || {
            cancel.withdraw();
            CeremonyResult::Failed("late".into())
        });
        assert_eq!(
            result,
            CeremonyResult::Canceled("Android withdrew the request".into())
        );
        let kept = Cancel::new();
        assert_eq!(
            perform_guarded(&kept, || CeremonyResult::Failed("x".into())),
            CeremonyResult::Failed("x".into())
        );
    }

    #[test]
    fn fr36_the_hook_runs_on_withdrawal_and_a_finished_ceremony_has_none() {
        use std::sync::atomic::AtomicU32;
        let calls = Arc::new(AtomicU32::new(0));
        let cancel = Cancel::new();
        let counted = Arc::clone(&calls);
        assert!(cancel.while_running(Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        })));
        cancel.withdraw();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let finished = Cancel::new();
        let counted = Arc::clone(&calls);
        finished.while_running(Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        finished.finished();
        finished.withdraw();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fr36_a_ceremony_is_described_without_the_user_or_any_secret() {
        let ceremony = Ceremony {
            create: true,
            rp_id: "bank.example".into(),
            user_name: "secret".into(),
            package: "com.example.bank".into(),
            ..Ceremony::default()
        };
        assert_eq!(
            describe(&ceremony),
            "making a passkey for bank.example (com.example.bank)"
        );
    }
}
