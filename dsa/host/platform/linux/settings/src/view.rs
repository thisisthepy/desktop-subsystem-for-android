//! What the pages show, worked out from what the service reports. Nothing here touches GTK,
//! so it is tested without a display.

use std::collections::VecDeque;

use crate::api::{Failure, Problem};
use crate::i18n::{tr, trf};
use crate::model::{
    AdbAutoConnectState, AndroidVersion, App, AppWindow, ArmTranslation, Backup, BackupJob,
    DnsResolver, DpiScaling, Gapps, GappsState, GuestMemory, HostRole, ImportedCertificate,
    KeyProtection, KeyProtectionShown, Memory, MemoryShare, Network, NetworkMode, Outcome,
    ProgramUpdateAnswer, ProgramUpdatePhase, State, Status, ALL_FOLDERS,
};

/// The dsa commands the Advanced page's command line group shows, each on its own row with a
/// copy button that copies only that command. They are separate commands, not a pipeline, and
/// they are typed as they are in every language.
pub const COMMAND_LINE_COMMANDS: [&str; 3] = ["dsa status", "dsa settings", "dsa memory"];

/// A problem in the user's language, with what the client said after it.
pub fn problem_text(failure: &Failure) -> String {
    let words = match failure.problem {
        Problem::CliMissing => tr("The dsa command could not be started. Reinstall DSA."),
        Problem::ServiceUnreachable => tr("The DSA service is not running."),
        Problem::Refused => tr("The DSA service could not do this."),
        Problem::Unreadable => {
            tr("The answer of the dsa command could not be read. Reinstall DSA.")
        }
        Problem::NotAuthorized => {
            tr("The setting was not changed, because an administrator's permission was not given.")
        }
    };
    if failure.detail.is_empty() || failure.problem == Problem::NotAuthorized {
        words
    } else {
        format!("{words}\n{}", failure.detail)
    }
}

/// Where attaching developer mode's port to the adb server stands, in the user's language.
pub fn adb_auto_connect_text(state: AdbAutoConnectState) -> String {
    tr(match state {
        AdbAutoConnectState::Off => {
            "Not connecting: developer mode or automatic connection is off."
        }
        AdbAutoConnectState::WaitingForServer => {
            "Waiting for adb: Android connects when adb starts on this computer."
        }
        AdbAutoConnectState::WaitingForAndroid => {
            "Waiting for Android: it connects once Android is running."
        }
        AdbAutoConnectState::Attached => "Connected: adb lists Android.",
        AdbAutoConnectState::Detached => {
            "Disconnected with adb disconnect. Android connects again when the adb server restarts."
        }
    })
}

pub fn state_text(state: State) -> String {
    tr(match state {
        State::Unknown => "Unknown",
        State::Stopped => "Stopped",
        State::Starting => "Starting",
        State::Active => "Running",
        State::Dozing => "Dozing (idle)",
        State::Frozen => "Frozen",
        State::Stopping => "Stopping",
        State::Stalled => "Not ready",
    })
}

/// Which of the General page's buttons can be used in a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controls {
    pub can_start: bool,
    pub can_stop: bool,
    pub can_freeze: bool,
}

pub fn controls(status: Option<&Status>) -> Controls {
    let state = status.map(|s| s.state).unwrap_or(State::Unknown);
    Controls {
        can_start: matches!(state, State::Stopped | State::Stalled),
        can_stop: matches!(
            state,
            State::Starting | State::Active | State::Dozing | State::Frozen | State::Stalled
        ),
        can_freeze: matches!(state, State::Active | State::Dozing),
    }
}

/// A size in KiB as people read it: `512 KB`, `340 MB`, `2.4 GB`.
pub fn size_kb(kb: u64) -> String {
    const MB: u64 = 1024;
    const GB: u64 = 1024 * 1024;
    if kb >= GB {
        format!("{:.1} GB", kb as f64 / GB as f64)
    } else if kb >= MB {
        format!("{} MB", (kb + MB / 2) / MB)
    } else {
        format!("{kb} KB")
    }
}

/// A size in bytes, as [`size_kb`] shows sizes.
pub fn size_bytes(bytes: u64) -> String {
    size_kb(bytes.div_ceil(1024))
}

/// One line of the memory page: what it is, a detail, and its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRow {
    pub label: String,
    pub detail: String,
    pub size: String,
}

fn row(label: String, detail: String, kb: u64) -> MemoryRow {
    MemoryRow {
        label,
        detail,
        size: size_kb(kb),
    }
}

pub fn host_rows(memory: &Memory) -> Vec<MemoryRow> {
    memory
        .host
        .iter()
        .map(|p| {
            let label = tr(match p.role {
                HostRole::Vm => "Virtual machine (crosvm)",
                HostRole::WindowManager => "Window manager",
                HostRole::Renderer => "Graphics renderer",
                HostRole::AppWindow => "App window",
            });
            let detail = trf(
                "Process {0}, {1} private",
                &[&p.pid.to_string(), &size_kb(p.private_kb)],
            );
            row(label, detail, p.working_set_kb)
        })
        .collect()
}

/// Android's breakdown: the apps and the system, the caches, the kernel's reclaimable caches,
/// shared memory, compressed memory and what is free.
pub fn guest_rows(guest: &GuestMemory) -> Vec<MemoryRow> {
    // What the processes cost: in memory, plus their compressed share of zram. A service that
    // does not say counts their PSS as in memory.
    let apps = guest.apps.unwrap_or(MemoryShare {
        resident_kb: guest.total_pss_kb,
        compressed_kb: Some(0),
    });
    let mut rows = vec![
        row(tr("Apps and system"), share_detail(&apps), apps.total_kb()),
        row(
            tr("Cache"),
            tr("File contents kept in memory. Android drops them when it needs the memory."),
            guest.cached_kb + guest.buffers_kb,
        ),
        row(
            tr("Kernel caches"),
            tr("Kernel caches the kernel can free on demand."),
            guest.slab_reclaimable_kb,
        ),
        row(tr("Shared memory"), String::new(), guest.shmem_kb),
        row(
            tr("Compressed memory"),
            trf(
                "Holds {0} of pages, compressed",
                &[&size_kb(guest.zram_original_kb)],
            ),
            guest.zram_used_kb,
        ),
        row(tr("Free"), String::new(), guest.free_kb),
    ];
    // Google Play services, which the user chose to install, on a row of its own, so what it
    // costs is not taken for what Android and DSA cost.
    if let Some(play) = &guest.google_play {
        rows.insert(
            1,
            row(
                tr("Google Play services"),
                trf(
                    "The Google apps you installed, {0} processes: {1}",
                    &[&play.processes.to_string(), &share_detail(&play.cost)],
                ),
                play.cost.total_kb(),
            ),
        );
    }
    rows
}

/// "N in memory, M compressed", the compressed part unknown when zram could not measure it.
fn share_detail(share: &MemoryShare) -> String {
    let compressed = share.compressed_kb.map_or_else(|| tr("unknown"), size_kb);
    trf(
        "{0} in memory, {1} compressed",
        &[&size_kb(share.resident_kb), &compressed],
    )
}

