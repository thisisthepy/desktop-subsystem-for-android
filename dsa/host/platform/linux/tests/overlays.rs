//! The images init puts in the system on the real image, with SELinux enforcing: the default
//! ARM translation package and a user's Google apps.
//!
//! Each is an erofs image that init mounts with `context=` (the translation labelled as system
//! libraries, the Google apps as system files), before the zygote starts. erofs reads each
//! image file itself, with `directio`: no loop device may back either mount, and the mount's
//! source must be the image file, because a loop device kept every page read from the image
//! twice, once in erofs's cache and once in the image file's. A user's add-on images (the
//! Google apps, a translation package they installed) are files on their add-on disk, which
//! the host attaches read only and init mounts at `/mnt/dsa-addons` from its partition: never
//! from `/data`, which is encrypted and so gives the image file no direct I/O, and with nothing
//! of them left there (the guest says whether an image an older DSA sent is still in `/data`). A `context=` mount is checked
//! against the type it names as well as the filesystem's own, and when the policy allowed only
//! the latter every boot refused both mounts
//! ("avc: denied { relabelfrom } ... tclass=filesystem"): Android started with no translator and
//! no Google Play, and nothing but the log said so.
//!
//! Nothing is laid over a folder of the system's own programs. While the translation package
//! was laid over `/system/bin`, every service there ran through overlayfs, which looked it up
//! with init's credentials; init may not read most services' programs, so bpfloader's exec
//! failed and Android rebooted 5.6 s into every boot ("avc: denied { getattr } ...
//! name="bpfloader" scontext=u:r:init:s0 tcontext=u:object_r:bpfloader_platform_exec:s0",
//! "overlayfs: failed to get metacopy (-13)"). The package is instead bound, folder by folder,
//! onto empty places the image carries: its libraries onto `/system_ext/lib64`, where the
//! zygote finds the bridge by name, its ARM64 libraries, linker and app_process onto
//! `/system/lib64/arm64` and `/system/bin/arm64`, its runners onto
//! `/system/bin/dsa-arm-translation`, its linker configuration onto
//! `/system/etc/ld.config.arm64.txt`, and its exec region library onto
//! `/system/lib64/libberberis_exec_region.so`, the absolute path Berberis opens it by. Until
//! that was bound, the files were all in place and every ARM app still aborted as it started
//! ("exec_region_elf_backed.cc:48: CHECK failed: 0 != 0"). The Google apps are laid over the folders of `/product`
//! and `/system_ext` that hold apps and their configuration, never over a partition's root,
//! whose `bin` holds services too. statfs on any of those is checked against the image's
//! superblock, so every program may read it only while the superblock has a type of its own.
//!
//! The service boots the image, which carries its default translator: the package must be
//! mounted, each part of it bound where it belongs, nothing mounted on `/system/bin`,
//! `/system/lib64` or `/system/etc`, binfmt_misc mounted, an ARM64 program of each kind the
//! package's two rules take (a fixed-address executable and a position-independent one)
//! running through the translator and writing what it was made to write, the bridge library
//! where the zygote looks, and the ARM64 ABI offered, with no native bridge failure in the log.
//! `pm list features` must report OpenGL ES 3.2 (`reqGlEsVersion=0x30002`) and the features
//! Google Play takes for granted, or Play hides the apps that need them. The platform lets no domain but init read binfmt_misc, so the shell cannot look at the
//! registered rules; a program that runs proves its rule is registered, enabled, and starts a
//! runner that is there and works, which reading the rule never did. An ARM app is then
//! installed and started: the ARM probe, whose only code is an ARM64 library that the framework's
//! NativeActivity loads through the native bridge. Android must install it for arm64-v8a, its
//! ARM64 code must run and log its line, and its process must not crash or abort, which is what
//! the translator did to every ARM app while its exec region library was missing. A program run
//! through binfmt_misc never loads that library, so only an app shows it. Then a small Google apps build, laid
//! out as LiteGapps publishes one, is installed through the local API from a mirror in the
//! test's folder, and Android starts again: the image must be mounted and laid over each of
//! its folders, with the translator still in place, the host's privileged permission allowlist
//! granting the privileged permission the build's Google Play services asks for and the build's
//! own list leaves out, and `ro.control_privapp_permissions` at `log`, so that a permission no
//! list names is logged rather than stopping Android at every boot; no boot may log one. In
//! that boot the framework must name Google Play services its device configurator, the compile
//! of the Google apps without an app image must run to its end and leave the app image format
//! at lz4, and no alarm may fire as a storm (60 times or more, more than once per 10 s on average
//! since boot). The
//! small build's APKs are not signed, so the package manager does not install them; the
//! allowlist is read from the system as the package manager would read it. Across both boots,
//! the guest log and the Android console log must show no init command of either script
//! failing, no denial of init's
//! mounts, no denial that names the translator or one of its places, no overlayfs failure, no
//! program of any domain refused filesystem getattr on an image's superblock, and no service
//! refused its domain change from a nosuid mount. A boot that never gets far enough to be
//! checked fails with those refusals in its message. Both boots must also keep at most 8 cached
//! app processes, as `dumpsys activity settings` reports the limit the activity manager applies
//! (`CUR_MAX_CACHED_PROCESSES`): Android 17 reads it from the device configuration and the
//! framework's resource, not from the global setting it once read, and with the limit only in
//! that setting it applied 1024. Google Play services must not change that.
//!
//! With the Google apps in place, the agent must keep Play services' desktop-irrelevant
//! services off. The small build's Play services is not installed (it is not signed), so an app
//! named as Play services that declares those services stands in for it (the Play services
//! probe): once it is installed they must be off and its push service on, and after they are
//! turned back on and the probe is installed over itself, as an update is, off again.
//!
//! A second boot check measures the idle reclaim with Google apps installed and no app window
//! open, for the decision on backing off its interval (`idle_reclaim`). It installs LiteGapps'
//! own newest build for the guest, downloaded into the mirror (the small stand-in build holds
//! no Play services that run, so it is used only when LiteGapps cannot be reached, and the
//! report says so), starts Android again, and samples the guest once a second for five
//! minutes: each run of the reclaim, the kernel time init spends in it, the guest's processor
//! time, the refaults and reads around each run, and which processes faulted file pages in
//! between. The report goes to `idle-reclaim.md` and `idle-reclaim.json` in the check's folder,
//! with the raw samples and the guest log beside them. It fails when it cannot measure, and
//! when an alarm fired as a storm in that boot, the one with a Play services that runs.
//!
//! A third boot check measures the memory with Google Play installed the same way and one app
//! window (Settings) open, so every release shows the number: it leaves the window untouched
//! for a minute and reads the memory six times, ten seconds apart, as `dsa memory` reads it
//! (the host's processes, the service's own, and the guest's report), plus the guest's PSS by
//! process from `dumpsys meminfo`. The medians go to `memory.md` and `memory.json`
//! (`memory_check`), with every reading beside them, and how much more the VM holds than the
//! guest uses. Beside the numbers it says what they are made of: the guest kernel's largest
//! slab caches, vmalloc users and threads, which the guest reads as root when the shell asks
//! (`dsa-channeld --memory-report`), and system_server's App Summary, threads and services,
//! its ART heap settings and, where the build allows one, its heap dump. Then it closes the
//! window and waits for the zram writeback, which the boot's command line lets run 30 s after
//! no app is in use, of pages unused for 60 s, and reads the memory again: the zram store
//! before and after, what was written, and the VM's excess over the guest's use with the
//! window open and closed. It fails on a large regression against the baseline recorded in
//! `memory_check/baseline.json`, and when the guest's zram has no backing file.
//!
//! Runs when CI gives it what it needs: `DSA_CROSVM` names the crosvm to run,
//! `DSA_WIDGET_TEST_IMAGE` the image folder built from the commit under test (made with a
//! default ARM translation package), `DSA_ARM_PROBE` the ARM probe APK
//! (`dsa/guest/arm-probe/build.sh`), `DSA_GMS_PROBE` the Play services probe APK
//! (`dsa/guest/gms-probe/build.sh`), `DSA_WIDGET_TEST_KERNEL` the kernel to boot it with when
//! the folder carries none, and `DSA_ADB` the adb to reach the guest with (`adb` on the path
//! when not set). It needs `/dev/kvm` and `/dev/vhost-vsock`. What it read stays in its folder
//! (`/tmp/overlays-test-<pid>`) for the job to keep.
#![cfg(target_os = "linux")]

mod boot_check;
mod idle_reclaim;
mod memory_check;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boot_check::{lock, run_bounded, Process as Service, Watchdog};

use dsa_host_core::gapps::listing::fixture as feed;
use dsa_host_core::gapps::md5::{hex, Md5};
use dsa_host_core::gapps::package::fixture as build;
use dsa_host_core::guest_log::LogFilter;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_unix::socket::UnixLocalConnector;

/// The init scripts that put the images in the system, and the add-on disk they come from.
const SCRIPTS: &[&str] = &["dsa-arm-translation.rc", "dsa-gapps.rc", "dsa-addons.rc"];
/// Where init mounts the user's add-on disk, and the partition it mounts there.
const ADDON_POINT: &str = "/mnt/dsa-addons";
const ADDON_PARTITION: &str = "/dev/block/by-name/dsa_addons";
/// Where the translation package is mounted.
const TRANSLATION_POINT: &str = "/mnt/dsa-arm-translation";
/// The files the translation package is mounted from: a package the user installed, on their
/// add-on disk, or the image's default one.
const TRANSLATION_IMAGES: &[&str] = &[
    "/mnt/dsa-addons/arm-translation.img",
    "/system/etc/dsa/arm-translation/default.img",
];
/// The file the Google apps image is mounted from, on the user's add-on disk.
const GAPPS_IMAGE: &str = "/mnt/dsa-addons/gapps.img";
/// The major number of loop devices.
const LOOP_MAJOR: &str = "7";
/// Where each part of the package is bound, and the part, as a path inside the image.
const TRANSLATION_BINDS: &[(&str, &str)] = &[
    ("/system_ext/lib64", "/system/lib64"),
    ("/system/lib64/arm64", "/system/lib64/arm64"),
    ("/system/bin/arm64", "/system/bin/arm64"),
    ("/system/bin/dsa-arm-translation", "/system/bin"),
    (
        "/system/etc/ld.config.arm64.txt",
        "/system/etc/ld.config.arm64.txt",
    ),
    (
        "/system/lib64/libberberis_exec_region.so",
        "/system/lib64/libberberis_exec_region.so",
    ),
];
/// The folders of the system's own programs, which nothing may be mounted on.
const PROGRAM_FOLDERS: &[&str] = &["/system/bin", "/system/lib64", "/system/etc"];
/// The partitions, which are mounted where they are and which nothing may be laid over.
const PARTITIONS: &[&str] = &["/", "/system", "/product", "/system_ext", "/vendor"];
/// Where the Google apps image is mounted, and the folders it is laid over.
const GAPPS_POINT: &str = "/mnt/dsa-gapps";
const GAPPS_FOLDERS: &[&str] = &[
    "/product/app",
    "/product/priv-app",
    "/product/framework",
    "/product/etc/permissions",
    "/product/etc/sysconfig",
    "/product/etc/default-permissions",
    "/product/etc/preferred-apps",
    "/system_ext/app",
    "/system_ext/priv-app",
    "/system_ext/framework",
    "/system_ext/etc/permissions",
    "/system_ext/etc/sysconfig",
    "/system_ext/etc/default-permissions",
    "/system_ext/etc/preferred-apps",
];
/// What the package puts where the runtime and the translator look: the bridge library its
/// properties name, on the zygote's linker search path, and the ARM64 linker the translator
/// starts every ARM64 program with.
const TRANSLATION_FILES: &[&str] = &[
    "/system_ext/lib64/libdsa_native_bridge.so",
    "/system/bin/arm64/linker64",
];
/// The binfmt_misc rules init registers from the package, each with the ELF type of the ARM64
/// programs it takes: `arm64_exe` matches a fixed-address executable (`ET_EXEC`), `arm64_dyn`
/// a position-independent one (`ET_DYN`).
const TRANSLATION_RULES: &[(&str, u16)] = &[("arm64_exe", 2), ("arm64_dyn", 3)];
/// Where the check puts its ARM64 programs in the guest: the one place the shell may run a
/// file it wrote.
const PROGRAM_DIR: &str = "/data/local/tmp";
/// The places of the translator, as an audit line names them.
const TRANSLATION_PLACES: &[&str] = &[
    "/mnt/dsa-arm-translation",
    "/system_ext/lib64",
    "/system/lib64/arm64",
    "/system/bin/arm64",
    "/system/bin/dsa-arm-translation",
    "ld.config.arm64.txt",
    "libberberis_exec_region",
    "libdsa_native_bridge",
    "binfmt_misc",
];
/// What libnativebridge (its log tag is `nativebridge`) logs when the zygote cannot load the
/// bridge, or rejects its name.
const BRIDGE_FAILURES: &[&str] = &[
    "Failed to load native bridge implementation",
    "Unsupported native bridge API",
    "has been rejected for",
    "native bridge is expected to be opened",
];
/// What the small Google apps build lays over the partitions, with the privileged permission
/// allowlists the host adds to its image.
const GAPPS_FILES: &[&str] = &[
    "/product/priv-app/GmsCore/GmsCore.apk",
    "/product/priv-app/Phonesky/Phonesky.apk",
    "/system_ext/priv-app/GoogleServicesFramework/GoogleServicesFramework.apk",
    "/product/etc/permissions/dsa-gapps-privapp-permissions.xml",
    "/system_ext/etc/permissions/dsa-gapps-privapp-permissions.xml",
];
/// Where the host's allowlist for the small build's Google Play services is, and the privileged
/// permission it must grant that LiteGapps' list in the build leaves out, as the Android 17
/// build left out `ACCESS_COMPANION_INFO`.
const GAPPS_ALLOWLIST: (&str, &str) = (
    "/product/etc/permissions/dsa-gapps-privapp-permissions.xml",
    build::GMS_PERMISSIONS[1],
);
/// What system_server says when a privileged app asks for a privileged permission no list
/// names; with the allowlist enforced it then stops at every boot.
const ALLOWLIST_VIOLATION: &str = "not in privileged permission allowlist";
/// The types of an erofs image's superblock while init mounts it and lays it over: the
/// filesystem's own (an overlay's own superblock has it too), the ones init gives the images
/// with `fscontext=`, and the file types they are labelled with, which a superblock carried
/// while the mounts named `context=` alone.
const OVERLAY_TYPES: &[&str] = &[
    "labeledfs",
    "dsa_arm_translation_fs",
    "dsa_gapps_fs",
    "dsa_addons_fs",
    "system_lib_file",
    "system_file",
];
/// The ARM probe (`dsa/guest/arm-probe`): its package, the activity that starts it, the tag it
/// logs with, and the line its ARM64 code logs once it ran.
const PROBE_PACKAGE: &str = "io.github.thisisthepy.dsa.armprobe";
const PROBE_ACTIVITY: &str = "io.github.thisisthepy.dsa.armprobe/android.app.NativeActivity";
const PROBE_TAG: &str = "DsaArmProbe";
const PROBE_LINE: &str = "DSA ARM probe ran as arm64";
/// How long the probe may take from its start to its line: a first translation is slow.
const PROBE_WAIT: Duration = Duration::from_secs(60);
/// How long the probe keeps running after its line before the log is read, so a crash just
/// after it is in the log too.
const PROBE_SETTLE: Duration = Duration::from_secs(5);
/// What the log holds when a process crashes or aborts: debuggerd's tombstone lines (the
/// translator's CHECK failures end in an abort), the kernel's signal, and the runtime's
/// uncaught exception.
const CRASH_SIGNS: &[&str] = &[
    "Abort message",
    "CHECK failed",
    "Fatal signal",
    "FATAL EXCEPTION",
];
/// The OpenGL ES version `pm list features` must report, 3.2: ANGLE gives every app 3.2 on the
/// host GPU's Vulkan. Google Play hides every app whose manifest asks for a version above the
/// reported one, and v0.2.1 reported 0x0, which hid YouTube, Chrome and most games.
const GLES_FEATURE: &str = "feature:reqGlEsVersion=0x30002";
/// Features Google Play takes for granted in the apps it lists, by name. Each one missing hides
/// every app that does not mark it optional.
const PLAY_FEATURES: &[&str] = &[
    "android.hardware.ram.normal",
    "android.hardware.screen.landscape",
    "android.hardware.screen.portrait",
    "android.hardware.touchscreen",
    "android.hardware.vulkan.level",
    "android.hardware.vulkan.version",
    "android.software.app_widgets",
    "android.software.home_screen",
    "android.software.input_methods",
];
/// The Play services probe (`dsa/guest/gms-probe`): an app named as Google Play services that
/// declares the services the agent keeps off on a desktop, and [GMS_KEPT], which it leaves on.
const GMS_PROBE_MANIFEST: &str = include_str!("../../../../guest/gms-probe/AndroidManifest.xml");
const GMS_PACKAGE: &str = "com.google.android.gms";
/// Play services' push service, which the agent must leave on.
const GMS_KEPT: &str = "com.google.android.gms.gcm.GcmService";
/// How long the agent may take to turn the services off after Android announces a change:
/// it waits two seconds for a burst of changes to end, then makes one call.
const GMS_WAIT: Duration = Duration::from_secs(30);
/// The API levels the mirror carries a build for, with the Android version LiteGapps names.
const MIRROR_SDKS: &[(u32, &str)] = &[(36, "16.0"), (37, "17.0")];
/// The whole test, both boots included, stays under fifteen minutes.
const BUDGET: Duration = Duration::from_secs(900);
/// How long past its budget the test may run before it stops everything it started and
/// ends: inside the job step's 16 minutes, so the step never kills it with its VM running.
const OVERRUN: Duration = Duration::from_secs(60);
/// How long one adb command may take; a log dump of a whole boot takes seconds.
const ADB_LIMIT: Duration = Duration::from_secs(120);

