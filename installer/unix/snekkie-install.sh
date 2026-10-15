#!/bin/sh
# Snekkie online installer for Linux and macOS.
#
# Downloads the latest stable Snekkie release from GitHub, checks it against
# the release's SHA256SUMS and installs the package that suits this computer:
# .deb (apt), .rpm (dnf, yum or zypper), Flatpak, AppImage or the macOS disk
# image. Run it again to update. Saved sessions and settings are never touched.
#
#   sh snekkie-install.sh --help
#
# Everything runs from main() on the last line, so a partially downloaded copy
# of this file cannot run half a script.
#
# shellcheck shell=sh disable=SC3043

INSTALLER_VERSION=1.0.0
REPOSITORY=Dynamitecrown/Snekkie
APP_ID=io.github.Dynamitecrown.Snekkie
FLATHUB_REPO=https://dl.flathub.org/repo/flathub.flatpakrepo
# The Linux builds need this glibc (Ubuntu 22.04). Flatpak brings its own.
GLIBC_FLOOR=2.35
MACOS_FLOOR=11.0
MAX_PACKAGE_BYTES=536870912
NL='
'

say() { printf '%s\n' "$*"; }
warn() { printf '%s\n' "$*" >&2; }

die() {
    warn "Snekkie setup could not continue: $*"
    warn "Packages for every system are at https://github.com/$REPOSITORY/releases/latest"
    exit 2
}

usage() {
    cat <<USAGE
Snekkie installer $INSTALLER_VERSION for Linux and macOS

Usage: sh snekkie-install.sh [options]

Installs or updates Snekkie with the package that suits this computer.
Run it again at any time to update. Saved sessions and settings are kept.

Options:
  --format FORMAT    deb, rpm, flatpak, appimage or dmg (default: automatic)
  --release VERSION  Install that release instead of the latest stable one
  --download-only    Download and verify the package, but don't install it
  --dir DIRECTORY    Where --download-only saves the package (default: .)
  --from DIRECTORY   Install from a folder holding a downloaded package and
                     the SHA256SUMS file from the same release (no internet)
  --reinstall        Install even if this version is already installed
  --uninstall        Remove Snekkie, keeping saved sessions and settings
  -y, --yes          Don't ask for confirmation
  --version          Show the installer version
  -h, --help         Show this help

Automatic choice: apt-based systems (Debian, Ubuntu, Mint, Pop!_OS) get the
.deb; dnf, yum or zypper systems (Fedora, RHEL, openSUSE) get the .rpm;
Fedora Atomic and other image-based systems, and systems older than glibc
$GLIBC_FLOOR, get the Flatpak; any other x86_64 Linux gets the AppImage, and
macOS gets the universal disk image. An existing installation is updated in
the format it already uses.
USAGE
}

# True if the whole of $2 matches the extended regular expression $1.
matches() {
    case $2 in *"$NL"*) return 1 ;; esac
    printf '%s\n' "$2" | grep -Eq "^($1)\$"
}

ere_escape() {
    printf '%s' "$1" | sed 's/[][\.*^$+?(){}|/]/\\&/g'
}

# Succeeds if dotted version $1 is at least $2. Non-numeric parts count as 0.
version_at_least() {
    awk -v a="$1" -v b="$2" 'BEGIN {
        na = split(a, x, "."); nb = split(b, y, ".")
        n = na > nb ? na : nb
        for (i = 1; i <= n; i++) {
            p = (i <= na) ? x[i] + 0 : 0
            q = (i <= nb) ? y[i] + 0 : 0
            if (p > q) exit 0
            if (p < q) exit 1
        }
        exit 0
    }'
}

SEMVER='(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
PRERELEASE='-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*'

# The package version for the Debian and RPM file names, matching
# scripts/package-linux.sh: a pre-release hyphen sorts below the release as ~.
deb_version() { printf '%s' "$1" | sed 's/-/~/'; }

