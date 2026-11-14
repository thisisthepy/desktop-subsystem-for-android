#!/usr/bin/env bash
# Builds the Linux packages of DSA, a .deb and an .rpm, from programs already built.
#
#   build-packages.sh --bin-dir <cargo release folder> --version <x.y.z> --out <folder>
#                     --vm-programs <folder> [--licenses <folder>]
#                     (--kernel <file> | --placeholder-kernel) [--image-dir <folder>]
#                     --repo-key <armored public key> [--repo-url <url>]
#                     [--channel stable|prerelease]
#                     [--arch amd64|arm64] [--no-deb] [--no-rpm]
#                     [--geodata-dir <folder>]
#
# The packages install the same things:
#
#   /usr/lib/dsa/bin/        dsa, dsa-linux-hostd, dsa-devices and the other host programs,
#                            and crosvm and passt, which run the Android VM and its network
#   /usr/bin/dsa             the command line client
#   /usr/lib/dsa/vm/         the guest kernel (bzImage, or Image on arm64)
#   /usr/lib/dsa/vm/image/   the Android image the first start uses, when one is given
#   /usr/lib/dsa/geodata/    the administrative boundaries the host turns Android's positions
#                            into addresses with, and their attribution (NOTICE.txt)
#   /etc/dsa/dsa-hostd.json  the service's configuration
#   systemd units            dsa-hostd.service (system) and dsa-devices.service (user)
#   udev rules, modules      /dev/kvm and /dev/vhost-vsock for the service and the user
#   desktop files            DSA Settings, DSA Files, and Open, Share and Install in Android
#   MIME type                split-APK bundles (.xapk, .apks, .apkm), which Install in Android
#                            opens
#                            for files
#   icons                    DSA's and DSA Settings' icons in the hicolor theme
#   translations             DSA Settings' gettext catalogues
#   GeoClue                  DSA's entry, so the desktop's agent can let it read the location
#   polkit action            root elevation for Android apps, which asks for an administrator
#                            (/usr/share/polkit-1/actions/io.github.thisisthepy.DSA.policy)
#   AppStream metainfo       what the software center shows when the package is opened
#   the repository key       /usr/share/keyrings/dsa-archive-keyring.gpg in the .deb,
#                            /etc/pki/rpm-gpg/RPM-GPG-KEY-dsa in the .rpm
#
# Installing either package registers DSA's own signed package repository, the way Chrome
# and VS Code do: /etc/apt/sources.list.d/dsa.sources on Debian and Ubuntu, and
# /etc/yum.repos.d/dsa.repo (and /etc/zypp/repos.d/dsa.repo where zypper is) on Fedora and
# openSUSE. Later versions then come through the package manager and the software center,
# and DSA has no updater of its own on Linux. The package scripts write the entry, and
# remove it when the package is removed (not when it is upgraded), as the key goes with the
# package's files. --repo-key is the public half of the key that signs the repository
# (.github/scripts/release/build-linux-repo.sh); --repo-url is where that repository is
# served, GitHub Pages by default.
#
# The repository has two release channels: the apt suites stable and prerelease, and the
# dnf and zypper repositories rpm/<arch> and rpm-prerelease/<arch>. --channel is the channel
# the package is published on (below 1.0 every version is a pre-release, and that is the
# default; from 1.0 the default is stable). A first installation writes the entry of that
# channel. An upgrade keeps the channel the entry already has, because the user chose it (the
# service points the entries at the channel the prerelease-updates setting chooses).
#
#   AppArmor                 crosvm's and passt's sandboxes, which make user namespaces
#   /usr/share/doc/<package> the licences, crosvm's crate list and passt's source
#
# --vm-programs is the folder dsa/host/platform/linux/crosvm/build.sh filled: crosvm, passt,
# passt-source.tar.gz and crosvm-crates.txt. --licenses is a bundle generate-licenses.py made.
#
# --geodata-dir is a folder dsa/host/core/geodata/build-geodata.py filled
# (address-boundaries.bin and NOTICE.txt). Without it this script runs build-geodata.py, which
# downloads its pinned sources once into .scratch/geodata in the repository.
#
# --placeholder-kernel puts an empty kernel in the package, for testing the package before a
# kernel is built. An image folder is one made by dsa/guest/image/make-image.py, with its
# image.json. Without one the package carries no Android image; the service then downloads
# the newest supported image.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin_dir=""
version=""
out=""
kernel=""
placeholder_kernel=0
image_dir=""
vm_programs=""
licenses=""
arch=""
make_deb=1
make_rpm=1
repo_key=""
repo_url="https://thisisthepy.github.io/desktop-subsystem-for-android"
channel=""
geodata_dir=""