pub fn kernel_rows(guest: &GuestMemory) -> Vec<MemoryRow> {
    let Some(k) = &guest.kernel else {
        return Vec::new();
    };
    let mut rows = vec![
        row(
            tr("Kernel objects"),
            tr("Memory the kernel keeps for its own data and cannot reclaim"),
            k.slab_unreclaimable_kb,
        ),
        row(tr("Thread stacks"), String::new(), k.kernel_stack_kb),
        row(tr("Page tables"), String::new(), k.page_tables_kb),
        row(
            tr("Kernel mappings (vmalloc)"),
            String::new(),
            k.vmalloc_used_kb,
        ),
        row(tr("Per-CPU data"), String::new(), k.percpu_kb),
        row(
            tr("Drivers"),
            tr("What no counter names, such as GPU and DMA buffers"),
            k.unaccounted_kb,
        ),
    ];
    if let Some(reserved) = k.reserved_at_boot_kb {
        rows.push(row(
            tr("Reserved at boot"),
            trf(
                "Kernel code, page structures and buffers, of {0} given; contiguous area {1}",
                &[&size_kb(k.mem_online_kb), &size_kb(k.cma_total_kb)],
            ),
            reserved,
        ));
    }
    rows
}

pub fn process_rows(guest: &GuestMemory) -> Vec<MemoryRow> {
    guest
        .top_processes
        .iter()
        .map(|p| {
            let detail = if p.google_play {
                trf("Process {0}, Google Play services", &[&p.pid.to_string()])
            } else {
                trf("Process {0}", &[&p.pid.to_string()])
            };
            row(
                p.name.clone(),
                detail,
                p.cost.map_or(p.pss_kb, |cost| cost.total_kb()),
            )
        })
        .collect()
}

/// What the VM holds on the host now, or `None` without a VM.
pub fn vm_size_kb(memory: &Memory) -> Option<u64> {
    let vm: Vec<_> = memory
        .host
        .iter()
        .filter(|p| p.role == HostRole::Vm)
        .collect();
    (!vm.is_empty()).then(|| vm.iter().map(|p| p.working_set_kb).sum())
}

/// How full the guest's memory is, from 0 to 1.
pub fn guest_used_fraction(guest: &GuestMemory) -> f64 {
    if guest.total_kb == 0 {
        return 0.0;
    }
    (guest.total_kb - guest.available_kb.min(guest.total_kb)) as f64 / guest.total_kb as f64
}

/// The smallest memory limit the service takes, in MiB.
pub const SMALLEST_MEMORY_LIMIT_MB: u32 = 2048;
const OFFERED_LIMITS_MB: [u32; 7] = [2048, 3072, 4096, 6144, 8192, 12288, 16384];

/// The memory limits to choose from, automatic (0) first, with the current one among them
/// even when it was set another way.
pub fn memory_limit_choices(current_mb: u32) -> Vec<(String, u32)> {
    let mut limits: Vec<u32> = OFFERED_LIMITS_MB.to_vec();
    if current_mb != 0 && !limits.contains(&current_mb) {
        limits.push(current_mb);
        limits.sort_unstable();
    }
    let mut choices = vec![(tr("Automatic"), 0)];
    choices.extend(
        limits
            .into_iter()
            .map(|mb| (size_kb(u64::from(mb) * 1024), mb)),
    );
    choices
}

/// The render GPU choices, as `settings set render-gpu` takes them, and their labels.
pub fn render_gpu_choices() -> Vec<(&'static str, String)> {
    vec![
        (
            "display",
            tr("The GPU of the primary monitor (recommended)"),
        ),
        ("integrated", tr("Integrated GPU")),
        ("discrete", tr("Discrete GPU (for example, for games)")),
    ]
}

/// Where the render GPU setting is among the choices; another name is shown as itself after
/// them.
pub fn render_gpu_index(setting: &str) -> usize {
    let setting = setting.to_ascii_lowercase();
    render_gpu_choices()
        .iter()
        .position(|(value, _)| *value == setting || setting.is_empty() && *value == "display")
        .unwrap_or(render_gpu_choices().len())
}

/// One choice on the Android version page: always the newest, or one major version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidChoice {
    pub channel: String,
    pub major: i64,
    pub label: String,
}

/// How moving to a choice treats the user's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndroidMove {
    Same,
    /// A newer major: apps and their data move along.
    Up,
    /// An older major: every user's Android data is erased.
    Down,
}

pub fn android_choices(info: &AndroidVersion) -> Vec<AndroidChoice> {
    let newest = info.supported.iter().copied().max().unwrap_or(info.major);
    let mut choices = vec![AndroidChoice {
        channel: "latest".into(),
        major: newest,
        label: trf(
            "Always the newest (now Android {0})",
            &[&newest.to_string()],
        ),
    }];
    let mut majors = info.supported.clone();
    majors.sort_unstable_by(|a, b| b.cmp(a));
    choices.extend(majors.into_iter().map(|major| AndroidChoice {
        channel: major.to_string(),
        major,
        label: trf("Android {0}", &[&major.to_string()]),
    }));
    choices
}

pub fn android_choice_index(info: &AndroidVersion) -> usize {
    android_choices(info)
        .iter()
        .position(|c| c.channel == info.channel)
        .unwrap_or(0)
}

/// The version the image updates follow: the newest supported one, or one major version and
/// its security updates.
pub fn android_channel_text(info: &AndroidVersion) -> String {
    if info.channel == "latest" {
        let newest = info.supported.iter().copied().max().unwrap_or(info.major);
        trf(
            "The newest supported version (now Android {0})",
            &[&newest.to_string()],
        )
    } else {
        trf("Android {0} and its security updates", &[&info.channel])
    }
}

/// How far an image download has got, from the service's notice ("downloading Android
/// ...: 45% ..."), or `None` when nothing is downloading.
pub fn android_download_percent(notice: Option<&str>) -> Option<u8> {
    let rest = notice?.strip_prefix("downloading Android ")?; // not translated
    let (_, after) = rest.split_once(": ")?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !after[digits.len()..].starts_with('%') {
        return None;
    }
    digits.parse::<u32>().ok().map(|p| p.min(100) as u8)
}

/// Where the Android image update stands, in one line: a download with its progress, a
/// downloaded image waiting for Android to restart, the service's news (what was applied), a
/// version waiting for the next start, or that none is waiting.
pub fn android_update_text(info: &AndroidVersion) -> String {
    if android_download_percent(info.notice.as_deref()).is_some() {
        return info.notice.clone().unwrap_or_default();
    }
    if let Some(version) = &info.restart_pending {
        return trf(
            "Android {0} is downloaded. Restart Android to use it now.",
            &[version.as_str()],
        );
    }
    match &info.notice {
        Some(notice) if !notice.is_empty() => notice.clone(),
        _ if info.target_major != info.major => trf(
            "Android {0} is chosen. It is downloaded now and used the next time Android starts.",
            &[&info.target_major.to_string()],
        ),
        _ => tr("No Android update is waiting. DSA checks every day, and restarts Android into an update once no app window is open."),
    }
}