# The name of the release asset in FORMAT for app VERSION, as an ERE.
asset_pattern() {
    local format="$1" version="$2"
    case $format in
        deb) printf 'snekkie_%s-[0-9]+_amd64\\.deb' "$(ere_escape "$(deb_version "$version")")" ;;
        rpm) printf 'snekkie-%s-[0-9]+\\.x86_64\\.rpm' "$(ere_escape "$(deb_version "$version")")" ;;
        appimage) printf 'Snekkie-%s-x86_64\\.AppImage' "$(ere_escape "$version")" ;;
        flatpak) printf 'Snekkie-%s-x86_64\\.flatpak' "$(ere_escape "$version")" ;;
        dmg) printf 'Snekkie-%s-macos-universal\\.dmg' "$(ere_escape "$version")" ;;
        *) return 1 ;;
    esac
}

format_name() {
    case $1 in
        deb) printf '.deb package' ;;
        rpm) printf '.rpm package' ;;
        appimage) printf 'AppImage' ;;
        flatpak) printf 'Flatpak' ;;
        dmg) printf 'macOS app' ;;
    esac
}

# Print "HASH NAME" for the single SHA256SUMS line naming the FORMAT asset of
# VERSION. Fails if the release has none, or more than one.
select_asset() {
    local format="$1" version="$2" sums="$3" pattern lines count
    pattern=$(asset_pattern "$format" "$version") || return 1
    lines=$(grep -E "^[0-9a-f]{64} [ *]${pattern}\$" "$sums") || return 1
    count=$(printf '%s\n' "$lines" | wc -l | tr -d ' ')
    [ "$count" = 1 ] || return 1
    printf '%s\n' "$lines" | sed 's/^\([0-9a-f]*\) [ *]\(.*\)$/\1 \2/'
}

# The single app version that SHA256SUMS describes, for --from.
version_from_sums() {
    local sums="$1" versions
    # Separate expressions: BSD sed on macOS has no \| alternation.
    versions=$(sed -n \
        -e 's/^[0-9a-f]\{64\} [ *]Snekkie-\(.*\)-x86_64\.AppImage$/\1/p' \
        -e 's/^[0-9a-f]\{64\} [ *]Snekkie-\(.*\)-x86_64\.flatpak$/\1/p' \
        -e 's/^[0-9a-f]\{64\} [ *]Snekkie-\(.*\)-macos-universal\.dmg$/\1/p' \
        -e 's/^[0-9a-f]\{64\} [ *]snekkie_\(.*\)-[0-9][0-9]*_amd64\.deb$/\1/p' "$sums" |
        sed 's/~/-/' | sort -u)
    [ -n "$versions" ] || return 1
    case $versions in *"$NL"*) return 1 ;; esac
    matches "$SEMVER($PRERELEASE)?" "$versions" || return 1
    printf '%s\n' "$versions"
}

# Read the tag from GitHub's JSON description of a release and print the
# version. Only a published, stable x.y.z release qualifies unless
# $2 = any, which also accepts a requested pre-release. String values are
# JSON-escaped, so a quoted key can't be faked from inside release notes.
release_version_from_json() {
    local json="$1" accept="${2:-stable}" tag version
    grep -Eq '"draft"[[:space:]]*:[[:space:]]*false' "$json" || return 1
    if [ "$accept" = stable ]; then
        grep -Eq '"prerelease"[[:space:]]*:[[:space:]]*false' "$json" || return 1
    fi
    tag=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"\\]*\)".*/\1/p' "$json" | head -n 1)
    version=${tag#v}
    [ "v$version" = "$tag" ] || return 1
    if [ "$accept" = stable ]; then
        matches "$SEMVER" "$version" || return 1
    else
        matches "$SEMVER($PRERELEASE)?" "$version" || return 1
    fi
    printf '%s\n' "$version"
}

# Pick a format. Arguments: os, installed format, apt, rpm manager, ostree,
# glibc is new enough, flatpak available (yes or empty for no).
choose_format() {
    local os="$1" installed="$2" apt="$3" rpm_manager="$4" ostree="$5" glibc_ok="$6" flatpak="$7"
    if [ "$os" = Darwin ]; then
        printf 'dmg\n'
    elif [ -n "$installed" ]; then
        printf '%s\n' "$installed"
    elif [ -n "$ostree" ] || [ -z "$glibc_ok" ]; then
        [ -n "$flatpak" ] || return 1
        printf 'flatpak\n'
    elif [ -n "$apt" ]; then
        printf 'deb\n'
    elif [ -n "$rpm_manager" ]; then
        printf 'rpm\n'
    else
        printf 'appimage\n'
    fi
}

# -- this computer ------------------------------------------------------------

have() { command -v "$1" >/dev/null 2>&1; }

glibc_version() {
    local text
    text=$(getconf GNU_LIBC_VERSION 2>/dev/null) || text=
    case $text in
        glibc\ *) printf '%s\n' "${text#glibc }" ;;
        *) ldd --version 2>/dev/null | sed -n '1s/.*[^0-9.]\([0-9][0-9]*\.[0-9][0-9]*\)$/\1/p' ;;
    esac
}

