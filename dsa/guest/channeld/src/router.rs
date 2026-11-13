//! Who gets which frame: the daemon's routing between the host and the guest's own users of
//! the channel.
//!
//! - A frame from the host goes to every live listener.
//! - A frame a guest client sends goes to the host. A task's bounds or its end
//!   (`TaskUpdated`, `TaskRemoved`) also go to every listener in the other guest processes,
//!   because the hwcomposer groups layers by the task bounds the agent reports.
//! - A listener whose process has died is dropped.
//! - The daemon keeps the latest state of each live task (its `TaskUpdated` and its
//!   `TaskFrames`) and the latest `AppInfo` of each app with a live task. A listener that
//!   subscribes later, such as a restarted hwcomposer, is told the tasks already there, and
//!   the host each time it connects again is told the apps and then the tasks, since a client
//!   sends them only when they change.
//! - The daemon keeps the host's latest `PowerState`, which a listener that subscribes later,
//!   such as a restarted health HAL, is told first, and each power mode the power HAL has on
//!   (`PowerHint`), which the host is told each time it connects again.
//! - The daemon keeps the `LocationRequest` of each process that wants a location (the GNSS
//!   HAL, and the agent as the network location provider) and tells the host what they want
//!   together: a location while any of them wants one, as often as the most frequent asks.
//!   The host is told again each time it connects. When a process that wants a location
//!   dies, the host is told what the others still want, and that none is wanted once none
//!   of them is left, so it stops its location service.
//! - The agent says it is there (`AgentConnected`); that stops here. The host hears how far the
//!   guest has got (`GuestBoot`) each time it connects and on each change: whether Android has
//!   finished booting, and whether the agent is connected, which ends when its process dies.
//!   Only the agent may say so: the platform gives the router a check for the caller, and
//!   [`agent_caller`] is the rule it applies to what binder and the kernel say of the caller.
//!
//! - The host's `DeveloperMode` stops here too: it goes to the platform's developer mode
//!   switch ([`Router::on_developer_mode`]), which has init start or stop adbd. A guest boots
//!   with it off; the host says when it is on.
//! - The host's `FileSharing` goes to the platform's file sharing switch
//!   ([`Router::on_file_sharing`]), which has init mount or take away the host's folders, and
//!   on to the listeners too: the agent has the media scanner look at the folders. Its
//!   `AndroidFiles` goes to the switch alone ([`Router::on_android_files`]), which has init
//!   serve the shared storage to the host.
//! - The host's `HostCertificates` goes to the platform's certificate installer
//!   ([`Router::on_host_certificates`]), which adds the roots the host names to Android's
//!   system certificates and takes away those it no longer names, and then on to the
//!   listeners: the agent has the running apps forget the trust anchors they looked up, once
//!   the folders already say what the host said.
//! - The host's ARM translation and Google apps messages stop here as well, and so do its
//!   reads of an add-on image an older host left on the user's disk: the daemon looks after
//!   those images ([`Router::on_arm_translation`], [`Router::on_gapps`]) and answers each one.
//! - So do the host's game controllers: each becomes a gamepad device of the daemon's
//!   ([`Router::on_gamepads`]), whose rumble goes back to the host.
//! - The diagnostics the agent gathers go to the log port ([`Router::divert_diagnostics`]),
//!   never on the control channel, and only the agent may send them. The guest's log is the
//!   daemon's own to send; no client may.
//!
//! None of this knows about binder: a listener is anything that can take a frame, so the
//! rules run and are tested on any platform. The binder service in `binder` is a thin layer
//! that turns calls into these.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    AddonImage, AddonImagePart, AddonImageRead, AndroidFiles, ArmTranslationState, Envelope,
    FileSharing, GappsState, GuestBoot, HostCertificates, LocationRequest, PowerHintKind,
    RootElevationConfig,
};

use crate::arm_translation::ArmTranslation;
use crate::gamepad::GamepadDevices;
use crate::gapps::Gapps;

/// The agent's package, which is the name of its process.
pub const AGENT_PROCESS: &str = "io.github.thisisthepy.dsa.agent";

