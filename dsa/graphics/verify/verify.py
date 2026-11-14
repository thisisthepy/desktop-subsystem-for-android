# /// script
# requires-python = ">=3.10"
# dependencies = ["psutil>=5.9"]
# ///
"""Checks the graphics path of a running DSA and measures it.

    uv run dsa/graphics/verify/verify.py check-subset
    uv run dsa/graphics/verify/verify.py fetch
    uv run dsa/graphics/verify/verify.py fetch-cts
    uv run dsa/graphics/verify/verify.py cts      [--serial S] [--software-vulkan] [--results DIR]
    uv run dsa/graphics/verify/verify.py apps     [--serial S] [--yes]
    uv run dsa/graphics/verify/verify.py measure  [--serial S] [--seconds 30]

check-subset needs only the network: it checks that every pattern in vk-cts-subset.txt
still names cases of the pinned CTS and writes the expanded case list. fetch-cts does that
and downloads the Vulkan CTS APK, without the test apps. The other commands need a running
DSA with developer mode on (dsa settings set developer-mode on): they connect adb to
127.0.0.1:58526 themselves, with the adb that DSA_ADB names or the one on the path.
Downloads go to .scratch/graphics-verify/.

`cts --software-vulkan` is for a host renderer on software Vulkan (Lavapipe), as in CI: the
cases the subset file lists under `lavapipe:` still run and are reported, but a failure of
one of them does not fail the run. `--results DIR` keeps each case's result there.
"""

import argparse
import fnmatch
import io
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
import urllib.request
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
WORK = REPO / ".scratch" / "graphics-verify"
SUBSET = HERE / "vk-cts-subset.txt"

# The Android CTS release the Vulkan CTS (dEQP) APK and its mustpass lists come from. Only the
# members below are read from the archive, by HTTP range requests, not the whole archive.
# It is the newest Android release DSA ships an image of. The CTS fails every KHR extension it
# does not know ("Unknown extension"), and each Android release's loader adds some of its own:
# Android 17's offers VK_KHR_surface_maintenance1 and VK_KHR_present_id2, which Android 16's
# CTS fails. A newer CTS knows every extension an older Android offers.
CTS_URL = "https://dl.google.com/dl/android/cts/android-cts-17_r3-linux_x86-x86.zip"
CTS_DIR = "android-cts/testcases/CtsDeqpTestCases/"
DEQP_APK = CTS_DIR + "com.drawelements.deqp.apk"
DEQP_APK_SIZE = 209769483
DEQP_APK_CRC = 0x6142F2B5
MUSTPASS = CTS_DIR + "vk-main-2019-03-01/"
DEQP_PACKAGE = "com.drawelements.deqp"
# The CTS runs in a process named after the package with this suffix, so `pidof` of the
# package alone never finds it.
DEQP_PROCESSES = f"{DEQP_PACKAGE}:testercore {DEQP_PACKAGE}"
# How many polls in a row the CTS process must be missing before a run counts as over. Android
# recreates the CTS activity when its window changes size right after it opens, and the CTS
# ends its process when its activity goes ("Done, killing process"); the new process starts
# about a second later and runs the case list again from the start.
DEQP_GONE_POLLS = 2

# The test apps: Android's NDK samples (Apache-2.0), built by the "Graphics test" workflow.
TEST_APPS_ARTIFACT = "graphics-test-apps"
# Where the runs are when neither --repo, $GITHUB_REPOSITORY nor the checkout's origin says.
OFFICIAL_REPOSITORY = "thisisthepy/desktop-subsystem-for-android"
GITHUB_REMOTE = re.compile(
    r"^(?:https?://(?:[^@/]+@)?github\.com/|ssh://git@github\.com/|git@github\.com:)"
    r"([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+?)(?:\.git)?/?$")


def github_repository(url):
    """owner/name from a GitHub remote URL, or None."""
    match = GITHUB_REMOTE.match(url.strip())
    return f"{match[1]}/{match[2]}" if match else None


def repository(given=None, environ=os.environ, origin_url=None):
    """The repository whose 'Graphics test' runs the test apps come from: --repo, the run's own
    repository in CI, the one this checkout's origin names (a fork's clone takes the fork's
    builds), or the official one."""
    if given:
        return given
    if environ.get("GITHUB_REPOSITORY"):
        return environ["GITHUB_REPOSITORY"]
    if origin_url is None:
        try:
            origin_url = subprocess.run(["git", "-C", str(Path(__file__).parent), "remote", "get-url", "origin"],
                                        capture_output=True, text=True).stdout
        except OSError:
            origin_url = ""
    return github_repository(origin_url) or OFFICIAL_REPOSITORY