rpm_manager() {
    have rpm || return 0
    for tool in dnf yum zypper; do
        if have "$tool"; then
            printf '%s\n' "$tool"
            return 0
        fi
    done
}

data_home() {
    case ${XDG_DATA_HOME:-} in
        /*) printf '%s\n' "$XDG_DATA_HOME" ;;
        *) printf '%s\n' "$HOME/.local/share" ;;
    esac
}

appimage_path() { printf '%s\n' "$HOME/.local/bin/snekkie"; }
appimage_desktop() { printf '%s\n' "$(data_home)/applications/snekkie.desktop"; }
appimage_icon() { printf '%s\n' "$(data_home)/icons/hicolor/512x512/apps/snekkie.png"; }

mac_app() {
    local dir
    for dir in /Applications "$HOME/Applications"; do
        if [ -d "$dir/Snekkie.app" ]; then
            printf '%s\n' "$dir/Snekkie.app"
            return 0
        fi
    done
    return 1
}

flatpak_scope() {
    have flatpak || return 1
    if flatpak info --user "$APP_ID" >/dev/null 2>&1; then
        printf 'user\n'
    elif flatpak info --system "$APP_ID" >/dev/null 2>&1; then
        printf 'system\n'
    else
        return 1
    fi
}

installed_format() {
    if [ "$OS" = Darwin ]; then
        mac_app >/dev/null && printf 'dmg\n'
        return 0
    fi
    if have dpkg-query && dpkg-query -W -f='${Status}' snekkie 2>/dev/null | grep -q 'install ok installed'; then
        printf 'deb\n'
    elif have rpm && rpm -q snekkie >/dev/null 2>&1; then
        printf 'rpm\n'
    elif flatpak_scope >/dev/null; then
        printf 'flatpak\n'
    elif [ -f "$(appimage_desktop)" ] && grep -q '^X-Snekkie-Installer=appimage$' "$(appimage_desktop)"; then
        printf 'appimage\n'
    fi
}

installed_version() {
    case $1 in
        deb) dpkg-query -W -f='${Version}' snekkie 2>/dev/null | sed 's/-[0-9]*$//; s/~/-/' ;;
        # rpm -q reports a missing package on stdout, so check first.
        rpm) rpm -q snekkie >/dev/null 2>&1 && rpm -q --queryformat '%{VERSION}' snekkie | sed 's/~/-/' ;;
        flatpak) flatpak info "--$(flatpak_scope)" "$APP_ID" 2>/dev/null | sed -n 's/^[[:space:]]*Version:[[:space:]]*//p' ;;
        appimage) sed -n 's/^X-Snekkie-Version=//p' "$(appimage_desktop)" 2>/dev/null ;;
        dmg) defaults read "$(mac_app)/Contents/Info" SnekkieVersion 2>/dev/null ;;
    esac
}

