#!/usr/bin/env bash
# Builds services.jar with DSA's patches (the window manager, package manager and WebView
# service changes in dsa/guest/framework/patches/frameworks/base), in a tree fetch-source.py made
# from the services lock (framework_lock.py services-lock): every project the published build's
# manifest syncs by default, each at the manifest's commit.
#
#   build-services.sh <tree> <android16|android17> <out folder> [<apps folder>]
#
# DSA_TREE_PART chooses what one runner builds from the tree: "services" (services.jar and
# DocumentsUI), "screens" (the system screens alone, no services.jar), or, unset, everything.
#
# With <apps folder>, the same tree also builds every app dsa/apps/apps.json builds in the tree
# (DocumentsUI and the system screens: Settings, PermissionController, PackageInstaller and the
# share sheet), with DSA's patches, and leaves each there as dsa/apps/apps.py records an app.
#
# The Java framework reaches across most of the platform, so unlike build.sh this builds in the
# whole tree, with the published build's own product (the lock's target) and release
# configuration, and so makes the services.jar that build made, with the patches. No dexpreopt
# (make-image.py compiles the jar itself against the guest's boot image, as it compiles the
# apps), so no boot image and no dex2oat.
#
# Only the dex jar is built, not the install path. Installing services.jar also installs what
# the module requires (its JNI libraries, and through them native and Rust code), which is
# thousands of compile steps and more disk than a standard runner has. So the build analyses the
# tree first (the goal `nothing`), asks ninja which file the install rule copies, and builds that
# file alone: the javac, R8 and d8 steps and the host tools they run. The jar goes to <out folder>
# under its target_files name (framework_lock.py java-outputs says where the build installs it).
#
# The swap Soong's analysis of the whole tree needs (DSA_ANALYSIS_SWAP, a swap file the workflow
# made) is given back to the disk once the analysis is done, as the Java build does not need it.
# DSA_BUILD_MINUTES (280 by default) bounds the whole script, so that a build that runs out of
# time says so before the job's limit stops it without a word. A failure says whether it was the
# disk, the memory or the time.
#
# The analysis is the step that did not fit. Android 16's takes 9 minutes on a standard runner;
# Android 17's printed nothing for 58 minutes and then GitHub took the runner away (run
# 43644988414), with no figure in the log to say why. Two things were found in the pinned
# build/soong, and the script answers both:
#   - Android 17's release configuration turns Soong's incremental analysis on
#     (RELEASE_SOONG_INCREMENTAL_ANALYSIS; the build's banner says SOONG_INCREMENTAL_ANALYSIS=true
#     and soong_build runs with --incremental-build-actions). Beside the ninja files, the
#     analysis then hashes and serialises every module's providers and writes them, and a second
#     copy of every module's ninja statements, to key-value stores under out/soong, so that a
#     later analysis of the same tree can skip the modules that did not change. This build
#     analyses a fresh tree once and throws it away, so that work buys nothing and costs memory
#     and disk the runner does not have. SOONG_INCREMENTAL_ANALYSIS=false turns it off; Android
#     16's Soong does not read the variable and never had it on.
#   - GOMEMLIMIT never reached soong_build: soong_ui starts it with an empty environment
#     (`env -i`), so its heap grew to twice what is live before Go collected it, in both
#     versions. framework_lock.py soong-memory-limit changes soong_ui's source in the tree to
#     hand the variable on.
# So that the next failure says why, a line a minute gives the memory, the swap, the pressure,
# the disk and the largest processes while the build runs, and the analysis is stopped after
# DSA_ANALYSIS_MINUTES (90 by default), within the time the job gives the build.
#
# The runner has 4 processors and 16 GiB, and its own agent must keep answering GitHub while the
# build runs: when the build took every processor and swapped hard, the agent went silent and
# GitHub gave the job up. So the build leaves it room:
#   - Its own memory cgroup (DSA_BUILD_CGROUP, which the workflow makes with a memory.high 2.5
#     GiB under the runner's memory). android17's analysis holds 19 to 20 GiB at its peak, so it
#     swaps however it runs; in the cgroup it swaps its own pages, and the agent's stay in
#     memory. Before, the two shared one cgroup, the agent had 0.5 GiB available for most of the
#     analysis, and android17 lost its runner in about half its attempts.
#   - DSA_SERVICES_JOBS (2 by default) ninja jobs, not one per processor. The Java steps are the
#     large ones: javac runs with a heap of up to 4 GiB, and R8 and metalava, which Soong puts
#     in its high memory pool, with as much or more, so four at once ask for more than the
#     runner has before any of it swaps. Two leave about 8 GiB for the rest.
#   - One high memory job at a time (NINJA_HIGHMEM_NUM_JOBS=1), whatever the job count, so R8
#     and metalava never run beside each other.
#   - GOMEMLIMIT=10GiB for Soong's analysis, which is Go: by default its heap grows to twice what
#     is live before it collects, and the limit makes it collect sooner instead of growing into
#     swap. Of the build's cgroup it leaves about 3 GiB for the page cache the ninja files are
#     written through and the compressed swap pool. Should more than the limit be live (android17's
#     analysis reaches about twice the limit), Go spends at most half its time collecting
#     and lets the heap pass the limit, so the limit slows the analysis and never stops it.
#   - The lowest processor and disk priority (nice 19, ionice best effort 7), so the agent runs
#     first whenever it needs to.
#   - The highest OOM score (choom 1000): when memory runs out, the kernel stops the build, which
#     explain() then reports, and not the agent, which the workflow guards with -1000.
#   - --skip-soong-tests: Soong's own Go tests are not built and run before the analysis.
set -euo pipefail

