# Free Windows code signing

Snekkie's GitHub Actions workflow supports the **SignPath Foundation free
open-source program**. It does not use Azure or a paid signing service.
SignPath must accept the project before production signing can be enabled;
committing this integration does not issue a certificate or enroll the project.

## Enrollment and configuration

1. Apply at <https://signpath.org/apply.html> for both the free OSS service and a
   Foundation certificate. Review <https://signpath.org/terms.html>. A free
   service account alone, using your own certificate, is not the same thing.
2. Link `Dynamitecrown/Snekkie` to the SignPath project's trusted GitHub build
   system and install the SignPath GitHub App as required by onboarding. Keep all
   jobs on GitHub-hosted runners. Configure the production policy to accept
   release tags (`refs/tags/v*` and `refs/tags/installer-v*`) and, for signed manual
   checks, `refs/heads/main`.
3. Add these artifact configurations to the SignPath project, using the exact
   slugs shown. Review them with SignPath during onboarding:

   | Slug | Repository file | Files signed |
   | --- | --- | --- |
   | `windows-payload-v1` | [`installer/signpath/windows-payload-v1.xml`](../installer/signpath/windows-payload-v1.xml) | App and uninstaller |
   | `windows-installer-v1` | [`installer/signpath/windows-installer-v1.xml`](../installer/signpath/windows-installer-v1.xml) | Finished installer |
   | `windows-master-v1` | [`installer/signpath/windows-master-v1.xml`](../installer/signpath/windows-master-v1.xml) | Independent online master installer |

   All configurations consume the ZIP archive created by `upload-artifact`,
   enforce `ProductName=Snekkie` and the submitted `ProductVersion` (Cargo's app
   version, or the independent master version), and
   request SHA-256 Authenticode signing. Use the Foundation's production
   certificate and enable trusted timestamping in the signing policy.
4. Under GitHub **Settings → Secrets and variables → Actions**, add the
   repository secret `SIGNPATH_API_TOKEN`. Use a submitter token for this
   project/policy; the workflow does not need an administrator token.
5. Add these repository **variables**, using the actual values from SignPath:

   | Variable | Value |
   | --- | --- |
   | `SIGNPATH_ORGANIZATION_ID` | Organization ID |
   | `SIGNPATH_PROJECT_SLUG` | Snekkie's project slug |
   | `SIGNPATH_SIGNING_POLICY_SLUG` | Production signing policy slug |
   | `SIGNPATH_ENABLED` | `true`, after enrollment/configuration is complete |

6. Run **Actions → CI → Run workflow** from `main`, selecting **sign**. Approve
   both signing requests in SignPath. Download the final `snekkie-windows`
   artifact and confirm its publisher before tagging the next signed release.

The workflow needs GitHub `actions: read` and `contents: read` for signing.
Only the publishing job receives `contents: write`. The SignPath action is
pinned to a reviewed v3 commit. No private signing key is stored in GitHub.

## Release behavior

Until `SIGNPATH_ENABLED=true`, tags retain the existing unsigned release behavior.
Manual builds are unsigned by default; requesting signing without enabling the
integration fails with a configuration message. Once enabled, **every `v*` tag,
including prereleases, requires signing**. Signed manual builds are restricted
to `main` or a `v*` tag. Pull requests never submit signing requests.

The independent **Master installer** workflow uses `installer-v*` tags and the
master version from `installer/master-version.txt`. Signed manual master builds
are restricted to `main` or `installer-v*`. With signing enabled, those release
tags also require signing. The signed master requires signed downloaded app
installers, so publish a signed stable app release first. See the
[master installer guide](master-installer.md).

The Windows packaging sequence is:

1. Build the app on Windows and export a standalone uninstaller using Linux NSIS.
2. Submit the app and uninstaller together for signing. Wait for manual approval,
   download the signed files, and validate both signatures and timestamps on Windows.
3. Build the installer with those exact signed executables. The portable release
   is a byte-for-byte copy of the signed app. Linux NSIS retains the existing
   64-bit installer stubs.
4. Submit the finished installer for a second signing approval.
5. On Windows, verify all signatures, timestamps, and matching publisher names.
   Extract the installer with 7-Zip and check that its app and uninstaller match
   the verified payload exactly. Publish only the final verified artifacts.
6. Calculate `SHA256SUMS` from the final Windows files and Linux tarball, then
   attach it to the GitHub release. GitHub's asset digest used by the updater
   also corresponds to the final uploaded installer.

Each signing request waits up to two hours for approval. Missing configuration,
rejection, timeout, invalid signatures, missing timestamps, or changed payloads
fail the workflow. A signed release never falls back to unsigned inputs.
Intermediate artifacts are retained in Actions for diagnosis; only
`snekkie-windows` contains the final Windows download files.

The second NSIS pass embeds the already signed uninstaller as a normal file.
Local builds without `EXPORT_UNINSTALLER` or `SIGNED_UNINSTALLER` continue to
generate their own unsigned uninstaller with `WriteUninstaller`.

## Code signing policy

The source repository and release workflow are maintained by
[Dynamitecrown (Lucas)](https://github.com/Dynamitecrown). Lucas is the committer,
reviewer of outside contributions, and release signing approver. Update this
policy if those roles change. Signing approvals must cover reviewed source,
matching version metadata, and artifacts from the repository's GitHub-hosted
workflow. Members with repository or signing access must use multi-factor
authentication.

After Foundation acceptance, add the following attribution to the project home
page, linking to this policy. The workflow adds it and the policy link to signed
release notes automatically:

> Free code signing provided by SignPath.io, certificate by SignPath Foundation

This integration is pending enrollment until the repository is configured.
Do not describe existing downloads as Foundation-signed before that happens.
The production certificate identifies SignPath Foundation as the publisher.

For network behavior, see the [README](../README.md): Snekkie connects to devices
chosen by the user, supports GitHub update checks according to the user's
network preferences, and allows offline mode. Disclose this behavior during
onboarding rather than claiming that the app never accesses the network.

Signing authenticates the publisher and file contents. SmartScreen and browser
download reputation may still produce warnings for a new publisher or release.

## References

- [SignPath GitHub integration](https://docs.signpath.io/trusted-build-systems/github)
- [SignPath artifact configuration](https://docs.signpath.io/artifact-configuration/)
- [NSIS external uninstaller signing](https://nsis.sourceforge.io/Signing_an_Uninstaller_externally)
- [Microsoft SmartScreen reputation guidance](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)