@dataclass(frozen=True)
class App:
    key: str
    apk: str
    package: str
    activity: str
    looks_like: str


GLES_APP = App("gles", "hello-gl2.apk", "com.android.gl2jni", ".GL2JNIActivity",
               "a green triangle, point up, in the middle of a background that fades between grays")
VULKAN_APP = App("vulkan", "hello-vulkan.apk", "com.android.hellovk", ".VulkanActivity",
                 "a dark red triangle, point up, in the middle of a background that fades between grays")
APPS = (GLES_APP, VULKAN_APP)


# ---------------------------------------------------------------- the Vulkan CTS subset

SOFTWARE_VULKAN = "lavapipe:"


def read_patterns(text):
    """The patterns of a subset file: one per line, without comments and blank lines. The
    `lavapipe:` lines are not patterns of the subset; read_software_exceptions reads them."""
    return read_subset(text)[0]


def read_software_exceptions(text):
    """The cases that cannot pass on software Vulkan, as `{pattern: reason}`."""
    return read_subset(text)[1]


def read_subset(text):
    """The patterns of a subset file, and its `lavapipe: <pattern>  # <reason>` lines as
    `{pattern: reason}`. An exception without a reason is refused, so none is kept unexplained."""
    patterns, exceptions = [], {}
    for number, raw in enumerate(text.splitlines(), 1):
        line, _, comment = raw.partition("#")
        line = line.strip()
        if not line:
            continue
        if line.startswith(SOFTWARE_VULKAN):
            pattern = line[len(SOFTWARE_VULKAN):].strip()
            reason = comment.strip()
            if not pattern or not reason:
                raise ValueError(f"line {number}: a {SOFTWARE_VULKAN} line names a pattern and says "
                                 f"why after '#': {raw.strip()!r}")
            exceptions[pattern] = reason
        else:
            patterns.append(line)
    return patterns, exceptions


def excused(case, exceptions):
    """Why `case` may fail on software Vulkan, or None when it must pass."""
    return next((reason for pattern, reason in exceptions.items()
                 if fnmatch.fnmatchcase(case, pattern)), None)


def mustpass_files(patterns):
    """The mustpass list each pattern's cases are in: dEQP-VK.<group>.* lives in <group>.txt."""
    files = set()
    for pattern in patterns:
        parts = pattern.split(".")
        if len(parts) < 3 or parts[0] != "dEQP-VK" or "*" in parts[1]:
            raise ValueError(f"{pattern!r} does not name one dEQP-VK group (dEQP-VK.<group>.<case>)")
        files.add(parts[1].replace("_", "-") + ".txt")
    return sorted(files)


def expand_subset(patterns, cases):
    """The cases the patterns select, in mustpass order, and the patterns that select none."""
    selected, unmatched = [], []
    matched = set()
    for pattern in patterns:
        hits = [case for case in cases if fnmatch.fnmatchcase(case, pattern)]
        if not hits:
            unmatched.append(pattern)
        matched.update(hits)
    selected = [case for case in cases if case in matched]
    return selected, unmatched


@dataclass
class CtsResults:
    statuses: dict = field(default_factory=dict)
    session_complete: bool = False
    # The case the log begins but never ends: the one running when the log was read, or the
    # one the CTS process ended in. None when every case the log begins is ended.
    unfinished: str = None

    def failed(self):
        return {case: status for case, status in self.statuses.items()
                if status not in ("Pass", "NotSupported", "QualityWarning", "CompatibilityWarning")}


def parse_qpa(text):
    """The status of each case in a dEQP log (.qpa)."""
    results = CtsResults()
    case = None
    for line in text.splitlines():
        if line.startswith("#beginTestCaseResult "):
            case = line.split(" ", 1)[1].strip()
            results.statuses[case] = "Crash"
        elif line.startswith("#terminateTestCaseResult ") and case:
            results.statuses[case] = line.split(" ", 1)[1].strip() or "Crash"
            case = None
        elif case and "<Result StatusCode=" in line:
            match = re.search(r'StatusCode="([^"]+)"', line)
            if match:
                results.statuses[case] = match.group(1)
        elif line.startswith("#endTestCaseResult"):
            case = None
        elif line.startswith("#endSession"):
            results.session_complete = True
    results.unfinished = case
    return results


