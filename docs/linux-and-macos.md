# Installing on Linux and macOS

Snekkie for Linux comes as a `.deb`, an `.rpm`, a Flatpak bundle, an AppImage and a
plain tarball. The macOS version is a universal app for Apple silicon and Intel Macs.
One install script picks the right one for your computer.

> **Availability:** these packages and the install script ship with the first release
> after 2.6.0. Release 2.6.0 and earlier offer only `snekkie-linux-x86_64.tar.gz` for
> Linux and nothing for macOS.

## Install script (recommended)

The install script is the Linux and macOS counterpart of the Windows master installer.
It finds the latest stable release on GitHub, downloads the package that suits your
system, checks it against the release's `SHA256SUMS` file and installs it:

```bash
curl -fsSLO https://github.com/Dynamitecrown/Snekkie/releases/download/unix-installer-v1.0.0/snekkie-install.sh
sh snekkie-install.sh
```

The script shows what it is about to do and asks before changing anything. Package
installs ask for your password through `sudo`; the AppImage, the Flatpak and the
macOS app install for your user without one. Run the same script again later to
update: it upgrades Snekkie in the format already installed and says so if you are
already up to date. Saved sessions and settings are kept.

| System | Gets |
| --- | --- |
| Debian, Ubuntu, Linux Mint, Pop!_OS, elementary OS and other apt systems | `.deb`, installed with `apt-get` |
| Fedora, RHEL-compatible, openSUSE and other dnf, yum or zypper systems | `.rpm` |
| Fedora Silverblue, Kinoite, Bazzite and other image-based systems | Flatpak |
| Systems with glibc older than 2.35, such as RHEL, Rocky and AlmaLinux 9 | Flatpak |
| Arch, Manjaro, Gentoo, Void and any other x86_64 Linux | AppImage in `~/.local/bin` |
| macOS 11 Big Sur or later | `Snekkie.app` in Applications |

Options:

- `--format deb|rpm|flatpak|appimage|dmg` chooses a format instead.
- `--download-only --dir FOLDER` downloads and verifies the package without installing it.
- `--from FOLDER` installs without internet access, from a folder holding a package and
  the `SHA256SUMS` file from the same release page.
- `--release 2.7.0` installs that release instead of the latest.
- `--uninstall` removes Snekkie, keeping saved sessions and settings.
- `--yes` skips the confirmation, for unattended installs.

The script needs `curl` or `wget`. It accepts only the exact package names in the
release's `SHA256SUMS`, downloads only over HTTPS, and installs nothing whose SHA-256
differs. Like the Windows master, it has its own version and doesn't change with app
releases.

## Installing a package yourself

Every package is on the [release page](https://github.com/Dynamitecrown/Snekkie/releases/latest).
Compare a download with `SHA256SUMS` first: `sha256sum -c SHA256SUMS --ignore-missing`.

**Debian and Ubuntu:** `sudo apt install ./snekkie_<version>-1_amd64.deb`

**Fedora:** `sudo dnf install ./snekkie-<version>-1.x86_64.rpm`

**openSUSE:** `sudo zypper install --allow-unsigned-rpm ./snekkie-<version>-1.x86_64.rpm`

The packages aren't signed with a distribution key, which is why zypper needs
`--allow-unsigned-rpm`; the checksum is what verifies them. They install
`/usr/bin/snekkie`, a menu entry and an icon, and declare the display, graphics and
keyboard libraries that Snekkie loads. Removing the package leaves your settings.

**Flatpak:** `flatpak install --user ./Snekkie-<version>-x86_64.flatpak`, then start it
from the menu or with `flatpak run io.github.Dynamitecrown.Snekkie`. Flatpak downloads
the Freedesktop runtime from Flathub the first time. To update, install the newer
bundle with `--reinstall`.

**AppImage:** `chmod +x Snekkie-<version>-x86_64.AppImage`, then run it. AppImages need
FUSE; if it doesn't start, install your distribution's `fuse3` package.

**Tarball:** `tar -xzf snekkie-linux-x86_64.tar.gz && ./snekkie`. It needs the same
system libraries as the packages and doesn't add a menu entry.

## macOS

Open `Snekkie-<version>-macos-universal.dmg` and drag Snekkie into Applications.

Snekkie is open source and not yet notarized by Apple. A copy downloaded with a web
browser may be blocked the first time with a message that macOS cannot verify it. Open
**System Settings → Privacy & Security** and click **Open Anyway** next to the Snekkie
message. Check the download against `SHA256SUMS` first if you like:
`shasum -a 256 Snekkie-*-macos-universal.dmg`. Copies installed by the install script
open normally, because `curl` downloads aren't marked for that check.

When Snekkie first connects to a device on your network, macOS 15 and later ask for
Local Network permission. Allow it, or Snekkie can't reach devices on your LAN. You can
change it later under **Privacy & Security → Local Network**.

On a Mac, app shortcuts use **Command** (Cmd+W closes a tab, Cmd+, opens Preferences)
and **Control** keys go to the device, so Ctrl+C still interrupts a remote command.
**Help → Keyboard shortcuts** lists them all.

## Serial consoles

USB serial adapters appear as `/dev/ttyUSB0` or `/dev/ttyACM0` on Linux and as
`/dev/cu.usbserial-…` on macOS. On Linux your user needs access to them: add yourself to
`dialout` (Debian, Ubuntu, Fedora) or `uucp` (Arch, openSUSE), then log out and back in:

```bash
sudo usermod -aG dialout "$USER"
```

The Flatpak can open serial devices too; the group membership still applies. Virtual
serial ports (pseudo-terminals, such as those `socat` creates) work on both systems.

## Settings and updates

| Install | Settings, sessions and themes |
| --- | --- |
| `.deb`, `.rpm`, AppImage, tarball | `~/.config/snekkie` (or `$XDG_CONFIG_HOME/snekkie`) |
| Flatpak | `~/.var/app/io.github.Dynamitecrown.Snekkie/config/snekkie` |
| macOS | `~/Library/Application Support/snekkie` |

The Flatpak keeps its own settings. To move sessions between it and another install, use
**Settings → Export profiles…** and **Settings → Import → Snekkie profiles (.json)…**.
SSH host keys go to `~/.ssh/known_hosts` in every format. Apart from `~/.ssh`, the
Flatpak reaches your files only through the file chooser. Its session logs default to
`logs` inside its settings folder; choose a log folder there, because the sandbox can't
create log files elsewhere in your home folder.

**Help → Check for updates…** tells you when a new version is out. On Linux and macOS it
can't install the update itself; rerun the install script, or download the new package.
Package-manager updates aren't automatic, because Snekkie isn't in a distribution repository.

## Limits

- Linux packages are built for x86_64 only. Other architectures can build from source.
- The `.deb`, `.rpm`, AppImage and tarball need glibc 2.35 or newer (Ubuntu 22.04,
  Debian 12, Fedora 36 or later). Older systems can use the Flatpak.
- The Flatpak is a bundle from GitHub releases, not a Flathub listing, so `flatpak update`
  doesn't update it.
- The macOS app is signed ad hoc, not with an Apple Developer ID, and isn't notarized.
