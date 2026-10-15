#!/usr/bin/env bash
# Real desktop launch check for disposable Linux CI, never the user's desktop.
# Usage: bash scripts/test-linux-portable.sh appimage|flatpak ARTIFACT [LOG_DIR]
set -euo pipefail
export LC_ALL=C
if (( $# < 2 || $# > 3 )) || [[ $1 != appimage && $1 != flatpak ]]; then
    echo "Usage: $0 appimage|flatpak ARTIFACT [LOG_DIRECTORY]" >&2
    exit 2
fi
kind=$1
artifact=$(realpath "$2")
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
logs=${3:-"$repo/target/portable-smoke/$kind"}
for tool in xvfb-run xdotool dbus-run-session realpath; do
    command -v "$tool" >/dev/null || { echo "Missing smoke-test tool: $tool" >&2; exit 1; }
done
mkdir -p "$logs"
logs=$(realpath "$logs")
test -s "$artifact"
# Keep the scratch HOME and XDG_RUNTIME_DIR short: Flatpak's session helper
# puts sockets there, and a deep checkout path exceeds the 108-byte limit.
work=$(mktemp -d "${TMPDIR:-/tmp}/snekkie-smoke.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT
# Fresh HOME/settings keeps startup update checks offline and prevents touching
# any real SSH keys or user settings. Flatpak's scoped SSH permission sees only
# this disposable .ssh directory in the smoke test.
export HOME="$work/home"
export XDG_CONFIG_HOME="$HOME/.config" XDG_CACHE_HOME="$HOME/.cache"
export XDG_DATA_HOME="$HOME/.local/share" XDG_RUNTIME_DIR="$work/runtime"
mkdir -p "$XDG_CONFIG_HOME/snekkie" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_RUNTIME_DIR" "$HOME/.ssh"
chmod 0700 "$XDG_RUNTIME_DIR" "$HOME/.ssh"
printf '[Network]\nOfflineMode=1\nCheckForUpdates=0\n' > "$XDG_CONFIG_HOME/snekkie/network.ini"
export SNEKKIE_SMOKE_LOGS="$logs"
export WGPU_BACKEND=gl LIBGL_ALWAYS_SOFTWARE=1 WINIT_UNIX_BACKEND=x11
unset WAYLAND_DISPLAY

if [[ $kind == appimage ]]; then
    xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session -- \
        bash "$repo/packaging/appimage/smoke-window.sh" \
        "$artifact" --appimage-extract-and-run
else
    command -v flatpak >/dev/null || { echo 'Missing smoke-test tool: flatpak' >&2; exit 1; }
    # Package builds keep verified runtimes here; retain that cache for smoke.
    export FLATPAK_USER_DIR="$repo/target/flatpak-tools/installation"
    app_id=io.github.Dynamitecrown.Snekkie
    # Start from, and always leave, an installation without the app.
    remove_app() { flatpak --user uninstall --assumeyes --noninteractive "$app_id" >/dev/null 2>&1 || true; }
    trap 'remove_app; rm -rf -- "$work"' EXIT
    remove_app
    # package-flatpak.sh installs the runtime without related refs; drawing
    # needs Mesa from the GL extension, which users' installs pull in.
    flatpak --user install --assumeyes --noninteractive flathub \
        "org.freedesktop.Platform.GL.default/x86_64/$(sed -n 's/^runtime_branch=//p' "$repo/scripts/package-flatpak.sh")"
    flatpak --user install --assumeyes --noninteractive --bundle "$artifact"
    flatpak --user info --show-permissions "$app_id" > "$logs/permissions.txt"
    flatpak --user info --show-metadata "$app_id" > "$logs/metadata.ini"
    # Validate the installed permissions rather than only checking the source.
    python3 "$repo/packaging/flatpak/check-permissions.py" "$logs/permissions.txt"
    # Flatpak remaps XDG_CONFIG_HOME, so seed the sandbox's private settings too.
    mkdir -p "$HOME/.var/app/$app_id/config/snekkie"
    cp "$XDG_CONFIG_HOME/snekkie/network.ini" "$HOME/.var/app/$app_id/config/snekkie/"
    xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session -- \
        bash "$repo/packaging/appimage/smoke-window.sh" \
        flatpak --user run --env=WGPU_BACKEND=gl --env=LIBGL_ALWAYS_SOFTWARE=1 \
        --env=WINIT_UNIX_BACKEND=x11 "$app_id"
    # Only our isolated installation is affected; the EXIT trap removes the
    # app and keeps downloaded runtimes.
fi
