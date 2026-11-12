//! `dsa-channeld`: the guest's control channel daemon, as init starts it.
//!
//!     dsa-channeld [--port <port>] [--log-port <port>] [--bootconfig <path>] [--cid <cid>]
//!     dsa-channeld --keymint-key
//!     dsa-channeld --cacerts-setup
//!     dsa-channeld --dns-redirect
//!
//! It reads the boot's secret from the kernel's bootconfig, connects to the host over
//! vsock, and keeps the connection, connecting again whenever it is lost. While the host
//! asks for Android's log, it reads logd and sends the records on the log port, a vsock port
//! of its own. It exits only
//! when it cannot work at all: no secret, or a host that refuses this guest for good.
//! With `--keymint-key` it only fetches KeyMint's root key from the host and exits. With
//! `--cacerts-setup` it only copies Android's system certificates into the folders init binds
//! over them, so the host's roots can be added beside them later, and exits. With
//! `--dns-redirect` (as root) it only makes the packet filter rules that send the guest's DNS
//! queries to its DNS stub match `vendor.dsa.dns`, and exits.
//! Messages go to standard error, which init sends to the kernel log.

#[cfg(any(target_os = "linux", target_os = "android"))]
fn main() {
    std::process::exit(daemon::run());
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn main() {
    eprintln!("dsa-channeld runs only in the Linux guest");
    std::process::exit(2);
}

/// Writes one message line to standard error in a single write. The kernel log makes a
/// record of every write, so a line written in pieces arrives cut into pieces.
#[cfg(any(target_os = "linux", target_os = "android"))]
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let line = format!("dsa-channeld: {}\n", format_args!($($arg)*));
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }};
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod daemon {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;

    use dsa_guest_channeld::arm_translation::{self, ArmTranslation, BootFacts};
    use dsa_guest_channeld::gamepad::{uinput::Uinput, GamepadDevices};
    use dsa_guest_channeld::gapps::{self, Gapps, GuestFacts};
    use dsa_guest_channeld::logbridge::{
        files_in, DiagnosticFiles, LogBridge, LogPackets, LogSource, PortConnector,
    };
    use dsa_guest_channeld::logd::{LogdSocket, LOGDR_SOCKET};
    use dsa_guest_channeld::router::{Peer, Router};
    use dsa_guest_channeld::su_relay::{ConfigApplier, SuRelay};
    use dsa_guest_channeld::{ChannelEvent, Credentials, RetryPolicy};
    use dsa_guest_transport::vsock::{VsockConnector, VMADDR_CID_HOST};
    use dsa_protocol::auth::{secret_from_bootconfig, BOOTCONFIG_KEY};
    use dsa_protocol::control::Role;
    use dsa_protocol::handshake::{guest_connect, local_hello};
    use dsa_transport::Connector;

    /// The host's control channel port unless the command line names another. The host's
    /// configuration has the same default.
    const DEFAULT_PORT: u32 = 5000;
    /// The host's log port unless the command line names another; the host's configuration
    /// has the same default.
    const DEFAULT_LOG_PORT: u32 = 5001;
    const DEFAULT_BOOTCONFIG: &str = "/proc/bootconfig";

    /// logd's reader socket.
    struct Logd;

    struct LogdPackets(LogdSocket);

    impl LogPackets for LogdPackets {
        fn next(&mut self, timeout: std::time::Duration) -> Result<Option<Vec<u8>>, String> {
            let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
            self.0.next(millis).map_err(|e| e.to_string())
        }
    }

    impl LogSource for Logd {
        fn open(&self, request: &str) -> Result<Box<dyn LogPackets>, String> {
            LogdSocket::open(LOGDR_SOCKET, request)
                .map(|socket| Box::new(LogdPackets(socket)) as Box<dyn LogPackets>)
                .map_err(|e| format!("{LOGDR_SOCKET}: {e}"))
        }
    }

    /// What the kernel kept of the boot before, in pstore: the last kernel log of a guest that
    /// crashed, and Android's last log lines.
    struct Pstore;

    const PSTORE: &str = "/sys/fs/pstore";

    impl DiagnosticFiles for Pstore {
        fn files(&self) -> Vec<(String, Result<Vec<u8>, String>)> {
            files_in(std::path::Path::new(PSTORE), "android/pstore")
        }

        /// Unlinking a pstore file erases its record from the pstore disk. Android's init
        /// leaves the folder to system, this daemon's user, without write access (0550), so
        /// the daemon gives itself that first.
        fn delivered(&self, names: &[String]) {
            use std::os::unix::fs::PermissionsExt;
            if names.is_empty() {
                return;
            }
            let dir = std::path::Path::new(PSTORE);
            let writable = std::fs::Permissions::from_mode(0o750);
            if let Err(why) = std::fs::set_permissions(dir, writable) {
                eprintln!("dsa-channeld: {PSTORE} cannot be cleared: {why}");
                return;
            }
            for name in names {
                let Some(file) = name.strip_prefix("android/pstore/") else {
                    continue;
                };
                let path = std::path::Path::new(PSTORE).join(file);
                if let Err(why) = std::fs::remove_file(&path) {
                    eprintln!("dsa-channeld: {} was not cleared: {why}", path.display());
                }
            }
        }
    }

    struct Options {
        port: u32,
        log_port: u32,
        bootconfig: String,
        /// The host's vsock address; another only for a test over vsock loopback.
        cid: u32,
    }

    fn options() -> Result<Options, String> {
        let mut options = Options {
            port: DEFAULT_PORT,
            log_port: DEFAULT_LOG_PORT,
            bootconfig: DEFAULT_BOOTCONFIG.into(),
            cid: VMADDR_CID_HOST,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or(format!("{arg} needs a value"));
            match arg.as_str() {
                "--port" => {
                    let text = value()?;
                    options.port = text
                        .parse()
                        .map_err(|_| format!("--port {text} is not a port number"))?;
                }
                "--log-port" => {
                    let text = value()?;
                    options.log_port = text
                        .parse()
                        .map_err(|_| format!("--log-port {text} is not a port number"))?;
                }
                "--bootconfig" => options.bootconfig = value()?,
                "--cid" => {
                    let text = value()?;
                    options.cid = text
                        .parse()
                        .map_err(|_| format!("--cid {text} is not a vsock address"))?;
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(options)
    }

    /// Registers the binder service the guest's clients call. Without it the daemon still
    /// holds the connection, so the host sees the guest, but no app task gets a window.
    #[cfg(target_os = "android")]
    fn serve_clients(router: &Arc<Router>) {
        match dsa_guest_channeld::binder::serve(Arc::clone(router)) {
            Ok(()) => say!(
                "serving {} to the guest's clients",
                dsa_guest_channeld::aidl::INSTANCE
            ),
            Err(why) => say!("the guest's clients cannot reach the daemon: {why}"),
        }
    }

    /// A Linux system other than Android has no servicemanager to register with.
    #[cfg(target_os = "android")]
    fn check_agent(peer: Peer) -> Result<(), String> {
        dsa_guest_channeld::binder::check_agent(peer)
    }

    /// A Linux system other than Android has no agent.
    #[cfg(not(target_os = "android"))]
    fn check_agent(_peer: Peer) -> Result<(), String> {
        Err("this system has no agent".to_string())
    }

    #[cfg(not(target_os = "android"))]
    fn serve_clients(_router: &Arc<Router>) {
        say!("no binder service off Android; only the connection runs");
    }

    /// Sets a system property through bionic, which hands it to init.
    #[cfg(target_os = "android")]
    fn set_property(name: &str, value: &str) -> Result<(), String> {
        use std::ffi::{c_char, c_int, CString};
        extern "C" {
            fn __system_property_set(name: *const c_char, value: *const c_char) -> c_int;
        }
        let (c_name, c_value) = (
            CString::new(name).map_err(|e| e.to_string())?,
            CString::new(value).map_err(|e| e.to_string())?,
        );
        // SAFETY: both are valid NUL-terminated strings that outlive the call.
        match unsafe { __system_property_set(c_name.as_ptr(), c_value.as_ptr()) } {
            0 => Ok(()),
            code => Err(format!("init refused {name}={value} ({code})")),
        }
    }

    /// Tells the router once Android has finished booting. Init sets the property the daemon
    /// may read when `sys.boot_completed` is set (`dsa-channeld.rc`). The property lasts for
    /// the boot, and the daemon restarts within it, so a late look still sees it.
    #[cfg(target_os = "android")]
    fn watch_boot(router: Arc<Router>) {
        use std::ffi::{c_char, c_int, CStr};
        extern "C" {
            fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
        }
        /// bionic's `PROP_VALUE_MAX`.
        const VALUE_MAX: usize = 92;
        let name = c"vendor.dsa.boot_completed";
        loop {
            let mut value = [0 as c_char; VALUE_MAX];
            // SAFETY: a NUL-terminated name and a buffer of the size bionic writes at most.
            unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
            // SAFETY: bionic leaves a NUL-terminated string there, empty if the property is unset.
            if unsafe { CStr::from_ptr(value.as_ptr()) }.to_bytes() == b"1" {
                say!("Android has finished booting");
                router.boot_completed();
                return;
            }
            thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    /// A Linux system other than Android has no such property.
    #[cfg(not(target_os = "android"))]
    fn watch_boot(_router: Arc<Router>) {}

    /// Has init start adbd (developer mode on) or stop it (`dsa-channeld.rc`). Only init may
    /// start a service; the daemon sets the property it watches.
    #[cfg(target_os = "android")]
    fn switch_adb(on: bool) {
        let value = if on { "1" } else { "0" };
        match set_property("vendor.dsa.adb", value) {
            Ok(()) => say!(
                "developer mode is {}",
                if on { "on: adbd starts" } else { "off" }
            ),
            Err(why) => say!("could not switch adbd for developer mode: {why}"),
        }
    }

    /// Reads a system property through bionic; empty when it is unset.
    #[cfg(target_os = "android")]
    fn read_property(name: &str) -> String {
        use std::ffi::{c_char, c_int, CStr, CString};
        extern "C" {
            fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
        }
        /// bionic's `PROP_VALUE_MAX`.
        const VALUE_MAX: usize = 92;
        let Ok(name) = CString::new(name) else {
            return String::new();
        };
        let mut value = [0 as c_char; VALUE_MAX];
        // SAFETY: a NUL-terminated name and a buffer of the size bionic writes at most.
        unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
        // SAFETY: bionic leaves a NUL-terminated string there, empty if the property is unset.
        unsafe { CStr::from_ptr(value.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    /// A Linux system other than Android has no system properties.
    #[cfg(not(target_os = "android"))]
    fn read_property(_name: &str) -> String {
        String::new()
    }

    /// Off Android there is no adbd to switch.
    #[cfg(not(target_os = "android"))]
    fn switch_adb(_on: bool) {}

    #[cfg(target_os = "android")]
    fn switch_su_helper(on: bool) -> Result<(), String> {
        set_property("vendor.dsa.root_elevation", if on { "1" } else { "0" })
    }

    #[cfg(not(target_os = "android"))]
    fn switch_su_helper(_on: bool) -> Result<(), String> {
        Err("Android init is not available".into())
    }

    /// Has init mount the host's folders the host shares and take the others away
    /// (`dsa-channeld.rc` runs `dsa-channeld --share-mount` as root when the property changes).
    #[cfg(target_os = "android")]
    fn switch_shares(sharing: &dsa_protocol::control::FileSharing) {
        use dsa_guest_channeld::share_mount::{property_value, PROPERTY};
        let value = property_value(sharing);
        match set_property(PROPERTY, &value) {
            Ok(()) => say!("the host's shared folders: {value}"),
            Err(why) => say!("could not have the host's folders mounted: {why}"),
        }
    }

    #[cfg(not(target_os = "android"))]
    fn switch_shares(_sharing: &dsa_protocol::control::FileSharing) {}

    /// Has init serve the shared storage to the host (`dsa-channeld.rc` restarts the files
    /// service when the property changes).
    #[cfg(target_os = "android")]
    fn switch_android_files(files: &dsa_protocol::control::AndroidFiles) {
        use dsa_guest_channeld::android_files::{property_value, PROPERTY};
        let value = property_value(files);
        match set_property(PROPERTY, &value) {
            Ok(()) => say!(
                "Android's files are {}",
                if value == "off" {
                    "not served"
                } else {
                    "served to the host"
                }
            ),
            Err(why) => say!("could not serve Android's files: {why}"),
        }
    }

    #[cfg(not(target_os = "android"))]
    fn switch_android_files(_files: &dsa_protocol::control::AndroidFiles) {}

    /// Makes Android's system certificates hold the root certificates the host added, and no
    /// other of the host's: in the tmpfs folders `dsa-cacerts.rc` binds over Android's, so
    /// every app sees the change at once.
    #[cfg(target_os = "android")]
    fn install_host_certificates(certificates: &dsa_protocol::control::HostCertificates) {
        use dsa_guest_channeld::cacerts::Installer;
        let wanted = if certificates.enabled {
            certificates.certificates.clone()
        } else {
            Vec::new()
        };
        let report = Installer::of_system().install(&wanted);
        say!(
            "the host's root certificates: {} added, {} kept, {} taken away, {} already trusted",
            report.added.len(),
            report.kept.len(),
            report.removed.len(),
            report.already_trusted
        );
        for problem in &report.problems {
            say!("the host's root certificates: {problem}");
        }
    }

    /// Off Android there are no system certificate folders to add to.
    #[cfg(not(target_os = "android"))]
    fn install_host_certificates(_certificates: &dsa_protocol::control::HostCertificates) {}

    /// Hands idle memory back to the host: every so often, decides whether the guest should
    /// reclaim now (see `reclaim`), and has init do it. Only init may write the
    /// kernel's memory controls; the daemon only asks.
    #[cfg(target_os = "android")]
    fn reclaim_idle_memory(router: Arc<Router>) {
        use dsa_guest_channeld::reclaim::{
            file_refaults_kb, memory_pressure, package_running, reclaim_step_mib, MemInfo,
            ReadFailures, Reason, ReclaimPolicy, Reclaimer, CHECK_INTERVAL, EXIT_POLL,
            EXIT_WAIT_LIMIT, RECLAIM_PROPERTY, RECLAIM_STEP_PROPERTY, TRIM_PROPERTY,
            TRIM_SIZE_PROPERTY,
        };
        // The command lines of the guest's processes, for whether a closed app's are gone.
        fn command_lines(failures: &mut ReadFailures) -> Vec<String> {
            let listing = std::fs::read_dir("/proc");
            if let Some(line) = failures.note(
                "/proc",
                &listing,
                "a closed app's memory is reclaimed without waiting for its processes to exit",
            ) {
                say!("{line}");
            }
            let Ok(entries) = listing else {
                return Vec::new();
            };
            entries
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .bytes()
                        .all(|b| b.is_ascii_digit())
                })
                .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .collect()
        }
        // SAFETY: sysconf has no preconditions.
        let page_kb = (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64) / 1024;
        let start = std::time::Instant::now();
        let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
        let mut failures = ReadFailures::default();
        let mut closed = router.apps_closed();
        loop {
            let now_closed = router.wait_for_closed_app(closed, CHECK_INTERVAL);
            let app_closed = now_closed != closed;
            if app_closed {
                closed = now_closed;
                // An app's last window closed: the agent ends its processes, and what they
                // held and the cache they read go back to the host now, not at the next look.
                // The reclaim starts the moment the processes have exited, so it frees what
                // they held; one that the agent keeps (a widget, a foreground service) holds
                // it back by EXIT_WAIT_LIMIT at most.
                let packages = router.take_closed_packages();
                let waited = std::time::Instant::now();
                while waited.elapsed() < EXIT_WAIT_LIMIT {
                    let lines = command_lines(&mut failures);
                    if !packages.iter().any(|p| package_running(&lines, p)) {
                        break;
                    }
                    thread::sleep(EXIT_POLL);
                }
                say!(
                    "{} closed: returning its memory to the host ({} ms after its window)",
                    packages.join(", "),
                    waited.elapsed().as_millis()
                );
            }
            let memory = match std::fs::read_to_string("/proc/meminfo") {
                Ok(text) => MemInfo::parse(&text),
                Err(error) => {
                    say!("cannot read /proc/meminfo, so idle memory is not reclaimed: {error}");
                    return;
                }
            };
            let tasks = router.tasks_in_use();
            reclaimer.host_short(router.host_memory_low());
            // Pressure stall information is optional in the kernel: unreadable counts as busy.
            let psi = std::fs::read_to_string("/proc/pressure/memory");
            if let Some(line) = failures.note(
                "/proc/pressure/memory",
                &psi,
                "the guest counts as under memory pressure and the gradual trim never runs",
            ) {
                say!("{line}");
            }
            let pressure = psi.map(|text| memory_pressure(&text, 5.0)).unwrap_or(true);
            // Evicted cache read back says the working set is larger than the trim assumed.
            let vmstat = std::fs::read_to_string("/proc/vmstat");
            if let Some(line) = failures.note(
                "/proc/vmstat",
                &vmstat,
                "the trim cannot tell when it evicted cache the guest still uses",
            ) {
                say!("{line}");
            }
            if let Some(refaults) = vmstat
                .ok()
                .and_then(|text| file_refaults_kb(&text, page_kb))
            {
                reclaimer.refaults(refaults);
            }
            let reason = if app_closed {
                reclaimer.app_closed(start.elapsed(), tasks, &memory)
            } else {
                reclaimer.decide_under(start.elapsed(), tasks, &memory, pressure)
            };
            if let Some(reason) = reason {
                let property = if reason == Reason::Gradual {
                    let size = reclaimer.trim_mib().to_string();
                    say!(
                        "trimming {size} MiB of cold page cache (working set target {} MiB)",
                        reclaimer.working_set_target_kb() / 1024
                    );
                    if let Err(why) = set_property(TRIM_SIZE_PROPERTY, &size) {
                        say!("could not size the trim: {why}");
                        continue;
                    }
                    TRIM_PROPERTY
                } else {
                    let step = reclaim_step_mib(&memory).to_string();
                    if let Err(why) = set_property(RECLAIM_STEP_PROPERTY, &step) {
                        say!("could not size the reclaim: {why}");
                        continue;
                    }
                    RECLAIM_PROPERTY
                };
                say!(
                    "reclaiming the guest's memory ({reason:?}, {tasks} tasks in use): {} MiB \
                     droppable and {} MiB anonymous of {} MiB in use",
                    memory.droppable_kb() / 1024,
                    memory.anon_pages_kb / 1024,
                    memory.used_kb() / 1024
                );
                if let Err(why) = set_property(property, "1") {
                    say!("could not ask init to reclaim: {why}");
                }
            }
        }
    }

    /// Off Android there is no init to ask, and the host is not Hyper-V.
    #[cfg(not(target_os = "android"))]
    fn reclaim_idle_memory(_router: Arc<Router>) {}

    /// Pages an app that no window shows back out to zram a few seconds after something woke
    /// it (see `pageout`), instead of leaving what the wake read back resident until the next
    /// idle reclaim. Logs what each pageout returned. Starts once Android has booted, so the
    /// boot's own work is left alone, and stops for good if the kernel or the policy refuses it.
    #[cfg(target_os = "android")]
    fn page_out_after_wakes(router: Arc<Router>) {
        use dsa_guest_channeld::pageout::system::{page_out, samples, Refused};
        use dsa_guest_channeld::pageout::{Pageout, PageoutPolicy, SAMPLE_INTERVAL};
        // The daemon runs ahead of apps so pointer events never wait; a pageout is work that
        // may wait for them, so this thread runs behind them.
        // SAFETY: setpriority on this thread's own id has no other effect.
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10);
        }
        while read_property("vendor.dsa.boot_completed") != "1" {
            thread::sleep(SAMPLE_INTERVAL);
        }
        let start = std::time::Instant::now();
        let mut pageout = Pageout::new(PageoutPolicy::default());
        let mut unreadable = false;
        loop {
            thread::sleep(SAMPLE_INTERVAL);
            let looked = match samples() {
                Ok(looked) => {
                    unreadable = false;
                    looked
                }
                Err(error) => {
                    if !unreadable {
                        say!("cannot list the processes, so no app is paged out after it wakes: {error}");
                        unreadable = true;
                    }
                    continue;
                }
            };
            let windows = router.task_packages();
            for target in pageout.look(start.elapsed(), &looked, &windows) {
                match page_out(&target) {
                    Ok(done) => {
                        pageout.paged(target.pid, done.after.rss_anon_kb);
                        say!(
                            "paged out {} (pid {}, oom_score_adj {}{}) after it woke: resident \
                             anonymous {} -> {} MiB, swapped out {} -> {} MiB, {} MiB asked for, \
                             {} ranges refused, {} ms",
                            target.name,
                            target.pid,
                            target.oom_score_adj,
                            if target.google_play {
                                ", Google Play services"
                            } else {
                                ""
                            },
                            done.before.rss_anon_kb / 1024,
                            done.after.rss_anon_kb / 1024,
                            done.before.swap_kb / 1024,
                            done.after.swap_kb / 1024,
                            done.advised_bytes / (1024 * 1024),
                            done.refused_ranges,
                            done.took.as_millis()
                        );
                    }
                    Err(Refused::Skipped(_)) => {}
                    Err(Refused::NotAllowed(why)) => {
                        say!(
                            "WARNING: {why}. Apps woken in the background stay resident until \
                             the next idle reclaim; no pageout is tried again until the daemon \
                             restarts."
                        );
                        return;
                    }
                }
            }
        }
    }

    /// Off Android there are no app processes to page out.
    #[cfg(not(target_os = "android"))]
    fn page_out_after_wakes(_router: Arc<Router>) {}

    /// Writes what stays cold in zram on to its backing file while no app is in use (see
    /// `writeback`): every so often decides whether a run is due, gives init the run's budget,
    /// has init run it, and logs and counts what it wrote. Only init may write zram's files;
    /// the daemon only asks and reads. Stops for good when zram has no backing file.
    #[cfg(target_os = "android")]
    fn write_back_cold_zram(router: Arc<Router>) {
        use dsa_guest_channeld::writeback::{
            day_of, has_backing_device, BackingStat, Day, Run, Writeback, WritebackPolicy,
            ZramStat, CHECK_INTERVAL, RUN_WAIT_LIMIT, WRITEBACK_AFTER_PROPERTY,
            WRITEBACK_AGE_PROPERTY, WRITEBACK_DAY_PROPERTY, WRITEBACK_PAGES_PROPERTY,
            WRITEBACK_PROPERTY, ZRAM,
        };
        fn today() -> u64 {
            day_of(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
            )
        }
        fn zram_stat() -> Option<ZramStat> {
            ZramStat::parse(&std::fs::read_to_string(format!("{ZRAM}/mm_stat")).ok()?)
        }
        fn backing_stat() -> Option<BackingStat> {
            BackingStat::parse(&std::fs::read_to_string(format!("{ZRAM}/bd_stat")).ok()?)
        }
        while read_property("vendor.dsa.boot_completed") != "1" {
            thread::sleep(CHECK_INTERVAL);
        }
        match std::fs::read_to_string(format!("{ZRAM}/backing_dev")) {
            Ok(name) if has_backing_device(&name) => {}
            Ok(_) => {
                say!(
                    "zram has no backing file, so what stays cold in it stays in memory: fs_mgr \
                     could not make /data/per_boot/zram_swap (look for \"zram backing device\" \
                     in the log of init)"
                );
                return;
            }
            Err(error) => {
                say!(
                    "cannot read zram's backing device ({error}), so what stays cold in zram \
                     stays in memory"
                );
                return;
            }
        }
        let policy = WritebackPolicy::with_times(
            &read_property(WRITEBACK_AFTER_PROPERTY),
            &read_property(WRITEBACK_AGE_PROPERTY),
        );
        let start = std::time::Instant::now();
        let mut writeback = Writeback::new(
            policy,
            today(),
            Day::parse(&read_property(WRITEBACK_DAY_PROPERTY)),
        );
        say!(
            "zram writes what no one has used for {} s to its backing file once no app has been \
             in use for {} s, at most {} MiB a run and {} MiB a day ({} MiB left today)",
            policy.age.as_secs(),
            policy.idle_after.as_secs(),
            policy.run_pages * 4 / 1024,
            policy.daily_pages * 4 / 1024,
            writeback.left_pages() * 4 / 1024
        );
        loop {
            thread::sleep(CHECK_INTERVAL);
            let Some(before) = zram_stat() else {
                continue;
            };
            let Some(pages) =
                writeback.decide(start.elapsed(), today(), router.tasks_in_use(), &before)
            else {
                continue;
            };
            let backing_before = backing_stat().unwrap_or_default();
            if let Err(why) = set_property(WRITEBACK_PAGES_PROPERTY, &pages.to_string()) {
                say!("could not give the zram writeback its budget: {why}");
                continue;
            }
            let began = std::time::Instant::now();
            if let Err(why) = set_property(WRITEBACK_PROPERTY, "1") {
                say!("could not ask init for the zram writeback: {why}");
                continue;
            }
            while read_property(WRITEBACK_PROPERTY) == "1" && began.elapsed() < RUN_WAIT_LIMIT {
                thread::sleep(std::time::Duration::from_millis(100));
            }
            let run = Run {
                before,
                after: zram_stat().unwrap_or(before),
                backing_before,
                backing_after: backing_stat().unwrap_or(backing_before),
                took: began.elapsed(),
                left_pages: 0,
                daily_pages: policy.daily_pages,
            };
            let day = writeback.wrote(run.written_pages());
            if let Err(why) = set_property(WRITEBACK_DAY_PROPERTY, &day.to_property()) {
                say!("could not keep what the zram writeback wrote today: {why}");
            }
            say!(
                "{}",
                Run {
                    left_pages: writeback.left_pages(),
                    ..run
                }
                .line()
            );
        }
    }

    /// Off Android there is no zram backing file.
    #[cfg(not(target_os = "android"))]
    fn write_back_cold_zram(_router: Arc<Router>) {}

    /// Tells the host how much memory the guest has, so it can add memory before the guest
    /// runs out and take it back once the guest no longer needs it.
    fn report_memory_pressure(router: Arc<Router>) {
        use dsa_guest_channeld::pressure::{PressureReporter, Sample, SAMPLE_INTERVAL};
        let mut reporter = PressureReporter::default();
        loop {
            thread::sleep(SAMPLE_INTERVAL);
            let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else {
                continue;
            };
            let psi = std::fs::read_to_string("/proc/pressure/memory").unwrap_or_default();
            let sample = Sample::parse(&meminfo, &psi);
            if reporter.due(&sample, std::time::Instant::now()) {
                router.to_host(sample.frame());
            }
        }
    }

    /// `--keymint-key`: KeyMint's root key from the host, into the tmpfs file only KeyMint
    /// reads. init runs it once, early in boot, as KeyMint's user (`dsa-channeld.rc`).
    fn keymint_key() -> i32 {
        use dsa_guest_channeld::keymint_key::{self, Retry};
        let secret = std::fs::read_to_string(DEFAULT_BOOTCONFIG)
            .ok()
            .and_then(|text| secret_from_bootconfig(&text));
        let hello = local_hello(
            Role::Guest,
            concat!("dsa-channeld ", env!("CARGO_PKG_VERSION"), " (keymint)"),
        );
        let mut connector = VsockConnector {
            cid: VMADDR_CID_HOST,
            port: keymint_key::PORT,
        };
        keymint_key::run(
            &mut connector,
            &hello,
            secret.as_ref(),
            std::path::Path::new(keymint_key::DIR),
            &Retry::default(),
            &|line: &str| say!("{line}"),
        )
    }

    /// Switches the packet filter rules that send the guest's DNS queries to the stub: init
    /// runs `dsa-channeld --dns-redirect` when the property changes (`dsa-channeld.rc`).
    #[cfg(target_os = "android")]
    fn switch_dns(on: bool) {
        use dsa_guest_channeld::dns_stub::PROPERTY;
        if let Err(why) = set_property(PROPERTY, if on { "on" } else { "off" }) {
            say!("could not switch the guest's DNS to the host's resolver: {why}");
        }
    }

    /// Off Android there is no init to set the packet filter.
    #[cfg(not(target_os = "android"))]
    fn switch_dns(_on: bool) {}

    /// The guest's DNS, answered by the host's resolver: the stub on the loopback addresses,
    /// and the connection to the host's DNS port it hands each query to, past the same
    /// handshake as the control channel. The redirect to the stub is on only while the host
    /// answers, so the guest keeps its network's DNS servers otherwise.
    fn serve_dns(credentials: &Credentials, cid: u32) {
        use dsa_guest_channeld::dns_stub::{self, Stub, Tunnel};
        let stub = match Stub::bind(dns_stub::STUB_PORT) {
            Ok(stub) => stub,
            Err(why) => {
                say!(
                    "the DNS stub cannot listen ({why}); the guest asks its network's DNS \
                     servers"
                );
                switch_dns(false);
                return;
            }
        };
        let credentials = credentials.clone();
        let connect: Box<dns_stub::DnsConnector> = Box::new(move || {
            let mut connection = VsockConnector {
                cid,
                port: dns_stub::PORT,
            }
            .connect(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
            guest_connect(
                connection.as_mut(),
                &credentials.hello,
                &credentials.secret,
                std::time::Duration::from_secs(10),
            )
            .map_err(|e| e.to_string())?;
            Ok(connection)
        });
        let tunnel = Arc::new(Tunnel::new(connect, Box::new(switch_dns)));
        stub.serve(Arc::clone(&tunnel), Arc::new(|line: &str| say!("{line}")));
        let spawned = thread::Builder::new()
            .name("dsa-dns-host".into())
            .spawn(move || {
                let stop = AtomicBool::new(false);
                tunnel.run(&stop, &|line| say!("{line}"));
            });
        if let Err(why) = spawned {
            say!(
                "the guest's DNS cannot reach the host ({why}); it asks its network's DNS servers"
            );
            switch_dns(false);
        }
    }

    /// `--dns-redirect`: makes the packet filter send the guest's DNS queries to the stub, or
    /// stop sending them, as `vendor.dsa.dns` says. init runs it as root.
    fn dns_redirect() -> i32 {
        let run = |program: &str, args: &[String]| -> Result<(), String> {
            let output = std::process::Command::new(program)
                .args(args)
                .output()
                .map_err(|e| e.to_string())?;
            if output.status.success() {
                return Ok(());
            }
            let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(if said.is_empty() {
                format!("it ended with {}", output.status)
            } else {
                said
            })
        };
        dsa_guest_channeld::dns_redirect::run(&read_property, &run, &|line| say!("{line}"))
    }

    pub fn run() -> i32 {
        // The two jobs init runs this program for besides the daemon: mounting the host's
        // folders (as root), and serving the shared storage to the host (as media_rw).
        let say_line = |line: &str| say!("{line}");
        if std::env::args().nth(1).as_deref() == Some("--su-relay") {
            return match dsa_guest_channeld::su_helper::run() {
                Ok(()) => 0,
                Err(error) => {
                    say!("su helper failed: {error}");
                    1
                }
            };
        }
        if std::env::args().nth(1).as_deref() == Some("--share-mount") {
            return dsa_guest_channeld::share_mount::run(&read_property, &say_line);
        }
        if std::env::args().nth(1).as_deref() == Some("--android-files") {
            return dsa_guest_channeld::android_files::run(&read_property, &say_line);
        }
        if std::env::args().nth(1).as_deref() == Some("--keymint-key") {
            return keymint_key();
        }
        if std::env::args().nth(1).as_deref() == Some("--dns-redirect") {
            return dns_redirect();
        }
        if std::env::args().nth(1).as_deref() == Some("--cacerts-setup") {
            use dsa_guest_channeld::cacerts::{setup, FOLDERS, ROOT};
            let problems = setup(std::path::Path::new(ROOT), &FOLDERS);
            for problem in &problems {
                say!("Android's system certificates: {problem}");
            }
            // A folder that could not be copied is not there, so init's bind of it fails and
            // Android's own folder stays in use.
            return 0;
        }
        let options = match options() {
            Ok(options) => options,
            Err(why) => {
                say!("{why}");
                return 2;
            }
        };
        let secret = match std::fs::read_to_string(&options.bootconfig) {
            Ok(text) => match secret_from_bootconfig(&text) {
                Some(secret) => secret,
                None => {
                    say!(
                        "{} has no {BOOTCONFIG_KEY}; the host did not start this \
                         boot, so there is nothing to connect to",
                        options.bootconfig
                    );
                    return 1;
                }
            },
            Err(error) => {
                say!("could not read {}: {error}", options.bootconfig);
                return 1;
            }
        };
        let credentials = Credentials {
            hello: local_hello(
                Role::Guest,
                concat!("dsa-channeld ", env!("CARGO_PKG_VERSION")),
            ),
            secret,
        };
        let mut connector = VsockConnector {
            cid: options.cid,
            port: options.port,
        };

        // The guest's clients (the agent and the hwcomposer) reach the daemon over binder.
        // Frames they send go to the host through `to_host`; the host's frames go to them.
        let (to_host, from_guest) = mpsc::channel::<Vec<u8>>();
        // The host's game controllers become uinput gamepads, whose rumble goes to the host.
        let gamepads = Arc::new(GamepadDevices::new(Box::new(Uinput), to_host.clone()));
        // Root elevation: the host's config drives the kernel's su guard, on connect and on
        // every change, and the host's decisions answer the su calls the relay holds.
        let su_relay = Arc::new(SuRelay::new());
        let su_config = ConfigApplier::of_system_with_host(
            Arc::clone(&su_relay),
            to_host.clone(),
            Box::new(switch_su_helper),
            |line| say!("{line}"),
        );
        // Until the host says otherwise, su exists for no app, whatever the image holds.
        su_config.hide_all();
        let router = Arc::new(
            Router::new(to_host, check_agent)
                .on_developer_mode(switch_adb)
                .on_file_sharing(switch_shares)
                .on_android_files(switch_android_files)
                .on_host_certificates(install_host_certificates)
                .on_arm_translation(Arc::new(ArmTranslation::new(
                    arm_translation::DIR,
                    BootFacts::of_system(|| read_property("ro.product.cpu.abilist")),
                )))
                .on_gapps(Arc::new(Gapps::new(
                    gapps::DIR,
                    GuestFacts::of_system(read_property),
                )))
                .on_gamepads(gamepads)
                .on_root_elevation(Arc::clone(&su_relay), move |config| {
                    su_config.apply(config);
                }),
        );
        let (to_guest, from_host) = mpsc::channel::<Vec<u8>>();
        // The guest's log goes on a port of its own, past the same handshake as the control
        // channel, and only while the host asks for it.
        let connect_log: Arc<PortConnector> = {
            let (credentials, cid, port) = (credentials.clone(), options.cid, options.log_port);
            Arc::new(move || {
                let mut connection = VsockConnector { cid, port }
                    .connect(std::time::Duration::from_secs(10))
                    .map_err(|e| e.to_string())?;
                guest_connect(
                    connection.as_mut(),
                    &credentials.hello,
                    &credentials.secret,
                    std::time::Duration::from_secs(10),
                )
                .map_err(|e| e.to_string())?;
                Ok(connection)
            })
        };
        let bridge = {
            let router = Arc::clone(&router);
            Arc::new(LogBridge::new(
                Arc::new(Logd),
                Arc::new(Pstore),
                connect_log,
                Arc::new(move || router.agent_connected()),
            ))
        };
        router.divert_diagnostics(bridge.agent_frames());
        {
            let (router, bridge) = (Arc::clone(&router), Arc::clone(&bridge));
            thread::spawn(move || {
                for frame in from_host {
                    if !bridge.from_host(&frame) {
                        router.from_host(&frame);
                    }
                }
            });
        }
        serve_clients(&router);
        serve_dns(&credentials, options.cid);
        {
            let router = Arc::clone(&router);
            thread::spawn(move || reclaim_idle_memory(router));
        }
        {
            let router = Arc::clone(&router);
            thread::spawn(move || page_out_after_wakes(router));
        }
        {
            let router = Arc::clone(&router);
            thread::spawn(move || write_back_cold_zram(router));
        }
        {
            let router = Arc::clone(&router);
            thread::spawn(move || report_memory_pressure(router));
        }
        {
            let router = Arc::clone(&router);
            thread::spawn(move || watch_boot(router));
        }
        let (events, event_rx) = mpsc::channel();
        thread::spawn(move || {
            for event in event_rx {
                match event {
                    ChannelEvent::Connected { host_build } => {
                        say!("connected to the host ({host_build})");
                        // A host that connects again, or a new host, learns the tasks the
                        // guest already has; the clients report a task only when it changes.
                        router.host_connected();
                    }
                    ChannelEvent::Lost(why) => {
                        // Nobody reads the log until a host asks again.
                        bridge.host_lost();
                        say!("lost the host ({why}); connecting again")
                    }
                }
            }
        });

        say!("connecting to the host on vsock port {}", options.port);
        let stop = AtomicBool::new(false);
        let ended = dsa_guest_channeld::run(
            &mut connector,
            &credentials,
            RetryPolicy::default(),
            &to_guest,
            &from_guest,
            &events,
            &stop,
        );
        say!("{ended}");
        1
    }
}