die() {
    echo "build-packages: $*" >&2
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --bin-dir) bin_dir="$2"; shift 2 ;;
        --version) version="$2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        --kernel) kernel="$2"; shift 2 ;;
        --placeholder-kernel) placeholder_kernel=1; shift ;;
        --image-dir) image_dir="$2"; shift 2 ;;
        --vm-programs) vm_programs="$2"; shift 2 ;;
        --licenses) licenses="$2"; shift 2 ;;
        --arch) arch="$2"; shift 2 ;;
        --no-deb) make_deb=0; shift ;;
        --no-rpm) make_rpm=0; shift ;;
        --repo-key) repo_key="$2"; shift 2 ;;
        --repo-url) repo_url="${2%/}"; shift 2 ;;
        --channel) channel="$2"; shift 2 ;;
        --geodata-dir) geodata_dir="$2"; shift 2 ;;
        *) die "unknown argument $1" ;;
    esac
done

[[ -n "$bin_dir" && -d "$bin_dir" ]] || die "name the folder of the built programs with --bin-dir"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "name the version as x.y.z with --version"
[[ -n "$out" ]] || die "name the output folder with --out"
if [[ -z "$arch" ]]; then
    case "$(uname -m)" in
        x86_64) arch=amd64 ;;
        aarch64) arch=arm64 ;;
        *) die "name the architecture with --arch" ;;
    esac
fi
case "$arch" in
    amd64) kernel_name=bzImage; rpm_arch=x86_64 ;;
    arm64) kernel_name=Image; rpm_arch=aarch64 ;;
    *) die "the architecture is amd64 or arm64, not $arch" ;;
esac
if [[ -z "$kernel" && "$placeholder_kernel" -eq 0 ]]; then
    die "name the guest kernel with --kernel (or --placeholder-kernel for a package test)"
fi
[[ -x "$bin_dir/dsa" ]] || die "$bin_dir has no dsa"
[[ -n "$repo_key" && -f "$repo_key" ]] \
    || die "name the repository's public key (ASCII armored) with --repo-key"
[[ "$repo_url" =~ ^https?://[^[:space:]]+$ ]] || die "the repository URL $repo_url is not a URL"
if [[ -z "$channel" ]]; then
    if [[ "${version%%.*}" == 0 ]]; then channel=prerelease; else channel=stable; fi
fi
case "$channel" in
    stable | prerelease) ;;
    *) die "the channel is stable or prerelease, not $channel" ;;
esac
command -v gpg >/dev/null || die "gpg is needed to write the repository keyring"
for file in crosvm passt passt-source.tar.gz crosvm-crates.txt; do
    [[ -f "$vm_programs/$file" ]] || die "name the folder crosvm/build.sh filled with --vm-programs (it has no $file)"
done

package=desktop-subsystem-for-android
mkdir -p "$out"
out="$(cd "$out" && pwd)"
stage="$out/stage"
rm -rf "$stage"
mkdir -p "$stage"

# The programs: every DSA program the build made.
install -d "$stage/usr/lib/dsa/bin" "$stage/usr/bin"
programs=()
for program in "$bin_dir"/dsa "$bin_dir"/dsa-*; do
    [[ -f "$program" && -x "$program" ]] || continue
    case "$program" in *.d | *.so | *.rlib) continue ;; esac
    install -m 0755 "$program" "$stage/usr/lib/dsa/bin/"
    programs+=("$(basename "$program")")