top="$(cd "$1" && pwd)"
android="$2"
out="$3"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
lock="$repo/dsa/guest/framework/framework-lock.json"
minutes="${DSA_BUILD_MINUTES:-280}"
deadline=$(( SECONDS + minutes * 60 ))
jobs="${DSA_SERVICES_JOBS:-2}"
analysis_minutes="${DSA_ANALYSIS_MINUTES:-90}"
# A bound on the next build step alone, in minutes (0: only the script's own), and what a step
# stopped for its time was given.
step_minutes=0
stopped_after="the $minutes minutes the job gives it (DSA_BUILD_MINUTES)"
stop_at="$deadline"

target="$(jq -r --arg a "$android" '.versions[] | select(.android == $a) | .target' "$lock")"
release="$(jq -r --arg a "$android" '.versions[] | select(.android == $a) | .release' "$lock")"
[[ -n "$target" && "$target" != null && -n "$release" && "$release" != null ]] \
  || { echo "the lock has no $android" >&2; exit 1; }
product="${target%-userdebug}"

room() {
  echo "::group::Disk and memory: $1"
  df -h / /mnt 2>/dev/null || df -h /
  free -h
  swapon --show || true
  if [[ -n "${DSA_BUILD_CGROUP:-}" ]]; then
    echo "the build's cgroup ($DSA_BUILD_CGROUP): $(cat "$DSA_BUILD_CGROUP/memory.current" 2>/dev/null || echo ?) bytes of $(cat "$DSA_BUILD_CGROUP/memory.high" 2>/dev/null || echo ?)"
    cat "$DSA_BUILD_CGROUP/memory.events" "$DSA_BUILD_CGROUP/memory.pressure" 2>/dev/null || true
  fi
  echo "::endgroup::"
}

