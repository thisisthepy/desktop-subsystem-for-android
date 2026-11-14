//! What the service reports, read from the JSON the `dsa` client prints, and the requests
//! that change it. [`Client`] is the one place that knows the command lines; they are the
//! same ones the Windows and macOS settings apps run.

use std::sync::Arc;

use serde_json::Value;

use crate::api::{Dsa, Failure, Problem};

fn text(json: &Value, name: &str) -> String {
    json[name].as_str().unwrap_or_default().to_string()
}

fn optional_text(json: &Value, name: &str) -> Option<String> {
    json[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn flag(json: &Value, name: &str, missing: bool) -> bool {
    json.get(name).and_then(Value::as_bool).unwrap_or(missing)
}

fn number(json: &Value, name: &str) -> u64 {
    json[name].as_u64().unwrap_or(0)
}

fn signed(json: &Value, name: &str) -> i64 {
    json[name].as_i64().unwrap_or(0)
}

fn texts(json: &Value, name: &str) -> Vec<String> {
    json[name]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn items<'a>(json: &'a Value, name: &str) -> &'a [Value] {
    json[name].as_array().map(Vec::as_slice).unwrap_or_default()
}

/// Decodes base64, as `dsa` prints icons and previews; nothing for anything else.
pub fn base64_decode(input: &str) -> Vec<u8> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0u32);
    for c in input.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let Some(v) = value(c) else {
            return Vec::new();
        };
        bits = (bits << 6) | v;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    out
}

/// Where the user's Android is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Unknown,
    Stopped,
    Starting,
    Active,
    Dozing,
    Frozen,
    Stopping,
    Stalled,
}

impl State {
    pub fn parse(name: &str) -> State {
        match name {
            "Stopped" => State::Stopped,
            "Starting" => State::Starting,
            "Active" => State::Active,
            "Dozing" => State::Dozing,
            "Frozen" => State::Frozen,
            "Stopping" => State::Stopping,
            "Stalled" => State::Stalled,
            _ => State::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub last_reason: Option<String>,
    pub update_notice: Option<String>,
    pub problem: Option<String>,
    /// Where an update of DSA itself stands, when one is under way or has just ended.
    pub program_update: Option<ProgramUpdate>,
}

impl Status {
    pub fn read(json: &Value) -> Status {
        Status {
            state: State::parse(json["state"].as_str().unwrap_or_default()),
            last_reason: optional_text(json, "last_reason"),
            update_notice: optional_text(json, "update_notice"),
            problem: optional_text(json, "problem"),
            program_update: ProgramUpdate::read(&json["program_update"]),
        }
    }
}

/// How far an update of DSA itself has got, as the service and the settings app see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramUpdatePhase {
    /// No update of DSA is under way.
    None,
    /// The app asked the service to install now and waits for its first word.
    Starting,
    Downloading,
    /// Every user's Android is being stopped so the new version can be installed.
    StoppingAndroid,
    Installing,
    /// The service is restarting into the new version, or is away while it is replaced.
    Restarting,
    /// The new version runs.
    Applied,
    /// The update failed and the version before it is still installed.
    Failed,
}

impl ProgramUpdatePhase {
    /// The phase a name of `dsa status --json` stands for. An unknown name, as a newer service
    /// may give, is none.
    pub fn parse(name: &str) -> ProgramUpdatePhase {
        match name {
            "downloading" => ProgramUpdatePhase::Downloading,
            "stopping_android" => ProgramUpdatePhase::StoppingAndroid,
            "installing" => ProgramUpdatePhase::Installing,
            "restarting" => ProgramUpdatePhase::Restarting,
            "applied" => ProgramUpdatePhase::Applied,
            "failed" => ProgramUpdatePhase::Failed,
            _ => ProgramUpdatePhase::None,
        }
    }

    /// Whether the update is still under way, so the user should leave DSA alone.
    pub fn in_progress(self) -> bool {
        matches!(
            self,
            ProgramUpdatePhase::Starting
                | ProgramUpdatePhase::Downloading
                | ProgramUpdatePhase::StoppingAndroid
                | ProgramUpdatePhase::Installing
                | ProgramUpdatePhase::Restarting
        )
    }
}

/// Where an update of DSA stands: its phase and the version it installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramUpdate {
    pub phase: ProgramUpdatePhase,
    pub version: String,
}

impl ProgramUpdate {
    fn read(json: &Value) -> Option<ProgramUpdate> {
        let phase = ProgramUpdatePhase::parse(json["phase"].as_str().unwrap_or_default());
        (phase != ProgramUpdatePhase::None).then(|| ProgramUpdate {
            phase,
            version: text(json, "version"),
        })
    }
}

/// What the service answered when asked to install the newest version of DSA now: the version
/// it installs, or none when there is nothing to install, and its own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramUpdateAnswer {
    pub installing: Option<String>,
    pub detail: String,
}

/// How Android reaches the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    Unknown,
    Nat,
    Advanced,
    Off,
}

/// Who answers Android's DNS queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsResolver {
    /// The service did not say: Android is still starting, or the service is older.
    Unknown,
    /// This computer's resolver, so its hosts file, VPN and DNS filters apply.
    Host,
    /// This computer's resolver does not answer, so Android asks the network's DNS servers
    /// itself.
    Network,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub mode: NetworkMode,
    pub addresses: Vec<String>,
    pub dns_servers: Vec<String>,
    pub gateway: String,
    pub problem: Option<String>,
    pub dns_resolver: DnsResolver,
}

impl Network {
    pub fn read(json: &Value) -> Network {
        Network {
            mode: match json["mode"].as_str() {
                Some("nat") => NetworkMode::Nat,
                Some("advanced") => NetworkMode::Advanced,
                Some("off") => NetworkMode::Off,
                _ => NetworkMode::Unknown,
            },
            addresses: texts(json, "addresses"),
            dns_servers: texts(json, "dns_servers"),
            gateway: text(json, "gateway"),
            problem: optional_text(json, "problem"),
            dns_resolver: match json["dns_resolver"].as_str() {
                Some("host") => DnsResolver::Host,
                Some("network") => DnsResolver::Network,
                _ => DnsResolver::Unknown,
            },
        }
    }
}

/// The folders that can be shared with Android, in the order the service keeps them.
pub const ALL_FOLDERS: [&str; 6] = [
    "Documents",
    "Downloads",
    "Pictures",
    "Music",
    "Videos",
    "Desktop",
];

/// The loopback port adb connects to in developer mode unless the user picks another.
pub const DEFAULT_ADB_PORT: u16 = 58526;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub render_gpu: String,
    pub freeze: bool,
    pub program_updates: bool,
    pub image_updates: bool,
    pub program_updates_available: bool,
    pub image_updates_available: bool,
    pub version: String,
    pub log_dir: Option<String>,
    pub clipboard: bool,
    pub links_in_android: bool,
    pub android_version: Option<String>,
    pub developer_mode: bool,
    pub adb_port: u16,
    pub input_trace: bool,
    pub resize_trace: bool,
    pub memory_limit_mb: u32,
    pub arm_translation: bool,
    pub stop_without_asking: bool,
    pub advanced_networking: bool,
    /// Whether Android restarts by itself into an Android image the service downloaded. On
    /// by default; off, the image is used the next time the user starts Android.
    pub image_auto_restart: bool,
    pub network: Option<Network>,
    pub file_sharing: bool,
    pub shared_folders: Vec<String>,
    /// Whether developer mode's port is attached to the user's adb server by itself.
    pub adb_auto_connect: bool,
    /// Where that stands, as the service last said.
    pub adb_auto_connect_state: AdbAutoConnectState,
    /// Whether DSA takes pre-releases as well as stable releases: the pre-release channel.
    /// The machine's; it starts from the package that was installed.
    pub prerelease_updates: bool,
    /// Whether this installation follows a release channel at all.
    pub update_channel_available: bool,
    /// Whether apps in Android may ask for root through an administrator's approval. The
    /// machine's, off by default; it applies from the next start of Android.
    pub root_elevation: bool,
    /// Whether Android trusts the root certificates a user or an administrator added to this
    /// computer. On by default; it applies at once.
    pub host_certificates: bool,
    /// The root certificates the service found added to this computer, whether or not
    /// [`Settings::host_certificates`] is on. Those the system ships are left out.
    pub imported_certificates: Vec<ImportedCertificate>,
}

/// A root certificate the service found added to this computer's trust stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedCertificate {
    /// Who it is for, as a distinguished name ("CN=Example Root, O=Example").
    pub subject: String,
    /// Who signed it, the same way.
    pub issuer: String,
    /// Where it was found: "system" on Linux (added to the system's certificate store beside
    /// the distribution's bundle); the other hosts name their own stores.
    pub source: String,
    /// The file or store it was found in.
    pub location: String,
    /// Its SHA-256 fingerprint, in lower-case hexadecimal.
    pub sha256: String,
    /// When it stops being valid, in seconds since 1970; 0 when unknown.
    pub not_after: i64,
}

