//! The images init lays over the system on the real image, with SELinux enforcing: the default
//! ARM translation package and a user's Google apps.
//!
//! Each is an erofs image that init mounts through a loop device with `context=` (the
//! translation labelled as system libraries, the Google apps as system files) and lays over a
//! partition with overlayfs, before the zygote starts. A `context=` mount is checked against
//! the type it names as well as the filesystem's own, and when the policy allowed only the
//! latter every boot refused both mounts ("avc: denied { relabelfrom } ... tclass=filesystem"):
//! Android started with no translator and no Google Play, and nothing but the log said so.
//!
//! Once laid over, the images are read by every program. statfs on an overlay is checked
//! against the superblock of its top lower layer, the image's, so while that superblock
//! carried the files' type every program but init was refused it, and bpfloader's refusal
//! rebooted Android 6.4 s into every boot ("avc: denied { getattr } ... scontext=u:r:bpfloader:s0
//! tcontext=u:object_r:system_lib_file:s0 tclass=filesystem"). And an overlay mounted nosuid
//! made the kernel refuse init's domain change into every service in it ("avc: denied
//! { nosuid_transition }"), vold among them.
//!
//! The service boots the image, which carries its default translator: the package must be
//! mounted and laid over `/system`, binfmt_misc mounted with the package's rules in place, and
//! the bridge library and the ARM64 ABI there. Then a small Google apps build, laid out as
//! LiteGapps publishes one, is installed through the local API from a mirror in the test's
//! folder, and Android starts again: the image must be mounted and laid over `/product` and
//! `/system_ext`, with the translator still in place. Across both boots, the guest log and the
//! Android console log must show no init command of either script failing, no denial of init's
//! mounts, no program of any domain refused filesystem getattr on an image's superblock, and no
//! service refused its domain change from a nosuid overlay. A boot that never gets far enough to
//! be checked fails with those refusals in its message.
//!
//! Runs when CI gives it what it needs: `DSA_CROSVM` names the crosvm to run,
//! `DSA_WIDGET_TEST_IMAGE` the image folder built from the commit under test (made with a
//! default ARM translation package), `DSA_WIDGET_TEST_KERNEL` the kernel to boot it with when
//! the folder carries none, and `DSA_ADB` the adb to reach the guest with (`adb` on the path
//! when not set). It needs `/dev/kvm` and `/dev/vhost-vsock`. What it read stays in its folder
//! (`/tmp/overlays-test-<pid>`) for the job to keep.
#![cfg(target_os = "linux")]

mod boot_check;

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

/// The init scripts that lay the images over the system.
const SCRIPTS: &[&str] = &["dsa-arm-translation.rc", "dsa-gapps.rc"];
/// Where the translation package is mounted, and the folders of `/system` it is laid over.
const TRANSLATION_POINT: &str = "/mnt/dsa-arm-translation";
const TRANSLATION_FOLDERS: &[&str] = &["/system/lib64", "/system/bin", "/system/etc"];
/// Where the Google apps image is mounted, and the partitions it is laid over.
const GAPPS_POINT: &str = "/mnt/dsa-gapps";
const GAPPS_PARTITIONS: &[&str] = &["/product", "/system_ext"];
/// What the package lays over `/system`: its binfmt_misc rules, which init registers, and
/// the bridge library its properties name.
const TRANSLATION_FILES: &[&str] = &[
    "/system/etc/binfmt_misc/arm64_exe",
    "/system/etc/binfmt_misc/arm64_dyn",
    "/system/lib64/libdsa_native_bridge.so",
];
/// What the small Google apps build lays over the partitions.
const GAPPS_FILES: &[&str] = &[
    "/product/priv-app/GmsCore/GmsCore.apk",
    "/product/priv-app/Phonesky/Phonesky.apk",
    "/system_ext/priv-app/GoogleServicesFramework/GoogleServicesFramework.apk",
];
/// The types of an erofs image's superblock while init mounts it and lays it over: the
/// filesystem's own (an overlay's own superblock has it too), the ones init gives the images
/// with `fscontext=`, and the file types they are labelled with, which a superblock carried
/// while the mounts named `context=` alone.
const OVERLAY_TYPES: &[&str] = &[
    "labeledfs",
    "dsa_arm_translation_fs",
    "dsa_gapps_fs",
    "system_lib_file",
    "system_file",
];
/// The API levels the mirror carries a build for, with the Android version LiteGapps names.
const MIRROR_SDKS: &[(u32, &str)] = &[(36, "16.0"), (37, "17.0")];
/// The whole test, both boots included, stays under fourteen minutes.
const BUDGET: Duration = Duration::from_secs(840);
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
        let out = run_bounded(
            Command::new(&self.program)
                .arg("-s")
                .arg(&self.serial)
                .args(args),
            ADB_LIMIT,
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
                ADB_LIMIT.as_secs()
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

/// One line of `/proc/mounts`: what is mounted, where, its type and its options.
struct Mount<'a> {
    point: &'a str,
    kind: &'a str,
    options: &'a str,
}

