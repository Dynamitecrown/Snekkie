#!/usr/bin/env bash
# Package an existing release ELF; no Cargo build or host package installation.
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
for tool in flatpak readelf realpath desktop-file-validate appstreamcli gzip; do
    command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }
done
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
binary=$(realpath "$binary")
test -s "$binary"
if [[ $(uname -m) != x86_64 ]] || ! readelf -h "$binary" | grep -q 'Machine:.*X86-64'; then
    echo 'The Flatpak currently requires the x86_64 Linux binary.' >&2
    exit 1
fi
mkdir -p "$repo/target/flatpak-tools" "$output"
output=$(realpath "$output")
work=$(mktemp -d "$repo/target/flatpak-package.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT

# Isolated per-user installation and config: runtime downloads stay in target/,
# and this script never adds a remote or application to the real user account.
export FLATPAK_USER_DIR="$repo/target/flatpak-tools/installation"
export XDG_DATA_HOME="$repo/target/flatpak-tools/data"
export XDG_CONFIG_HOME="$repo/target/flatpak-tools/config"
export XDG_CACHE_HOME="$repo/target/flatpak-tools/cache"
mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"
app_id=io.github.Dynamitecrown.Snekkie
runtime_branch=26.08
runtime_repo=https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak --user remote-add --if-not-exists flathub "$runtime_repo"
# Supported stable branch, verified against Freedesktop's 26.08 release and
# Flathub's runtime ref on 2026-10-15. GPG-verified runtime security updates
# remain enabled; an immutable runtime commit would freeze security fixes.
# build-init insists on both being installed, although nothing is compiled
# here. --no-related skips their locale, GL and codec extensions.
flatpak --user install --assumeyes --noninteractive --no-related flathub \
    "org.freedesktop.Platform/x86_64/$runtime_branch" \
    "org.freedesktop.Sdk/x86_64/$runtime_branch"
build="$work/build"
# build-* commands find runtimes in every installation; they take no --user.
flatpak build-init --arch=x86_64 "$build" "$app_id" \
    org.freedesktop.Sdk org.freedesktop.Platform "$runtime_branch"
install -Dm755 "$binary" "$build/files/bin/snekkie"
install -Dm644 "$repo/LICENSE" "$build/files/share/licenses/$app_id/LICENSE"
install -Dm644 "$repo/packaging/flatpak/$app_id.desktop" \
    "$build/files/share/applications/$app_id.desktop"
install -Dm644 "$repo/assets/icon.png" \
    "$build/files/share/icons/hicolor/512x512/apps/$app_id.png"
mkdir -p "$build/files/share/metainfo"
release_date=${SNEKKIE_RELEASE_DATE:-$(date -u +%F)}
if [[ ! $release_date =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
    echo 'SNEKKIE_RELEASE_DATE must be YYYY-MM-DD.' >&2
    exit 2
fi
sed -e "s/@VERSION@/$version/g" -e "s/@DATE@/$release_date/g" \
    "$repo/packaging/flatpak/$app_id.metainfo.xml.in" \
    > "$build/files/share/metainfo/$app_id.metainfo.xml"
desktop-file-validate "$build/files/share/applications/$app_id.desktop"
appstreamcli validate --no-net "$build/files/share/metainfo/$app_id.metainfo.xml"
# Compose the catalog that flatpak-builder would: Flatpak reads the installed
# version from it (flatpak info, and the installer's update check), and so do
# GNOME Software and KDE Discover.
appstreamcli compose --no-color --prefix=/ --origin="$app_id" \
    --result-root="$build/files" --data-dir="$build/files/share/app-info/xmls" \
    --icons-dir="$build/files/share/app-info/icons/flatpak" --components="$app_id" \
    "$build/files"
catalog="$build/files/share/app-info/xmls/$app_id.xml.gz"
if ! gzip -dc "$catalog" | grep -q "<release[^>]*version=\"$version\""; then
    echo "The composed AppStream catalog does not record version $version." >&2
    exit 1
fi
mapfile -t permissions < "$repo/packaging/flatpak/finish-args.txt"
flatpak build-finish --command=snekkie "${permissions[@]}" "$build"
flatpak build-export --arch=x86_64 "$work/repo" "$build" stable
artifact="$output/Snekkie-$version-x86_64.flatpak"
flatpak build-bundle --arch=x86_64 --runtime-repo="$runtime_repo" \
    "$work/repo" "$artifact" "$app_id" stable
test -s "$artifact"
printf 'Built %s\n' "$artifact"
