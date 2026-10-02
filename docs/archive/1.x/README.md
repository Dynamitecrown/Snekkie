# Snekkie 1.x archive

This directory keeps historical information about the retired Python releases separate from current Snekkie documentation. Use the [latest release](https://github.com/Dynamitecrown/Snekkie/releases/latest) for current development and packaged updates.

| Version | Release and downloads | Source |
| --- | --- | --- |
| 1.0.2 | [Archived release](https://github.com/Dynamitecrown/Snekkie/releases/tag/v1.0.2) | [Source at v1.0.2](https://github.com/Dynamitecrown/Snekkie/tree/v1.0.2) |
| 1.0.1 | [Archived release](https://github.com/Dynamitecrown/Snekkie/releases/tag/v1.0.1) | [Source at v1.0.1](https://github.com/Dynamitecrown/Snekkie/tree/v1.0.1) |

Tags, commit history, original release notes and release downloads are retained for historical use. The source, build instructions, dependencies and screenshots for each version remain accessible through its source link. The current branch contains the Rust application.

## Historical upgrade information

- Saved sessions and settings carry over automatically: the Rust application reads the same `%APPDATA%\snekkie` (Windows) or `~/.config/snekkie` (Linux) files.
- The 1.x `snekkie.exe` was a standalone file. The current installer does not remove it; after verifying the new installation, you can remove that old executable and its shortcuts.
- Mark/Space serial parity and 1.5 stop bits are not supported by the current serial driver library. A saved session using an unsupported setting reports the issue when connecting.