/// The channel to choose again so that the service checks for an image now: the one it
/// already follows, which changes nothing else.
pub fn android_check_channel(info: &AndroidVersion) -> String {
    info.channel.clone()
}

pub fn android_move(info: &AndroidVersion, choice: &AndroidChoice) -> AndroidMove {
    if choice.major == info.major {
        AndroidMove::Same
    } else if choice.major > info.major {
        AndroidMove::Up
    } else {
        AndroidMove::Down
    }
}

/// The apps a search matches, by name: the query is matched against the label and the
/// package, ignoring case. Apps that share a user ID are one group: the group is shown
/// together, in the place of its first app by name, when any of its apps matches.
pub fn filter_apps<'a>(apps: &'a [App], query: &str) -> Vec<&'a App> {
    let query = query.trim().to_lowercase();
    let matches = |a: &App| {
        query.is_empty()
            || a.label.to_lowercase().contains(&query)
            || a.package.to_lowercase().contains(&query)
    };
    let mut shown: Vec<&App> = apps
        .iter()
        .filter(|a| matches(a) || shared_with(a, apps).into_iter().any(matches))
        .collect();
    shown.sort_by_key(|a| {
        let first = std::iter::once(*a)
            .chain(shared_with(a, apps))
            .map(|member| member.label.to_lowercase())
            .min()
            .unwrap_or_default();
        (first, a.shared_user_id.clone(), a.label.to_lowercase())
    });
    shown
}

/// The other apps that share `app`'s user ID among `apps`, by name.
pub fn shared_with<'a>(app: &App, apps: &'a [App]) -> Vec<&'a App> {
    if app.shared_user_id.is_empty() {
        return Vec::new();
    }
    let mut others: Vec<&App> = apps
        .iter()
        .filter(|other| other.shared_user_id == app.shared_user_id && other.package != app.package)
        .collect();
    others.sort_by_key(|other| other.label.to_lowercase());
    others
}

/// What the Apps page says under an app's root level when it shares its user ID with other
/// apps; `None` for an app with a user ID of its own.
pub fn root_group_note(app: &App, apps: &[App]) -> Option<String> {
    let others = shared_with(app, apps);
    if others.is_empty() {
        return None;
    }
    let names = others
        .iter()
        .map(|other| other.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Some(trf(
        "Shares its user ID with {0}. Android cannot tell these apps apart, so their root access is one setting: changing it here changes it for all of them, and if any of them is set to Not allowed, none of them can use su.",
        &[&names],
    ))
}

pub fn apps_count(total: usize, shown: usize, searching: bool) -> String {
    if searching {
        trf("{0} of {1} apps", &[&shown.to_string(), &total.to_string()])
    } else {
        trf("{0} apps", &[&total.to_string()])
    }
}

/// What the row of an app says under its name.
pub fn app_detail(app: &App) -> String {
    let mut parts = Vec::new();
    if !app.version_name.is_empty() {
        parts.push(trf("Version {0}", &[&app.version_name]));
    }
    if app.total_bytes > 0 {
        parts.push(size_bytes(app.total_bytes));
    }
    if app.system_app {
        parts.push(tr("Part of Android"));
    }
    parts.join(" · ")
}

/// The sizes an app's windows can open at: Android's choice first, then phones and tablets.
pub fn start_size_choices() -> Vec<(String, Option<(u32, u32)>)> {
    let size =
        |key: &str, w: u32, h: u32| (trf(key, &[&w.to_string(), &h.to_string()]), Some((w, h)));
    vec![
        (tr("Opens at the size Android chooses"), None),
        size("Phone, portrait ({0} × {1})", 412, 915),
        size("Phone, landscape ({0} × {1})", 915, 412),
        size("Tablet, portrait ({0} × {1})", 800, 1280),
        size("Tablet, landscape ({0} × {1})", 1280, 800),
    ]
}

/// Where an app's start size is among [`start_size_choices`]; `None` for a size set another
/// way (with `dsa app-window`).
pub fn start_size_index(window: Option<&AppWindow>) -> Option<usize> {
    let size = window.and_then(|w| w.start_size);
    start_size_choices().iter().position(|(_, s)| *s == size)
}

/// The ways an app's window can follow a monitor of another scale, in the order of
/// [`DpiScaling::ALL`]. System (forced) is stored and can be chosen, but while Android cannot
/// do it (`system_forced_available` is false), its label says it is not available yet.
pub fn dpi_scaling_choices(system_forced_available: bool) -> Vec<(String, DpiScaling)> {
    DpiScaling::ALL
        .into_iter()
        .map(|scaling| {
            let label = match scaling {
                DpiScaling::Auto => tr("Automatic"),
                DpiScaling::App => tr("App"),
                DpiScaling::System => tr("System"),
                DpiScaling::SystemForced if system_forced_available => tr("System (forced)"),
                DpiScaling::SystemForced => tr("System (forced), not available yet"),
            };
            (label, scaling)
        })
        .collect()
}

/// Where an app's scaling choice is among [`dpi_scaling_choices`].
pub fn dpi_scaling_index(window: Option<&AppWindow>) -> usize {
    let scaling = window.map(|w| w.dpi_scaling).unwrap_or_default();
    DpiScaling::ALL
        .iter()
        .position(|s| *s == scaling)
        .unwrap_or(0)
}

/// What the scaling row says under its title: why System (forced) works like System for now,
/// while it is the app's choice and not available; otherwise what the choices do.
pub fn dpi_scaling_subtitle(window: Option<&AppWindow>, system_forced_available: bool) -> String {
    let scaling = window.map(|w| w.dpi_scaling).unwrap_or_default();
    if scaling == DpiScaling::SystemForced && !system_forced_available {
        tr("System (forced) gives the app the new scale without restarting it, even if it does not say it handles one. It is not available yet: it needs a change to Android's framework that this build does not have, so until then it works like System.")
    } else {
        tr("App: the app redraws at the new scale, and text stays sharp; for an app that does not say it handles this, Android recreates its screen and restores its state, so it may redraw briefly when it moves. System: the app keeps its scale and DSA scales its window to the same physical size; text can look a little blurry. Automatic picks App for apps that say they handle it, and System for the others. Automatic and System never restart the app.")
    }
}

/// What an app's "Run as ARM" switch shows: whether it is on, whether it can be changed, and
/// what its row says under its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAsArmView {
    pub on: bool,
    pub sensitive: bool,
    pub subtitle: String,
}