def merge_run(merged, run, given, still_running):
    """Adds the statuses one CTS process wrote for the cases it was given to `merged`, and
    returns the case the process ended in, if it ended in one. A case still running when the
    run was stopped because the time ran out is a timeout, not a crash."""
    if still_running and run.unfinished:
        run.statuses[run.unfinished] = "Timeout"
    given = set(given)
    for case, status in run.statuses.items():
        if case in given:
            merged.statuses[case] = status
    merged.session_complete = run.session_complete
    return None if still_running else run.unfinished


def cases_left(cases, results):
    """The cases, in order, that no CTS process has written a status for."""
    return [case for case in cases if case not in results.statuses]


# ---------------------------------------------------------------- measurements

INT64_MAX = (1 << 63) - 1


def parse_sf_latency(text):
    """The refresh period and the present times (ns) in `dumpsys SurfaceFlinger --latency`."""
    lines = [line.split() for line in text.splitlines() if line.strip()]
    if not lines or len(lines[0]) != 1:
        return None, []
    period = int(lines[0][0])
    presents = []
    for fields in lines[1:]:
        if len(fields) != 3:
            continue
        actual = int(fields[1])
        if 0 < actual < INT64_MAX:
            presents.append(actual)
    return period, presents


def frame_stats(presents, period_ns):
    """Frame time statistics, in milliseconds, from a sorted list of present times."""
    times = sorted(set(presents))
    intervals = [(b - a) / 1e6 for a, b in zip(times, times[1:])]
    if not intervals:
        return {"frames": len(times)}
    ordered = sorted(intervals)

    def percentile(p):
        return ordered[min(len(ordered) - 1, int(round(p / 100 * (len(ordered) - 1))))]

    jank_limit = 2 * period_ns / 1e6 if period_ns else None
    return {
        "frames": len(times),
        "fps": round(len(intervals) / (sum(intervals) / 1000), 1),
        "frame_ms_median": round(statistics.median(intervals), 2),
        "frame_ms_p90": round(percentile(90), 2),
        "frame_ms_p99": round(percentile(99), 2),
        "frame_ms_max": round(ordered[-1], 2),
        "janky_frames": sum(1 for t in intervals if jank_limit and t > jank_limit),
    }


def parse_proc_stat(text):
    """(busy, total) jiffies of all CPUs from /proc/stat."""
    for line in text.splitlines():
        if line.startswith("cpu "):
            values = [int(v) for v in line.split()[1:]]
            idle = values[3] + (values[4] if len(values) > 4 else 0)
            return sum(values) - idle, sum(values)
    raise ValueError("/proc/stat has no cpu line")


def cpu_percent(before, after):
    busy = after[0] - before[0]
    total = after[1] - before[1]
    return round(100 * busy / total, 1) if total > 0 else 0.0


def parse_meminfo(text):
    """Memory in use (MiB) from /proc/meminfo: total less what is available."""
    values = {}
    for line in text.splitlines():
        match = re.match(r"(\w+):\s+(\d+) kB", line)
        if match:
            values[match.group(1)] = int(match.group(2))
    return round((values["MemTotal"] - values["MemAvailable"]) / 1024)


def pick_layers(listing, package):
    """The SurfaceFlinger layers an app draws into: its SurfaceView first, then its window."""
    layers = [line.strip() for line in listing.splitlines() if package in line]
    return sorted(layers, key=lambda name: (0 if "SurfaceView" in name else 1, name))


HOST_PROCESSES = ("vmmem", "vmmemdsa", "dsa-wm.exe", "dsa-app-window.exe")


def host_sample():
    """CPU seconds and resident memory of the VM and the compositor's processes."""
    import psutil

    sample = {}
    for proc in psutil.process_iter(["name"]):
        name = (proc.info["name"] or "").lower()
        if name not in HOST_PROCESSES:
            continue
        try:
            times = proc.cpu_times()
            rss = proc.memory_info().rss
        except (psutil.AccessDenied, psutil.NoSuchProcess):
            sample.setdefault("denied", []).append(name)
            continue
        entry = sample.setdefault(name, {"cpu_s": 0.0, "rss": 0})
        entry["cpu_s"] += times.user + times.system
        entry["rss"] += rss
    return sample


