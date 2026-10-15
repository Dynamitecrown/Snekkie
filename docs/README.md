# Snekkie documentation

Current release: **[Snekkie 2.6.0](https://github.com/Dynamitecrown/Snekkie/releases/tag/v2.6.0)**, published October 6, 2026. See the [main README](../README.md) for installation, connections, keyboard shortcuts and build instructions.

**[Download Snekkie for Windows](https://github.com/Dynamitecrown/Snekkie/releases/download/installer-v1.0.0/Snekkie-Setup.exe)** — download `Snekkie-Setup.exe` once to fetch and install the latest stable app from GitHub. Installed users then use the **Update** button for future versions, preserving saved sessions and preferences. The master has its own version and remains unchanged during ordinary app releases; see the [master installer guide](master-installer.md).

Installed Windows users on 2.5.0 can choose **Help → Check for updates…**, then **Update to 2.6.0**, when online updates are enabled. The updater downloads `Snekkie-Setup-2.6.0.exe` and verifies its published SHA-256. Full `Snekkie-Setup-<version>.exe` installers for offline use, portable Windows files and Linux builds remain on the [app release downloads](https://github.com/Dynamitecrown/Snekkie/releases/latest). The master, full installer and app are currently unsigned.

## Guides

| Guide | What it covers |
| --- | --- |
| [2.6.0 release notes](releases/2.6.0.md) | Snippets, paste controls, logging, search fixes and validation |
| [Command snippets](command-snippets.md) | Saved templates, temporary variables, preview, drafts and JSON sharing |
| [Paste behavior](paste-behavior.md) | Multiline review, pacing, Stop, destination binding and right-click preferences |
| [Session logging](session-logging.md) | Per-profile TX/RX logs, timestamps, raw format, password controls, Private input and rotation |
| [Master installer](master-installer.md) | Download-once Windows installation, updates, verification, silent setup and independent releases |
| [Linux and macOS](linux-and-macos.md) | Install script, `.deb`, `.rpm`, Flatpak, AppImage and macOS app (from the release after 2.6.0), serial permissions and settings locations |
| [Syntax highlighting](syntax-highlighting.md) | Device platforms, intensity levels and display behavior |
| [2.5.0 feature guide](new-features.md) | Themes, animations, search, session organization, imports and network privacy retained in 2.6.0 |

Multiline paste review defaults on with a 100 ms delay; single-line paste stays immediate. Logging is enabled per profile. Password logging defaults off, and Private input suppresses both directions for unusual sensitive workflows. Older profiles retain raw logging and their previous rotation defaults. See the relevant guides for the full behavior.

## Release history

- [2.6.0](releases/2.6.0.md)
- [2.5.0](releases/2.5.0.md)
- [2.4.1](releases/2.4.1.md)
- [2.4.0](releases/2.4.0.md)
- [Retired Python 1.x archive](archive/1.x/README.md)

Windows and Linux release CI passed for 2.6.0. Native clipboard/file dialogs, physical serial hardware and real vendor-device workflows remain separate checks from the automated and headless UI validation.
