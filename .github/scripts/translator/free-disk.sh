#!/usr/bin/env bash
# Makes room on a GitHub-hosted Ubuntu runner for the translator build, or another AOSP
# build from a reduced tree (the framework build), and picks where the tree goes. The
# runner's image carries SDKs and toolchains the build never uses (the build brings its own
# Clang, JDK and Go), so they go. It prints the free space before and after, adds swap for the
# compiler's peaks, and writes DSA_TRANSLATOR_WORK (a folder on the disk with the most room,
# named after the build: free-disk.sh [name], dsa-translator by default) to $GITHUB_ENV.
set -euo pipefail
name="${1:-dsa-translator}"

echo "::group::Disk before"
df -h / /mnt 2>/dev/null || df -h /
echo "::endgroup::"

sudo rm -rf \
  /usr/share/dotnet /usr/local/lib/android /opt/ghc /usr/local/.ghcup \
  /opt/hostedtoolcache /usr/local/share/boost /usr/share/swift /usr/local/share/powershell \
  /usr/local/share/chromium /usr/local/lib/node_modules /opt/az /opt/microsoft /opt/google \
  /usr/lib/google-cloud-sdk /usr/share/miniconda /usr/local/julia* /usr/lib/jvm \
  /opt/pipx /usr/local/aws-cli /usr/local/aws-sam-cli /usr/share/az_* \
  || true
if command -v docker >/dev/null; then
  sudo docker system prune -af >/dev/null 2>&1 || true
fi
sudo apt-get clean

free_kib() { df -k --output=avail "$1" 2>/dev/null | tail -1 | tr -d ' ' || echo 0; }
root_free="$(free_kib /)"
mnt_free=0
if mountpoint -q /mnt; then
  mnt_free="$(free_kib /mnt)"
fi

# The tree and its output go on the disk with more room; swap goes on the other one when
# there is one, so that the two never compete.
if (( mnt_free > root_free )); then
  work="/mnt/$name"
  swap_dir="$HOME"
else
  work="$HOME/$name"
  swap_dir=/mnt
  (( mnt_free > 0 )) || swap_dir="$HOME"
fi
sudo mkdir -p "$work"
sudo chown "$(id -u):$(id -g)" "$work"

# 16 GiB of RAM is not always enough when four translator files compile at once.
if ! swapon --show=NAME --noheadings | grep -q dsa-swap; then
  sudo fallocate -l 12G "$swap_dir/dsa-swap" 2>/dev/null || sudo dd if=/dev/zero of="$swap_dir/dsa-swap" bs=1M count=12288 status=none
  sudo chmod 600 "$swap_dir/dsa-swap"
  sudo mkswap "$swap_dir/dsa-swap" >/dev/null
  sudo swapon "$swap_dir/dsa-swap"
fi

# The build's genrule and kati sandboxes run in user namespaces, which Ubuntu 24.04 keeps
# from unprivileged processes by default.
sudo sysctl -q -w kernel.apparmor_restrict_unprivileged_userns=0 || true

echo "::group::Disk after"
df -h / /mnt 2>/dev/null || df -h /
free -h
echo "::endgroup::"
echo "the tree goes in $work"
if [[ -n "${GITHUB_ENV:-}" ]]; then
  echo "DSA_TRANSLATOR_WORK=$work" >> "$GITHUB_ENV"
fi