/// The "Run as ARM" switch of `app`: off and insensitive, with the reason, when it cannot be
/// changed.
pub fn run_as_arm_view(app: &App) -> RunAsArmView {
    let arm = &app.run_as_arm;
    let subtitle = if arm.available {
        tr("Runs the app's ARM code through the translator. Off runs its x86_64 code.")
    } else {
        tr(match arm.reason.as_str() {
            "system-app" => "Apps that are part of Android run as Android installed them.",
            "no-arm-libraries" => "No APK of this app on the device holds its ARM code.",
            "translation-off" => {
                "ARM translation is not on. Turn it on under ARM apps and restart Android."
            }
            "arm-only" => "This app has only ARM code, so it always runs as ARM.",
            _ => "This version of DSA cannot say whether the app can run as ARM.",
        })
    };
    RunAsArmView {
        on: arm.on,
        sensitive: arm.available,
        subtitle,
    }
}

/// What follows a change of the "Run as ARM" switch, from how `dsa` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunAsArmNext {
    /// It is done; the toast says how the app runs now.
    Done(String),
    /// Android cannot keep the app's data: the user is asked whether to erase it.
    ConfirmErase,
    /// Android refused; the message says why.
    Refused(String),
}

/// What follows `outcome` of switching `label` to ARM (`on`) or back.
pub fn run_as_arm_next(label: &str, on: bool, outcome: &Outcome) -> RunAsArmNext {
    if outcome.done {
        return RunAsArmNext::Done(if on {
            trf("{0} runs as ARM now.", &[label])
        } else {
            trf("{0} runs its x86_64 code now.", &[label])
        });
    }
    if outcome.status == "data-would-be-erased" {
        return RunAsArmNext::ConfirmErase;
    }
    let reason = tr(match outcome.status.as_str() {
        "not-installed" => "it is no longer installed",
        "system-app" => "it is part of Android",
        "no-libraries" => "its APKs on the device hold no code for that",
        "translation-off" => "ARM translation is not on",
        _ => "Android refused",
    });
    RunAsArmNext::Refused(trf(
        "{0} was not installed again: {1} ({2})",
        &[label, &reason, &outcome.detail],
    ))
}

/// The root levels an app can have, as `dsa apps root` names them, in the order the Apps
/// page lists them.
pub const ROOT_LEVELS: [&str; 3] = ["off", "ask", "remember"];

/// The root levels as the Apps page lists them.
pub fn root_choices() -> Vec<String> {
    vec![
        tr("Not allowed"),
        tr("Ask each time"),
        tr("Remember (no asking)"),
    ]
}

/// What an app's root level shows: the level chosen, whether it can be changed, and what its
/// row says under its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootView {
    /// Where the level is among [`root_choices`]; `None` when there is none to show.
    pub index: Option<usize>,
    pub sensitive: bool,
    pub subtitle: String,
}

/// The root level of `app`: insensitive, with the reason, while it cannot be chosen.
pub fn root_view(app: &App) -> RootView {
    let index = ROOT_LEVELS.iter().position(|level| *level == app.root);
    let subtitle = match (index, app.root.as_str()) {
        (Some(_), _) => tr("Not allowed hides su from the app. Ask each time asks an administrator for each request, then not again for 15 minutes. Remember approves without asking, and choosing it needs an administrator's approval."),
        (None, "unavailable") => {
            tr("Root access is off for this computer. An administrator turns it on under Advanced.")
        }
        (None, _) => tr("This version of DSA cannot say how far the app may use root."),
    };
    RootView {
        index,
        sensitive: index.is_some(),
        subtitle,
    }
}

/// Whether root elevation is off for the computer, as the apps' levels say, so the Apps page
/// says where to turn it on.
pub fn root_unavailable(apps: &[App]) -> bool {
    apps.iter().any(|app| app.root == "unavailable")
}

/// What follows `outcome` of setting `label`'s root level: the toast when it is done, or why
/// it was not.
pub fn root_next(label: &str, outcome: &Outcome) -> Result<String, String> {
    if !outcome.done {
        return Err(trf(
            "The root level of {0} was not changed: {1}",
            &[label, &outcome.detail],
        ));
    }
    Ok(match outcome.status.as_str() {
        "off" => trf(
            "{0} cannot use root any more. su disappears for it from the next start of Android.",
            &[label],
        ),
        "remember" => trf(
            "{0} now uses su without asking. If it could not see su before, it sees it from the next start of Android.",
            &[label],
        ),
        _ => trf(
            "{0} now asks an administrator of this computer each time it uses su. If it could not see su before, it sees it from the next start of Android.",
            &[label],
        ),
    })
}

/// Whether root elevation differs from what it was when the page first read it: the change
/// applies from the next start of Android.
pub fn root_restart_needed(at_start: Option<bool>, now: bool) -> bool {
    at_start.is_some_and(|before| before != now)
}

/// Where each root request and its result is recorded: the service log.
pub fn root_log_text(log_dir: Option<&str>) -> String {
    match log_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => trf(
            "Each request is kept in the service log with the app, the time, whether it was approved and the command: {0}",
            &[dir],
        ),
        None => tr("Each request is kept in the service log with the app, the time, whether it was approved and the command."),
    }
}

/// Why Android refused to uninstall an app, in words.
pub fn uninstall_reason(status: &str) -> String {
    tr(match status {
        "not-installed" => "it is no longer installed",
        "system-app" => "it is part of Android",
        "blocked" => {
            "Android does not allow it, because the app is a device administrator or a policy keeps it"
        }
        _ => "Android could not uninstall it",
    })
}

/// Why Android refused to install an APK, in words.
pub fn install_reason(status: &str) -> String {
    tr(match status {
        "incompatible" => "the app does not run on this Android: it needs another processor type, a newer Android version or a feature this device does not have.",
        "conflicting-signature" => "the app is installed already, signed with another key. Uninstall it in Android first.",
        "invalid-apk" => "the file is not a signed APK that Android can read.",
        "insufficient-storage" => "Android has no room for the app.",
        "downgrade" => "a newer version of the app is installed. Uninstall it in Android first.",
        "blocked" => "a policy of this Android does not allow the app.",
        "app-bundle" => "it is an Android App Bundle, made to be uploaded to an app store. Get the app as an APK, or as an .xapk, .apks or .apkm bundle.",
        "encrypted-bundle" => "the bundle is encrypted, so its APKs cannot be read. Get the app as an APK, or as a newer bundle.",
        "split-signature-mismatch" => "a split APK of the bundle is signed with another key than its base APK, so Android would refuse it.",
        "invalid-bundle" => "the bundle is not one app that Android can install.",
        "unsupported-abi" => "the app's native code is none that DSA's Android runs, which runs 64-bit code only: x86_64, and arm64-v8a through ARM translation. Get the app's x86_64 or arm64-v8a build.",
        "translation-off" => "the app runs only as ARM code (arm64-v8a), and ARM translation is not on. Turn it on under ARM apps, restart Android, and install the app again.",
        _ => "Android did not install the app.",
    })
}