impl ImportedCertificate {
    pub fn read(json: &Value) -> ImportedCertificate {
        ImportedCertificate {
            subject: text(json, "subject"),
            issuer: text(json, "issuer"),
            source: text(json, "source"),
            location: text(json, "location"),
            sha256: text(json, "sha256"),
            not_after: signed(json, "not_after"),
        }
    }
}

/// Where attaching developer mode's port to the user's adb server stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdbAutoConnectState {
    /// Developer mode or auto-connect is off.
    #[default]
    Off,
    /// No adb server runs on this computer yet.
    WaitingForServer,
    /// Android is not running, or adb in Android has not answered yet.
    WaitingForAndroid,
    /// The adb server lists Android.
    Attached,
    /// The user ran `adb disconnect`; it is attached again when the adb server restarts.
    Detached,
}

impl AdbAutoConnectState {
    /// The state a name the service gives stands for. An empty or unknown name, as an older
    /// service gives, is off.
    pub fn read(name: &str) -> AdbAutoConnectState {
        match name {
            "waiting-for-server" => AdbAutoConnectState::WaitingForServer,
            "waiting-for-android" => AdbAutoConnectState::WaitingForAndroid,
            "attached" => AdbAutoConnectState::Attached,
            "detached" => AdbAutoConnectState::Detached,
            _ => AdbAutoConnectState::Off,
        }
    }
}

impl Settings {
    pub fn read(json: &Value) -> Settings {
        let port = number(json, "adb_port");
        Settings {
            render_gpu: text(json, "render_gpu"),
            freeze: flag(json, "freeze", false),
            program_updates: flag(json, "program_updates", false),
            image_updates: flag(json, "image_updates", false),
            program_updates_available: flag(json, "program_updates_available", false),
            image_updates_available: flag(json, "image_updates_available", false),
            version: text(json, "version"),
            log_dir: optional_text(json, "log_dir"),
            clipboard: flag(json, "clipboard", true),
            links_in_android: flag(json, "links_in_android", false),
            android_version: optional_text(json, "android_version"),
            developer_mode: flag(json, "developer_mode", false),
            adb_port: if (1..=u64::from(u16::MAX)).contains(&port) {
                port as u16
            } else {
                DEFAULT_ADB_PORT
            },
            input_trace: flag(json, "input_trace", false),
            resize_trace: flag(json, "resize_trace", false),
            memory_limit_mb: number(json, "memory_limit_mb") as u32,
            arm_translation: flag(json, "arm_translation", true),
            stop_without_asking: flag(json, "stop_without_asking", false),
            advanced_networking: flag(json, "advanced_networking", false),
            image_auto_restart: flag(json, "image_auto_restart", true),
            network: json
                .get("network")
                .filter(|n| n.is_object())
                .map(Network::read),
            file_sharing: flag(json, "file_sharing", true),
            shared_folders: if json["shared_folders"].is_array() {
                texts(json, "shared_folders")
            } else {
                ALL_FOLDERS.iter().map(|f| f.to_string()).collect()
            },
            adb_auto_connect: flag(json, "adb_auto_connect", true),
            adb_auto_connect_state: AdbAutoConnectState::read(&text(
                json,
                "adb_auto_connect_state",
            )),
            prerelease_updates: flag(json, "prerelease_updates", false),
            update_channel_available: flag(json, "update_channel_available", false),
            root_elevation: flag(json, "root_elevation", false),
            host_certificates: flag(json, "host_certificates", true),
            imported_certificates: items(json, "imported_certificates")
                .iter()
                .filter(|c| c.is_object())
                .map(ImportedCertificate::read)
                .collect(),
        }
    }

    /// What to type to connect adb while developer mode is on.
    pub fn adb_connect(&self) -> String {
        format!("adb connect 127.0.0.1:{}", self.adb_port)
    }
}

/// What a host process does for the user's Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRole {
    Vm,
    WindowManager,
    /// The graphics renderer (`dsa-gfx`), which the window manager starts.
    Renderer,
    AppWindow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProcess {
    pub role: HostRole,
    pub pid: u64,
    pub working_set_kb: u64,
    pub private_kb: u64,
}

/// What some of Android's processes cost, in KiB: their pages in memory, and their share of
/// compressed memory (zram) at the compression zram measures. PSS counts what is swapped out
/// at its size before compression, two or three times this. `compressed_kb` is `None` when the
/// compression could not be measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryShare {
    pub resident_kb: u64,
    pub compressed_kb: Option<u64>,
}

impl MemoryShare {
    pub fn total_kb(&self) -> u64 {
        self.resident_kb + self.compressed_kb.unwrap_or(0)
    }