# The build's cgroup and the runner's side of it: what the build holds of its memory.high, how
# many times it went over and was held back, and the share of the last 10 seconds some task
# (and every task) of the runner's own cgroup, the one this script runs in, waited for memory.
# Empty when the build has no cgroup of its own.
cgroup_line() {
  [[ -n "${DSA_BUILD_CGROUP:-}" && -r "$DSA_BUILD_CGROUP/memory.current" ]] || return 0
  local current high over runner_cgroup runner_waits
  current="$(cat "$DSA_BUILD_CGROUP/memory.current")"
  high="$(cat "$DSA_BUILD_CGROUP/memory.high")"
  over="$(awk '$1 == "high" { print $2 }' "$DSA_BUILD_CGROUP/memory.events" 2>/dev/null || true)"
  runner_cgroup="/sys/fs/cgroup$(cut -d: -f3- /proc/self/cgroup 2>/dev/null || true)"
  runner_waits="$(awk '{ split($2, avg, "="); share[$1] = avg[2] }
    END { printf "%s/%s %%", share["some"], share["full"] }' "$runner_cgroup/memory.pressure" 2>/dev/null || echo "?")"
  awk -v current="$current" -v high="$high" -v over="${over:-?}" -v waits="$runner_waits" 'BEGIN {
    printf "; the build holds %.1f of its %.1f GiB (held back %s times), the runner waits for memory %s",
      current / 1073741824, high / 1073741824, over, waits }'
}

# One line of what the build has of the runner now: the memory still available, the swap in use
# and how much of it zswap holds compressed in memory, the pages waiting to be written, the
# share of the last 10 seconds some task (and every task) waited for memory and for the disk,
# the disk left, the load, the three largest processes with what each has in memory and in
# swap, and cgroup_line.
room_line() {
  local memory pressure disk largest
  memory="$(awk '
    /^MemTotal:/ { total = $2 } /^MemAvailable:/ { available = $2 }
    /^SwapTotal:/ { swap = $2 } /^SwapFree:/ { swap_free = $2 }
    /^Zswap:/ { pool = $2 } /^Zswapped:/ { held = $2 } /^Dirty:/ { dirty = $2 }
    END { printf "memory %.1f of %.1f GiB available, swap %.1f of %.1f GiB used (zswap holds %.1f GiB in %.1f), %.1f GiB to write",
          available / 1048576, total / 1048576, (swap - swap_free) / 1048576, swap / 1048576,
          held / 1048576, pool / 1048576, dirty / 1048576 }' /proc/meminfo 2>/dev/null || true)"
  pressure="$(for what in memory io; do
    awk -v what="$what" '{ split($2, avg, "="); share[$1] = avg[2] }
      END { printf "%s %s/%s %% ", what, share["some"], share["full"] }' "/proc/pressure/$what" 2>/dev/null || true
  done)"
  disk="$(df -BM --output=avail "$top" 2>/dev/null | tail -1 | tr -d ' M' || true)"
  largest="$(ps -eo rss=,pid=,comm= --sort=-rss 2>/dev/null | head -3 | awk '{
      swapped = 0; status = "/proc/" $2 "/status"
      while ((getline line < status) > 0) if (line ~ /^VmSwap:/) { split(line, field, " "); swapped = field[2] }
      close(status)
      printf "%s%s %.1f GiB (and %.1f in swap)", (NR > 1 ? ", " : ""), $3, $1 / 1048576, swapped / 1048576
    }' || true)"
  echo "[room $(date -u +%H:%M:%S)] $memory; waiting for ${pressure% }; disk ${disk:-?} MiB free; load $(cut -d' ' -f1 /proc/loadavg); largest: $largest$(cgroup_line || true)"
}

# room_line every DSA_ROOM_SECONDS (60 by default) until the script ends. It runs at the
# shell's own priority, not the build's, so it still prints when the build has the runner busy.
watch_room() {
  local every="${DSA_ROOM_SECONDS:-60}" nap=
  trap '[[ -z "$nap" ]] || kill "$nap" 2>/dev/null; exit 0' TERM
  while true; do
    sleep "$every" &
    nap=$!
    wait "$nap" || true
    room_line || true
  done
}