done
ln -s ../lib/dsa/bin/dsa "$stage/usr/bin/dsa"
# gfxstream's host renderer, which the graphics renderer process (dsa-gfx, which the
# compositor starts) loads from beside itself.
if [[ -f "$bin_dir/libgfxstream_backend.so" ]]; then
    install -m 0755 "$bin_dir/libgfxstream_backend.so" "$stage/usr/lib/dsa/bin/"
fi
# The VM's programs, built from pinned upstream revisions.
install -m 0755 "$vm_programs/crosvm" "$vm_programs/passt" "$stage/usr/lib/dsa/bin/"
programs+=(crosvm passt)
echo "programs: ${programs[*]}"
has_program() {
    [[ -x "$stage/usr/lib/dsa/bin/$1" ]]
}

# The guest kernel and the Android image.
install -d "$stage/usr/lib/dsa/vm"
if [[ -n "$kernel" ]]; then
    install -m 0644 "$kernel" "$stage/usr/lib/dsa/vm/$kernel_name"
else
    : > "$stage/usr/lib/dsa/vm/$kernel_name"
    chmod 0644 "$stage/usr/lib/dsa/vm/$kernel_name"
fi
system_image=""
if [[ -n "$image_dir" ]]; then
    [[ -f "$image_dir/image.json" ]] || die "$image_dir has no image.json"
    install -d "$stage/usr/lib/dsa/vm/image"
    for file in "$image_dir"/*; do
        install -m 0644 "$file" "$stage/usr/lib/dsa/vm/image/"
    done
    for name in system.img system.vhdx disk.img disk.vhdx; do
        if [[ -f "$stage/usr/lib/dsa/vm/image/$name" ]]; then
            system_image="/usr/lib/dsa/vm/image/$name"
            break
        fi
    done
fi

# The service's configuration.
image_folder=""
if [[ -n "$image_dir" ]]; then
    image_folder=/usr/lib/dsa/vm/image
fi
install -d "$stage/etc/dsa"
cat > "$stage/etc/dsa/dsa-hostd.json" <<EOF
{
  "vm_name": "dsa-android",
  "kernel": "/usr/lib/dsa/vm/$kernel_name",
  "system_image": "$system_image",
  "image_dir": "$image_folder",
  "images_dir": "/var/lib/dsa/images",
  "socket": "/run/dsa/host.sock",
  "data_disk_size_gb": 64,
  "kernel_cmdline": "",
  "processors": 4,
  "control_port": 5000,
  "log_port": 5001,
  "connect_timeout_secs": 120,
  "stop_timeout_secs": 30,
  "doze_delay_secs": 30
}
EOF
chmod 0644 "$stage/etc/dsa/dsa-hostd.json"

# systemd, udev and the kernel modules.
install -d "$stage/usr/lib/systemd/system" "$stage/usr/lib/systemd/user" \
    "$stage/usr/lib/systemd/system-preset" "$stage/usr/lib/systemd/user-preset" \
    "$stage/usr/lib/udev/rules.d" "$stage/usr/lib/modules-load.d"
system_units=()
user_units=()
if has_program dsa-linux-hostd; then
    install -m 0644 "$here/systemd/dsa-hostd.service" "$stage/usr/lib/systemd/system/"
    install -m 0644 "$here/systemd/90-dsa.preset" "$stage/usr/lib/systemd/system-preset/"
    system_units+=(dsa-hostd.service)
fi
if has_program dsa-devices; then
    install -m 0644 "$here/systemd/dsa-devices.service" "$stage/usr/lib/systemd/user/"
    install -m 0644 "$here/systemd/90-dsa-user.preset" "$stage/usr/lib/systemd/user-preset/"
    user_units+=(dsa-devices.service)
fi
if has_program dsa-compositor; then
    install -m 0644 "$here/systemd/dsa-compositor.service" "$stage/usr/lib/systemd/user/"
    install -m 0644 "$here/systemd/90-dsa-user.preset" "$stage/usr/lib/systemd/user-preset/"
    user_units+=(dsa-compositor.service)
fi
install -m 0644 "$here/udev/70-dsa.rules" "$stage/usr/lib/udev/rules.d/"
install -m 0644 "$here/udev/dsa-modules.conf" "$stage/usr/lib/modules-load.d/dsa.conf"

# The desktop: the file actions and DSA's name for GeoClue.
install -d "$stage/usr/share/applications" "$stage/etc/geoclue/conf.d"
install -m 0644 "$here"/desktop/*.desktop "$stage/usr/share/applications/"
install -d "$stage/usr/share/mime/packages"
install -m 0644 "$here"/mime/*.xml "$stage/usr/share/mime/packages/"
install -m 0644 "$here/geoclue/dsa.conf" "$stage/etc/geoclue/conf.d/90-dsa.conf"

# polkit's action for root elevation: the service asks polkit for it before an Android app's
# command runs as root, and the desktop's polkit agent asks for an administrator's password.
install -d "$stage/usr/share/polkit-1/actions"
install -m 0644 "$here/polkit/io.github.thisisthepy.DSA.policy" "$stage/usr/share/polkit-1/actions/"

# DSA's icons, in the hicolor theme: the colour icon and the symbolic one of DSA and of
# its companion apps (Settings, APK Installer, Files).
settings_dir="$here/../settings"
for kind in scalable symbolic; do
    install -d "$stage/usr/share/icons/hicolor/$kind/apps"
    install -m 0644 "$settings_dir/data/icons/hicolor/$kind/apps/"*.svg \
        "$stage/usr/share/icons/hicolor/$kind/apps/"
done

# DSA Settings: its desktop entry, its AppStream entry and its translations.
if has_program dsa-settings; then
    install -m 0644 "$settings_dir/data/io.github.thisisthepy.DSA.Settings.desktop" \
        "$stage/usr/share/applications/"
    install -d "$stage/usr/share/metainfo"
    sed -e "s|@VERSION@|$version|g" -e "s|@DATE@|$(date -u +%Y-%m-%d)|g" \
        "$settings_dir/data/io.github.thisisthepy.DSA.Settings.metainfo.xml" \
        > "$stage/usr/share/metainfo/io.github.thisisthepy.DSA.Settings.metainfo.xml"
    chmod 0644 "$stage/usr/share/metainfo/io.github.thisisthepy.DSA.Settings.metainfo.xml"
    command -v msgfmt >/dev/null || die "msgfmt (gettext) is needed to build the translations"
    while read -r language; do
        [[ -n "$language" ]] || continue
        install -d "$stage/usr/share/locale/$language/LC_MESSAGES"
        msgfmt --check -o "$stage/usr/share/locale/$language/LC_MESSAGES/dsa-settings.mo" \
            "$settings_dir/po/$language.po"
        chmod 0644 "$stage/usr/share/locale/$language/LC_MESSAGES/dsa-settings.mo"
    done < "$settings_dir/po/LINGUAS"
fi

# What the software center shows for the package.
install -d "$stage/usr/share/metainfo"
sed -e "s|@VERSION@|$version|g" -e "s|@DATE@|$(date -u +%Y-%m-%d)|g" \
    "$here/appstream/io.github.thisisthepy.DSA.metainfo.xml" \
    > "$stage/usr/share/metainfo/io.github.thisisthepy.DSA.metainfo.xml"
chmod 0644 "$stage/usr/share/metainfo/io.github.thisisthepy.DSA.metainfo.xml"

# The key of DSA's package repository: a binary keyring for apt's Signed-By, and the armored
# key for dnf and zypper. Each format takes its own below. Both are package files, so
# removing the package removes the key.
key_dir="$out/key"
rm -rf "$key_dir"
install -d "$key_dir"
gpg --batch --yes --dearmor --output "$key_dir/dsa-archive-keyring.gpg" "$repo_key"
chmod 0644 "$key_dir/dsa-archive-keyring.gpg"

# AppArmor: the sandboxes of crosvm and passt.
install -d "$stage/etc/apparmor.d"
install -m 0644 "$here/apparmor/dsa-vm" "$stage/etc/apparmor.d/dsa-vm"

# The licence.
install -d "$stage/usr/share/doc/$package"
repo_root="$(cd "$here/../../../../.." && pwd)"
install -m 0644 "$repo_root/LICENSE" "$stage/usr/share/doc/$package/copyright"
install -m 0644 "$vm_programs/crosvm-crates.txt" "$vm_programs/passt-source.tar.gz" \
    "$stage/usr/share/doc/$package/"
if [[ -n "$licenses" ]]; then
    install -d "$stage/usr/share/doc/$package/licenses"
    install -m 0644 "$licenses"/* "$stage/usr/share/doc/$package/licenses/"
fi

# The address data: the administrative boundaries dsa-devices answers Android's address
# lookups from, with no server.
if [[ -z "$geodata_dir" ]]; then
    geodata_dir="$out/geodata"
    geodata_script="$repo_root/dsa/host/core/geodata/build-geodata.py"
    if command -v uv >/dev/null; then
        uv run "$geodata_script" --out "$geodata_dir/address-boundaries.bin"
    else
        python3 "$geodata_script" --out "$geodata_dir/address-boundaries.bin"
    fi
fi
install -d "$stage/usr/lib/dsa/geodata"
for file in address-boundaries.bin NOTICE.txt; do
    [[ -f "$geodata_dir/$file" ]] || die "$geodata_dir has no $file (build-geodata.py makes it)"
    install -m 0644 "$geodata_dir/$file" "$stage/usr/lib/dsa/geodata/"
done

# The repository entries the package scripts write. They are not package files: a conffile
# would stay behind after a removal, and the entry must go with the package. Each comes in
# both channels; the scripts pick one.
# apt's entry for channel $1.
apt_source() {
    cat <<EOF
Types: deb
URIs: $repo_url/apt
Suites: $1
Components: main
Architectures: $arch
Signed-By: /usr/share/keyrings/dsa-archive-keyring.gpg
EOF
}
# The folder of channel $1's rpm repository.
rpm_folder() {
    if [[ "$1" == prerelease ]]; then echo rpm-prerelease; else echo rpm; fi
}
# dnf's entry for channel $1.
dnf_repo() {
    cat <<EOF
[dsa]
name=Desktop Subsystem for Android
baseurl=$repo_url/$(rpm_folder "$1")/\$basearch
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-dsa
EOF
}
# zypper's entry for channel $1.
zypp_repo() {
    cat <<EOF
[dsa]
name=Desktop Subsystem for Android
baseurl=$repo_url/$(rpm_folder "$1")/$rpm_arch
enabled=1
autorefresh=1
type=rpm-md
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-dsa
EOF
}
# Writes entry $1 for the channel in $channel at run time: the package's own on a first
# installation, the one the entry already names on an upgrade.
write_entry() {
    local file="$1" kind="$2"
    echo "if [ \"\$channel\" = prerelease ]; then"
    echo "cat > $file <<'DSA_EOF'"
    "$kind" prerelease
    echo "DSA_EOF"
    echo "else"
    echo "cat > $file <<'DSA_EOF'"
    "$kind" stable
    echo "DSA_EOF"
    echo "fi"
    echo "chmod 0644 $file"
}
# The channel an existing entry names, for the scripts: \$channel keeps it.
keep_channel="channel=$channel
for entry in /etc/apt/sources.list.d/dsa.sources /etc/yum.repos.d/dsa.repo /etc/zypp/repos.d/dsa.repo; do
    [ -f \"\$entry\" ] || continue
    if grep -qx 'Suites: prerelease' \"\$entry\" || grep -q '^baseurl=.*/rpm-prerelease/' \"\$entry\"; then
        channel=prerelease
    elif grep -qx 'Suites: stable' \"\$entry\" || grep -q '^baseurl=.*/rpm/' \"\$entry\"; then
        channel=stable
    fi
    break