    fn read(share: &Value) -> MemoryShare {
        MemoryShare {
            resident_kb: number(share, "resident_kb"),
            compressed_kb: share["compressed_kb"].as_u64(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestProcess {
    pub pid: i64,
    pub name: String,
    pub pss_kb: u64,
    /// It belongs to Google Play services.
    pub google_play: bool,
    /// What it costs; `None` from a service that does not say.
    pub cost: Option<MemoryShare>,
}

/// Every process of Google Play services together: how many, and what they cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GooglePlayMemory {
    pub processes: u64,
    pub cost: MemoryShare,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestKernel {
    pub slab_unreclaimable_kb: u64,
    pub kernel_stack_kb: u64,
    pub page_tables_kb: u64,
    pub vmalloc_used_kb: u64,
    pub percpu_kb: u64,
    pub cma_total_kb: u64,
    pub mem_online_kb: u64,
    pub unaccounted_kb: u64,
    pub reserved_at_boot_kb: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMemory {
    pub total_kb: u64,
    pub free_kb: u64,
    pub available_kb: u64,
    pub buffers_kb: u64,
    pub cached_kb: u64,
    pub shmem_kb: u64,
    pub slab_reclaimable_kb: u64,
    pub zram_original_kb: u64,
    pub zram_used_kb: u64,
    pub total_pss_kb: u64,
    pub top_processes: Vec<GuestProcess>,
    pub age_seconds: u64,
    pub kernel: Option<GuestKernel>,
    /// What the processes cost together; `None` from a service that does not say.
    pub apps: Option<MemoryShare>,
    /// `None` while none of Google Play services' processes runs.
    pub google_play: Option<GooglePlayMemory>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub guest: Option<GuestMemory>,
    pub host: Vec<HostProcess>,
    pub host_total_kb: u64,
    pub vm_beyond_guest_kb: Option<u64>,
}

impl Memory {
    pub fn read(json: &Value) -> Memory {
        let host = items(json, "host")
            .iter()
            .filter_map(|p| {
                let role = match p["role"].as_str()? {
                    "vm" => HostRole::Vm,
                    "window manager" => HostRole::WindowManager,
                    "graphics renderer" => HostRole::Renderer,
                    "app window" => HostRole::AppWindow,
                    _ => return None,
                };
                Some(HostProcess {
                    role,
                    pid: number(p, "pid"),
                    working_set_kb: number(p, "working_set_kb"),
                    private_kb: number(p, "private_kb"),
                })
            })
            .collect();
        let guest = json.get("guest").filter(|g| g.is_object()).map(|g| {
            let kernel = g
                .get("kernel")
                .filter(|k| k.is_object())
                .map(|k| GuestKernel {
                    slab_unreclaimable_kb: number(k, "slab_unreclaimable_kb"),
                    kernel_stack_kb: number(k, "kernel_stack_kb"),
                    page_tables_kb: number(k, "page_tables_kb"),
                    vmalloc_used_kb: number(k, "vmalloc_used_kb"),
                    percpu_kb: number(k, "percpu_kb"),
                    cma_total_kb: number(k, "cma_total_kb"),
                    mem_online_kb: number(k, "mem_online_kb"),
                    unaccounted_kb: number(k, "unaccounted_kb"),
                    reserved_at_boot_kb: k["reserved_at_boot_kb"].as_u64(),
                });
            GuestMemory {
                total_kb: number(g, "mem_total_kb"),
                free_kb: number(g, "mem_free_kb"),
                available_kb: number(g, "mem_available_kb"),
                buffers_kb: number(g, "buffers_kb"),
                cached_kb: number(g, "cached_kb"),
                shmem_kb: number(g, "shmem_kb"),
                slab_reclaimable_kb: number(g, "slab_reclaimable_kb"),
                zram_original_kb: number(g, "zram_original_kb"),
                zram_used_kb: number(g, "zram_used_kb"),
                total_pss_kb: number(g, "total_pss_kb"),
                top_processes: items(g, "top_processes")
                    .iter()
                    .map(|p| GuestProcess {
                        pid: signed(p, "pid"),
                        name: text(p, "name"),
                        pss_kb: number(p, "pss_kb"),
                        google_play: p["google_play"].as_bool().unwrap_or(false),
                        cost: p.get("resident_kb").map(|_| MemoryShare::read(p)),
                    })
                    .collect(),
                age_seconds: number(g, "age_seconds"),
                kernel,
                apps: g
                    .get("apps")
                    .filter(|a| a.is_object())
                    .map(MemoryShare::read),
                google_play: g.get("google_play").filter(|p| p.is_object()).map(|p| {
                    GooglePlayMemory {
                        processes: number(p, "processes"),
                        cost: MemoryShare::read(p),
                    }
                }),
            }
        });
        Memory {
            guest,
            host,
            host_total_kb: number(json, "host_total_kb"),
            vm_beyond_guest_kb: json["vm_beyond_guest_kb"].as_u64(),
        }
    }
}

/// An app installed in Android.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub package: String,
    pub label: String,
    pub version_name: String,
    pub first_install_time_ms: u64,
    pub system_app: bool,
    pub total_bytes: u64,
    pub icon_png: Vec<u8>,
    /// The app's "Run as ARM" switch.
    pub run_as_arm: RunAsArm,
    /// How far the app may use root: `off`, `ask` or `remember`, or `unavailable` while root
    /// elevation is off for the computer. Empty from a `dsa` that does not say.
    pub root: String,
    /// The user ID the app shares with other apps (`android:sharedUserId`); empty for an app
    /// with a user ID of its own, and from a `dsa` that does not say.
    pub shared_user_id: String,
}

/// The "Run as ARM" switch of an app, as `dsa apps list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAsArm {
    /// The app runs its ARM code through the translator now.
    pub on: bool,
    /// The switch can be changed.
    pub available: bool,
    /// Why it cannot: `system-app`, `no-arm-libraries`, `translation-off` or `arm-only`;
    /// `unknown` from a `dsa` that says nothing of the switch.
    pub reason: String,
}

impl RunAsArm {
    pub fn read(json: &Value) -> RunAsArm {
        if !json.is_object() {
            return RunAsArm {
                on: false,
                available: false,
                reason: "unknown".into(),
            };
        }
        RunAsArm {
            on: flag(json, "on", false),
            available: flag(json, "available", false),
            reason: text(json, "reason"),
        }
    }
}

impl App {
    pub fn read(json: &Value) -> App {
        App {
            package: text(json, "package"),
            label: text(json, "label"),
            version_name: text(json, "version_name"),
            first_install_time_ms: number(json, "first_install_time_ms"),
            system_app: flag(json, "system_app", false),
            total_bytes: number(json, "total_bytes")
                .max(number(json, "app_bytes") + number(json, "data_bytes")),
            icon_png: base64_decode(json["icon_png"].as_str().unwrap_or_default()),
            run_as_arm: RunAsArm::read(&json["run_as_arm"]),
            root: text(json, "root"),
            shared_user_id: text(json, "shared_user_id"),
        }
    }
}

/// How an app's window follows a move to a monitor of another scale. Auto and System never
/// restart the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DpiScaling {
    /// App for apps that say they handle a change of density themselves, System for the
    /// others.
    #[default]
    Auto,
    /// The app redraws at the new monitor's scale. Android recreates the screen of an app that
    /// does not say it handles a change of density, and restores its state.
    App,
    /// The app keeps its scale and its window is scaled to the new monitor, at the same
    /// physical size.
    System,
    /// The app gets the new scale without a restart even when it does not say it handles one.
    /// It needs a change to Android's framework; until a build has it, it works like
    /// [`DpiScaling::System`].
    SystemForced,
}

impl DpiScaling {
    /// Every choice, in the order the settings app offers them.
    pub const ALL: [DpiScaling; 4] = [
        DpiScaling::Auto,
        DpiScaling::App,
        DpiScaling::System,
        DpiScaling::SystemForced,
    ];

    /// The word `dsa app-window --dpi-scaling` takes.
    pub fn code(self) -> &'static str {
        match self {
            DpiScaling::Auto => "auto",
            DpiScaling::App => "app",
            DpiScaling::System => "system",
            DpiScaling::SystemForced => "system-forced",
        }
    }

    /// The choice `dsa` names; anything else, or nothing, is [`DpiScaling::Auto`].
    pub fn read(code: &str) -> DpiScaling {
        DpiScaling::ALL
            .into_iter()
            .find(|scaling| scaling.code() == code)
            .unwrap_or_default()
    }
}

/// The user's window settings for one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppWindow {
    pub package: String,
    pub fixed_size: bool,
    pub start_size: Option<(u32, u32)>,
    pub dpi_scaling: DpiScaling,
}

/// Every app's window settings, and whether this build of Android scales an app the System
/// (forced) way yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppWindows {
    pub apps: Vec<AppWindow>,
    pub system_forced_available: bool,
}

impl AppWindow {
    pub fn read(json: &Value) -> AppWindow {
        let size = json["start_size"].as_array().and_then(|pair| {
            Some((
                pair.first()?.as_u64()? as u32,
                pair.get(1)?.as_u64()? as u32,
            ))
        });
        AppWindow {
            package: text(json, "package"),
            fixed_size: flag(json, "fixed_size", false),
            start_size: size,
            dpi_scaling: DpiScaling::read(json["dpi_scaling"].as_str().unwrap_or_default()),
        }
    }
}

/// A widget an app in Android offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetOffer {
    pub provider: String,
    /// The package of the app that offers it, whose icon stands in for a missing preview.
    pub package: String,
    pub label: String,
    pub app: String,
    pub preview_png: Vec<u8>,
    pub width_dp: i64,
    pub height_dp: i64,
    pub needs_configuration: bool,
}

/// A widget placed on the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedWidget {
    pub id: i64,
    pub label: String,
    pub app: String,
    pub x: i64,
    pub y: i64,
    pub width_dp: i64,
    pub height_dp: i64,
}

impl PlacedWidget {
    pub fn read(json: &Value) -> PlacedWidget {
        let p = &json["placement"];
        PlacedWidget {
            id: signed(json, "id"),
            label: text(json, "label"),
            app: text(json, "app"),
            x: signed(p, "x"),
            y: signed(p, "y"),
            width_dp: signed(p, "width_dp"),
            height_dp: signed(p, "height_dp"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Widgets {
    pub offers: Vec<WidgetOffer>,
    pub placed: Vec<PlacedWidget>,
}

impl Widgets {
    pub fn read(json: &Value) -> Widgets {
        Widgets {
            offers: items(json, "providers")
                .iter()
                .map(|p| WidgetOffer {
                    provider: text(p, "provider"),
                    package: {
                        let package = text(p, "package");
                        if package.is_empty() {
                            text(p, "provider")
                                .split('/')
                                .next()
                                .unwrap_or_default()
                                .to_string()
                        } else {
                            package
                        }
                    },
                    label: text(p, "label"),
                    app: text(p, "app"),
                    preview_png: base64_decode(p["preview_png"].as_str().unwrap_or_default()),
                    width_dp: signed(p, "default_width_dp"),
                    height_dp: signed(p, "default_height_dp"),
                    needs_configuration: flag(p, "needs_configuration", false),
                })
                .collect(),
            placed: items(json, "widgets")
                .iter()
                .map(PlacedWidget::read)
                .collect(),
        }
    }

    /// Applies a change `dsa widgets watch` reported to the placed widgets. The widgets on
    /// offer stay as they were read.
    pub fn apply(&mut self, change: WidgetChange) {
        match change {
            WidgetChange::All(placed) => self.placed = placed,
            WidgetChange::Placed(widget) => {
                match self.placed.iter_mut().find(|w| w.id == widget.id) {
                    Some(shown) => *shown = widget,
                    None => {
                        self.placed.push(widget);
                        self.placed.sort_by_key(|w| w.id);
                    }
                }
            }
            WidgetChange::Removed(id) => self.placed.retain(|w| w.id != id),
        }
    }
}

/// A change to the placed widgets, as `dsa widgets watch --json` reports it: from this page,
/// a widget's own menu on the desktop, or `dsa`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetChange {
    /// The widgets placed now, all of them.
    All(Vec<PlacedWidget>),
    /// A widget was placed, or described again (moved or resized).
    Placed(PlacedWidget),
    Removed(i64),
}

impl WidgetChange {
    /// The change one line of `dsa widgets watch --json` reports; `None` for any other line.
    pub fn read(line: &str) -> Option<WidgetChange> {
        let json: Value = serde_json::from_str(line).ok()?;
        match json["event"].as_str()? {
            "widgets" => Some(WidgetChange::All(
                items(&json, "widgets")
                    .iter()
                    .map(PlacedWidget::read)
                    .collect(),
            )),
            "placed" if json["widget"].is_object() => {
                Some(WidgetChange::Placed(PlacedWidget::read(&json["widget"])))
            }
            "removed" => Some(WidgetChange::Removed(signed(&json, "id"))),
            _ => None,
        }
    }
}

/// The name an app's icon has in the desktop's icon theme, where the compositor keeps it
/// beside the app's launcher entry: `dsa-` and its package.
pub fn app_icon_name(package: &str) -> String {
    format!("dsa-{package}")
}

/// The arguments of the watch of the placed widgets.
pub const WIDGETS_WATCH: [&str; 3] = ["widgets", "watch", "--json"];

/// An app that keeps running in the background, with the notification it keeps shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundApp {
    pub package: String,
    pub label: String,
    pub title: String,
    pub text: String,
    pub icon_png: Vec<u8>,
}

/// An app's media session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    pub package: String,
    pub title: String,
    pub artist: String,
    pub playback: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Background {
    pub apps: Vec<BackgroundApp>,
    pub media: Vec<Media>,
}

impl Background {
    pub fn read(json: &Value) -> Background {
        Background {
            apps: items(json, "apps")
                .iter()
                .map(|a| BackgroundApp {
                    package: text(a, "package"),
                    label: text(a, "label"),
                    title: text(a, "title"),
                    text: text(a, "text"),
                    icon_png: base64_decode(a["icon_png"].as_str().unwrap_or_default()),
                })
                .collect(),
            media: items(json, "media")
                .iter()
                .map(|m| Media {
                    package: text(m, "package"),
                    title: text(m, "title"),
                    artist: text(m, "artist"),
                    playback: text(m, "playback"),
                })
                .collect(),
        }
    }
}

/// The Android version that runs and the ones that can be chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidVersion {
    pub version: String,
    pub major: i64,
    pub build_id: Option<String>,
    pub security_patch: Option<String>,
    /// `latest`, or a major version.
    pub channel: String,
    pub supported: Vec<i64>,
    pub target_major: i64,
    pub notice: Option<String>,
    /// The downloaded image the user's running Android waits to restart into, if one does.
    pub restart_pending: Option<String>,
}

impl AndroidVersion {
    pub fn read(json: &Value) -> AndroidVersion {
        AndroidVersion {
            version: text(json, "version"),
            major: signed(json, "android_major"),
            build_id: optional_text(json, "build_id"),
            security_patch: optional_text(json, "security_patch"),
            channel: optional_text(json, "channel").unwrap_or_else(|| "latest".into()),
            supported: items(json, "supported_majors")
                .iter()
                .filter_map(Value::as_i64)
                .collect(),
            target_major: signed(json, "target_major"),
            notice: optional_text(json, "notice"),
            restart_pending: optional_text(json, "restart_pending").filter(|v| !v.is_empty()),
        }
    }
}

/// Where an ARM translator comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmSource {
    /// The default translator the image of this Android carries.
    Default,
    /// A package the user installed.
    User,
    /// Google's translator, which the user fetched.
    Google,
}

