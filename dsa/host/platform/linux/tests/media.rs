//! Sound, codecs and root approval in an ordinary app on the real image, with SELinux enforcing:
//! the service boots the image under crosvm, the test installs the media probe
//! (`dsa/guest/media-probe/`) through adb (developer mode) and runs its instrumentation, which
//! plays sound and encodes and decodes H.264 and AAC in the app's own process.
//!
//! An app runs in the untrusted app domain at an MLS level of its own, and its sound goes
//! through memory it shares with the audio server: a streaming track's control block, which
//! the audio server makes, and a static track's buffer, which the app makes. When the guest's
//! policy refused either, `AudioTrack` failed with "Could not get control block" and no app
//! played a sound, while the platform's own sounds, made at the services' level, still played.
//!
//! Every codec's input buffers are memory the framework allocates in the app's process, the
//! way the device's default codec store says to allocate it. When DSA's store (dsa-codec2)
//! answered that it knew how and then refused to say, every `MediaCodec.start()` failed with
//! NO_MEMORY, the software codecs' too, whatever SELinux's mode. The probe decodes with every
//! decoder the guest lists for each type, so a host that offers its GPU's decoder has that one
//! checked as well.
//!
//! The test serves the guest's video port itself, with a host decoder whose pictures are the
//! probe's colour bars in a padded surface ([`PatternDecoders`]), so the guest lists DSA's
//! decoders as it does on a host with a GPU decoder. The probe's video check then decodes
//! clips at 426x240 and 360x640 with those and the guest's own decoders, to buffers and to
//! surfaces on screen, and fails on green or wrong colours, on pictures that stop reaching the
//! screen when a player or a feed of short videos changes surface, and on a sound that stops.
//! The colours are checked twice: in the app's own copy of its surface, and in a capture of the
//! display, where SurfaceFlinger composes the picture from what the composer copied up to the
//! host. Only the second saw YouTube's green video: the composer had copied nothing up, while
//! the app's own graphics driver copied the picture for the first.
//!
//! The test also reads the guest's log for a denial of a media service's shared memory, for
//! the codec HAL losing a picture because its surface gave it no buffer, and for the composer
//! failing to copy a picture up to the host.
//!
//! The root certificate check makes a throwaway root and a server certificate for a test name
//! of its own ([`CERTIFICATE_HOST`]) signed by it, serves HTTPS with it on the host
//! (`openssl s_server`), and carries the guest's loopback port 8443 there with `adb reverse`.
//! The probe's HTTPS check connects to that port and checks the server under the test's name,
//! as an app does after a lookup: not under `localhost`, for which Android 17 trusts no
//! certificate at all in an app without a network security config of its own. It is refused
//! at first, then let through once the root is in the host's system store (added with
//! sudo, as an administrator adds one), refused again with the host-certificates setting off,
//! let through with it on, and refused once the root is taken away. Android's own system
//! certificates stay as they were throughout. It needs passwordless sudo, which CI's runners
//! have.
//!
//! The Widevine check asks the probe whether an app sees Widevine (`MediaDrm`): not before the
//! user installs it; at L3 once the module is taken from Google's emulator image for this
//! Android, installed as `dsa drm widevine install` installs it, and Android started again; and
//! not after it is removed and Android started again, with the device certificates it kept
//! gone. `DSA_GOOGLE_REPOSITORY` names a mirror of Google's repository instead of Google's own.
//!
//! Runs when CI gives it what it needs: `DSA_CROSVM` names the crosvm to run,
//! `DSA_WIDGET_TEST_IMAGE` the image folder built from the commit under test,
//! `DSA_WIDGET_TEST_KERNEL` the kernel to boot it with when the folder carries none,
//! `DSA_MEDIA_PROBE` the probe's APK, and `DSA_ADB` the adb to reach the guest with (`adb` on
//! the path when not set). It needs `/dev/kvm` and `/dev/vhost-vsock`. What it read stays in
//! its folder (`.scratch/media-<pid>`) for the job to keep.
#![cfg(target_os = "linux")]

mod boot_check;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boot_check::{lock, run_bounded, Process as Service, Watchdog};

use dsa_host_core::guest_log::LogFilter;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::session::SessionState;
use dsa_host_core::video::{
    Codec, CodecSupport, Packet, Picture, VideoDecoder, VideoDecoders, VIDEO_PORT,
};
use dsa_host_core::video_layout::SurfaceFormat;
use dsa_host_linux::devices::relay::{device_socket, DeviceSocketListener};
use dsa_host_unix::socket::UnixLocalConnector;

const PROBE_PACKAGE: &str = "io.github.thisisthepy.dsa.mediaprobe";
const PROBE_RUNNER: &str = "io.github.thisisthepy.dsa.mediaprobe/.MediaProbe";
/// `AudioTrack.STATE_INITIALIZED`.
const TRACK_INITIALIZED: i64 = 1;
/// The tone the probe plays: a second at 48 kHz.
const TONE_FRAMES: i64 = 48_000;
/// The tone's amplitude, out of 32768 (`MediaProbe.tone`).
const TONE_PEAK: u32 = 8000;
/// How long the check waits for the probe's sound to go idle on the host, which is when the
/// host logs how loud it arrived: the guest's mixer stands by a few seconds after the last
/// sound.
const LEVEL_WAIT: Duration = Duration::from_secs(30);
/// The guest's own decoders, which every image has.
const SOFTWARE_DECODERS: &[(&str, &str)] = &[
    ("avc", "c2.android.avc.decoder"),
    ("aac", "c2.android.aac.decoder"),
];
/// The memfd types of the media services: a denial naming one means an app or a service could
/// not use memory another process shared with it.
const SHARED_MEMORY_TYPES: &[&str] = &[
    "audioserver_tmpfs",
    "mediaserver_tmpfs",
    "mediaextractor_tmpfs",
    "cameraserver_tmpfs",
    "appdomain_tmpfs",
    // The sensors HAL's event queue, which system_server makes.
    "system_server_tmpfs",
    // The codec services' buffer pool queues.
    "dsa_codec2_tmpfs",
];
/// The media check, boot included, stays under fifteen minutes: the video check decodes
/// four clips with each decoder three ways.
const MEDIA_BUDGET: Duration = Duration::from_secs(900);
/// The root check boots three times (as it starts, then with root elevation on and off), so
/// it stays under twenty-one minutes.
const ROOT_BUDGET: Duration = Duration::from_secs(1260);
/// The root certificate check boots once and waits on five changes of the host's trust, so it
/// stays under fifteen minutes.
const CERTIFICATES_BUDGET: Duration = Duration::from_secs(900);
/// How long past its budget a check may run before it stops everything it started and ends:
/// inside its job step's limit (18 minutes for media, 23 for root), so the step never kills
/// it with its VM still running.
const OVERRUN: Duration = Duration::from_secs(60);
/// How long one adb command may take. The probe's longest check, the video check, encodes
/// four clips and decodes each with every decoder to buffers, to a surface and as a feed; a
/// log dump of the whole boot takes seconds.
const ADB_LIMIT: Duration = Duration::from_secs(420);