# Says why a build step failed: the disk, the memory, the time, or the build itself.
explain() {
  local status="$1" what="$2" avail logs
  avail="$(df -k --output=avail "$top" | tail -1 | tr -d ' ')"
  logs="$(cat out/error.log out/soong.log 2>/dev/null || true)"
  room "after the failed $what"
  if (( status == 124 || status == 137 && SECONDS >= stop_at )); then
    echo "::error::$what ran out of time: it was stopped after $stopped_after; the [room] lines above say what it was short of"
  elif (( avail < 2 * 1024 * 1024 )) || grep -q "No space left on device" <<< "$logs"; then
    echo "::error::$what ran out of disk: $(( avail / 1024 )) MiB left on the tree's disk"
  elif sudo dmesg 2>/dev/null | grep -qiE "out of memory|oom-kill|killed process" \
      || grep -qE "OutOfMemoryError|signal: killed|fatal error: runtime: out of memory" <<< "$logs"; then
    echo "::error::$what ran out of memory (the kernel's OOM killer or Java's heap; see free above)"
    sudo dmesg 2>/dev/null | grep -iE "out of memory|oom-kill|killed process" | tail -5 || true
  else
    echo "::error::$what failed (status $status), with disk and memory to spare; the errors are above"
  fi
  echo "::group::What the build could not find"
  grep -hoE "(depends on undefined module|missing dependencies?:|module source path|unrecognized module type|no such file or directory|Cannot find|not found)[^\"]{0,240}" \
    <<< "$logs" | sort | uniq -c | sort -rn | head -80 || true
  echo "::endgroup::"
  tail -n 120 out/error.log 2>/dev/null || true
  exit "$status"
}

# Runs its arguments in the build's cgroup (DSA_BUILD_CGROUP) when the workflow made one: a
# subshell moves itself there and becomes the command, so the command and everything it starts
# are held to the cgroup's memory.high, and the script around it is not.
in_build_cgroup() {
  if [[ -z "${DSA_BUILD_CGROUP:-}" ]]; then
    "$@"
    return
  fi
  (
    # Read here, not in the pipeline: each side of a pipeline is a process of its own.
    self=$BASHPID
    echo "$self" | sudo tee "$DSA_BUILD_CGROUP/cgroup.procs" >/dev/null \
      || { echo "::error::the build could not join its cgroup ($DSA_BUILD_CGROUP), so it would take the runner's memory"; exit 1; }
    exec "$@"
  )
}

# soong_ui, as m runs it, within what is left of the time, with the room the notes above give the
# runner. choom, nice and ionice each run the next command in their own process, so timeout's
# signal reaches soong_ui. step_minutes, when set, bounds this one step sooner.
build() {
  local what="$1" left status=0
  shift
  left=$(( deadline - SECONDS ))
  stopped_after="the $minutes minutes the job gives it (DSA_BUILD_MINUTES)"
  if (( step_minutes > 0 && step_minutes * 60 < left )); then
    left=$(( step_minutes * 60 ))
    stopped_after="the $step_minutes minutes this step is given"
  fi
  stop_at=$(( SECONDS + left ))
  (( left > 60 )) || { explain 124 "$what"; }
  in_build_cgroup timeout --signal=TERM --kill-after=120 "$left" \
    choom -n 1000 -- nice -n 19 ionice -c 2 -n 7 \
    build/soong/soong_ui.bash --make-mode --skip-soong-tests -j"$jobs" "$@" || status=$?
  (( status == 0 )) || explain "$status" "$what"
}

