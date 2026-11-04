# /// script
# requires-python = ">=3.10"
# dependencies = ["psutil>=5.9"]
# ///
"""Checks the graphics path of a running DSA and measures it.

    uv run dsa/graphics/verify/verify.py check-subset
    uv run dsa/graphics/verify/verify.py fetch
    uv run dsa/graphics/verify/verify.py cts      [--serial S]
    uv run dsa/graphics/verify/verify.py apps     [--serial S] [--yes]
    uv run dsa/graphics/verify/verify.py measure  [--serial S] [--seconds 30]

check-subset needs only the network: it checks that every pattern in vk-cts-subset.txt
still names cases of the pinned CTS and writes the expanded case list. The other commands
need an installed DSA with developer mode on (dsa settings set developer-mode on), on the
Windows machine whose GPU renders the task windows: they connect adb to 127.0.0.1:58526
themselves. Downloads go to .scratch/graphics-verify/.
"""

import argparse
import fnmatch
import io
import json
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
CTS_URL = "https://dl.google.com/dl/android/cts/android-cts-16_r2-linux_x86-x86.zip"
CTS_DIR = "android-cts/testcases/CtsDeqpTestCases/"
DEQP_APK = CTS_DIR + "com.drawelements.deqp.apk"
DEQP_APK_SIZE = 208486292
DEQP_APK_CRC = 0xF8F5905F
MUSTPASS = CTS_DIR + "vk-main-2019-03-01/"
DEQP_PACKAGE = "com.drawelements.deqp"

# The test apps: Android's NDK samples (Apache-2.0), built by the "Graphics test" workflow.
TEST_APPS_ARTIFACT = "graphics-test-apps"
REPOSITORY = "thisisthepy/desktop-subsystem-for-android"


@dataclass(frozen=True)
class App:
    key: str
    apk: str
    package: str
    activity: str
    looks_like: str


GLES_APP = App("gles", "hello-gl2.apk", "com.android.gl2jni", ".GL2JNIActivity",
               "a green triangle on a background that fades between grays")
VULKAN_APP = App("vulkan", "hello-vulkan.apk", "com.android.hellovk", ".VulkanActivity",
                 "a triangle with red, green and blue corners on a dark background")
APPS = (GLES_APP, VULKAN_APP)


# ---------------------------------------------------------------- the Vulkan CTS subset

def read_patterns(text):
    """The patterns of a subset file: one per line, without comments and blank lines."""
    patterns = []
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            patterns.append(line)
    return patterns


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
    return results


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
    patterns = read_patterns(SUBSET.read_text(encoding="utf-8"))
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