def host_summary(before, after, seconds):
    import psutil

    cpus = psutil.cpu_count() or 1
    summary = {}
    for name, entry in after.items():
        if name == "denied":
            continue
        start = before.get(name, {"cpu_s": entry["cpu_s"]})
        summary[name] = {
            "cpu_percent_of_machine": round(100 * (entry["cpu_s"] - start["cpu_s"]) / seconds / cpus, 1),
            "resident_mib": round(entry["rss"] / (1 << 20)),
        }
    if after.get("denied"):
        summary["not_readable"] = sorted(set(after["denied"])) + ["(run as administrator to read them)"]
    return summary


# ---------------------------------------------------------------- downloads


class RemoteFile(io.RawIOBase):
    """A file on an HTTP server that answers range requests, read only where asked."""

    def __init__(self, url):
        self.url = url
        self.pos = 0
        with urllib.request.urlopen(urllib.request.Request(url, method="HEAD")) as response:
            self.size = int(response.headers["Content-Length"])

    def readable(self):
        return True

    def seekable(self):
        return True

    def tell(self):
        return self.pos

    def seek(self, offset, whence=io.SEEK_SET):
        base = {io.SEEK_SET: 0, io.SEEK_CUR: self.pos, io.SEEK_END: self.size}[whence]
        self.pos = base + offset
        return self.pos

    def readinto(self, buffer):
        if self.pos >= self.size:
            return 0
        end = min(self.pos + len(buffer), self.size) - 1
        request = urllib.request.Request(self.url, headers={"Range": f"bytes={self.pos}-{end}"})
        for attempt in range(5):
            try:
                with urllib.request.urlopen(request, timeout=120) as response:
                    data = response.read()
                break
            except OSError:
                if attempt == 4:
                    raise
                time.sleep(2 ** attempt)
        buffer[: len(data)] = data
        self.pos += len(data)
        return len(data)


def open_cts():
    return zipfile.ZipFile(io.BufferedReader(RemoteFile(CTS_URL), 4 << 20))


def caselist(write_to=None):
    """Checks the subset against the pinned CTS and returns the cases it selects."""
    patterns, exceptions = read_subset(SUBSET.read_text(encoding="utf-8"))
    cache = WORK / "mustpass"
    cache.mkdir(parents=True, exist_ok=True)
    archive = None
    cases = []
    for name in mustpass_files(patterns):
        local = cache / name
        if not local.exists():
            archive = archive or open_cts()
            local.write_bytes(archive.read(MUSTPASS + name))
        cases += local.read_text(encoding="utf-8").split()
    selected, unmatched = expand_subset(patterns, cases)
    if unmatched:
        raise SystemExit("these subset patterns match no case of the pinned CTS:\n  " + "\n  ".join(unmatched))
    _, stray = expand_subset(list(exceptions), selected)
    if stray:
        raise SystemExit(f"these {SOFTWARE_VULKAN} patterns match no case of the subset:\n  "
                         + "\n  ".join(stray))
    if write_to:
        write_to.write_text("\n".join(selected) + "\n", encoding="utf-8")
    return selected


def fetch_deqp():
    target = WORK / "com.drawelements.deqp.apk"
    if target.exists() and target.stat().st_size == DEQP_APK_SIZE:
        return target
    print(f"downloading the Vulkan CTS APK ({DEQP_APK_SIZE >> 20} MiB) from {CTS_URL}")
    with open_cts() as archive:
        info = archive.getinfo(DEQP_APK)
        if info.CRC != DEQP_APK_CRC or info.file_size != DEQP_APK_SIZE:
            raise SystemExit(f"{DEQP_APK} in {CTS_URL} is not the pinned one")
        partial = target.with_suffix(".part")
        with archive.open(info) as source, partial.open("wb") as out:
            shutil.copyfileobj(source, out, 4 << 20)  # zipfile checks the CRC at the end
    partial.replace(target)
    return target


