#!/usr/bin/env bash
# Package the Apple silicon and Intel release builds as one universal
# Snekkie.app inside a compressed disk image. Run on macOS with Xcode's
# command-line tools after building both targets:
#   MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --locked --target aarch64-apple-darwin
#   MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --locked --target x86_64-apple-darwin
#   bash scripts/package-macos.sh VERSION [OUTPUT_DIRECTORY]
# The app is ad-hoc signed, not notarized: see docs/linux-and-macos.md.
set -euo pipefail
export LC_ALL=C

if (($# < 1 || $# > 2)); then
    echo "Usage: $0 VERSION [OUTPUT_DIRECTORY]" >&2
    exit 2
fi
version=$1
output=${2:-dist}
if [[ ! $version =~ ^([0-9]+\.[0-9]+\.[0-9]+)(-[0-9A-Za-z.-]+)?$ ]]; then
    echo 'Expected a SemVer app version.' >&2
    exit 2
fi
# CFBundleShortVersionString must be numeric; SnekkieVersion keeps the rest.
short_version=${BASH_REMATCH[1]}
[[ $(uname -s) == Darwin ]] || { echo 'Run this on macOS.' >&2; exit 1; }
for tool in lipo vtool codesign sips iconutil hdiutil plutil ditto; do
    command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }
done
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
arm=$repo/target/aarch64-apple-darwin/release/snekkie
intel=$repo/target/x86_64-apple-darwin/release/snekkie
for binary in "$arm" "$intel"; do
    [[ -s $binary ]] || { echo "Missing release build: $binary" >&2; exit 1; }
done
lipo "$arm" -verify_arch arm64
lipo "$intel" -verify_arch x86_64
mkdir -p "$output"
output=$(cd "$output" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/snekkie-macos.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT

app=$work/root/Snekkie.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/snekkie" "$arm" "$intel"
chmod 0755 "$app/Contents/MacOS/snekkie"
lipo "$app/Contents/MacOS/snekkie" -verify_arch arm64 x86_64
# Both slices must run on the advertised minimum macOS (LSMinimumSystemVersion).
for arch in arm64 x86_64; do
    minimum=$(vtool -arch "$arch" -show-build "$app/Contents/MacOS/snekkie" | awk '/minos/ {print $2; exit}')
    if [[ -z $minimum ]] || ! awk -v m="$minimum" 'BEGIN { split(m, p, "."); exit !(p[1] < 11 || (p[1] == 11 && p[2] + 0 == 0)) }'; then
        echo "The $arch build targets macOS ${minimum:-unknown}; set MACOSX_DEPLOYMENT_TARGET=11.0." >&2
        exit 1
    fi
done

sed -e "s/@VERSION@/$version/g" -e "s/@SHORT_VERSION@/$short_version/g" \
    "$repo/packaging/macos/Info.plist.in" >"$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
printf 'APPL????' >"$app/Contents/PkgInfo"

# The repository's icon is 512 px; larger slots are left to macOS scaling.
iconset=$work/Snekkie.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$repo/assets/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    if ((double <= 512)); then
        sips -z "$double" "$double" "$repo/assets/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    fi
done
iconutil --convert icns --output "$app/Contents/Resources/Snekkie.icns" "$iconset"
cp "$repo/LICENSE" "$app/Contents/Resources/LICENSE"

# Ad-hoc signature: required for Apple silicon, and it seals the bundle so
# macOS reports an unnotarized app rather than a damaged one.
codesign --force --sign - --timestamp=none "$app/Contents/MacOS/snekkie"
codesign --force --sign - --timestamp=none "$app"
codesign --verify --strict --verbose=2 "$app"

ln -s /Applications "$work/root/Applications"
cat >"$work/root/Read me first.txt" <<'TEXT'
Install Snekkie by dragging Snekkie.app onto the Applications folder.

Snekkie is open source and not yet notarized by Apple. The first time you
open a copy downloaded with a web browser, macOS may say it cannot verify
the developer. To open it anyway:

  1. Open Snekkie once, then close the warning.
  2. Open System Settings > Privacy & Security.
  3. Under Security, click Open Anyway next to the Snekkie message.

To check a download first, compare its SHA-256 with the SHA256SUMS file on
the release page:  shasum -a 256 Snekkie-*-macos-universal.dmg

Saved sessions and settings are in ~/Library/Application Support/snekkie.
TEXT

dmg=$output/Snekkie-$version-macos-universal.dmg
rm -f "$dmg"
# hdiutil occasionally reports a busy device on CI runners; retry briefly.
for attempt in 1 2 3; do
    if hdiutil create -volname Snekkie -srcfolder "$work/root" -fs HFS+ -format UDZO \
        -imagekey zlib-level=9 -ov "$dmg" >/dev/null; then
        break
    fi
    ((attempt < 3)) || exit 1
    sleep 5
done
hdiutil verify "$dmg" >/dev/null
printf 'Built %s\n' "$dmg"