/// What the ARM translation section says.
pub fn arm_status(arm: &ArmTranslation) -> String {
    use crate::model::ArmSource;
    if let Some(translator) = &arm.translator {
        let license = if translator.license.is_empty() {
            tr("licence not given")
        } else {
            translator.license.clone()
        };
        let values = [
            translator.name.as_str(),
            translator.version.as_str(),
            license.as_str(),
        ];
        return match translator.source {
            ArmSource::Default => trf(
                "Supported by {0} {1} ({2}), the translator that comes with this Android.",
                &values,
            ),
            ArmSource::User | ArmSource::Google => trf("Supported by {0} {1} ({2}).", &values),
        };
    }
    match (&arm.package, &arm.default) {
        (Some((name, version, _)), _) if !arm.enabled => trf(
            "Not supported: {0} {1} is installed but turned off.",
            &[name, version],
        ),
        (None, Some((name, version, _))) if !arm.enabled => {
            trf("Not supported: {0} {1} is turned off.", &[name, version])
        }
        _ => tr("Not supported: no ARM translation package is installed."),
    }
}

/// What the row that goes back to the default translator says, where the image has one.
pub fn arm_default_text(arm: &ArmTranslation) -> Option<String> {
    let (name, version, license) = arm.default.as_ref()?;
    let license = if license.is_empty() {
        tr("licence not given")
    } else {
        license.clone()
    };
    Some(trf(
        "{0} {1} ({2}) comes with this Android and is used unless you install another translator. Going back to it removes the package installed in its place, from the next start of Android.",
        &[name, version, &license],
    ))
}

/// Whether the default translator can be chosen again: the image has one, and a package is
/// installed in its place.
pub fn arm_can_use_default(arm: &ArmTranslation) -> bool {
    arm.default.is_some() && arm.package.is_some()
}

pub fn arm_abis(arm: &ArmTranslation) -> String {
    match &arm.abilist {
        Some(abis) => trf("Android installs apps for: {0}", &[abis]),
        None => tr("Android is not running, so what it supports now is not known."),
    }
}

/// What the confirmation says before Google's ARM translator is downloaded: where it comes
/// from, and the terms the user accepts.
pub fn arm_google_notice() -> String {
    trf(
        "DSA downloads the Android emulator image for this Android version from Google's SDK repository ({0}) and takes out only its ARM translator. The translator is Google's software, offered under the Android SDK License ({1}); by downloading it you accept those terms. DSA does not ship or pass it on. The image is about 2 GB and is deleted afterwards. The translator works from the next start of Android.",
        &[
            crate::model::GOOGLE_ARM_SOURCE,
            crate::model::GOOGLE_ARM_TERMS_URL,
        ],
    )
}

/// What the ARM apps section says while Google's translator is fetched.
pub fn arm_google_busy_text() -> String {
    tr("Downloading about 2 GB from Google and taking out the translator. It takes minutes.")
}

/// What the Google Play item says of the package and whether Android uses it.
pub fn gapps_status(gapps: &Gapps) -> String {
    let package = match &gapps.package {
        Some(p) => trf(
            "LiteGapps {0} {1} for Android {2} ({3})",
            &[&p.variant, &p.version, &p.android_version, &p.arch],
        ),
        None => String::new(),
    };
    match gapps.state() {
        GappsState::RestartPending if gapps.package.is_some() => trf(
            "Installed: {0}. Restart Android to start using it.",
            &[&package],
        ),
        GappsState::RestartPending => {
            tr("Removed. The Google apps stay in Android until it restarts.")
        }
        GappsState::ReinstallNeeded => trf(
            "{0} was installed by an older DSA and is no longer used, so Android starts without Google's apps. Install Google Play again.",
            &[&package],
        ),
        GappsState::Installed if gapps.package.is_some() => trf("Installed: {0}.", &[&package]),
        _ => tr("Not installed."),
    }
}

/// Where the installed package came from; empty without one.
pub fn gapps_source(gapps: &Gapps) -> String {
    match &gapps.package {
        Some(p) => {
            let file = if p.file.is_empty() {
                &p.source
            } else {
                &p.file
            };
            trf("Downloaded from the LiteGapps project: {0}", &[file])
        }
        None => String::new(),
    }
}

/// What the Google Play item says while the package is being installed.
pub fn gapps_busy_text() -> String {
    tr("Downloading and installing Google Play. This can take several minutes.")
}

/// What the Google Play item says of the Android ID Google's registration page asks for.
pub fn gapps_device_id(gapps: &Gapps) -> String {
    match &gapps.device_id {
        Some(_) => tr("Enter this number on Google's registration page."),
        None => tr("It shows here once Google Play is installed and Android has started."),
    }
}

/// What the protected content item says of the module and whether Android uses it.
pub fn widevine_status(widevine: &crate::model::Widevine) -> String {
    use crate::model::WidevineState;
    let package = match &widevine.package {
        Some(p) => trf(
            "Google's image {0} for Android {1} ({2})",
            &[&p.version, &p.android_major.to_string(), &p.abi],
        ),
        None => String::new(),
    };
    match widevine.state() {
        WidevineState::RestartPending if widevine.package.is_some() => trf(
            "Installed: {0}. Restart Android to start using it.",
            &[&package],
        ),
        WidevineState::RestartPending => {
            tr("Removed. Widevine stays in Android until it restarts.")
        }
        WidevineState::Installed if widevine.package.is_some() => {
            trf("Installed: {0}.", &[&package])
        }
        _ => tr("Not installed."),
    }
}

/// What the confirmation says before Google's image is downloaded for Widevine: where it comes
/// from, and the terms the user accepts.
pub fn widevine_notice() -> String {
    trf(
        "DSA downloads the Android emulator image for this Android version from Google's SDK repository ({0}) and takes out only its Widevine module, unchanged. Widevine is Google's software, offered under the Android SDK License ({1}); by downloading it you accept those terms. DSA does not ship or pass it on. The image is about 2 GB and is deleted afterwards. Widevine works from the next start of Android.",
        &[
            crate::model::WIDEVINE_SOURCE,
            crate::model::WIDEVINE_TERMS_URL,
        ],
    )
}

/// What the protected content item always says: L3 is standard definition at most, and where
/// to watch in high definition.
pub fn widevine_quality() -> String {
    tr("Widevine here is security level L3, so streaming services send standard definition (SD) at most. To watch in high definition, use this computer's own browser or app.")
}

/// What the protected content item says while the module is taken from Google's image.
pub fn widevine_busy_text() -> String {
    tr("Downloading about 2 GB from Google and taking out Widevine. It takes minutes.")
}

/// When something last happened to the backup, in local time through `local` (which takes
/// milliseconds since 1970), or "Never".
pub fn backup_when(unix_ms: Option<i64>, local: impl Fn(i64) -> String) -> String {
    match unix_ms {
        Some(ms) => local(ms),
        None => tr("Never"),
    }
}

/// What the backup holds: how many apps, and how much of their data.
pub fn backup_contents(backup: &Backup) -> String {
    if backup.packages == 0 {
        return tr("No apps yet");
    }
    trf(
        "{0} apps, {1}",
        &[&backup.packages.to_string(), &size_bytes(backup.size)],
    )
}