def fetch_apps(run_id=None, repo=None):
    apps = WORK / "apps"
    if all((apps / app.apk).exists() for app in APPS):
        return apps
    if not shutil.which("gh"):
        raise SystemExit(f"download the {TEST_APPS_ARTIFACT} artifact of a successful 'Graphics test' run "
                         f"into {apps}, or install the GitHub CLI (gh)")
    repo = repository(repo)
    if run_id is None:
        out = subprocess.run(
            ["gh", "run", "list", "--repo", repo, "--workflow", "test-graphics.yml",
             "--status", "success", "--limit", "20", "--json", "databaseId"],
            check=True, capture_output=True, text=True).stdout
        runs = json.loads(out)
        if not runs:
            raise SystemExit(f"no successful 'Graphics test' run in {repo} to take the test apps from")
        run_id = runs[0]["databaseId"]
    apps.mkdir(parents=True, exist_ok=True)
    subprocess.run(["gh", "run", "download", str(run_id), "--repo", repo,
                    "--name", TEST_APPS_ARTIFACT, "--dir", str(apps)], check=True)
    return apps


# ---------------------------------------------------------------- the device


# Where adb reaches DSA's Android while developer mode is on: the host's loopback port (58526
# unless the adb-port setting picks another), bridged to the guest's adbd.
DEFAULT_SERIAL = "127.0.0.1:58526"


def connect_command(serial):
    """The adb command that connects to `serial` first, for a network serial (host:port); None
    for a device adb already knows by name."""
    if re.fullmatch(r"[^\s:]+:\d+", serial):
        return ["adb", "connect", serial]
    return None


def adb_program():
    """The adb to run: the one DSA_ADB names, as the boot checks take it, or adb on the path."""
    return os.environ.get("DSA_ADB") or "adb"


class Adb:
    def __init__(self, serial=None):
        program = adb_program()
        if not shutil.which(program):
            raise SystemExit(f"{program} is not there (Android SDK platform-tools); DSA_ADB can name one")
        serial = serial or DEFAULT_SERIAL
        connect = connect_command(serial)
        if connect:
            connect[0] = program
            result = subprocess.run(connect, capture_output=True, text=True, encoding="utf-8", errors="replace")
            said = (result.stdout + result.stderr).strip()
            if result.returncode != 0 or "connected" not in said or "cannot" in said or "failed" in said:
                raise SystemExit(
                    f"adb cannot connect to {serial} ({said}). Start Android and turn developer mode "
                    "on (dsa settings set developer-mode on); dsa settings shows the adb port.")
        self.base = [program, "-s", serial]

    def run(self, *args, check=True):
        result = subprocess.run(self.base + list(args), capture_output=True, text=True,
                                encoding="utf-8", errors="replace")
        if check and result.returncode != 0:
            raise SystemExit(f"adb {' '.join(args)} failed: {result.stderr.strip() or result.stdout.strip()}")
        return result.stdout

    def shell(self, command, check=True):
        return self.run("shell", command, check=check)


def graphics_stack(adb):
    """What the guest reports about its GLES and Vulkan drivers."""
    egl = adb.shell("getprop ro.hardware.egl").strip()
    vulkan = adb.shell("getprop ro.hardware.vulkan").strip()
    sf = adb.shell("dumpsys SurfaceFlinger", check=False)
    gles = next((line.strip() for line in sf.splitlines() if line.startswith("GLES:")), "GLES: (not reported)")
    return {"ro.hardware.egl": egl, "ro.hardware.vulkan": vulkan, "surfaceflinger": gles}


def check_stack(stack):
    problems = []
    if stack["ro.hardware.egl"] != "angle":
        problems.append(f"the GLES driver is {stack['ro.hardware.egl']!r}, not ANGLE")
    if "angle" not in stack["surfaceflinger"].lower():
        problems.append(f"SurfaceFlinger does not report ANGLE: {stack['surfaceflinger']}")
    if stack["ro.hardware.vulkan"] != "ranchu":
        problems.append(f"the Vulkan driver is {stack['ro.hardware.vulkan']!r}, not the remoting driver (ranchu)")
    return problems


PASSING = ("Pass", "NotSupported", "QualityWarning", "CompatibilityWarning")


def cts_report(cases, results, exceptions, software_vulkan):
    """Each case's outcome, in case order: its status ("NotRun" when the log does not have
    it), whether it passed, and, for a case that did not, why that is excused on software
    Vulkan (None when it is not)."""
    report = []
    for case in cases:
        status = results.statuses.get(case, "NotRun")
        passed = status in PASSING
        reason = excused(case, exceptions) if software_vulkan and not passed else None
        report.append({"case": case, "status": status, "passed": passed, "excused": reason})
    return report