/// Whether a caller is the agent, from its SELinux context as binder reports it and its process's
/// `/proc/<pid>/cmdline`. The domain comes from the kernel, and keeps out the hwcomposer and
/// anything else that may call the daemon. The name tells the agent from other platform-signed
/// apps in the same domain. An error says what did not match.
pub fn agent_caller(context: Option<&str>, cmdline: Option<&[u8]>) -> Result<(), String> {
    let context = context.ok_or_else(|| "binder did not say the caller's context".to_string())?;
    let domain = context
        .strip_prefix("u:r:")
        .and_then(|rest| rest.split(':').next())
        .unwrap_or_default();
    // Android 17 runs a platform app that targets SDK 36 or later in `platform_app_<sdk>`.
    let platform_app = domain == "platform_app"
        || domain
            .strip_prefix("platform_app_")
            .is_some_and(|sdk| !sdk.is_empty() && sdk.bytes().all(|b| b.is_ascii_digit()));
    if !platform_app && domain != "system_app" {
        return Err(format!(
            "the caller runs in {context:?}, not as a platform app"
        ));
    }
    let cmdline = cmdline.ok_or_else(|| "the caller's process name cannot be read".to_string())?;
    let process = cmdline.split(|byte| *byte == 0).next().unwrap_or_default();
    if process != AGENT_PROCESS.as_bytes() {
        return Err(format!(
            "the caller is the process {:?}, not {AGENT_PROCESS}",
            String::from_utf8_lossy(process)
        ));
    }
    Ok(())
}

/// The guest process a call came from (its process ID, as binder reports the caller).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Peer(pub i32);

/// Names one subscription, so the listener can be dropped when its process dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(pub u64);

/// What became of one frame handed to a listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    /// The listener's process is gone; it never takes another frame.
    Gone,
    /// This frame did not arrive (the listener's queue was full, say), but the listener
    /// lives and takes the next one.
    Failed,
}

/// A guest client's callback for frames. It must not block: the router calls it with its
/// state locked, which keeps every listener's frames in the order they were routed.
pub trait Listener: Send + Sync {
    fn deliver(&self, frame: &[u8]) -> Delivery;
}

