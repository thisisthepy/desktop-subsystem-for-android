"""Lays out each click's input trace on the host clock.

    uv run scripts/input-trace.py [--no-guest] [log files or folders ...]

With no arguments it reads the host logs (%LOCALAPPDATA%\\DSA\\logs\\*.log), the service's logs
(the logs folder of the machine's DSA folder, %ProgramData%\\DSA\\logs\\*.log unless another data
folder was chosen at install), and Android's log, which holds the guest's stages: it runs
`dsa logcat -d -b all`, which prints what Android's log holds now. Android keeps its log in
memory only, so take the trace soon after the clicks. `--no-guest` skips Android's log; a file
saved from `dsa logcat -d` can be named like any log file.

The trace is a setting: `dsa settings set input-trace on`, run as an administrator, turns it on
for every DSA process that starts afterwards (`DSA_INPUT_TRACE=1` does it for one process).

Every process on a click's path writes `input-trace stage=... key=value ...` lines. Host
lines carry `host=`, milliseconds on the host's monotonic clock, which every host process
shares. Guest lines carry `mono=`, milliseconds on the guest's CLOCK_MONOTONIC. While the
trace is on, the service pairs the two clocks once a second and logs `stage=clock` lines
with `offset=` (guest minus host). Each guest time is moved to the host clock with the
pairing of the smallest round trip near it, so every stage of a click is on one clock.

For each press and each release (`app.pointer`), it prints every stage that follows on the
same task or app until the next press or release, at most `--window` ms, in order on the host
clock, with the gap to the stage before:

    app.pointer, agent.inject, app.frame (main and render thread stages), sf.frame,
    hwc.frame, wm.frame, app.present, app.shown

Times a stage is placed at: `app.frame` at its intended vsync, `sf.frame` when SurfaceFlinger
presented it (`actual`), `app.shown` at the vertical blank it was shown at (`vblank`), and
every other stage when it was logged.
"""

from __future__ import annotations

import argparse
import glob
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field

TAG = "input-trace"

# The order stages are listed in when they fall at the same time.
STAGES = [
    "app.pointer",
    "agent.inject",
    "app.frame",
    "sf.frame",
    "hwc.frame",
    "wm.frame",
    "app.present",
    "app.shown",
]

# How far from an event, in seconds of wall clock, a clock pairing may be and still be used.
CLOCK_REACH_S = 30.0


@dataclass
class Line:
    stage: str
    fields: dict[str, str]
    source: str
    # On the host clock, in milliseconds, once placed.
    host: float | None = None
    extra: dict[str, str] = field(default_factory=dict)

    @property
    def task(self) -> str | None:
        return self.fields.get("task")

    @property
    def utc_s(self) -> float | None:
        return utc_seconds(self.fields.get("utc"))


def utc_seconds(stamp: str | None) -> float | None:
    """`2026-10-26T06:23:03.123Z` as seconds since the epoch, or None."""
    if not stamp:
        return None
    from datetime import datetime, timezone

    try:
        parsed = datetime.strptime(stamp, "%Y-%m-%dT%H:%M:%S.%fZ")
    except ValueError:
        return None
    return parsed.replace(tzinfo=timezone.utc).timestamp()


def parse_line(text: str, source: str = "") -> Line | None:
    """The trace line in one log line, whatever the log put before it."""
    start = text.find(TAG + " stage=")
    if start < 0:
        return None
    fields: dict[str, str] = {}
    for token in text[start + len(TAG) :].split():
        key, sep, value = token.partition("=")
        if sep:
            fields[key] = value
    stage = fields.pop("stage", None)
    if not stage:
        return None
    return Line(stage, fields, source)


def number(fields: dict[str, str], key: str) -> float | None:
    try:
        return float(fields[key])
    except (KeyError, ValueError):
        return None


@dataclass
class Pairing:
    utc_s: float
    guest: float
    rtt: float
    offset: float


class Clock:
    """The service's clock pairings, to move guest times to the host clock."""

    def __init__(self, lines: list[Line]):
        self.pairings: list[Pairing] = []
        for line in lines:
            if line.stage != "clock":
                continue
            values = [number(line.fields, k) for k in ("guest", "rtt", "offset")]
            utc = line.utc_s
            if utc is None or any(v is None for v in values):
                continue
            guest, rtt, offset = values
            self.pairings.append(Pairing(utc, guest, rtt, offset))

    def offset_near(self, utc_s: float | None, guest: float) -> float | None:
        """The offset of the pairing with the smallest round trip near the event: within
        CLOCK_REACH_S of its wall clock time, which also keeps a later boot's pairings (whose
        guest clock started again) away from an earlier boot's events."""
        near = [
            p
            for p in self.pairings
            if utc_s is None or abs(p.utc_s - utc_s) <= CLOCK_REACH_S
        ]
        if utc_s is None:
            near = [p for p in near if abs(p.guest - guest) <= CLOCK_REACH_S * 1000]
        if not near:
            return None
        return min(near, key=lambda p: p.rtt).offset

    def to_host(self, line: Line, guest: float) -> float | None:
        offset = self.offset_near(line.utc_s, guest)
        return None if offset is None else guest - offset


def place(lines: list[Line], clock: Clock) -> list[Line]:
    """Puts each stage line on the host clock. Lines that cannot be placed are left out."""
    placed = []
    for line in lines:
        f = line.fields
        if line.stage == "clock" or line.stage not in STAGES:
            continue
        if line.stage == "app.shown":
            line.host = number(f, "vblank")
        elif "host" in f:
            line.host = number(f, "host")
        else:
            # A guest line: its own event time on the guest clock.
            key = "actual" if line.stage == "sf.frame" else "mono"
            guest = number(f, key)
            if guest is None:
                continue
            line.host = clock.to_host(line, guest)
        if line.host is not None:
            placed.append(line)
    return placed