/// The ARM translator Android uses: where it comes from, its name, version and licence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmTranslator {
    pub source: ArmSource,
    pub name: String,
    pub version: String,
    pub license: String,
}

/// Whether ARM apps can run, and with which translation package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmTranslation {
    /// The installed package's name, version and licence, when one is installed.
    pub package: Option<(String, String, String)>,
    /// The name, version and licence of the default package the image carries; only an
    /// x86_64 image has one.
    pub default: Option<(String, String, String)>,
    /// What Android uses from its next start; `None` when it uses no translator.
    pub translator: Option<ArmTranslator>,
    pub enabled: bool,
    pub supported: bool,
    /// The ABIs the running Android installs apps for, when it runs.
    pub abilist: Option<String>,
    pub restart_needed: bool,
    /// Whether Google's ARM translator can be fetched here: not on an ARM64 computer, whose
    /// Android runs ARM apps itself.
    pub google_fetch_available: bool,
}

impl ArmTranslation {
    pub fn read(json: &Value) -> ArmTranslation {
        let described = |key: &str| {
            json.get(key)
                .filter(|p| p.is_object())
                .map(|p| (text(p, "name"), text(p, "version"), text(p, "license")))
        };
        let package = described("package");
        let enabled = flag(json, "enabled", true);
        let translator = match json.get("translator") {
            Some(t) if t.is_object() => {
                let source = match text(t, "source").as_str() {
                    "default" => Some(ArmSource::Default),
                    "google" => Some(ArmSource::Google),
                    "user" => Some(ArmSource::User),
                    _ => None,
                };
                source.map(|source| ArmTranslator {
                    source,
                    name: text(t, "name"),
                    version: text(t, "version"),
                    license: text(t, "license"),
                })
            }
            Some(_) => None,
            // A dsa from before the default translator: the installed package, while on.
            None => package
                .clone()
                .filter(|_| enabled)
                .map(|(name, version, license)| ArmTranslator {
                    source: ArmSource::User,
                    name,
                    version,
                    license,
                }),
        };
        ArmTranslation {
            package,
            default: described("default"),
            translator,
            enabled,
            supported: flag(json, "supported", false),
            abilist: json
                .get("android")
                .filter(|a| a.is_object())
                .map(|a| text(a, "abilist")),
            restart_needed: flag(json, "restart_needed", false),
            google_fetch_available: flag(json, "google_fetch_available", false),
        }
    }
}

/// Google's terms for what its SDK repository gives, the Android SDK License, which cover
/// Google's ARM translator.
pub const GOOGLE_ARM_TERMS_URL: &str = "https://developer.android.com/studio/terms";
/// Where the emulator image Google's ARM translator comes from is downloaded.
pub const GOOGLE_ARM_SOURCE: &str = "https://dl.google.com/android/repository/sys-img/google_apis/";

/// Google's terms, which its apps follow, when the service does not name the page.
pub const GAPPS_TERMS_URL: &str = "https://policies.google.com/terms";
/// Google's page that registers a device it has not certified, when the service does not
/// name it.
pub const GAPPS_REGISTRATION_URL: &str = "https://www.google.com/android/uncertified/";

/// The package of Google's apps DSA installed for the user, as LiteGapps names it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GappsPackage {
    /// LiteGapps' variant, such as `core`.
    pub variant: String,
    /// LiteGapps' build, such as `20261001`.
    pub version: String,
    /// The Android version the package is for, such as `16.0`.
    pub android_version: String,
    pub sdk: u64,
    pub arch: String,
    /// The file it came in, and where it was downloaded from.
    pub file: String,
    pub source: String,
}

/// What the Google Play item shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GappsState {
    /// No package is installed, and the running Android has none.
    NotInstalled,
    /// A package is installed, and the running Android uses it (or does not run).
    Installed,
    /// What is installed differs from what the running Android booted with: a new package
    /// waits for a restart, or a removed one is still in use until then.
    RestartPending,
    /// The installed package's image was made by an older DSA, which this one no longer
    /// uses: Android starts without Google's apps until Google Play is installed again.
    ReinstallNeeded,
}

/// Where Google Play stands for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gapps {
    pub package: Option<GappsPackage>,
    /// Whether this boot of Android has the Google apps laid over its system; `None` while
    /// Android is not running.
    pub active: Option<bool>,
    /// The Google Services Framework Android ID, which Google's registration page asks for,
    /// once Android knows it.
    pub device_id: Option<String>,
    pub restart_needed: bool,
    /// The package must be installed again: see [`GappsState::ReinstallNeeded`].
    pub reinstall_needed: bool,
    pub terms_url: String,
    pub registration_url: String,
}

impl Gapps {
    pub fn read(json: &Value) -> Gapps {
        let package = json
            .get("package")
            .filter(|p| p.is_object())
            .map(|p| GappsPackage {
                variant: text(p, "variant"),
                version: text(p, "version"),
                android_version: text(p, "android_version"),
                sdk: number(p, "sdk"),
                arch: text(p, "arch"),
                file: text(p, "file"),
                source: text(p, "source"),
            });
        let android = json.get("android").filter(|a| a.is_object());
        Gapps {
            package,
            active: android.map(|a| flag(a, "active", false)),
            device_id: android.and_then(|a| optional_text(a, "device_id")),
            restart_needed: flag(json, "restart_needed", false),
            reinstall_needed: flag(json, "reinstall_needed", false),
            terms_url: optional_text(json, "terms_url")
                .unwrap_or_else(|| GAPPS_TERMS_URL.to_string()),
            registration_url: optional_text(json, "registration_url")
                .unwrap_or_else(|| GAPPS_REGISTRATION_URL.to_string()),
        }
    }

    pub fn state(&self) -> GappsState {
        if self.reinstall_needed && self.package.is_some() {
            GappsState::ReinstallNeeded
        } else if self.restart_needed {
            GappsState::RestartPending
        } else if self.package.is_some() {
            GappsState::Installed
        } else {
            GappsState::NotInstalled
        }
    }

    /// Whether the install button may be pressed: without a package, or with one that must
    /// be installed again, and not while a request is under way.
    pub fn can_install(&self, busy: bool) -> bool {
        !busy && (self.package.is_none() || self.state() == GappsState::ReinstallNeeded)
    }