/// What the backup section says while `job` runs; empty for a request that ends at once.
pub fn backup_busy_text(job: BackupJob) -> String {
    match job {
        BackupJob::BackUp => tr(
            "Backing up every app. Android starts if it is not running. This can take a while.",
        ),
        BackupJob::Restore => tr(
            "Restoring apps and their data from the backup. Android starts if it is not running. This can take a while.",
        ),
        BackupJob::Read | BackupJob::ShowKey | BackupJob::ImportKey | BackupJob::Switch => {
            String::new()
        }
    }
}

/// What the backup switch says under its title: what backup keeps, and while it is off, what
/// earlier backups are still kept.
pub fn backup_switch_subtitle(backup: Option<&Backup>) -> String {
    let what = tr("Keeps a second copy of your Android apps' user data on this computer, encrypted, so it can be restored after a data reset or on another computer. Off until you turn it on.");
    match backup {
        Some(backup) if !backup.enabled && backup.has_backups_kept() => format!(
            "{what}\n{}",
            trf(
                "Earlier backups of {0} apps ({1}) are still kept in your DSA Backup folder.",
                &[&backup.packages.to_string(), &size_bytes(backup.size)],
            )
        ),
        _ => what,
    }
}

/// What the question says when backup is turned off while backups are kept.
pub fn backup_delete_question(backup: &Backup) -> String {
    trf(
        "The backups of {0} apps ({1}) are still kept on this computer. Delete them and their key as well, or keep them for a later restore?",
        &[&backup.packages.to_string(), &size_bytes(backup.size)],
    )
}

/// What turning backup on or off did.
pub fn backup_switched(on: bool, deleted: bool) -> String {
    match (on, deleted) {
        (true, _) => {
            tr("Backup is on. Android backs up its apps' data to this computer from now on.")
        }
        (false, true) => tr("Backup is off, and the backups and their key were deleted."),
        (false, false) => tr("Backup is off. The backups made so far are kept."),
    }
}

/// What protects Android's keys on this computer.
pub fn key_protection_text(key: &KeyProtection) -> String {
    match key.shown() {
        KeyProtectionShown::NotYet => {
            tr("Android's keys get their protection the first time Android starts.")
        }
        KeyProtectionShown::Tpm => tr("Sealed by this computer's TPM. A copy of Android's disk cannot open the apps' keys or Android's encrypted storage anywhere else."),
        KeyProtectionShown::Keyring => tr("Kept by your keyring only, because this computer has no TPM 2.0 that can be used. Once one works, DSA seals the keys with it."),
        KeyProtectionShown::Other => trf("Protected by {0}.", &[&key.description]),
    }
}

/// Why no TPM seals Android's keys, when none does.
pub fn key_protection_reason(key: &KeyProtection) -> Option<String> {
    key.shown_reason()
        .map(|reason| trf("Why not the TPM: {0}", &[reason]))
}

/// How a backup or a restore ended, from the report it answered with.
pub fn backup_result(job: BackupJob, backup: &Backup) -> String {
    let (ok, failed, installed) = (
        backup.succeeded.to_string(),
        backup.failed.to_string(),
        backup.installed.to_string(),
    );
    match (job, backup.failed) {
        (BackupJob::Restore, 0) => trf(
            "Apps restored: {0}. Apps installed first: {1}.",
            &[&ok, &installed],
        ),
        (BackupJob::Restore, _) => trf(
            "Apps restored: {0}. Apps installed first: {1}. Apps that could not be restored: {2}.",
            &[&ok, &installed, &failed],
        ),
        (_, 0) => trf("Apps backed up: {0}.", &[&ok]),
        (_, _) => trf(
            "Apps backed up: {0}. Apps that could not be backed up: {1}.",
            &[&ok, &failed],
        ),
    }
}

/// What the question before a restore says: what it installs and what it replaces.
pub fn backup_restore_warning() -> String {
    tr("Apps in the backup that Android does not have are installed, and the data of every app in the backup is replaced with the backup's. What those apps saved since the backup is lost. Android starts if it is not running.")
}

/// What the recovery key dialog says under its title.
pub fn backup_key_advice() -> String {
    tr("Write this key down and keep it somewhere safe, away from this computer. Anyone with it can read the backup. Without it, the backup cannot be opened on another computer or after this computer's keyring is lost.")
}

/// Why a recovery key was not used, from the service's reason; empty is no key at all.
pub fn backup_key_refusal(reason: &str) -> String {
    if reason.is_empty() {
        tr("Enter the recovery key.")
    } else {
        trf("The key was not used: {0}", &[reason])
    }
}

/// A shared folder's name in the user's language.
pub fn folder_label(folder: &str) -> String {
    tr(folder)
}

/// The shared folders after turning `folder` on or off, in the order the service keeps them.
pub fn toggle_folder(current: &[String], folder: &str, on: bool) -> Vec<String> {
    ALL_FOLDERS
        .iter()
        .filter(|f| {
            if **f == folder {
                on
            } else {
                current.iter().any(|c| c == *f)
            }
        })
        .map(|f| f.to_string())
        .collect()
}

pub fn network_mode_text(network: Option<&Network>) -> String {
    tr(match network.map(|n| n.mode) {
        Some(NetworkMode::Nat) => {
            "Connected through this computer (NAT). Android apps see it as Wi-Fi, with no data charges."
        }
        Some(NetworkMode::Advanced) => {
            "Connected to this computer's network as a device of its own (advanced networking)."
        }
        Some(NetworkMode::Off) => "Android is not running, so it has no network yet.",
        _ => "The DSA service does not report the network. Update DSA to see it.",
    })
}

/// Who answers Android's DNS queries: this computer's resolver, or, when that does not answer,
/// the network's DNS servers. A service that does not say shows the servers.
pub fn network_dns_text(network: Option<&Network>) -> String {
    let servers = list_or_none(
        network
            .map(|n| n.dns_servers.as_slice())
            .unwrap_or_default(),
    );
    match network.map(|n| n.dns_resolver) {
        Some(DnsResolver::Host) => tr(
            "Answered by this computer's resolver, so its hosts file, VPN and DNS filters apply to Android apps.",
        ),
        Some(DnsResolver::Network) => trf(
            "This computer's resolver does not answer, so Android asks the network's DNS servers: {0}",
            &[servers.as_str()],
        ),
        _ => servers,
    }
}

/// A list of addresses, or "None".
pub fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        tr("None")
    } else {
        items.join(", ")
    }
}

/// Whether the advanced networking setting differs from the network Android has now, so it
/// applies only at the next start.
pub fn network_restart_needed(advanced_setting: bool, network: Option<&Network>) -> bool {
    match network.map(|n| n.mode) {
        Some(NetworkMode::Nat) => advanced_setting,
        Some(NetworkMode::Advanced) => !advanced_setting,
        _ => false,
    }
}

/// Where an added root certificate was found, in the user's language. A source this app does
/// not know, from a newer service, is shown as the service names it.
pub fn certificate_source_label(source: &str) -> String {
    match source {
        "machine" => tr("This computer's trusted root certificates"),
        "machine_group_policy" => tr("This computer's group policy"),
        "machine_enterprise" => tr("This computer's enterprise certificates"),
        "user" => tr("Your trusted root certificates"),
        "user_group_policy" => tr("Your group policy"),
        "admin" => tr("This computer's administrator trust settings"),
        "system" => tr("Added to the system's certificate store"),
        other => other.to_string(),
    }
}