def is_press(line: Line) -> bool | None:
    """Whether an app.pointer line is a press (True) or a release (False)."""
    f = line.fields
    if "mouse_down" in f:
        return f["mouse_down"] == "true"
    phase = f.get("phase", "")
    if phase == "Down":
        return True
    if phase == "Up":
        return False
    return None


def describe(line: Line) -> str:
    """The fields of a stage worth reading beside it."""
    skip = {"task", "utc", "mono", "host"}
    return " ".join(f"{k}={v}" for k, v in line.fields.items() if k not in skip)


@dataclass
class Event:
    pointer: Line
    press: bool
    stages: list[Line]


def events(lines: list[Line], window_ms: float) -> list[Event]:
    """Each press and release, with the stages that follow it on its task or its app."""
    placed = sorted(lines, key=lambda l: (l.host, STAGES.index(l.stage)))
    pointers = [l for l in placed if l.stage == "app.pointer" and is_press(l) is not None]
    # app.frame and sf.frame name a package, not a task, so they go with the click whose
    # stretch of time they fall in.
    result = []
    for i, pointer in enumerate(pointers):
        end = pointer.host + window_ms
        if i + 1 < len(pointers):
            end = min(end, pointers[i + 1].host)
        stages = [pointer]
        for line in placed:
            if line is pointer or line.stage == "app.pointer":
                continue
            if not (pointer.host <= line.host < end):
                continue
            if line.task is not None and line.task != pointer.task:
                continue
            stages.append(line)
        result.append(Event(pointer, bool(is_press(pointer)), stages))
    return result


def report(evts: list[Event]) -> str:
    out = []
    for event in evts:
        p = event.pointer
        kind = "press" if event.press else "release"
        out.append(f"{kind} task={p.task} host={p.host:.3f} utc={p.fields.get('utc', '-')}")
        previous = p.host
        for line in event.stages:
            since = line.host - p.host
            gap = line.host - previous
            previous = line.host
            out.append(f"  +{since:8.1f}  gap {gap:7.1f}  {line.stage:<12} {describe(line)}")
        out.append("")
    return "\n".join(out)


def machine_root() -> str:
    """The machine's DSA folder: the data folder the installer recorded, or %ProgramData%\\DSA."""
    try:
        import winreg

        access = winreg.KEY_READ | winreg.KEY_WOW64_64KEY
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"SOFTWARE\thisisthepy\DSA", 0, access) as key:
            folder, _ = winreg.QueryValueEx(key, "DataFolder")
            if folder:
                return str(folder).rstrip("\\")
    except (ImportError, OSError):
        pass
    return os.path.join(os.environ.get("ProgramData", r"C:\ProgramData"), "DSA")


def default_paths() -> list[str]:
    """The user's log folder and the machine's, the two places DSA keeps logs."""
    paths = []
    local = os.environ.get("LOCALAPPDATA")
    if local:
        paths.append(os.path.join(local, "DSA", "logs"))
    paths.append(os.path.join(machine_root(), "logs"))
    return paths


def guest_command() -> list[str]:
    """What prints Android's log as it is now: the DSA command line tool's dump of every buffer."""
    return ["dsa", "logcat", "-d", "-b", "all"]


def guest_lines(run=subprocess.run) -> list[Line]:
    """The trace lines of Android's log, read with `dsa logcat -d`. Android keeps its log in
    memory, so nothing is on disk to read instead."""
    command = guest_command()
    if run is subprocess.run and shutil.which(command[0]) is None:
        print("dsa is not on the PATH, so Android's log is not read", file=sys.stderr)
        return []
    try:
        done = run(command, capture_output=True, text=True, encoding="utf-8", errors="replace")
    except OSError as error:
        print(f"cannot run {' '.join(command)}: {error}", file=sys.stderr)
        return []
    if done.returncode != 0:
        print(f"{' '.join(command)} failed: {done.stderr.strip()}", file=sys.stderr)
    lines = []
    for text in done.stdout.splitlines():
        line = parse_line(text, "dsa logcat")
        if line is not None:
            lines.append(line)
    return lines


def log_files(paths: list[str]) -> list[str]:
    files = []
    for path in paths:
        if os.path.isdir(path):
            files.extend(sorted(glob.glob(os.path.join(path, "*.log"))))
        else:
            files.extend(sorted(glob.glob(path)) or [path])
    return files


def read(files: list[str]) -> list[Line]:
    lines = []
    for name in files:
        try:
            with open(name, encoding="utf-8", errors="replace") as handle:
                for text in handle:
                    line = parse_line(text, name)
                    if line is not None:
                        lines.append(line)
        except OSError as error:
            print(f"cannot read {name}: {error}", file=sys.stderr)
    return lines


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="*", help="log files, folders or patterns")
    parser.add_argument(
        "--no-guest",
        action="store_true",
        help="do not read Android's log with `dsa logcat -d`",
    )
    parser.add_argument(
        "--window",
        type=float,
        default=1500.0,
        help="the longest a click's stages are followed, in ms (default 1500)",
    )
    args = parser.parse_args(argv)
    files = log_files(args.paths or default_paths())
    lines = read(files)
    if not args.paths and not args.no_guest:
        lines += guest_lines()
    clock = Clock(lines)
    if not clock.pairings:
        print(
            "no clock lines: guest stages cannot be placed. Turn the trace on for the guest "
            "(androidboot.hardware.dsa_input_trace=1) as well as for the host.",
            file=sys.stderr,
        )
    placed = place(lines, clock)
    print(report(events(placed, args.window)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
