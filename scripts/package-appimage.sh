#!/usr/bin/env bash
# Package the Ubuntu 22.04 x86_64 release binary without installing anything.
# See packaging/README.md for build requirements and tool provenance.
set -euo pipefail
export LC_ALL=C

if (( $# < 1 || $# > 3 )); then
    echo "Usage: $0 VERSION [BINARY] [OUTPUT_DIRECTORY]" >&2
    exit 2
fi
version=$1
binary=${2:-target/release/snekkie}
output=${3:-dist}
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    echo 'Expected a SemVer app version.' >&2
    exit 2
fi
for tool in curl sha256sum readelf ldconfig desktop-file-validate realpath; do
    command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }
done
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
binary=$(realpath "$binary")
test -s "$binary"
if [[ $(uname -m) != x86_64 ]] || ! readelf -h "$binary" | grep -q 'Machine:.*X86-64'; then
    echo 'AppImage currently requires the x86_64 Linux binary.' >&2
    exit 1
fi
needed_glibc=$(readelf --version-info "$binary" |
    sed -n 's/.*Name: GLIBC_\([0-9.]*\).*/\1/p' | sort -Vu | tail -n 1)
if [[ -z $needed_glibc ]] || [[ $(printf '%s\n' "$needed_glibc" 2.35 | sort -V | tail -n 1) != 2.35 ]]; then
    echo "Expected an Ubuntu 22.04-compatible ELF (highest GLIBC: ${needed_glibc:-unknown})." >&2
    exit 1
fi
mkdir -p "$repo/target/appimage-tools" "$output"
output=$(realpath "$output")
cache="$repo/target/appimage-tools"
work=$(mktemp -d "$repo/target/appimage-package.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT

# Immutable upstream release tags, with GitHub asset digests verified on
# 2026-10-15. Neither tooling nor the embedded runtime follows "continuous".
fetch_checked() {
    local url=$1 expected=$2 destination=$3
    if [[ ! -s $destination ]] || ! printf '%s  %s\n' "$expected" "$destination" | sha256sum --check --status; then
        curl --fail --location --proto '=https' --proto-redir '=https' \
            --retry 3 --connect-timeout 30 --max-time 300 "$url" -o "$destination.part"
        printf '%s  %s\n' "$expected" "$destination.part" | sha256sum --check --strict
        mv -- "$destination.part" "$destination"
    fi
}
deploy="$cache/linuxdeploy-1-alpha-20251107-1-x86_64.AppImage"
runtime="$cache/runtime-20251108-x86_64"
fetch_checked 'https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage' \
    c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d "$deploy"
fetch_checked 'https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64' \
    2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d "$runtime"
chmod 0755 "$deploy"

appdir="$work/Snekkie.AppDir"
install -Dm644 "$repo/LICENSE" "$appdir/usr/share/doc/snekkie/LICENSE"
desktop-file-validate "$repo/packaging/linux/snekkie.desktop"

# winit dlopens these SONAMEs, so an ELF DT_NEEDED scan alone misses them.
# Bundle only the keyboard and X11 extension helpers that desktops may lack.
# libX11, libxcb, libwayland-*, libEGL/libGL, libvulkan and libdbus stay with
# the host: they are on linuxdeploy's excludelist or talk to the host's GPU
# drivers and services, and an older bundled copy breaks newer drivers.
# linuxdeploy sets RUNPATH=$ORIGIN/../lib on the binary, which dlopen from the
# binary honors, so the default AppRun symlink needs no LD_LIBRARY_PATH that
# would leak into programs Snekkie starts (browser, file manager).
libraries=(libxkbcommon.so.0 libxkbcommon-x11.so.0 libXcursor.so.1 libXrandr.so.2 libXi.so.6)
library_args=()
linker_cache=$(ldconfig -p)
for soname in "${libraries[@]}"; do
    path=$(awk -v name="$soname" '$1 == name && /x86-64/ {print $NF; exit}' <<< "$linker_cache")
    if [[ -z $path || ! -f $path ]]; then
        echo "Missing runtime library: $soname (see packaging/README.md)." >&2
        exit 1
    fi
    library_args+=(--library "$path")
done
artifact="$output/Snekkie-$version-x86_64.AppImage"
export APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 VERSION="$version"
export LDAI_RUNTIME_FILE="$runtime" LDAI_OUTPUT="$artifact"
# No AppImage-specific update feed is advertised: updates remain manual.
"$deploy" --appdir "$appdir" --executable "$binary" \
    --desktop-file "$repo/packaging/linux/snekkie.desktop" \
    --icon-file "$repo/assets/icon.png" --icon-filename snekkie \
    "${library_args[@]}" --output appimage
test -s "$artifact"
chmod 0755 "$artifact"
# Guard the bundling policy above against linuxdeploy behavior changes.
for excluded in libX11.so.6 libxcb.so.1 libEGL.so.1 libGL.so.1 libvulkan.so.1 \
    libwayland-client.so.0 libdbus-1.so.3 libsystemd.so.0; do
    if [[ -e $appdir/usr/lib/$excluded ]]; then
        echo "The AppImage must use the host's $excluded, not a bundled copy." >&2
        exit 1
    fi
done
[[ -L $appdir/AppRun && $(readlink "$appdir/AppRun") == usr/bin/snekkie ]] || {
    echo 'Expected the default AppRun symlink to usr/bin/snekkie.' >&2
    exit 1
}
printf 'Built %s\n' "$artifact"