# -- downloading --------------------------------------------------------------

# Fetch https URL $1 into file $2, allowing at most $3 bytes.
fetch() {
    local url="$1" output="$2" limit="$3" progress="--silent"
    case $url in https://*) ;; *) die "refusing to download from a non-HTTPS address." ;; esac
    if have curl; then
        [ "$limit" -gt 1048576 ] && [ -t 2 ] && progress=--progress-bar
        curl --fail $progress --show-error --location --proto '=https' --proto-redir '=https' \
            --retry 3 --connect-timeout 20 --max-filesize "$limit" \
            --user-agent "Snekkie-Installer/$INSTALLER_VERSION" \
            --header 'Accept: application/vnd.github+json, application/octet-stream' \
            --output "$output" "$url"
    else
        wget --quiet --tries=3 --timeout=20 --user-agent="Snekkie-Installer/$INSTALLER_VERSION" \
            --output-document="$output" "$url"
    fi
}

sha256_of() {
    if have sha256sum; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif have shasum; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        openssl dgst -sha256 -r "$1" | cut -d ' ' -f 1
    fi
}

latest_version() {
    local json="$WORK/release.json" api accept="stable"
    if [ -n "$RELEASE" ]; then
        api="https://api.github.com/repos/$REPOSITORY/releases/tags/v$RELEASE"
        accept=any
    else
        api="https://api.github.com/repos/$REPOSITORY/releases/latest"
    fi
    fetch "$api" "$json" 2097152 ||
        die "GitHub did not describe the release. It may be limiting requests; try again later."
    VERSION=$(release_version_from_json "$json" "$accept") ||
        die "GitHub did not return a published Snekkie release${RELEASE:+ called v$RELEASE}."
    if [ -n "$RELEASE" ] && [ "$VERSION" != "$RELEASE" ]; then
        die "GitHub returned release $VERSION instead of $RELEASE."
    fi
}

# -- installing ---------------------------------------------------------------

as_root() {
    if [ "$(id -u)" = 0 ]; then
        "$@"
    elif have sudo; then
        sudo "$@"
    elif have doas; then
        doas "$@"
    else
        die "administrator rights are needed. Install sudo, or run this as root."
    fi
}

# Show what is about to happen, then ask unless --yes was given.
confirm() {
    say "$1"
    [ -n "$ASSUME_YES" ] && return 0
    if ! (: </dev/tty) 2>/dev/null; then
        die "there is no terminal to confirm with. Rerun with --yes to continue without asking."
    fi
    printf 'Continue? [Y/n] ' >/dev/tty
    read -r answer </dev/tty || answer=n
    case $answer in
        '' | [Yy] | [Yy][Ee][Ss]) return 0 ;;
    esac
    say 'Setup canceled. Nothing was changed.'
    exit 1
}

install_deb() {
    as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y "$1"
}

install_rpm() {
    case $(rpm_manager) in
        dnf) as_root dnf install -y "$1" ;;
        yum) as_root yum install -y "$1" ;;
        # GitHub's packages aren't signed by a distribution key: the SHA-256
        # check above is what verifies this one.
        zypper) as_root zypper --non-interactive install --allow-unsigned-rpm "$1" ;;
        *) die 'no RPM package manager (dnf, yum or zypper) was found.' ;;
    esac
}

install_flatpak() {
    local scope="--user"
    [ "$(id -u)" = 0 ] && scope=--system
    # The bundle names Flathub for its runtime; add it if it's missing.
    flatpak remote-add "$scope" --if-not-exists flathub "$FLATHUB_REPO" ||
        die "could not add Flathub, which provides Snekkie's Flatpak runtime."
    local reinstall=''
    flatpak info "$scope" "$APP_ID" >/dev/null 2>&1 && reinstall=--reinstall
    flatpak install "$scope" --noninteractive --assumeyes $reinstall --bundle "$1"
}