def report_lines(report):
    """One line per case: PASS, FAIL or EXCUSED, the status, the case, and an excuse's reason."""
    lines = []
    for entry in report:
        verdict = "PASS" if entry["passed"] else ("EXCUSED" if entry["excused"] else "FAIL")
        line = f"{verdict:8} {entry['status']:20} {entry['case']}"
        if entry["excused"]:
            line += f"  ({entry['excused']})"
        lines.append(line)
    return lines


def write_cts_results(out, report, stack, problems, log, logcat):
    """The per-case results as JSON and as text, the dEQP log and the CTS's logcat, in `out`."""
    out.mkdir(parents=True, exist_ok=True)
    (out / "vk-cts-results.json").write_text(json.dumps(
        {"graphics": stack, "stack_problems": problems, "cases": report}, indent=2), encoding="utf-8")
    (out / "vk-cts-results.txt").write_text("\n".join(report_lines(report)) + "\n", encoding="utf-8")
    (out / "vk-cts-logcat.txt").write_text(logcat, encoding="utf-8")
    if log.exists():
        shutil.copyfile(log, out / log.name)


def run_cts(args):
    adb = Adb(args.serial)
    stack = graphics_stack(adb)
    print(json.dumps(stack, indent=2))
    WORK.mkdir(parents=True, exist_ok=True)
    cases_file = WORK / "vk-cts-subset.cases.txt"
    cases = caselist(cases_file)
    exceptions = read_software_exceptions(SUBSET.read_text(encoding="utf-8"))
    apk = fetch_deqp()
    adb.run("install", "-r", "-g", str(apk))
    # The log goes to shared storage, which the CTS writes with the storage permissions that
    # -g granted; newer Android also wants all-files access, which the shell can allow.
    adb.shell(f"appops set {DEQP_PACKAGE} MANAGE_EXTERNAL_STORAGE allow", check=False)
    adb.shell("logcat -c", check=False)
    print(f"running {len(cases)} Vulkan CTS cases")
    deadline = time.time() + args.timeout
    results = CtsResults()
    local_log = WORK / "vk-cts-results.qpa"
    local_log.write_text("", encoding="utf-8")
    remaining = list(cases)
    run = 0
    # A case that ends the CTS process is a crash and fails the subset, but the cases after it
    # still run: each new process is given the cases no process has written a status for.
    while remaining and time.time() < deadline:
        run += 1
        run_cases = WORK / f"vk-cts-cases-{run}.txt"
        run_cases.write_text("\n".join(remaining) + "\n", encoding="utf-8")
        remote_cases = f"/sdcard/dsa-vk-cases-{run}.txt"
        remote_log = f"/sdcard/dsa-vk-results-{run}.qpa"
        run_log = WORK / f"vk-cts-results-{run}.qpa"
        run_log.unlink(missing_ok=True)
        adb.run("push", str(run_cases), remote_cases)
        adb.shell(f"rm -f {remote_log}", check=False)
        command = (f"deqp --deqp-caselist-file={remote_cases} --deqp-log-filename={remote_log} "
                   "--deqp-log-images=disable --deqp-log-shader-sources=disable --deqp-watchdog=enable")
        adb.shell(f"am start -W -n {DEQP_PACKAGE}/android.app.NativeActivity -e cmdLine '{command}'")
        run_results = CtsResults()
        running = True
        gone = 0
        while time.time() < deadline:
            time.sleep(5)
            running = bool(adb.shell(f"pidof {DEQP_PROCESSES}", check=False).strip())
            adb.run("pull", remote_log, str(run_log), check=False)
            if run_log.exists():
                run_results = parse_qpa(run_log.read_text(encoding="utf-8", errors="replace"))
            if run_results.session_complete:
                running = False
                break
            gone = 0 if running else gone + 1
            if gone >= DEQP_GONE_POLLS:
                break
        adb.shell(f"am force-stop {DEQP_PACKAGE}", check=False)
        if run_log.exists():
            with local_log.open("a", encoding="utf-8") as combined:
                combined.write(run_log.read_text(encoding="utf-8", errors="replace"))
        before = len(results.statuses)
        crashed = merge_run(results, run_results, remaining, running)
        remaining = cases_left(cases, results)
        if running or len(results.statuses) == before:
            # The time ran out, or the process wrote no result, so another would not either.
            break
        if crashed and remaining:
            print(f"the CTS process ended in {crashed}; running the {len(remaining)} cases "
                  "after it in a new process")
    logcat = adb.shell("logcat -d -v threadtime", check=False)
    report = cts_report(cases, results, exceptions, args.software_vulkan)
    problems = check_stack(stack)
    counts = {}
    for entry in report:
        counts[entry["status"]] = counts.get(entry["status"], 0) + 1
    print(json.dumps({"cases": len(cases), "statuses": counts, "session_complete": results.session_complete,
                      "log": str(local_log)}, indent=2))
    for line in report_lines(report):
        if not line.startswith("PASS"):
            print("  " + line)
    if args.results:
        write_cts_results(Path(args.results), report, stack, problems, local_log, logcat)
        print(f"each case's result is in {args.results}")
    if not results.statuses:
        print("  the CTS wrote no result; its log lines:")
        for line in logcat.splitlines():
            if "dEQP" in line or DEQP_PACKAGE in line:
                print("    " + line)
    for problem in problems:
        print("  " + problem)
    failed = [entry for entry in report if not entry["passed"] and not entry["excused"]]
    ok = not failed and not problems
    print(f"Vulkan CTS subset: {len(report) - len(failed)} of {len(report)} cases passed or excused; "
          + ("passed" if ok else "FAILED"))
    return 0 if ok else 1


