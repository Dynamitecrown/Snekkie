# Master installer

**[Download Snekkie for Windows](https://github.com/Dynamitecrown/Snekkie/releases/download/installer-v1.0.0/Snekkie-Setup.exe)** (`Snekkie-Setup.exe`, master version 1.0.0).

`Snekkie-Setup.exe` is a small Windows online installer. It asks GitHub for the
latest published stable Snekkie release, downloads that release's full installer,
verifies it, and opens normal setup. The full installer handles installation,
upgrades, shortcuts, and network privacy choices as before.

The master has its own version in `installer/master-version.txt`. **Do not
rebuild, re-sign, or replace it for normal Snekkie app updates.** An unchanged
master keeps the same file hash. Only change its version when its downloader,
verification, UI, or embedded signing requirement needs to change.

## Using it

Run `Snekkie-Setup.exe` and click **Install**. Download progress appears in the
details pane. When verification finishes, the latest full setup opens. The
master waits for that setup to finish before closing.

You only need to download the master once. After installation, use Snekkie's
**Update to x.y.z** button and **Update now** to download and install future
versions, retaining saved sessions and preferences. **Help → Check for updates…**
checks on demand when online updates are enabled. You can also run the same
master again to fetch the latest stable setup for a new installation or upgrade.

- `/S` downloads and runs the full installer silently. It preserves the full
  installer's exit code in the downloader; the master returns `0` on success,
  `1` for cancelled setup, and `2` for other failures.
- `/D=C:\Chosen folder` selects the installation directory. As with NSIS,
  `/D=` must be last and must not be quoted, even if the path contains spaces.
  Existing installations are upgraded in their registered location.
- Internet access to GitHub is required. Full
  `Snekkie-Setup-<app-version>.exe` assets remain available for offline installs,
  earlier versions, and troubleshooting. Offline/privacy preferences are chosen
  in the full setup; the master itself needs network access to download it.

The master requires Windows PowerShell 5.1, included with supported Windows
installations. It runs an embedded script with no console window. Environments
that block PowerShell should use the full installer.

## Verification and failure behavior

The production endpoint is fixed to
`https://api.github.com/repos/Dynamitecrown/Snekkie/releases/latest`.
Only a published stable version is accepted. The asset must have the exact
`Snekkie-Setup-<version>.exe` name and URL in that repository/tag. Portable files,
the master itself, drafts, and prereleases are never selected.

The downloader requires a valid GitHub SHA-256 asset digest, checks the exact
byte count, limits downloads to 512 MiB, follows only HTTPS redirects, and
validates the hash before opening the file. Partial downloads are removed.
Verified bytes stay locked against modification while signature checking and
setup run. Setup is never started after a failed download or verification.

Before SignPath activation, the unsigned master accepts unsigned releases only
after their GitHub digest matches. It always rejects invalid signatures; signed
downloads must have a valid signature and trusted timestamp. A master built for
production SignPath signing additionally **requires** a valid signature on each
downloaded release. Publish at least one signed stable app release before
distributing that stricter master.

The installer is downloaded to a private temporary directory and removed after
setup exits. Download errors, rate limits, and unavailable releases show an error
and point to the normal release page. There is no fallback to an unchecked file.

## Build and test

From the repository root, with NSIS 3 installed:

```powershell
./scripts/test-master-installer.ps1
./scripts/build-master-installer.ps1
```

The result is `dist/Snekkie-Setup.exe`. No Rust build or app version bump is
needed. For a production master that requires signed app releases:

```powershell
./scripts/build-master-installer.ps1 -RequireSignedRelease
```

Regression checks use an in-memory HTTP handler and harmless fixture bytes;
they do not access GitHub or run an installer. They cover release selection,
source and digest validation, HTTPS redirects, partial/oversized/altered
downloads, signature requirements, cleanup, silent arguments, and child exit
codes. The workflow runs them under Windows PowerShell 5.1.

The workflow also runs real installation checks on a disposable GitHub-hosted
Windows VM. They cover fresh installation, custom paths, an upgrade from 2.5.0,
settings and shortcut preservation, running-app handling, uninstall, and the
production **Update now** button through the UI test harness. The checks refuse
to run on local or self-hosted machines. Installation logs are retained as the
`master-installation-logs` workflow artifact.

After publication, run the workflow manually with `verify_published` selected
and `sign` left off. It downloads the public master and checksum, then runs the
same installation and Update-button checks against those published bytes.
Manual verification does not publish or replace the master release.

For a live download check without installing anything:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File installer/master-installer.ps1 `
  -DownloadDirectory C:\Temp\SnekkieDownloadCheck -DownloadOnly
```

Use a fresh download directory. A successful check retains the verified full
installer there. This developer option is not used by the packaged master.

## Independent release workflow

The **Master installer** GitHub workflow builds when master sources change, on
pull requests that affect them, or through **Run workflow**. Those runs upload
`snekkie-master-installer` for review. Regular `v*` app tags do not build or
replace the master. Manual builds can request free SignPath signing after
enrollment; see [code-signing.md](code-signing.md).

To publish an approved master build, tag the reviewed commit with
`installer-v<master-version>`, for example:

```bash
git tag -a installer-v1.0.0 -m "Snekkie master installer 1.0.0"
git push --atomic origin main installer-v1.0.0
```

Commit the changes to `main` before tagging so the workflow is on the default
branch. The tag must match `installer/master-version.txt`. The workflow publishes only
`Snekkie-Setup.exe` and its `SHA256SUMS`. With `SIGNPATH_ENABLED=true`, it requires
successful signing and signature/timestamp verification before publication.
The additional SignPath artifact configuration slug is `windows-master-v1`.

Distribute the direct asset link for that master version:

```text
https://github.com/Dynamitecrown/Snekkie/releases/download/installer-v1.0.0/Snekkie-Setup.exe
```

That file and URL stay unchanged through later app updates. It downloads
whichever full app release is latest when run. The dedicated master release is
marked prerelease and `latest=false` **only to exclude it from GitHub's app
latest-release endpoint**. This does not make it a beta app release. Update the
public download link only when intentionally releasing a new master version.

Master 1.0.0 is published unsigned, as are the current app installers. SHA-256
verification is required before the downloaded installer can run. Free SignPath
signing is a separate follow-up after enrollment and configuration; this release
does not claim a verified publisher. SmartScreen or browser warnings may appear,
and keeping the master unchanged does not guarantee they will disappear.