done"
deb_add_repo="mkdir -p /etc/apt/sources.list.d
$keep_channel
$(write_entry /etc/apt/sources.list.d/dsa.sources apt_source)"
deb_remove_repo="rm -f /etc/apt/sources.list.d/dsa.sources"
rpm_add_repo="$keep_channel
if [ -d /etc/yum.repos.d ]; then
$(write_entry /etc/yum.repos.d/dsa.repo dnf_repo)
fi
if [ -d /etc/zypp/repos.d ]; then
$(write_entry /etc/zypp/repos.d/dsa.repo zypp_repo)
fi"
# The key file goes with the package. dnf and zypper import it into rpm's database the first
# time they read the repository; a scriptlet cannot take it out again, because rpm holds its
# database while the scriptlet runs.
rpm_remove_repo="rm -f /etc/yum.repos.d/dsa.repo /etc/zypp/repos.d/dsa.repo"

# What the package scripts do, the same for both formats.
system_units_text="${system_units[*]:-}"
user_units_text="${user_units[*]:-}"
read -r -d '' after_install <<EOF || true
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
    for unit in $system_units_text; do
        systemctl preset "\$unit" || true
        systemctl restart "\$unit" || true
    done
    for unit in $user_units_text; do
        systemctl --global preset "\$unit" || true
    done
    systemctl restart systemd-modules-load.service || true