fn scratch() -> PathBuf {
    // Short: the service's socket goes inside, and a socket's path has at most 107 bytes.
    let dir = std::env::temp_dir().join(format!("overlays-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Fails the test with what the service printed, what SELinux refused through the overlays,
/// and the guest log's last lines. A boot that the refusals end early (bpfloader's reboots
/// Android before the guest log has a line) fails here, so its message names them.
fn fail(why: String, service: &Service, log: &Mutex<Vec<String>>) -> ! {
    // The lock is let go before the panic, so the thread that reads the guest log is not
    // left with a poisoned lock and does not panic on top of this failure.
    let (tail, whole) = {
        let log = lock(log);
        let whole = log.join("\n");
        let _ = std::fs::write(service.log.with_file_name("guest.log"), &whole);
        (log[log.len().saturating_sub(200)..].join("\n"), whole)
    };
    let mut refusals = Vec::new();
    refusal_problems(&whole, "guest log", &mut refusals);
    refusal_problems(&console_logs(), "console log", &mut refusals);
    refusals.dedup();
    panic!(
        "{why}\n--- what SELinux refused through the overlays ---\n{}\n--- the service's output ---\n{}\n--- the guest log's last lines ---\n{tail}",
        refusals.join("\n"),
        service.log(),
    )
}

/// The Android console logs of the user this test runs as, as the service keeps them for the
/// user's diagnostics (the current one and the one before it): the kernel's and init's
/// messages from the start of each boot, which the guest log has only once Android is far
/// enough up to send it.
fn console_logs() -> String {
    // The service acts for the user whose socket connection reaches it: this test's.
    // SAFETY: no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut files = dsa_host_linux::service::host_logs_of(&format!("uid:{uid}"));
    // The older log first.
    files.reverse();
    files
        .iter()
        .filter_map(|file| std::fs::read_to_string(&file.path).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

fn client(socket: &Path, wait: Duration) -> HostApiClient {
    let mut connector = UnixLocalConnector {
        path: socket.to_path_buf(),
    };
    let deadline = Instant::now() + wait;
    loop {
        match HostApiClient::connect(&mut connector, "overlays-test", Duration::from_secs(30)) {
            Ok(client) => return client,
            Err(why) if Instant::now() >= deadline => {
                panic!("the service did not answer on {}: {why}", socket.display())
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// Keeps every line of the guest log in `log` while Android runs. A boot ends the watch, so
/// each boot gets its own.
fn read_log(socket: &Path, log: &Arc<Mutex<Vec<String>>>) {
    let reader = client(socket, Duration::from_secs(10))
        .watch_log(&LogFilter::default(), false)
        .expect("reading the guest log");
    let log = Arc::clone(log);
    std::thread::spawn(move || {
        let mut reader = reader;
        loop {
            match reader.next(Duration::from_secs(5)) {
                Ok(Some(batch)) => {
                    let mut log = lock(&log);
                    for entry in batch.entries {
                        log.push(entry.threadtime());
                    }
                }
                Ok(None) => {}
                Err(_) => return,
            }
        }
    });
}

/// adb, talking to the guest through the service's developer mode port.
struct Adb {
    program: PathBuf,
    serial: String,
}

impl Adb {
    /// Runs adb with `args` against the guest, for [ADB_LIMIT] at most.
    fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        self.run_for(args, ADB_LIMIT)
    }

    /// Runs adb with `args` against the guest, for `limit` at most.
    fn run_for(&self, args: &[&str], limit: Duration) -> Result<Vec<u8>, String> {
        let out = run_bounded(
            Command::new(&self.program)
                .arg("-s")
                .arg(&self.serial)
                .args(args),
            limit,
            usize::MAX,
        )?;
        match out.status {
            Some(status) if status.success() => Ok(out.stdout),
            Some(_) => Err(format!(
                "adb {} failed: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )),
            None => Err(format!(
                "adb {} did not finish within {} s and was stopped",
                args.join(" "),
                limit.as_secs()
            )),
        }
    }

    fn shell(&self, command: &str) -> Result<String, String> {
        self.run(&["shell", command])
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    }

    /// Connects, and waits until the guest's shell answers.
    fn connect(&self, wait: Duration) -> Result<(), String> {
        let deadline = Instant::now() + wait;
        let _ = run_bounded(
            Command::new(&self.program).arg("start-server"),
            ADB_LIMIT,
            usize::MAX,
        );
        loop {
            let _ = run_bounded(
                Command::new(&self.program)
                    .arg("disconnect")
                    .arg(&self.serial),
                ADB_LIMIT,
                usize::MAX,
            );
            let _ = run_bounded(
                Command::new(&self.program).arg("connect").arg(&self.serial),
                ADB_LIMIT,
                usize::MAX,
            );
            match self.shell("echo ready") {
                Ok(out) if out.trim() == "ready" => return Ok(()),
                Ok(out) if Instant::now() >= deadline => {
                    return Err(format!("the guest's shell answered {out:?}"))
                }
                Err(why) if Instant::now() >= deadline => return Err(why),
                _ => std::thread::sleep(Duration::from_secs(2)),
            }
        }
    }

    /// Which of `paths` the guest's shell does not find.
    fn missing(&self, paths: &[&str]) -> Result<Vec<String>, String> {
        let script = paths
            .iter()
            .map(|path| format!("[ -e {path} ] || echo {path}"))
            .collect::<Vec<_>>()
            .join("; ");
        let out = self.shell(&script)?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }
}

/// One line of `/proc/self/mountinfo`: the device, the folder of its filesystem that is
/// mounted (`/` for the whole of it, a folder for a bind), where, the mount's own options, the
/// filesystem type, what it was mounted from and the filesystem's options.
struct Mount<'a> {
    device: &'a str,
    root: &'a str,
    point: &'a str,
    options: &'a str,
    kind: &'a str,
    source: &'a str,
    super_options: &'a str,
}

fn mounts(table: &str) -> Vec<Mount<'_>> {
    table
        .lines()
        .filter_map(|line| {
            let (mine, theirs) = line.split_once(" - ")?;
            let mut fields = mine.split_whitespace();
            let _id = fields.next()?;
            let _parent = fields.next()?;
            let device = fields.next()?;
            let root = fields.next()?;
            let point = fields.next()?;
            let options = fields.next().unwrap_or("");
            let mut fields = theirs.split_whitespace();
            let kind = fields.next()?;
            let source = fields.next().unwrap_or("");
            let super_options = fields.next().unwrap_or("");
            Some(Mount {
                device,
                root,
                point,
                options,
                kind,
                source,
                super_options,
            })
        })
        .collect()
}

fn has_option(options: &str, wanted: &str) -> bool {
    options.split(',').any(|option| option == wanted)
}

/// What is wrong with how the erofs image mounted at `image.point` reads its file: no loop
/// device may back it (its device is not a loop device, and it was not mounted from one), it
/// must be mounted from one of `files`, and with `directio`. A loop device keeps every page
/// read from the image a second time, in the image file's own page cache.
fn file_backed_problems(image: &Mount<'_>, files: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    let loop_device = image.device.split(':').next() == Some(LOOP_MAJOR)
        || image.source.starts_with("/dev/block/loop")
        || image.source.starts_with("/dev/loop");
    if loop_device {
        problems.push(format!(
            "a loop device ({}, device {}) backs {}, so every page read from the image is cached twice",
            image.source, image.device, image.point
        ));
    } else if !files.contains(&image.source) {
        problems.push(format!(
            "{} is mounted from {:?}, not from the image file ({})",
            image.point,
            image.source,
            files.join(" or ")
        ));
    }
    if !has_option(image.super_options, "directio") {
        problems.push(format!(
            "{} is mounted without directio ({}), so erofs reads its file through the file's page cache",
            image.point, image.super_options
        ));
    }
    problems
}

/// What is wrong with the mount table `table` for the translation package: it must be mounted
/// at [`TRANSLATION_POINT`] from one of [`TRANSLATION_IMAGES`] itself (see
/// [`file_backed_problems`]) and each part bound where [`TRANSLATION_BINDS`] says, from that
/// same image; nothing may be mounted on [`PROGRAM_FOLDERS`] or laid over a partition.
fn translation_mount_problems(table: &str) -> Vec<String> {
    let table = mounts(table);
    let mut problems = Vec::new();
    let image = table
        .iter()
        .find(|m| m.point == TRANSLATION_POINT && m.kind == "erofs" && m.root == "/");
    match image {
        None => problems.push(format!("no erofs image is mounted at {TRANSLATION_POINT}")),
        Some(image) => {
            problems.extend(file_backed_problems(image, TRANSLATION_IMAGES));
            for (point, part) in TRANSLATION_BINDS {
                let bound = table.iter().find(|m| m.point == *point);
                match bound {
                    Some(m) if m.device == image.device && m.root == *part => {}
                    Some(m) => problems.push(format!(
                        "{point} holds {} of {} ({}), not the package's {part}",
                        m.root, m.device, m.kind
                    )),
                    None => {
                        problems.push(format!("the package's {part} is not bound onto {point}"))
                    }
                }
            }
        }
    }
    problems.extend(program_folder_problems(&table));
    problems
}

/// The mounts of `table` on a folder of the system's own programs, or laid over a partition:
/// a service started from there runs through them, and fails.
fn program_folder_problems(table: &[Mount<'_>]) -> Vec<String> {
    let mut problems = Vec::new();
    for m in table {
        if PROGRAM_FOLDERS.contains(&m.point) {
            problems.push(format!(
                "{} {} is mounted on {}, so the services there run through it",
                m.kind, m.root, m.point
            ));
        } else if PARTITIONS.contains(&m.point) && m.kind == "overlay" {
            problems.push(format!(
                "an overlay is laid over {}, so the services in it run through overlayfs",
                m.point
            ));
        }
    }
    problems
}

/// What is wrong with the mount table `table` for the Google apps image: it must be mounted at
/// [`GAPPS_POINT`] from [`GAPPS_IMAGE`] itself (see [`file_backed_problems`]) and laid over
/// each of [`GAPPS_FOLDERS`] with overlayfs, the image's folder as the upper of the lower
/// layers, and not nosuid, as the partitions are not.
fn gapps_mount_problems(table: &str) -> Vec<String> {
    let parsed = mounts(table);
    let mut problems = Vec::new();
    match parsed
        .iter()
        .find(|m| m.point == GAPPS_POINT && m.kind == "erofs")
    {
        None => problems.push(format!("no erofs image is mounted at {GAPPS_POINT}")),
        Some(image) => problems.extend(file_backed_problems(image, &[GAPPS_IMAGE])),
    }
    for folder in GAPPS_FOLDERS {
        let layer = format!("{GAPPS_POINT}{folder}");
        let laid = parsed.iter().find(|m| {
            m.point == *folder
                && m.kind == "overlay"
                && m.super_options.split(',').any(|option| {
                    option
                        .strip_prefix("lowerdir=")
                        .is_some_and(|dirs| dirs.split(':').next() == Some(layer.as_str()))
                })
        });
        match laid {
            None => problems.push(format!("{layer} is not laid over {folder}")),
            Some(m) if has_option(m.options, "nosuid") => problems.push(format!(
                "{layer} is laid over {folder} nosuid, as {folder} is not"
            )),
            Some(_) => {}
        }
    }
    problems.extend(program_folder_problems(&parsed));
    problems
}

/// What is wrong with the mount table `table` for the user's add-on disk: its partition must be
/// mounted at [`ADDON_POINT`] as erofs, read only, from the partition itself and no loop device,
/// so the add-on images on it are files of an unencrypted file system that erofs reads directly.
fn addon_disk_problems(table: &str) -> Vec<String> {
    let parsed = mounts(table);
    let Some(disk) = parsed.iter().find(|m| m.point == ADDON_POINT) else {
        return vec![format!(
            "the add-on disk is not mounted at {ADDON_POINT}, so the add-on images are not on it"
        )];
    };
    let mut problems = Vec::new();
    if disk.kind != "erofs" {
        problems.push(format!(
            "{ADDON_POINT} is {}, not the add-on disk's erofs partition",
            disk.kind
        ));
    }
    if disk.device.split(':').next() == Some(LOOP_MAJOR) || disk.source.contains("/loop") {
        problems.push(format!(
            "a loop device ({}, device {}) backs the add-on disk",
            disk.source, disk.device
        ));
    } else if disk.source != ADDON_PARTITION {
        problems.push(format!(
            "{ADDON_POINT} is mounted from {:?}, not the add-on disk's partition {ADDON_PARTITION}",
            disk.source
        ));
    }
    if !has_option(disk.options, "ro") {
        problems.push(format!(
            "{ADDON_POINT} is mounted writable ({}), though only the host writes the add-on disk",
            disk.options
        ));
    }
    problems
}

/// Whether binfmt_misc is mounted where the translation script registers the package's rules.
fn binfmt_mounted(table: &str) -> bool {
    mounts(table)
        .iter()
        .any(|m| m.point == "/proc/sys/fs/binfmt_misc" && m.kind == "binfmt_misc")
}

/// A movz instruction of AArch64: `rd` = `imm`.
fn movz(rd: u32, imm: u16) -> u32 {
    0xD280_0000 | (u32::from(imm) << 5) | rd
}

/// An adr instruction of AArch64: `rd` = its own address + `offset`.
fn adr(rd: u32, offset: u32) -> u32 {
    0x1000_0000 | ((offset & 3) << 29) | (((offset >> 2) & 0x7_FFFF) << 5) | rd
}

/// What the ARM64 program for `rule` writes when it runs.
fn program_output(rule: &str) -> String {
    format!("DSA ran an ARM64 program through {rule}\n")
}

/// A small ARM64 Linux program of ELF type `e_type` (2, a fixed-address executable, or 3, a
/// position-independent one) that writes [program_output] for `rule` to its standard output
/// and exits with 0. It needs no library and no linker: no x86_64 kernel runs it, so only a
/// registered binfmt_misc rule and the translator's runner can make it do anything. It
/// carries the dynamic section (an empty symbol table and its hash) the translator's loader
/// reads from every position-independent file, and its code addresses its message relative to
/// itself, so it runs wherever it is loaded.
fn arm64_program(rule: &str, e_type: u16) -> Vec<u8> {
    const EHDR: usize = 64;
    const PHDR: usize = 56;
    const CODE: usize = EHDR + 2 * PHDR;
    const CODE_LEN: usize = 8 * 4;
    let message = program_output(rule).into_bytes();
    let message_at = CODE + CODE_LEN;
    let align8 = |n: usize| n.div_ceil(8) * 8;
    let hash_at = align8(message_at + message.len());
    let symtab_at = hash_at + 16;
    let strtab_at = symtab_at + 24;
    let dynamic_at = align8(strtab_at + 1);
    let dynamic_len = 6 * 16;
    let size = dynamic_at + dynamic_len;
    // A fixed-address executable is loaded where it says: the usual place for one.
    let base: u64 = if e_type == 2 { 0x40_0000 } else { 0 };
    let at = |offset: usize| base + offset as u64;

    let mut elf = Vec::with_capacity(size);
    // The ELF header: 64-bit, little-endian, version 1, System V, AArch64 (0xb7).
    elf.extend_from_slice(b"\x7fELF\x02\x01\x01\x00");
    elf.extend_from_slice(&[0; 8]);
    elf.extend_from_slice(&e_type.to_le_bytes());
    elf.extend_from_slice(&0xb7u16.to_le_bytes());
    elf.extend_from_slice(&1u32.to_le_bytes());
    elf.extend_from_slice(&at(CODE).to_le_bytes()); // e_entry
    elf.extend_from_slice(&(EHDR as u64).to_le_bytes()); // e_phoff
    elf.extend_from_slice(&0u64.to_le_bytes()); // e_shoff: no sections
    elf.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    elf.extend_from_slice(&(EHDR as u16).to_le_bytes());
    elf.extend_from_slice(&(PHDR as u16).to_le_bytes());
    elf.extend_from_slice(&2u16.to_le_bytes()); // e_phnum
                                                // The translator's loader refuses a section header size other than ELF64's and a section
                                                // name table index of 0, even with no sections.
    elf.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
    elf.extend_from_slice(&0u16.to_le_bytes()); // e_shnum
    elf.extend_from_slice(&1u16.to_le_bytes()); // e_shstrndx
    let mut phdr = |kind: u32, flags: u32, offset: usize, len: usize, align: u64| {
        elf.extend_from_slice(&kind.to_le_bytes());
        elf.extend_from_slice(&flags.to_le_bytes());
        elf.extend_from_slice(&(offset as u64).to_le_bytes());
        elf.extend_from_slice(&at(offset).to_le_bytes()); // p_vaddr
        elf.extend_from_slice(&at(offset).to_le_bytes()); // p_paddr
        elf.extend_from_slice(&(len as u64).to_le_bytes()); // p_filesz
        elf.extend_from_slice(&(len as u64).to_le_bytes()); // p_memsz
        elf.extend_from_slice(&align.to_le_bytes());
    };
    // The whole file, readable and executable (the loader refuses one that is writable too),
    // and its dynamic section.
    phdr(1, 5, 0, size, 0x1000);
    phdr(2, 4, dynamic_at, dynamic_len, 8);
    // write(1, message, len); exit_group(0), AArch64 Linux's system calls 64 and 94.
    let code = [
        movz(0, 1),
        adr(1, (message_at - (CODE + 4)) as u32),
        movz(2, message.len() as u16),
        movz(8, 64),
        0xD400_0001, // svc #0
        movz(0, 0),
        movz(8, 94),
        0xD400_0001,
    ];
    for instruction in code {
        elf.extend_from_slice(&instruction.to_le_bytes());
    }
    elf.extend_from_slice(&message);
    elf.resize(hash_at, 0);
    // A System V hash of one bucket and one chain, both empty; the null symbol; an empty name.
    for word in [1u32, 1, 0, 0] {
        elf.extend_from_slice(&word.to_le_bytes());
    }
    elf.extend_from_slice(&[0; 24]);
    elf.push(0);
    elf.resize(dynamic_at, 0);
    let dynamic: [(u64, u64); 6] = [
        (4, at(hash_at)),   // DT_HASH
        (5, at(strtab_at)), // DT_STRTAB
        (6, at(symtab_at)), // DT_SYMTAB
        (10, 1),            // DT_STRSZ
        (11, 24),           // DT_SYMENT
        (0, 0),             // DT_NULL
    ];
    for (tag, value) in dynamic {
        elf.extend_from_slice(&tag.to_le_bytes());
        elf.extend_from_slice(&value.to_le_bytes());
    }
    assert_eq!(elf.len(), size);
    elf
}

/// What is wrong with what the guest's shell printed running the ARM64 program for `rule`,
/// which ends with the line `exit <status>` the check adds.
fn run_problems(rule: &str, shown: &str) -> Vec<String> {
    let status = shown
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("exit "))
        .map(str::trim);
    let wrote = shown.contains(program_output(rule).trim_end());
    match (wrote, status) {
        (true, Some("0")) => Vec::new(),
        _ => vec![format!(
            "an ARM64 program that {rule} takes did not run through the translator: {shown:?}"
        )],
    }
}

/// The lines of `log` where libnativebridge says it could not load the bridge.
fn bridge_failures(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("nativebridge"))
        .filter(|line| BRIDGE_FAILURES.iter().any(|failure| line.contains(failure)))
        .collect()
}

/// The lines of `log` where init says a command of one of the overlay scripts failed.
fn failed_commands(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("Command '") && line.contains("failed"))
        .filter(|line| {
            SCRIPTS
                .iter()
                .any(|script| line.contains(&format!("{script}:")))
        })
        .collect()
}

/// The lines of `log` that record SELinux refusing init a filesystem permission or the
/// binfmt_misc files: what an overlay mount or a rule's registration needs.
fn mount_denials(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("avc:") && line.contains("denied"))
        .filter(|line| line.contains("scontext=u:r:init:s0"))
        .filter(|line| {
            let mount = line.contains("tclass=filesystem")
                && OVERLAY_TYPES
                    .iter()
                    .any(|kind| line.contains(&format!("tcontext=u:object_r:{kind}:")));
            // mounton: binding a part of the package onto its place, or the image onto its
            // mount point.
            mount
                || line.contains("tcontext=u:object_r:binfmt_miscfs:")
                || permissions(line).contains(&"mounton")
        })
        .collect()
}

/// The permissions an audit line of SELinux names between its braces.
fn permissions(line: &str) -> Vec<&str> {
    let Some(start) = line.find('{') else {
        return Vec::new();
    };
    let rest = &line[start + 1..];
    rest[..rest.find('}').unwrap_or(rest.len())]
        .split_whitespace()
        .collect()
}

/// The lines of `log` that record SELinux refusing a program of any domain filesystem getattr
/// on an overlay image's superblock. statfs on an overlay is checked against the superblock of
/// its top lower layer, the image's, so such a refusal fails every program that asks how full
/// `/system/bin`, `/product` or `/system_ext` is, and bpfloader's failure reboots Android.
fn statfs_denials(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("avc:") && line.contains("denied"))
        .filter(|line| line.contains("tclass=filesystem"))
        .filter(|line| permissions(line).contains(&"getattr"))
        .filter(|line| {
            OVERLAY_TYPES
                .iter()
                .any(|kind| line.contains(&format!("tcontext=u:object_r:{kind}:")))
        })
        .collect()
}

/// The lines of `log` that record SELinux refusing init a domain change on exec because the
/// program's mount is nosuid: what an overlay laid over a folder of services with nosuid does.
fn nosuid_denials(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("avc:") && line.contains("denied"))
        .filter(|line| line.contains("scontext=u:r:init:s0"))
        .filter(|line| permissions(line).contains(&"nosuid_transition"))
        .collect()
}

/// Whether an audit line records a refusal of the shell, the domain of every command this check
/// runs through adb. Nothing of the translator runs as the shell but the check's own ARM64
/// programs, and when one of those is refused, its run fails and is reported with what the
/// shell printed. Any other refusal of the shell is the check looking where it may not (it
/// once read binfmt_misc, which the platform lets no domain but init search), not the
/// translator failing.
fn own_command(line: &str) -> bool {
    line.contains("scontext=u:r:shell:s0")
}

/// The lines of `log` that record SELinux refusing anything of the translator: its superblock,
/// one of its places, or init a mount onto a file or folder. The check's own commands are
/// left out ([own_command]).
fn translator_denials(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("avc:") && line.contains("denied"))
        .filter(|line| !own_command(line))
        .filter(|line| {
            line.contains("tcontext=u:object_r:dsa_arm_translation_fs:")
                || TRANSLATION_PLACES.iter().any(|place| line.contains(place))
                || (line.contains("scontext=u:r:init:s0") && permissions(line).contains(&"mounton"))
        })
        .collect()
}

/// The lines of `log` where overlayfs says it could not do what a program asked through it,
/// as it did for every service laid over ("overlayfs: failed to get metacopy (-13)").
fn overlayfs_failures(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("overlayfs:"))
        .filter(|line| line.contains("failed") || line.contains("-13"))
        .collect()
}

/// Adds what `log` (named `from` in each line) says SELinux refused programs through the
/// overlays to `problems`.
fn refusal_problems(log: &str, from: &str, problems: &mut Vec<String>) {
    for line in translator_denials(log) {
        problems.push(format!(
            "{from}: SELinux refused something of the ARM translator: {}",
            line.trim()
        ));
    }
    for line in overlayfs_failures(log) {
        problems.push(format!(
            "{from}: overlayfs failed a program through an overlay: {}",
            line.trim()
        ));
    }
    for line in statfs_denials(log) {
        problems.push(format!(
            "{from}: SELinux refused a program statfs through an overlay: {}",
            line.trim()
        ));
    }
    for line in nosuid_denials(log) {
        problems.push(format!(
            "{from}: SELinux refused init a service's domain from a nosuid mount: {}",
            line.trim()
        ));
    }
}

/// Writes a mirror of LiteGapps' project into `mirror`, laid out as `file:` addresses read
/// it: for each API level of [MIRROR_SDKS], the feed of its folder and one small Core build
/// for `arch`, made as the host's own tests make one.
fn write_mirror(mirror: &Path, arch: &str) -> std::io::Result<()> {
    for (sdk, android) in MIRROR_SDKS {
        let zip = build::sample_for(arch, *sdk);
        let mut md5 = Md5::new();
        md5.update(&zip);
        let digest = hex(&md5.finish());
        let folder = format!("/litegapps/{arch}/{sdk}/core");
        let file = format!("LiteGapps-core-{arch}-{android}-20261001-official.zip");
        let path = format!("{folder}/2026-10-01/{file}");
        let listing = mirror.join(format!("rss?path={folder}"));
        std::fs::create_dir_all(listing.parent().unwrap())?;
        std::fs::write(
            &listing,
            feed::feed(&[feed::item(&path, zip.len() as u64, &digest)]),
        )?;
        let download = mirror.join(format!("files{path}/download"));
        std::fs::create_dir_all(download.parent().unwrap())?;
        std::fs::write(&download, &zip)?;
    }
    Ok(())
}

/// The mount table of a boot with the translation package in place, as `/proc/self/mountinfo`
/// shows it.
const BOUND: &str = "\
20 1 254:0 / / ro,relatime shared:1 - erofs /dev/block/dm-0 ro,seclabel\n\
21 1 254:3 / /system_ext ro,relatime shared:2 - erofs /dev/block/dm-3 ro,seclabel\n\
22 1 254:2 / /product ro,relatime shared:3 - erofs /dev/block/dm-2 ro,seclabel\n\
90 30 0:48 / /mnt/dsa-arm-translation ro,nosuid,nodev,relatime shared:40 - erofs /system/etc/dsa/arm-translation/default.img ro,seclabel,directio\n\
91 21 0:48 /system/lib64 /system_ext/lib64 ro,nosuid,nodev,relatime shared:40 - erofs /system/etc/dsa/arm-translation/default.img ro,seclabel,directio\n\
92 20 0:48 /system/lib64/arm64 /system/lib64/arm64 ro,nosuid,nodev,relatime shared:40 - erofs /system/etc/dsa/arm-translation/default.img ro,seclabel,directio\n\
93 20 0:48 /system/bin/arm64 /system/bin/arm64 ro,nosuid,nodev,relatime shared:40 - erofs /system/etc/dsa/arm-translation/default.img ro,seclabel,directio\n\
94 20 0:48 /system/bin /system/bin/dsa-arm-translation ro,nosuid,nodev,relatime shared:40 - erofs /system/etc/dsa/arm-translation/default.img ro,seclabel,directio\n\
95 20 0:48 /system/etc/ld.config.arm64.txt /system/etc/ld.config.arm64.txt ro,nosuid,nodev,relatime shared:40 - erofs /system/etc/dsa/arm-translation/default.img ro,seclabel,directio\n\
98 20 0:48 /system/lib64/libberberis_exec_region.so /system/lib64/libberberis_exec_region.so ro,nosuid,nodev,relatime shared:40 - erofs /system/etc/dsa/arm-translation/default.img ro,seclabel,directio\n\
96 31 0:41 / /proc/sys/fs/binfmt_misc rw,nosuid,nodev,noexec,relatime shared:41 - binfmt_misc binfmt_misc rw\n";

#[test]
fn fr25_a_translation_package_out_of_its_places_is_found_in_the_mount_table() {
    assert_eq!(translation_mount_problems(BOUND), Vec::<String>::new());
    assert!(binfmt_mounted(BOUND));

    // What develop 3b9b304c laid over, which rebooted Android: overlays on /system's own
    // folders of programs, libraries and configuration.
    let laid_over = "\
20 1 254:0 / / ro,relatime shared:1 - erofs /dev/block/dm-0 ro,seclabel\n\
90 30 7:8 / /mnt/dsa-arm-translation ro,nosuid,nodev,relatime shared:40 - erofs /dev/block/loop8 ro,seclabel\n\
91 20 0:40 / /system/lib64 ro,nodev,relatime shared:42 - overlay overlay ro,seclabel,lowerdir=/mnt/dsa-arm-translation/system/lib64:/system/lib64\n\
92 20 0:41 / /system/bin ro,nodev,relatime shared:43 - overlay overlay ro,seclabel,lowerdir=/mnt/dsa-arm-translation/system/bin:/system/bin\n\
93 20 0:42 / /system/etc ro,nodev,relatime shared:44 - overlay overlay ro,seclabel,lowerdir=/mnt/dsa-arm-translation/system/etc:/system/etc\n";
    let problems = translation_mount_problems(laid_over);
    assert_eq!(
        problems
            .iter()
            .filter(|p| p.contains("so the services there run through it"))
            .count(),
        3,
        "{problems:?}"
    );
    assert_eq!(
        problems
            .iter()
            .filter(|p| p.contains("is not bound onto"))
            .count(),
        TRANSLATION_BINDS.len(),
        "{problems:?}"
    );

    // What every boot of 0.1.9 had: the image refused, so nothing is anywhere.
    let refused = "20 1 254:0 / / ro,relatime shared:1 - erofs /dev/block/dm-0 ro,seclabel\n";
    assert_eq!(translation_mount_problems(refused).len(), 1);
    assert!(!binfmt_mounted(refused));

    // A part bound onto the wrong place, or from another image, does not count.
    let crossed = BOUND.replace(
        "0:48 /system/bin /system/bin/dsa-arm-translation",
        "0:48 /system/etc /system/bin/dsa-arm-translation",
    );
    assert_eq!(translation_mount_problems(&crossed).len(), 1);
    let other = BOUND.replace(
        "0:48 /system/lib64 /system_ext/lib64",
        "0:49 /system/lib64 /system_ext/lib64",
    );
    assert_eq!(translation_mount_problems(&other).len(), 1);
    // A partition laid over is found too.
    let partition = format!(
        "{BOUND}97 20 0:43 / /system_ext ro,relatime - overlay overlay ro,lowerdir=/a:/system_ext\n"
    );
    assert_eq!(translation_mount_problems(&partition).len(), 1);
}

#[test]
fn nfr2_fr25_fr32_an_image_mounted_through_a_loop_device_is_found_in_the_mount_table() {
    // What v0.2.1 mounted: both images through loop devices, every page read from them kept
    // twice. The translation's binds share its device, so they still match.
    let looped = BOUND.replace("0:48 ", "7:8 ").replace(
        "erofs /system/etc/dsa/arm-translation/default.img ro,seclabel,directio",
        "erofs /dev/block/loop8 ro,seclabel",
    );
    let problems = translation_mount_problems(&looped);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0]
        .contains("a loop device (/dev/block/loop8, device 7:8) backs /mnt/dsa-arm-translation"));
    assert!(problems[1].contains("without directio"));
    // A loop device is found by its device number too, whatever the source says.
    let numbered = BOUND.replace(
        "0:48 / /mnt/dsa-arm-translation",
        "7:8 / /mnt/dsa-arm-translation",
    );
    assert!(translation_mount_problems(&numbered)
        .iter()
        .any(|p| p.contains("a loop device")));
    // A package the user installed is mounted from its own file, on the add-on disk.
    let installed = BOUND.replace(
        "/system/etc/dsa/arm-translation/default.img",
        "/mnt/dsa-addons/arm-translation.img",
    );
    assert_eq!(translation_mount_problems(&installed), Vec::<String>::new());
    // One left where an older DSA kept it, in the encrypted /data, is not.
    let on_data = BOUND.replace(
        "/system/etc/dsa/arm-translation/default.img",
        "/data/vendor/dsa/arm-translation/system.img",
    );
    assert_eq!(translation_mount_problems(&on_data).len(), 1);
    // A file that is not the package's is not it.
    let elsewhere = BOUND.replace(
        "/system/etc/dsa/arm-translation/default.img",
        "/data/local/tmp/other.img",
    );
    let problems = translation_mount_problems(&elsewhere);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("not from the image file"));

    let gapps = "80 30 7:9 / /mnt/dsa-gapps ro,nosuid,nodev,relatime - erofs /dev/block/loop9 ro,seclabel\n";
    let problems = gapps_mount_problems(gapps);
    assert!(
        problems.iter().any(
            |p| p.contains("a loop device (/dev/block/loop9, device 7:9) backs /mnt/dsa-gapps")
        ),
        "{problems:?}"
    );
    assert!(problems
        .iter()
        .any(|p| p.contains("/mnt/dsa-gapps is mounted without directio")));
    let gapps = "80 30 0:49 / /mnt/dsa-gapps ro,nosuid,nodev,relatime - erofs /mnt/dsa-addons/gapps.img ro,seclabel,directio\n";
    assert!(!gapps_mount_problems(gapps)
        .iter()
        .any(|p| p.contains("loop device") || p.contains("directio") || p.contains("image file")));
}