/// The title of an added root certificate's row: who it is for, or its fingerprint when the
/// service could not read a name.
pub fn certificate_title(certificate: &ImportedCertificate) -> String {
    if !certificate.subject.is_empty() {
        certificate.subject.clone()
    } else if !certificate.sha256.is_empty() {
        trf("SHA-256 {0}", &[&certificate.sha256])
    } else {
        tr("A certificate without a name")
    }
}

/// What an added root certificate's row says under its title: who signed it, where it was
/// found, and the file or store, each on its own line.
pub fn certificate_detail(certificate: &ImportedCertificate) -> String {
    let mut lines = Vec::new();
    if !certificate.issuer.is_empty() {
        lines.push(trf("Issued by {0}", &[&certificate.issuer]));
    }
    if !certificate.source.is_empty() {
        lines.push(certificate_source_label(&certificate.source));
    }
    if !certificate.location.is_empty() {
        lines.push(certificate.location.clone());
    }
    lines.join("\n")
}

/// What the list of added root certificates says when it has none.
pub fn certificates_empty_text() -> String {
    tr("No added root certificates were found on this computer.")
}

/// The fingerprint Android reports from its next start, from the values on the page: the
/// whole fingerprint when one is written, or the one its parts make.
pub fn fingerprint_preview(value: impl Fn(&str) -> String) -> String {
    let whole = value("fingerprint");
    if !whole.is_empty() {
        return whole;
    }
    format!(
        "{}/{}/{}:{}/{}/{}:{}/{}",
        value("brand"),
        value("name"),
        value("device"),
        value("fingerprint.release"),
        value("fingerprint.id"),
        value("fingerprint.incremental"),
        value("fingerprint.type"),
        value("fingerprint.tags")
    )
}

/// The label of a device identity value.
pub fn identity_label(key: &str) -> String {
    tr(match key {
        "manufacturer" => "Manufacturer",
        "brand" => "Brand",
        "model" => "Model",
        "device" => "Device",
        "name" => "Product name",
        "characteristics" => "Form factor",
        "fingerprint.release" => "Release",
        "fingerprint.id" => "Build ID",
        "fingerprint.incremental" => "Incremental version",
        "fingerprint.type" => "Build type",
        "fingerprint.tags" => "Tags",
        "model_name" => "Model name",
        "device_name" => "Device name",
        "soc_manufacturer" => "Chip manufacturer",
        "soc_model" => "Chip model",
        _ => "Whole fingerprint",
    })
}

/// Where a default came from, as the page says it: `computer` or `dsa`.
pub fn origin_label(origin: &str) -> String {
    if origin == "computer" {
        tr("Default: from this computer")
    } else {
        tr("Default: DSA's value")
    }
}

/// A value that follows the computer, as the page shows it: the value, or a note that the
/// computer does not say it and Android keeps its own.
pub fn computer_value(value: &str) -> String {
    if value.is_empty() {
        tr("This computer does not say; Android keeps its own.")
    } else {
        value.to_string()
    }
}

/// Checks a value typed for the device identity as the service will; `None` when it is
/// fine. An empty value is fine: it keeps the value shown.
pub fn identity_problem(key: &str, value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let field = dsa_host_core::device_identity::Field::parse(key).ok()?;
    field.check(value).err()
}

/// The priorities of Android's log, lowest first, as `dsa logcat` takes them.
pub const LOG_PRIORITIES: [&str; 6] = ["V", "D", "I", "W", "E", "F"];

pub fn priority_label(priority: &str) -> String {
    tr(match priority {
        "V" => "Verbose",
        "D" => "Debug",
        "I" => "Info",
        "W" => "Warning",
        "E" => "Error",
        _ => "Fatal",
    })
}

/// The arguments that read Android's log from `priority` up.
pub fn logcat_args(priority: &str) -> Vec<String> {
    vec!["logcat".into(), "--json".into(), format!("*:{priority}")]
}

/// The log page's lines: the newest records, filtered by text, which can be paused.
#[derive(Debug, Default)]
pub struct LogView {
    lines: VecDeque<crate::model::LogLine>,
    waiting: Vec<crate::model::LogLine>,
    pub filter: String,
    paused: bool,
}

impl LogView {
    /// The most lines the page keeps.
    pub const LIMIT: usize = 5000;

    /// Adds a line `dsa logcat --json` printed. Returns whether the page should show it now.
    pub fn add(&mut self, json_line: &str) -> bool {
        let Some(line) = crate::model::LogLine::parse(json_line) else {
            return false;
        };
        if self.paused {
            self.waiting.push(line);
            return false;
        }
        self.push(line);
        true
    }

    fn push(&mut self, line: crate::model::LogLine) {
        self.lines.push_back(line);
        while self.lines.len() > Self::LIMIT {
            self.lines.pop_front();
        }
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// How many lines wait while paused, in words; empty when not paused.
    pub fn paused_text(&self) -> String {
        if self.paused {
            trf(
                "Paused. {0} new lines are waiting.",
                &[&self.waiting.len().to_string()],
            )
        } else {
            String::new()
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if !paused {
            for line in std::mem::take(&mut self.waiting) {
                self.push(line);
            }
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.waiting.clear();
    }

    pub fn matches(&self, line: &crate::model::LogLine) -> bool {
        let filter = self.filter.trim().to_lowercase();
        filter.is_empty() || line.text().to_lowercase().contains(&filter)
    }

    /// The lines the filter shows, oldest first.
    pub fn shown(&self) -> Vec<&crate::model::LogLine> {
        self.lines.iter().filter(|l| self.matches(l)).collect()
    }

    /// The newest line, for appending it to what is shown.
    pub fn last(&self) -> Option<&crate::model::LogLine> {
        self.lines.back()
    }
}

/// What copying log lines puts on the clipboard: each whole line as the log prints it, one
/// per line. `shown` is what the page shows and `selected` the positions in it the user
/// picked, in any order; the lines keep the page's order.
pub fn log_copy_text(shown: &[crate::model::LogLine], selected: &[usize]) -> String {
    let mut picked: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|&i| i < shown.len())
        .collect();
    picked.sort_unstable();
    picked.dedup();
    picked
        .iter()
        .map(|&i| shown[i].text())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The user's DSA logs: the window manager's and the app windows'.
pub fn user_log_folder() -> std::path::PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(|| "/tmp".into());
    data.join("dsa").join("logs")
}

/// The service's and Android's logs: the folder the service reports, or the machine's.
pub fn service_log_folder(reported: Option<&str>) -> std::path::PathBuf {
    reported
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/var/lib/dsa/logs".into())
}

/// Where the packages put the licences of everything DSA ships.
pub const LICENSE_FOLDER: &str = "/usr/share/doc/desktop-subsystem-for-android";

/// Where the source code is.
pub const SOURCE_URL: &str = "https://github.com/thisisthepy/desktop-subsystem-for-android";

/// The program that starts the user's window manager again: the compositor's user unit.
pub fn restart_window_manager_command() -> Vec<&'static str> {
    vec!["systemctl", "--user", "restart", "dsa-compositor.service"]
}

/// The program that shows the system's software updates, when the desktop has one.
pub fn software_updates_commands() -> Vec<Vec<&'static str>> {
    vec![
        vec!["gnome-software", "--mode=updates"],
        vec!["plasma-discover", "--mode", "update"],
    ]
}

