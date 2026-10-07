# Security Policy

## Supported Versions

Security fixes are provided for the latest released version of Snekkie. Please update to the newest release before reporting an issue.

| Version | Supported |
| ------- | --------- |
| Latest release | Yes |
| Older releases | No |

## Reporting a Vulnerability

Please do not report security vulnerabilities in public issues, discussions or pull requests.

Report them privately using GitHub's private vulnerability reporting:

1. Open the [Security tab](https://github.com/Dynamitecrown/Snekkie/security) of this repository.
2. Select **Report a vulnerability**.
3. Describe the issue, the affected version and how to reproduce it.

Helpful details include:

- Snekkie version and operating system
- Connection type involved (SSH, Telnet, raw TCP or serial)
- Steps to reproduce, and proof-of-concept code or logs if available
- The impact you believe the issue has

Please remove passwords, private keys and other secrets from anything you attach.

## What to Expect

- Acknowledgement of your report within 7 days.
- An assessment and a plan or follow-up questions within 30 days.
- Credit in the release notes once a fix ships, unless you prefer to stay anonymous.

If the report is accepted, a fix is released and the issue is disclosed after users have had a reasonable time to update. If it is declined, you will get an explanation.

## Scope

In scope: vulnerabilities in Snekkie itself, including credential or session-data handling, logging redaction, the updater and installer, and handling of untrusted device output.

Out of scope: weaknesses in the remote devices you connect to, in the Telnet or raw TCP protocols themselves (which are unencrypted by design), and issues that require an already compromised local machine.