    /// Whether the remove button may be pressed: only with a package, and not while a
    /// request is under way.
    pub fn can_remove(&self, busy: bool) -> bool {
        !busy && self.package.is_some()
    }
}

/// Google's terms for what its SDK repository gives, which Widevine comes from.
pub const WIDEVINE_TERMS_URL: &str = "https://developer.android.com/studio/terms";
/// Where the emulator image Widevine is taken from is downloaded.
pub const WIDEVINE_SOURCE: &str = "https://dl.google.com/android/repository/sys-img/google_apis/";

/// The Widevine module DSA installed for the user, as dsa describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WidevinePackage {
    /// The APEX's file name, `com.google.android.widevine.apex`.
    pub apex: String,
    /// Google's image it came from: its build, API level and revision.
    pub version: String,
    pub android_major: u64,
    pub abi: String,
    pub source: String,
}

/// What the protected content item shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidevineState {
    /// No module is installed, and the running Android has none.
    NotInstalled,
    /// A module is installed, and the running Android uses it (or does not run).
    Installed,
    /// What is installed differs from what the running Android started with.
    RestartPending,
}

/// Where Widevine stands for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Widevine {
    pub package: Option<WidevinePackage>,
    /// Whether Android runs now, and whether it was started with Widevine.
    pub running: bool,
    pub active: bool,
    pub restart_needed: bool,
    /// The security level the module gives: L3.
    pub security_level: String,
}

impl Widevine {
    pub fn read(json: &Value) -> Widevine {
        let package = json
            .get("package")
            .filter(|p| p.is_object())
            .map(|p| WidevinePackage {
                apex: text(p, "apex"),
                version: text(p, "version"),
                android_major: number(p, "android_major"),
                abi: text(p, "abi"),
                source: text(p, "source"),
            });
        Widevine {
            package,
            running: flag(json, "running", false),
            active: flag(json, "active", false),
            restart_needed: flag(json, "restart_needed", false),
            security_level: optional_text(json, "security_level").unwrap_or_else(|| "L3".into()),
        }
    }

    pub fn state(&self) -> WidevineState {
        if self.restart_needed {
            WidevineState::RestartPending
        } else if self.package.is_some() {
            WidevineState::Installed
        } else {
            WidevineState::NotInstalled
        }
    }

    /// Whether the install button may be pressed: without a module, and not while a request is
    /// under way.
    pub fn can_install(&self, busy: bool) -> bool {
        !busy && self.package.is_none()
    }

    /// Whether the remove button may be pressed: only with a module, and not while a request
    /// is under way.
    pub fn can_remove(&self, busy: bool) -> bool {
        !busy && self.package.is_some()
    }
}

/// Where the user's app data backup stands, as `dsa backup --json` reports it. The backup is
/// encrypted, in a folder of the user's home; the key that opens it is kept in the desktop's
/// credential store, and its recovery key opens it anywhere else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Backup {
    /// Whether the user turned backup on. Off until they do: it keeps a second copy of the
    /// apps' user data on this computer.
    pub enabled: bool,
    /// Whether backups can be made and read here.
    pub available: bool,
    /// Why they cannot, when they cannot.
    pub problem: Option<String>,
    /// The backup folder.
    pub folder: String,
    /// When the last backup ended, in milliseconds since 1970; 0 is never.
    pub last_backup_unix_ms: i64,
    /// When the last restore ended, in milliseconds since 1970; 0 is never.
    pub last_restore_unix_ms: i64,
    /// How many apps the backup holds.
    pub packages: u32,
    /// How many bytes of app data it holds, before encryption.
    pub size: u64,
    /// What went wrong last, when something did.
    pub last_error: Option<String>,
    /// After a backup or a restore: the apps it worked for, the apps it failed for, and the
    /// apps a restore installed first.
    pub succeeded: u32,
    pub failed: u32,
    pub installed: u32,
    /// How Android's KeyMint root key is sealed here; `None` from a service that does not say.
    pub keymint_key: Option<KeyProtection>,
}

/// How the root key of Android's KeyMint is sealed on this computer, as `dsa backup --json`
/// reports it under `keymint_key`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyProtection {
    /// Whether a key is kept yet; one is made when Android first starts.
    pub kept: bool,
    /// `tpm`, `secret-service` (and on other hosts `dpapi`, `secure-enclave`, `keychain`).
    pub sealed_by: String,
    /// Whether hardware of this computer holds what seals it.
    pub hardware: bool,
    /// What seals it, in the service's words.
    pub description: String,
    /// Why the TPM does not seal it, when it does not.
    pub fallback_reason: Option<String>,
    /// Why the latest start of Android could not open it, and what to do.
    pub problem: Option<String>,
}

/// What the key protection row says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyProtectionShown {
    /// No key yet.
    NotYet,
    /// Sealed by the computer's TPM.
    Tpm,
    /// Kept by the user's keyring, because no TPM 2.0 works.
    Keyring,
    /// Sealed by other hardware, or kept by another store; the description says which.
    Other,
}

impl KeyProtection {
    pub fn read(json: &Value) -> KeyProtection {
        KeyProtection {
            kept: flag(json, "kept", false),
            sealed_by: text(json, "sealed_by"),
            hardware: flag(json, "hardware", false),
            description: text(json, "description"),
            fallback_reason: optional_text(json, "fallback_reason"),
            problem: optional_text(json, "problem"),
        }
    }

    pub fn shown(&self) -> KeyProtectionShown {
        match (self.kept, self.sealed_by.as_str()) {
            (false, _) => KeyProtectionShown::NotYet,
            (true, "tpm") => KeyProtectionShown::Tpm,
            (true, "secret-service") => KeyProtectionShown::Keyring,
            _ => KeyProtectionShown::Other,
        }
    }

    /// Why no TPM seals the key, to show beside it: only while the key is kept without one.
    pub fn shown_reason(&self) -> Option<&str> {
        if self.kept && !self.hardware {
            self.fallback_reason.as_deref()
        } else {
            None
        }
    }
}

impl Backup {
    pub fn read(json: &Value) -> Backup {
        let count = |name: &str| u32::try_from(number(json, name)).unwrap_or(u32::MAX);
        Backup {
            enabled: flag(json, "enabled", false),
            available: flag(json, "available", false),
            problem: optional_text(json, "problem"),
            folder: text(json, "folder"),
            last_backup_unix_ms: signed(json, "last_backup_unix_ms"),
            last_restore_unix_ms: signed(json, "last_restore_unix_ms"),
            packages: count("packages"),
            size: number(json, "size"),
            last_error: optional_text(json, "last_error"),
            succeeded: count("succeeded"),
            failed: count("failed"),
            installed: count("installed"),
            keymint_key: json
                .get("keymint_key")
                .filter(|key| key.is_object())
                .map(KeyProtection::read),
        }
    }

    /// Whether turning backup off leaves backups behind, which the section offers to delete.
    pub fn has_backups_kept(&self) -> bool {
        self.packages > 0
    }

    /// When the last backup ended, in milliseconds since 1970; `None` when there was none.
    pub fn last_backup(&self) -> Option<i64> {
        (self.last_backup_unix_ms > 0).then_some(self.last_backup_unix_ms)
    }

    /// When the last restore ended, in milliseconds since 1970; `None` when there was none.
    pub fn last_restore(&self) -> Option<i64> {
        (self.last_restore_unix_ms > 0).then_some(self.last_restore_unix_ms)
    }

    /// The problem to show: why backups cannot be made here, or nothing while they can. Off
    /// is the user's choice, not a problem.
    pub fn shown_problem(&self) -> Option<&str> {
        if self.available || (!self.enabled && self.problem.is_none()) {
            None
        } else {
            Some(self.problem.as_deref().unwrap_or_default())
        }
    }
}

/// A request of the backup section that is under way. Only one runs at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupJob {
    /// Reading where the backup stands.
    Read,
    /// Backing up every app now, which waits for Android.
    BackUp,
    /// Installing the backup's apps Android lacks and restoring every app's data, which waits
    /// for Android.
    Restore,
    /// Reading the recovery key.
    ShowKey,
    /// Keeping a recovery key the user entered.
    ImportKey,
    /// Turning backup on or off.
    Switch,
}

impl BackupJob {
    /// Whether the request waits for Android, which can take long, so the section shows that
    /// it is under way.
    pub fn is_long(self) -> bool {
        matches!(self, BackupJob::BackUp | BackupJob::Restore)
    }
}

/// Which of the backup section's buttons can be pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackupActions {
    pub switch: bool,
    pub back_up: bool,
    pub restore: bool,
    pub show_key: bool,
    pub import_key: bool,
}

/// The buttons that work for `backup` (`None` before it is read) while `running` is under
/// way: none while a request runs; backing up and the recovery key only where backups can be
/// made; restoring only from a backup that holds apps. While backup is on a recovery key can
/// always be entered, since it is what makes a backup from another computer readable; while
/// it is off only the switch works.
pub fn backup_actions(backup: Option<&Backup>, running: Option<BackupJob>) -> BackupActions {
    let Some(backup) = backup.filter(|_| running.is_none()) else {
        return BackupActions::default();
    };
    BackupActions {
        switch: true,
        back_up: backup.available,
        restore: backup.available && backup.packages > 0,
        show_key: backup.available,
        import_key: backup.enabled,
    }
}

