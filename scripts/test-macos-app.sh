#!/usr/bin/env bash
# Check the macOS disk image on a disposable GitHub-hosted runner: its
# layout, universal signed app and version, then a real launch that must
# open a window and keep running. Never touches the runner user's settings.
#   bash scripts/test-macos-app.sh DMG VERSION [LOG_DIRECTORY]
set -euo pipefail
export LC_ALL=C
if (($# < 2 || $# > 3)); then
    echo "Usage: $0 DMG VERSION [LOG_DIRECTORY]" >&2
    exit 2
fi
if [[ ${GITHUB_ACTIONS:-} != true || ${RUNNER_ENVIRONMENT:-} != github-hosted || $(uname -s) != Darwin ]]; then
    echo 'This launch check runs only on GitHub-hosted macOS runners.' >&2
    exit 2
fi
dmg=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
version=$2
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
logs=${3:-$repo/target/macos-smoke}
mkdir -p "$logs"
logs=$(cd "$logs" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/snekkie-macos-smoke.XXXXXXXX")
mount=$work/mount
pid=
cleanup() {
    # wait reports the stopped app's SIGTERM status; that is not a failure.
    if [[ -n $pid ]]; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    hdiutil detach "$mount" -quiet -force 2>/dev/null || true
    rm -rf -- "$work"
}
trap cleanup EXIT

mkdir -p "$mount"
hdiutil attach -nobrowse -noautoopen -readonly -mountpoint "$mount" "$dmg" >/dev/null
[[ -d $mount/Snekkie.app && -L $mount/Applications && -f "$mount/Read me first.txt" ]] ||
    { echo 'Unexpected disk image layout.' >&2; ls -la "$mount" >&2; exit 1; }
codesign --verify --strict --verbose=2 "$mount/Snekkie.app"
archs=" $(lipo -archs "$mount/Snekkie.app/Contents/MacOS/snekkie") "
[[ $archs == *' arm64 '* && $archs == *' x86_64 '* ]] ||
    { echo "Expected a universal x86_64 + arm64 executable, found:$archs" >&2; exit 1; }
[[ $(plutil -extract SnekkieVersion raw "$mount/Snekkie.app/Contents/Info.plist") == "$version" ]] ||
    { echo "Info.plist does not record version $version." >&2; exit 1; }
[[ $(plutil -extract CFBundleIdentifier raw "$mount/Snekkie.app/Contents/Info.plist") == io.github.Dynamitecrown.Snekkie ]]
ditto "$mount/Snekkie.app" "$work/Snekkie.app"
hdiutil detach "$mount" -quiet

# A fresh HOME keeps settings and SSH files away from the runner's, and an
# offline policy keeps the launch from contacting GitHub.
home=$work/home
mkdir -p "$home/Library/Application Support/snekkie" "$home/.ssh"
printf '[Network]\nOfflineMode=1\nCheckForUpdates=0\n' >"$home/Library/Application Support/snekkie/network.ini"
swiftc -O -o "$work/window-of" "$repo/packaging/macos/window-of.swift"

HOME=$home "$work/Snekkie.app/Contents/MacOS/snekkie" >"$logs/launch.log" 2>&1 &
pid=$!
window=
for ((attempt = 0; attempt < 120; attempt++)); do
    if ! kill -0 "$pid" 2>/dev/null; then
        cat "$logs/launch.log" >&2
        echo 'Snekkie exited before opening a window.' >&2
        pid=
        exit 1
    fi
    if window=$("$work/window-of" "$pid"); then
        break
    fi
    sleep 0.5
done
[[ -n $window ]] || { cat "$logs/launch.log" >&2; echo 'Snekkie opened no window within 60 seconds.' >&2; exit 1; }
printf '%s\n' "$window" >"$logs/window.txt"
# Catch a renderer that fails after the window appears.
sleep 3
kill -0 "$pid" || { cat "$logs/launch.log" >&2; echo 'Snekkie exited after opening its window.' >&2; exit 1; }
"$work/window-of" "$pid" >/dev/null
# Screen capture needs a permission runners may not grant; it is optional.
screencapture -x -o -l "${window%% *}" "$logs/window.png" 2>/dev/null || true
[[ -f "$home/Library/Application Support/snekkie/network.ini" ]]
printf 'PASS: Snekkie %s (%s) opened a %s window and kept running.\n' "$version" "$(uname -m)" "${window#* }"