# Refuse home directories that can't be written safely into a desktop entry.
check_desktop_path() {
    case $1 in
        *[\"\`\$\\%]* | *"$NL"*) die "the path $1 can't be used in a desktop entry. Try --format flatpak." ;;
    esac
}

install_appimage() {
    local file="$1" target desktop icon extract
    target=$(appimage_path)
    desktop=$(appimage_desktop)
    icon=$(appimage_icon)
    check_desktop_path "$target"
    mkdir -p "$(dirname "$target")" "$(dirname "$desktop")" "$(dirname "$icon")" || return 1
    # Replace atomically: a running copy keeps working until it is closed.
    if ! { cp "$file" "$target.new.$$" && chmod 0755 "$target.new.$$" && mv -f "$target.new.$$" "$target"; }; then
        rm -f "$target.new.$$"
        return 1
    fi
    extract=$WORK/extract
    mkdir -p "$extract"
    if (cd "$extract" && "$target" --appimage-extract 'usr/share/icons/hicolor/512x512/apps/snekkie.png' >/dev/null 2>&1) &&
        [ -s "$extract/squashfs-root/usr/share/icons/hicolor/512x512/apps/snekkie.png" ]; then
        cp "$extract/squashfs-root/usr/share/icons/hicolor/512x512/apps/snekkie.png" "$icon.new.$$"
        mv -f "$icon.new.$$" "$icon"
    else
        warn 'Note: the menu icon could not be extracted. Snekkie itself is installed.'
    fi
    cat >"$desktop.new.$$" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Snekkie
Comment=SSH and serial terminal
Exec="$target"
TryExec=$target
Icon=snekkie
Terminal=false
Categories=Network;TerminalEmulator;
Keywords=SSH;Serial;Telnet;Console;
StartupWMClass=snekkie
X-Snekkie-Installer=appimage
X-Snekkie-Version=$VERSION
DESKTOP
    mv -f "$desktop.new.$$" "$desktop" || return 1
    if have update-desktop-database; then
        update-desktop-database "$(dirname "$desktop")" >/dev/null 2>&1 || true
    fi
    if ! have fusermount3 && ! have fusermount; then
        warn 'Note: AppImages need FUSE. Install your distribution'\''s fuse3 package if Snekkie does not start.'
    fi
    case :$PATH: in
        *":$(dirname "$target"):"*) ;;
        *) warn "Note: $(dirname "$target") is not on your PATH, so start Snekkie from the application menu." ;;
    esac
    return 0
}

# Run a command, as root only when the destination needs it.
in_place() {
    local dir="$1"
    shift
    if [ -w "$dir" ]; then "$@"; else as_root "$@"; fi
}

install_dmg() {
    local file="$1" mount="$WORK/mount" existing destination app
    mkdir -p "$mount"
    hdiutil attach -nobrowse -noautoopen -readonly -mountpoint "$mount" "$file" >/dev/null ||
        die 'could not open the disk image.'
    MOUNTED=$mount
    [ -d "$mount/Snekkie.app" ] || die 'the disk image does not contain Snekkie.app.'
    if existing=$(mac_app); then
        destination=$(dirname "$existing")
    elif [ -w /Applications ]; then
        destination=/Applications
    else
        destination=$HOME/Applications
        mkdir -p "$destination"
    fi
    app=$destination/Snekkie.app
    in_place "$destination" rm -rf "$destination/.Snekkie.app.new" "$destination/.Snekkie.app.old" || return 1
    in_place "$destination" ditto "$mount/Snekkie.app" "$destination/.Snekkie.app.new" || return 1
    if [ -d "$app" ]; then
        in_place "$destination" mv "$app" "$destination/.Snekkie.app.old" || return 1
    fi
    if ! in_place "$destination" mv "$destination/.Snekkie.app.new" "$app"; then
        [ -d "$destination/.Snekkie.app.old" ] && in_place "$destination" mv "$destination/.Snekkie.app.old" "$app"
        return 1
    fi
    in_place "$destination" rm -rf "$destination/.Snekkie.app.old"
    hdiutil detach "$mount" -quiet && MOUNTED=
    INSTALLED_APP=$app
    return 0
}

