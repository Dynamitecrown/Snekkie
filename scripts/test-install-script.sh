#!/usr/bin/env bash
# End-to-end checks of installer/unix/snekkie-install.sh with real package
# managers. OLD and NEW are folders holding two releases' packages, each with
# its SHA256SUMS, so no GitHub release is needed.
#
#   bash scripts/test-install-script.sh containers OLD NEW [IMAGE ...]
#       Docker on Linux: per distribution, install OLD with its package
#       manager, rerun, refuse a tampered package and a missing confirmation,
#       upgrade to NEW and uninstall; then the same for the per-user AppImage.
#   bash scripts/test-install-script.sh flatpak OLD NEW
#       The Flatpak lifecycle, in the isolated installation that
#       package-flatpak.sh uses. Disposable VMs or containers only.
#   bash scripts/test-install-script.sh macos OLD NEW
#       The disk image lifecycle. GitHub-hosted macOS runners only.
set -euo pipefail
export LC_ALL=C

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
installer=$repo/installer/unix/snekkie-install.sh
mode=${1:-}

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# Run the installer, keeping its output and exit status for the checks.
run_installer() {
    set +e
    "$@" >"$output" 2>&1
    status=$?
    set -e
    sed 's/^/    | /' "$output"
}

expect_status() {
    [[ $status == "$1" ]] || fail "expected exit $1, got $status"
}

expect_output() {
    grep -Fq -- "$1" "$output" || fail "expected output containing: $1"
}

disposable_ci() {
    [[ ${GITHUB_ACTIONS:-} == true && ${RUNNER_ENVIRONMENT:-} == github-hosted ]]
}

