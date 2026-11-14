#!/usr/bin/env bash
# The image CI boots the Android 17 x86_64 image once per boot check, each check in a job of
# its own and all at once, instead of one check after another in the image's job. Each check
# keeps its test, its time limit and its logs, and a check that skips for want of an image
# fails instead of passing.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-image.yml"
job() { sed -n "/^  $1:\$/,/^  [a-z-]*:\$/p" "$workflow"; }
boot="$(job boot-check)"
test -n "$boot" || { echo "there is no boot-check job"; exit 1; }

# Every check, with the time limit and the logs it had in the image's job.
check() { # test, timeout, artifact, check name
  local block
  block="$(grep -F -A4 -- "- check: $4" <<<"$boot")"
  grep -Fq "test: $1" <<<"$block" || { echo "$4 does not run the $1 check"; exit 1; }
  grep -Fq "timeout: $2" <<<"$block" || { echo "$4 lost its $2 minute limit"; exit 1; }
  grep -Fq "artifact: $3" <<<"$block" || { echo "$4 no longer keeps $3"; exit 1; }
}
check widgets 15 widget-boot-logs "Place a widget on the booted image"
check system_screens 14 system-screens-captures "Check Android's system screens on the booted image"
check media 18 media-check-logs "Play sound and run codecs in an app on the booted image"
check media 23 root-check-logs "Approve and refuse root for an app on the booted image"
check media 18 root-certificate-check-logs "Trust a root certificate the host added in an app on the booted image"
grep -Fq 'filter: fr28_' <<<"$boot" || { echo "the root certificate check does not run alone"; exit 1; }
check overlays 18 overlay-check-logs "Lay the ARM translator and Google Play over the booted system"
check overlays 25 idle-reclaim-measurement "Measure the idle reclaim with Google Play on the booted image"
check overlays 25 memory-measurement "Measure the memory with Google Play and one app window on the booted image"
check dns 14 dns-check-logs "Resolve a name only the host knows on the booted image"
check graphics 28 graphics-check-results "Run the Vulkan CTS subset and the graphics test apps on the booted image"
# A real app's window resized while an accessibility client watches it, on its own runner.
check resize 18 resize-check-measurement "Resize a real app's window on the booted image"
grep -Fq 'summary: /tmp/resize-test-*/resize.md' <<<"$boot" || { echo "the resize check's numbers do not reach the job summary"; exit 1; }
grep -Fq 'timeout-minutes: ${{ matrix.timeout }}' <<<"$boot"
grep -Fq 'fail-fast: false' <<<"$boot"
grep -Fq '"$GITHUB_WORKSPACE/.scratch/boot-checks/tests/${{ matrix.test }}" --nocapture --test-threads 1' <<<"$boot"
grep -Fq "grep -q 'skipped: DSA_'" <<<"$boot"
grep -Fq 'if: always()' <<<"$boot"

# The boot checks wait for the one image they boot and for their programs, not for every image.
grep -Eq 'needs: \[boot-image, boot-checks(, [a-z-]+)*\]' <<<"$boot"
grep -Fq 'steps: &image-steps' "$workflow"
grep -Fq 'steps: *image-steps' <<<"$(job boot-image)"
grep -Fq 'android: ["17"]' <<<"$(job boot-image)"
# The boot checks boot the kernel built from the same commit, so a kernel change (the su
# guard among them) is checked before it ships.
grep -Eq 'needs: \[([a-z-]+, )*boot-kernel(, [a-z-]+)*\]' <<<"$boot"
grep -Fq 'uses: ./.github/actions/guest-kernel' <<<"$(job boot-kernel)"
grep -Fq 'exclude:' <<<"$(job image)"

# Each boot runner gets what the image job's runner had: KVM and vhost-vsock, the VM programs
# where the package puts them, the headless compositor, the session bus, Mesa's software
# Vulkan driver and adb.
for needed in 'sudo modprobe vhost_vsock' 'name: boot-check-vm-programs' '/etc/apparmor.d/dsa-vm' \
    'weston libxkbcommon0 mesa-vulkan-drivers libvulkan1 dbus-daemon' 'dbus-daemon --session' \
    'weston --backend=headless' 'DSA_ADB="$GITHUB_WORKSPACE/.scratch/platform-tools/adb"' \
    'name: boot-check-programs' 'name: boot-image' 'name: boot-kernel'; do
  grep -Fq -- "$needed" <<<"$boot" || { echo "the boot checks lack: $needed"; exit 1; }
done
# The programs go back to the paths the checks were built to start them from.
grep -Fq 'done < .scratch/boot-checks/bin/programs.tsv' <<<"$boot"
grep -Fq '> "$out/bin/programs.tsv"' <<<"$(job boot-checks)"
echo "image boot checks in parallel: ok"