uninstall() {
    local format
    format=$(installed_format)
    [ -n "$format" ] || {
        say 'Snekkie is not installed by a package this installer manages. Nothing was changed.'
        exit 0
    }
    confirm "Snekkie ($(format_name "$format")) will be removed. Saved sessions and settings are kept."
    case $format in
        deb) as_root env DEBIAN_FRONTEND=noninteractive apt-get remove -y snekkie ;;
        rpm)
            case $(rpm_manager) in
                dnf) as_root dnf remove -y snekkie ;;
                yum) as_root yum remove -y snekkie ;;
                zypper) as_root zypper --non-interactive remove snekkie ;;
                *) as_root rpm -e snekkie ;;
            esac
            ;;
        flatpak) flatpak uninstall "--$(flatpak_scope)" --noninteractive --assumeyes "$APP_ID" ;;
        appimage)
            rm -f "$(appimage_path)" "$(appimage_desktop)" "$(appimage_icon)" || die "could not remove the AppImage files."
            if have update-desktop-database; then
                update-desktop-database "$(dirname "$(appimage_desktop)")" >/dev/null 2>&1 || true
            fi
            ;;
        dmg)
            local app
            app=$(mac_app)
            in_place "$(dirname "$app")" rm -rf "$app"
            ;;
    esac || die 'the package manager could not remove Snekkie.'
    say 'Snekkie was removed. Saved sessions and settings were kept.'
}

cleanup() {
    if [ -n "${MOUNTED:-}" ]; then
        hdiutil detach "$MOUNTED" -quiet -force 2>/dev/null || true
    fi
    if [ -n "${WORK:-}" ] && [ -d "$WORK" ]; then
        rm -rf "$WORK"
    fi
}