fi
for module in vhost_vsock vhost_net tun; do
    modprobe "\$module" 2>/dev/null || true
done
if command -v apparmor_parser >/dev/null 2>&1 && [ -d /sys/kernel/security/apparmor ]; then
    apparmor_parser -r -W /etc/apparmor.d/dsa-vm 2>/dev/null || true
fi
if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules || true
    udevadm trigger --subsystem-match=misc --subsystem-match=video4linux || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database /usr/share/mime || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
fi
EOF
read -r -d '' before_remove <<EOF || true
if [ -d /run/systemd/system ]; then
    for unit in $system_units_text; do
        systemctl stop "\$unit" || true
        systemctl disable "\$unit" || true
    done
    for unit in $user_units_text; do
        systemctl --global disable "\$unit" || true
    done
fi
EOF
read -r -d '' after_remove <<EOF || true
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
# The raw copy the service read from the image's VHDX system disk, which no package owns.
if [ ! -e /usr/lib/dsa/vm/image/image.json ]; then
    rm -f /usr/lib/dsa/vm/image/system.img /usr/lib/dsa/vm/image/disk.img
    rmdir /usr/lib/dsa/vm/image /usr/lib/dsa/vm /usr/lib/dsa 2>/dev/null || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database /usr/share/mime || true