#[test]
fn fr32_addons_mount_from_the_read_only_addon_disk() {
    // The add-on disk's partition, read only, and the Google apps image mounted from a file on
    // it, with directio and no loop device: what the boot with the Google apps must show.
    let disk = "70 30 254:48 / /mnt/dsa-addons ro,nosuid,nodev,noexec,relatime - erofs /dev/block/by-name/dsa_addons ro,seclabel\n";
    let mut table = String::from(disk);
    table.push_str("80 30 0:49 / /mnt/dsa-gapps ro,nosuid,nodev,relatime - erofs /mnt/dsa-addons/gapps.img ro,seclabel,directio\n");
    for (at, folder) in GAPPS_FOLDERS.iter().enumerate() {
        table.push_str(&format!(
            "{} 22 0:{} / {folder} ro,nodev,relatime - overlay overlay ro,seclabel,lowerdir=/mnt/dsa-gapps{folder}:{folder}\n",
            81 + at,
            50 + at
        ));
    }
    assert_eq!(addon_disk_problems(&table), Vec::<String>::new());
    assert_eq!(gapps_mount_problems(&table), Vec::<String>::new());
    // No disk at all, a loop device, a writable disk, or another source.
    assert_eq!(addon_disk_problems("").len(), 1);
    let looped = disk
        .replace("254:48", "7:3")
        .replace("/dev/block/by-name/dsa_addons", "/dev/block/loop3");
    assert!(addon_disk_problems(&looped)
        .iter()
        .any(|p| p.contains("a loop device")));
    let writable = disk.replace("ro,nosuid,nodev,noexec", "rw,nosuid,nodev,noexec");
    assert!(addon_disk_problems(&writable)
        .iter()
        .any(|p| p.contains("writable")));
    let elsewhere = disk.replace("/dev/block/by-name/dsa_addons", "/dev/block/vdb");
    assert_eq!(addon_disk_problems(&elsewhere).len(), 1);
    // The Google apps image mounted from /data, as before the add-on disk, is refused.
    let from_data = table.replace(
        "erofs /mnt/dsa-addons/gapps.img",
        "erofs /data/vendor/dsa/gapps/system.img",
    );
    assert!(gapps_mount_problems(&from_data)
        .iter()
        .any(|p| p.contains("not from the image file")));
}