main() {
    FORMAT='' RELEASE='' DOWNLOAD_ONLY='' SAVE_DIR=. FROM='' REINSTALL='' UNINSTALL='' ASSUME_YES=''
    while [ $# -gt 0 ]; do
        case $1 in
            --format) [ $# -ge 2 ] || die '--format needs a value.'; FORMAT=$2; shift ;;
            --format=*) FORMAT=${1#*=} ;;
            --release) [ $# -ge 2 ] || die '--release needs a version.'; RELEASE=${2#v}; shift ;;
            --release=*) RELEASE=${1#*=}; RELEASE=${RELEASE#v} ;;
            --download-only) DOWNLOAD_ONLY=1 ;;
            --dir) [ $# -ge 2 ] || die '--dir needs a folder.'; SAVE_DIR=$2; shift ;;
            --dir=*) SAVE_DIR=${1#*=} ;;
            --from) [ $# -ge 2 ] || die '--from needs a folder.'; FROM=$2; shift ;;
            --from=*) FROM=${1#*=} ;;
            --reinstall) REINSTALL=1 ;;
            --uninstall) UNINSTALL=1 ;;
            -y | --yes) ASSUME_YES=1 ;;
            --version) say "Snekkie installer $INSTALLER_VERSION"; exit 0 ;;
            -h | --help) usage; exit 0 ;;
            *) warn "Unknown option: $1"; usage >&2; exit 2 ;;
        esac
        shift
    done
    case $FORMAT in '' | deb | rpm | appimage | flatpak | dmg) ;; *) die "unknown format $FORMAT." ;; esac
    if [ -n "$RELEASE" ] && ! matches "$SEMVER($PRERELEASE)?" "$RELEASE"; then
        die "$RELEASE is not a Snekkie version number."
    fi
    [ -n "${HOME:-}" ] || die 'HOME is not set.'

    OS=$(uname -s)
    case $OS in
        Linux)
            case $(uname -m) in
                x86_64 | amd64) ;;
                *) die "Snekkie for Linux is built for x86_64 (64-bit Intel and AMD) computers, not $(uname -m)." ;;
            esac
            ;;
        Darwin)
            local macos
            macos=$(sw_vers -productVersion 2>/dev/null) || macos=0
            version_at_least "$macos" "$MACOS_FLOOR" || die "Snekkie needs macOS $MACOS_FLOOR or later."
            ;;
        *) die "this installer supports Linux and macOS. On Windows, use Snekkie-Setup.exe." ;;
    esac

    WORK=
    MOUNTED=
    trap cleanup EXIT
    trap 'exit 1' INT TERM HUP
    if [ -n "$UNINSTALL" ]; then
        uninstall
        exit 0
    fi

    have sha256sum || have shasum || have openssl || die 'sha256sum, shasum or openssl is needed to verify downloads.'
    if [ -z "$FROM" ] && ! have curl && ! have wget; then
        die 'curl or wget is needed to download Snekkie.'
    fi
    WORK=$(mktemp -d "${TMPDIR:-/tmp}/snekkie-install.XXXXXX") || die 'could not create a temporary folder.'
    # apt reads local packages as its unprivileged _apt user.
    chmod 0755 "$WORK"

    local installed glibc glibc_ok='' ostree='' flatpak=''
    installed=$(installed_format)
    if [ "$OS" = Linux ]; then
        glibc=$(glibc_version)
        [ -n "$glibc" ] && version_at_least "$glibc" "$GLIBC_FLOOR" && glibc_ok=yes
        [ -e /run/ostree-booted ] && ostree=yes
        have flatpak && flatpak=yes
    fi
    if [ -z "$FORMAT" ]; then
        local apt=''
        have apt-get && have dpkg && apt=yes
        FORMAT=$(choose_format "$OS" "$installed" "$apt" "$(rpm_manager)" "$ostree" "$glibc_ok" "$flatpak") ||
            die "this system needs the Flatpak version of Snekkie (glibc ${glibc:-unknown} is older than $GLIBC_FLOOR, or the system is image-based). Install Flatpak from https://flatpak.org/setup/ and run this again."
    fi
    case $OS:$FORMAT in
        Darwin:dmg | Linux:deb | Linux:rpm | Linux:appimage | Linux:flatpak) ;;
        *) die "the $FORMAT format is not available on $OS." ;;
    esac
    if [ -z "$DOWNLOAD_ONLY" ]; then
        case $FORMAT in
            deb) have apt-get || die 'the .deb needs apt-get. Try --format appimage.' ;;
            rpm) [ -n "$(rpm_manager)" ] || die 'the .rpm needs dnf, yum or zypper. Try --format appimage.' ;;
            flatpak) have flatpak || die 'Flatpak is not installed. See https://flatpak.org/setup/' ;;
            appimage) [ "$(id -u)" != 0 ] || die 'install the AppImage as your normal user, not as root.' ;;
        esac
        case $FORMAT in
            deb | rpm | appimage)
                [ -n "$glibc_ok" ] ||
                    die "the $(format_name "$FORMAT") needs glibc $GLIBC_FLOOR or newer (found ${glibc:-none}). Use --format flatpak instead."
                ;;
        esac
        if [ -n "$installed" ] && [ "$installed" != "$FORMAT" ]; then
            warn "Note: Snekkie is also installed as a $(format_name "$installed"), which stays installed. Remove it with --uninstall first to keep one copy."
        fi
    fi

    local sums="$WORK/SHA256SUMS"
    if [ -n "$FROM" ]; then
        [ -f "$FROM/SHA256SUMS" ] || die "$FROM has no SHA256SUMS file. Download it from the same release."
        cp "$FROM/SHA256SUMS" "$sums"
        if [ -n "$RELEASE" ]; then
            VERSION=$RELEASE
        else
            VERSION=$(version_from_sums "$sums") || die "SHA256SUMS in $FROM does not name one Snekkie version. Use --release."
        fi
    else
        say 'Checking GitHub for the latest Snekkie release...'
        latest_version
        fetch "https://github.com/$REPOSITORY/releases/download/v$VERSION/SHA256SUMS" "$sums" 65536 ||
            die "could not download SHA256SUMS for Snekkie $VERSION. Releases up to 2.6.0 have no Linux or macOS packages."
    fi

    if [ -z "$DOWNLOAD_ONLY" ] && [ -z "$REINSTALL" ] && [ "$installed" = "$FORMAT" ] &&
        [ "$(installed_version "$FORMAT")" = "$VERSION" ]; then
        say "Snekkie $VERSION is already installed and up to date."
        exit 0
    fi

    local asset expected name
    asset=$(select_asset "$FORMAT" "$VERSION" "$sums") ||
        die "Snekkie $VERSION has no single $(format_name "$FORMAT") in its release."
    expected=${asset%% *}
    name=${asset#* }

    if [ -n "$DOWNLOAD_ONLY" ]; then
        [ -d "$SAVE_DIR" ] || die "the folder $SAVE_DIR does not exist."
        confirm "$name (Snekkie $VERSION) will be downloaded, verified and saved in $SAVE_DIR."
    else
        local how
        case $FORMAT in
            deb) how='with apt-get (needs administrator rights)' ;;
            rpm) how="with $(rpm_manager) (needs administrator rights)" ;;
            flatpak)
                how='for this user, adding Flathub for its runtime if needed'
                [ "$(id -u)" = 0 ] && how='for all users, adding Flathub for its runtime if needed'
                ;;
            appimage) how="for this user in $(appimage_path)" ;;
            dmg) how='as Snekkie.app in Applications' ;;
        esac
        if [ -n "$installed" ] && [ "$installed" = "$FORMAT" ]; then
            confirm "Snekkie will be updated to $VERSION ($(format_name "$FORMAT")) $how."
        else
            confirm "Snekkie $VERSION will be installed ($(format_name "$FORMAT")) $how."
        fi
    fi

    local package="$WORK/$name" actual
    if [ -n "$FROM" ]; then
        [ -f "$FROM/$name" ] || die "$FROM does not contain $name."
        cp "$FROM/$name" "$package"
    else
        say "Downloading $name..."
        fetch "https://github.com/$REPOSITORY/releases/download/v$VERSION/$name" "$package" "$MAX_PACKAGE_BYTES" ||
            die "the download of $name failed."
    fi
    actual=$(sha256_of "$package")
    [ "$actual" = "$expected" ] || die "$name does not match the release's SHA-256 checksum. It was not installed."
    chmod 0644 "$package"
    say "Verified $name (SHA-256 $actual)."

    if [ -n "$DOWNLOAD_ONLY" ]; then
        if ! { cp "$package" "$SAVE_DIR/.$name.part.$$" && mv -f "$SAVE_DIR/.$name.part.$$" "$SAVE_DIR/$name"; }; then
            rm -f "$SAVE_DIR/.$name.part.$$"
            die "could not save $name in $SAVE_DIR."
        fi
        say "Saved $SAVE_DIR/$name"
        exit 0
    fi

    case $FORMAT in
        deb) install_deb "$package" ;;
        rpm) install_rpm "$package" ;;
        flatpak) install_flatpak "$package" ;;
        appimage) install_appimage "$package" ;;
        dmg) install_dmg "$package" ;;
    esac || die "the $(format_name "$FORMAT") could not be installed."

    say ''
    say "Snekkie $VERSION is installed."
    case $FORMAT in
        flatpak) say "Start it from the application menu, or run: flatpak run $APP_ID" ;;
        dmg) say "Start it from Launchpad or $INSTALLED_APP" ;;
        *) say 'Start it from the application menu, or run: snekkie' ;;
    esac
    if have pgrep && pgrep -x snekkie >/dev/null 2>&1; then
        say 'Snekkie is running: restart it to use the new version.'
    fi
    say 'To update later, run this installer again.'
}

# Tests source the functions without running anything.
if [ "${SNEKKIE_INSTALL_SOURCE_ONLY:-}" != 1 ]; then
    main "$@"
fi
