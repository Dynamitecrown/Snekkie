# Linux and macOS packaging

Release CI builds every package from one binary per platform. The scripts can also run
by hand; none of them installs anything on the machine that runs it.

| Output | Script | Built on |
| --- | --- | --- |
| `snekkie_<v>-1_amd64.deb`, `snekkie-<v>-1.x86_64.rpm` | [`scripts/package-linux.sh`](../scripts/package-linux.sh) | Ubuntu 22.04 |
| `Snekkie-<v>-x86_64.AppImage` | [`scripts/package-appimage.sh`](../scripts/package-appimage.sh) | Ubuntu 22.04 |
| `Snekkie-<v>-x86_64.flatpak` | [`scripts/package-flatpak.sh`](../scripts/package-flatpak.sh) | Ubuntu 24.04 |
| `Snekkie-<v>-macos-universal.dmg` | [`scripts/package-macos.sh`](../scripts/package-macos.sh) | macOS |
| `snekkie-install.sh` | [`installer/unix/snekkie-install.sh`](../installer/unix/snekkie-install.sh) | (a script) |

The Linux binary is built on Ubuntu 22.04 so that it runs with glibc 2.35 or newer; the
package scripts refuse a binary that needs more. It links only `libc`, `libm` and
`libgcc_s`. winit, wgpu and the clipboard open the X11, Wayland, xkbcommon, EGL/GL,
Vulkan and D-Bus libraries at run time, so the `.deb` and `.rpm` declare them explicitly
(by SONAME for RPM, which works across Fedora and openSUSE).

## Build

```bash
cargo build --release --locked                    # on Ubuntu 22.04
bash scripts/package-linux.sh 2.7.0               # .deb and .rpm into dist/
bash scripts/package-appimage.sh 2.7.0            # AppImage into dist/
bash scripts/package-flatpak.sh 2.7.0             # Flatpak bundle (needs flatpak, appstream-compose)
```

Each takes `VERSION [BINARY] [OUTPUT_DIRECTORY]`; `package-linux.sh` also takes a
package revision (default 1). On macOS, build both architectures with
`MACOSX_DEPLOYMENT_TARGET=11.0`, then `bash scripts/package-macos.sh 2.7.0`.

Tool requirements: `dpkg-dev`, `rpm`, `binutils` and `desktop-file-utils` for the
packages; `curl` plus the runtime libraries listed in the script for the AppImage;
`flatpak`, `appstream` and `appstream-compose` for the Flatpak; Xcode command-line
tools for macOS.

### AppImage

`package-appimage.sh` downloads two pinned upstream tools into `target/appimage-tools`
and checks their SHA-256 before use: linuxdeploy `1-alpha-20251107-1` and the static
type2 runtime `20251108`, which needs no `libfuse2` on the host. Update the URLs and
digests together. The AppImage bundles only xkbcommon and the X11 cursor, RandR and
input helpers. The host provides `libX11`, `libxcb`, Wayland, EGL/GL, Vulkan and D-Bus,
as linuxdeploy's excludelist requires, because bundled copies break newer GPU drivers;
the script fails if any of them is bundled. The default `AppRun` symlink is used, and
the executable's `RUNPATH` finds the bundled libraries without an `LD_LIBRARY_PATH`
that would leak into programs Snekkie opens.

### Flatpak

The bundle uses `org.freedesktop.Platform` 26.08 from Flathub and is assembled from the
release binary rather than compiled in the SDK. Runtimes download into an isolated
installation under `target/flatpak-tools`, never the account's own. `build-init` needs
the SDK installed even though nothing is compiled. The script composes the AppStream
catalog that `flatpak-builder` would, which is where Flatpak, GNOME Software and the
install script read the installed version.

Permissions ([`flatpak/finish-args.txt`](flatpak/finish-args.txt)) are network, IPC,
Wayland with X11 fallback, the SSH agent socket, all devices (serial ports have no
narrower permission) and `~/.ssh` for keys and `known_hosts`. Any change must also
update [`flatpak/check-permissions.py`](flatpak/check-permissions.py), which checks the
installed app.

### macOS

`package-macos.sh` combines the two release builds with `lipo`, checks both slices
target macOS 11, makes `Snekkie.icns` from `assets/icon.png`, signs the bundle ad hoc
and builds a compressed HFS+ disk image with an Applications link and first-launch
notes. `Info.plist` records the full version as `SnekkieVersion`, which the install
script compares. There is no Developer ID signing or notarization yet.

## Tests

Destructive checks refuse to run outside disposable containers or GitHub-hosted runners.

| Script | Checks |
| --- | --- |
| [`test-linux-packages.sh`](../scripts/test-linux-packages.sh) | Install, reinstall, removal, upgrade from revision 0, dependency resolution and an Xvfb window, per distribution container |
| [`test-linux-portable.sh`](../scripts/test-linux-portable.sh) | AppImage and Flatpak launches under Xvfb; the installed Flatpak's permissions |
| [`test-macos-app.sh`](../scripts/test-macos-app.sh) | Disk image layout, universal signed app, version, and a launch that opens a window |
| [`test-install-script-unit.sh`](../scripts/test-install-script-unit.sh) | Install script parsing, asset selection, format choice and refusals, offline, under each `sh` |
| [`test-install-script.sh`](../scripts/test-install-script.sh) | Install script lifecycles with real package managers, AppImage, Flatpak and macOS: no confirmation without a terminal, install, no-op rerun, tampered package refused, update, uninstall |

Running the container tests from Windows or macOS needs Docker and Linux paths; see each
script's usage line. CI runs them on Ubuntu 22.04 and 24.04, Debian 12 and 13, Fedora 44,
and openSUSE Leap 16 and Tumbleweed, and launches the app on Apple silicon and Intel Macs.

## Publishing

The `v*` tag workflow in [`ci.yml`](../.github/workflows/ci.yml) attaches every package
to the release and lists them in `SHA256SUMS`, which the install script requires. The
install script is versioned separately (`INSTALLER_VERSION`) and published by the
**Linux and macOS installer** workflow from an `unix-installer-v<version>` tag, as a
prerelease that doesn't affect the app's latest-release endpoint. Publish it only after
an app release containing these packages exists: the tag workflow downloads and verifies
every format from the latest release first. Change its version only when the script
itself changes, and update the download link in the docs.
