#!/usr/bin/env bash
# Builds the upstream binder driver as an external module against one WSL kernel tag and
# lists the kernel symbols it needs that the kernel does not export to modules.
#
#   experiments/wsl-binder-module/check-symbols.sh [tag]
#
# Everything it downloads or builds goes under .scratch/wsl-binder-module/<tag>/. It runs
# `make modules_prepare` only, never a full kernel build, so it takes about a minute after
# the fetch (about 2 GB).
#
# Build dependencies are those of the WSL kernel README: gcc, make, flex, bison, bc,
# libelf headers, openssl headers. If they live somewhere other than the system paths,
# point TOOLS_ROOT at an extracted root (with usr/bin, usr/include, usr/lib/<triplet>).
set -euo pipefail

tag="${1:-linux-msft-wsl-6.18.54.1}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$repo_root/.scratch/wsl-binder-module/$tag"
src="$work/src"
mod="$work/binder"

if [[ -n "${TOOLS_ROOT:-}" ]]; then
    triplet="$(gcc -dumpmachine)"
    export PATH="$TOOLS_ROOT/usr/bin:$PATH"
    export LD_LIBRARY_PATH="$TOOLS_ROOT/usr/lib/$triplet${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    host_flags=(HOSTCFLAGS="-I$TOOLS_ROOT/usr/include" HOSTLDFLAGS="-L$TOOLS_ROOT/usr/lib/$triplet")
else
    host_flags=()
fi

if [[ ! -f "$src/Makefile" ]]; then
    mkdir -p "$src"
    git -C "$src" init -q
    git -C "$src" fetch -q --depth 1 https://github.com/microsoft/WSL2-Linux-Kernel tag "$tag"
    git -C "$src" checkout -q FETCH_HEAD
fi

cp "$src/Microsoft/config-wsl" "$src/.config"
# LOCALVERSION= stops setlocalversion from adding "+" for a git checkout, so the release
# string (and with it vermagic) is exactly the one WSL ships.
make -s -C "$src" -j"$(nproc)" "${host_flags[@]}" LOCALVERSION= olddefconfig modules_prepare >&2
echo "kernel release: $(cat "$src/include/config/kernel.release")" >&2

echo "binder in the shipped configuration:" >&2
grep -E 'CONFIG_ANDROID_BINDER(FS|_IPC)[ =]' "$src/Microsoft/config-wsl" >&2 || true

# The upstream Kconfig makes binder a bool, so it cannot be selected as a module. Build
# the same sources out of tree, defining the options the build would have set.
rm -rf "$mod"
mkdir -p "$mod"
cp "$src"/drivers/android/*.[ch] "$mod/"
rm -f "$mod"/*selftest*.c
# The file set changes between kernel versions (binder_netlink.c is new in 6.18).
objs="$(cd "$mod" && ls *.c | sed "s/\\.c$/.o/" | tr "\\n" " ")"
cat > "$mod/Kbuild" <<KBUILD
ccflags-y += -I\$(src) -DCONFIG_ANDROID_BINDER_IPC=1 -DCONFIG_ANDROID_BINDERFS=1 -DCONFIG_ANDROID_BINDER_DEVICES=\\"\\"
obj-m := binder_linux.o
binder_linux-y := $objs
KBUILD
# Without the full build there is no Module.symvers, so modpost cannot resolve anything;
# KBUILD_MODPOST_WARN keeps it going and the check below works from the sources instead.
make -s -C "$src" -j"$(nproc)" LOCALVERSION= M="$mod" KBUILD_MODPOST_WARN=1 modules >/dev/null 2>&1 || true
[[ -f "$mod/binder_linux.o" ]] || { echo "the binder sources did not compile" >&2; exit 2; }

nm -u "$mod/binder_linux.o" | awk '{print $2}' | sort -u > "$work/needed.txt"

# A symbol counts as exported if any export macro names it. The macro families that build
# the name (per-CPU, tracepoints, static calls, generated helpers) are matched by name.
missing=()
while read -r sym; do
    case "$sym" in
        __this_module|__x86_indirect_thunk_*|__x86_return_thunk|__fentry__) continue ;;
        __SCT__*|__SCK__*|__tracepoint_*|__traceiter_*|bpf_trace_run*|param_*) continue ;;
    esac
    if git -C "$src" grep -qE "EXPORT[A-Z_]*\(\s*${sym}\s*[,)]" -- '*.c' '*.h' '*.S'; then
        continue
    fi
    missing+=("$sym")
done < "$work/needed.txt"

echo "needed by binder: $(wc -l < "$work/needed.txt") symbols" >&2
echo "not exported by $tag: ${#missing[@]}" >&2
printf '%s\n' "${missing[@]}"