case $mode in
containers)
    (($# >= 3)) || { echo "Usage: $0 containers OLD NEW [IMAGE ...]" >&2; exit 2; }
    command -v docker >/dev/null || fail 'Docker is required.'
    old=$(realpath "$2")
    new=$(realpath "$3")
    shift 3
    images=("$@")
    if ((${#images[@]} == 0)); then
        images=(ubuntu:22.04 ubuntu:24.04 debian:13 fedora:44 opensuse/tumbleweed)
    fi
    for image in "${images[@]}"; do
        printf '\nTesting the installer on %s\n' "$image"
        docker run --rm \
            --mount "type=bind,src=$old,dst=/old,readonly" \
            --mount "type=bind,src=$new,dst=/new,readonly" \
            --mount "type=bind,src=$repo/installer/unix,dst=/installer,readonly" \
            --mount "type=bind,src=$repo/scripts,dst=/scripts,readonly" \
            "$image" bash /scripts/test-install-script.sh inside
    done
    exit 0
    ;;
inside)
    # Installs and removes system packages: never on a developer's host.
    if [[ ! -f /.dockerenv || $(id -u) != 0 || ! -d /old || ! -d /new ]]; then
        echo 'The internal test runner requires a disposable root Docker container.' >&2
        exit 2
    fi
    installer=/installer/snekkie-install.sh
    ;;
flatpak | macos)
    (($# == 3)) || { echo "Usage: $0 $mode OLD NEW" >&2; exit 2; }
    old=$(realpath "$2")
    new=$(realpath "$3")
    if [[ $mode == macos ]] && ! disposable_ci; then
        echo 'The macOS check installs into /Applications: GitHub-hosted runners only.' >&2
        exit 2
    fi
    if [[ $mode == flatpak ]] && ! disposable_ci && [[ ! -f /.dockerenv ]]; then
        echo 'The Flatpak check needs a disposable CI VM or container.' >&2
        exit 2
    fi
    ;;
*)
    sed -n '2,16s/^# \{0,1\}//p' "$0" >&2
    exit 2
    ;;
esac

output=$(mktemp)
status=0

# The app version a folder's AppImage, Flatpak or disk image was built for.
release_in() {
    local file name
    for file in "$1"/Snekkie-*-x86_64.AppImage "$1"/Snekkie-*-x86_64.flatpak "$1"/Snekkie-*-macos-universal.dmg; do
        [[ -e $file ]] || continue
        name=${file##*/}
        name=${name#Snekkie-}
        name=${name%-x86_64.AppImage}
        name=${name%-x86_64.flatpak}
        printf '%s
' "${name%-macos-universal.dmg}"
        return 0
    done
    return 1
}

if [[ $mode == inside ]]; then
    old=/old
    new=/new
    old_release=$(release_in "$old") || fail 'OLD has no AppImage naming its release'
    new_release=$(release_in "$new") || fail 'NEW has no AppImage naming its release'
    if command -v apt-get >/dev/null; then
        family=deb
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends sudo ca-certificates >/dev/null
        package_version() { dpkg-query -W -f='${Status} ${Version}' snekkie 2>/dev/null | sed -n 's/^install ok installed //p'; }
    elif command -v dnf >/dev/null; then
        family=rpm
        dnf install -y -q sudo findutils util-linux diffutils >/dev/null
        package_version() { if rpm -q snekkie >/dev/null 2>&1; then rpm -q --queryformat '%{VERSION}-%{RELEASE}' snekkie; fi; }
    elif command -v zypper >/dev/null; then
        family=rpm
        zypper --non-interactive --quiet install sudo findutils gawk util-linux diffutils >/dev/null
        package_version() { if rpm -q snekkie >/dev/null 2>&1; then rpm -q --queryformat '%{VERSION}-%{RELEASE}' snekkie; fi; }
    else
        fail 'unsupported container image'
    fi
    if [[ $family == deb ]]; then
        old_file=$(cd "$old" && ls snekkie_*_amd64.deb)
        new_file=$(cd "$new" && ls snekkie_*_amd64.deb)
        old_version=${old_file#snekkie_}
        old_version=${old_version%_amd64.deb}
        new_version=${new_file#snekkie_}
        new_version=${new_version%_amd64.deb}
    else
        old_file=$(cd "$old" && ls snekkie-*.x86_64.rpm)
        new_file=$(cd "$new" && ls snekkie-*.x86_64.rpm)
        old_version=${old_file#snekkie-}
        old_version=${old_version%.x86_64.rpm}
        new_version=${new_file#snekkie-}
        new_version=${new_version%.x86_64.rpm}
    fi
    useradd --create-home --shell /bin/sh tester
    printf 'tester ALL=(ALL) NOPASSWD: ALL\n' >/etc/sudoers.d/tester
    chmod 0440 /etc/sudoers.d/tester
    home=/home/tester
    mkdir -p "$home/.config/snekkie"
    printf '{"installer_test":"keep my sessions"}\n' >"$home/.config/snekkie/sessions.json"
    printf '{"installer_test":"keep my settings"}\n' >"$home/.config/snekkie/settings.json"
    chown -R tester:tester "$home"
    (cd "$home/.config/snekkie" && sha256sum ./*) >/tmp/config-before
    config_kept() { (cd "$home/.config/snekkie" && sha256sum --check --quiet /tmp/config-before) || fail 'settings changed'; }
    # shellcheck disable=SC2317,SC2329 # invoked through run_installer "$@"
    as_tester() {
        runuser -u tester -- env -i HOME="$home" USER=tester LANG=C.UTF-8 \
            PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin "$@"
    }
    expect_package() {
        [[ $(package_version) == "$1" ]] || fail "expected package version '$1', got '$(package_version)'"
    }
    tampered=$(mktemp -d)
    cp "$new"/* "$tampered/"
    for file in "$tampered"/snekkie_*.deb "$tampered"/snekkie-*.rpm; do printf 'x' >>"$file"; done
    chmod -R a+rX "$tampered"

    echo '== No confirmation without a terminal or --yes'
    run_installer as_tester sh "$installer" --from "$old"
    expect_status 2
    expect_output 'Rerun with --yes'
    expect_package ''

    echo "== Install the older release as a $family package"
    run_installer as_tester sh "$installer" --from "$old" --yes
    expect_status 0
    expect_output "Snekkie $old_release is installed."
    expect_package "$old_version"
    [[ -x /usr/bin/snekkie && -f /usr/share/applications/snekkie.desktop ]] || fail 'package files missing'
    config_kept

    echo '== Rerunning with the same release changes nothing'
    run_installer as_tester sh "$installer" --from "$old" --yes
    expect_status 0
    expect_output "Snekkie $old_release is already installed and up to date."
    expect_package "$old_version"

    echo '== A package that does not match SHA256SUMS is refused'
    run_installer as_tester sh "$installer" --from "$tampered" --yes
    expect_status 2
    expect_output "does not match the release's SHA-256 checksum"
    expect_package "$old_version"

    echo '== Update to the newer release'
    run_installer as_tester sh "$installer" --from "$new" --yes
    expect_status 0
    expect_output "Snekkie $new_release is installed."
    expect_package "$new_version"
    config_kept

    echo '== Uninstall keeps settings'
    run_installer as_tester sh "$installer" --uninstall --yes
    expect_status 0
    expect_output 'Snekkie was removed.'
    expect_package ''
    [[ ! -e /usr/bin/snekkie ]] || fail '/usr/bin/snekkie remains'
    config_kept

    echo '== Root can install the package without sudo'
    run_installer sh "$installer" --from "$new" --yes
    expect_status 0
    expect_package "$new_version"
    run_installer sh "$installer" --uninstall --yes
    expect_status 0
    expect_package ''

    echo '== The AppImage is refused for root'
    run_installer sh "$installer" --from "$new" --format appimage --yes
    expect_status 2
    expect_output 'normal user'

    echo '== Per-user AppImage installation'
    run_installer as_tester sh "$installer" --from "$old" --format appimage --yes
    expect_status 0
    expect_output "Snekkie $old_release is installed."
    appimage=$home/.local/bin/snekkie
    desktop=$home/.local/share/applications/snekkie.desktop
    icon=$home/.local/share/icons/hicolor/512x512/apps/snekkie.png
    [[ -x $appimage ]] || fail 'AppImage not installed'
    cmp -s "$appimage" "$old/Snekkie-$old_release-x86_64.AppImage" || fail 'installed AppImage differs'
    [[ $(stat -c %U "$appimage") == tester ]] || fail 'AppImage not owned by the user'
    grep -qx "Exec=\"$appimage\"" "$desktop" || fail 'desktop entry Exec'
    grep -qx "X-Snekkie-Version=$old_release" "$desktop" || fail 'desktop entry version'
    cmp -s "$icon" "$repo/assets/icon.png" 2>/dev/null || [[ -s $icon ]] || fail 'icon missing'
    expect_package ''
    config_kept

    echo '== AppImage rerun and update (format detected from the installation)'
    run_installer as_tester sh "$installer" --from "$old" --yes
    expect_status 0
    expect_output 'already installed and up to date'
    run_installer as_tester sh "$installer" --from "$new" --yes
    expect_status 0
    expect_output "Snekkie will be updated to $new_release (AppImage)"
    cmp -s "$appimage" "$new/Snekkie-$new_release-x86_64.AppImage" || fail 'AppImage not updated'
    grep -qx "X-Snekkie-Version=$new_release" "$desktop" || fail 'desktop entry not updated'
    expect_package ''

    echo '== AppImage uninstall'
    run_installer as_tester sh "$installer" --uninstall --yes
    expect_status 0
    [[ ! -e $appimage && ! -e $desktop && ! -e $icon ]] || fail 'AppImage files remain'
    config_kept

    echo '== Automatic choice for this distribution, download only'
    mkdir -p "$home/downloads"
    chown tester:tester "$home/downloads"
    run_installer as_tester sh "$installer" --from "$new" --download-only --dir "$home/downloads" --yes
    expect_status 0
    cmp -s "$home/downloads/$new_file" "$new/$new_file" || fail "$new_file not saved"
    expect_package ''
    # shellcheck source=/dev/null
    echo "PASS: installer lifecycle on $(. /etc/os-release && echo "$PRETTY_NAME")"
    exit 0
fi

if [[ $mode == flatpak ]]; then
    command -v flatpak >/dev/null || fail 'Flatpak is required.'
    # Reuse the runtimes package-flatpak.sh downloaded, isolated from the
    # account's real Flatpak installation.
    export FLATPAK_USER_DIR=$repo/target/flatpak-tools/installation
    app_id=io.github.Dynamitecrown.Snekkie
    remove_app() { flatpak --user uninstall --assumeyes --noninteractive "$app_id" >/dev/null 2>&1 || true; }
    trap remove_app EXIT
    remove_app
    [[ $(id -u) != 0 ]] || fail 'run the Flatpak check as a normal user'
    old_release=$(release_in "$old") || fail 'OLD has no Flatpak bundle'
    new_release=$(release_in "$new") || fail 'NEW has no Flatpak bundle'
    version_installed() { flatpak info --user "$app_id" 2>/dev/null | sed -n 's/^[[:space:]]*Version:[[:space:]]*//p'; }

    echo '== Install the older Flatpak bundle'
    run_installer sh "$installer" --from "$old" --format flatpak --yes
    expect_status 0
    [[ $(version_installed) == "$old_release" ]] || fail "expected Flatpak $old_release, got '$(version_installed)'"
    echo '== Rerun changes nothing'
    run_installer sh "$installer" --from "$old" --yes
    expect_status 0
    expect_output 'already installed and up to date'
    echo '== Update the installed bundle'
    run_installer sh "$installer" --from "$new" --yes
    expect_status 0
    expect_output "Snekkie will be updated to $new_release (Flatpak)"
    [[ $(version_installed) == "$new_release" ]] || fail "expected Flatpak $new_release, got '$(version_installed)'"
    echo '== Uninstall'
    run_installer sh "$installer" --uninstall --yes
    expect_status 0
    [[ -z $(version_installed) ]] || fail 'Flatpak still installed'
    echo 'PASS: installer Flatpak lifecycle'
    exit 0
fi

# macOS: a GitHub-hosted runner, whose /Applications is writable by its user.
app=/Applications/Snekkie.app
[[ ! -e $app && ! -e $HOME/Applications/Snekkie.app ]] || fail 'Snekkie.app already exists on this runner'
trap 'rm -rf "$app"' EXIT
plist_version() { defaults read "$app/Contents/Info" SnekkieVersion 2>/dev/null || true; }
old_version=$(release_in "$old") || fail 'OLD has no disk image'
new_version=$(release_in "$new") || fail 'NEW has no disk image'
old_dmg=Snekkie-$old_version-macos-universal.dmg

echo '== Install the older disk image'
run_installer sh "$installer" --from "$old" --yes
expect_status 0
[[ $(plist_version) == "$old_version" ]] || fail "expected $old_version, got '$(plist_version)'"
codesign --verify --strict "$app" || fail 'installed app signature'
[[ -z $(hdiutil info | grep -F "$old_dmg" || true) ]] || fail 'disk image left mounted'
echo '== Rerun changes nothing'
run_installer sh "$installer" --from "$old" --yes
expect_status 0
expect_output 'already installed and up to date'
echo '== Update'
run_installer sh "$installer" --from "$new" --yes
expect_status 0
expect_output "Snekkie will be updated to $new_version (macOS app)"
[[ $(plist_version) == "$new_version" ]] || fail "expected $new_version, got '$(plist_version)'"
[[ ! -e /Applications/.Snekkie.app.old && ! -e /Applications/.Snekkie.app.new ]] || fail 'staging copies remain'
echo '== Uninstall'
run_installer sh "$installer" --uninstall --yes
expect_status 0
[[ ! -e $app ]] || fail 'Snekkie.app remains'
echo 'PASS: installer macOS lifecycle'
