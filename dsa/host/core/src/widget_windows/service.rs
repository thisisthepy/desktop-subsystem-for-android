//! The widget windows and the service: what the window manager asks of the windows, what they
//! report back, and the watch that ties them to the service's widgets ([`sync`]).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_protocol::host_api::types::PointerAction;
use dsa_transport::Connector;

use super::geometry::{android_density, placement_of, ScreenRect};
use crate::host_api::HostApiClient;
use crate::session::SessionState;
use crate::widgets::{Placement, Widget, WidgetEvent, WidgetFrame};

/// Where the watch writes what it has to say: the window manager's log.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);

/// How long to wait before connecting again after the service could not be reached.
const RETRY: Duration = Duration::from_secs(1);

/// How long the watch waits for the service before it passes on what the windows reported.
const POLL: Duration = Duration::from_millis(15);

/// What the window manager asks of the widget windows.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowCommand {
    /// Open the widget's window, or move it to its placement.
    Place(Widget),
    /// Show what the widget shows now.
    Frame(WidgetFrame),
    /// Close the widget's window.
    Remove(i32),
}

/// What the widget windows report to the window manager.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowReport {
    /// The window is at `placement`, `width` by `height` pixels on a monitor where Android
    /// draws at `density_dpi` ([`android_density`]).
    Placed {
        widget_id: i32,
        placement: Placement,
        width: u32,
        height: u32,
        density_dpi: u32,
    },
    /// Pointer input in the widget, in its frame's pixels.
    Pointer {
        widget_id: i32,
        action: PointerAction,
        x: f32,
        y: f32,
    },
    /// The user chose Remove in the window's menu.
    Remove { widget_id: i32 },
    /// The user chose Widget settings in the window's menu: the widget's setup opens again.
    Reconfigure { widget_id: i32 },
}

/// What a widget window reports when its widget `widget_id` sits at `widget` on the screen
/// (left, top, right and bottom; its own rectangle, without the room of an edit) on a monitor
/// of `dpi`: where it is and how large, in pixels and in dp at Android's density there. The
/// service keeps it and tells Android, which draws the widget at that size.
pub fn placed_report(widget_id: i32, widget: ScreenRect, dpi: u32) -> WindowReport {
    let (width, height) = ((widget.2 - widget.0).max(1), (widget.3 - widget.1).max(1));
    WindowReport::Placed {
        widget_id,
        placement: placement_of(widget.0, widget.1, width, height, dpi),
        width: width as u32,
        height: height as u32,
        density_dpi: android_density(dpi),
    }
}

/// The widget windows, as the watch drives them: a window manager's are windows on a thread
/// of their own; a test's records what it is asked.
pub trait WidgetWindows {
    fn command(&mut self, command: WindowCommand);
    /// What the windows reported since the last call.
    fn reports(&mut self) -> Vec<WindowReport>;
}

/// The command a change to the widgets is for the windows.
pub fn command_of(event: WidgetEvent) -> Option<WindowCommand> {
    match event {
        WidgetEvent::Placed(widget) => Some(WindowCommand::Place(widget)),
        WidgetEvent::Frame(frame) => Some(WindowCommand::Frame(frame)),
        WidgetEvent::Removed { widget_id } => Some(WindowCommand::Remove(widget_id)),
        WidgetEvent::Listed(_) => None,
    }
}

/// Moves still to send are dropped when a newer one of the same widget follows, so a slow
/// service never makes a drag lag behind.
pub fn coalesce(reports: Vec<WindowReport>) -> Vec<WindowReport> {
    let mut out: Vec<WindowReport> = Vec::with_capacity(reports.len());
    for report in reports {
        if let WindowReport::Pointer {
            widget_id,
            action: PointerAction::Move,
            ..
        } = &report
        {
            if let Some(WindowReport::Pointer {
                widget_id: last,
                action: PointerAction::Move,
                ..
            }) = out.last()
            {
                if last == widget_id {
                    out.pop();
                }
            }
        }
        out.push(report);
    }
    out
}

/// Whether the window manager starts Android at sign-in, seeing it in `state`: when it is not
/// running. One that is starting, running or dozing is left as it is.
pub fn starts_android_at_sign_in(state: SessionState) -> bool {
    state == SessionState::Stopped
}

/// Asks the service, through a connection of its own, to open the setup of widget
/// `widget_id` again, and logs how it ended. It waits for the user, who may take minutes, so
/// it runs on a thread of its own and nothing else waits for it.
pub fn reconfigure(mut client: HostApiClient, widget_id: i32, log: &dyn Fn(&str)) {
    log(&format!(
        "opening the settings of widget {widget_id} from its menu"
    ));
    let wait = crate::host_api::WIDGET_ADD_WAIT + Duration::from_secs(10);
    match client.reconfigure_widget(widget_id, wait) {
        Ok(_) => log(&format!("widget {widget_id} was set up again")),
        Err(error) => log(&format!("widget {widget_id} was not set up again: {error}")),
    }
}