fn scratch(kind: &str) -> PathBuf {
    // Short: the service's socket goes inside, and a socket's path has at most 107 bytes.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest
        .ancestors()
        .nth(4)
        .unwrap()
        .join(".scratch")
        .join(format!("{kind}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Fails the test with what the service printed and the guest log's last lines.
fn fail(why: String, service: &Service, log: &Mutex<Vec<String>>) -> ! {
    // The lock is let go before the panic, so the thread that reads the guest log is not
    // left with a poisoned lock and does not panic on top of this failure.
    let tail = {
        let log = lock(log);
        let _ = std::fs::write(service.log.with_file_name("guest.log"), log.join("\n"));
        log[log.len().saturating_sub(200)..].join("\n")
    };
    let devices =
        std::fs::read_to_string(service.log.with_file_name("devices.log")).unwrap_or_default();
    panic!(
        "{why}\n--- the service's output ---\n{}\n--- the devices' output ---\n{devices}\n--- the guest log's last lines ---\n{tail}",
        service.log(),
    )
}

fn client(socket: &Path, wait: Duration) -> HostApiClient {
    let mut connector = UnixLocalConnector {
        path: socket.to_path_buf(),
    };
    let deadline = Instant::now() + wait;
    loop {
        match HostApiClient::connect(&mut connector, "media-test", Duration::from_secs(30)) {
            Ok(client) => return client,
            Err(why) if Instant::now() >= deadline => {
                panic!("the service did not answer on {}: {why}", socket.display())
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// adb, talking to the guest through the service's developer mode port.
struct Adb {
    program: PathBuf,
    serial: String,
}

impl Adb {
    /// Runs adb with `args` against the guest, for [ADB_LIMIT] at most: a command that
    /// hangs fails the check with what it was, instead of holding it until the job's timeout.
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

    /// Runs one of the probe's checks and returns its results, keeping its output in `dir`.
    fn probe(&self, dir: &Path, check: &str) -> Result<BTreeMap<String, String>, String> {
        let out = self.shell(&format!(
            "am instrument -w -r -e check {check} {PROBE_RUNNER}"
        ))?;
        let _ = std::fs::write(dir.join(format!("probe-{check}.txt")), &out);
        instrumentation_results(&out)
    }
}

/// The results an instrumentation finished with, from `am instrument -r`'s output: each
/// `INSTRUMENTATION_RESULT: key=value` line. An instrumentation that did not finish (its
/// process died, or it never started) has no `INSTRUMENTATION_CODE: -1` line.
fn instrumentation_results(out: &str) -> Result<BTreeMap<String, String>, String> {
    let mut results = BTreeMap::new();
    let mut finished = false;
    for line in out.lines() {
        let line = line.trim_end();
        if let Some(pair) = line.strip_prefix("INSTRUMENTATION_RESULT: ") {
            if let Some((key, value)) = pair.split_once('=') {
                results.insert(key.to_string(), value.to_string());
            }
        } else if line == "INSTRUMENTATION_CODE: -1" {
            finished = true;
        }
    }
    if !finished {
        return Err(format!("the probe did not finish:\n{out}"));
    }
    if let Some(error) = results.get("error") {
        return Err(format!("the probe failed: {error}"));
    }
    Ok(results)
}

/// The lines of `log` that record an SELinux denial naming one of `types`.
/// The agent's package, whose speech and spelling services the image makes the defaults.
const AGENT: &str = "io.github.thisisthepy.dsa.agent";

/// The secure settings that name the guest's text-to-speech engine, speech recognition
/// service and spell checker, the services' intent actions, and the component each must be.
const LANGUAGE_SERVICES: [(&str, &str, &str); 3] = [
    (
        "tts_default_synth",
        "android.intent.action.TTS_SERVICE",
        ".platform.HostTextToSpeechService",
    ),
    (
        "voice_recognition_service",
        "android.speech.RecognitionService",
        ".platform.HostRecognitionService",
    ),
    (
        "selected_spell_checker",
        "android.service.textservice.SpellCheckerService",
        ".platform.HostSpellCheckerService",
    ),
];

/// Whether `value` names the agent's component `class` (".platform.Name"), in the short or
/// the full form; for the engine, whose setting names only a package, the agent's package.
fn names_agent(value: &str, class: &str) -> bool {
    let value = value.trim();
    if value == AGENT {
        return class.ends_with("TextToSpeechService");
    }
    value == format!("{AGENT}/{class}") || value == format!("{AGENT}/{AGENT}{class}")
}

/// What is wrong with the guest's speech and spelling defaults: each setting with its value
/// (`settings get secure`), and for each service action the components that answer it
/// (`cmd package query-services --brief`). The agent's must be the defaults and the only ones,
/// and spell checking must be on.
fn language_service_problems(
    settings: &BTreeMap<String, String>,
    services: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (key, action, class) in LANGUAGE_SERVICES {
        let value = settings.get(key).map(String::as_str).unwrap_or("");
        if !names_agent(value, class) {
            problems.push(format!("{key} is {value:?}, not the agent's {class}"));
        }
        let listed = services.get(action).map(String::as_str).unwrap_or("");
        let others: Vec<&str> = listed
            .lines()
            .map(str::trim)
            .filter(|line| line.contains('/') && !line.starts_with(&format!("{AGENT}/")))
            .collect();
        if !others.is_empty() {
            problems.push(format!(
                "the image has another {action}: {}",
                others.join(", ")
            ));
        }
        if !listed.lines().any(|line| names_agent(line, class)) {
            problems.push(format!(
                "the agent's {class} does not answer {action}: {listed:?}"
            ));
        }
    }
    let enabled = settings
        .get("spell_checker_enabled")
        .map(|v| v.trim())
        .unwrap_or("");
    if enabled != "1" {
        problems.push(format!("spell_checker_enabled is {enabled:?}, not 1"));
    }
    problems
}

fn denials<'a>(log: &'a str, types: &[&str]) -> Vec<&'a str> {
    log.lines()
        .filter(|line| line.contains("avc:") && line.contains("denied"))
        .filter(|line| types.iter().any(|kind| line.contains(&format!(":{kind}:"))))
        .collect()
}

/// A number the probe reported, or why it is not there.
fn number(results: &BTreeMap<String, String>, key: &str) -> Result<i64, String> {
    let value = results.get(key).ok_or_else(|| {
        let error = results
            .get(
                &key.rsplit_once('.')
                    .map(|(head, _)| format!("{head}.error"))
                    .unwrap_or_default(),
            )
            .cloned()
            .unwrap_or_else(|| "no error was reported".into());
        format!("the probe reported no {key} ({error})")
    })?;
    value
        .parse()
        .map_err(|_| format!("the probe reported {key}={value}, which is not a number"))
}

fn root_absent_problems(results: &BTreeMap<String, String>) -> Vec<String> {
    let mut problems = Vec::new();
    for path in ["/system/bin/su", "/system/xbin/su", "/sbin/su"] {
        for operation in ["stat", "procstat", "access", "exec"] {
            let key = format!("root.{path}.{operation}");
            if results.get(&key).map(String::as_str) != Some("ENOENT") {
                problems.push(format!(
                    "{key} did not report ENOENT: {:?}",
                    results.get(&key)
                ));
            }
        }
    }
    if results.get("root.path_lookup_exit").map(String::as_str) != Some("1") {
        problems.push(format!(
            "PATH found su: {:?}",
            results.get("root.path_lookup_exit")
        ));
    }
    problems
}

fn set_machine_switch(dir: &Path, enabled: bool) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let output = Command::new("sudo")
        .arg("env")
        .arg(format!(
            "DSA_ROOT_TEST_SOCKET={}",
            dir.join("host.sock").display()
        ))
        .arg(format!("DSA_ROOT_TEST_ENABLED={}", u8::from(enabled)))
        .arg(executable)
        .args(["--exact", "fr35_set_machine_switch_as_admin", "--nocapture"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "setting root elevation: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn restart_for_root(
    api: &mut HostApiClient,
    adb: &Adb,
    dir: &Path,
    enabled: bool,
) -> Result<(), String> {
    api.stop().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if api.status().map_err(|e| e.to_string())?.state == SessionState::Stopped {
            break;
        }
        if Instant::now() >= deadline {
            return Err("Android did not stop".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    set_machine_switch(dir, enabled)?;
    if enabled {
        let result = api
            .set_app_root(PROBE_PACKAGE, "ask")
            .map_err(|e| e.to_string())?;
        if result.level != "ask" {
            return Err(format!(
                "setting the probe's root level: {}",
                result.message
            ));
        }
    }
    api.ensure_running(Duration::from_secs(360), Duration::from_secs(2), |_| {})
        .map_err(|e| e.to_string())?;
    adb.connect(Duration::from_secs(120))
}

#[test]
fn fr35_set_machine_switch_as_admin() {
    let Some(socket) = std::env::var_os("DSA_ROOT_TEST_SOCKET") else {
        return;
    };
    let enabled = std::env::var("DSA_ROOT_TEST_ENABLED").unwrap() == "1";
    let mut api = client(Path::new(&socket), Duration::from_secs(10));
    let mut settings = api.settings().unwrap().settings;
    settings.root_elevation = enabled;
    api.set_settings(&settings).unwrap();
}

fn root_approval_check(api: &mut HostApiClient, adb: &Adb, dir: &Path) -> Result<(), String> {
    let approver = dir.join("approver");
    std::fs::write(approver.join("answer"), "deny").map_err(|e| e.to_string())?;
    restart_for_root(api, adb, dir, true)?;
    let visible_until = Instant::now() + Duration::from_secs(90);
    loop {
        let results = adb.probe(dir, "root_visible")?;
        if results.get("root.visible").map(String::as_str) == Some("yes") {
            break;
        }
        if Instant::now() >= visible_until {
            return Err(format!("the allowed app cannot see su: {results:?}"));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let requests = approver.join("requests.jsonl");
    let count = || -> Result<usize, String> {
        std::fs::read_to_string(&requests)
            .map(|text| text.lines().count())
            .map_err(|e| e.to_string())
    };
    let denied = adb.probe(dir, "root_request")?;
    if number(&denied, "root.exit")? == 0 || count()? != 1 {
        return Err(format!("a refused root call did not fail once: {denied:?}"));
    }
    let records = std::fs::read_to_string(&requests).map_err(|e| e.to_string())?;
    let first: serde_json::Value =
        serde_json::from_str(records.lines().next().unwrap_or("")).map_err(|e| e.to_string())?;
    if first["package"] != PROBE_PACKAGE
        || first["app_name"] != "DSA media probe"
        || first["command"][0] != "id"
        || first["uid"].as_u64().unwrap_or(0) < 10_000
        || first["icon_bytes"].as_u64().unwrap_or(0) == 0
    {
        return Err(format!(
            "the approval prompt did not identify the calling app and command: {first}"
        ));
    }
    std::fs::remove_file(approver.join("answer")).map_err(|e| e.to_string())?;
    let waited = Instant::now();
    let unanswered = adb.probe(dir, "root_request")?;
    if number(&unanswered, "root.exit")? == 0
        || count()? != 2
        || waited.elapsed() < Duration::from_secs(55)
    {
        return Err(format!(
            "an unanswered root call did not time out: {unanswered:?}"
        ));
    }
    // Let the host's own deadline finish before changing the shared answer file.
    std::thread::sleep(Duration::from_secs(3));
    std::fs::write(approver.join("answer"), "approve").map_err(|e| e.to_string())?;
    let approved = adb.probe(dir, "root_request")?;
    if number(&approved, "root.exit")? != 0
        || !approved
            .get("root.output")
            .is_some_and(|out| out.contains("uid=0"))
        || count()? != 3
    {
        return Err(format!(
            "the approved command did not run as root: {approved:?}"
        ));
    }
    let grace = adb.probe(dir, "root_request")?;
    if number(&grace, "root.exit")? != 0 || count()? != 3 {
        return Err(format!(
            "the app's second root call did not use its grace: {grace:?}"
        ));
    }
    let stdin_run = adb.probe(dir, "root_stdin")?;
    if number(&stdin_run, "root.exit")? != 0
        || !stdin_run
            .get("root.output")
            .is_some_and(|out| out.contains("uid=0"))
    {
        return Err(format!(
            "su with a command on standard input did not run as root: {stdin_run:?}"
        ));
    }
    let adb_root = adb.shell("su -c id")?;
    if !adb_root.contains("uid=0") || count()? != 4 {
        return Err(format!(
            "adb shell su -c id did not ask the host and run as root: {adb_root:?}"
        ));
    }
    let records = std::fs::read_to_string(&requests).map_err(|e| e.to_string())?;
    let adb_record: serde_json::Value =
        serde_json::from_str(records.lines().nth(3).unwrap_or("")).map_err(|e| e.to_string())?;
    if adb_record["package"] != "com.android.shell" || adb_record["app_name"] != "adb shell" {
        return Err(format!(
            "the approval prompt did not identify adb shell: {adb_record}"
        ));
    }
    let disallowed = api
        .set_app_root(PROBE_PACKAGE, "off")
        .map_err(|e| e.to_string())?;
    if disallowed.level != "off" {
        return Err(format!(
            "setting the probe's root level to off: {}",
            disallowed.message
        ));
    }
    let per_app_off = adb.probe(dir, "root_absent")?;
    let problems = root_absent_problems(&per_app_off);
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    let restored = api
        .set_app_root(PROBE_PACKAGE, "ask")
        .map_err(|e| e.to_string())?;
    if restored.level != "ask" {
        return Err(format!(
            "restoring the probe's root level to ask: {}",
            restored.message
        ));
    }
    let visible = adb.probe(dir, "root_visible")?;
    if visible.get("root.visible").map(String::as_str) != Some("yes") {
        return Err(format!("the re-enabled app cannot see su: {visible:?}"));
    }
    set_machine_switch(dir, false)?;
    let off = adb.probe(dir, "root_absent")?;
    let problems = root_absent_problems(&off);
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    let stop_until = Instant::now() + Duration::from_secs(5);
    loop {
        let helper = adb.shell("getprop init.svc.dsa_su")?;
        if matches!(helper.trim(), "" | "stopped") {
            break;
        }
        if Instant::now() >= stop_until {
            return Err(format!(
                "the helper still runs after root elevation is off: {helper}"
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

#[test]
fn fr35_the_boot_check_rejects_a_visible_su() {
    let mut results = BTreeMap::new();
    for path in ["/system/bin/su", "/system/xbin/su", "/sbin/su"] {
        for operation in ["stat", "procstat", "access", "exec"] {
            results.insert(format!("root.{path}.{operation}"), "ENOENT".into());
        }
    }
    results.insert("root.path_lookup_exit".into(), "1".into());
    assert!(root_absent_problems(&results).is_empty());
    results.insert("root./system/bin/su.stat".into(), "visible".into());
    assert_eq!(root_absent_problems(&results).len(), 1);
}

/// What is wrong with the probe's audio results: each track must initialise, take the whole
/// tone and play at least half of it.
fn audio_problems(results: &BTreeMap<String, String>) -> Vec<String> {
    let mut problems = Vec::new();
    for track in ["stream", "static"] {
        if let Some(error) = results.get(&format!("audio.{track}.error")) {
            problems.push(format!("the {track} track failed: {error}"));
            continue;
        }
        match number(results, &format!("audio.{track}.state")) {
            Ok(TRACK_INITIALIZED) => {}
            Ok(state) => problems.push(format!(
                "the {track} track's state is {state}, not initialised"
            )),
            Err(why) => problems.push(why),
        }
        match number(results, &format!("audio.{track}.written")) {
            Ok(TONE_FRAMES) => {}
            Ok(written) => problems.push(format!(
                "the {track} track took {written} of the tone's {TONE_FRAMES} frames"
            )),
            Err(why) => problems.push(why),
        }
        match number(results, &format!("audio.{track}.played")) {
            Ok(played) if played >= TONE_FRAMES / 2 => {}
            Ok(played) => problems.push(format!(
                "the {track} track played {played} frames in three seconds, not half the tone"
            )),
            Err(why) => problems.push(why),
        }
    }
    problems
}

/// The loudest peak an output of the guest's sound reached the host at, as the audio port logs
/// it when the output goes idle or closes ("audio stream primary is idle: 96000 frames
/// received, peak 8000").
fn loudest_peak(log: &str) -> Option<u32> {
    log.lines()
        .filter(|line| line.contains("audio stream "))
        .filter_map(|line| {
            let after = &line[line.find(" peak ")? + " peak ".len()..];
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .max()
}

/// What is wrong with how loud the probe's tone reached the host. Android's media volume is
/// left at its default, and the tone must still arrive as the probe made it, within a tenth:
/// the host's volume is the only one the guest's sound has. Before, the guest's mixer applied
/// Android's media volume first, and the sound left the guest some 15 dB down.
fn sound_level_problems(log: &str) -> Vec<String> {
    match loudest_peak(log) {
        None => vec!["no output of the guest's sound said how loud it reached the host".into()],
        Some(peak) if peak * 10 < TONE_PEAK * 9 => vec![format!(
            "the probe's tone reached the host at a peak of {peak}, not its {TONE_PEAK}: the \
             guest turned it down before the host's volume"
        )],
        Some(_) => Vec::new(),
    }
}

/// What is wrong with the probe's decode results: each stream must be encoded, the guest's
/// software decoder must be among the decoders, and every decoder must start and give back at
/// least half of what went in (pictures for H.264, sample frames for AAC).
fn decode_problems(results: &BTreeMap<String, String>) -> Vec<String> {
    let mut problems = Vec::new();
    for (stream, software) in SOFTWARE_DECODERS {
        if let Some(error) = results.get(&format!("encode.{stream}.error")) {
            problems.push(format!("the {stream} encoder failed: {error}"));
            continue;
        }
        let wanted = if *stream == "avc" {
            match number(results, "encode.avc.units") {
                Ok(units) if units > 0 => (units + 1) / 2,
                Ok(_) => {
                    problems.push("the H.264 encoder gave no pictures".into());
                    continue;
                }
                Err(why) => {
                    problems.push(why);
                    continue;
                }
            }
        } else {
            TONE_FRAMES / 2
        };
        let Some(decoders) = results.get(&format!("decode.{stream}.decoders")) else {
            problems.push(format!("the probe decoded no {stream} stream"));
            continue;
        };
        let decoders: Vec<&str> = decoders
            .split(',')
            .filter(|name| !name.is_empty())
            .collect();
        if !decoders.contains(software) {
            problems.push(format!(
                "the guest lists no {software} among its decoders ({decoders:?})"
            ));
        }
        for decoder in decoders {
            if let Some(error) = results.get(&format!("decode.{decoder}.error")) {
                problems.push(format!("{decoder} failed: {error}"));
                continue;
            }
            match number(results, &format!("decode.{decoder}.out")) {
                Ok(out) if out >= wanted => {}
                Ok(out) => problems.push(format!(
                    "{decoder} gave back {out} of the {wanted} it should have"
                )),
                Err(why) => problems.push(why),
            }
        }
    }
    problems
}

/// The bars the probe's video clips show, left to right, as Y, Cb, Cr: grey, red, yellow,
/// blue. The probe (`VideoCheck.BARS`) encodes the same ones.
const BARS: [[u8; 3]; 4] = [
    [180, 128, 128],
    [81, 90, 240],
    [210, 16, 146],
    [41, 240, 110],
];

/// The probe's video clips, and the guest's own decoder for each format.
const CLIPS: &[(&str, &str)] = &[
    ("avc_426x240", "c2.android.avc.decoder"),
    ("avc_360x640", "c2.android.avc.decoder"),
    ("vp9_426x240", "c2.android.vp9.decoder"),
    ("vp9_360x640", "c2.android.vp9.decoder"),
];

/// The host decoder each clip's format has here: the boot check's own (see
/// [`PatternDecoders`]), which the guest lists as `c2.dsa.<format>.decoder`.
fn host_decoder(clip: &str) -> &'static str {
    if clip.starts_with("vp9") {
        "c2.dsa.vp9.decoder"
    } else {
        "c2.dsa.avc.decoder"
    }
}

/// The boot check's host decoder: CI's host has no GPU decoder, so this stands in for one on
/// the video port. Whatever a packet holds, its picture is the probe's colour bars at the
/// size the guest opened the stream with, put in a surface padded the way GPU decoders pad
/// theirs (the coded size rounded up to 16, the surface taller still, its padding zero and
/// its chroma plane after the surface's rows) and copied out with the rules every host's
/// decoder uses. So the check covers the whole path a host decoder's picture takes: the plane
/// offset and visible size on the host, the video port, and the guest's buffer, crop and
/// surface.
struct PatternDecoders;

struct PatternDecoder {
    surface: Vec<u8>,
    pitch: usize,
    chroma_row: usize,
    width: u32,
    height: u32,
}

/// The bars at `width` by `height` in a padded NV12 surface: its pitch and the row its chroma
/// plane starts at.
fn pattern_surface(width: u32, height: u32) -> (Vec<u8>, usize, usize) {
    let (w, h) = (width as usize, height as usize);
    let coded = (w.div_ceil(16) * 16, h.div_ceil(16) * 16);
    let pitch = coded.0.div_ceil(64) * 64 + 64;
    // A driver's surface for the largest picture it expects: taller than the coded size.
    let rows = coded.1.div_ceil(64) * 64 + 64;
    let chroma_row = dsa_host_core::video_layout::chroma_start_row(Some(rows), coded.1, h);
    let mut surface = vec![0u8; pitch * (rows + rows.div_ceil(2))];
    let bar = |x: usize| (x * 4 / w).min(3);
    for y in 0..h {
        for x in 0..w {
            surface[y * pitch + x] = BARS[bar(x)][0];
        }
    }
    for y in 0..h.div_ceil(2) {
        for x in 0..w.div_ceil(2) {
            let at = (chroma_row + y) * pitch + 2 * x;
            surface[at] = BARS[bar(2 * x)][1];
            surface[at + 1] = BARS[bar(2 * x)][2];
        }
    }
    (surface, pitch, chroma_row)
}

impl VideoDecoders for PatternDecoders {
    fn capabilities(&self) -> Vec<CodecSupport> {
        [Codec::H264, Codec::Vp9]
            .into_iter()
            .map(|codec| CodecSupport {
                codec,
                max_width: 4096,
                max_height: 2304,
            })
            .collect()
    }

    fn open(&self, _: Codec, width: u32, height: u32) -> Result<Box<dyn VideoDecoder>, String> {
        if width == 0 || height == 0 {
            return Err(format!("the guest opened a stream of {width}x{height}"));
        }
        let (surface, pitch, chroma_row) = pattern_surface(width, height);
        Ok(Box::new(PatternDecoder {
            surface,
            pitch,
            chroma_row,
            width,
            height,
        }))
    }
}

impl VideoDecoder for PatternDecoder {
    fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String> {
        if packet.codec_config {
            return Ok(Vec::new());
        }
        dsa_host_core::video_layout::picture_from_surface(
            packet.id,
            &self.surface,
            SurfaceFormat::Nv12,
            self.pitch,
            self.chroma_row,
            self.width,
            self.height,
        )
        .map(|picture| vec![picture])
        .ok_or_else(|| "the pattern does not fit its surface".into())
    }

    fn drain(&mut self) -> Result<Vec<Picture>, String> {
        Ok(Vec::new())
    }

    fn flush(&mut self) {}
}

/// Serves the video port with [`PatternDecoders`] on the socket the service relays the guest's
/// video port to, until `stop`. Its log lines go to `log`.
fn serve_pattern_video(
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<String>>>,
) -> Result<std::thread::JoinHandle<()>, String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .ok_or("XDG_RUNTIME_DIR names no runtime folder for the video port's socket")?;
    let path = device_socket(&runtime, VIDEO_PORT);
    let listener = DeviceSocketListener::bind(&path)
        .map_err(|e| format!("{} cannot be opened: {e}", path.display()))?;
    let decoders: Arc<dyn VideoDecoders> = Arc::new(PatternDecoders);
    let line: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |text: &str| {
        lock(&log).push(text.to_string());
    });
    std::thread::Builder::new()
        .name("pattern-video".into())
        .spawn(move || {
            dsa_host_core::video::serve(
                Box::new(listener),
                decoders,
                "media-test pattern-video".into(),
                stop,
                line,
            )
        })
        .map_err(|e| format!("the video port's thread did not start: {e}"))
}

/// The guest's codec HAL logging that a picture had no buffer or did not fit one.
fn codec_buffer_errors(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("dsa-codec2"))
        .filter(|line| {
            line.contains("gave no")
                || line.contains("did not fit")
                || line.contains("cannot write")
        })
        .collect()
}

/// The guest's composer logging that it could not copy what a CPU wrote into a buffer up to
/// the host: a decoder's pictures then reach the screen as the ColorBuffer's untouched memory,
/// which a YUV layer shows as green.
fn copy_up_errors(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains("dsa-hwcomposer"))
        .filter(|line| line.contains("what the CPU wrote") || line.contains("what a CPU wrote"))
        .filter(|line| {
            line.contains("cannot import")
                || line.contains("cannot read")
                || line.contains("did not take")
        })
        .collect()
}

/// The first Codec 2.0 component name (`c2.<vendor>.<name>`) a log line mentions.
fn component_in(line: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(at) = line[from..].find("c2.") {
        let start = from + at;
        let before = line[..start].chars().next_back();
        let name: &str = {
            let rest = &line[start..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
                .unwrap_or(rest.len());
            rest[..end].trim_end_matches('.')
        };
        if !before.is_some_and(|c| c.is_ascii_alphanumeric()) && name.matches('.').count() >= 2 {
            return Some(name);
        }
        from = start + 3;
    }
    None
}

/// The framework's "config failed => CORRUPTED" lines for one of DSA's decoders: a configure
/// the component's interface refused as corrupted rather than as a bad value or an unknown
/// parameter. The line names no component, so each is put down to the last component its
/// process named before it (the probe and an app create their decoders one at a time). The
/// platform's own decoders answer the same for a surface's pixel format, so theirs are not
/// counted.
fn dsa_config_corrupted(log: &str) -> Vec<&str> {
    let mut last: BTreeMap<&str, &str> = BTreeMap::new();
    let mut found = Vec::new();
    for line in log.lines() {
        // threadtime: date, time, pid, tid, level, tag: message
        let Some(pid) = line.split_whitespace().nth(2) else {
            continue;
        };
        if line.contains("config failed => CORRUPTED") {
            if last
                .get(pid)
                .is_some_and(|name| name.starts_with("c2.dsa."))
            {
                found.push(line);
            }
            continue;
        }
        if let Some(name) = component_in(line) {
            last.insert(pid, name);
        }
    }
    found
}

/// What is wrong with the probe's video results (see `VideoCheck`): each clip must be encoded
/// and decoded by the guest's own decoder and the host's; every decoder's pictures must keep
/// the bars' colours to the edges, in buffers and on screen; pictures must reach both
/// surfaces of a player that moves between them, while its sound plays; and a feed's second
/// clip must show its first picture within seconds.
fn video_problems(results: &BTreeMap<String, String>) -> Vec<String> {
    let mut problems = Vec::new();
    for (clip, software) in CLIPS {
        let key = format!("video.{clip}");
        if let Some(error) = results.get(&format!("{key}.encode_error")) {
            problems.push(format!("{clip}: the encoder failed: {error}"));
            continue;
        }
        let units = match number(results, &format!("{key}.units")) {
            Ok(units) if units > 0 => units,
            Ok(_) => {
                problems.push(format!("{clip}: the encoder gave no pictures"));
                continue;
            }
            Err(why) => {
                problems.push(why);
                continue;
            }
        };
        let Some(decoders) = results.get(&format!("{key}.decoders")) else {
            problems.push(format!("{clip}: the probe decoded nothing"));
            continue;
        };
        let decoders: Vec<&str> = decoders.split(',').filter(|n| !n.is_empty()).collect();
        for wanted in [*software, host_decoder(clip)] {
            if !decoders.contains(&wanted) {
                problems.push(format!(
                    "{clip}: the guest lists no {wanted} among its decoders ({decoders:?})"
                ));
            }
        }
        for decoder in decoders {
            let at = format!("{key}.{decoder}");
            let mut check = |what: &str, found: Result<(), String>| {
                if let Err(why) = found {
                    problems.push(format!("{clip}, {decoder}, {what}: {why}"));
                }
            };
            for way in ["buffer", "surface", "shorts"] {
                if let Some(error) = results.get(&format!("{at}.{way}.error")) {
                    check(way, Err(format!("failed: {error}")));
                    continue;
                }
                let colour = results
                    .get(&format!("{at}.{way}.colour"))
                    .map(String::as_str);
                if colour != Some("ok") {
                    check(
                        way,
                        Err(format!("colours: {}", colour.unwrap_or("not reported"))),
                    );
                }
                // What the display shows, as SurfaceFlinger composes it. The copy of the
                // surface above is read in the probe's own process, which copies the decoder's
                // picture up to the host itself, so it stays right even when the composer's
                // copy fails and the screen is green.
                if way != "buffer" {
                    let screen = results
                        .get(&format!("{at}.{way}.screen"))
                        .map(String::as_str);
                    if screen != Some("ok") {
                        check(
                            way,
                            Err(format!("on screen: {}", screen.unwrap_or("not reported"))),
                        );
                    }
                }
            }
            let at_least = |key: &str, wanted: i64| -> Result<(), String> {
                match number(results, key) {
                    Ok(n) if n >= wanted => Ok(()),
                    Ok(n) => Err(format!("{key} is {n}, below {wanted}")),
                    Err(why) => Err(why),
                }
            };
            if !results.contains_key(&format!("{at}.buffer.error")) {
                check(
                    "buffer",
                    at_least(&format!("{at}.buffer.out"), (units + 1) / 2),
                );
            }
            if !results.contains_key(&format!("{at}.surface.error")) {
                check(
                    "surface",
                    at_least(&format!("{at}.surface.rendered"), (units + 1) / 2),
                );
                // Pictures reached the screen on the first surface and kept coming on the
                // second: a decoder whose surface gives it no buffer shows none.
                check("surface", at_least(&format!("{at}.surface.presented"), 2));
                check("surface", at_least(&format!("{at}.surface.after_swap"), 1));
                check(
                    "surface",
                    at_least(&format!("{at}.surface.audio"), TONE_FRAMES / 4),
                );
            }
            if !results.contains_key(&format!("{at}.shorts.error")) {
                check("shorts", at_least(&format!("{at}.shorts.first"), 1));
                check("shorts", at_least(&format!("{at}.shorts.second"), 1));
                match number(results, &format!("{at}.shorts.first_frame_ms")) {
                    Ok(ms) if (0..=3000).contains(&ms) => {}
                    Ok(ms) => check(
                        "shorts",
                        Err(format!("the second clip's first picture took {ms} ms")),
                    ),
                    Err(why) => check("shorts", Err(why)),
                }
            }
        }
    }
    problems
}

#[test]
fn fr26_the_pattern_decoders_pictures_are_the_bars_out_of_a_padded_surface() {
    for (width, height) in [(426u32, 240u32), (360, 640), (425, 239)] {
        let mut decoder = PatternDecoders.open(Codec::H264, width, height).unwrap();
        let pictures = decoder
            .decode(Packet {
                id: 7,
                data: vec![0, 0, 0, 1, 0x65],
                codec_config: false,
                timestamp_us: 0,
            })
            .unwrap();
        assert_eq!(pictures.len(), 1);
        let picture = &pictures[0];
        assert_eq!(
            (picture.id, picture.width, picture.height),
            (7, width, height)
        );
        let stride = picture.stride as usize;
        let (w, h) = (width as usize, height as usize);
        // The right edge and the bottom row hold the last bar, chroma included: nothing of
        // the zero padding comes along.
        assert_eq!(picture.nv12[(h - 1) * stride + w - 1], BARS[3][0]);
        let chroma = &picture.nv12[stride * h..];
        let last = (h.div_ceil(2) - 1) * stride + 2 * (w.div_ceil(2) - 1);
        assert_eq!(&chroma[last..last + 2], &BARS[3][1..]);
        assert!(chroma.chunks(2).all(|pair| pair != [0, 0]), "chroma lost");
    }
    let mut decoder = PatternDecoders.open(Codec::Vp9, 426, 240).unwrap();
    let config = Packet {
        id: 1,
        data: vec![1],
        codec_config: true,
        timestamp_us: 0,
    };
    assert!(decoder.decode(config).unwrap().is_empty());
}

fn video_results() -> BTreeMap<String, String> {
    let mut results = BTreeMap::new();
    for (clip, software) in CLIPS {
        let key = format!("video.{clip}");
        results.insert(format!("{key}.units"), "30".into());
        results.insert(
            format!("{key}.decoders"),
            format!("{},{software}", host_decoder(clip)),
        );
        for decoder in [host_decoder(clip), *software] {
            let at = format!("{key}.{decoder}");
            for (k, v) in [
                ("buffer.out", "30"),
                ("buffer.colour", "ok"),
                ("surface.rendered", "30"),
                ("surface.presented", "30"),
                ("surface.after_swap", "15"),
                ("surface.colour", "ok"),
                ("surface.screen", "ok"),
                ("surface.audio", "48000"),
                ("shorts.first", "30"),
                ("shorts.second", "30"),
                ("shorts.first_frame_ms", "40"),
                ("shorts.colour", "ok"),
                ("shorts.screen", "ok"),
            ] {
                results.insert(format!("{at}.{k}"), v.to_string());
            }
        }
    }
    results
}

#[test]
fn fr26_green_pictures_and_a_stalled_surface_fail_the_video_check() {
    let good = video_results();
    assert!(
        video_problems(&good).is_empty(),
        "{:?}",
        video_problems(&good)
    );
    let at = "video.vp9_426x240.c2.dsa.vp9.decoder";

    // A picture whose chroma was lost.
    let mut green = good.clone();
    green.insert(
        format!("{at}.buffer.colour"),
        "no chroma (green) at 425,120: Y41 Cb0 Cr0".into(),
    );
    let problems = video_problems(&green);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("green"));

    // What YouTube showed: the surface gave the decoder no buffer, so nothing reached the
    // screen, and a feed never started its second clip.
    let mut stalled = good.clone();
    stalled.insert(format!("{at}.surface.presented"), "0".into());
    stalled.insert(format!("{at}.surface.after_swap"), "0".into());
    stalled.insert(format!("{at}.shorts.second"), "0".into());
    stalled.insert(format!("{at}.shorts.first_frame_ms"), "-1".into());
    assert_eq!(
        video_problems(&stalled).len(),
        4,
        "{:?}",
        video_problems(&stalled)
    );

    // The sound stopped while the video played.
    let mut silent = good.clone();
    silent.insert(format!("{at}.surface.audio"), "0".into());
    assert_eq!(video_problems(&silent).len(), 1);

    // A host whose decoder the guest does not list.
    let mut unlisted = good.clone();
    unlisted.insert(
        "video.avc_360x640.decoders".into(),
        "c2.android.avc.decoder".into(),
    );
    assert_eq!(video_problems(&unlisted).len(), 1);

    // What YouTube showed in issue 387: the picture is right in the app's own copy of its
    // surface but green on the display, because the composer never copied it up.
    let mut green_on_screen = good.clone();
    green_on_screen.insert(
        format!("{at}.surface.screen"),
        "wrong colour on screen at 300,410: #008700, not #c1c1c1".into(),
    );
    green_on_screen.insert(
        format!("{at}.shorts.screen"),
        "wrong colour on screen at 300,410: #008700, not #c1c1c1".into(),
    );
    let problems = video_problems(&green_on_screen);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().all(|p| p.contains("on screen")));
    let mut unreported = good.clone();
    unreported.remove(&format!("{at}.surface.screen"));
    assert_eq!(video_problems(&unreported).len(), 1);

    // The error the HAL logged on every picture.
    let log = "11-10 05:00:00.000  612  640 E dsa-codec2: c2.dsa.vp9.decoder: the surface gave no 426x240 buffer (1 in a row): Status(-8, EX_SERVICE_SPECIFIC): '14: '\n\
               11-10 05:00:00.010  612  640 I dsa-codec2: c2.dsa.vp9.decoder: decoding 426x240 on the host's GPU\n";
    assert_eq!(codec_buffer_errors(log).len(), 1);
}

#[test]
fn fr26_a_composer_that_cannot_copy_a_picture_up_fails_the_media_check() {
    let log = "11-12 06:03:13.682   536   541 E dsa-hwcomposer: cannot import gralloc buffer 72 to copy what the CPU wrote: -22\n\
               11-12 06:03:13.700   536   541 E dsa-hwcomposer: cannot import gralloc buffer 73 (640x360, format 0x37393939) to copy what the CPU wrote: -22 (1 in a row); its layer shows what the host held before\n\
               11-12 06:03:13.710   536   541 D dsa-hwcomposer: copied 4050 KiB that a CPU wrote up to the host in 12 uploads over 1003 ms\n\
               11-12 06:03:13.720   536   541 I dsa-hwcomposer: the host renderer makes YUV ColorBuffers\n";
    assert_eq!(copy_up_errors(log).len(), 2);
    assert!(
        copy_up_errors("11-12 06:03:13.710   536   541 E dsa-codec2: cannot import\n").is_empty()
    );
}

#[test]
fn fr26_a_dsa_decoders_configure_refused_as_corrupted_fails_the_media_check() {
    // What a surface-mode configure logged before the pixel format was settable.
    let log = "11-10 05:00:00.000   738   790 D CCodec  : allocate(c2.dsa.avc.decoder)\n\
               11-10 05:00:00.010   738   790 D CCodecConfig: config failed => CORRUPTED\n\
               11-10 05:00:00.020   738   790 D CCodecConfig: query failed after returning 11 values (BAD_INDEX)\n\
               11-10 05:00:01.000  4650  4670 D CCodec  : allocate(c2.android.avc.decoder)\n\
               11-10 05:00:01.010  4650  4670 D CCodecConfig: config failed => CORRUPTED\n\
               11-10 05:00:01.020   738   790 I MediaCodec: loading /vendor/etc/media_codecs_c2.xml\n";
    let found = dsa_config_corrupted(log);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains(" 738 "));
    assert_eq!(
        component_in("[c2.dsa.vp9.decoder] setting surface generation"),
        Some("c2.dsa.vp9.decoder")
    );
    assert_eq!(component_in("loading media_codecs_c2.xml"), None);
    assert_eq!(component_in("libc2.dsa.so"), None);
}

#[test]
fn pr4_the_probes_results_are_read_from_the_instrumentation_output() {
    let out = "INSTRUMENTATION_RESULT: audio.stream.state=1\r\n\
               INSTRUMENTATION_RESULT: audio.stream.written=48000\r\n\
               INSTRUMENTATION_CODE: -1\r\n";
    let results = instrumentation_results(out).unwrap();
    assert_eq!(results["audio.stream.state"], "1");
    assert_eq!(results["audio.stream.written"], "48000");

    // A probe whose process died never reports its code.
    let died = "INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n";
    assert!(instrumentation_results(died).is_err());
    // An error the probe caught fails the check.
    let caught =
        "INSTRUMENTATION_RESULT: error=java.lang.IllegalStateException\nINSTRUMENTATION_CODE: -1\n";
    assert!(instrumentation_results(caught).is_err());
}

#[test]
fn pr4_a_track_that_could_not_get_its_control_block_is_a_failure() {
    let good: BTreeMap<String, String> = [
        ("audio.stream.state", "1"),
        ("audio.stream.written", "48000"),
        ("audio.stream.played", "47000"),
        ("audio.static.state", "1"),
        ("audio.static.written", "48000"),
        ("audio.static.played", "48000"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert!(
        audio_problems(&good).is_empty(),
        "{:?}",
        audio_problems(&good)
    );

    // What the image's apps saw: the builder threw, so there is no state.
    let mut refused = good.clone();
    refused.remove("audio.stream.state");
    refused.remove("audio.stream.written");
    refused.remove("audio.stream.played");
    refused.insert(
        "audio.stream.error".into(),
        "java.lang.UnsupportedOperationException: Cannot create AudioTrack".into(),
    );
    let problems = audio_problems(&refused);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("Cannot create AudioTrack"));

    let mut silent = good;
    silent.insert("audio.static.played".into(), "0".into());
    assert_eq!(audio_problems(&silent).len(), 1);
}

#[test]
fn pr4_a_decoder_that_could_not_start_is_a_failure() {
    let good: BTreeMap<String, String> = [
        ("encode.avc.units", "30"),
        (
            "decode.avc.decoders",
            "c2.dsa.avc.decoder,c2.android.avc.decoder",
        ),
        ("decode.c2.dsa.avc.decoder.out", "30"),
        ("decode.c2.android.avc.decoder.out", "30"),
        ("encode.aac.units", "48"),
        ("decode.aac.decoders", "c2.android.aac.decoder"),
        ("decode.c2.android.aac.decoder.out", "47104"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert!(
        decode_problems(&good).is_empty(),
        "{:?}",
        decode_problems(&good)
    );

    // What every app saw: start() failed with NO_MEMORY, the software decoders' too.
    let mut refused = good.clone();
    refused.remove("decode.c2.android.avc.decoder.out");
    refused.insert(
        "decode.c2.android.avc.decoder.error".into(),
        "android.media.MediaCodec$CodecException: Error 0xfffffff4".into(),
    );
    let problems = decode_problems(&refused);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("c2.android.avc.decoder failed"));

    // A decoder that started but gave nothing back.
    let mut empty = good.clone();
    empty.insert("decode.c2.dsa.avc.decoder.out".into(), "0".into());
    assert_eq!(decode_problems(&empty).len(), 1);

    // An encoder that failed leaves nothing to decode.
    let mut no_encoder = good;
    no_encoder.insert(
        "encode.aac.error".into(),
        "java.io.IOException: no encoder".into(),
    );
    assert_eq!(decode_problems(&no_encoder).len(), 1);
}

#[test]
fn pr4_a_denial_of_a_media_services_shared_memory_is_found_in_the_log() {
    let log = "11-08 12:00:01.554  4650  4650 I auditd  : type=1400 audit(0.0:30): avc:  denied  { read write } for  comm=\"binder:772_3\" path=\"/memfd:AudioFlinger::Client (deleted)\" dev=\"tmpfs\" ino=120 scontext=u:r:untrusted_app:s0:c149,c256,c512,c768 tcontext=u:object_r:audioserver_tmpfs:s0 tclass=file permissive=0\n\
               11-08 12:00:02.000  4650  4650 I auditd  : type=1400 audit(0.0:31): avc:  denied  { read } for  comm=\"uiautomator\" scontext=u:r:shell:s0 tcontext=u:object_r:qemu_sf_lcd_density_prop:s0 tclass=file permissive=0\n\
               11-08 12:00:03.000  4650  4650 I AudioTrack: start\n";
    let found = denials(log, SHARED_MEMORY_TYPES);
    assert_eq!(found.len(), 1);
    assert!(found[0].contains("audioserver_tmpfs"));
}

#[test]
fn fr26_the_sensors_hal_refused_its_event_queue_is_found_in_the_log() {
    // What every boot logged before the sensors HAL could use system_server's memfds.
    let log = "11-08 11:53:18.987   772   772 I auditd  : type=1400 audit(0.0:8): avc:  denied  { read write } for  comm=\"system-server-i\" path=2F6D656D66643A4D6573736167655175657565202864656C6574656429 dev=\"tmpfs\" ino=2073 scontext=u:r:hal_sensors_default:s0 tcontext=u:object_r:system_server_tmpfs:s0 tclass=file permissive=0\n";
    assert_eq!(denials(log, SHARED_MEMORY_TYPES).len(), 1);
}

fn agent_language_settings() -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let settings = [
        ("tts_default_synth", AGENT.to_string()),
        (
            "voice_recognition_service",
            format!("{AGENT}/{AGENT}.platform.HostRecognitionService"),
        ),
        (
            "selected_spell_checker",
            format!("{AGENT}/.platform.HostSpellCheckerService"),
        ),
        ("spell_checker_enabled", "1".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let services = LANGUAGE_SERVICES
        .iter()
        .map(|(_, action, class)| (action.to_string(), format!("{AGENT}/{class}\n")))
        .collect();
    (settings, services)
}

#[test]
fn fr36_the_boot_check_finds_the_agents_speech_and_spelling_services_the_defaults() {
    let (settings, services) = agent_language_settings();
    assert_eq!(
        language_service_problems(&settings, &services),
        Vec::<String>::new()
    );
}

#[test]
fn fr36_the_boot_check_rejects_another_default_or_engine() {
    let (mut settings, mut services) = agent_language_settings();
    settings.insert("tts_default_synth".into(), "com.svox.pico".into());
    settings.insert("spell_checker_enabled".into(), "null".into());
    services.insert(
        "android.service.textservice.SpellCheckerService".into(),
        format!("{AGENT}/.platform.HostSpellCheckerService\ncom.android.inputmethod.latin/.spellcheck.AndroidSpellCheckerService\n"),
    );
    let problems = language_service_problems(&settings, &services);
    assert_eq!(problems.len(), 3, "{problems:?}");
}

#[test]
fn fr26_the_boot_check_finds_sound_turned_down_before_the_host() {
    let full = "dsa-devices: audio stream primary opened\n\
                audio stream primary is idle: 96000 frames received, peak 8000\n";
    assert!(sound_level_problems(full).is_empty());
    assert_eq!(loudest_peak(full), Some(8000));
    // Android's media volume at 10 out of 15 took the tone some 15 dB down.
    let turned_down = "audio stream primary is idle: 96000 frames received, peak 1423\n\
                       audio stream app 0 closed (the guest closed it): 0 frames received, \
                       peak 0, 0 frames of underrun\n";
    assert_eq!(loudest_peak(turned_down), Some(1423));
    assert_eq!(sound_level_problems(turned_down).len(), 1);
    assert_eq!(sound_level_problems("nothing about sound").len(), 1);
}

#[test]
fn pr4_an_app_plays_sound_and_decodes_h264_and_aac_with_selinux_enforcing() {
    boot_check(true, false, false);
}

#[test]
fn fr35_an_app_needs_approval_and_su_is_hidden_when_disabled() {
    boot_check(false, true, false);
}

#[test]
fn fr28_android_trusts_the_root_certificates_the_host_added() {
    boot_check(false, false, true);
}

#[test]
fn fr37_widevine_is_offered_only_when_installed() {
    BOOT_CHECK_DRM.store(true, Ordering::Relaxed);
    boot_check(false, false, false);
}

/// Whether the boot check runs the Widevine check, which boots three times: without the module,
/// with it, and after it is removed.
static BOOT_CHECK_DRM: AtomicBool = AtomicBool::new(false);

/// The Widevine check boots three times and downloads Google's emulator image, so it stays
/// under twenty-five minutes.
const DRM_BUDGET: Duration = Duration::from_secs(1500);

/// What the probe's "drm" check says that the acceptance does not allow: with the module, an
/// app sees Widevine at L3; without it, not at all.
fn drm_problems(results: &BTreeMap<String, String>, installed: bool) -> Vec<String> {
    let mut problems = Vec::new();
    let supported = results.get("drm.widevine.supported").map(String::as_str);
    let level = results
        .get("drm.widevine.securityLevel")
        .map(String::as_str);
    match (installed, supported) {
        (true, Some("true")) => {
            if level != Some("L3") {
                problems.push(format!(
                    "Widevine is offered at {level:?}, where the module gives L3"
                ));
            }
        }
        (true, other) => problems.push(format!(
            "with Widevine installed, an app is told it is not supported ({other:?})"
        )),
        (false, Some("false")) => {}
        (false, other) => problems.push(format!(
            "without Widevine installed, an app is told it is supported ({other:?})"
        )),
    }
    if let Some(error) = results.get("error") {
        problems.push(format!("the probe's drm check failed: {error}"));
    }
    problems
}

#[test]
fn fr37_the_drm_check_reads_what_the_probe_says() {
    let results = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let none = results(&[("drm.widevine.supported", "false")]);
    let l3 = results(&[
        ("drm.widevine.supported", "true"),
        ("drm.widevine.securityLevel", "L3"),
    ]);
    let l1 = results(&[
        ("drm.widevine.supported", "true"),
        ("drm.widevine.securityLevel", "L1"),
    ]);
    assert!(drm_problems(&none, false).is_empty());
    assert!(drm_problems(&l3, true).is_empty());
    assert_eq!(drm_problems(&none, true).len(), 1);
    assert_eq!(drm_problems(&l3, false).len(), 1);
    assert_eq!(drm_problems(&l1, true).len(), 1);
    assert_eq!(drm_problems(&results(&[]), false).len(), 1);
}

/// Restarts Android and connects adb to it again.
fn restart_android(api: &mut HostApiClient, adb: &Adb) -> Result<(), String> {
    api.stop().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(90);
    while api.status().map_err(|e| e.to_string())?.state != SessionState::Stopped {
        if Instant::now() >= deadline {
            return Err("Android did not stop".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    api.ensure_running(Duration::from_secs(360), Duration::from_secs(2), |_| {})
        .map_err(|e| e.to_string())?;
    adb.connect(Duration::from_secs(120))
}

/// The Widevine check: an app sees no Widevine before it is installed; the module is taken from
/// Google's emulator image (`DSA_GOOGLE_REPOSITORY` names a mirror instead) and installed as
/// `dsa drm widevine install` installs it; after a restart an app sees Widevine at L3 and the
/// module's service runs; once it is removed and Android restarts, an app sees none again.
fn widevine_check(
    api: &mut HostApiClient,
    socket: &Path,
    adb: &Adb,
    dir: &Path,
) -> Result<Vec<String>, String> {
    use dsa_host_core::widevine;
    let mut problems = Vec::new();
    let before = adb.probe(dir, "drm")?;
    eprintln!("drm before the install: {before:?}");
    problems.extend(drm_problems(&before, false));

    let release = adb.shell("getprop ro.build.version.release")?;
    let major: u32 = release
        .trim()
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .ok_or_else(|| format!("Android's version is {release:?}"))?;
    let abi = adb.shell("getprop ro.product.cpu.abi")?.trim().to_string();
    let repository = std::env::var(widevine::REPOSITORY_ENV)
        .unwrap_or_else(|_| widevine::REPOSITORY.to_string());
    let fetch = dsa_host_core::update::store::FileFetch;
    let curl = CurlFetch;
    let fetch: &dyn dsa_host_core::update::store::Fetch = if repository.starts_with("https://") {
        &curl
    } else {
        &fetch
    };
    let image = widevine::find_image(fetch, &repository, major, &abi)?;
    eprintln!("taking Widevine from {}", image.url);
    let apex = dir.join("widevine.apex");
    let package = widevine::take_apex(
        fetch,
        &image,
        major,
        &abi,
        &dir.join("download"),
        &apex,
        &mut |_| {},
    )?;
    let mut file = std::fs::File::open(&apex).map_err(|e| e.to_string())?;
    let installed = client(socket, Duration::from_secs(10))
        .install_widevine(&package, &mut file)
        .map_err(|e| format!("installing Widevine: {e}"))?;
    if !installed.on_disk || !installed.restart_needed {
        problems.push(format!(
            "the install did not put the module on the disk for the next start: {installed:?}"
        ));
    }
    restart_android(api, adb)?;
    let status = api.widevine().map_err(|e| e.to_string())?;
    if !status.active || status.restart_needed {
        problems.push(format!("Android did not start with Widevine: {status:?}"));
    }
    let mounts = adb.shell("cat /proc/mounts")?;
    if !mounts
        .lines()
        .any(|line| line.contains(" /vendor/apex overlay "))
    {
        problems.push("nothing is laid over /vendor/apex".into());
    }
    let active = adb.shell(&format!("ls /apex/{}", widevine::MODULE))?;
    let _ = std::fs::write(dir.join("widevine-apex.txt"), &active);
    let with = adb.probe(dir, "drm")?;
    eprintln!("drm with Widevine: {with:?}");
    problems.extend(drm_problems(&with, true));
    let log = adb.shell("logcat -d -b all").unwrap_or_default();
    let _ = std::fs::write(dir.join("logcat-widevine.txt"), &log);
    for line in denials(
        &log,
        &[
            "dsa_widevine_fs",
            "hal_drm_widevine",
            "hal_drm_widevine_exec",
            "mediadrm_vendor_data_file",
            "vendor_apex_file",
        ],
    ) {
        problems.push(format!("SELinux refused Widevine: {line}"));
    }

    let removed = api.remove_widevine().map_err(|e| e.to_string())?;
    if removed.package.is_some() || removed.on_disk {
        problems.push(format!("the module was not removed: {removed:?}"));
    }
    restart_android(api, adb)?;
    let after = adb.probe(dir, "drm")?;
    eprintln!("drm after the removal: {after:?}");
    problems.extend(drm_problems(&after, false));
    let left = adb.shell("ls -A /data/vendor/mediadrm 2>&1 || true")?;
    if !left.trim().is_empty() && !left.contains("Permission denied") {
        problems.push(format!(
            "the device certificates are still there after the removal: {left}"
        ));
    }
    Ok(problems)
}

/// Downloads through curl, as the command line does on Linux.
struct CurlFetch;

impl dsa_host_core::update::store::Fetch for CurlFetch {
    fn fetch(&self, url: &str, out: &mut dyn std::io::Write) -> Result<u64, String> {
        let mut child = Command::new("curl")
            .args(["--fail", "--silent", "--show-error", "--location"])
            .args(["--retry", "3", "--connect-timeout", "30", "--", url])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("curl cannot run: {e}"))?;
        let mut body = child.stdout.take().ok_or("curl gave no output")?;
        let copied = std::io::copy(&mut body, out).map_err(|e| e.to_string())?;
        let status = child.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("curl failed for {url}: {status}"));
        }
        Ok(copied)
    }
}

/// Where the certificate check adds its test root, as an administrator adds one: the folder
/// of the certificates added to the system's store beside the distribution's bundle.
const ADDED_ROOT: &str = "/usr/local/share/ca-certificates/dsa-fr28-test-root.crt";

/// The port the probe reaches the test's HTTPS server at, on the guest's own loopback address
/// (`adb reverse` carries it to the host's).
const GUEST_HTTPS_PORT: u16 = 8443;

/// The name the test's server certificate is for, and the host the probe checks it under. It
/// is not `localhost`: from Android 17 the network security config of an app without one of
/// its own gives `localhost` and every loopback address a config that trusts no certificate,
/// so a server checked under such a name is refused whatever Android trusts. `.test` is
/// reserved for testing (RFC 6761); the probe connects to the loopback address itself instead
/// of looking the name up.
const CERTIFICATE_HOST: &str = "certificate-check.dsa.test";

/// How long a change on the host may take to reach the apps: the host notices the store
/// changed, sends the guest the roots, and the daemon writes them.
const TRUST_WAIT: Duration = Duration::from_secs(120);

/// Runs a program the check needs and fails with what it said when it does not succeed.
fn run_tool(command: &mut Command) -> Result<String, String> {
    let shown = format!("{command:?}");
    let out = run_bounded(command, Duration::from_secs(120), usize::MAX)?;
    match out.status {
        Some(status) if status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        _ => Err(format!(
            "{shown} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )),
    }
}

/// The test's root in the host's system store, put there with sudo as an administrator would
/// and taken away when dropped, so a failed check leaves the runner's store as it found it.
struct AddedRoot;

impl AddedRoot {
    fn add(root: &Path) -> Result<AddedRoot, String> {
        run_tool(
            Command::new("sudo")
                .args(["-n", "install", "-m", "0644"])
                .arg(root)
                .arg(ADDED_ROOT),
        )?;
        run_tool(Command::new("sudo").args(["-n", "update-ca-certificates"]))?;
        Ok(AddedRoot)
    }

    fn remove(&self) -> Result<(), String> {
        run_tool(Command::new("sudo").args(["-n", "rm", "-f", ADDED_ROOT]))?;
        run_tool(Command::new("sudo").args(["-n", "update-ca-certificates", "--fresh"]))?;
        Ok(())
    }
}

impl Drop for AddedRoot {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

/// Makes a throwaway root and a server certificate for `localhost` signed by it, in `dir`.
fn make_test_certificates(dir: &Path) -> Result<(), String> {
    let openssl = || {
        let mut command = Command::new("openssl");
        command.current_dir(dir);
        command
    };
    run_tool(openssl().args([
        "req",
        "-x509",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
        "-keyout",
        "root.key",
        "-out",
        "root.crt",
        "-days",
        "2",
        "-subj",
        "/O=DSA Test/CN=DSA Host Certificates Test Root",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    ]))?;
    run_tool(openssl().args([
        "req",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
        "-keyout",
        "server.key",
        "-out",
        "server.csr",
        "-subj",
        &format!("/CN={CERTIFICATE_HOST}"),
    ]))?;
    std::fs::write(
        dir.join("server.ext"),
        format!(
            "subjectAltName=DNS:{CERTIFICATE_HOST}\nbasicConstraints=CA:FALSE\n\
             keyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    run_tool(openssl().args([
        "x509",
        "-req",
        "-in",
        "server.csr",
        "-CA",
        "root.crt",
        "-CAkey",
        "root.key",
        "-CAcreateserial",
        "-out",
        "server.crt",
        "-days",
        "2",
        "-extfile",
        "server.ext",
    ]))?;
    Ok(())
}

impl Adb {
    /// What the probe's HTTPS check found at the test's server ("connected", "untrusted",
    /// "misnamed" or "unreachable"), and what the probe's process saw of Android's certificate
    /// folders, of the file `certificate` among them, and of its mount table (its `certs.*`
    /// results, one line each).
    fn https(
        &self,
        dir: &Path,
        attempt: usize,
        certificate: &str,
    ) -> Result<(String, String), String> {
        let out = self.shell(&format!(
            "am instrument -w -r -e check https \
             -e url https://{CERTIFICATE_HOST}:{GUEST_HTTPS_PORT}/ -e address 127.0.0.1 \
             -e certificate {certificate} {PROBE_RUNNER}"
        ))?;
        let _ = std::fs::write(dir.join(format!("probe-https-{attempt}.txt")), &out);
        let results = instrumentation_results(&out)?;
        let seen = results
            .iter()
            .filter(|(key, _)| key.starts_with("certs."))
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let outcome = results
            .get("outcome")
            .cloned()
            .ok_or_else(|| format!("the HTTPS check said no outcome: {results:?}"))?;
        Ok((outcome, seen))
    }
}

/// Asks the probe until its HTTPS check finds `wanted`, for [TRUST_WAIT] at most. `certificate`
/// is the test root's file name in Android's folders, which the probe looks for in the app's
/// own view of them.
fn wait_for_https(
    adb: &Adb,
    dir: &Path,
    attempt: &mut usize,
    certificate: &str,
    wanted: &str,
    what: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + TRUST_WAIT;
    loop {
        *attempt += 1;
        let (outcome, seen) = adb.https(dir, *attempt, certificate)?;
        eprintln!("HTTPS check {attempt} ({what}): {outcome}");
        if outcome == wanted {
            return Ok(());
        }
        if outcome == "unreachable" {
            return Err(format!(
                "{what}: the probe did not reach the test's server at all (probe-https-{attempt}.txt)"
            ));
        }
        if outcome == "misnamed" {
            return Err(format!(
                "{what}: the test's server certificate does not name {CERTIFICATE_HOST} \
                 (probe-https-{attempt}.txt)"
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{what}: the HTTPS check still found {outcome} after {} s, not {wanted}; the \
                 probe's process saw (probe-https-{attempt}.txt):\n{seen}\nand adb's shell \
                 mounts:\n{}",
                TRUST_WAIT.as_secs(),
                certificate_mounts(adb)
            ));
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// The lines of adb's shell's mount table that name a certificate folder, to set beside the
/// probe's own when the HTTPS check does not find what it should.
fn certificate_mounts(adb: &Adb) -> String {
    match adb.shell("cat /proc/self/mountinfo") {
        Ok(table) => {
            let lines: Vec<&str> = table.lines().filter(|l| l.contains("cacerts")).collect();
            if lines.is_empty() {
                "none".to_string()
            } else {
                lines.join("\n")
            }
        }
        Err(why) => format!("unreadable: {why}"),
    }
}

/// The folder of Android's system certificates that apps read: the APEX's, which Android
/// prefers to its own folder under /system whenever it holds any certificate. The image binds
/// a copy over it that holds Android's certificates and the host's roots.
const APPS_CERTIFICATES: &str = "/apex/com.android.conscrypt/cacerts";

/// How many certificates the folder apps read holds, and whether `name` is among them.
fn guest_certificates(adb: &Adb, name: &str) -> Result<(usize, bool), String> {
    let listed = adb.shell(&format!("ls {APPS_CERTIFICATES}"))?;
    let names: Vec<&str> = listed.split_whitespace().collect();
    Ok((names.len(), names.contains(&name)))
}

/// The root certificate check: an app reaches a local HTTPS server signed by a root the host
/// added, and is refused once the root is taken away or the setting is off; Android's own
/// certificates stay as they were throughout, and a root the host did not add is not trusted.
fn certificate_check(api: &mut HostApiClient, adb: &Adb, dir: &Path) -> Result<(), String> {
    // The probe stands in for an app the user has open, but it shows no window, so DSA puts
    // Android into Doze partway through the check, and Doze cuts an app off the network: its
    // lookup of "localhost" then fails and the check reads as "unreachable". An app with a
    // window keeps Android out of Doze; the probe is exempted from it instead.
    adb.shell(&format!("cmd deviceidle whitelist +{PROBE_PACKAGE}"))
        .map_err(|why| format!("exempting the probe from Doze: {why}"))?;
    make_test_certificates(dir)?;
    let hash = run_tool(Command::new("openssl").current_dir(dir).args([
        "x509",
        "-noout",
        "-subject_hash_old",
        "-in",
        "root.crt",
    ]))?;
    let file = format!("{}.0", hash.trim());
    let fingerprint = run_tool(Command::new("openssl").current_dir(dir).args([
        "x509",
        "-noout",
        "-fingerprint",
        "-sha256",
        "-in",
        "root.crt",
    ]))?;
    let fingerprint = fingerprint
        .trim()
        .rsplit('=')
        .next()
        .unwrap_or_default()
        .replace(':', "")
        .to_ascii_lowercase();

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map_err(|e| e.to_string())?
        .port();
    let _server = Service::spawn(
        Command::new("openssl").current_dir(dir).args([
            "s_server",
            "-accept",
            &format!("127.0.0.1:{port}"),
            "-cert",
            "server.crt",
            "-key",
            "server.key",
            "-www",
        ]),
        dir.join("https-server.log"),
    );
    // The server listens before the probe looks, so "unreachable" means the way to it is
    // broken, not that it was still starting.
    let listening = Instant::now() + Duration::from_secs(20);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if Instant::now() >= listening {
            return Err("the test's HTTPS server did not start (https-server.log)".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    adb.run(&[
        "reverse",
        &format!("tcp:{GUEST_HTTPS_PORT}"),
        &format!("tcp:{port}"),
    ])?;

    let mut attempt = 0;
    let (android_count, present) = guest_certificates(adb, &file)?;
    if present {
        return Err(format!(
            "{file} is among Android's certificates before the root was added"
        ));
    }
    eprintln!("Android has {android_count} system certificates of its own");
    wait_for_https(
        adb,
        dir,
        &mut attempt,
        &file,
        "untrusted",
        "before the root is added",
    )?;

    let added = AddedRoot::add(&dir.join("root.crt"))?;
    wait_for_https(
        adb,
        dir,
        &mut attempt,
        &file,
        "connected",
        "with the root added on the host",
    )
    .map_err(|why| match guest_certificates(adb, &file) {
        Ok((count, present)) => format!(
            "{why}; Android's folder holds {count} certificates (it had {android_count}), and \
             {file} is {}there",
            if present { "" } else { "not " }
        ),
        Err(listing) => format!("{why}; Android's folder could not be listed: {listing}"),
    })?;
    match guest_certificates(adb, &file)? {
        (count, true) if count == android_count + 1 => {}
        (count, present) => {
            return Err(format!(
                "with the root added, Android's folder holds {count} certificates (it had \
                 {android_count}), and {file} is {}there",
                if present { "" } else { "not " }
            ))
        }
    }
    let report = api.settings().map_err(|e| e.to_string())?;
    if !report
        .imported_certificates
        .iter()
        .any(|certificate| certificate.sha256 == fingerprint)
    {
        return Err(format!(
            "the settings do not list the added root ({fingerprint}): {:?}",
            report.imported_certificates
        ));
    }

    // Off: the root goes, and Android's own stay.
    let mut wanted = report.settings;
    wanted.host_certificates = false;
    api.set_settings(&wanted).map_err(|e| e.to_string())?;
    wait_for_https(
        adb,
        dir,
        &mut attempt,
        &file,
        "untrusted",
        "with the setting off",
    )?;
    if guest_certificates(adb, &file)? != (android_count, false) {
        return Err("with the setting off, Android's folder is not as it was".into());
    }

    // On again, then the root taken away on the host.
    wanted.host_certificates = true;
    api.set_settings(&wanted).map_err(|e| e.to_string())?;
    wait_for_https(
        adb,
        dir,
        &mut attempt,
        &file,
        "connected",
        "with the setting on again",
    )?;
    added.remove()?;
    drop(added);
    wait_for_https(
        adb,
        dir,
        &mut attempt,
        &file,
        "untrusted",
        "with the root taken away on the host",
    )?;
    if guest_certificates(adb, &file)? != (android_count, false) {
        return Err("with the root taken away, Android's folder is not as it was".into());
    }
    let _ = adb.run(&["reverse", "--remove-all"]);
    Ok(())
}

fn boot_check(check_media: bool, check_root: bool, check_certificates: bool) {
    let (Some(crosvm), Some(image), Some(probe)) = (
        std::env::var_os("DSA_CROSVM").map(PathBuf::from),
        std::env::var_os("DSA_WIDGET_TEST_IMAGE").map(PathBuf::from),
        std::env::var_os("DSA_MEDIA_PROBE").map(PathBuf::from),
    ) else {
        eprintln!(
            "skipped: DSA_CROSVM, DSA_WIDGET_TEST_IMAGE and DSA_MEDIA_PROBE name no crosvm, image and probe"
        );
        return;
    };
    let kernel = std::env::var_os("DSA_WIDGET_TEST_KERNEL").map(PathBuf::from);
    let adb_program = std::env::var_os("DSA_ADB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("adb"));
    let started = Instant::now();
    let check_drm = BOOT_CHECK_DRM.load(Ordering::Relaxed);
    let (budget, what) = if check_drm {
        (DRM_BUDGET, "the Widevine check")
    } else if check_root {
        (ROOT_BUDGET, "the root check")
    } else if check_certificates {
        (CERTIFICATES_BUDGET, "the root certificate check")
    } else {
        (MEDIA_BUDGET, "the media check")
    };
    let deadline = started + budget;
    let _watchdog = Watchdog::start(what, budget + OVERRUN);
    let dir = scratch(if check_drm {
        "drm"
    } else if check_root {
        "root"
    } else if check_certificates {
        "certs"
    } else {
        "media"
    });
    let approver = dir.join("approver");
    std::fs::create_dir_all(&approver).unwrap();
    std::fs::set_permissions(&approver, std::fs::Permissions::from_mode(0o700)).unwrap();
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
        "test_root_approver": approver,
    });
    std::fs::write(&config, settings.to_string()).unwrap();
    // The host decoder the guest's codec store finds when it starts, so it must be up before
    // the guest boots.
    let video_stop = Arc::new(AtomicBool::new(false));
    let video_log: Arc<Mutex<Vec<String>>> = Arc::default();
    let pattern_video = if check_media {
        Some(serve_pattern_video(
            Arc::clone(&video_stop),
            Arc::clone(&video_log),
        ))
    } else {
        None
    };
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
    // The guest's sound goes to the host's audio port, which dsa-devices serves.
    let devices_log = dir.join("devices.log");
    let _devices = Service::spawn(
        Command::new(env!("CARGO_BIN_EXE_dsa-devices"))
            .arg("--socket")
            .arg(&socket),
        devices_log.clone(),
    );
    let mut api = client(&socket, Duration::from_secs(30));
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let reader = client(&socket, Duration::from_secs(10))
            .watch_log(&LogFilter::default(), false)
            .expect("reading the guest log");
        let log = Arc::clone(&log);
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
    let boot = deadline.saturating_duration_since(Instant::now()) - Duration::from_secs(240);
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

    let probe_path = probe.to_string_lossy().into_owned();
    if let Err(why) = adb.run(&["install", "-r", "-g", &probe_path]) {
        fail(format!("installing the media probe: {why}"), &service, &log);
    }
    match adb.shell(&format!("pm list packages {PROBE_PACKAGE}")) {
        Ok(list) if list.contains(PROBE_PACKAGE) => {}
        Ok(list) => fail(
            format!("the media probe is not installed: {list}"),
            &service,
            &log,
        ),
        Err(why) => fail(why, &service, &log),
    }

    let mut problems = Vec::new();
    if check_root {
        // Keep the first boot's init and kernel messages before the approval check restarts it.
        for (name, command) in [
            ("boot-logcat.txt", "logcat -d -b all"),
            ("guard.txt", "ls -ldZ /system/xbin /system/xbin/su; stat /system/xbin /system/xbin/su; cat /proc/filesystems; cat /proc/mounts; cat /sys/kernel/security/lsm; ls -lZ /sys/kernel/security /sys/kernel/security/dsa_su"),
        ] {
            let output = adb.shell(command).unwrap_or_else(|error| error);
            let _ = std::fs::write(dir.join(name), output);
        }
        match adb.shell("getprop init.svc.dsa_su") {
            Ok(state) if matches!(state.trim(), "" | "stopped") => {}
            Ok(state) => problems.push(format!(
                "the root helper is running while root elevation is off: {state}"
            )),
            Err(why) => problems.push(why),
        }
        match adb.shell("getprop ro.debuggable") {
            Ok(value) if value.trim() == "0" => {}
            Ok(value) => problems.push(format!("ro.debuggable is not 0: {value}")),
            Err(why) => problems.push(why),
        }
        match adb.shell("getprop ro.build.tags") {
            Ok(value) if value.trim() == "release-keys" => {}
            Ok(value) => problems.push(format!("ro.build.tags is not release-keys: {value}")),
            Err(why) => problems.push(why),
        }
        match adb.probe(&dir, "root_absent") {
            Ok(results) => problems.extend(root_absent_problems(&results)),
            Err(why) => problems.push(why),
        }
    }
    if check_media {
        // The agent's text-to-speech engine, speech recognition service and spell checker
        // are the image's only ones, and its defaults.
        let mut settings = BTreeMap::new();
        for key in LANGUAGE_SERVICES
            .iter()
            .map(|(key, _, _)| *key)
            .chain(["spell_checker_enabled"])
        {
            let value = adb
                .shell(&format!("settings get secure {key}"))
                .unwrap_or_else(|error| error);
            settings.insert(key.to_string(), value.trim().to_string());
        }
        let mut services = BTreeMap::new();
        for (_, action, _) in LANGUAGE_SERVICES {
            let listed = adb
                .shell(&format!("cmd package query-services --brief -a {action}"))
                .unwrap_or_else(|error| error);
            services.insert(action.to_string(), listed);
        }
        eprintln!("speech and spelling: {settings:?} {services:?}");
        problems.extend(language_service_problems(&settings, &services));
        match adb.probe(&dir, "audio") {
            Ok(results) => {
                eprintln!("audio: {results:?}");
                problems.extend(audio_problems(&results));
                // How loud the tone reached the host, at Android's default media volume.
                let waited = Instant::now();
                let level = loop {
                    let log = std::fs::read_to_string(&devices_log).unwrap_or_default();
                    let level = sound_level_problems(&log);
                    if level.is_empty() || waited.elapsed() >= LEVEL_WAIT {
                        eprintln!("sound level on the host: {:?}", loudest_peak(&log));
                        break level;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                };
                problems.extend(level);
            }
            Err(why) => problems.push(why),
        }
        match adb.probe(&dir, "decode") {
            Ok(results) => {
                eprintln!("decode: {results:?}");
                problems.extend(decode_problems(&results));
            }
            Err(why) => problems.push(why),
        }
        match &pattern_video {
            Some(Ok(_)) => match adb.probe(&dir, "video") {
                Ok(results) => {
                    eprintln!("video: {results:?}");
                    problems.extend(video_problems(&results));
                }
                Err(why) => problems.push(why),
            },
            Some(Err(why)) => problems.push(format!("the boot check's host decoder: {why}")),
            None => {}
        }
        let _ = std::fs::write(dir.join("video.log"), lock(&video_log).join("\n"));
        // Every denial since the boot, from the guest's own log, where logd keeps the kernel's
        // audit messages.
        let guest_log = adb.shell("logcat -d -b all").unwrap_or_default();
        let _ = std::fs::write(dir.join("logcat.txt"), &guest_log);
        for line in denials(&guest_log, SHARED_MEMORY_TYPES) {
            problems.push(format!("SELinux refused shared memory: {line}"));
        }
        for line in codec_buffer_errors(&guest_log) {
            problems.push(format!("the codec HAL lost a picture: {line}"));
        }
        for line in copy_up_errors(&guest_log) {
            problems.push(format!(
                "the composer could not copy a picture up to the host: {line}"
            ));
        }
        for line in dsa_config_corrupted(&guest_log) {
            problems.push(format!(
                "a DSA decoder's configure was refused as corrupted: {line}"
            ));
        }
    }
    if check_drm {
        match widevine_check(&mut api, &socket, &adb, &dir) {
            Ok(found) => problems.extend(found),
            Err(why) => problems.push(format!("Widevine: {why}")),
        }
    }
    if check_root {
        if let Err(why) = root_approval_check(&mut api, &adb, &dir) {
            problems.push(format!("root elevation: {why}"));
        }
    }
    if check_certificates {
        if let Err(why) = certificate_check(&mut api, &adb, &dir) {
            problems.push(format!("the host's root certificates: {why}"));
        }
        let guest_log = adb.shell("logcat -d -b all").unwrap_or_default();
        let _ = std::fs::write(dir.join("logcat.txt"), &guest_log);
        // The copies' own type, and Android's folders init binds them over.
        for line in denials(
            &guest_log,
            &["dsa_cacerts_file", "system_security_cacerts_file"],
        ) {
            problems.push(format!("SELinux refused the host's certificates: {line}"));
        }
    }

    video_stop.store(true, Ordering::Relaxed);
    if !problems.is_empty() {
        fail(problems.join("\n"), &service, &log);
    }
}