fi
EOF

summary="Runs Android apps in windows of their own on the desktop"
description="Desktop Subsystem for Android (DSA) runs Android in a dedicated lightweight VM
and shows each Android task as a real desktop window, with the computer's
sound, cameras, location, sensors, printers and folders lent to its apps."

if [[ "$make_deb" -eq 1 ]]; then
    deb_root="$out/deb"
    rm -rf "$deb_root"
    cp -a "$stage" "$deb_root"
    install -d "$deb_root/usr/share/keyrings"
    install -m 0644 "$key_dir/dsa-archive-keyring.gpg" "$deb_root/usr/share/keyrings/"
    install -d "$deb_root/DEBIAN"
    size_kb="$(du -sk "$stage" | cut -f1)"
    {
        echo "Package: $package"
        echo "Version: $version"
        echo "Architecture: $arch"
        echo "Maintainer: thisisthepy <noreply@github.com>"
        echo "Installed-Size: $size_kb"
        echo "Depends: libc6, libstdc++6, libcap2, libwayland-client0, libdrm2, libvulkan1, libgtk-4-1, libadwaita-1-0, systemd, udev"
        echo "Recommends: pipewire-pulse | pulseaudio, libpulse0, geoclue-2.0, xdg-desktop-portal, cups-client, upower, power-profiles-daemon, iio-sensor-proxy, xdg-utils, polkitd | policykit-1, speech-dispatcher, libenchant-2-2"
        echo "Section: otherosfs"
        echo "Priority: optional"
        echo "Homepage: https://github.com/thisisthepy/desktop-subsystem-for-android"
        echo "Description: $summary"
        echo "$description" | sed 's/^/ /'
    } > "$deb_root/DEBIAN/control"
    printf '%s\n' /etc/dsa/dsa-hostd.json /etc/geoclue/conf.d/90-dsa.conf /etc/apparmor.d/dsa-vm > "$deb_root/DEBIAN/conffiles"
    printf '#!/bin/sh\nset -e\nif [ "$1" = configure ]; then\n%s\nfi\n%s\n' \
        "$deb_add_repo" "$after_install" > "$deb_root/DEBIAN/postinst"
    printf '#!/bin/sh\nset -e\nif [ "$1" = remove ] || [ "$1" = deconfigure ]; then\n%s\nfi\n' "$before_remove" > "$deb_root/DEBIAN/prerm"
    printf '#!/bin/sh\nset -e\nif [ "$1" = remove ] || [ "$1" = purge ]; then\n%s\nfi\n%s\n' \
        "$deb_remove_repo" "$after_remove" > "$deb_root/DEBIAN/postrm"
    chmod 0755 "$deb_root/DEBIAN/postinst" "$deb_root/DEBIAN/prerm" "$deb_root/DEBIAN/postrm"
    deb="$out/${package}_${version}_${arch}.deb"
    dpkg-deb --root-owner-group --build "$deb_root" "$deb"
    echo "built $deb"