/// Passes what the windows reported to the service through `client`. A report the service
/// refuses is logged; the rest go on. A widget's settings go to `settings`, which opens them
/// without holding up the reports after it.
fn send_reports(
    client: &mut HostApiClient,
    reports: Vec<WindowReport>,
    log: &dyn Fn(&str),
    settings: &mut dyn FnMut(i32),
) -> Result<(), String> {
    for report in coalesce(reports) {
        let result = match report {
            WindowReport::Placed {
                widget_id,
                placement,
                width,
                height,
                density_dpi,
            } => client.place_widget(widget_id, placement, width, height, density_dpi),
            WindowReport::Pointer {
                widget_id,
                action,
                x,
                y,
            } => client.widget_pointer(widget_id, action, 0, x, y),
            WindowReport::Remove { widget_id } => {
                log(&format!("removing widget {widget_id} from its menu"));
                client.remove_widget(widget_id)
            }
            WindowReport::Reconfigure { widget_id } => {
                settings(widget_id);
                Ok(())
            }
        };
        match result {
            Ok(()) => {}
            Err(crate::host_api::ClientError::Service { detail, .. }) => {
                log(&format!("a widget's window: {detail}"));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// Shows the widgets of the user's Android until `stop`, reconnecting to the service
/// whenever the connection ends. `build` is what the window manager calls itself to the
/// service, and `log` is its log.
pub fn sync(
    mut connector: Box<dyn Connector>,
    windows: &mut dyn WidgetWindows,
    stop: Arc<AtomicBool>,
    build: &str,
    log: Log,
) {
    let mut last_error = String::new();
    let mut started_android = false;
    let mut shown: HashSet<i32> = HashSet::new();
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(&mut *connector, build, ANSWER)
            .and_then(|client| client.watch_widgets());
        let requests = HostApiClient::connect(&mut *connector, build, ANSWER);
        let (mut stream, mut requests) = match (stream, requests) {
            (Ok(stream), Ok(requests)) => (stream, requests),
            (Err(error), _) | (_, Err(error)) => {
                let error = error.to_string();
                if error != last_error {
                    log(&format!("widgets cannot reach the service: {error}"));
                    last_error = error;
                }
                thread::sleep(RETRY);
                continue;
            }
        };
        last_error.clear();
        // Android starts with the user's sign-in, so no app launch waits for it to boot, and
        // the widgets the service remembers come back once it runs.
        if !started_android {
            started_android = true;
            match requests.status() {
                Ok(status) if starts_android_at_sign_in(status.state) => {
                    log("starting Android at sign-in");
                    if let Err(why) = requests.start() {
                        log(&format!("Android did not start at sign-in: {why}"));
                    }
                }
                Ok(_) => {}
                Err(why) => log(&format!("widgets: {why}")),
            }
        }
        // The windows of widgets that went while the connection was down close.
        let current: Vec<i32> = stream.current().iter().map(|w| w.widget_id).collect();
        for gone in shown
            .iter()
            .copied()
            .filter(|id| !current.contains(id))
            .collect::<Vec<_>>()
        {
            shown.remove(&gone);
            windows.command(WindowCommand::Remove(gone));
        }
        for widget in stream.current().to_vec() {
            shown.insert(widget.widget_id);
            windows.command(WindowCommand::Place(widget));
        }
        while !stop.load(Ordering::Relaxed) {
            let mut settings = |widget_id: i32| {
                match HostApiClient::connect(&mut *connector, build, ANSWER) {
                    Ok(client) => {
                        let thread_log = Arc::clone(&log);
                        let spawned = thread::Builder::new()
                            .name("dsa-wm-widget-settings".into())
                            .spawn(move || reconfigure(client, widget_id, &*thread_log));
                        if let Err(why) = spawned {
                            log(&format!(
                                "the settings of widget {widget_id} did not open: {why}"
                            ));
                        }
                    }
                    Err(error) => log(&format!(
                        "the settings of widget {widget_id} did not open: the service cannot be reached: {error}"
                    )),
                }
            };
            let reports = windows.reports();
            if let Err(error) = send_reports(&mut requests, reports, &*log, &mut settings) {
                log(&format!(
                    "a widget's window did not reach the service: {error}"
                ));
                break;
            }
            match stream.next(POLL) {
                Ok(Some(event)) => {
                    match &event {
                        WidgetEvent::Placed(widget) => {
                            shown.insert(widget.widget_id);
                        }
                        WidgetEvent::Removed { widget_id } => {
                            shown.remove(widget_id);
                        }
                        _ => {}
                    }
                    if let Some(command) = command_of(event) {
                        windows.command(command);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    log(&format!(
                        "the widgets' connection to the service ended: {error}"
                    ));
                    break;
                }
            }
        }
    }
    for id in shown {
        windows.command(WindowCommand::Remove(id));
    }
}

/// The widget windows on the far side of two channels: the watch sends commands into one and
/// takes reports out of the other, and `wake` tells the windows' thread that a command waits.
/// The Linux and macOS window managers run their widget windows on their main thread so.
pub struct ChannelWidgetWindows {
    pub commands: std::sync::mpsc::Sender<WindowCommand>,
    pub reports: std::sync::mpsc::Receiver<WindowReport>,
    pub wake: Box<dyn Fn() + Send>,
}

impl WidgetWindows for ChannelWidgetWindows {
    fn command(&mut self, command: WindowCommand) {
        if self.commands.send(command).is_ok() {
            (self.wake)();
        }
    }

    fn reports(&mut self) -> Vec<WindowReport> {
        self.reports.try_iter().collect()
    }
}
