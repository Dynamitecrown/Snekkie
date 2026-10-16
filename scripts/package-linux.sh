#!/usr/bin/env bash
# Build downloadable Debian and RPM packages from the existing Linux payload.
# Run on Ubuntu 22.04 with dpkg-dev, rpm, binutils and desktop-file-utils.
set -euo pipefail
export LC_ALL=C

if (( $# < 1 || $# > 4 )); then
    echo "Usage: $0 VERSION [BINARY] [OUTPUT_DIRECTORY] [PACKAGE_REVISION]" >&2
    exit 2
fi

version=$1
binary=${2:-target/release/snekkie}
output=${3:-dist}
revision=${4:-1}
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] ||
    [[ ! $revision =~ ^[0-9]+$ ]]; then
    echo 'Expected a SemVer app version and a numeric package revision.' >&2
    exit 2
fi

for tool in dpkg-deb dpkg-shlibdeps dpkg readelf rpmbuild rpm desktop-file-validate realpath; do
    command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }
done
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
binary=$(realpath "$binary")
test -s "$binary"
if [[ $(uname -m) != x86_64 ]] || ! readelf -h "$binary" | grep -q 'Machine:.*X86-64'; then
    echo 'Linux packages currently require the x86_64 Linux binary.' >&2
    exit 1
fi

# Keep the existing Ubuntu 22.04 compatibility floor. Reject an accidentally
# newer build rather than publishing a misleading libc6 >= 2.35 dependency.
needed_glibc=$(readelf --version-info "$binary" |
    sed -n 's/.*Name: GLIBC_\([0-9.]*\).*/\1/p' | sort -Vu | tail -n 1)
if [[ -z $needed_glibc ]] || ! dpkg --compare-versions "$needed_glibc" le 2.35; then
    echo "Expected an Ubuntu 22.04-compatible ELF (highest GLIBC: ${needed_glibc:-unknown})." >&2
    exit 1
fi

mkdir -p "$output" "$repo/target"
output=$(realpath "$output")
work=$(mktemp -d "$repo/target/linux-package.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT
payload="$work/payload"
install -Dm755 "$binary" "$payload/usr/bin/snekkie"
install -Dm644 "$repo/packaging/linux/snekkie.desktop" "$payload/usr/share/applications/snekkie.desktop"
install -Dm644 "$repo/assets/icon.png" "$payload/usr/share/icons/hicolor/512x512/apps/snekkie.png"
install -Dm644 "$repo/LICENSE" "$payload/usr/share/doc/snekkie/copyright"
install -Dm644 "$repo/README.md" "$payload/usr/share/doc/snekkie/README.md"
desktop-file-validate "$payload/usr/share/applications/snekkie.desktop"

deb_version=${version/-/~}
rpm_version=${deb_version/+/.}

# dpkg-shlibdeps needs a source-package control file even with -O. Keep all
# generated metadata under target, outside the checkout's packaging sources.
mkdir -p "$work/debian"
cat > "$work/debian/control" <<'CONTROL'
Source: snekkie
Section: net
Priority: optional
Maintainer: Dynamitecrown <67209475+Dynamitecrown@users.noreply.github.com>

Package: snekkie
Architecture: amd64
Description: A tabbed SSH and serial terminal
CONTROL
elf_dependencies=$(cd "$work" && dpkg-shlibdeps -O -e"$payload/usr/bin/snekkie")
elf_dependencies=${elf_dependencies#shlibs:Depends=}
if [[ -z $elf_dependencies || $elf_dependencies == *$'\n'* ]]; then
    echo 'dpkg-shlibdeps did not produce a dependency list.' >&2
    exit 1
fi

# These libraries are opened by name rather than linked (dpkg-shlibdeps already
# adds the libc6 floor from the ELF symbols). There is deliberately
# no libudev dependency: serialport disables that feature in Cargo.toml.
runtime_dependencies='ca-certificates, xdg-utils, libx11-6, libx11-xcb1, libxcursor1, libxrandr2, libxi6, libxcb1, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libwayland-cursor0, libwayland-egl1, libegl1, libgl1, libvulkan1, libdbus-1-3'
debroot="$work/deb"
cp -a "$payload" "$debroot"
mkdir -p "$debroot/DEBIAN"
installed_size=$(du -sk "$debroot/usr" | cut -f1)
cat > "$debroot/DEBIAN/control" <<CONTROL
Package: snekkie
Version: $deb_version-$revision
Section: net
Priority: optional
Architecture: amd64
Maintainer: Dynamitecrown <67209475+Dynamitecrown@users.noreply.github.com>
Installed-Size: $installed_size
Depends: $elf_dependencies, $runtime_dependencies
Recommends: xdg-desktop-portal
Homepage: https://github.com/Dynamitecrown/Snekkie
Description: A tabbed SSH and serial terminal
 SSH, serial, Telnet and raw TCP sessions in one desktop application.
CONTROL
chmod 0755 "$debroot/DEBIAN"
chmod 0644 "$debroot/DEBIAN/control"
(cd "$debroot" && find usr -type f -print0 | sort -z | xargs -0 md5sum > DEBIAN/md5sums)
deb="$output/snekkie_${deb_version}-${revision}_amd64.deb"
dpkg-deb --root-owner-group --build "$debroot" "$deb"

rpmtop="$work/rpm"
mkdir -p "$rpmtop"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
rpmbuild -bb "$repo/packaging/linux/snekkie.spec.in" \
    --define "_topdir $rpmtop" \
    --define "snekkie_version $rpm_version" \
    --define "snekkie_revision $revision" \
    --define "snekkie_payload $payload"
rpm_package="$rpmtop/RPMS/x86_64/snekkie-${rpm_version}-${revision}.x86_64.rpm"
test -s "$rpm_package"
# Do not silently publish an RPM if a build host has disabled automatic ELF
# dependencies. Runtime SONAME additions complement, not replace, that scan.
rpm -qp --requires "$rpm_package" > "$work/rpm-requires.txt"
if ! grep -q '^libc\.so\.6' "$work/rpm-requires.txt"; then
    echo 'RPM automatic ELF dependency generation is missing.' >&2
    exit 1
fi
cp "$rpm_package" "$output/"
printf 'Built %s\nBuilt %s\n' "$deb" "$output/$(basename "$rpm_package")"