# Every patch, the ones to Java sources included: the whole tree has every file they change.
uv run "$repo/dsa/guest/framework/apply-patches.py" --android "$android" --top "$top"
# soong_ui is built from this source when the build starts, so the change is in the first run.
uv run "$repo/dsa/guest/framework/framework_lock.py" soong-memory-limit "$top/build/soong/ui/build/soong.go"
# The apps built in the tree (DocumentsUI and the system screens) build only in an Android
# tree, and this one is the published build's own. Their design files, their patches (and
# SettingsLib's, which Settings and PermissionController build from, laid over once before them)
# and the flag value that turns DocumentsUI's Material 3 look on go in before the analysis: they
# add files that the analysis globs, and changing them afterwards would make the next build
# analyse the whole tree again. DSA_SERVICES_LOCK, when set, is the lock the tree was fetched
# from, which says the tree has each at the commit dsa/apps/apps.json pins.
apps_out="${4:-}"
tree_part="${DSA_TREE_PART:-}"
part_args=()
[[ -z "$tree_part" ]] || part_args=(--part "$tree_part")
tree_apps=()
if [[ -n "$apps_out" ]]; then
  lock_args=()
  [[ -z "${DSA_SERVICES_LOCK:-}" ]] || lock_args=(--lock "$DSA_SERVICES_LOCK")
  for part in $(uv run "$repo/dsa/apps/apps.py" tree-order "${part_args[@]}" | jq -r '.[]'); do
    uv run "$repo/dsa/apps/apps.py" patch-tree "$part" --top "$top" --android "$android" \
      --release "$release" "${lock_args[@]}"
  done
  mapfile -t tree_apps < <(uv run "$repo/dsa/apps/apps.py" tree-apps "${part_args[@]}" | jq -r '.[]')
fi

cd "$top"
export ALLOW_MISSING_DEPENDENCIES=true
export SKIP_ABI_CHECKS=true
export DISABLE_ARTIFACT_PATH_REQUIREMENTS=true
export BUILD_USERNAME=dsa-framework BUILD_HOSTNAME=github-actions
# make-image.py compiles the jar against the guest's own boot image; the build makes no odex.
export WITH_DEXPREOPT=false
# Android 17 runs siso by default; ninja is in build-tools.
export SOONG_NINJA=ninja
# The memory limits of the notes at the top.
export NINJA_HIGHMEM_NUM_JOBS=1
export GOMEMLIMIT=10GiB
# One analysis of a fresh tree: nothing would ever read the caches an incremental analysis keeps.
export SOONG_INCREMENTAL_ANALYSIS=false
echo "building with $jobs ninja jobs, $NINJA_HIGHMEM_NUM_JOBS high memory job, GOMEMLIMIT=$GOMEMLIMIT, SOONG_INCREMENTAL_ANALYSIS=$SOONG_INCREMENTAL_ANALYSIS"

set +u
# shellcheck disable=SC1091
source build/envsetup.sh
lunch "$product-$release-userdebug"
set -u
product_out="$(get_build_var PRODUCT_OUT)"
room "before the analysis"

watch_room &
watcher=$!
trap 'kill "$watcher" 2>/dev/null || true' EXIT