fn mounts(table: &str) -> Vec<Mount<'_>> {
    table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _source = fields.next()?;
            let point = fields.next()?;
            let kind = fields.next()?;
            let options = fields.next().unwrap_or("");
            Some(Mount {
                point,
                kind,
                options,
            })
        })
        .collect()
}

/// What is wrong with the mount table `table` for an erofs image mounted at `point` and laid
/// over each of `over` with overlayfs, the image's folder as the upper of the lower layers. An
/// overlay must not be nosuid: the folders it covers are not, and on a nosuid mount the kernel
/// refuses init's domain change into each service it starts from there.
fn overlay_problems(table: &str, point: &str, over: &[&str]) -> Vec<String> {
    let table = mounts(table);
    let mut problems = Vec::new();
    if !table.iter().any(|m| m.point == point && m.kind == "erofs") {
        problems.push(format!("no erofs image is mounted at {point}"));
    }
    for folder in over {
        let layer = format!("{point}{folder}");
        let laid = table.iter().find(|m| {
            m.point == *folder
                && m.kind == "overlay"
                && m.options.split(',').any(|option| {
                    option
                        .strip_prefix("lowerdir=")
                        .is_some_and(|dirs| dirs.split(':').next() == Some(layer.as_str()))
                })
        });
        match laid {
            None => problems.push(format!("{layer} is not laid over {folder}")),
            Some(m) if m.options.split(',').any(|option| option == "nosuid") => problems.push(format!(
                "{layer} is laid over {folder} nosuid, so init cannot start the services in {folder} in their own domains"
            )),
            Some(_) => {}
        }
    }
    problems
}