/// How long the service may stay away during an update of DSA before the page stops waiting
/// for it and says so.
pub const PROGRAM_RESTART_LIMIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// An update of DSA itself while it runs: what it is doing, the version, and that the user
/// should leave DSA alone until it is done. The service reports the phase in its status; while
/// the service is being replaced there is no service to ask, and that gap reads as DSA
/// restarting, not as an error. Once the new service answers, the page says which version runs.
/// The same rules as the Windows and macOS apps' view models.
#[derive(Debug, Clone, Default)]
pub struct ProgramUpdateFollower {
    phase: Option<ProgramUpdatePhase>,
    version: String,
    /// Set while the request to install now waits for its answer.
    asking: bool,
    /// When the service went away during the update.
    away_since: Option<std::time::Instant>,
    /// The service stayed away longer than [`PROGRAM_RESTART_LIMIT`].
    not_back: bool,
    error: Option<String>,
}

impl ProgramUpdateFollower {
    pub fn phase(&self) -> ProgramUpdatePhase {
        self.phase.unwrap_or(ProgramUpdatePhase::None)
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Whether an update is under way, so the page shows its progress.
    pub fn in_progress(&self) -> bool {
        self.phase().in_progress()
    }

    /// Why the request to install now was refused, in the user's language.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn set(&mut self, phase: ProgramUpdatePhase, version: &str) {
        if !version.is_empty() {
            self.version = version.to_string();
        }
        self.phase = Some(phase);
    }

    /// What the update is doing now, in the user's language.
    pub fn phase_text(&self) -> String {
        let version = self.version.as_str();
        match self.phase() {
            ProgramUpdatePhase::Starting => tr("Getting the update ready"),
            ProgramUpdatePhase::Downloading => trf("Downloading DSA {0}", &[version]),
            ProgramUpdatePhase::StoppingAndroid => {
                trf("Stopping Android to install DSA {0}", &[version])
            }
            ProgramUpdatePhase::Installing => trf("Installing DSA {0}", &[version]),
            ProgramUpdatePhase::Restarting => tr("DSA is restarting to finish the update"),
            _ => String::new(),
        }
    }

    /// The version under the phase, while it is known.
    pub fn version_text(&self) -> Option<String> {
        (self.in_progress() && !self.version.is_empty())
            .then(|| trf("Version {0}", &[self.version.as_str()]))
    }

    /// The line that asks the user to leave DSA alone while the update runs.
    pub fn keep_open_text() -> String {
        tr("Do not close DSA or start Android apps until the update is done.")
    }

    /// How the update ended, once it has: whether it succeeded, and the text.
    pub fn ended_text(&self) -> Option<(bool, String)> {
        let version = self.version.as_str();
        match self.phase() {
            ProgramUpdatePhase::Applied => Some((true, trf("Updated to {0}", &[version]))),
            ProgramUpdatePhase::Failed if self.not_back => Some((
                false,
                trf(
                    "DSA has not come back after the update to {0}. If it does not start soon, restart the computer.",
                    &[version],
                ),
            )),
            ProgramUpdatePhase::Failed => Some((
                false,
                trf(
                    "Updating to {0} failed. The version before it is still installed.",
                    &[version],
                ),
            )),
            _ => None,
        }
    }

    /// The request to install now was sent: the page shows the update from this moment.
    pub fn asked(&mut self) {
        self.error = None;
        self.not_back = false;
        self.away_since = None;
        self.version.clear();
        self.asking = true;
        self.phase = Some(ProgramUpdatePhase::Starting);
    }

    /// The service answered the request to install now, at `now`.
    pub fn answered(
        &mut self,
        answer: Result<ProgramUpdateAnswer, Failure>,
        now: std::time::Instant,
    ) {
        self.asking = false;
        match answer {
            Ok(ProgramUpdateAnswer {
                installing: None, ..
            }) => self.set(ProgramUpdatePhase::None, ""),
            Ok(ProgramUpdateAnswer {
                installing: Some(version),
                ..
            }) => {
                // A poll may already have seen a later phase, or the service go away.
                let phase = match self.phase() {
                    ProgramUpdatePhase::Starting
                    | ProgramUpdatePhase::Downloading
                    | ProgramUpdatePhase::StoppingAndroid => ProgramUpdatePhase::Installing,
                    later => later,
                };
                self.set(phase, &version);
            }
            // Once the new version is being installed, a lost answer is the restart.
            Err(_)
                if matches!(
                    self.phase(),
                    ProgramUpdatePhase::Installing | ProgramUpdatePhase::Restarting
                ) =>
            {
                self.gone_away(now)
            }
            Err(failure) => {
                self.error = Some(problem_text(&failure));
                self.set(ProgramUpdatePhase::None, "");
            }
        }
    }

    /// What a poll of the running service found, at `now`. Returns true when the service
    /// answers again after it went away during the update, so the page reads the new
    /// version's settings.
    pub fn polled(&mut self, status: Result<&Status, &Failure>, now: std::time::Instant) -> bool {
        let reported = match status {
            Ok(status) => status.program_update.clone(),
            Err(_) => {
                // A service that is away matters only while an update runs.
                if self.in_progress() {
                    self.gone_away(now);
                }
                return false;
            }
        };
        if self.away_since.take().is_some() {
            self.not_back = false;
            match reported {
                Some(update) => self.set(update.phase, &update.version),
                // A new service with nothing to report runs the version that was installed.
                None => self.set(ProgramUpdatePhase::Applied, ""),
            }
            return true;
        }
        if self.asking {
            // Until the request is answered, only a phase of this update counts.
            if let Some(update) = reported
                .filter(|u| u.phase.in_progress() && u.phase != ProgramUpdatePhase::Starting)
            {
                self.set(update.phase, &update.version);
            }
            return false;
        }
        match reported {
            Some(update) => self.set(update.phase, &update.version),
            // Nothing is under way. An update the page followed keeps saying how it ended.
            None if self.in_progress() => self.set(ProgramUpdatePhase::None, ""),
            None => {}
        }
        false
    }

    fn gone_away(&mut self, now: std::time::Instant) {
        let since = *self.away_since.get_or_insert(now);
        if now.duration_since(since) > PROGRAM_RESTART_LIMIT {
            self.not_back = true;
            self.set(ProgramUpdatePhase::Failed, "");
        } else {
            self.set(ProgramUpdatePhase::Restarting, "");
        }
    }
}