/// The words of a recovery key as the user typed or pasted it, as `dsa backup key import`
/// takes them: split at spaces, with dashes at the ends of a word dropped (they only group
/// the letters), so nothing reads as an option.
pub fn backup_key_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| word.trim_matches('-'))
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// How entering a recovery key ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyImport {
    /// The key is kept, and the backup reads as it now stands.
    Kept(Backup),
    /// The key was refused; the reason is the service's words, empty when no key was entered.
    Refused(String),
}

impl KeyImport {
    /// Reads a refusal of the key from what the client said: the service's reason, or the
    /// client's own when it did not take the key. `None` for a failure that is not about the
    /// key, such as a service that cannot be reached.
    pub fn refusal(failure: &Failure) -> Option<KeyImport> {
        if failure.problem != Problem::Refused {
            return None;
        }
        let detail = failure.detail.lines().next().unwrap_or_default().trim();
        let detail = detail.strip_prefix("the DSA service: ").unwrap_or(detail);
        Some(KeyImport::Refused(detail.to_string()))
    }
}

/// The device identity: what the user chose (empty where they chose nothing), what Android
/// reports, and where each default came from (`computer` or `dsa`), keyed by the names
/// `dsa settings set device.<name>` takes. `computer` holds the values that follow the
/// computer and are not chosen: the model name, the device name and the chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub chosen: Vec<(String, String)>,
    pub effective: Vec<(String, String)>,
    pub sources: Vec<(String, String)>,
    pub computer: Vec<(String, String)>,
    /// What Android reports when nothing is chosen.
    pub defaults: Vec<(String, String)>,
}

/// This computer's specifications, in the order the page lists them: its name, its
/// manufacturer and model, the model name and the chip.
pub const ABOUT_KEYS: [&str; 6] = [
    "device_name",
    "manufacturer",
    "model",
    "model_name",
    "soc_manufacturer",
    "soc_model",
];

impl Identity {
    pub fn read(json: &Value) -> Identity {
        let fields = || {
            dsa_host_core::device_identity::Field::ALL
                .iter()
                .map(|field| field.key())
        };
        let read = |side: &Value| -> Vec<(String, String)> {
            fields()
                .map(|key| (key.to_string(), text(side, &key.replace('.', "_"))))
                .collect()
        };
        let all = fields().chain(dsa_host_core::device_identity::COMPUTER_KEYS);
        Identity {
            chosen: read(&json["chosen"]),
            effective: read(&json["effective"]),
            // A service from before the computer's identity names no source: DSA's.
            sources: all
                .map(|key| {
                    let origin = match text(&json["sources"], &key.replace('.', "_")).as_str() {
                        "computer" => "computer",
                        _ => "dsa",
                    };
                    (key.to_string(), origin.to_string())
                })
                .collect(),
            computer: dsa_host_core::device_identity::COMPUTER_KEYS
                .iter()
                .map(|key| (key.to_string(), text(&json["effective"], key)))
                .collect(),
            defaults: read(&json["defaults"]),
        }
    }

    /// This computer's own value of `key`, as the firmware and the system report it: one of
    /// the values that follow the computer, or for the manufacturer and the model the default,
    /// when it came from the computer. A value chosen for Android is not the computer's, and
    /// empty means the computer does not say.
    pub fn of_computer(&self, key: &str) -> &str {
        if dsa_host_core::device_identity::COMPUTER_KEYS.contains(&key) {
            self.computer(key)
        } else if self.origin(key) == "computer" {
            Self::find(&self.defaults, key)
        } else {
            ""
        }
    }

    /// This computer's manufacturer and model, as the page shows them under its name.
    pub fn computer_make_model(&self) -> String {
        [self.of_computer("manufacturer"), self.of_computer("model")]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The specifications as text, a row per line with the label and the value apart, as
    /// Copy puts them on the clipboard. `label` names each key and `missing` stands for a
    /// value the computer does not say.
    pub fn about_text(&self, label: impl Fn(&str) -> String, missing: &str) -> String {
        ABOUT_KEYS
            .iter()
            .map(|key| {
                let value = match self.of_computer(key) {
                    "" => missing,
                    value => value,
                };
                format!("{}\t{value}", label(key))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn chosen(&self, key: &str) -> &str {
        Self::find(&self.chosen, key)
    }

    pub fn effective(&self, key: &str) -> &str {
        Self::find(&self.effective, key)
    }

    /// Where the default of `key` came from: `computer` or `dsa`.
    pub fn origin(&self, key: &str) -> &str {
        match Self::find(&self.sources, key) {
            "" => "dsa",
            origin => origin,
        }
    }

    /// One of the values that follow the computer; empty when the computer does not say.
    pub fn computer(&self, key: &str) -> &str {
        Self::find(&self.computer, key)
    }

    fn find<'a>(list: &'a [(String, String)], key: &str) -> &'a str {
        list.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or_default()
    }
}

/// How installing an APK or uninstalling an app ended: done, or the stable reason `dsa`
/// gives and what it said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub done: bool,
    pub status: String,
    pub package: Option<String>,
    pub detail: String,
}

impl Outcome {
    /// Reads `not installed (<reason>): <detail>`, `not uninstalled (<reason>): <detail>` or
    /// `not changed (<reason>): <detail>`, as `dsa` says a refusal; `None` for any other
    /// failure.
    pub fn refusal(failure: &Failure) -> Option<Outcome> {
        let rest = failure
            .detail
            .strip_prefix("not installed (")
            .or_else(|| failure.detail.strip_prefix("not uninstalled ("))
            .or_else(|| failure.detail.strip_prefix("not changed ("))?;
        let (status, detail) = rest.split_once("): ")?;
        let valid =
            !status.is_empty() && status.chars().all(|c| c.is_ascii_lowercase() || c == '-');
        valid.then(|| Outcome {
            done: false,
            status: status.to_string(),
            package: None,
            detail: detail.to_string(),
        })
    }
}

/// One record of Android's log, as `dsa logcat --json` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub time: String,
    pub priority: String,
    pub tag: String,
    pub pid: i64,
    pub tid: i64,
    pub message: String,
    pub dropped: u64,
}

impl LogLine {
    /// Reads one line; `None` for a line that is not a record.
    pub fn parse(line: &str) -> Option<LogLine> {
        let json: Value = serde_json::from_str(line).ok()?;
        json.is_object().then(|| LogLine {
            time: text(&json, "time"),
            priority: text(&json, "priority"),
            tag: text(&json, "tag"),
            pid: signed(&json, "pid"),
            tid: signed(&json, "tid"),
            message: text(&json, "message"),
            dropped: number(&json, "dropped"),
        })
    }

    /// The record as logcat's threadtime format prints it.
    pub fn text(&self) -> String {
        if self.dropped > 0 {
            return format!("--------- {} records dropped", self.dropped);
        }
        format!(
            "{} {:>5} {:>5} {} {}: {}",
            self.time, self.pid, self.tid, self.priority, self.tag, self.message
        )
    }

    /// The log page's columns: time, PID, TID, priority and the body (`tag: message`), as
    /// the Windows settings app splits them. A record that stands for dropped records has
    /// only its body.
    pub fn columns(&self) -> LogColumns {
        if self.dropped > 0 {
            return LogColumns {
                body: self.text(),
                ..LogColumns::default()
            };
        }
        LogColumns {
            time: self.time.clone(),
            pid: self.pid.to_string(),
            tid: self.tid.to_string(),
            priority: self.priority.clone(),
            body: format!("{}: {}", self.tag, self.message),
        }
    }
}

/// One log line split into the page's columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogColumns {
    pub time: String,
    pub pid: String,
    pub tid: String,
    pub priority: String,
    pub body: String,
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

/// The requests the pages make. Every change goes through the service; the app writes no
/// settings file of its own.
#[derive(Clone)]
pub struct Client {
    dsa: Arc<dyn Dsa>,
    elevated: Arc<dyn Dsa>,
    /// Whether the machine's settings are changed through [`Client::elevated`].
    elevate: bool,
}

impl Client {
    /// A client that makes every request through `dsa`, and those only an administrator may
    /// make through `elevated` unless `elevate` is false (the app runs as the administrator).
    pub fn new(dsa: Arc<dyn Dsa>, elevated: Arc<dyn Dsa>, elevate: bool) -> Client {
        Client {
            dsa,
            elevated,
            elevate,
        }
    }