def fetch_apps(run_id=None):
    apps = WORK / "apps"
    if all((apps / app.apk).exists() for app in APPS):
        return apps
    if not shutil.which("gh"):
        raise SystemExit(f"download the {TEST_APPS_ARTIFACT} artifact of a successful 'Graphics test' run "
                         f"into {apps}, or install the GitHub CLI (gh)")
    if run_id is None:
        out = subprocess.run(
            ["gh", "run", "list", "--repo", REPOSITORY, "--workflow", "test-graphics.yml",
             "--status", "success", "--limit", "20", "--json", "databaseId"],
            check=True, capture_output=True, text=True).stdout
        runs = json.loads(out)
        if not runs:
            raise SystemExit("no successful 'Graphics test' run to take the test apps from")
        run_id = runs[0]["databaseId"]
    apps.mkdir(parents=True, exist_ok=True)
    subprocess.run(["gh", "run", "download", str(run_id), "--repo", REPOSITORY,
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


class Adb:
    def __init__(self, serial=None):
        if not shutil.which("adb"):
            raise SystemExit("adb is not on PATH (Android SDK platform-tools)")
        serial = serial or DEFAULT_SERIAL
        connect = connect_command(serial)
        if connect:
            result = subprocess.run(connect, capture_output=True, text=True, encoding="utf-8", errors="replace")
            said = (result.stdout + result.stderr).strip()
            if result.returncode != 0 or "connected" not in said or "cannot" in said or "failed" in said:
                raise SystemExit(
                    f"adb cannot connect to {serial} ({said}). Start Android and turn developer mode "
                    "on (dsa settings set developer-mode on); dsa settings shows the adb port.")
        self.base = ["adb", "-s", serial]

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


def run_cts(args):
    adb = Adb(args.serial)
    stack = graphics_stack(adb)
    print(json.dumps(stack, indent=2))
    WORK.mkdir(parents=True, exist_ok=True)
    cases_file = WORK / "vk-cts-subset.cases.txt"
    cases = caselist(cases_file)
    apk = fetch_deqp()
    adb.run("install", "-r", "-g", str(apk))
    remote_cases, remote_log = "/sdcard/dsa-vk-cases.txt", "/sdcard/dsa-vk-results.qpa"
    adb.run("push", str(cases_file), remote_cases)
    adb.shell(f"rm -f {remote_log}", check=False)
    command = (f"deqp --deqp-caselist-file={remote_cases} --deqp-log-filename={remote_log} "
               "--deqp-log-images=disable --deqp-log-shader-sources=disable --deqp-watchdog=enable")
    print(f"running {len(cases)} Vulkan CTS cases")
    adb.shell(f"am start -W -n {DEQP_PACKAGE}/android.app.NativeActivity -e cmdLine '{command}'")
    deadline = time.time() + args.timeout
    results = CtsResults()
    local_log = WORK / "vk-cts-results.qpa"
    while time.time() < deadline:
        time.sleep(5)
        running = adb.shell(f"pidof {DEQP_PACKAGE}", check=False).strip()
        adb.run("pull", remote_log, str(local_log), check=False)
        if local_log.exists():
            results = parse_qpa(local_log.read_text(encoding="utf-8", errors="replace"))
        if results.session_complete or not running:
            break
    missing = [case for case in cases if case not in results.statuses]
    failed = results.failed()
    counts = {}
    for status in results.statuses.values():
        counts[status] = counts.get(status, 0) + 1
    print(json.dumps({"cases": len(cases), "statuses": counts, "log": str(local_log)}, indent=2))
    for case, status in sorted(failed.items()):
        print(f"  {status:12} {case}")
    for case in missing:
        print(f"  {'NotRun':12} {case}")
    ok = not failed and not missing and not check_stack(stack)
    for problem in check_stack(stack):
        print("  " + problem)
    print("Vulkan CTS subset: " + ("passed" if ok else "FAILED"))
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
    apps_dir = fetch_apps(args.run)
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
    apps_dir = fetch_apps(args.run)
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
    for name in ("cts", "apps", "measure"):
        command = sub.add_parser(name)
        command.add_argument("--serial", help=f"the adb serial of the DSA guest (default {DEFAULT_SERIAL}, "
                             "developer mode's loopback port)")
        if name == "cts":
            command.add_argument("--timeout", type=int, default=1800)
    for command in (fetch, sub.choices["apps"], sub.choices["measure"]):
        command.add_argument("--run", type=int, help="the 'Graphics test' run to take the test apps from")
    sub.choices["apps"].add_argument("--yes", action="store_true", help="skip the questions about what each window shows")
    sub.choices["measure"].add_argument("--seconds", type=int, default=30)
    args = parser.parse_args(argv)

    if args.command == "check-subset":
        WORK.mkdir(parents=True, exist_ok=True)
        cases = caselist(WORK / "vk-cts-subset.cases.txt")
        print(f"the subset selects {len(cases)} cases of the pinned CTS")
        return 0
    if args.command == "fetch":
        WORK.mkdir(parents=True, exist_ok=True)
        print(fetch_deqp())
        print(fetch_apps(args.run))
        return 0
    return {"cts": run_cts, "apps": run_apps, "measure": run_measure}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
