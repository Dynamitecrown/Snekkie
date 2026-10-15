#!/bin/sh
# Offline checks of installer/unix/snekkie-install.sh: release parsing, asset
# selection, format choice and refusal paths. No network, no installation.
# Run it with each shell users may have as sh:
#   for shell in dash bash busybox zsh; do sh scripts/test-install-script-unit.sh; done
# shellcheck shell=sh disable=SC2016
set -u
repo=$(cd "$(dirname "$0")/.." && pwd)
installer=$repo/installer/unix/snekkie-install.sh
shell=${SNEKKIE_TEST_SHELL:-sh}
SNEKKIE_INSTALL_SOURCE_ONLY=1
export SNEKKIE_INSTALL_SOURCE_ONLY
# shellcheck source=/dev/null
. "$installer"
unset SNEKKIE_INSTALL_SOURCE_ONLY

failures=0
checks=0
fail() {
    printf 'FAIL: %s\n' "$*"
    failures=$((failures + 1))
}
ok() {
    checks=$((checks + 1))
    "$@" || fail "expected success: $*"
}
no() {
    checks=$((checks + 1))
    if "$@"; then fail "expected failure: $*"; fi
}
same() {
    checks=$((checks + 1))
    [ "$2" = "$3" ] || fail "$1: expected [$2], got [$3]"
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/snekkie-install-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# -- versions -------------------------------------------------------------
ok version_at_least 2.35 2.35
ok version_at_least 2.39 2.35
ok version_at_least 2.100 2.35
no version_at_least 2.34 2.35
no version_at_least 2.4 2.35
ok version_at_least 15.0.1 11.0
ok version_at_least 11 11.0
no version_at_least 10.15.7 11.0
ok matches "$SEMVER" 2.7.0
ok matches "$SEMVER" 10.0.12
no matches "$SEMVER" 2.7
no matches "$SEMVER" 02.7.0
no matches "$SEMVER" "2.7.0$NL"
no matches "$SEMVER" "2.7.0${NL}9.9.9"
no matches "$SEMVER" 2.7.0-beta.1
ok matches "$SEMVER($PRERELEASE)?" 2.7.0-beta.1
no matches "$SEMVER($PRERELEASE)?" '2.7.0-beta..1'
no matches "$SEMVER($PRERELEASE)?" '2.7.0-be;ta'
same 'Debian version' 2.7.0 "$(deb_version 2.7.0)"
same 'Debian pre-release' '2.7.0~beta.1' "$(deb_version 2.7.0-beta.1)"
same 'escaped dots' '2\.7\.0' "$(ere_escape 2.7.0)"

# -- GitHub release descriptions ------------------------------------------
cat >"$tmp/pretty.json" <<'JSON'
{
  "url": "https://api.github.com/repos/Dynamitecrown/Snekkie/releases/1",
  "tag_name": "v2.7.0",
  "name": "Snekkie 2.7.0",
  "draft": false,
  "prerelease": false,
  "author": { "login": "Dynamitecrown" },
  "body": "Notes mention \"tag_name\": \"v9.9.9\" and \"prerelease\": false."
}
JSON
same 'pretty release' 2.7.0 "$(release_version_from_json "$tmp/pretty.json")"
printf '%s' '{"body":"x \"tag_name\": \"v9.9.9\" \"draft\": false","tag_name":"v2.7.1","draft":false,"prerelease":false,"assets":[]}' >"$tmp/compact.json"
same 'compact release, escaped quotes in notes ignored' 2.7.1 "$(release_version_from_json "$tmp/compact.json")"
sed 's/"prerelease": false/"prerelease": true/' "$tmp/pretty.json" >"$tmp/pre.json"
no release_version_from_json "$tmp/pre.json"
sed 's/"tag_name": "v2.7.0"/"tag_name": "v2.8.0-beta.1"/' "$tmp/pre.json" >"$tmp/beta.json"
no release_version_from_json "$tmp/beta.json"
same 'requested pre-release' 2.8.0-beta.1 "$(release_version_from_json "$tmp/beta.json" any)"
sed 's/"draft": false/"draft": true/' "$tmp/pretty.json" >"$tmp/draft.json"
no release_version_from_json "$tmp/draft.json"
no release_version_from_json "$tmp/draft.json" any
for tag in 2.7.0 v2.7 vv2.7.0 'v2.7.0 ' 'v2.7.0;rm' 'v../2.7.0'; do
    sed "s|\"tag_name\": \"v2.7.0\"|\"tag_name\": \"$tag\"|" "$tmp/pretty.json" >"$tmp/tag.json"
    no release_version_from_json "$tmp/tag.json"
done
printf '{"message":"API rate limit exceeded"}' >"$tmp/limited.json"
no release_version_from_json "$tmp/limited.json"
: >"$tmp/empty.json"
no release_version_from_json "$tmp/empty.json"

# -- SHA256SUMS -----------------------------------------------------------
h1=1111111111111111111111111111111111111111111111111111111111111111
h2=2222222222222222222222222222222222222222222222222222222222222222
h3=3333333333333333333333333333333333333333333333333333333333333333
h4=4444444444444444444444444444444444444444444444444444444444444444
h5=5555555555555555555555555555555555555555555555555555555555555555
h6=6666666666666666666666666666666666666666666666666666666666666666
cat >"$tmp/SHA256SUMS" <<SUMS
$h1  Snekkie-Setup-2.7.0.exe
$h2  snekkie_2.7.0-1_amd64.deb
$h3  snekkie-2.7.0-1.x86_64.rpm
$h4 *Snekkie-2.7.0-x86_64.AppImage
$h5  Snekkie-2.7.0-x86_64.flatpak
$h6  Snekkie-2.7.0-macos-universal.dmg
$h1  snekkie-linux-x86_64.tar.gz
$h1  snekkie_2.7.0.1-1_amd64.deb
$h1  snekkie_2x7x0-1_amd64.deb
SUMS
same deb "$h2 snekkie_2.7.0-1_amd64.deb" "$(select_asset deb 2.7.0 "$tmp/SHA256SUMS")"
same rpm "$h3 snekkie-2.7.0-1.x86_64.rpm" "$(select_asset rpm 2.7.0 "$tmp/SHA256SUMS")"
same 'binary-mode AppImage line' "$h4 Snekkie-2.7.0-x86_64.AppImage" "$(select_asset appimage 2.7.0 "$tmp/SHA256SUMS")"
same flatpak "$h5 Snekkie-2.7.0-x86_64.flatpak" "$(select_asset flatpak 2.7.0 "$tmp/SHA256SUMS")"
same dmg "$h6 Snekkie-2.7.0-macos-universal.dmg" "$(select_asset dmg 2.7.0 "$tmp/SHA256SUMS")"
no select_asset deb 2.6.0 "$tmp/SHA256SUMS"
no select_asset exe 2.7.0 "$tmp/SHA256SUMS"
printf '%s  snekkie_2.7.0-2_amd64.deb\n' "$h1" >>"$tmp/SHA256SUMS"
no select_asset deb 2.7.0 "$tmp/SHA256SUMS"
printf '%s  snekkie-2.7.0-1.x86_64.rpm\n' "$(printf %s "$h3" | tr 3 C)" >"$tmp/upper"
no select_asset rpm 2.7.0 "$tmp/upper"
printf '%s  Snekkie-2.7.0-x86_64.AppImage\r\n' "$h4" >"$tmp/crlf"
no select_asset appimage 2.7.0 "$tmp/crlf"
printf '%s  snekkie_2.8.0~rc.1-1_amd64.deb\n%s  snekkie-2.8.0~rc.1-1.x86_64.rpm\n' "$h2" "$h3" >"$tmp/rc"
same 'pre-release deb' "$h2 snekkie_2.8.0~rc.1-1_amd64.deb" "$(select_asset deb 2.8.0-rc.1 "$tmp/rc")"
same 'pre-release rpm' "$h3 snekkie-2.8.0~rc.1-1.x86_64.rpm" "$(select_asset rpm 2.8.0-rc.1 "$tmp/rc")"
same 'version from release checksums' 2.8.0-rc.1 "$(version_from_sums "$tmp/rc")"
head -n 6 "$tmp/SHA256SUMS" >"$tmp/one-version"
same 'version from one release' 2.7.0 "$(version_from_sums "$tmp/one-version")"
cat "$tmp/one-version" "$tmp/rc" >"$tmp/mixed"
no version_from_sums "$tmp/mixed"
no version_from_sums "$tmp/empty.json"

# -- automatic format choice ----------------------------------------------
same 'macOS' dmg "$(choose_format Darwin '' '' '' '' '' '')"
same 'Debian/Ubuntu' deb "$(choose_format Linux '' yes dnf '' yes yes)"
same 'Fedora/openSUSE' rpm "$(choose_format Linux '' '' dnf '' yes yes)"
same 'other glibc Linux' appimage "$(choose_format Linux '' '' '' '' yes yes)"
same 'image-based Fedora' flatpak "$(choose_format Linux '' '' dnf yes yes yes)"
same 'old glibc' flatpak "$(choose_format Linux '' '' dnf '' '' yes)"
no choose_format Linux '' '' dnf '' '' ''
no choose_format Linux '' '' '' yes yes ''
same 'existing installation wins' appimage "$(choose_format Linux appimage yes dnf '' yes yes)"
same 'existing Flatpak on old glibc' flatpak "$(choose_format Linux flatpak '' '' '' '' yes)"

# -- refusals that must not reach the network or a package manager ----------
checks=$((checks + 1))
(fetch http://example.invalid/x "$tmp/never" 10) >/dev/null 2>&1
same 'plain HTTP refused' 2 "$?"
for path in '/home/a"b/.local/bin/snekkie' '/home/a`b/x' '/home/$x/y' '/home/a\b/x' '/home/100%/x'; do
    (check_desktop_path "$path") >/dev/null 2>&1
    same "desktop path $path refused" 2 "$?"
done
(check_desktop_path '/home/Jane Doe/.local/bin/snekkie') >/dev/null 2>&1
same 'spaces are quoted, not refused' 0 "$?"
printf abc >"$tmp/abc"
same 'SHA-256' ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad "$(sha256_of "$tmp/abc")"

# shellcheck disable=SC2086 # "busybox sh" is a command and an argument
run() { $shell "$installer" "$@" >"$tmp/out" 2>&1; }
run --version
same '--version exit' 0 "$?"
same '--version text' 'Snekkie installer 1.0.0' "$(cat "$tmp/out")"
run --help
same '--help exit' 0 "$?"
grep -q -- '--download-only' "$tmp/out" || fail '--help lists options'
run --format exe
same 'unknown format' 2 "$?"
run --release 2.7
same 'invalid release' 2 "$?"
run --release '2.7.0;id'
same 'release injection' 2 "$?"
run --bogus
same 'unknown option' 2 "$?"
run --from
same 'missing --from value' 2 "$?"
mkdir "$tmp/no-sums"
format=appimage
[ "$(uname -s)" = Darwin ] && format=dmg
if [ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || [ "$(uname -s)" = Darwin ]; then
    run --from "$tmp/no-sums" --yes --format "$format" --download-only --dir "$tmp"
    same 'missing SHA256SUMS' 2 "$?"
    grep -q 'has no SHA256SUMS' "$tmp/out" || fail 'missing SHA256SUMS is explained'
fi

printf '%s: %s checks, %s failed\n' "$shell" "$checks" "$failures"
[ "$failures" = 0 ]
