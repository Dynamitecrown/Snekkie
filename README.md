# Snekkie

A tabbed SSH, Telnet, raw TCP and serial terminal for network engineers, built in Rust for Windows, Linux and macOS.

**[Download the Windows master installer](https://github.com/Dynamitecrown/Snekkie/releases/download/installer-v1.0.0/Snekkie-Setup.exe)**

Download this master installer once. It fetches and verifies the latest stable app from GitHub; future updates install through Snekkie's **Update** button.

[Offline Windows installers, portable Windows and Linux downloads](https://github.com/Dynamitecrown/Snekkie/releases/latest) · [2.6.0 release notes](docs/releases/2.6.0.md)

## What it does

- Saved sessions, tabs, favorites and PuTTY import.
- Scrollback search, device syntax highlighting and paste controls.
- Command snippets and local session logging.
- Custom themes and optional offline mode.

![Snekkie showing Cisco switch output](docs/screenshots/show-commands.png)

## Install and update

**Windows:** Run `Snekkie-Setup.exe`. It downloads the latest version and installs for the current user without administrator rights. Saved sessions and settings survive upgrades.

For updates, click **Update to x.y.z → Update now**, or use **Help → Check for updates…**. You can also rerun the same master installer.

Full `Snekkie-Setup-<version>.exe` downloads support offline setup. Portable copies run without installation and link to new releases instead of updating themselves.

Windows downloads are currently unsigned, so SmartScreen may warn. See the [installer guide](docs/master-installer.md) and [signing policy](docs/code-signing.md).

**Linux and macOS:** Starting with the release after 2.6.0, one install script picks the `.deb`, `.rpm`, Flatpak, AppImage or universal macOS app that suits your computer, verifies it against the release checksums and installs it. Run it again to update:

```bash
curl -fsSLO https://github.com/Dynamitecrown/Snekkie/releases/download/unix-installer-v1.0.0/snekkie-install.sh
sh snekkie-install.sh
```

Each package can also be downloaded and installed by itself. See [Installing on Linux and macOS](docs/linux-and-macos.md) for per-distribution commands, macOS first launch and serial permissions. Release 2.6.0 offers only `snekkie-linux-x86_64.tar.gz` for x86_64 Linux: extract it with `tar -xzf snekkie-linux-x86_64.tar.gz` and run `./snekkie`.

Linux serial access may require membership in `dialout`. On Windows, install your USB console cable's driver if its COM port is missing.

## Build and test

Install [Rust stable](https://rustup.rs), then:

```bash
cargo run --release
cargo test --locked
```

On Windows, `build-windows.bat` also builds the full installer when [NSIS 3](https://nsis.sourceforge.io) is installed. Linux packages and the macOS disk image are built by the scripts described in [packaging/README.md](packaging/README.md).

## Documentation

[Guides and feature documentation](docs/README.md)

## License

[GPL-3.0-or-later](LICENSE).