fi

if [[ "$make_rpm" -eq 1 ]]; then
    top="$out/rpmbuild"
    rm -rf "$top"
    mkdir -p "$top/SPECS" "$top/BUILD" "$top/RPMS" "$top/SOURCES" "$top/SRPMS"
    rpm_stage="$out/rpm-stage"
    rm -rf "$rpm_stage"
    cp -a "$stage" "$rpm_stage"
    install -d "$rpm_stage/etc/pki/rpm-gpg"
    install -m 0644 "$repo_key" "$rpm_stage/etc/pki/rpm-gpg/RPM-GPG-KEY-dsa"
    files_list="$top/files.list"
    (
        cd "$rpm_stage"
        find . -mindepth 1 \( -type f -o -type l \) | sed 's|^\.||' | sort | while read -r path; do
            case "$path" in
                /etc/*) echo "%config(noreplace) $path" ;;
                *) echo "$path" ;;
            esac
        done
        echo "%dir /usr/lib/dsa"
        echo "%dir /usr/lib/dsa/bin"
        echo "%dir /usr/lib/dsa/vm"
        echo "%dir /usr/lib/dsa/geodata"
        [[ -d usr/lib/dsa/vm/image ]] && echo "%dir /usr/lib/dsa/vm/image"
        echo "%dir /etc/dsa"
    ) > "$files_list"
    sed -e "s|@NAME@|$package|g" -e "s|@VERSION@|$version|g" \
        -e "s|@SUMMARY@|$summary|g" -e "s|@STAGE@|$rpm_stage|g" -e "s|@FILES@|$files_list|g" \
        "$here/rpm/dsa.spec.in" > "$top/SPECS/dsa.spec"
    {
        echo "%description"
        echo "$description"
        echo
        echo "%post"
        echo "$rpm_add_repo"
        echo "$after_install"
        echo
        echo "%preun"
        echo 'if [ "$1" -eq 0 ]; then'
        echo "$before_remove"
        echo "fi"
        echo
        echo "%postun"
        echo 'if [ "$1" -eq 0 ]; then'
        echo "$rpm_remove_repo"
        echo "fi"
        echo "$after_remove"
    } >> "$top/SPECS/dsa.spec"
    rpmbuild --define "_topdir $top" --target "$rpm_arch" -bb "$top/SPECS/dsa.spec"
    find "$top/RPMS" -name '*.rpm' -exec cp {} "$out/" \;
    echo "built $(ls "$out"/*.rpm)"
fi