    /// Runs `args` and gives what it printed.
    pub fn raw(&self, args: &[&str]) -> Result<String, Failure> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        self.dsa.run(&args)
    }

    fn json_with(&self, dsa: &dyn Dsa, args: &[&str]) -> Result<Value, Failure> {
        let mut all: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        all.push("--json".into());
        let out = dsa.run(&all)?;
        serde_json::from_str(&out).map_err(|e| {
            Failure::new(
                Problem::Unreadable,
                format!("dsa printed something other than JSON: {e}"),
            )
        })
    }

    fn json(&self, args: &[&str]) -> Result<Value, Failure> {
        self.json_with(&*self.dsa, args)
    }

    fn machine_json(&self, args: &[&str]) -> Result<Value, Failure> {
        if self.elevate {
            self.json_with(&*self.elevated, args)
        } else {
            self.json(args)
        }
    }

    pub fn status(&self) -> Result<Status, Failure> {
        Ok(Status::read(&self.json(&["status"])?))
    }

    /// The status from a running service only. While the service is being replaced this
    /// fails rather than start the old service again.
    pub fn running_status(&self) -> Result<Status, Failure> {
        Ok(Status::read(&self.json(&["status", "--no-start"])?))
    }

    /// Asks the service to install the newest version of DSA now.
    pub fn update_now(&self) -> Result<ProgramUpdateAnswer, Failure> {
        let json = self.json(&["update"])?;
        Ok(ProgramUpdateAnswer {
            installing: optional_text(&json, "installing"),
            detail: text(&json, "detail"),
        })
    }

    /// Starts Android and waits until it has started or failed.
    pub fn start(&self) -> Result<Status, Failure> {
        Ok(Status::read(&self.json(&[
            "start",
            "--wait",
            "--timeout",
            "360",
        ])?))
    }

    /// Stops Android and waits until it has stopped.
    pub fn stop(&self) -> Result<Status, Failure> {
        Ok(Status::read(&self.json(&[
            "stop",
            "--wait",
            "--timeout",
            "120",
        ])?))
    }

    /// Freezes Android until one of its windows is shown again.
    pub fn freeze(&self) -> Result<Status, Failure> {
        Ok(Status::read(&self.json(&["freeze"])?))
    }

    pub fn settings(&self) -> Result<Settings, Failure> {
        Ok(Settings::read(&self.json(&["settings"])?))
    }

    /// Changes one of the user's own settings.
    pub fn set(&self, name: &str, value: &str) -> Result<Settings, Failure> {
        Ok(Settings::read(
            &self.json(&["settings", "set", name, value])?,
        ))
    }

    /// Turns one of the user's own switches on or off.
    pub fn switch(&self, name: &str, on: bool) -> Result<Settings, Failure> {
        self.set(name, on_off(on))
    }

    /// Changes a setting that applies to every user of the computer, which only an
    /// administrator may: image updates, program updates, the release channel
    /// (`prerelease-updates`) and the traces.
    pub fn set_machine(&self, name: &str, on: bool) -> Result<Settings, Failure> {
        Ok(Settings::read(&self.machine_json(&[
            "settings",
            "set",
            name,
            on_off(on),
        ])?))
    }

    /// Whether Android trusts the root certificates a user or an administrator added to this
    /// computer. The user's own setting; it applies at once.
    pub fn set_host_certificates(&self, on: bool) -> Result<Settings, Failure> {
        self.switch("host-certificates", on)
    }

    /// The most memory Android may grow to, in MiB; 0 is automatic.
    pub fn set_memory_limit(&self, mb: u32) -> Result<Settings, Failure> {
        let value = if mb == 0 {
            "auto".to_string()
        } else {
            mb.to_string()
        };
        self.set("memory-limit", &value)
    }

    /// Which folders are shared, in the order the service keeps them.
    pub fn set_shared_folders(&self, folders: &[String]) -> Result<Settings, Failure> {
        let list = folders.join(",");
        self.set(
            "shared-folders",
            if list.is_empty() { "none" } else { &list },
        )
    }

    pub fn network(&self) -> Result<Network, Failure> {
        Ok(Network::read(&self.json(&["network"])?))
    }

    pub fn memory(&self) -> Result<Memory, Failure> {
        Ok(Memory::read(&self.json(&["memory"])?))
    }

    /// The names of this machine's GPUs, and whether each drives the primary monitor.
    pub fn gpus(&self) -> Result<Vec<(String, bool)>, Failure> {
        let json = self.json(&["gpus"])?;
        Ok(items(&json, "gpus")
            .iter()
            .map(|g| (text(g, "name"), flag(g, "primary_display", false)))
            .collect())
    }

    /// Opens Android's shared storage in the file manager, starting Android when it is not
    /// running, and gives the location the file manager opened.
    pub fn open_android_files(&self) -> Result<String, Failure> {
        Ok(text(&self.json(&["files", "open"])?, "path"))
    }

    /// Erases the user's Android data. The Android image and the settings stay.
    pub fn reset_data(&self) -> Result<(), Failure> {
        self.json(&["reset-data", "--yes"]).map(|_| ())
    }

    pub fn arm_translation(&self) -> Result<ArmTranslation, Failure> {
        Ok(ArmTranslation::read(
            &self.json(&["arm-translation", "status"])?,
        ))
    }

    pub fn remove_arm_translation(&self) -> Result<ArmTranslation, Failure> {
        Ok(ArmTranslation::read(
            &self.json(&["arm-translation", "remove"])?,
        ))
    }

    /// Goes back to the default translator the image carries, removing the package installed
    /// in its place. It applies from the next start of Android.
    pub fn use_default_arm_translator(&self) -> Result<ArmTranslation, Failure> {
        Ok(ArmTranslation::read(
            &self.json(&["arm-translation", "use-default"])?,
        ))
    }

    /// Downloads Google's emulator image for this Android from Google, takes its ARM
    /// translator out and installs it, starting Android first when it is not running. The user
    /// agreed to Google's terms in the app first, so dsa does not ask again. It takes minutes.
    pub fn fetch_google_arm_translator(&self) -> Result<ArmTranslation, Failure> {
        let args = ["arm-translation", "fetch", "google", "--yes"];
        Ok(ArmTranslation::read(&self.json(&args)?))
    }

    /// Where Google Play stands.
    pub fn gapps(&self) -> Result<Gapps, Failure> {
        Ok(Gapps::read(&self.json(&["gapps", "status"])?))
    }

    /// Downloads, checks and installs the Google apps package, starting Android first when
    /// it is not running. It takes minutes; Android uses the package from its next start.
    pub fn install_gapps(&self) -> Result<Gapps, Failure> {
        Ok(Gapps::read(&self.json(&["gapps", "install"])?))
    }

    /// Removes the Google apps package. Android is not started for it, so this works while
    /// Google's apps keep Android from starting; Android goes without them from its next start.
    pub fn remove_gapps(&self) -> Result<Gapps, Failure> {
        Ok(Gapps::read(&self.json(&["gapps", "remove"])?))
    }

    /// Where Widevine stands.
    pub fn widevine(&self) -> Result<Widevine, Failure> {
        Ok(Widevine::read(&self.json(&["drm", "widevine", "status"])?))
    }

    /// Downloads Google's emulator image for this Android, takes Widevine out of it unchanged
    /// and installs it. The user agreed to Google's terms in the app first, so dsa does not ask
    /// again. It takes minutes; Android uses the module from its next start.
    pub fn install_widevine(&self) -> Result<Widevine, Failure> {
        let args = ["drm", "widevine", "install", "--yes"];
        Ok(Widevine::read(&self.json(&args)?))
    }

    /// Removes Widevine and the device certificates it kept, from Android's next start.
    pub fn remove_widevine(&self) -> Result<Widevine, Failure> {
        Ok(Widevine::read(&self.json(&["drm", "widevine", "remove"])?))
    }

    /// Where the app data backup stands. It never starts Android.
    pub fn backup(&self) -> Result<Backup, Failure> {
        Ok(Backup::read(&self.json(&["backup", "status"])?))
    }

    /// Turns backup on, or off; off with `delete_backups`, the backups kept so far and their
    /// key are removed as well.
    pub fn set_backup(&self, on: bool, delete_backups: bool) -> Result<Backup, Failure> {
        let args: &[&str] = match (on, delete_backups) {
            (true, _) => &["backup", "on"],
            (false, true) => &["backup", "off", "--delete-backups"],
            (false, false) => &["backup", "off"],
        };
        Ok(Backup::read(&self.json(args)?))
    }

    /// Has Android back up every app now, starting Android first when it is not running, and
    /// waits until it has. It can take long.
    pub fn back_up_now(&self) -> Result<Backup, Failure> {
        Ok(Backup::read(&self.json(&["backup", "now"])?))
    }

    /// Installs the backup's apps that Android lacks, then replaces every app's data with the
    /// backup's, starting Android first when it is not running. It can take long.
    pub fn restore_backup(&self) -> Result<Backup, Failure> {
        Ok(Backup::read(&self.json(&["backup", "restore"])?))
    }

    /// The backup's recovery key, to write down.
    pub fn backup_recovery_key(&self) -> Result<String, Failure> {
        Ok(text(
            &self.json(&["backup", "key", "show"])?,
            "recovery_key",
        ))
    }

    /// Keeps the key the recovery key `text` carries as the backup key. A key the service or
    /// the client refuses, such as one that does not open the backup folder, is a refusal with
    /// its reason; nothing is asked when `text` holds no key.
    pub fn import_backup_key(&self, text: &str) -> Result<KeyImport, Failure> {
        let words = backup_key_words(text);
        if words.is_empty() {
            return Ok(KeyImport::Refused(String::new()));
        }
        let mut args = vec!["backup", "key", "import"];
        args.extend(words.iter().map(String::as_str));
        match self.json(&args) {
            Ok(json) => Ok(KeyImport::Kept(Backup::read(&json))),
            Err(failure) => KeyImport::refusal(&failure).ok_or(failure),
        }
    }

    /// Every app installed in Android; Android is started first when it is not running.
    pub fn apps(&self) -> Result<Vec<App>, Failure> {
        let json = self.json(&["apps", "list"])?;
        Ok(items(&json, "apps").iter().map(App::read).collect())
    }

    pub fn launch(&self, package: &str) -> Result<(), Failure> {
        self.raw(&["launch", package]).map(|_| ())
    }

    /// Opens an app's App info in Android, or the list of apps without a package.
    pub fn open_app_settings(&self, package: Option<&str>) -> Result<(), Failure> {
        match package {
            Some(package) => self.json(&["apps", "settings", package]),
            None => self.json(&["apps", "settings"]),
        }
        .map(|_| ())
    }

    /// Puts a shortcut to an app on the desktop; returns where it is.
    pub fn desktop_shortcut(&self, package: &str) -> Result<String, Failure> {
        self.json(&["apps", "desktop-shortcut", package])
            .map(|json| text(&json, "path"))
    }

    /// Uninstalls an app; a refusal by Android is an outcome with its reason.
    pub fn uninstall(&self, package: &str) -> Result<Outcome, Failure> {
        match self.json(&["apps", "uninstall", package]) {
            Ok(json) => Ok(Outcome {
                done: flag(&json, "uninstalled", false),
                status: text(&json, "status"),
                package: Some(package.to_string()),
                detail: text(&json, "message"),
            }),
            Err(failure) => Outcome::refusal(&failure).ok_or(failure),
        }
    }

    /// Installs an app again with its ARM code (`on`) or its x86_64 code, keeping its data
    /// unless `erase_data`; a refusal (`data-would-be-erased` among them) is an outcome with
    /// its reason.
    pub fn run_as_arm(
        &self,
        package: &str,
        on: bool,
        erase_data: bool,
    ) -> Result<Outcome, Failure> {
        let mut args = vec!["apps", "run-as-arm", package, if on { "on" } else { "off" }];
        if erase_data {
            args.push("--erase-data");
        }
        match self.json(&args) {
            Ok(json) => Ok(Outcome {
                done: text(&json, "status") == "done",
                status: text(&json, "status"),
                package: Some(package.to_string()),
                detail: text(&json, "message"),
            }),
            Err(failure) => Outcome::refusal(&failure).ok_or(failure),
        }
    }

    /// Sets how far an app may use root: `off`, `ask` or `remember`. A refusal by the service
    /// (root elevation off for the computer, or `remember` without an administrator's
    /// approval) is an outcome with the service's words.
    pub fn set_app_root(&self, package: &str, level: &str) -> Result<Outcome, Failure> {
        match self.json(&["apps", "root", package, level]) {
            Ok(json) => Ok(Outcome {
                done: true,
                status: text(&json, "level"),
                package: Some(package.to_string()),
                detail: String::new(),
            }),
            Err(failure) => {
                let refused = failure
                    .detail
                    .strip_prefix("not changed: ")
                    .and_then(|rest| rest.strip_prefix(package))
                    .and_then(|rest| rest.strip_prefix(": "));
                match refused {
                    Some(why) => Ok(Outcome {
                        done: false,
                        status: "refused".into(),
                        package: Some(package.to_string()),
                        detail: why.to_string(),
                    }),
                    None => Err(failure),
                }
            }
        }
    }

    pub fn app_windows(&self) -> Result<Vec<AppWindow>, Failure> {
        Ok(self.app_window_list()?.apps)
    }

    /// Every app's window settings, and whether Android scales the System (forced) way yet.
    /// A `dsa` that does not say counts as one whose Android cannot.
    pub fn app_window_list(&self) -> Result<AppWindows, Failure> {
        let json = self.json(&["app-window"])?;
        Ok(AppWindows {
            apps: items(&json, "apps").iter().map(AppWindow::read).collect(),
            system_forced_available: flag(&json, "system_forced_available", false),
        })
    }

    /// Changes an app's window settings: whether it may be resized, the size it opens at
    /// (`None` is the size Android chooses), and how it follows a monitor of another scale.
    pub fn set_app_window(
        &self,
        package: &str,
        fixed_size: bool,
        start_size: Option<(u32, u32)>,
        dpi_scaling: DpiScaling,
    ) -> Result<(), Failure> {
        let size = match start_size {
            Some((w, h)) => format!("{w}x{h}"),
            None => "auto".into(),
        };
        self.json(&[
            "app-window",
            package,
            "--fixed-size",
            on_off(fixed_size),
            "--start-size",
            &size,
            "--dpi-scaling",
            dpi_scaling.code(),
        ])
        .map(|_| ())
    }

    /// Installs the APK at `path`; a refusal by Android is an outcome with its reason.
    pub fn install(&self, path: &str) -> Result<Outcome, Failure> {
        match self.json(&["install", path]) {
            Ok(json) => Ok(Outcome {
                done: flag(&json, "installed", false),
                status: text(&json, "status"),
                package: optional_text(&json, "package"),
                detail: text(&json, "message"),
            }),
            Err(failure) => Outcome::refusal(&failure).ok_or(failure),
        }
    }

    /// The widgets on offer and the ones placed, starting Android when it is not running.
    pub fn widgets(&self) -> Result<Widgets, Failure> {
        Ok(Widgets::read(&self.json(&["widgets", "list"])?))
    }

    /// Places a widget; when its app asks for a setup, this waits for the user to finish it.
    pub fn add_widget(&self, provider: &str) -> Result<PlacedWidget, Failure> {
        Ok(PlacedWidget::read(
            &self.json(&["widgets", "add", provider])?,
        ))
    }

    pub fn remove_widget(&self, id: i64) -> Result<(), Failure> {
        self.json(&["widgets", "remove", &id.to_string()])
            .map(|_| ())
    }

    /// The apps that run in the background and the media sessions. It never starts Android.
    pub fn background(&self) -> Result<Background, Failure> {
        Ok(Background::read(&self.json(&["background", "list"])?))
    }

    pub fn open_background(&self, package: &str) -> Result<(), Failure> {
        self.json(&["background", "open", package]).map(|_| ())
    }

    /// Force-stops an app, which ends its background work and frees its memory.
    pub fn stop_background(&self, package: &str) -> Result<(), Failure> {
        self.json(&["background", "stop", package]).map(|_| ())
    }

    pub fn android_version(&self) -> Result<AndroidVersion, Failure> {
        Ok(AndroidVersion::read(&self.json(&["android", "version"])?))
    }

    /// Restarts the user's Android now into the downloaded image that waits for it, then
    /// reads the version again.
    pub fn restart_android(&self) -> Result<AndroidVersion, Failure> {
        self.json(&["android", "restart"])?;
        self.android_version()
    }

    /// Chooses an Android version for every user of the computer, which only an
    /// administrator may. Moving to an older one erases every user's Android data.
    pub fn use_android(&self, channel: &str, erase_data: bool) -> Result<AndroidVersion, Failure> {
        let mut args = vec!["android", "use", channel];
        if erase_data {
            args.push("--erase-data");
        }
        Ok(AndroidVersion::read(&self.machine_json(&args)?))
    }

    pub fn identity(&self) -> Result<Identity, Failure> {
        Ok(Identity::read(&self.json(&["settings", "device"])?))
    }

    /// Sets one value of the device identity; an empty value goes back to the default.
    pub fn set_identity(&self, key: &str, value: &str) -> Result<Identity, Failure> {
        let name = format!("device.{key}");
        let json = if value.is_empty() {
            self.json(&["settings", "reset", &name])?
        } else {
            self.json(&["settings", "set", &name, value])?
        };
        Ok(Identity::read(&json))
    }

    pub fn reset_identity(&self) -> Result<Identity, Failure> {
        Ok(Identity::read(
            &self.json(&["settings", "reset", "device"])?,
        ))
    }

    /// Saves the diagnostics of Android and of DSA to `path`, as one zip archive.
    pub fn collect_diagnostics(&self, path: &str) -> Result<String, Failure> {
        self.raw(&["logs", "collect", "-o", path])
    }
}