def presented_frames(adb, package, seconds):
    """Present times of the app's busiest layer over `seconds`, sampled before the 128-frame
    history SurfaceFlinger keeps can wrap."""
    layers = pick_layers(adb.shell("dumpsys SurfaceFlinger --list"), package)
    if not layers:
        return None, [], None
    adb.shell("dumpsys SurfaceFlinger --latency-clear", check=False)
    presents = {layer: set() for layer in layers}
    period = None
    end = time.time() + seconds
    while time.time() < end:
        time.sleep(0.5)
        for layer in layers:
            got_period, times = parse_sf_latency(adb.shell(f"dumpsys SurfaceFlinger --latency '{layer}'", check=False))
            period = period or got_period
            presents[layer].update(times)
    busiest = max(layers, key=lambda layer: len(presents[layer]))
    return period, sorted(presents[busiest]), busiest


def launch(adb, app, apps_dir):
    adb.run("install", "-r", "-g", str(apps_dir / app.apk))
    adb.shell(f"am force-stop {app.package}", check=False)
    adb.shell(f"am start -W -n {app.package}/{app.activity}")
    time.sleep(2)


def run_apps(args):
    adb = Adb(args.serial)
    apps_dir = fetch_apps(args.run, args.repo)
    stack = graphics_stack(adb)
    problems = check_stack(stack)
    for app in APPS:
        launch(adb, app, apps_dir)
        period, presents, layer = presented_frames(adb, app.package, 5)
        stats = frame_stats(presents, period)
        print(f"{app.key}: {app.package} presented {stats['frames']} frames in 5 s on {layer}")
        if stats["frames"] < 30:
            problems.append(f"{app.package} presented only {stats['frames']} frames in 5 s")
        if args.yes:
            continue
        answer = input(f"Does the {app.package} window show {app.looks_like}? [y/n] ").strip().lower()
        if answer != "y":
            problems.append(f"{app.package} did not draw correctly in its window")
        adb.shell(f"am force-stop {app.package}", check=False)
    for problem in problems:
        print("  " + problem)
    print("GLES and Vulkan apps: " + ("passed" if not problems else "FAILED"))
    return 0 if not problems else 1


def measure_scenario(adb, name, package, seconds, drive=None):
    guest_cpu_before = parse_proc_stat(adb.shell("cat /proc/stat"))
    host_before = host_sample()
    started = time.time()
    if drive:
        drive.start()
    period, presents, layer = presented_frames(adb, package, seconds)
    if drive:
        drive.stop()
    elapsed = time.time() - started
    guest_cpu_after = parse_proc_stat(adb.shell("cat /proc/stat"))
    host_after = host_sample()
    return {
        "scenario": name,
        "layer": layer,
        "seconds": round(elapsed, 1),
        **frame_stats(presents, period),
        "guest_cpu_percent": cpu_percent(guest_cpu_before, guest_cpu_after),
        "guest_memory_in_use_mib": parse_meminfo(adb.shell("cat /proc/meminfo")),
        "host": host_summary(host_before, host_after, elapsed),
    }