/// Why a frame a guest client sent was not passed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// It is not one control message.
    Malformed(String),
    /// It is a message only the daemon itself may send, or carries no message at all.
    NotAllowed(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Malformed(why) | SendError::NotAllowed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SendError {}

struct Subscription {
    id: ListenerId,
    peer: Peer,
    listener: Arc<dyn Listener>,
}

/// The most apps the daemon keeps an `AppInfo` for. An app's is dropped when its last task
/// ends, so only a broken or hostile client reaches this; past it, the frames still go to the
/// host but are not kept.
pub const MAX_APPS: usize = 1024;

/// The most power modes the daemon keeps on. android.hardware.power has fewer than this.
pub const MAX_POWER_MODES: usize = 64;

/// The latest state of one live task, as the frames that carried it.
#[derive(Default)]
struct Task {
    /// The last `TaskUpdated`, and who sent it.
    updated: Option<(Peer, Vec<u8>)>,
    /// The package that `TaskUpdated` named.
    package: String,
    /// The last `TaskFrames`.
    frames: Option<Vec<u8>>,
}

#[derive(Default)]
struct State {
    listeners: Vec<Subscription>,
    tasks: BTreeMap<i32, Task>,
    /// The last `AppInfo` of each app, by package.
    apps: BTreeMap<String, Vec<u8>>,
    /// The host said no task window is shown: open tasks are minimized, and the guest dozes.
    screen_off: bool,
    /// The tasks whose window the host last said it shows (open and not minimized), or `None`
    /// until it says.
    shown_tasks: Option<std::collections::BTreeSet<i32>>,
    /// The host said it is short of memory.
    host_memory_low: bool,
    next_id: u64,
    /// Android has finished booting.
    boot_completed: bool,
    /// The process of the agent, while it is connected.
    agent: Option<Peer>,
    /// The host's last `PowerState`.
    power_state: Option<Vec<u8>>,
    /// The `PowerHint` that turned each power mode on, by mode, while it is on.
    power_modes: BTreeMap<i32, Vec<u8>>,
    /// The interval each process that wants a location asked for, in milliseconds (0 lets
    /// the host choose), by its process ID, while it wants one.
    location_requests: BTreeMap<i32, u32>,
}

impl State {
    /// Hands `frame` to every listener not in `except`'s process, and drops those whose
    /// process is gone.
    fn deliver(&mut self, frame: &[u8], except: Option<Peer>) {
        self.listeners
            .retain(|s| Some(s.peer) == except || s.listener.deliver(frame) != Delivery::Gone);
    }

    /// What the host is told about how far the guest has got.
    fn boot_report(&self) -> Vec<u8> {
        dsa_protocol::encode(&Envelope {
            body: Some(Body::GuestBoot(GuestBoot {
                boot_completed: self.boot_completed,
                agent_connected: self.agent.is_some(),
            })),
        })
    }

    /// Whether a process that wants a location has no listener any longer; then forgets its
    /// request.
    fn location_left(&mut self) -> bool {
        let before = self.location_requests.len();
        let listeners = &self.listeners;
        self.location_requests
            .retain(|pid, _| listeners.iter().any(|s| s.peer.0 == *pid));
        self.location_requests.len() != before
    }

    /// What the processes want of the host's location together, as the frame the host is
    /// told: a location while any wants one, at the shortest interval any asked for (one that
    /// lets the host choose counts only when none named an interval).
    fn location_wanted(&self) -> Vec<u8> {
        let request = if self.location_requests.is_empty() {
            LocationRequest::default()
        } else {
            LocationRequest {
                active: true,
                interval_ms: self
                    .location_requests
                    .values()
                    .copied()
                    .filter(|ms| *ms > 0)
                    .min()
                    .unwrap_or(0),
            }
        };
        dsa_protocol::encode(&Envelope {
            body: Some(Body::LocationRequest(request)),
        })
    }

    /// Whether the agent's listener is gone, which ends its connection; then forgets it.
    fn agent_left(&mut self) -> bool {
        let gone = self
            .agent
            .is_some_and(|agent| !self.listeners.iter().any(|s| s.peer == agent));
        if gone {
            self.agent = None;
        }
        gone
    }
}

/// Routes frames between the host connection and the guest's listeners.
pub struct Router {
    to_host: Sender<Vec<u8>>,
    state: Mutex<State>,
    agent_check: AgentCheck,
    developer_mode: Option<DeveloperModeSwitch>,
    /// Mounts or takes away the host's folders, as the host says.
    file_sharing: Option<FileSharingSwitch>,
    /// Serves the shared storage to the host, as the host asks.
    android_files: Option<AndroidFilesSwitch>,
    /// Adds the host's root certificates to Android's system certificates.
    host_certificates: Option<HostCertificatesSwitch>,
    /// The ARM translation package on the user's disk, which the host installs and removes.
    arm_translation: Option<Arc<ArmTranslation>>,
    /// The Google apps' image on the user's disk, which the host installs and removes.
    gapps: Option<Arc<Gapps>>,
    /// The gamepad devices of the host's controllers.
    gamepads: Option<Arc<GamepadDevices>>,
    /// Drives the kernel's su guard for the host's root elevation config.
    root_elevation: Option<RootElevationSwitch>,
    /// The su relay, which the host's su decisions answer.
    su_relay: Option<Arc<crate::su_relay::SuRelay>>,
    /// Where the agent's diagnostics go instead of the host's control channel.
    diagnostics: Mutex<Option<Sender<Vec<u8>>>>,
    /// How many times an app's last task has ended, and the wake-up for whoever waits on it.
    apps_closed: (Mutex<u64>, Condvar),
    /// The packages whose last task ended since the reclaim last asked.
    closed_packages: Mutex<Vec<String>>,
}

/// Turns the guest's developer mode (adbd) on or off, as the host says.
type DeveloperModeSwitch = Box<dyn Fn(bool) + Send + Sync>;

/// Mounts the host's folders the host shares, and takes the others away.
type FileSharingSwitch = Box<dyn Fn(&FileSharing) + Send + Sync>;

/// Serves the shared storage to the host on the port, with the token, it names.
type AndroidFilesSwitch = Box<dyn Fn(&AndroidFiles) + Send + Sync>;

/// Makes Android's system certificates hold the host's root certificates it names.
type HostCertificatesSwitch = Box<dyn Fn(&HostCertificates) + Send + Sync>;

/// Drives the kernel's su guard for a root elevation config: which uids may see su, on or off.
type RootElevationSwitch = Box<dyn Fn(&RootElevationConfig) + Send + Sync>;

/// Says whether the caller of the current call is the agent. It runs on the caller's thread.
type AgentCheck = Box<dyn Fn(Peer) -> Result<(), String> + Send + Sync>;

impl Router {
    /// A router that hands the host's share of guest frames to `to_host`, the channel the
    /// connection pump drains. `agent_check` says whether a caller is the agent.
    pub fn new(
        to_host: Sender<Vec<u8>>,
        agent_check: impl Fn(Peer) -> Result<(), String> + Send + Sync + 'static,
    ) -> Router {
        Router {
            to_host,
            state: Mutex::new(State::default()),
            agent_check: Box::new(agent_check),
            developer_mode: None,
            file_sharing: None,
            android_files: None,
            host_certificates: None,
            arm_translation: None,
            gapps: None,
            gamepads: None,
            root_elevation: None,
            su_relay: None,
            diagnostics: Mutex::new(None),
            apps_closed: (Mutex::new(0), Condvar::new()),
            closed_packages: Mutex::new(Vec::new()),
        }
    }

    /// Sends a frame of the daemon's own to the host.
    pub fn to_host(&self, frame: Vec<u8>) {
        let _ = self.to_host.send(frame);
    }

    /// How many times an app's last task has ended so far.
    pub fn apps_closed(&self) -> u64 {
        *self.apps_closed.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits until an app's last task has ended since the count was `seen`, or `timeout`
    /// passes, and returns the count then. The idle reclaim waits on it so a closed app's
    /// memory goes back to the host at once rather than at the next periodic look.
    pub fn wait_for_closed_app(&self, seen: u64, timeout: Duration) -> u64 {
        let (count, wake) = &self.apps_closed;
        let guard = count.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = wake
            .wait_timeout_while(guard, timeout, |count| *count == seen)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }

    /// The packages whose last task ended since the last call, oldest first.
    pub fn take_closed_packages(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .closed_packages
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }

    fn app_closed(&self, package: &str) {
        self.closed_packages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(package.to_string());
        let (count, wake) = &self.apps_closed;
        *count.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        wake.notify_all();
    }

    /// Hands the host's developer mode to `switch`: `true` when the user turns it on, `false`
    /// when they turn it off.
    pub fn on_developer_mode(mut self, switch: impl Fn(bool) + Send + Sync + 'static) -> Router {
        self.developer_mode = Some(Box::new(switch));
        self
    }

    /// Hands the host's file sharing to `switch`, which mounts the folders it names.
    pub fn on_file_sharing(
        mut self,
        switch: impl Fn(&FileSharing) + Send + Sync + 'static,
    ) -> Router {
        self.file_sharing = Some(Box::new(switch));
        self
    }

    /// Hands the host's request for the shared storage to `switch`.
    pub fn on_android_files(
        mut self,
        switch: impl Fn(&AndroidFiles) + Send + Sync + 'static,
    ) -> Router {
        self.android_files = Some(Box::new(switch));
        self
    }

    /// Hands the host's root certificates to `switch`, which installs them, before the
    /// listeners hear of them.
    pub fn on_host_certificates(
        mut self,
        switch: impl Fn(&HostCertificates) + Send + Sync + 'static,
    ) -> Router {
        self.host_certificates = Some(Box::new(switch));
        self
    }

    /// Hands the host's ARM translation messages to `package`, and its answers to the host.
    pub fn on_arm_translation(mut self, package: Arc<ArmTranslation>) -> Router {
        self.arm_translation = Some(package);
        self
    }

    /// Hands the host's messages about the Google apps to `gapps`, and its answers to the host.
    pub fn on_gapps(mut self, gapps: Arc<Gapps>) -> Router {
        self.gapps = Some(gapps);
        self
    }

    /// Hands the host's root elevation config to `switch`, which drives the kernel's su guard,
    /// and keeps `relay` so the host's su decisions reach the waiting su calls.
    pub fn on_root_elevation(
        mut self,
        relay: Arc<crate::su_relay::SuRelay>,
        switch: impl Fn(&RootElevationConfig) + Send + Sync + 'static,
    ) -> Router {
        self.su_relay = Some(relay);
        self.root_elevation = Some(Box::new(switch));
        self
    }

    /// Hands the host's game controller messages to `devices`.
    pub fn on_gamepads(mut self, devices: Arc<GamepadDevices>) -> Router {
        self.gamepads = Some(devices);
        self
    }

    /// Acts on the host's game controller message `body`, and says whether it was one. A guest
    /// without gamepad devices drops them: no client of the channel acts on them.
    fn gamepad_message(&self, body: &Body) -> bool {
        let controller = matches!(
            body,
            Body::GamepadConnected(_) | Body::GamepadDisconnected(_) | Body::GamepadState(_)
        );
        if let Some(devices) = &self.gamepads {
            devices.handle(body);
        }
        controller
    }

    /// Answers the host's ARM translation message `body`, and says whether it was one. A guest
    /// without the package's store answers that it has none.
    fn arm_translation_message(&self, body: &Body) -> bool {
        let state = match (body, &self.arm_translation) {
            (_, Some(package)) => match package.handle(body) {
                Some(state) => state,
                None => return false,
            },
            (Body::ArmTranslationRemove(_) | Body::ArmTranslationQuery(_), None) => {
                ArmTranslationState {
                    error: "this guest cannot keep an ARM translation package".into(),
                    ..ArmTranslationState::default()
                }
            }
            _ => return false,
        };
        let _ = self.to_host.send(dsa_protocol::encode(&Envelope {
            body: Some(Body::ArmTranslationState(state)),
        }));
        true
    }

    /// Answers the host's message about the Google apps `body`, and says whether it was one.
    /// A guest without their store answers that it has none.
    fn gapps_message(&self, body: &Body) -> bool {
        let state = match (body, &self.gapps) {
            (_, Some(gapps)) => match gapps.handle(body) {
                Some(state) => state,
                None => return false,
            },
            (Body::GappsRemove(_) | Body::GappsQuery(_), None) => GappsState {
                error: "this guest cannot keep the Google apps".into(),
                ..GappsState::default()
            },
            _ => return false,
        };
        let _ = self.to_host.send(dsa_protocol::encode(&Envelope {
            body: Some(Body::GappsState(state)),
        }));
        true
    }

    /// Answers the host's read of an add-on image an older host left on the user's disk.
    fn addon_image_read(&self, read: &AddonImageRead) {
        let image = read.image();
        let part = match image {
            AddonImage::Gapps => self
                .gapps
                .as_ref()
                .map(|gapps| gapps.read(read.offset, read.length)),
            AddonImage::ArmTranslation => self
                .arm_translation
                .as_ref()
                .map(|package| package.read(read.offset, read.length)),
            AddonImage::Unspecified => None,
        };
        let answer = match part {
            Some(part) => AddonImagePart {
                image: image as i32,
                offset: part.offset,
                data: part.data,
                size: part.size,
                error: part.error,
            },
            None => AddonImagePart {
                image: image as i32,
                offset: read.offset,
                error: "this guest keeps no such add-on image".into(),
                ..AddonImagePart::default()
            },
        };
        let _ = self.to_host.send(dsa_protocol::encode(&Envelope {
            body: Some(Body::AddonImagePart(answer)),
        }));
    }

    fn set_developer_mode(&self, on: bool) {
        if let Some(switch) = &self.developer_mode {
            switch(on);
        }
    }

    /// Whether the agent is connected.
    pub fn agent_connected(&self) -> bool {
        self.lock().agent.is_some()
    }

    /// Sends the diagnostics the agent gathers (`DiagnosticsFile`, `DiagnosticsDone`) to `to`
    /// instead of the host's control channel. Without it the agent's diagnostics are refused.
    pub fn divert_diagnostics(&self, to: Sender<Vec<u8>>) {
        *self.diagnostics.lock().unwrap_or_else(|e| e.into_inner()) = Some(to);
    }

    /// Adds a listener for `peer` and tells it the live tasks other processes reported. It
    /// takes frames until it is reported gone or removed.
    pub fn subscribe(&self, peer: Peer, listener: Arc<dyn Listener>) -> ListenerId {
        let mut state = self.lock();
        state.next_id += 1;
        let id = ListenerId(state.next_id);
        if let Some(frame) = &state.power_state {
            if listener.deliver(frame) == Delivery::Gone {
                return id;
            }
        }
        for task in state.tasks.values() {
            if let Some((sender, frame)) = &task.updated {
                if *sender != peer && listener.deliver(frame) == Delivery::Gone {
                    return id;
                }
            }
        }
        state.listeners.push(Subscription { id, peer, listener });
        id
    }

    /// Removes a listener, as when its process dies. Returns whether it was still there.
    pub fn remove(&self, id: ListenerId) -> bool {
        let mut state = self.lock();
        let before = state.listeners.len();
        state.listeners.retain(|s| s.id != id);
        let removed = state.listeners.len() != before;
        self.tell_host_of_departures(&mut state);
        removed
    }

    /// Android has finished booting. The host is told once.
    pub fn boot_completed(&self) {
        let mut state = self.lock();
        if !state.boot_completed {
            state.boot_completed = true;
            let _ = self.to_host.send(state.boot_report());
        }
    }

    fn tell_host_of_departures(&self, state: &mut State) {
        if state.agent_left() {
            let _ = self.to_host.send(state.boot_report());
        }
        if state.location_left() {
            let _ = self.to_host.send(state.location_wanted());
        }
    }

    /// How many listeners take frames now.
    pub fn listener_count(&self) -> usize {
        self.lock().listeners.len()
    }

    /// How many app tasks the guest has open: those a client has reported and not yet ended.
    pub fn task_count(&self) -> usize {
        self.lock()
            .tasks
            .values()
            .filter(|task| task.updated.is_some())
            .count()
    }

    /// The packages that have an app task open, shown or minimized: their processes are the
    /// user's to see, and their memory is left where it is.
    pub fn task_packages(&self) -> std::collections::BTreeSet<String> {
        self.lock()
            .tasks
            .values()
            .filter(|task| task.updated.is_some() && !task.package.is_empty())
            .map(|task| task.package.clone())
            .collect()
    }

    /// How many app tasks are in use: none while the host shows no window for them, when
    /// they are as idle as if they were closed.
    pub fn tasks_in_use(&self) -> usize {
        if self.lock().screen_off {
            0
        } else {
            self.task_count()
        }
    }

    /// How many of the tasks in use have their window shown on the host: open and not
    /// minimized. Until the host says which windows it shows, every task in use counts as shown.
    pub fn tasks_shown(&self) -> usize {
        let state = self.lock();
        if state.screen_off {
            return 0;
        }
        state
            .tasks
            .iter()
            .filter(|(_, task)| task.updated.is_some())
            .filter(|(id, _)| {
                state
                    .shown_tasks
                    .as_ref()
                    .is_none_or(|shown| shown.contains(id))
            })
            .count()
    }

    /// Whether the host last said it is short of memory.
    pub fn host_memory_low(&self) -> bool {
        self.lock().host_memory_low
    }

    /// Passes a frame from the host to every listener.
    pub fn from_host(&self, frame: &[u8]) {
        let body = dsa_protocol::decode(frame).ok().and_then(|e| e.body);
        if let Some(Body::DeveloperMode(mode)) = body {
            // For the daemon alone: no client of the channel acts on it.
            self.set_developer_mode(mode.on);
            return;
        }
        if let Some(Body::AndroidFiles(files)) = &body {
            // For the daemon alone, like developer mode.
            if let Some(switch) = &self.android_files {
                switch(files);
            }
            return;
        }
        if let Some(Body::FileSharing(sharing)) = &body {
            // The daemon has the folders mounted; the agent hears of it as well.
            if let Some(switch) = &self.file_sharing {
                switch(sharing);
            }
        }
        if let Some(Body::HostCertificates(certificates)) = &body {
            // The daemon installs them first; the agent hears of it as well, and has the
            // running apps look their trust anchors up again.
            if let Some(switch) = &self.host_certificates {
                switch(certificates);
            }
        }
        if let Some(Body::RootElevationConfig(config)) = &body {
            // For the daemon alone: it drives the kernel's su guard.
            if let Some(switch) = &self.root_elevation {
                switch(config);
            }
            return;
        }
        if let Some(Body::SuDecision(decision)) = &body {
            // For the daemon alone: it answers a waiting su call.
            if let Some(relay) = &self.su_relay {
                relay.decide(decision);
            }
            return;
        }
        if let Some(Body::AddonImageRead(read)) = &body {
            // For the daemon alone: the image is on the user's disk, which only it reads.
            self.addon_image_read(read);
            return;
        }
        if let Some(body) = &body {
            // For the daemon alone, like developer mode.
            if self.arm_translation_message(body) {
                return;
            }
            if self.gapps_message(body) {
                return;
            }
            if self.gamepad_message(body) {
                return;
            }
        }
        let mut state = self.lock();
        match body {
            Some(Body::ScreenState(screen)) => state.screen_off = !screen.on,
            Some(Body::HostMemory(memory)) => state.host_memory_low = memory.low,
            Some(Body::PowerState(_)) => state.power_state = Some(frame.to_vec()),
            _ => {}
        }
        state.deliver(frame, None);
        self.tell_host_of_departures(&mut state);
    }

    /// Tells a host that has just connected the state of every live task. Frames that were
    /// waiting for the connection go first, so the host ends with the latest state.
    pub fn host_connected(&self) {
        // A host that connects again gives its controllers anew.
        if let Some(devices) = &self.gamepads {
            devices.reset();
        }
        let mut state = self.lock();
        // The host takes the screen to be on when it connects, and says so if not; it says
        // again if it is short of memory.
        state.screen_off = false;
        state.host_memory_low = false;
        let _ = self.to_host.send(state.boot_report());
        for frame in state.power_modes.values() {
            let _ = self.to_host.send(frame.clone());
        }
        if !state.location_requests.is_empty() {
            let _ = self.to_host.send(state.location_wanted());
        }
        for frame in state.apps.values() {
            let _ = self.to_host.send(frame.clone());
        }
        for task in state.tasks.values() {
            let frames = task
                .updated
                .iter()
                .map(|(_, f)| f)
                .chain(task.frames.iter());
            for frame in frames {
                let _ = self.to_host.send(frame.clone());
            }
        }
    }

    /// Passes a frame a guest client sent: to the host, and task bounds and task ends also
    /// to the listeners of every other process.
    pub fn from_guest(&self, sender: Peer, mut frame: Vec<u8>) -> Result<(), SendError> {
        dsa_transport::check_frame_len(frame.len())
            .map_err(|error| SendError::Malformed(error.to_string()))?;
        let envelope = dsa_protocol::decode(&frame)
            .map_err(|error| SendError::Malformed(error.to_string()))?;
        let mut state = self.lock();
        match envelope.body {
            None => {
                return Err(SendError::NotAllowed(
                    "the frame carries no message".to_string(),
                ))
            }
            Some(
                Body::Hello(_)
                | Body::HelloAccepted(_)
                | Body::HelloRejected(_)
                | Body::AuthChallenge(_)
                | Body::AuthProof(_),
            ) => {
                return Err(SendError::NotAllowed(
                    "the handshake is the daemon's own; a guest client cannot send its messages"
                        .to_string(),
                ))
            }
            Some(Body::GuestBoot(_)) => {
                return Err(SendError::NotAllowed(
                    "how far the guest has got is the daemon's to say".to_string(),
                ))
            }
            Some(Body::GappsState(_)) => {
                return Err(SendError::NotAllowed(
                    "where the Google apps stand is the daemon's to say".to_string(),
                ))
            }
            Some(Body::ArmTranslationState(_)) => {
                return Err(SendError::NotAllowed(
                    "where ARM translation stands is the daemon's to say".to_string(),
                ))
            }
            Some(Body::AddonImagePart(_)) => {
                return Err(SendError::NotAllowed(
                    "an add-on image on the user's disk is the daemon's to send".to_string(),
                ))
            }
            Some(Body::GoogleDeviceId(_)) => {
                (self.agent_check)(sender).map_err(|why| {
                    SendError::NotAllowed(format!(
                        "only the agent says the Google Services Framework Android ID: {why}"
                    ))
                })?;
            }
            // The host stores what the backup transport sends as the user's app data, and
            // trusts a passkey request to name the app Android's credential manager says
            // called; both are the agent's alone.
            Some(
                Body::PasskeyRequest(_)
                | Body::BackupUpload(_)
                | Body::BackupCommit(_)
                | Body::BackupDiscard(_)
                | Body::BackupClear(_)
                | Body::BackupQuery(_)
                | Body::BackupFetch(_)
                | Body::BackupRunResult(_),
            ) => {
                (self.agent_check)(sender).map_err(|why| {
                    SendError::NotAllowed(format!(
                        "only the agent speaks for the guest's backups and passkeys: {why}"
                    ))
                })?;
            }
            Some(Body::KeymintRootKeyRequest(_) | Body::KeymintRootKey(_)) => {
                return Err(SendError::NotAllowed(
                    "KeyMint's root key is asked for on a port of its own, by the daemon alone"
                        .to_string(),
                ))
            }
            Some(Body::DnsQuery(_) | Body::DnsAnswer(_)) => {
                return Err(SendError::NotAllowed(
                    "DNS queries go to the host on a port of their own, by the daemon alone"
                        .to_string(),
                ))
            }
            Some(Body::AgentConnected(_)) => {
                if !state.listeners.iter().any(|s| s.peer == sender) {
                    return Err(SendError::NotAllowed(
                        "an agent subscribes before it says it is connected".to_string(),
                    ));
                }
                (self.agent_check)(sender).map_err(|why| {
                    SendError::NotAllowed(format!("only the agent may say it is connected: {why}"))
                })?;
                if state.agent != Some(sender) {
                    state.agent = Some(sender);
                    let _ = self.to_host.send(state.boot_report());
                }
                return Ok(());
            }
            Some(Body::TaskUpdated(updated)) => {
                let task = state.tasks.entry(updated.task_id).or_default();
                task.updated = Some((sender, frame.clone()));
                task.package = updated.package_name;
                state.deliver(&frame, Some(sender));
            }
            Some(Body::TaskRemoved(removed)) => {
                if let Some(task) = state.tasks.remove(&removed.task_id) {
                    if !state.tasks.values().any(|t| t.package == task.package) {
                        state.apps.remove(&task.package);
                        self.app_closed(&task.package);
                    }
                }
                state.deliver(&frame, Some(sender));
            }
            Some(Body::AppInfo(app)) => {
                if state.apps.contains_key(&app.package_name) || state.apps.len() < MAX_APPS {
                    state.apps.insert(app.package_name, frame.clone());
                }
            }
            Some(Body::TaskFrames(frames)) => {
                state.tasks.entry(frames.task_id).or_default().frames = Some(frame.clone());
            }
            Some(Body::DiagnosticsFile(_) | Body::DiagnosticsDone(_)) => {
                // The agent's part of the diagnostics goes on the log port, never on the
                // control channel, so a large dump cannot hold up input.
                (self.agent_check)(sender).map_err(|why| {
                    SendError::NotAllowed(format!("only the agent sends diagnostics: {why}"))
                })?;
                let diverted = self
                    .diagnostics
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map(|to| to.send(frame).is_ok())
                    .unwrap_or(false);
                if !diverted {
                    return Err(SendError::NotAllowed(
                        "no diagnostics are being gathered".to_string(),
                    ));
                }
                return Ok(());
            }
            Some(
                Body::LogStream(_)
                | Body::LogDump(_)
                | Body::LogRecords(_)
                | Body::CollectDiagnostics(_),
            ) => {
                return Err(SendError::NotAllowed(
                    "the guest's log is the daemon's to send".to_string(),
                ))
            }
            Some(Body::LocationRequest(request)) => {
                // Kept only for a process that listens, so its death can be seen.
                let listens = state.listeners.iter().any(|s| s.peer == sender);
                if request.active && listens {
                    state
                        .location_requests
                        .insert(sender.0, request.interval_ms);
                } else {
                    state.location_requests.remove(&sender.0);
                }
                // The host hears what every process wants together, not this one alone.
                frame = state.location_wanted();
            }
            Some(Body::PowerHint(hint)) if hint.kind == PowerHintKind::Mode as i32 => {
                if !hint.enabled {
                    state.power_modes.remove(&hint.id);
                } else if state.power_modes.contains_key(&hint.id)
                    || state.power_modes.len() < MAX_POWER_MODES
                {
                    state.power_modes.insert(hint.id, frame.clone());
                }
            }
            Some(_) => {}
        }
        self.tell_host_of_departures(&mut state);
        // Sent with the state locked, so the host gets the frames in the order the state
        // changed. The pump owns the other end for as long as the daemon runs; if it has
        // gone, the daemon is stopping and the frame has nowhere to go.
        let _ = self.to_host.send(frame);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves the state itself whole; keep routing.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