/// Whether binfmt_misc is mounted where the translation script registers the package's rules.
fn binfmt_mounted(table: &str) -> bool {
    mounts(table)
        .iter()
        .any(|m| m.point == "/proc/sys/fs/binfmt_misc" && m.kind == "binfmt_misc")
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
            mount || line.contains("tcontext=u:object_r:binfmt_miscfs:")
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

/// Adds what `log` (named `from` in each line) says SELinux refused programs through the
/// overlays to `problems`.
fn refusal_problems(log: &str, from: &str, problems: &mut Vec<String>) {
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

#[test]
fn fr25_a_translation_image_that_is_not_laid_over_is_found_in_the_mount_table() {
    let good = "/dev/block/dm-0 / erofs ro,seclabel,relatime 0 0\n\
                /dev/block/loop8 /mnt/dsa-arm-translation erofs ro,seclabel,nosuid,nodev,relatime 0 0\n\
                overlay /system/lib64 overlay ro,seclabel,nodev,relatime,lowerdir=/mnt/dsa-arm-translation/system/lib64:/system/lib64 0 0\n\
                overlay /system/bin overlay ro,seclabel,nodev,relatime,lowerdir=/mnt/dsa-arm-translation/system/bin:/system/bin 0 0\n\
                overlay /system/etc overlay ro,seclabel,nodev,relatime,lowerdir=/mnt/dsa-arm-translation/system/etc:/system/etc 0 0\n\
                binfmt_misc /proc/sys/fs/binfmt_misc binfmt_misc rw,nosuid,nodev,noexec,relatime 0 0\n";
    assert!(overlay_problems(good, TRANSLATION_POINT, TRANSLATION_FOLDERS).is_empty());
    assert!(binfmt_mounted(good));

    // What develop d69b9ff2 laid over: nosuid, which kept init from starting vold and apexd.
    let nosuid = good.replace(
        "ro,seclabel,nodev,relatime,lowerdir=",
        "ro,seclabel,nosuid,nodev,relatime,lowerdir=",
    );
    let problems = overlay_problems(&nosuid, TRANSLATION_POINT, TRANSLATION_FOLDERS);
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(problems.iter().all(|problem| problem.contains("nosuid")));

    // What every boot of 0.1.9 had: the image refused, so nothing was laid over.
    let refused = "/dev/block/dm-0 / erofs ro,seclabel,relatime 0 0\n";
    assert_eq!(
        overlay_problems(refused, TRANSLATION_POINT, TRANSLATION_FOLDERS).len(),
        4
    );
    assert!(!binfmt_mounted(refused));

    // A layer laid over the wrong folder does not count.
    let crossed = good.replace(
        "lowerdir=/mnt/dsa-arm-translation/system/bin:",
        "lowerdir=/mnt/dsa-arm-translation/system/etc:",
    );
    assert_eq!(
        overlay_problems(&crossed, TRANSLATION_POINT, TRANSLATION_FOLDERS).len(),
        1
    );
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

    let good = "/dev/block/loop9 /mnt/dsa-gapps erofs ro,seclabel,nosuid,nodev,relatime 0 0\n\
                overlay /product overlay ro,seclabel,nodev,relatime,lowerdir=/mnt/dsa-gapps/product:/product 0 0\n\
                overlay /system_ext overlay ro,seclabel,nodev,relatime,lowerdir=/mnt/dsa-gapps/system_ext:/system_ext 0 0\n";
    assert!(overlay_problems(good, GAPPS_POINT, GAPPS_PARTITIONS).is_empty());
    assert_eq!(overlay_problems("", GAPPS_POINT, GAPPS_PARTITIONS).len(), 3);
    let nosuid = good.replace(
        "ro,seclabel,nodev,relatime,lowerdir=",
        "ro,seclabel,nosuid,nodev,relatime,lowerdir=",
    );
    assert_eq!(
        overlay_problems(&nosuid, GAPPS_POINT, GAPPS_PARTITIONS).len(),
        2
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

/// Reads `/proc/mounts` and the log of the running boot, and adds what is wrong with the
/// translation package's mounts to `problems`.
fn translation_problems(adb: &Adb, table: &str, problems: &mut Vec<String>) {
    for problem in overlay_problems(table, TRANSLATION_POINT, TRANSLATION_FOLDERS) {
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
    match adb.shell("getprop ro.product.cpu.abilist") {
        Ok(abis) if abis.split(',').any(|abi| abi.trim() == "arm64-v8a") => {}
        Ok(abis) => problems.push(format!(
            "ARM translation: Android installs apps for {:?} only, not arm64-v8a",
            abis.trim()
        )),
        Err(why) => problems.push(why),
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
    let table = adb.shell("cat /proc/mounts").unwrap_or_default();
    let _ = std::fs::write(dir.join("mounts-first.txt"), &table);
    translation_problems(&adb, &table, &mut problems);
    match api.arm_translation() {
        Ok(status) if status.guest.as_ref().is_some_and(|guest| guest.active) => {}
        Ok(status) => problems.push(format!(
            "ARM translation: the guest says no translator is laid over its system ({:?})",
            status.guest
        )),
        Err(why) => problems.push(format!("reading where ARM translation stands: {why}")),
    }
    log_problems(&adb, &dir, "first boot", &mut problems);

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
        .saturating_sub(Duration::from_secs(120));
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

    let table = adb.shell("cat /proc/mounts").unwrap_or_default();
    let _ = std::fs::write(dir.join("mounts-second.txt"), &table);
    for problem in overlay_problems(&table, GAPPS_POINT, GAPPS_PARTITIONS) {
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
        Ok(status) if status.guest.as_ref().is_some_and(|guest| guest.active) => {}
        Ok(status) => problems.push(format!(
            "Google Play: the guest says the Google apps are not laid over its system ({:?})",
            status.guest
        )),
        Err(why) => problems.push(format!("reading where Google Play stands: {why}")),
    }
    translation_problems(&adb, &table, &mut problems);
    log_problems(&adb, &dir, "second boot", &mut problems);

    // The service's own copy of the guest log, across both boots.
    let whole = lock(&log).join("\n");
    let _ = std::fs::write(dir.join("guest.log"), &whole);
    for line in failed_commands(&whole) {
        problems.push(format!("init failed an overlay command: {line}"));
    }
    for line in mount_denials(&whole) {
        problems.push(format!("SELinux refused init: {line}"));
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