# The analysis of the whole tree, and the ninja files, with nothing built.
analysis_start=$SECONDS
step_minutes="$analysis_minutes"
build "the analysis of the tree" nothing
step_minutes=0
echo "analysed the tree in $(( (SECONDS - analysis_start) / 60 )) minutes"
room "after the analysis"
du -sh out/soong/*.ninja 2>/dev/null | sort -rh | head -5 || true
# How soong_build was started, from the rule soong_ui wrote for it: with the memory limit, and
# without the incremental analysis.
bootstrap=out/soong/bootstrap.ninja
if [[ -f "$bootstrap" ]]; then
  grep -q "GOMEMLIMIT=$GOMEMLIMIT" "$bootstrap" \
    || echo "::warning::soong_build ran without GOMEMLIMIT: $bootstrap does not name it"
  ! grep -q -- "--incremental-build-actions" "$bootstrap" \
    || echo "::warning::soong_build ran its incremental analysis although SOONG_INCREMENTAL_ANALYSIS is false"
fi

ninja_file="$(ls -t out/combined-*.ninja 2>/dev/null | head -1 || true)"
[[ -n "$ninja_file" ]] || ninja_file="out/soong/build.$TARGET_PRODUCT.ninja"
[[ -f "$ninja_file" ]] || { echo "::error::the analysis wrote no ninja file (looked for out/combined-*.ninja and $ninja_file)"; exit 1; }
ninja=prebuilts/build-tools/linux-x86/bin/ninja

# What each install rule copies: the module's own output, which the install path is a copy of.
outputs=""
[[ "$tree_part" == screens ]] || outputs="$(uv run "$repo/dsa/guest/framework/framework_lock.py" java-outputs)"
names=()
jars=()
while read -r name installed; do
  [[ -n "$name" ]] || continue
  query="$("$ninja" -f "$ninja_file" -t query "$product_out/$installed")" \
    || { echo "::error::ninja knows no rule for $product_out/$installed"; exit 1; }
  if ! jar="$(uv run "$repo/dsa/guest/framework/framework_lock.py" install-source <<< "$query")"; then
    head -40 <<< "$query"
    echo "::error::$installed: the build cannot make the module's own jar in place of the install path"
    exit 1
  fi
  echo "$installed is a copy of $jar"
  names+=("$name")
  jars+=("$jar")
done <<< "$outputs"

# Each tree app's APK, which its install path is a copy of, the same way: the module's own APK,
# signed with the tree's platform test key (make-image.py signs it again with the image's). An
# app inside an APEX (PermissionController) has no install path of its own, so its APK is the
# one ninja target its apk_pattern in apps.json names.
tree_apks=()
targets_file=""
for app in "${tree_apps[@]}"; do
  installed="$(jq -r --arg a "$app" '.apps[] | select(.name == $a) | .build.installed // empty' "$repo/dsa/apps/apps.json")"
  if [[ -z "$installed" ]]; then
    if [[ -z "$targets_file" ]]; then
      targets_file="$(mktemp)"
      "$ninja" -f "$ninja_file" -t targets all > "$targets_file"
    fi
    apk="$(uv run "$repo/dsa/apps/apps.py" apk-target "$app" < "$targets_file")" \
      || { echo "::error::$app: the build has no APK target its apk_pattern names"; exit 1; }
    echo "$app is built as $apk"
  else
    query="$("$ninja" -f "$ninja_file" -t query "$product_out/$installed")" \
      || { echo "::error::ninja knows no rule for $product_out/$installed"; exit 1; }
    if ! apk="$(uv run "$repo/dsa/guest/framework/framework_lock.py" install-source --apk <<< "$query")"; then
      head -40 <<< "$query"
      echo "::error::$installed: the build cannot make $app's own APK in place of the install path"
      exit 1
    fi
    echo "$installed is a copy of $apk"
  fi
  tree_apks+=("$apk")
done

# The Java build needs no more than the runner's own memory and the first swap file.
if [[ -n "${DSA_ANALYSIS_SWAP:-}" && -f "$DSA_ANALYSIS_SWAP" ]]; then
  sudo swapoff "$DSA_ANALYSIS_SWAP"
  sudo rm -f "$DSA_ANALYSIS_SWAP"
  echo "gave the analysis swap ($DSA_ANALYSIS_SWAP) back to the disk"
fi
room "before the build"

if (( ${#jars[@]} )); then
  echo "building ${jars[*]} for $product with release configuration $release"
  build "the services.jar build" "${jars[@]}"
fi
room "after the build"

mkdir -p "$out"
for i in "${!names[@]}"; do
  mkdir -p "$out/$(dirname "${names[$i]}")"
  cp "${jars[$i]}" "$out/${names[$i]}"
  ls -l "$out/${names[$i]}"
done
cp "$top/.dsa-patches.json" "$out/services-patches.json"

# After services.jar is in its folder, so that an app's failure still leaves the jar for the
# workflow to keep. One app at a time, so a failure names the app.
for i in "${!tree_apps[@]}"; do
  build "the ${tree_apps[$i]} build" "${tree_apks[$i]}"
  room "after the ${tree_apps[$i]} build"
  uv run "$repo/dsa/apps/apps.py" record "${tree_apps[$i]}" --apk "${tree_apks[$i]}" --android "$android" --out "$apps_out"
done
[[ -z "$apps_out" ]] || ls -l "$apps_out"