class Swiper:
    """Scrolls the foreground app up and down with `input swipe` in a background process."""

    def __init__(self, adb):
        self.adb = adb
        self.proc = None

    def start(self):
        size = re.search(r"(\d+)x(\d+)", self.adb.shell("wm size"))
        width, height = (int(size.group(1)), int(size.group(2))) if size else (1080, 1920)
        x, low, high = width // 2, height * 3 // 4, height // 4
        loop = (f"while true; do input swipe {x} {low} {x} {high} 300; "
                f"input swipe {x} {high} {x} {low} 300; done")
        self.proc = subprocess.Popen(self.adb.base + ["shell", loop],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def stop(self):
        if self.proc:
            self.proc.kill()
            self.adb.shell("pkill -f 'input swipe'; pkill -f 'while true'", check=False)


def run_measure(args):
    adb = Adb(args.serial)
    apps_dir = fetch_apps(args.run, args.repo)
    environment = {
        "graphics": graphics_stack(adb),
        "guest_build": adb.shell("getprop ro.build.fingerprint").strip(),
        "host_os": sys.platform,
        "measured_at": time.strftime("%Y-%m-%d %H:%M:%S %z"),
    }
    results = []
    adb.shell("am start -W -a android.settings.SETTINGS")
    time.sleep(2)
    results.append(measure_scenario(adb, "app scroll (Settings)", "com.android.settings", args.seconds, Swiper(adb)))
    adb.shell("input keyevent KEYCODE_BACK", check=False)
    for app, label in ((GLES_APP, "OpenGL ES game load (hello-gl2)"), (VULKAN_APP, "Vulkan game load (hello-vulkan)")):
        launch(adb, app, apps_dir)
        results.append(measure_scenario(adb, label, app.package, args.seconds))
        adb.shell(f"am force-stop {app.package}", check=False)
    report = {"environment": environment, "results": results}
    out = WORK / f"measure-{time.strftime('%Y%m%d-%H%M%S')}.json"
    out.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report, indent=2))
    print(f"saved to {out}")
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check-subset", help="check the Vulkan CTS subset against the pinned CTS")
    fetch = sub.add_parser("fetch", help="download the Vulkan CTS APK and the test apps")
    sub.add_parser("fetch-cts", help="download the Vulkan CTS APK and write the expanded case list")
    for name in ("cts", "apps", "measure"):
        command = sub.add_parser(name)
        command.add_argument("--serial", help=f"the adb serial of the DSA guest (default {DEFAULT_SERIAL}, "
                             "developer mode's loopback port)")
        if name == "cts":
            command.add_argument("--timeout", type=int, default=1800)
            command.add_argument("--software-vulkan", action="store_true",
                                 help="the host renderer runs on software Vulkan (Lavapipe): the subset's "
                                 "lavapipe: cases may fail, for the reason the subset file gives")
            command.add_argument("--results", help="a folder for each case's result (JSON and text), "
                                 "the dEQP log and the guest log")
    for command in (fetch, sub.choices["apps"], sub.choices["measure"]):
        command.add_argument("--run", type=int, help="the 'Graphics test' run to take the test apps from")
        command.add_argument("--repo", help="owner/name of the repository that run is in (default: $GITHUB_REPOSITORY, "
                             "else the one this checkout's origin names, so a fork takes its own builds)")
    sub.choices["apps"].add_argument("--yes", action="store_true", help="skip the questions about what each window shows")
    sub.choices["measure"].add_argument("--seconds", type=int, default=30)
    args = parser.parse_args(argv)

    if args.command == "check-subset":
        WORK.mkdir(parents=True, exist_ok=True)
        cases = caselist(WORK / "vk-cts-subset.cases.txt")
        print(f"the subset selects {len(cases)} cases of the pinned CTS")
        return 0
    if args.command == "fetch-cts":
        WORK.mkdir(parents=True, exist_ok=True)
        cases = caselist(WORK / "vk-cts-subset.cases.txt")
        print(f"the subset selects {len(cases)} cases of the pinned CTS")
        print(fetch_deqp())
        return 0
    if args.command == "fetch":
        WORK.mkdir(parents=True, exist_ok=True)
        print(fetch_deqp())
        print(fetch_apps(args.run, args.repo))
        return 0
    return {"cts": run_cts, "apps": run_apps, "measure": run_measure}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
