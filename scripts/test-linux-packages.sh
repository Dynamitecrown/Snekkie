#!/usr/bin/env bash
# Package-manager lifecycle and graphical-launch checks in disposable containers.
# Build revision 0 into FIXTURES and revision 1 into DIST using package-linux.sh.
set -euo pipefail
export LC_ALL=C

if [[ ${1:-} != --inside ]]; then
    if (( $# < 3 )); then
        echo "Usage: $0 DIST VERSION FIXTURES [CONTAINER_IMAGE ...]" >&2
        exit 2
    fi
    command -v docker >/dev/null || { echo 'Docker is required.' >&2; exit 1; }
    dist=$(realpath "$1")
    version=$2
    fixtures=$(realpath "$3")
    shift 3
    scripts=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
    images=("$@")
    if (( ${#images[@]} == 0 )); then
        images=(ubuntu:22.04 debian:12 fedora:44 opensuse/leap:16.0)
    fi
    for image in "${images[@]}"; do
        printf '\nTesting Linux packages on %s\n' "$image"
        docker run --rm \
            --mount "type=bind,src=$dist,dst=/packages,readonly" \
            --mount "type=bind,src=$fixtures,dst=/fixtures,readonly" \
            --mount "type=bind,src=$scripts,dst=/scripts,readonly" \
            "$image" bash /scripts/test-linux-packages.sh --inside "$version"
    done
    exit 0
fi

# This branch deliberately installs and removes packages. Never permit it to
# run on the developer's host, even if the internal switch is used by mistake.
if [[ ! -f /.dockerenv || $(id -u) != 0 || ! -d /packages || ! -d /fixtures ]]; then
    echo 'The internal test runner requires a disposable root Docker container.' >&2
    exit 2
fi
version=${2:?Missing app version}
deb_version=${version/-/~}
rpm_version=${deb_version/+/.}
deb=/packages/snekkie_${deb_version}-1_amd64.deb
old_deb=/fixtures/snekkie_${deb_version}-0_amd64.deb
rpm_package=/packages/snekkie-${rpm_version}-1.x86_64.rpm
old_rpm=/fixtures/snekkie-${rpm_version}-0.x86_64.rpm

if command -v apt-get >/dev/null; then
    family=deb
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        desktop-file-utils xvfb x11-utils dbus libgl1-mesa-dri libegl-mesa0 passwd util-linux
    install_package() { apt-get install -y --no-install-recommends "$1"; }
    reinstall_package() { apt-get install -y --no-install-recommends --reinstall "$1"; }
    remove_package() { apt-get remove -y snekkie; }
    current=$deb
    previous=$old_deb
elif command -v dnf >/dev/null; then
    family=rpm
    dnf install -y desktop-file-utils diffutils xorg-x11-server-Xvfb xwininfo dbus-daemon \
        mesa-dri-drivers mesa-libEGL mesa-libGL shadow-utils util-linux
    install_package() { dnf install -y "$1"; }
    reinstall_package() { dnf reinstall -y "$1"; }
    remove_package() { dnf remove -y snekkie; }
    current=$rpm_package
    previous=$old_rpm
elif command -v zypper >/dev/null; then
    family=rpm
    zypper --non-interactive refresh
    zypper --non-interactive install desktop-file-utils diffutils xorg-x11-server-Xvfb xwininfo \
        dbus-1 dbus-1-daemon Mesa-dri Mesa-libEGL1 Mesa-libGL1 shadow util-linux
    install_package() { zypper --non-interactive --no-gpg-checks install "$1"; }
    reinstall_package() { zypper --non-interactive --no-gpg-checks install --force "$1"; }
    remove_package() { zypper --non-interactive remove snekkie; }
    current=$rpm_package
    previous=$old_rpm
else
    echo 'Unsupported disposable container image.' >&2
    exit 2
fi
test -s "$current"
test -s "$previous"

if [[ $family == deb ]]; then
    [[ $(dpkg-deb -f "$current" Package) == snekkie ]]
    [[ $(dpkg-deb -f "$current" Architecture) == amd64 ]]
    dpkg --compare-versions "$(dpkg-deb -f "$previous" Version)" lt "$(dpkg-deb -f "$current" Version)"
    dependencies=$(dpkg-deb -f "$current" Depends)
    [[ $dependencies == *'libc6 (>= 2.35)'* && $dependencies != *libudev* ]]
    dpkg-deb --ctrl-tarfile "$current" | tar -tf - > /tmp/snekkie-control-files
    if grep -Eq '(^|/)(preinst|postinst|prerm|postrm)$' /tmp/snekkie-control-files; then
        echo 'Unexpected package maintainer scripts.' >&2
        exit 1
    fi
else
    [[ $(rpm -qp --queryformat '%{NAME} %{ARCH}' "$current") == 'snekkie x86_64' ]]
    [[ $(rpm -qp --queryformat '%{RELEASE}' "$previous") == 0 ]]
    [[ $(rpm -qp --queryformat '%{RELEASE}' "$current") == 1 ]]
    dependencies=$(rpm -qp --requires "$current")
    [[ $dependencies == *'glibc >= 2.35'* && $dependencies == *'libc.so.6'* && $dependencies != *libudev* ]]
    [[ -z $(rpm -qp --scripts "$current") ]]
fi

test_user=snekkie-package-test
test_home=/home/$test_user
useradd --create-home --shell /bin/sh "$test_user"
config_dir=$test_home/.config/snekkie
mkdir -p "$config_dir" "$test_home/runtime" "$test_home/smoke-config"
printf '{"packaging_test":"preserve settings"}\n' > "$config_dir/settings.json"
printf '{"packaging_test":"preserve sessions"}\n' > "$config_dir/sessions.json"
printf 'preserve host keys\n' > "$config_dir/known_hosts"
chown -R "$test_user:$test_user" "$test_home"
chmod 0700 "$test_home/runtime"
sha256sum "$config_dir"/* > /tmp/snekkie-config-before

assert_config_preserved() {
    sha256sum --check /tmp/snekkie-config-before
    getent group > /tmp/snekkie-groups-after
    cmp /tmp/snekkie-groups-before /tmp/snekkie-groups-after
}

assert_installed() {
    test -x /usr/bin/snekkie
    [[ $(stat -c '%U:%G %a' /usr/bin/snekkie) == 'root:root 755' ]]
    test -s /usr/share/doc/snekkie/copyright
    test -s /usr/share/icons/hicolor/512x512/apps/snekkie.png
    desktop-file-validate /usr/share/applications/snekkie.desktop
    # Container images skip documentation files (dpkg path-exclude, RPM
    # nodocs), so check that the package ships README.md rather than the disk.
    if [[ $family == deb ]]; then
        [[ $(dpkg-query -S /usr/bin/snekkie) == 'snekkie: /usr/bin/snekkie' ]]
        dpkg -L snekkie | grep -qx /usr/share/doc/snekkie/README.md
        dpkg --verify snekkie
    else
        [[ $(rpm -qf /usr/bin/snekkie --queryformat '%{NAME}') == snekkie ]]
        rpm -ql snekkie | grep -qx /usr/share/doc/snekkie/README.md
        rpm --verify snekkie
    fi
    ldd /usr/bin/snekkie > /tmp/snekkie-ldd
    if grep -q 'not found' /tmp/snekkie-ldd; then
        cat /tmp/snekkie-ldd >&2
        exit 1
    fi
    ldconfig -p > /tmp/snekkie-ldconfig
    for soname in libX11.so.6 libX11-xcb.so.1 libXcursor.so.1 libXrandr.so.2 libXi.so.6 libxcb.so.1 \
        libxkbcommon.so.0 libxkbcommon-x11.so.0 libwayland-client.so.0 \
        libwayland-cursor.so.0 libwayland-egl.so.1 libEGL.so.1 libGL.so.1 \
        libvulkan.so.1 libdbus-1.so.3; do
        grep -Fq "$soname " /tmp/snekkie-ldconfig || {
            echo "Missing runtime library: $soname" >&2
            exit 1
        }
    done
    assert_config_preserved
}

assert_removed() {
    for path in /usr/bin/snekkie /usr/share/applications/snekkie.desktop \
        /usr/share/icons/hicolor/512x512/apps/snekkie.png /usr/share/doc/snekkie; do
        test ! -e "$path"
    done
    assert_config_preserved
}

smoke_launch() {
    # Use a separate config directory for launch so package-lifecycle fixtures
    # remain untouched. Confirm that an actual window maps, not merely that the
    # process lives, using Xvfb and Mesa software GL in the container.
    # shellcheck disable=SC2016 # the inner script expands its own variables
    runuser -u "$test_user" -- env HOME="$test_home" DISPLAY=:99 \
        XDG_CONFIG_HOME="$test_home/smoke-config" XDG_RUNTIME_DIR="$test_home/runtime" \
        WGPU_BACKEND=gl LIBGL_ALWAYS_SOFTWARE=1 dbus-run-session -- bash -c '
        set -euo pipefail
        Xvfb "$DISPLAY" -screen 0 1280x800x24 -nolisten tcp -noreset > "$HOME/xvfb.log" 2>&1 &
        xpid=$!
        app_pid=""
        trap '\''if [[ -n $app_pid ]]; then kill "$app_pid" 2>/dev/null || true; wait "$app_pid" 2>/dev/null || true; fi; kill "$xpid" 2>/dev/null || true; wait "$xpid" 2>/dev/null || true'\'' EXIT
        for attempt in {1..30}; do
            if xwininfo -root >/dev/null 2>&1; then break; fi
            kill -0 "$xpid" || { cat "$HOME/xvfb.log"; exit 1; }
            sleep 1
        done
        xwininfo -root >/dev/null
        /usr/bin/snekkie > "$HOME/snekkie-smoke.log" 2>&1 &
        app_pid=$!
        mapped=false
        for attempt in {1..30}; do
            if ! kill -0 "$app_pid" 2>/dev/null; then cat "$HOME/snekkie-smoke.log"; exit 1; fi
            xwininfo -root -tree > "$HOME/windows.log"
            if grep -q '\''"Snekkie"'\'' "$HOME/windows.log"; then mapped=true; break; fi
            sleep 1
        done
        if [[ $mapped != true ]]; then
            cat "$HOME/snekkie-smoke.log" "$HOME/windows.log"
            exit 1
        fi
        sleep 2
        kill -0 "$app_pid" || { cat "$HOME/snekkie-smoke.log"; exit 1; }
        '
    assert_config_preserved
}

echo 'Fresh installation'
install_package "$current"
# Baseline groups after dependencies arrive: some (geoclue2 via
# xdg-desktop-portal on Fedora) create system groups; this package must not.
getent group > /tmp/snekkie-groups-before
assert_installed
smoke_launch
echo 'Reinstallation'
reinstall_package "$current"
assert_installed
echo 'Removal preserves per-user configuration'
remove_package
assert_removed
echo 'Install earlier package revision and upgrade'
install_package "$previous"
assert_installed
install_package "$current"
assert_installed
smoke_launch
echo 'Final removal'
remove_package
assert_removed
if [[ $family == deb ]]; then
    # No conffiles, so removal leaves nothing for a purge; it must stay clean.
    dpkg --purge snekkie
    [[ -z $(dpkg-query -W -f='${Status}' snekkie 2>/dev/null) ]]
    assert_removed
fi
echo 'Linux package lifecycle and graphical launch checks passed.'