#[test]
fn fr25_the_arm64_programs_match_their_rules_and_write_their_message() {
    for (rule, e_type) in TRANSLATION_RULES {
        let elf = arm64_program(rule, *e_type);
        let u16_at = |at: usize| u16::from_le_bytes([elf[at], elf[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(elf[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(elf[at..at + 8].try_into().unwrap());
        // What the package's rule compares: the ELF identification, the type and AArch64.
        let mut magic = b"\x7fELF\x02\x01\x01\x00".to_vec();
        magic.extend_from_slice(&[0; 8]);
        magic.extend_from_slice(&[*e_type as u8, 0, 0xb7]);
        assert_eq!(&elf[..magic.len()], &magic[..], "{rule}");
        // What the translator's loader checks in the header.
        assert_eq!(u32_at(20), 1, "{rule}: e_version");
        assert_eq!(u16_at(58), 64, "{rule}: e_shentsize");
        assert_ne!(u16_at(62), 0, "{rule}: e_shstrndx");
        assert_eq!(u16_at(56), 2, "{rule}: e_phnum");
        // The one loaded segment covers the file, readable and executable, not writable.
        assert_eq!(u32_at(64), 1, "{rule}: PT_LOAD");
        assert_eq!(u32_at(68), 5, "{rule}: R+X");
        assert_eq!(u64_at(64 + 32), elf.len() as u64, "{rule}: p_filesz");
        let base = u64_at(64 + 16);
        assert_eq!(base, if *e_type == 2 { 0x40_0000 } else { 0 }, "{rule}");
        // The code at the entry: the adr names the message, and the length is its length.
        let entry = (u64_at(24) - base) as usize;
        let code: Vec<u32> = (0..8).map(|i| u32_at(entry + 4 * i)).collect();
        assert_eq!(code[0], 0xD280_0020, "{rule}: mov x0, #1");
        let adr = code[1];
        assert_eq!(adr & 0x9F00_001F, 0x1000_0001, "{rule}: adr x1");
        let offset = ((adr >> 29) & 3) | (((adr >> 5) & 0x7_FFFF) << 2);
        let message = entry + 4 + offset as usize;
        let len = ((code[2] >> 5) & 0xFFFF) as usize;
        assert_eq!(
            &elf[message..message + len],
            program_output(rule).as_bytes(),
            "{rule}"
        );
        assert_eq!(code[3], 0xD280_0808, "{rule}: mov x8, #64 (write)");
        assert_eq!(code[4], 0xD400_0001, "{rule}: svc #0");
        assert_eq!(code[6], 0xD280_0BC8, "{rule}: mov x8, #94 (exit_group)");
        // The dynamic section names a hash, the names, the symbol table and their sizes.
        assert_eq!(u32_at(64 + 56), 2, "{rule}: PT_DYNAMIC");
        let dynamic = u64_at(64 + 56 + 8) as usize;
        let tags: Vec<u64> = (0..6).map(|i| u64_at(dynamic + 16 * i)).collect();
        assert_eq!(tags, [4, 5, 6, 10, 11, 0], "{rule}");
    }
}

#[test]
fn fr25_an_arm64_program_that_did_not_run_is_found() {
    let ran = format!("{}exit 0\n", program_output("arm64_exe"));
    assert!(run_problems("arm64_exe", &ran).is_empty());
    // No rule took it: the kernel found no handler for an AArch64 file.
    let unregistered =
        "/system/bin/sh: /data/local/tmp/dsa-arm64_exe: can't execute: Exec format error\nexit 126\n";
    assert_eq!(run_problems("arm64_exe", unregistered).len(), 1);
    // The rule names a runner that is not there.
    let no_runner =
        "/system/bin/sh: /data/local/tmp/dsa-arm64_dyn: No such file or directory\nexit 127\n";
    assert_eq!(run_problems("arm64_dyn", no_runner).len(), 1);
    // The translator failed it after the message, or refused to load it.
    let crashed = format!("{}exit 139\n", program_output("arm64_dyn"));
    assert_eq!(run_problems("arm64_dyn", &crashed).len(), 1);
    let refused =
        "Error running /data/local/tmp/dsa-arm64_dyn: \"x\" has invalid e_shstrndx\nexit 255\n";
    assert_eq!(run_problems("arm64_dyn", refused).len(), 1);
    // The other rule's message does not count.
    assert_eq!(run_problems("arm64_dyn", &ran).len(), 1);
}

#[test]
fn fr25_a_refusal_of_the_checks_own_commands_is_not_the_translators() {
    // What the check's own read of binfmt_misc caused in develop 97ef3caf's run.
    let log = "11-09 05:04:14.662  2635  2635 I auditd  : type=1400 audit(0.0:6): avc:  denied  { search } for  comm=\"cat\" name=\"/\" dev=\"binfmt_misc\" ino=1 scontext=u:r:shell:s0 tcontext=u:object_r:binfmt_miscfs:s0 tclass=dir permissive=0\n\
               11-09 05:04:14.662  2635  2635 W cat     : type=1400 audit(0.0:6): avc:  denied  { search } for  name=\"/\" dev=\"binfmt_misc\" ino=1 scontext=u:r:shell:s0 tcontext=u:object_r:binfmt_miscfs:s0 tclass=dir permissive=0\n\
               [   43.674664] type=1400 audit(1794200654.662:6): avc:  denied  { search } for  comm=\"cat\" name=\"/\" dev=\"binfmt_misc\" ino=1 scontext=u:r:shell:s0 tcontext=u:object_r:binfmt_miscfs:s0 tclass=dir permissive=0\n";
    assert!(
        translator_denials(log).is_empty(),
        "{:?}",
        translator_denials(log)
    );
    let mut problems = Vec::new();
    refusal_problems(log, "guest log", &mut problems);
    assert!(problems.is_empty(), "{problems:?}");
    // The same refusal of an app is the translator's.
    let app = log.replace("u:r:shell:s0", "u:r:untrusted_app:s0:c512,c768");
    assert_eq!(translator_denials(&app).len(), 3);
    // And init's, which registers the rules, is a mount denial too.
    let init = "avc:  denied  { search } for  comm=\"init\" name=\"/\" dev=\"binfmt_misc\" scontext=u:r:init:s0 tcontext=u:object_r:binfmt_miscfs:s0 tclass=dir permissive=0\n";
    assert_eq!(translator_denials(init).len(), 1);
    assert_eq!(mount_denials(init).len(), 1);
}

/// The ABI Android installed the probe for, as `dumpsys package` shows it.
fn primary_abi(dump: &str) -> Option<&str> {
    dump.lines()
        .find_map(|line| line.trim().strip_prefix("primaryCpuAbi="))
        .map(str::trim)
}

/// The lines of `log` that say a process crashed or aborted ([CRASH_SIGNS]).
fn crash_lines(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| CRASH_SIGNS.iter().any(|sign| line.contains(sign)))
        .collect()
}

/// What is wrong with an ARM app's start, given what `dumpsys package` says of the probe and
/// the log from its start on: it must be installed for arm64-v8a, its ARM64 code must have
/// logged [PROBE_LINE], and no process may have crashed.
fn arm_app_problems(dump: &str, log: &str) -> Vec<String> {
    let mut problems = Vec::new();
    match primary_abi(dump) {
        Some("arm64-v8a") => {}
        abi => problems.push(format!(
            "Android installed the ARM probe for {abi:?}, not arm64-v8a, so it never ran through the translator"
        )),
    }
    let crashes = crash_lines(log);
    for line in &crashes {
        problems.push(format!(
            "an ARM app crashed as it started through the translator: {}",
            line.trim()
        ));
    }
    if !log.contains(PROBE_LINE) {
        problems.push(format!(
            "the ARM probe's ARM64 code never ran: its line ({PROBE_LINE:?}) is not in the log{}",
            if crashes.is_empty() {
                ""
            } else {
                ", and a process crashed"
            }
        ));
    }
    problems
}

#[test]
fn fr25_an_arm_app_the_translator_aborts_as_it_starts_is_found() {
    let dump = "Packages:\n  Package [io.github.thisisthepy.dsa.armprobe] (1a2b3c):\n    primaryCpuAbi=arm64-v8a\n    secondaryCpuAbi=null\n";
    assert_eq!(primary_abi(dump), Some("arm64-v8a"));
    let ran = "11-10 10:00:01.000  2001  2001 I DsaArmProbe: DSA ARM probe ran as arm64 in process 2001 (333328333350000)\n";
    assert_eq!(arm_app_problems(dump, ran), Vec::<String>::new());

    // What v0.2.1 did to every ARM app, APKPure's here: the exec region library was not at
    // the path Berberis opens it by.
    let aborted = "11-10 10:00:01.000  2001  2001 F libc    : frameworks/libs/binary_translation/runtime_primitives/exec_region_elf_backed.cc:48: CHECK failed: 0 != 0\n\
                   11-10 10:00:01.001  2001  2001 F libc    : Fatal signal 6 (SIGABRT), code -1 (SI_QUEUE) in tid 2001 (thisisthepy.dsa.armprobe), pid 2001 (armprobe)\n\
                   11-10 10:00:01.200  2050  2050 F DEBUG   : Abort message: 'frameworks/libs/binary_translation/runtime_primitives/exec_region_elf_backed.cc:48: CHECK failed: 0 != 0'\n";
    let problems = arm_app_problems(dump, aborted);
    assert_eq!(problems.len(), 4, "{problems:?}");
    assert!(problems
        .iter()
        .any(|p| p.contains("exec_region_elf_backed.cc:48")));
    assert!(problems.last().unwrap().contains("never ran"));

    // Its line, and then a crash, is a failure too.
    assert_eq!(arm_app_problems(dump, &format!("{ran}{aborted}")).len(), 3);
    // Installed for x86_64: the translator was never asked.
    let x86 = dump.replace("primaryCpuAbi=arm64-v8a", "primaryCpuAbi=x86_64");
    assert_eq!(arm_app_problems(&x86, ran).len(), 1);
    assert_eq!(arm_app_problems("", ran).len(), 1);
}

#[test]
fn fr25_a_bridge_the_zygote_could_not_load_is_found_in_the_log() {
    let log = "11-09 10:00:01.000   400   400 W nativebridge: Failed to load native bridge implementation: dlopen failed: library \"libdsa_native_bridge.so\" not found\n\
               11-09 10:00:01.001   400   400 E nativebridge: Native bridge library /system/lib64/x.so has been rejected for first character /\n\
               11-09 10:00:01.002   400   400 I zygote64: Native bridge library loaded\n\
               11-09 10:00:01.003   900   900 W SomeApp : Failed to load native bridge implementation\n";
    assert_eq!(bridge_failures(log).len(), 2);
}

#[test]
fn fr25_a_denial_of_the_translator_or_an_overlayfs_failure_is_found_in_the_log() {
    let log = "[    5.580284] type=1400 audit(1794192860.536:9): avc:  denied  { getattr } for  comm=\"uprobestatsbpfl\" name=\"bpfloader\" dev=\"dm-9\" ino=18062390 scontext=u:r:init:s0 tcontext=u:object_r:bpfloader_platform_exec:s0 tclass=file permissive=0\n\
               [    5.580284] overlayfs: failed to get metacopy (-13)\n\
               avc:  denied  { mounton } for  comm=\"init\" path=\"/system/etc/ld.config.arm64.txt\" scontext=u:r:init:s0 tcontext=u:object_r:system_linker_config_file:s0 tclass=file permissive=0\n\
               avc:  denied  { execute } for  comm=\"app\" path=\"/system/bin/dsa-arm-translation/runner\" scontext=u:r:untrusted_app:s0 tcontext=u:object_r:system_lib_file:s0 tclass=file permissive=0\n\
               avc:  denied  { read } for  comm=\"x\" scontext=u:r:untrusted_app:s0 tcontext=u:object_r:dsa_arm_translation_fs:s0 tclass=filesystem permissive=0\n\
               avc:  denied  { read } for  comm=\"uiautomator\" scontext=u:r:shell:s0 tcontext=u:object_r:qemu_sf_lcd_density_prop:s0 tclass=file permissive=0\n";
    assert_eq!(
        translator_denials(log).len(),
        3,
        "{:?}",
        translator_denials(log)
    );
    assert_eq!(overlayfs_failures(log).len(), 1);
    let mut problems = Vec::new();
    refusal_problems(log, "console log", &mut problems);
    assert_eq!(problems.len(), 4, "{problems:?}");
}

#[test]
fn fr25_a_refused_translation_mount_is_found_in_the_log() {
    // What 0.1.9 logged at every boot.
    let log = "11-08 21:00:08.700     1     1 I init    : type=1400 audit(0.0:4): avc:  denied  { relabelfrom } for  comm=\"init\" scontext=u:r:init:s0 tcontext=u:object_r:system_lib_file:s0 tclass=filesystem permissive=0\n\
               11-08 21:00:08.701     1     1 E init    : Command 'mount erofs loop@/system/etc/dsa/arm-translation/default.img /mnt/dsa-arm-translation ro nodev nosuid context=u:object_r:system_lib_file:s0' action=post-fs-data && property:ro.boot.dsa.arm_translation=default (/system/etc/init/dsa-arm-translation.rc:30) took 2ms and failed: mount() failed: Permission denied\n\
               11-08 21:00:09.000   500   500 I zygote64: Starting\n\
               11-08 21:00:09.100   600   600 I auditd  : avc:  denied  { read } for  comm=\"uiautomator\" scontext=u:r:shell:s0 tcontext=u:object_r:qemu_sf_lcd_density_prop:s0 tclass=file permissive=0\n";
    assert_eq!(failed_commands(log).len(), 1);
    assert!(failed_commands(log)[0].contains("dsa-arm-translation.rc:30"));
    assert_eq!(mount_denials(log).len(), 1);
    assert!(mount_denials(log)[0].contains("system_lib_file"));

    // A rule init could not register.
    let binfmt = "avc:  denied  { write } for  comm=\"init\" name=\"register\" scontext=u:r:init:s0 tcontext=u:object_r:binfmt_miscfs:s0 tclass=file permissive=0\n";
    assert_eq!(mount_denials(binfmt).len(), 1);
}

#[test]
fn fr32_a_refused_google_apps_mount_is_found_in_the_log_and_the_mount_table() {
    let log = "E init    : Command 'mount erofs loop@/data/vendor/dsa/gapps/system.img /mnt/dsa-gapps ro nodev nosuid context=u:object_r:system_file:s0' action=post-fs-data && property:ro.boot.dsa.gapps=on (/system/etc/init/dsa-gapps.rc:15) took 3ms and failed: mount() failed: Permission denied\n\
               E init    : Command 'mount overlay overlay /product ro nodev nosuid lowerdir=/mnt/dsa-gapps/product:/product' action=post-fs-data && property:ro.boot.dsa.gapps=on (/system/etc/init/dsa-gapps.rc:16) took 0ms and failed: mount() failed: No such file or directory\n\
               I init    : avc:  denied  { relabelfrom } for  comm=\"init\" scontext=u:r:init:s0 tcontext=u:object_r:system_file:s0 tclass=filesystem permissive=0\n\
               E init    : Command 'start foo' action=boot (/system/etc/init/other.rc:3) took 0ms and failed: service foo not found\n";
    assert_eq!(failed_commands(log).len(), 2);
    assert_eq!(mount_denials(log).len(), 1);

    let mut good =
        String::from("80 30 0:49 / /mnt/dsa-gapps ro,nosuid,nodev,relatime - erofs /mnt/dsa-addons/gapps.img ro,seclabel,directio\n");
    for (at, folder) in GAPPS_FOLDERS.iter().enumerate() {
        good.push_str(&format!(
            "{} 22 0:{} / {folder} ro,nodev,relatime - overlay overlay ro,seclabel,lowerdir=/mnt/dsa-gapps{folder}:{folder}\n",
            81 + at,
            50 + at
        ));
    }
    assert_eq!(gapps_mount_problems(&good), Vec::<String>::new());
    assert_eq!(gapps_mount_problems("").len(), GAPPS_FOLDERS.len() + 1);
    let nosuid = good.replace(
        "ro,nodev,relatime - overlay",
        "ro,nosuid,nodev,relatime - overlay",
    );
    assert_eq!(gapps_mount_problems(&nosuid).len(), GAPPS_FOLDERS.len());
    // What develop 3b9b304c laid over: the partitions' roots, whose services then fail.
    let roots = "80 30 0:49 / /mnt/dsa-gapps ro,nosuid,nodev,relatime - erofs /mnt/dsa-addons/gapps.img ro,seclabel,directio\n\
                 81 1 0:50 / /product ro,nodev,relatime - overlay overlay ro,seclabel,lowerdir=/mnt/dsa-gapps/product:/product\n\
                 82 1 0:51 / /system_ext ro,nodev,relatime - overlay overlay ro,seclabel,lowerdir=/mnt/dsa-gapps/system_ext:/system_ext\n";
    let problems = gapps_mount_problems(roots);
    assert_eq!(
        problems
            .iter()
            .filter(|p| p.contains("services in it run through overlayfs"))
            .count(),
        2,
        "{problems:?}"
    );
}

#[test]
fn fr25_fr32_a_program_refused_statfs_through_an_overlay_is_found_in_the_log() {
    // What develop d69b9ff2's console logged 6.4 s into every boot with the default translator,
    // before bpfloader's failure rebooted Android.
    let log = "[    6.428340] type=1400 audit(1794172804.484:4): avc:  denied  { nosuid_transition } for  comm=\"init\" scontext=u:r:init:s0 tcontext=u:r:recovery_persist:s0 tclass=process2 permissive=0\n\
               [    6.428340] type=1400 audit(1794172804.484:6): avc:  denied  { execute_no_trans } for  comm=\"init\" path=\"/system/bin/recovery-persist\" dev=\"overlay\" ino=18054423 scontext=u:r:init:s0 tcontext=u:object_r:recovery_persist_exec:s0 tclass=file permissive=0\n\
               [    6.440341] type=1400 audit(1794172804.500:7): avc:  denied  { getattr } for  comm=\"rpmb_dev.test.s\" name=\"/\" dev=\"loop76\" ino=36 scontext=u:r:rpmb_dev_system:s0 tcontext=u:object_r:system_lib_file:s0 tclass=filesystem permissive=0\n\
               [    6.460342] type=1400 audit(1794172804.520:9): avc:  denied  { getattr } for  comm=\"netbpfload\" name=\"/\" dev=\"loop76\" ino=36 scontext=u:r:bpfloader:s0 tcontext=u:object_r:system_lib_file:s0 tclass=filesystem permissive=0\n\
               [    6.460342] init: Service bpfloader has 'reboot_on_failure' option and failed, shutting down system.\n\
               [    6.524346] type=1400 audit(1794172804.584:14): avc:  denied  { nosuid_transition } for  comm=\"init\" scontext=u:r:init:s0 tcontext=u:r:vdc:s0 tclass=process2 permissive=0\n\
               11-08 21:00:09.100   600   600 I auditd  : avc:  denied  { read } for  comm=\"uiautomator\" scontext=u:r:shell:s0 tcontext=u:object_r:qemu_sf_lcd_density_prop:s0 tclass=file permissive=0\n";
    let statfs = statfs_denials(log);
    assert_eq!(statfs.len(), 2, "{statfs:?}");
    assert!(statfs[1].contains("scontext=u:r:bpfloader:s0"));
    assert_eq!(nosuid_denials(log).len(), 2);
    // Neither is init's, so the mount checks alone missed them.
    assert!(mount_denials(log).is_empty());
    let mut problems = Vec::new();
    refusal_problems(log, "console log", &mut problems);
    assert_eq!(problems.len(), 4, "{problems:?}");

    // The same refusal against the superblock types the images have now, from any domain.
    for kind in ["dsa_arm_translation_fs", "dsa_gapps_fs"] {
        let line = format!("avc:  denied  {{ getattr }} for  comm=\"df\" name=\"/\" dev=\"loop9\" scontext=u:r:shell:s0 tcontext=u:object_r:{kind}:s0 tclass=filesystem permissive=0");
        assert_eq!(statfs_denials(&line).len(), 1, "{kind}");
    }
    // A getattr on a file of the image is not a statfs.
    let file = "avc:  denied  { getattr } for  comm=\"ls\" path=\"/system/bin/ls\" scontext=u:r:shell:s0 tcontext=u:object_r:system_file:s0 tclass=file permissive=0";
    assert!(statfs_denials(file).is_empty());
}

#[test]
fn fr32_the_mirror_holds_a_build_the_service_takes() {
    // A folder of its own: the boot check's folder holds what the job keeps.
    let dir = std::env::temp_dir().join(format!("overlays-mirror-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write_mirror(&dir, "x86_64").unwrap();
    for (sdk, android) in MIRROR_SDKS {
        let listing = dir.join(format!("rss?path=/litegapps/x86_64/{sdk}/core"));
        let target = dsa_host_core::gapps::Target::for_guest(*sdk, "x86_64").unwrap();
        let builds = dsa_host_core::gapps::listing::builds(
            &std::fs::read_to_string(listing).unwrap(),
            &target,
        );
        assert_eq!(builds.len(), 1, "API level {sdk}");
        assert_eq!(builds[0].android_version, *android);
        let url = dsa_host_core::gapps::listing::download_url(
            &format!("file://{}", dir.display()),
            &target,
            &builds[0],
        );
        let zip = std::fs::read(url.strip_prefix("file://").unwrap()).unwrap();
        assert_eq!(zip.len() as u64, builds[0].size);
    }
}

/// Reads the running boot through adb and adds what is wrong with the translation package to
/// `problems`: its mounts (`table`, the guest's `/proc/self/mountinfo`), its files where the
/// runtime and the translator look, an ARM64 program of each rule init registered running
/// through the translator (written to `dir` first), the ABIs the package manager installs for,
/// and whether the zygote loaded the bridge.
fn translation_problems(adb: &Adb, dir: &Path, table: &str, problems: &mut Vec<String>) {
    for problem in translation_mount_problems(table) {
        problems.push(format!("ARM translation: {problem}"));
    }
    if !binfmt_mounted(table) {
        problems.push(
            "ARM translation: binfmt_misc is not mounted at /proc/sys/fs/binfmt_misc, so no ARM64 program runs"
                .into(),
        );
    }
    match adb.missing(TRANSLATION_FILES) {
        Ok(missing) => {
            for path in missing {
                problems.push(format!("ARM translation: {path} is not in the system"));
            }
        }
        Err(why) => problems.push(why),
    }
    // Each rule is judged by an ARM64 program it takes running through the translator: the
    // shell may not read binfmt_misc.
    for (rule, e_type) in TRANSLATION_RULES {
        let program = dir.join(format!("dsa-{rule}"));
        let guest = format!("{PROGRAM_DIR}/dsa-{rule}");
        let shown = std::fs::write(&program, arm64_program(rule, *e_type))
            .map_err(|why| format!("writing {}: {why}", program.display()))
            .and_then(|()| adb.run(&["push", &program.to_string_lossy(), &guest]))
            .and_then(|_| adb.shell(&format!("chmod 755 {guest} && {guest} 2>&1; echo exit $?")))
            .unwrap_or_else(|why| why);
        for problem in run_problems(rule, &shown) {
            problems.push(format!("ARM translation: {problem}"));
        }
    }
    // The ARM64 linker reads its namespaces from this path only; the image's own is empty.
    match adb.shell("[ -s /system/etc/ld.config.arm64.txt ] && echo full || echo empty") {
        Ok(out) if out.trim() == "full" => {}
        Ok(_) => problems.push(
            "ARM translation: /system/etc/ld.config.arm64.txt is empty, so the ARM64 linker has no namespaces"
                .into(),
        ),
        Err(why) => problems.push(why),
    }
    match adb.shell("getprop ro.product.cpu.abilist") {
        Ok(abis) if abis.split(',').any(|abi| abi.trim() == "arm64-v8a") => {}
        Ok(abis) => problems.push(format!(
            "ARM translation: Android installs apps for {:?} only, not arm64-v8a",
            abis.trim()
        )),
        Err(why) => problems.push(why),
    }
    match adb.shell("getprop ro.dalvik.vm.native.bridge") {
        Ok(bridge) if bridge.trim() == "libdsa_native_bridge.so" => {}
        Ok(bridge) => problems.push(format!(
            "ARM translation: the native bridge property is {:?}, not libdsa_native_bridge.so",
            bridge.trim()
        )),
        Err(why) => problems.push(why),
    }
    // libnativebridge logs only when the zygote fails to load the bridge.
    let log = adb
        .shell("logcat -d -b main -b system -s nativebridge")
        .unwrap_or_default();
    for line in bridge_failures(&log) {
        problems.push(format!(
            "ARM translation: the zygote did not load the bridge: {}",
            line.trim()
        ));
    }
}

/// Installs the ARM probe `apk`, starts it, and adds what is wrong with its start to
/// `problems`: an ARM app's process, started through the native bridge, which loads the
/// translator's exec region library, as no program run through binfmt_misc does. What it read
/// stays in `dir`.
fn arm_app_run_problems(adb: &Adb, apk: &Path, dir: &Path, problems: &mut Vec<String>) {
    if let Err(why) = adb.run(&["install", "-r", &apk.to_string_lossy()]) {
        problems.push(format!("ARM translation: installing the ARM probe: {why}"));
        return;
    }
    let dump = adb
        .shell(&format!("dumpsys package {PROBE_PACKAGE}"))
        .unwrap_or_default();
    let _ = std::fs::write(dir.join("arm-probe-package.txt"), &dump);
    // Only what the start logs.
    let _ = adb.shell("logcat -c -b main -b system -b crash");
    let started = adb
        .shell(&format!("am start -W -n {PROBE_ACTIVITY} 2>&1"))
        .unwrap_or_else(|why| why);
    let _ = std::fs::write(dir.join("arm-probe-start.txt"), &started);
    let until = Instant::now() + PROBE_WAIT;
    while Instant::now() < until {
        let seen = adb
            .shell(&format!("logcat -d -s {PROBE_TAG}"))
            .unwrap_or_default();
        if seen.contains(PROBE_LINE) {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    std::thread::sleep(PROBE_SETTLE);
    let log = adb
        .shell("logcat -d -b main -b system -b crash")
        .unwrap_or_default();
    let _ = std::fs::write(dir.join("arm-probe-log.txt"), &log);
    for problem in arm_app_problems(&dump, &log) {
        problems.push(format!("ARM translation: {problem}"));
    }
    let _ = adb.shell(&format!("am force-stop {PROBE_PACKAGE}"));
}

/// What in `pm list features` (`features`) and `ro.opengles.version` (`gles`) makes Google Play
/// call apps incompatible with the device.
fn play_feature_problems(features: &str, gles: &str) -> Vec<String> {
    let lines: Vec<&str> = features.lines().map(str::trim).collect();
    let mut problems = Vec::new();
    if !lines.contains(&GLES_FEATURE) {
        let reported = lines
            .iter()
            .find(|line| line.starts_with("feature:reqGlEsVersion="))
            .copied()
            .unwrap_or("no reqGlEsVersion");
        problems.push(format!(
            "Android reports {reported} (ro.opengles.version {:?}), not OpenGL ES 3.2 \
             ({GLES_FEATURE}), so Google Play hides every app that needs OpenGL ES",
            gles.trim()
        ));
    }
    for feature in PLAY_FEATURES {
        let declared = lines.iter().any(|line| {
            line.strip_prefix("feature:")
                .is_some_and(|rest| rest.split('=').next() == Some(*feature))
        });
        if !declared {
            problems.push(format!(
                "Android does not declare {feature}, so Google Play hides every app that needs it"
            ));
        }
    }
    problems
}

#[test]
fn fr32_pr3_a_device_without_opengles_or_a_feature_play_needs_is_found() {
    let complete = format!(
        "{GLES_FEATURE}\n{}\nfeature:android.software.webview\n",
        PLAY_FEATURES
            .iter()
            .map(|feature| match *feature {
                "android.hardware.vulkan.level" => format!("feature:{feature}=1"),
                "android.hardware.vulkan.version" => format!("feature:{feature}=4206592"),
                _ => format!("feature:{feature}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(play_feature_problems(&complete, "196610").is_empty());

    // v0.2.1 on a real PC: no ro.opengles.version, and so reqGlEsVersion=0x0.
    let without_gles = complete.replace(GLES_FEATURE, "feature:reqGlEsVersion=0x0");
    let problems = play_feature_problems(&without_gles, "");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("reqGlEsVersion=0x0"), "{problems:?}");
    let gles_3_1 = complete.replace(GLES_FEATURE, "feature:reqGlEsVersion=0x30001");
    assert_eq!(play_feature_problems(&gles_3_1, "196609").len(), 1);

    let without_vulkan = complete.replace("feature:android.hardware.vulkan.level=1\n", "");
    let problems = play_feature_problems(&without_vulkan, "196610");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("android.hardware.vulkan.level"),
        "{problems:?}"
    );
    // A feature whose name only starts with a wanted one does not stand in for it.
    let lookalike = complete.replace(
        "feature:android.software.home_screen\n",
        "feature:android.software.home_screen_extra\n",
    );
    assert_eq!(play_feature_problems(&lookalike, "196610").len(), 1);
    assert_eq!(play_feature_problems("", "").len(), 1 + PLAY_FEATURES.len());
}

/// The most cached app processes the image keeps, as the agent and the framework overlay set it.
const MAX_CACHED_PROCESSES: u32 = 8;

/// The cached process limit the activity manager applies, from `dumpsys activity settings`:
/// `CUR_MAX_CACHED_PROCESSES`, not the `max_cached_processes` line, which shows a constant the
/// limit is only compared with.
fn applied_cached_limit(settings: &str) -> Option<u32> {
    settings.lines().find_map(|line| {
        line.trim()
            .strip_prefix("CUR_MAX_CACHED_PROCESSES=")
            .and_then(|value| value.trim().parse().ok())
    })
}

/// What is wrong with the cached process limit of the running boot.
fn cached_limit_problems(adb: &Adb, dir: &Path, boot: &str, problems: &mut Vec<String>) {
    match adb.shell("dumpsys activity settings") {
        Ok(settings) => {
            let _ = std::fs::write(dir.join(format!("activity-settings-{boot}.txt")), &settings);
            match applied_cached_limit(&settings) {
                Some(MAX_CACHED_PROCESSES) => {}
                Some(limit) => problems.push(format!(
                    "{boot}: the activity manager keeps up to {limit} cached processes, not \
                     {MAX_CACHED_PROCESSES} (CUR_MAX_CACHED_PROCESSES in dumpsys activity settings)"
                )),
                None => problems.push(format!(
                    "{boot}: dumpsys activity settings shows no CUR_MAX_CACHED_PROCESSES line"
                )),
            }
        }
        Err(why) => problems.push(format!("{boot}: reading dumpsys activity settings: {why}")),
    }
}

/// The files `dumpsys pinner` lists as locked in memory, with their pinned bytes, leaving out
/// those it lists with none.
fn pinned_files(dump: &str) -> Vec<(String, u64)> {
    dump.lines()
        .filter_map(|line| {
            let (file, rest) = line.trim().split_once(" pinned:")?;
            let bytes: u64 = rest.split_whitespace().next()?.parse().ok()?;
            (bytes > 0).then(|| (file.to_string(), bytes))
        })
        .collect()
}

/// What is wrong with the memory the pinner service holds: the image turns it off, because
/// locked pages are neither reclaimed nor compressed, so the host could never get them back.
fn pinned_memory_problems(adb: &Adb, dir: &Path, boot: &str, problems: &mut Vec<String>) {
    match adb.shell("dumpsys pinner") {
        Ok(dump) => {
            let _ = std::fs::write(dir.join(format!("pinner-{boot}.txt")), &dump);
            let pinned = pinned_files(&dump);
            if !pinned.is_empty() {
                let total: u64 = pinned.iter().map(|(_, bytes)| bytes).sum();
                let files: Vec<String> = pinned
                    .iter()
                    .map(|(file, bytes)| format!("{file} ({} KiB)", bytes >> 10))
                    .collect();
                problems.push(format!(
                    "{boot}: the pinner service locks {} KiB in memory, which no reclaim can give \
                     back to the host: {}",
                    total >> 10,
                    files.join(", ")
                ));
            }
        }
        Err(why) => problems.push(format!("{boot}: reading dumpsys pinner: {why}")),
    }
}

#[test]
fn nfr1_the_files_the_pinner_service_locks_are_read_from_its_dump() {
    // Android 17's dump with its default configuration: the home app (the agent) and WebView.
    let dump = "Pinner Configs:\n   Total Pinner quota: 10% of total device memory\n   \
                Max Home App Pin Bytes (without deps): 62914560 (60.00 MB)\n\nPinned Files:\n\
                Home uid=10109 active=true\n  \
                /system_ext/priv-app/DsaAgent/DsaAgent.apk pinned:4194304 bytes (4.00 MB) pinlist:false\n  \
                /system_ext/priv-app/DsaAgent/oat/x86_64/DsaAgent.odex pinned:2097152 bytes (2.00 MB) pinlist:false (Dependency)\n\
                Total Pinned = 6291456 (6.00 MB) [App=4194304 (4.00 MB), Dependencies=2097152 (2.00 MB)]\n\n\
                \nGroup:webview\n  /product/app/webview/webview.apk pinned: 20971520 bytes (20.00 MB) pinlist:false\n\
                Total Pinned = 20971520 (20.00 MB) [Main=20971520 (20.00 MB), Dependencies=0 (0.00 MB)]\n";
    assert_eq!(
        pinned_files(dump),
        vec![
            (
                "/system_ext/priv-app/DsaAgent/DsaAgent.apk".to_string(),
                4194304
            ),
            (
                "/system_ext/priv-app/DsaAgent/oat/x86_64/DsaAgent.odex".to_string(),
                2097152
            ),
            ("/product/app/webview/webview.apk".to_string(), 20971520),
        ]
    );
    // With the image's configuration nothing is listed, and a file pinned with no bytes is
    // nothing held.
    let off = "Pinner Configs:\n   Total Pinner quota: 0% of total device memory\n\nPinned Files:\n\n\
               \nGroup:system\n  /system/framework/framework.jar pinned: 0 bytes (0.00 MB) pinlist:false\n";
    assert_eq!(pinned_files(off), Vec::<(String, u64)>::new());
}

#[test]
fn nfr2_the_cached_process_limit_is_read_as_the_activity_manager_applies_it() {
    // Android 17 on the owner's PC: the setting said 8, the activity manager applied 1024.
    let ignored =
        "ACTIVITY MANAGER SETTINGS (dumpsys activity settings) activity_manager_constants:\n  \
                   max_cached_processes=1024\n  background_settle_time=60000\n\n  \
                   mCustomizedMaxCachedProcesses=1024\n  CUR_MAX_CACHED_PROCESSES=1024\n  \
                   CUR_MAX_EMPTY_PROCESSES=512\n";
    assert_eq!(applied_cached_limit(ignored), Some(1024));
    let applied = ignored.replace(
        "CUR_MAX_CACHED_PROCESSES=1024",
        "CUR_MAX_CACHED_PROCESSES=8",
    );
    assert_eq!(applied_cached_limit(&applied), Some(MAX_CACHED_PROCESSES));
    assert_eq!(applied_cached_limit("max_cached_processes=8\n"), None);
}

/// The services the Play services probe declares, by class name.
fn probe_services(manifest: &str) -> Vec<&str> {
    manifest
        .split("<service ")
        .skip(1)
        .filter_map(|element| {
            let rest = element.split("android:name=\"").nth(1)?;
            rest.split('"').next()
        })
        .collect()
}

/// The components `dumpsys package` lists as turned off for each user, across the package's
/// entries (an updated system app has two).
fn disabled_components(dump: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut header: Option<usize> = None;
    for line in dump.lines() {
        let indent = line.len() - line.trim_start().len();
        let text = line.trim();
        if text == "disabledComponents:" {
            header = Some(indent);
            continue;
        }
        match header {
            Some(level) if indent > level && !text.is_empty() && !text.contains([' ', ':']) => {
                found.push(text)
            }
            Some(_) => header = None,
            None => {}
        }
    }
    found
}

/// What is wrong with the probe's components in `dump`, `when` naming the step.
fn play_services_component_problems(dump: &str, when: &str) -> Vec<String> {
    let off = disabled_components(dump);
    let mut problems = Vec::new();
    for service in probe_services(GMS_PROBE_MANIFEST) {
        let is_off = off.contains(&service);
        if service == GMS_KEPT && is_off {
            problems.push(format!(
                "{when}: the agent turned off {service}, which carries push messages"
            ));
        } else if service != GMS_KEPT && !is_off {
            problems.push(format!(
                "{when}: {service} is still on {} s after Android announced the change",
                GMS_WAIT.as_secs()
            ));
        }
    }
    problems
}

#[test]
fn fr32_the_play_services_components_turned_off_are_read_from_the_package_dump() {
    // The shell may turn single components back on only in a test-only package; for any
    // other app the package manager refuses `pm enable <package>/<class>` from adb.
    assert!(
        GMS_PROBE_MANIFEST.contains("android:testOnly=\"true\""),
        "the Play services probe must be a test-only package, or the shell cannot turn its services back on"
    );
    let services = probe_services(GMS_PROBE_MANIFEST);
    assert!(services.contains(&GMS_KEPT), "{services:?}");
    assert!(
        services
            .contains(&"com.google.android.gms.crisisalerts.init.CrisisAlertsPersistentService"),
        "{services:?}"
    );
    let turned_off: Vec<&str> = services
        .iter()
        .copied()
        .filter(|service| *service != GMS_KEPT)
        .collect();
    let dump = format!(
        "  Package [{GMS_PACKAGE}] (1a2b3c):\n    User 0: installed=true enabled=0\n      \
         disabledComponents:\n{}\n      enabledComponents:\n        {GMS_KEPT}\n    \
         User 10: installed=false\n",
        turned_off
            .iter()
            .map(|service| format!("        {service}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(disabled_components(&dump), turned_off);
    assert!(play_services_component_problems(&dump, "after install").is_empty());

    // One left on, and the push service turned off.
    let one_on = dump.replace(&format!("        {}\n", turned_off[0]), "");
    let problems = play_services_component_problems(&one_on, "after install");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains(turned_off[0]), "{problems:?}");
    let push_off = dump.replace(
        "      enabledComponents:\n",
        &format!("        {GMS_KEPT}\n      enabledComponents:\n"),
    );
    let problems = play_services_component_problems(&push_off, "after install");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("push"), "{problems:?}");
    // Nothing turned off at all.
    assert_eq!(
        play_services_component_problems("", "after install").len(),
        turned_off.len()
    );
}

/// Waits until every service the probe declares but [GMS_KEPT] is off, for [GMS_WAIT] at
/// most, and returns the last package dump.
fn wait_for_off(adb: &Adb) -> String {
    let until = Instant::now() + GMS_WAIT;
    loop {
        let dump = adb
            .shell(&format!("dumpsys package {GMS_PACKAGE}"))
            .unwrap_or_default();
        if play_services_component_problems(&dump, "").is_empty() || Instant::now() >= until {
            return dump;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The agent with Play services installed. The probe stands in for it, since the small Google
/// apps build is not signed and the package manager does not install it. After the install the
/// services the agent keeps off must be off and the push service on. Then each is turned on
/// again from the shell, as Play services' own setup does after an update, and the probe is
/// installed over itself, which Android announces as an update: a moment later they must be
/// off again. The probe is a test-only package, so it installs with `-t` and the shell may
/// change its single components.
fn play_services_problems(adb: &Adb, apk: &Path, dir: &Path, problems: &mut Vec<String>) {
    let _ = adb.shell("logcat -c -b main");
    if let Err(why) = adb.run(&["install", "-r", "-t", &apk.to_string_lossy()]) {
        problems.push(format!("Google Play services: installing the probe: {why}"));
        return;
    }
    let dump = wait_for_off(adb);
    let _ = std::fs::write(dir.join("gms-probe-installed.txt"), &dump);
    for problem in play_services_component_problems(&dump, "after install") {
        problems.push(format!("Google Play services: {problem}"));
    }

    let enable = probe_services(GMS_PROBE_MANIFEST)
        .iter()
        .map(|service| format!("pm enable {GMS_PACKAGE}/{service} >/dev/null"))
        .collect::<Vec<_>>()
        .join("; ");
    if let Err(why) = adb.shell(&enable) {
        problems.push(format!(
            "Google Play services: turning the probe's services back on: {why}"
        ));
        return;
    }
    if let Err(why) = adb.run(&["install", "-r", "-t", &apk.to_string_lossy()]) {
        problems.push(format!("Google Play services: updating the probe: {why}"));
        return;
    }
    let dump = wait_for_off(adb);
    let _ = std::fs::write(dir.join("gms-probe-updated.txt"), &dump);
    for problem in play_services_component_problems(&dump, "after an update") {
        problems.push(format!("Google Play services: {problem}"));
    }
    let log = adb.shell("logcat -d -s DsaAgent").unwrap_or_default();
    let _ = std::fs::write(dir.join("gms-probe-agent-log.txt"), &log);
    if !log.contains("turned off Google Play services components a desktop does not use") {
        problems.push(
            "Google Play services: the agent did not log which components it turned off"
                .to_string(),
        );
    }
}

/// An alarm that fires at least this many times since boot, and more often on average than
/// once per [STORM_INTERVAL], is retrying something it never gets. Google Play services'
/// `platformconfigurator.RETRY_UPDATE` fired 720 times in the first ten minutes of a boot on a
/// PC while the image named no device configurator; the clock's TIME_TICK fires once a minute.
const STORM_ALARMS: u64 = 60;
const STORM_INTERVAL: Duration = Duration::from_secs(10);

/// Each alarm of `dumpsys alarm`'s "Alarm Stats" that fired as a storm in the `uptime` the
/// guest has been running, as "<owner> <alarm>: <count> alarms in <seconds> s".
fn alarm_storms(dump: &str, uptime: Duration) -> Vec<String> {
    let mut storms = Vec::new();
    let mut in_stats = false;
    let mut owner = "";
    let mut count: Option<u64> = None;
    for line in dump.lines() {
        let text = line.trim();
        if text == "Alarm Stats:" {
            in_stats = true;
            continue;
        }
        if !in_stats || text.is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent <= 2 {
            // An owner ("u0a110:com.google.android.gms +3s578ms running, 1 wakeups:"), or
            // the next section, which ends the stats.
            if text.ends_with(" wakeups:") && text.contains(" running, ") {
                owner = text.split_whitespace().next().unwrap_or("");
                count = None;
                continue;
            }
            break;
        }
        // An alarm's line ("+2s704ms 0 wakes 720 alarms, last -53m21s222ms:") and then its
        // name ("*alarm*:com.google.android.gms.platformconfigurator.RETRY_UPDATE").
        if let Some(fired) = count.take() {
            let seconds = uptime.as_secs_f64();
            if fired >= STORM_ALARMS && seconds / (fired as f64) < STORM_INTERVAL.as_secs_f64() {
                storms.push(format!(
                    "{owner} {text}: {fired} alarms in {} s",
                    uptime.as_secs()
                ));
            }
            continue;
        }
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.len() >= 5
            && words[0].starts_with('+')
            && words[2] == "wakes"
            && words[4].trim_end_matches(',') == "alarms"
        {
            count = words[3].parse().ok();
        }
    }
    storms
}

#[test]
fn nfr2_an_alarm_retried_every_second_after_boot_is_a_storm() {
    // What the PC's guest reported while Google Play services could not write DeviceConfig.
    let dump = "  Top Alarms:\n\
                \x20   +2s704ms running, 0 wakeups, 720 alarms: u0a110:com.google.android.gms\n\
                \x20     *alarm*:com.google.android.gms.platformconfigurator.RETRY_UPDATE\n\
                \x20 \n\
                \x20 Alarm Stats:\n\
                \x20 1000:android +2s822ms running, 13 wakeups:\n\
                \x20   +1s33ms 0 wakes 32 alarms, last -3m24s655ms:\n\
                \x20     *alarm*:TIME_TICK\n\
                \x20 u0a110:com.google.android.gms +3s578ms running, 1 wakeups:\n\
                \x20   +2s704ms 0 wakes 720 alarms, last -53m21s222ms:\n\
                \x20     *alarm*:com.google.android.gms.platformconfigurator.RETRY_UPDATE\n\
                \x20   +864ms 0 wakes 8 alarms, last -45m21s98ms:\n\
                \x20     *alarm*:com.google.android.gms.gcm.ACTION_CHECK_QUEUE\n\
                \x20 \n\
                \x20 Alarm manager stats:\n\
                \x20   +5s 0 wakes 900 alarms, last -1s:\n\
                \x20     *alarm*:not an alarm of the stats\n";
    let storms = alarm_storms(dump, Duration::from_secs(600));
    assert_eq!(
        storms,
        vec!["u0a110:com.google.android.gms *alarm*:com.google.android.gms.platformconfigurator.RETRY_UPDATE: 720 alarms in 600 s".to_string()]
    );
    // The same counts over a long session are alarms at their usual pace.
    assert_eq!(
        alarm_storms(dump, Duration::from_secs(3 * 3600)),
        Vec::<String>::new()
    );
    assert_eq!(
        alarm_storms("", Duration::from_secs(60)),
        Vec::<String>::new()
    );
}

/// What `cmd overlay lookup` must answer for the device configurator, without which Google Play
/// services is never granted WRITE_DEVICE_CONFIG and retries its flag sync every second.
const CONFIGURATOR_RESOURCE: &str = "android:string/config_deviceConfiguratorPackageName";
const CONFIGURATOR: &str = "com.google.android.gms";

/// What is wrong with the device configurator, the compile of the Google apps without an app
/// image, and the alarms of the boot with the Google apps. The compile runs at boot completion
/// and clears the app image format while it runs; it must end, log that it looked, and leave
/// the format as the build gave it.
fn gapps_boot_problems(adb: &Adb, dir: &Path, problems: &mut Vec<String>) {
    match adb.shell(&format!(
        "cmd overlay lookup android {CONFIGURATOR_RESOURCE}"
    )) {
        Ok(value) if value.trim() == CONFIGURATOR => {}
        Ok(value) => problems.push(format!(
            "Google Play: the framework's device configurator is {:?}, not {CONFIGURATOR}, so \
             Play services may not write the flags it syncs and retries every second",
            value.trim()
        )),
        Err(why) => problems.push(format!("reading the device configurator: {why}")),
    }
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let state = adb
            .shell("getprop init.svc.dsa_gapps_compile")
            .unwrap_or_default();
        let logged = adb
            .shell("logcat -d -s DsaGappsCompile")
            .unwrap_or_default();
        if state.trim() == "stopped" && logged.contains("checked ") {
            let _ = std::fs::write(dir.join("gapps-compile.txt"), &logged);
            break;
        }
        if Instant::now() >= until {
            problems.push(format!(
                "Google Play: the compile without an app image did not run to its end after boot \
                 (dsa_gapps_compile is {:?}; its log: {:?})",
                state.trim(),
                logged.trim()
            ));
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    match adb.shell("getprop dalvik.vm.appimageformat") {
        Ok(format) if format.trim() == "lz4" => {}
        Ok(format) => problems.push(format!(
            "Google Play: dalvik.vm.appimageformat is {:?} after the compile, not lz4, so no app \
             compiled from now on gets an app image",
            format.trim()
        )),
        Err(why) => problems.push(format!("reading dalvik.vm.appimageformat: {why}")),
    }
    alarm_storm_problems(adb, &dir.join("alarms.txt"), problems);
}

/// The alarms of the running boot that fire as a storm, read through `adb` and kept in `keep`.
fn alarm_storm_problems(adb: &Adb, keep: &Path, problems: &mut Vec<String>) {
    let uptime = adb
        .shell("cat /proc/uptime")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
        .map(Duration::from_secs_f64);
    match (adb.shell("dumpsys alarm"), uptime) {
        (Ok(dump), Some(uptime)) => {
            let _ = std::fs::write(keep, &dump);
            for storm in alarm_storms(&dump, uptime) {
                problems.push(format!("an alarm fires as a storm after boot: {storm}"));
            }
        }
        (Err(why), _) => problems.push(format!("reading the alarms: {why}")),
        (_, None) => problems.push("reading how long the guest has been up".to_string()),
    }
}

/// What the guest's own log of the running boot says went wrong with the overlay scripts.
fn log_problems(adb: &Adb, dir: &Path, boot: &str, problems: &mut Vec<String>) {
    // Init's messages are in the kernel's buffer, which logd reads, with the audit messages.
    let log = adb.shell("logcat -d -b all").unwrap_or_default();
    let _ = std::fs::write(dir.join(format!("logcat-{boot}.txt")), &log);
    for line in failed_commands(&log) {
        problems.push(format!("{boot}: init failed an overlay command: {line}"));
    }
    for line in mount_denials(&log) {
        problems.push(format!("{boot}: SELinux refused init: {line}"));
    }
    for line in log
        .lines()
        .filter(|line| line.contains(ALLOWLIST_VIOLATION))
    {
        problems.push(format!(
            "{boot}: a privileged permission is in no allowlist: {}",
            line.trim()
        ));
    }
    refusal_problems(&log, boot, problems);
}

#[test]
fn fr25_fr32_the_translation_and_google_apps_images_are_laid_over_the_booted_system() {
    let (Some(crosvm), Some(image)) = (
        std::env::var_os("DSA_CROSVM").map(PathBuf::from),
        std::env::var_os("DSA_WIDGET_TEST_IMAGE").map(PathBuf::from),
    ) else {
        eprintln!("skipped: DSA_CROSVM and DSA_WIDGET_TEST_IMAGE name no crosvm and image");
        return;
    };
    let kernel = std::env::var_os("DSA_WIDGET_TEST_KERNEL").map(PathBuf::from);
    let probe = std::env::var_os("DSA_ARM_PROBE")
        .map(PathBuf::from)
        .filter(|apk| apk.is_file())
        .expect(
            "DSA_ARM_PROBE names no ARM probe APK (dsa/guest/arm-probe/build.sh), so no ARM app can be started",
        );
    let gms_probe = std::env::var_os("DSA_GMS_PROBE")
        .map(PathBuf::from)
        .filter(|apk| apk.is_file())
        .expect(
            "DSA_GMS_PROBE names no Play services probe APK (dsa/guest/gms-probe/build.sh), so the agent's handling of Play services cannot be checked",
        );
    let adb_program = std::env::var_os("DSA_ADB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("adb"));
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(image.join("image.json")).expect("reading the image's image.json"),
    )
    .expect("the image's image.json is JSON");
    let arch = manifest["arch"].as_str().unwrap_or("x86_64").to_string();
    assert_eq!(
        arch, "x86_64",
        "only an x86_64 image carries a translator; boot this check with one"
    );
    assert!(
        manifest.get("arm_translation").is_some(),
        "the image was made without a default ARM translation package (make-image.py --arm-translation), so there is nothing to lay over"
    );

    let started = Instant::now();
    let deadline = started + BUDGET;
    let _watchdog = Watchdog::start("the overlay check", BUDGET + OVERRUN);
    let dir = scratch();
    let mirror = dir.join("mirror");
    write_mirror(&mirror, &arch).unwrap();
    let socket = dir.join("host.sock");
    let config = dir.join("dsa-hostd.json");
    let settings = serde_json::json!({
        "image_dir": image,
        "kernel": kernel,
        "crosvm": crosvm,
        "socket": socket,
        "machine_dir": dir.join("machine"),
        "network": false,
        "memory_mb": 4096,
        "processors": 2,
        "data_disk_size_gb": 8,
        "gapps_project": format!("file://{}", mirror.display()),
    });
    std::fs::write(&config, settings.to_string()).unwrap();
    let service = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-linux-hostd"))
            .arg("--config")
            .arg(&config),
        dir.join("hostd.log"),
    );
    let _compositor = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-compositor")).env("DSA_SOCKET", &socket),
        dir.join("compositor.log"),
    );
    let mut api = client(&socket, Duration::from_secs(30));
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    read_log(&socket, &log);
    // Each boot may take a third of what is left, so the second one still has its turn.
    let boot = (deadline.saturating_duration_since(Instant::now())) / 3;
    if let Err(why) = api.ensure_running(boot, Duration::from_secs(2), |state| {
        eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
    }) {
        fail(
            format!("Android did not become ready: {why}"),
            &service,
            &log,
        );
    }
    eprintln!(
        "Android was ready {} s after the start",
        started.elapsed().as_secs()
    );

    // Developer mode, so adb reaches the guest through the service's loopback port.
    let mut wanted = match api.settings() {
        Ok(report) => report.settings,
        Err(why) => fail(format!("reading the settings: {why}"), &service, &log),
    };
    wanted.developer_mode = true;
    wanted.adb_auto_connect = false;
    let port = wanted.adb_port;
    if let Err(why) = api.set_settings(&wanted) {
        fail(format!("turning developer mode on: {why}"), &service, &log);
    }
    let adb = Adb {
        program: adb_program,
        serial: format!("127.0.0.1:{port}"),
    };
    if let Err(why) = adb.connect(Duration::from_secs(120)) {
        fail(
            format!("adb did not reach the guest: {why}"),
            &service,
            &log,
        );
    }
    // The policy is what is under test, so it must be in force.
    match adb.shell("getenforce") {
        Ok(mode) if mode.trim() == "Enforcing" => {}
        Ok(mode) => fail(
            format!("SELinux is {:?} on the image, not enforcing", mode.trim()),
            &service,
            &log,
        ),
        Err(why) => fail(why, &service, &log),
    }

    // The first boot: the image's default translator, and no Google apps.
    let mut problems = Vec::new();
    let table = adb.shell("cat /proc/self/mountinfo").unwrap_or_default();
    let _ = std::fs::write(dir.join("mounts-first.txt"), &table);
    translation_problems(&adb, &dir, &table, &mut problems);
    // Nothing is installed yet, so the host attaches no add-on disk.
    if mounts(&table).iter().any(|m| m.point == ADDON_POINT) {
        problems.push(format!(
            "first boot: {ADDON_POINT} is mounted though no add-on is installed"
        ));
    }
    match api.arm_translation() {
        Ok(status) if status.guest.as_ref().is_some_and(|guest| guest.active) => {}
        Ok(status) => problems.push(format!(
            "ARM translation: the guest says no translator is in place ({:?})",
            status.guest
        )),
        Err(why) => problems.push(format!("reading where ARM translation stands: {why}")),
    }
    log_problems(&adb, &dir, "first boot", &mut problems);
    cached_limit_problems(&adb, &dir, "first boot", &mut problems);
    pinned_memory_problems(&adb, &dir, "first boot", &mut problems);
    // An ARM app, after the boot's log is read: the probe clears the log to read its own start.
    arm_app_run_problems(&adb, &probe, &dir, &mut problems);
    // What Google Play compares each app's manifest with, which the Google apps do not change.
    match (
        adb.shell("pm list features"),
        adb.shell("getprop ro.opengles.version"),
    ) {
        (Ok(features), Ok(gles)) => {
            let _ = std::fs::write(dir.join("features.txt"), &features);
            for problem in play_feature_problems(&features, &gles) {
                problems.push(format!("Google Play: {problem}"));
            }
        }
        (Err(why), _) | (_, Err(why)) => {
            problems.push(format!("reading the device's features: {why}"))
        }
    }

    // Google apps from the mirror, through the local API as `dsa gapps install` does. The
    // guest reports its API level a moment after it is ready, so the install is tried again
    // until it does.
    let install_until = Instant::now() + Duration::from_secs(90);
    let installed = loop {
        let result = client(&socket, Duration::from_secs(10)).install_gapps(&mut |news| {
            eprintln!("Google Play: {news:?}");
        });
        match result {
            Ok(status) => break status,
            Err(why) if Instant::now() >= install_until => fail(
                format!("installing the Google apps from the mirror: {why}"),
                &service,
                &log,
            ),
            Err(why) => {
                eprintln!("installing the Google apps from the mirror: {why}; trying again");
                std::thread::sleep(Duration::from_secs(3));
            }
        }
    };
    if installed.package.is_none() || !installed.restart_needed {
        fail(
            format!(
                "the service took the Google apps but does not say they apply from the next start: {:?}",
                installed.package
            ),
            &service,
            &log,
        );
    }

    // Android starts again, with the Google apps and the translator.
    if let Err(why) = api.stop() {
        fail(format!("stopping Android: {why}"), &service, &log);
    }
    // Stopped, or on its way up again when something asked for it to start before this looked.
    let stop_until = Instant::now() + Duration::from_secs(120);
    let mut left = false;
    loop {
        use dsa_host_core::session::SessionState;
        match api.status() {
            Ok(status) if status.state == SessionState::Stopped => break,
            Ok(status) if left && status.state == SessionState::Starting => break,
            Ok(status) if Instant::now() < stop_until => {
                left |= status.state == SessionState::Stopping;
                std::thread::sleep(Duration::from_secs(1))
            }
            Ok(status) => fail(
                format!("Android did not stop: it is {:?}", status.state),
                &service,
                &log,
            ),
            Err(why) => fail(format!("reading the status: {why}"), &service, &log),
        }
    }
    eprintln!("{} s: Android stopped", started.elapsed().as_secs());
    let boot = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(Duration::from_secs(180));
    if let Err(why) = api.ensure_running(boot, Duration::from_secs(2), |state| {
        eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
    }) {
        fail(
            format!("Android did not become ready again with the Google apps: {why}"),
            &service,
            &log,
        );
    }
    read_log(&socket, &log);
    eprintln!(
        "Android was ready again {} s after the start",
        started.elapsed().as_secs()
    );
    if let Err(why) = adb.connect(Duration::from_secs(120)) {
        fail(
            format!("adb did not reach the guest after the restart: {why}"),
            &service,
            &log,
        );
    }

    let table = adb.shell("cat /proc/self/mountinfo").unwrap_or_default();
    let _ = std::fs::write(dir.join("mounts-second.txt"), &table);
    for problem in addon_disk_problems(&table) {
        problems.push(format!("add-on disk: {problem}"));
    }
    for problem in gapps_mount_problems(&table) {
        problems.push(format!("Google Play: {problem}"));
    }
    match adb.missing(GAPPS_FILES) {
        Ok(missing) => {
            for path in missing {
                problems.push(format!("Google Play: {path} is not in the system"));
            }
        }
        Err(why) => problems.push(why),
    }
    match api.gapps() {
        Ok(status)
            if status
                .guest
                .as_ref()
                .is_some_and(|guest| guest.active && guest.installed && !guest.left_on_data) => {}
        Ok(status) => problems.push(format!(
            "Google Play: the guest says the Google apps are not laid over its system from the \
             add-on disk, or an image of them is left in /data ({:?})",
            status.guest
        )),
        Err(why) => problems.push(format!("reading where Google Play stands: {why}")),
    }
    match api.arm_translation() {
        Ok(status)
            if status
                .guest
                .as_ref()
                .is_some_and(|guest| !guest.left_on_data) => {}
        Ok(status) => problems.push(format!(
            "ARM translation: a package image is left in /data ({:?})",
            status.guest
        )),
        Err(why) => problems.push(format!("reading where ARM translation stands: {why}")),
    }
    // The host's allowlist grants what the build's own list leaves out, and a boot with the
    // Google apps logs a miss instead of stopping at it.
    let (allowlist, permission) = GAPPS_ALLOWLIST;
    match adb.shell(&format!("cat {allowlist}")) {
        Ok(text) if text.contains(&format!("<permission name=\"{permission}\"/>")) => {}
        Ok(text) => problems.push(format!(
            "Google Play: {allowlist} does not grant {permission}: {}",
            text.trim()
        )),
        Err(why) => problems.push(format!("reading {allowlist}: {why}")),
    }
    match adb.shell("getprop ro.control_privapp_permissions") {
        Ok(mode) if mode.trim() == "log" => {}
        Ok(mode) => problems.push(format!(
            "Google Play: ro.control_privapp_permissions is {:?} with the Google apps, not \
             \"log\", so a permission no list names stops Android at every boot",
            mode.trim()
        )),
        Err(why) => problems.push(format!("reading ro.control_privapp_permissions: {why}")),
    }
    translation_problems(&adb, &dir, &table, &mut problems);
    gapps_boot_problems(&adb, &dir, &mut problems);
    log_problems(&adb, &dir, "second boot", &mut problems);
    cached_limit_problems(&adb, &dir, "second boot", &mut problems);
    // With Google Play, which may write the device configuration the pinner also reads.
    pinned_memory_problems(&adb, &dir, "second boot", &mut problems);
    play_services_problems(&adb, &gms_probe, &dir, &mut problems);

    // The service's own copy of the guest log, across both boots.
    let whole = lock(&log).join("\n");
    let _ = std::fs::write(dir.join("guest.log"), &whole);
    for line in failed_commands(&whole) {
        problems.push(format!("init failed an overlay command: {line}"));
    }
    for line in mount_denials(&whole) {
        problems.push(format!("SELinux refused init: {line}"));
    }
    for line in whole
        .lines()
        .filter(|line| line.contains(ALLOWLIST_VIOLATION))
    {
        problems.push(format!(
            "Google Play: a privileged permission is in no allowlist: {}",
            line.trim()
        ));
    }
    refusal_problems(&whole, "guest log", &mut problems);
    // The console logs of both boots, which hold what the kernel refused before logd ran.
    let console = console_logs();
    let _ = std::fs::write(dir.join("android-console.txt"), &console);
    refusal_problems(&console, "console log", &mut problems);

    if !problems.is_empty() {
        problems.dedup();
        fail(problems.join("\n"), &service, &log);
    }
}

/// How long the idle reclaim is watched for, with Google apps installed and no app window.
const IDLE_WINDOW: Duration = Duration::from_secs(300);
/// Every how many of the sampler's looks (one a second) it also reads every process and cgroup.
const IDLE_SNAPSHOT_EVERY: u32 = 10;
/// The whole measurement, both boots, LiteGapps' download and the window included, stays under
/// twenty-two minutes.
const IDLE_BUDGET: Duration = Duration::from_secs(1320);
/// How long LiteGapps' build (120 to 260 MiB) may take to download.
const LITEGAPPS_DOWNLOAD: Duration = Duration::from_secs(420);
/// Where the sampler is put in the guest: the one place the shell may run a file it wrote.
const SAMPLER_GUEST: &str = "/data/local/tmp/dsa-idle-sampler.sh";
/// The guest's memory and processors while the idle reclaim is measured.
const IDLE_MEMORY_MB: u64 = 4096;
const IDLE_PROCESSORS: u64 = 2;

/// Puts LiteGapps' newest Core build for `arch` at API level `sdk` into the mirror at `mirror`,
/// in place of the small stand-in build, so the service installs the build a user gets.
/// Returns its file name.
fn mirror_litegapps(mirror: &Path, arch: &str, sdk: u32) -> Result<String, String> {
    use dsa_host_core::gapps::{listing, Target};
    let abi = if arch == "x86_64" {
        "x86_64"
    } else {
        "arm64-v8a"
    };
    let target = Target::for_guest(sdk, abi)?;
    let build = listing::newest(&dsa_host_linux::fetch::CurlFetch, listing::PROJECT, &target)?;
    let folder = format!("/litegapps/{}/{}/core", target.arch, target.sdk);
    let path = format!("{folder}/{}/{}", build.folder, build.file);
    let download = mirror.join(format!("files{path}/download"));
    std::fs::create_dir_all(download.parent().unwrap())
        .map_err(|why| format!("making {}: {why}", download.display()))?;
    let out = run_bounded(
        Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--retry",
                "3",
                "--connect-timeout",
                "30",
                "--max-time",
            ])
            .arg(LITEGAPPS_DOWNLOAD.as_secs().to_string())
            .arg("--output")
            .arg(&download)
            .arg(listing::download_url(listing::PROJECT, &target, &build)),
        LITEGAPPS_DOWNLOAD + Duration::from_secs(30),
        usize::MAX,
    )?;
    if !out.status.is_some_and(|status| status.success()) {
        let _ = std::fs::remove_file(&download);
        return Err(format!(
            "downloading {}: {}",
            build.file,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut file = std::fs::File::open(&download)
        .map_err(|why| format!("reading {}: {why}", download.display()))?;
    let mut md5 = Md5::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)
            .map_err(|why| format!("reading {}: {why}", download.display()))?;
        if read == 0 {
            break;
        }
        md5.update(&buffer[..read]);
        size += read as u64;
    }
    let digest = hex(&md5.finish());
    if size != build.size || digest != build.md5 {
        let _ = std::fs::remove_file(&download);
        return Err(format!(
            "{} arrived as {size} bytes with MD5 {digest}; LiteGapps lists {} bytes with {}",
            build.file, build.size, build.md5
        ));
    }
    std::fs::write(
        mirror.join(format!("rss?path={folder}")),
        feed::feed(&[feed::item(&path, size, &digest)]),
    )
    .map_err(|why| format!("writing the mirror's list: {why}"))?;
    Ok(build.file)
}

/// Android booted twice by a measurement: first to install Google Play from the mirror, then
/// with it in place, with developer mode on so adb reaches the guest.
struct GooglePlayBoot {
    service: Service,
    _compositor: Service,
    api: HostApiClient,
    adb: Adb,
    log: Arc<Mutex<Vec<String>>>,
    dir: PathBuf,
    started: Instant,
    /// Which Google apps build was installed, and the image mounted from it, for the report.
    gapps: String,
    mounted: String,
    /// The service's configuration file and what it says, so a measurement can start the
    /// service again with a setting changed.
    config: PathBuf,
    settings: serde_json::Value,
    socket: PathBuf,
}

impl GooglePlayBoot {
    /// Fails the measurement with what the service and the guest said.
    fn fail(&self, why: String) -> ! {
        fail(why, &self.service, &self.log)
    }
}

/// Boots the image with Google Play for a measurement: installs LiteGapps' own newest build
/// for the guest (the check's small stand-in build when LiteGapps cannot be reached, which the
/// returned description says), starts Android again with it and checks that its image is
/// mounted. The VM has `memory_mb` of memory and `processors` processors. The second boot
/// leaves `after` of the time to `deadline` for the measurement itself. `kernel_cmdline` goes on
/// the guest's kernel command line after the image's. `direct_mapped` attaches the read-only
/// disks as persistent memory, mapped with DAX. `None` when CI named no crosvm and
/// image, so there is nothing to boot.
fn boot_with_google_play(
    deadline: Instant,
    after: Duration,
    memory_mb: u64,
    processors: u64,
    kernel_cmdline: &str,
    direct_mapped: bool,
) -> Option<GooglePlayBoot> {
    let (Some(crosvm), Some(image)) = (
        std::env::var_os("DSA_CROSVM").map(PathBuf::from),
        std::env::var_os("DSA_WIDGET_TEST_IMAGE").map(PathBuf::from),
    ) else {
        eprintln!("skipped: DSA_CROSVM and DSA_WIDGET_TEST_IMAGE name no crosvm and image");
        return None;
    };
    let kernel = std::env::var_os("DSA_WIDGET_TEST_KERNEL").map(PathBuf::from);
    let adb_program = std::env::var_os("DSA_ADB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("adb"));
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(image.join("image.json")).expect("reading the image's image.json"),
    )
    .expect("the image's image.json is JSON");
    let arch = manifest["arch"].as_str().unwrap_or("x86_64").to_string();

    let started = Instant::now();
    let dir = scratch();
    let mirror = dir.join("mirror");
    write_mirror(&mirror, &arch).unwrap();
    let socket = dir.join("host.sock");
    let config = dir.join("dsa-hostd.json");
    let settings = serde_json::json!({
        "image_dir": image,
        "kernel": kernel,
        "crosvm": crosvm,
        "socket": socket,
        "machine_dir": dir.join("machine"),
        "network": false,
        "memory_mb": memory_mb,
        "processors": processors,
        "data_disk_size_gb": 8,
        "gapps_project": format!("file://{}", mirror.display()),
        "kernel_cmdline": kernel_cmdline,
        "direct_mapped_images": direct_mapped,
    });
    std::fs::write(&config, settings.to_string()).unwrap();
    let service = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-linux-hostd"))
            .arg("--config")
            .arg(&config),
        dir.join("hostd.log"),
    );
    let compositor = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-compositor")).env("DSA_SOCKET", &socket),
        dir.join("compositor.log"),
    );
    let mut api = client(&socket, Duration::from_secs(30));
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    read_log(&socket, &log);
    let boot = (deadline.saturating_duration_since(Instant::now())) / 4;
    if let Err(why) = api.ensure_running(boot, Duration::from_secs(2), |state| {
        eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
    }) {
        fail(
            format!("Android did not become ready: {why}"),
            &service,
            &log,
        );
    }

    // Developer mode, so adb reaches the guest through the service's loopback port.
    let mut wanted = match api.settings() {
        Ok(report) => report.settings,
        Err(why) => fail(format!("reading the settings: {why}"), &service, &log),
    };
    wanted.developer_mode = true;
    wanted.adb_auto_connect = false;
    let port = wanted.adb_port;
    if let Err(why) = api.set_settings(&wanted) {
        fail(format!("turning developer mode on: {why}"), &service, &log);
    }
    let adb = Adb {
        program: adb_program,
        serial: format!("127.0.0.1:{port}"),
    };
    if let Err(why) = adb.connect(Duration::from_secs(120)) {
        fail(
            format!("adb did not reach the guest: {why}"),
            &service,
            &log,
        );
    }

    // LiteGapps' own build, which a user's Android runs, wherever it can be had.
    let sdk = adb
        .shell("getprop ro.build.version.sdk")
        .ok()
        .and_then(|sdk| sdk.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let gapps = match mirror_litegapps(&mirror, &arch, sdk) {
        Ok(file) => format!("LiteGapps' {file}"),
        Err(why) => {
            println!(
                "::warning::the measurement runs with the check's small stand-in Google apps build, not LiteGapps' own: {why}"
            );
            format!(
                "the check's small stand-in build, because LiteGapps' own could not be had ({why}); \
                 it holds no Google Play services that run, so this measures only the system with \
                 the image laid over"
            )
        }
    };
    eprintln!("{} s: installing {gapps}", started.elapsed().as_secs());
    let install_until = Instant::now() + Duration::from_secs(240);
    let installed = loop {
        let result = client(&socket, Duration::from_secs(10)).install_gapps(&mut |news| {
            eprintln!("Google Play: {news:?}");
        });
        match result {
            Ok(status) => break status,
            Err(why) if Instant::now() >= install_until => fail(
                format!("installing the Google apps from the mirror: {why}"),
                &service,
                &log,
            ),
            Err(why) => {
                eprintln!("installing the Google apps from the mirror: {why}; trying again");
                std::thread::sleep(Duration::from_secs(3));
            }
        }
    };
    if installed.package.is_none() || !installed.restart_needed {
        fail(
            format!(
                "the service took the Google apps but does not say they apply from the next start: {:?}",
                installed.package
            ),
            &service,
            &log,
        );
    }

    // Android starts again, with the Google apps.
    if let Err(why) = api.stop() {
        fail(format!("stopping Android: {why}"), &service, &log);
    }
    let stop_until = Instant::now() + Duration::from_secs(120);
    let mut left = false;
    loop {
        use dsa_host_core::session::SessionState;
        match api.status() {
            Ok(status) if status.state == SessionState::Stopped => break,
            Ok(status) if left && status.state == SessionState::Starting => break,
            Ok(status) if Instant::now() < stop_until => {
                left |= status.state == SessionState::Stopping;
                std::thread::sleep(Duration::from_secs(1))
            }
            Ok(status) => fail(
                format!("Android did not stop: it is {:?}", status.state),
                &service,
                &log,
            ),
            Err(why) => fail(format!("reading the status: {why}"), &service, &log),
        }
    }
    let boot = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(after);
    if let Err(why) = api.ensure_running(boot, Duration::from_secs(2), |state| {
        eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
    }) {
        fail(
            format!("Android did not become ready again with the Google apps: {why}"),
            &service,
            &log,
        );
    }
    read_log(&socket, &log);
    if let Err(why) = adb.connect(Duration::from_secs(120)) {
        fail(
            format!("adb did not reach the guest after the restart: {why}"),
            &service,
            &log,
        );
    }
    let table = adb.shell("cat /proc/self/mountinfo").unwrap_or_default();
    let _ = std::fs::write(dir.join("google-play-mounts.txt"), &table);
    let Some(mounted) = mounts(&table)
        .iter()
        .find(|m| m.point == GAPPS_POINT && m.kind == "erofs")
        .map(|m| format!("{} ({})", m.source, m.super_options))
    else {
        fail(
            format!(
                "no Google apps image is mounted at {GAPPS_POINT}, so there is nothing to measure"
            ),
            &service,
            &log,
        );
    };
    Some(GooglePlayBoot {
        service,
        _compositor: compositor,
        api,
        adb,
        log,
        dir,
        started,
        gapps,
        mounted,
        config,
        settings,
        socket,
    })
}

#[test]
fn nfr3_the_idle_reclaim_with_google_apps_is_measured_on_the_booted_image() {
    let deadline = Instant::now() + IDLE_BUDGET;
    let _watchdog = Watchdog::start("the idle reclaim measurement", IDLE_BUDGET + OVERRUN);
    let Some(boot) = boot_with_google_play(
        deadline,
        IDLE_WINDOW + Duration::from_secs(180),
        IDLE_MEMORY_MB,
        IDLE_PROCESSORS,
        "",
        false,
    ) else {
        return;
    };
    let GooglePlayBoot {
        service,
        adb,
        log,
        dir,
        started,
        gapps,
        mounted,
        ..
    } = &boot;

    // Five minutes with no app window open, sampled from inside the guest.
    let sampler = dir.join("idle-sampler.sh");
    std::fs::write(&sampler, idle_reclaim::SAMPLER).unwrap();
    if let Err(why) = adb.run(&["push", &sampler.to_string_lossy(), SAMPLER_GUEST]) {
        fail(
            format!("putting the sampler in the guest: {why}"),
            service,
            log,
        );
    }
    eprintln!(
        "{} s: watching the idle reclaim for {} s",
        started.elapsed().as_secs(),
        IDLE_WINDOW.as_secs()
    );
    let command = format!(
        "sh {SAMPLER_GUEST} {} {IDLE_SNAPSHOT_EVERY}",
        IDLE_WINDOW.as_secs()
    );
    let raw = match adb.run_for(&["shell", &command], IDLE_WINDOW + Duration::from_secs(120)) {
        Ok(out) => String::from_utf8_lossy(&out).into_owned(),
        Err(why) => fail(format!("the idle sampler: {why}"), service, log),
    };
    let _ = std::fs::write(dir.join("idle-reclaim-samples.txt"), &raw);
    let guest_log = adb
        .shell("logcat -d -b kernel -b main -b system -v monotonic")
        .unwrap_or_default();
    let _ = std::fs::write(dir.join("idle-reclaim-log.txt"), &guest_log);

    let record = idle_reclaim::parse(&raw);
    let events = idle_reclaim::log_events(&guest_log);
    let report = idle_reclaim::report(
        &record,
        &events,
        &format!("{gapps}, its image mounted from {mounted}"),
        IDLE_MEMORY_MB,
        IDLE_PROCESSORS,
    );
    let _ = std::fs::write(dir.join("idle-reclaim.md"), &report.markdown);
    let _ = std::fs::write(
        dir.join("idle-reclaim.json"),
        serde_json::to_string_pretty(&report.json).unwrap_or_default(),
    );
    println!("{}", report.markdown);
    if record.looks.len() < 10 {
        fail(
            format!(
                "the idle sampler gave {} looks in {} s, too few to measure; its output is in idle-reclaim-samples.txt",
                record.looks.len(),
                IDLE_WINDOW.as_secs()
            ),
            service,
            log,
        );
    }
    // The one boot with a Play services that runs: none of its alarms, nor any other, may
    // retry as Play services' flag sync did while it could not write DeviceConfig.
    let mut storms = Vec::new();
    alarm_storm_problems(adb, &dir.join("idle-reclaim-alarms.txt"), &mut storms);
    if !storms.is_empty() {
        fail(storms.join("\n"), service, log);
    }
}

/// The memory measurement, both boots, LiteGapps' download, the kernel and system_server
/// reports and the part after the window closes included, stays under twenty-nine minutes,
/// inside the job's thirty-four.
const MEMORY_BUDGET: Duration = Duration::from_secs(1740);
/// How long the heap dump of system_server, and then copying it out, may each take.
const HEAP_DUMP_WAIT: Duration = Duration::from_secs(60);
/// The zram writeback's times for this check, on the guest's kernel command line: a writeback
/// 30 s after no app is in use, of pages unused for 60 s. The defaults (ten minutes, half an
/// hour) are longer than the check can wait.
const MEMORY_WRITEBACK_CMDLINE: &str =
    "androidboot.dsa.zram_writeback_after=30 androidboot.dsa.zram_writeback_age=60";
/// How long after the window closes the check waits for the first zram writeback: the daemon
/// looks every 30 s and waits 30 s of no app in use.
const MEMORY_WRITEBACK_WAIT: Duration = Duration::from_secs(150);
/// The readings after the window closed and the writeback ran, and how far apart.
const MEMORY_IDLE_READINGS: u32 = 3;
/// The app whose window is open while the memory is read: the simple app the one-window target
/// is set for.
const MEMORY_APP: &str = "com.android.settings";
/// How long its task may take to appear.
const MEMORY_APP_WAIT: Duration = Duration::from_secs(90);
/// How long the window stays untouched before the first reading.
const MEMORY_SETTLE: Duration = Duration::from_secs(60);
/// How many readings are taken, and how far apart.
const MEMORY_READINGS: u32 = 6;
const MEMORY_EVERY: Duration = Duration::from_secs(10);

/// The service's own process (`dsa-linux-hostd`), which the host's snapshot does not list.
fn service_memory() -> memory_check::ServiceMemory {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return memory_check::ServiceMemory::default();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let named = std::fs::read_to_string(path.join("comm"))
            .is_ok_and(|name| name.trim() == "dsa-linux-hostd");
        if !named {
            continue;
        }
        let status = std::fs::read_to_string(path.join("status")).unwrap_or_default();
        let resident_kb = dsa_host_linux::memory::resident_kb(&status).unwrap_or(0);
        let private_kb = std::fs::read_to_string(path.join("smaps_rollup"))
            .ok()
            .and_then(|text| dsa_host_linux::memory::private_kb(&text))
            .unwrap_or(resident_kb);
        return memory_check::ServiceMemory {
            resident_kb,
            private_kb,
        };
    }
    memory_check::ServiceMemory::default()
}

/// How long the guest has to write its kernel memory report once asked.
const KERNEL_REPORT_WAIT: Duration = Duration::from_secs(60);

/// Asks the guest for its kernel memory report (root reads the slab caches, vmalloc users and
/// threads, which the shell cannot) and waits for it in the kernel log. The log goes to
/// `memory-kernel-log.txt` and the report's lines to `memory-kernel.txt`.
fn kernel_memory_report(adb: &Adb, dir: &Path) -> Result<Vec<String>, String> {
    let property = memory_check::KERNEL_REPORT_PROPERTY;
    adb.shell(&format!("setprop {property} 0; setprop {property} 1"))?;
    let until = Instant::now() + KERNEL_REPORT_WAIT;
    let mut log = String::new();
    loop {
        if let Ok(text) = adb.shell("logcat -d -b kernel") {
            log = text;
        }
        if let Some(lines) = memory_check::kernel_report(&log) {
            let _ = std::fs::write(dir.join("memory-kernel-log.txt"), &log);
            let _ = std::fs::write(dir.join("memory-kernel.txt"), lines.join("\n"));
            return Ok(lines);
        }
        if Instant::now() >= until {
            let _ = std::fs::write(dir.join("memory-kernel-log.txt"), &log);
            return Err(format!(
                "the guest wrote no whole report to the kernel log within {} s of {property} \
                 set to 1 (dsa_memory_report in dsa-channeld.rc; look in memory-kernel-log.txt \
                 for its lines and for an avc denial of dsa_memory_report)",
                KERNEL_REPORT_WAIT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// system_server's memory: its `dumpsys meminfo` (`memory-system-server.txt`, with ART's
/// details), its threads, the services it lists (`memory-services.txt`), what it holds locked
/// in memory (`memory-pinner.txt`, with the kernel's locked page count), the ART heap
/// settings, and a heap dump when the build allows one (`memory-system-server.hprof`; a user
/// build refuses it, and the report says what it answered).
fn system_server_memory(adb: &Adb, dir: &Path) -> (String, serde_json::Value) {
    let pid = adb
        .shell("pidof system_server")
        .unwrap_or_default()
        .trim()
        .to_string();
    let meminfo = if pid.is_empty() {
        String::new()
    } else {
        adb.shell(&format!("dumpsys meminfo -d {pid}"))
            .unwrap_or_else(|why| why)
    };
    let _ = std::fs::write(dir.join("memory-system-server.txt"), &meminfo);
    let threads = if pid.is_empty() {
        String::new()
    } else {
        adb.shell(&format!("cat /proc/{pid}/task/*/comm"))
            .unwrap_or_default()
    };
    let services = adb.shell("dumpsys -l").unwrap_or_default();
    let _ = std::fs::write(dir.join("memory-services.txt"), &services);
    // The features decide which services system_server starts.
    let features = adb.shell("pm list features").unwrap_or_default();
    let _ = std::fs::write(dir.join("memory-features.txt"), &features);
    // What system_server holds locked in memory (none, with the image's pinner settings), and
    // the kernel's count of locked and unevictable pages.
    let pinner = adb.shell("dumpsys pinner").unwrap_or_default();
    let locked = adb
        .shell("grep -E '^(Mlocked|Unevictable):' /proc/meminfo")
        .unwrap_or_default();
    let _ = std::fs::write(dir.join("memory-pinner.txt"), format!("{locked}\n{pinner}"));
    let heap_settings = memory_check::heap_settings(&adb.shell("getprop").unwrap_or_default());
    let heap_dump = if pid.is_empty() {
        "system_server was not found".to_string()
    } else {
        let path = "/data/local/tmp/system_server.hprof";
        let answer = adb
            .run_for(
                &["shell", &format!("am dumpheap {pid} {path}")],
                HEAP_DUMP_WAIT,
            )
            .map(|out| String::from_utf8_lossy(&out).into_owned())
            .unwrap_or_else(|why| why);
        let kept = dir.join("memory-system-server.hprof");
        let kept_path = kept.to_string_lossy().into_owned();
        let pulled = adb
            .run_for(&["pull", path, kept_path.as_str()], HEAP_DUMP_WAIT)
            .is_ok()
            && std::fs::metadata(&kept).is_ok_and(|m| m.len() > 0);
        let _ = adb.shell(&format!("rm -f {path}"));
        if pulled {
            "kept as memory-system-server.hprof".to_string()
        } else {
            let answer: String = answer
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(300)
                .collect();
            format!("none (`am dumpheap` answered: {answer})")
        }
    };
    memory_check::system_server_report(&memory_check::SystemServer {
        meminfo: &meminfo,
        threads: &threads,
        services: &services,
        heap_settings: &heap_settings,
        heap_dump: &heap_dump,
    })
}

/// The runner the measurement ran on: its kernel, processor and memory.
fn runner_environment() -> String {
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let processor = cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| rest.split_once(':'))
        .map_or("an unknown processor", |(_, name)| name.trim());
    let threads = cpuinfo
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count();
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let memory_mib = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map_or(0, memory_check::mib);
    format!(
        "Linux {}, {processor} ({threads} threads), {memory_mib} MiB, crosvm over KVM",
        kernel.trim()
    )
}

/// Opens one window of [`MEMORY_APP`], as a user does, leaves it untouched for a minute and
/// reads the memory [`MEMORY_READINGS`] times as `dsa memory` reads it. What each reading said
/// goes to `<name>-readings.txt` in the boot's folder.
fn one_window_readings(boot: &mut GooglePlayBoot, name: &str) -> Vec<memory_check::Reading> {
    eprintln!(
        "{} s: opening {MEMORY_APP}",
        boot.started.elapsed().as_secs()
    );
    if let Err(why) = boot.api.launch_app(MEMORY_APP) {
        boot.fail(format!("opening {MEMORY_APP}: {why}"));
    }
    let until = Instant::now() + MEMORY_APP_WAIT;
    loop {
        match boot.api.tasks() {
            Ok(list) if list.tasks.iter().any(|t| t.package_name == MEMORY_APP) => break,
            Ok(_) if Instant::now() < until => std::thread::sleep(Duration::from_secs(1)),
            Ok(list) => {
                let open: Vec<String> = list.tasks.into_iter().map(|t| t.package_name).collect();
                boot.fail(format!(
                    "{MEMORY_APP}'s task did not appear within {} s; the tasks are {open:?}",
                    MEMORY_APP_WAIT.as_secs()
                ))
            }
            Err(why) => boot.fail(format!("listing the tasks: {why}")),
        }
    }

    // A minute untouched, then the readings, as `dsa memory` reads them.
    std::thread::sleep(MEMORY_SETTLE);
    let mut readings = Vec::new();
    let mut raw = Vec::new();
    for index in 0..MEMORY_READINGS {
        if index > 0 {
            std::thread::sleep(MEMORY_EVERY);
        }
        let snapshot = match boot.api.memory() {
            Ok(snapshot) => snapshot,
            Err(why) => boot.fail(format!("reading the memory: {why}")),
        };
        raw.push(snapshot.to_string());
        if let Some(reading) = memory_check::reading(&snapshot, service_memory()) {
            readings.push(reading);
        }
    }
    let _ = std::fs::write(
        boot.dir.join(format!("{name}-readings.txt")),
        raw.join("\n---\n"),
    );
    readings
}

/// What the VM's crosvm process holds by kind, from `/proc/<pid>/status`: the largest crosvm
/// the runner has, which is the VM's main process (its device processes hold little).
fn crosvm_resident() -> memory_check::direct_mapped::Resident {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Default::default();
    };
    entries
        .flatten()
        .filter(|entry| {
            std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim() == "crosvm")
        })
        .map(|entry| {
            let status = std::fs::read_to_string(entry.path().join("status")).unwrap_or_default();
            memory_check::direct_mapped::resident(&status)
        })
        .max_by_key(|resident| resident.anon_kb + resident.file_kb + resident.shmem_kb)
        .unwrap_or_default()
}

/// The direct-mapped measurement, its four boots, LiteGapps' download and the making of the
/// uncompressed system disk included, stays under fifty minutes.
const DIRECT_MAPPED_BUDGET: Duration = Duration::from_secs(3000);

/// How long the service may take to make and check the uncompressed system disk.
const DIRECT_MAPPED_DISK_WAIT: Duration = Duration::from_secs(900);

/// Stops Android and starts it again in the same service, and connects adb again.
fn restart_android(boot: &mut GooglePlayBoot, within: Duration, why: &str) {
    use dsa_host_core::session::SessionState;
    eprintln!(
        "{} s: starting Android again {why}",
        boot.started.elapsed().as_secs()
    );
    if let Err(error) = boot.api.stop() {
        boot.fail(format!("stopping Android: {error}"));
    }
    let stop_until = Instant::now() + Duration::from_secs(120);
    loop {
        match boot.api.status() {
            Ok(status) if status.state == SessionState::Stopped => break,
            Ok(_) if Instant::now() < stop_until => std::thread::sleep(Duration::from_secs(1)),
            Ok(status) => boot.fail(format!("Android did not stop: it is {:?}", status.state)),
            Err(error) => boot.fail(format!("reading the status: {error}")),
        }
    }
    let started = boot.started;
    if let Err(error) = boot
        .api
        .ensure_running(within, Duration::from_secs(2), |state| {
            eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
        })
    {
        boot.fail(format!("Android did not become ready {why}: {error}"));
    }
    if let Err(error) = boot.adb.connect(Duration::from_secs(120)) {
        boot.fail(format!("adb did not reach the guest {why}: {error}"));
    }
}

/// What a boot says of its verified boot: `ro.boot.avb_version` and the kernel log's lines
/// about AVB and dm-verity.
fn verified_boot(adb: &Adb) -> (String, Vec<String>) {
    let version = adb
        .shell("getprop ro.boot.avb_version")
        .unwrap_or_default()
        .trim()
        .to_string();
    let log = adb.shell("logcat -d -b kernel").unwrap_or_default();
    let lines = log
        .lines()
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("avb") || line.contains("verity")
        })
        .map(str::to_string)
        .collect();
    (version, lines)
}

#[test]
fn nfr1_nfr2_the_direct_mapped_disks_are_measured_against_the_cached_ones_on_the_booted_image() {
    // The same image twice: its system and add-on disks first on virtio-pmem, mapped with DAX
    // (Google Play is installed in this mode), then on virtio-blk, cached by the guest, with
    // Google Play and one app window both times. It fails only when the mapped boot did not map
    // its read-only partitions; the numbers are reported, not judged.
    let deadline = Instant::now() + DIRECT_MAPPED_BUDGET;
    let _watchdog = Watchdog::start(
        "the direct-mapped measurement",
        DIRECT_MAPPED_BUDGET + OVERRUN,
    );
    let one_window =
        MEMORY_APP_WAIT + MEMORY_SETTLE + MEMORY_EVERY * MEMORY_READINGS + Duration::from_secs(60);
    // The making of the disk and a boot, the mapped measurement, then a boot and the cached
    // measurement.
    let after = DIRECT_MAPPED_DISK_WAIT + one_window * 2 + Duration::from_secs(720);
    let Some(mut boot) =
        boot_with_google_play(deadline, after, IDLE_MEMORY_MB, IDLE_PROCESSORS, "", true)
    else {
        return;
    };
    // The image is the published one, compressed: the service makes its uncompressed system
    // disk in the background while Android runs from the image's own, and the next start of
    // Android maps it.
    let image = PathBuf::from(std::env::var_os("DSA_WIDGET_TEST_IMAGE").expect("named above"));
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(image.join("image.json")).unwrap()).unwrap();
    if manifest["direct_mapped"]["system_disk"].is_null() {
        boot.fail(
            "the image's image.json describes no direct-mapped system disk (dsa-release \
             direct-mapped did not record one)"
                .into(),
        );
    }
    let version = manifest["version"].as_str().unwrap_or_default().to_string();
    let copy = boot
        .dir
        .join("machine")
        .join("direct-mapped")
        .join(format!("{version}.img"));
    let record = copy.with_extension("img.json");
    let waited = Instant::now();
    while !record.exists() {
        if waited.elapsed() > DIRECT_MAPPED_DISK_WAIT {
            boot.fail(format!(
                "the service did not make and check the direct-mapped system disk {} within {} s",
                copy.display(),
                DIRECT_MAPPED_DISK_WAIT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    let made_in = waited.elapsed();
    let (_, before) = dsa_host_core::direct_mapped_disk::file_sha256(&copy).unwrap();
    let made = format!(
        "{} bytes, ready {} s after Android first booted with Google Play; SHA-256 {before}",
        std::fs::metadata(&copy).map(|m| m.len()).unwrap_or(0),
        made_in.as_secs()
    );
    eprintln!("the direct-mapped system disk: {made}");
    let _ = std::fs::write(boot.dir.join("direct-mapped-disk.txt"), &made);
    // The boot before it, from the image's own disk, checked its verified boot data.
    let (cached_avb, cached_avb_lines) = verified_boot(&boot.adb);
    let rest = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(one_window * 2 + Duration::from_secs(420));
    restart_android(&mut boot, rest, "with its disks mapped");
    let mapped_table = boot
        .adb
        .shell("cat /proc/self/mountinfo")
        .unwrap_or_default();
    let _ = std::fs::write(boot.dir.join("direct-mapped-mounts.txt"), &mapped_table);
    let devices = boot
        .adb
        .shell(
            "ls -l /sys/block /dev/block/by-name; getprop ro.boot.boot_devices; \
             getprop ro.boot.fstab_suffix; getprop ro.boot.dsa.direct_mapped; \
             getprop ro.boot.dsa.gapps_image",
        )
        .unwrap_or_default();
    let _ = std::fs::write(boot.dir.join("direct-mapped-devices.txt"), &devices);
    let kernel = boot
        .adb
        .shell("logcat -d -b kernel -v monotonic")
        .unwrap_or_default();
    let dax_lines: Vec<&str> = kernel
        .lines()
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("dax") || line.contains("pmem") || line.contains("nvdimm")
        })
        .collect();
    let _ = std::fs::write(
        boot.dir.join("direct-mapped-kernel.txt"),
        dax_lines.join("\n"),
    );
    let mapped_mounts = memory_check::direct_mapped::erofs_mounts(&mapped_table);
    let problems = memory_check::direct_mapped::mapping_problems(&mapped_mounts, GAPPS_POINT);
    if !problems.is_empty() {
        boot.fail(format!(
            "the direct-mapped boot did not map its read-only disks:\n{}\nthe kernel said:\n{}",
            problems.join("\n"),
            dax_lines.join("\n")
        ));
    }
    // The mapped boot still checks the verified boot data as the boot from the image's own
    // disk did: first stage init opens it for odm, which keeps dm-verity.
    let (mapped_avb, mapped_avb_lines) = verified_boot(&boot.adb);
    let _ = std::fs::write(
        boot.dir.join("direct-mapped-avb.txt"),
        format!(
            "image's own disk: ro.boot.avb_version={cached_avb}\n{}\n\nmapped: \
             ro.boot.avb_version={mapped_avb}\n{}\n",
            cached_avb_lines.join("\n"),
            mapped_avb_lines.join("\n")
        ),
    );
    if mapped_avb.is_empty() || mapped_avb != cached_avb {
        boot.fail(format!(
            "the direct-mapped boot reports ro.boot.avb_version {mapped_avb:?}, the boot from the \
             image's own disk {cached_avb:?}: its verified boot data was not checked the same \
             way (direct-mapped-avb.txt)"
        ));
    }
    if !cached_avb_lines.is_empty() && mapped_avb_lines.is_empty() {
        boot.fail(
            "the direct-mapped boot's kernel log has no AVB or dm-verity line, and the boot from \
             the image's own disk had some (direct-mapped-avb.txt)"
                .into(),
        );
    }
    // The persistent memory disks are read only in the guest, and a write to one does not
    // reach the host's file.
    let read_only = boot
        .adb
        .shell("for d in /sys/block/pmem*; do echo \"$d $(cat $d/ro)\"; done")
        .unwrap_or_default();
    let write = boot
        .adb
        .shell("dd if=/dev/zero of=/dev/block/pmem0 bs=4096 count=1 seek=1 2>&1; echo status $?")
        .unwrap_or_default();
    let _ = std::fs::write(
        boot.dir.join("direct-mapped-read-only.txt"),
        format!("{read_only}\n---\n{write}"),
    );
    let flags: Vec<&str> = read_only.lines().filter(|l| !l.trim().is_empty()).collect();
    if flags.is_empty() || flags.iter().any(|l| !l.trim_end().ends_with(" 1")) {
        boot.fail(format!(
            "the guest's persistent memory disks are not all read only:\n{read_only}"
        ));
    }
    let mapped = one_window_readings(&mut boot, "direct-mapped");
    let mapped_crosvm = crosvm_resident();
    let (_, after_boot) = dsa_host_core::direct_mapped_disk::file_sha256(&copy).unwrap();
    if after_boot != before {
        boot.fail(format!(
            "the direct-mapped system disk changed while the guest mapped it: SHA-256 {before} \
             before, {after_boot} after"
        ));
    }

    // The service again with the disks on virtio-blk; Google Play stays installed.
    eprintln!(
        "{} s: starting Android again with its disks cached",
        boot.started.elapsed().as_secs()
    );
    if let Err(why) = boot.api.stop() {
        boot.fail(format!("stopping Android: {why}"));
    }
    let stop_until = Instant::now() + Duration::from_secs(120);
    loop {
        use dsa_host_core::session::SessionState;
        match boot.api.status() {
            Ok(status) if status.state == SessionState::Stopped => break,
            Ok(_) if Instant::now() < stop_until => std::thread::sleep(Duration::from_secs(1)),
            Ok(status) => boot.fail(format!("Android did not stop: it is {:?}", status.state)),
            Err(why) => boot.fail(format!("reading the status: {why}")),
        }
    }
    boot.settings["direct_mapped_images"] = serde_json::Value::Bool(false);
    std::fs::write(&boot.config, boot.settings.to_string()).unwrap();
    // The old service ends before the new one takes its socket: a stand-in that exits at once
    // takes its place first, which stops it.
    boot.service = Service::spawn(&mut Command::new("true"), boot.dir.join("between.log"));
    let _ = std::fs::remove_file(&boot.socket);
    boot.service = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-linux-hostd"))
            .arg("--config")
            .arg(&boot.config),
        boot.dir.join("hostd-cached.log"),
    );
    boot.api = client(&boot.socket, Duration::from_secs(30));
    read_log(&boot.socket, &boot.log);
    let rest = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(one_window + Duration::from_secs(60));
    let started = boot.started;
    if let Err(why) = boot
        .api
        .ensure_running(rest, Duration::from_secs(2), |state| {
            eprintln!("{} s: Android is {state:?}", started.elapsed().as_secs());
        })
    {
        boot.fail(format!(
            "Android did not become ready with its disks cached: {why}"
        ));
    }
    if let Err(why) = boot.adb.connect(Duration::from_secs(120)) {
        boot.fail(format!(
            "adb did not reach the guest after the restart: {why}"
        ));
    }
    let cached_table = boot
        .adb
        .shell("cat /proc/self/mountinfo")
        .unwrap_or_default();
    let _ = std::fs::write(boot.dir.join("cached-mounts.txt"), &cached_table);
    let cached_mounts = memory_check::direct_mapped::erofs_mounts(&cached_table);
    if cached_mounts.iter().any(|m| m.dax) {
        boot.fail(
            "the boot with the setting off still mapped a disk with DAX; see cached-mounts.txt"
                .into(),
        );
    }
    let cached = one_window_readings(&mut boot, "cached");
    let cached_crosvm = crosvm_resident();

    let google_play = format!("{}, its image mounted from {}", boot.gapps, boot.mounted);
    let environment = runner_environment();
    let setting = memory_check::Setting {
        google_play: &google_play,
        app: "Settings",
        memory_mb: IDLE_MEMORY_MB,
        processors: IDLE_PROCESSORS,
        environment: &environment,
    };
    let Some(report) = memory_check::direct_mapped::report(
        &memory_check::direct_mapped::Boot {
            readings: &cached,
            crosvm: cached_crosvm,
            mounts: &cached_mounts,
        },
        &memory_check::direct_mapped::Boot {
            readings: &mapped,
            crosvm: mapped_crosvm,
            mounts: &mapped_mounts,
        },
        &setting,
    ) else {
        boot.fail(
            "a boot had no reading with both the guest's report and the VM's process, so \
             nothing was measured; they are in direct-mapped-readings.txt and cached-readings.txt"
                .into(),
        );
    };
    let _ = std::fs::write(boot.dir.join("direct-mapped.md"), &report.markdown);
    let _ = std::fs::write(
        boot.dir.join("direct-mapped.json"),
        serde_json::to_string_pretty(&report.json).unwrap_or_default(),
    );
    println!("{}", report.markdown);
}

#[test]
fn nfr1_the_memory_with_google_play_and_one_app_window_is_measured_on_the_booted_image() {
    let deadline = Instant::now() + MEMORY_BUDGET;
    let _watchdog = Watchdog::start("the memory measurement", MEMORY_BUDGET + OVERRUN);
    let after = MEMORY_APP_WAIT
        + MEMORY_SETTLE
        + MEMORY_EVERY * (MEMORY_READINGS + MEMORY_IDLE_READINGS)
        + MEMORY_WRITEBACK_WAIT
        + KERNEL_REPORT_WAIT
        + HEAP_DUMP_WAIT * 2
        + Duration::from_secs(180);
    let Some(mut boot) = boot_with_google_play(
        deadline,
        after,
        IDLE_MEMORY_MB,
        IDLE_PROCESSORS,
        MEMORY_WRITEBACK_CMDLINE,
        false,
    ) else {
        return;
    };
    let readings = one_window_readings(&mut boot, "memory");
    // What the kernel's part is made of, read by root in the guest right after the readings.
    let kernel = kernel_memory_report(&boot.adb, &boot.dir);
    let dumpsys = boot.adb.shell("dumpsys meminfo").unwrap_or_default();
    let _ = std::fs::write(boot.dir.join("memory-dumpsys.txt"), &dumpsys);
    let pss = memory_check::pss_by_process(&dumpsys);
    let (system_server_md, system_server_json) = system_server_memory(&boot.adb, &boot.dir);

    let baseline: serde_json::Value =
        serde_json::from_str(memory_check::BASELINE).expect("the memory baseline is JSON");
    let google_play = format!("{}, its image mounted from {}", boot.gapps, boot.mounted);
    let environment = runner_environment();
    let setting = memory_check::Setting {
        google_play: &google_play,
        app: "Settings",
        memory_mb: IDLE_MEMORY_MB,
        processors: IDLE_PROCESSORS,
        environment: &environment,
    };
    let Some(mut report) = memory_check::report(&readings, &pss, &baseline, &setting) else {
        boot.fail(format!(
            "none of the {MEMORY_READINGS} readings had both the guest's report and the VM's \
             process, so nothing was measured; they are in memory-readings.txt"
        ));
    };
    report.markdown.push('\n');
    match &kernel {
        Ok(lines) => {
            report
                .markdown
                .push_str(&memory_check::kernel_markdown(lines));
            report.json["kernel_report"] = serde_json::json!(lines);
        }
        Err(why) => {
            report.markdown.push_str(&format!(
                "## The guest kernel's memory\n\nNo kernel memory report: {why}\n\n"
            ));
            report.json["kernel_report"] = serde_json::json!({ "error": why });
        }
    }
    report.markdown.push_str(&system_server_md);
    report.json["system_server"] = system_server_json;

    // The window closes: the idle reclaim, the free pages reported again, and the zram
    // writeback, at the times the boot set. The numbers again, beside the ones above.
    let closed = Instant::now();
    match boot.api.tasks() {
        Ok(list) => {
            for task in list.tasks.iter().filter(|t| t.package_name == MEMORY_APP) {
                if let Err(why) = boot.api.close_task(task.task_id) {
                    boot.fail(format!("closing {MEMORY_APP}'s window: {why}"));
                }
            }
        }
        Err(why) => boot.fail(format!("listing the tasks: {why}")),
    }
    let guest_log = || {
        boot.adb
            .shell("logcat -d -b kernel -b main -v monotonic")
            .unwrap_or_default()
    };
    let mut log = guest_log();
    while memory_check::writeback_runs(&log).is_empty()
        && !memory_check::no_backing_file(&log)
        && closed.elapsed() < MEMORY_WRITEBACK_WAIT
    {
        std::thread::sleep(Duration::from_secs(5));
        log = guest_log();
    }
    // The reclaim and the rereport after the writeback settle before the readings.
    std::thread::sleep(MEMORY_EVERY);
    let mut idle_readings = Vec::new();
    let mut idle_raw = Vec::new();
    for index in 0..MEMORY_IDLE_READINGS {
        if index > 0 {
            std::thread::sleep(MEMORY_EVERY);
        }
        let snapshot = match boot.api.memory() {
            Ok(snapshot) => snapshot,
            Err(why) => boot.fail(format!("reading the memory after the window closed: {why}")),
        };
        idle_raw.push(snapshot.to_string());
        if let Some(reading) = memory_check::reading(&snapshot, service_memory()) {
            idle_readings.push(reading);
        }
    }
    let waited = closed.elapsed().as_secs();
    let log = guest_log();
    let _ = std::fs::write(
        boot.dir.join("memory-idle-readings.txt"),
        idle_raw.join("\n---\n"),
    );
    let _ = std::fs::write(boot.dir.join("memory-guest-log.txt"), &log);
    let runs = memory_check::writeback_runs(&log);
    let mut problems = Vec::new();
    match memory_check::idle_report(&readings, &idle_readings, &runs, waited) {
        Some(idle) => {
            report.markdown.push('\n');
            report.markdown.push_str(&idle.markdown);
            report.json["after_the_window_closes"] = idle.json;
        }
        None => problems.push(format!(
            "none of the {MEMORY_IDLE_READINGS} readings after the window closed had both the \
             guest's report and the VM's process; they are in memory-idle-readings.txt"
        )),
    }
    // A guest whose zram has no backing file keeps all of zram in memory: the image or the
    // kernel lost what the writeback needs.
    if memory_check::no_backing_file(&log) {
        problems.push(
            "the guest's zram has no backing file, so no writeback can run; the guest's log is \
             in memory-guest-log.txt"
                .to_string(),
        );
    } else if runs.is_empty() {
        println!(
            "::warning::no zram writeback ran within {} s of the window closing; the guest's log is in memory-guest-log.txt",
            MEMORY_WRITEBACK_WAIT.as_secs()
        );
    }

    let _ = std::fs::write(boot.dir.join("memory.md"), &report.markdown);
    let _ = std::fs::write(
        boot.dir.join("memory.json"),
        serde_json::to_string_pretty(&report.json).unwrap_or_default(),
    );
    println!("{}", report.markdown);
    if let Err(why) = &kernel {
        println!("::warning::no kernel memory report: {why}");
    }
    if !problems.is_empty() {
        boot.fail(problems.join("\n"));
    }
    let missing = memory_check::unrecorded(&baseline);
    if !missing.is_empty() {
        println!(
            "::warning::the memory baseline records no value for {}, so they are reported and not compared",
            missing.join(", ")
        );
    }
    let problems: Vec<String> =
        serde_json::from_value(report.json["regressions"].clone()).unwrap_or_default();
    if !problems.is_empty() {
        boot.fail(problems.join("\n"));
    }
}
