# Syntax highlighting

Choose the platform in the session form's **Device** menu. The choice is saved with the connection profile; **None** keeps device output in its original colors. Existing `cisco_ios` profiles continue to work with the **Cisco IOS / IOS XE** option.

Use **Settings → Preferences → Highlighting → Intensity** to control the amount of highlighting. It applies to all open and new sessions when you click **OK**. **Cancel** discards changes. The preview device changes only the sample, so you can compare platforms without changing a session.

| Level | Adds highlighting for |
| --- | --- |
| 1 — Essential | Valid IPv4 and IPv6 addresses, prefixes, encryption algorithms, password types, authentication and security settings |
| 2 — Status | Prompts, routing and network protocols, positive and negative status |
| 3 — Commands (default) | Platform commands, negation and interface names |
| 4 — Detailed | More platform configuration terms and MAC addresses |
| 5 — Full | Numbers, units, table headings, descriptions and comments |

Each level includes the lower levels. Important fields keep their color as detail increases. Addresses are validated to avoid coloring impossible IPs or partial matches inside version strings. Prompts can be recognized before echoed commands, including Cisco configuration prompts, Junos `user@host` prompts and FortiOS contexts. Colors adapt to light terminal backgrounds.

Monochrome themes take precedence over syntax colors. To see syntax colors with a monochrome preset, clear **Monochrome output** under **General → Theme colors** and save the customized theme. CRT glow and the retro interface can remain enabled independently.

Highlighting is a text display overlay, not a complete configuration parser or security audit. It colors recognizable terms in commands, configuration and output; arbitrary names and vendor/version-specific syntax may remain uncolored. It never changes the actual output, transmitted commands, clipboard text, logs or exports.

The refactored matcher shares address, security and status rules across platforms. Device vocabulary, interface patterns and preview samples live in `src/terminal/highlight/devices.rs`; matching and category priorities live in `src/terminal/highlight.rs`. Adding a device in the registry makes it available in both the session picker and preview.

The device rules are informed by these primary references; the preview samples use documentation addresses and fictional device names:

- [Cisco IOS XE hardening guide](https://sec.cloudapps.cisco.com/security/center/resources/IOS_XE_hardening)
- [Cisco NX-OS interface commands](https://www.cisco.com/c/dam/en/us/td/docs/switches/datacenter/sw/5_x/nx-os/interfaces/command/reference/if_cmd.pdf)
- [Cisco ASA IPsec and ISAKMP configuration](https://www.cisco.com/c/en/us/td/docs/security/asa/asa912/configuration/vpn/asa-912-vpn-config/vpn-ike.html)
- [Juniper Junos configuration displayed as set commands](https://www.juniper.net/documentation/us/en/software/junos/cli-reference/topics/ref/command/show-pipe-display-set.html)
- [Aruba AOS-CX IP services](https://www.arubanetworks.com/techdocs/AOS-CX/10.12/PDF/ip_services_4100i-6000-6100.pdf)
- [Arista EOS command-line interface](https://www.arista.com/en/um-eos/eos-command-line-interface-cli)
- [Fortinet FortiOS system interfaces](https://docs.fortinet.com/document/fortigate/7.2.10/cli-reference/317104469/config-system-interface)
- [Palo Alto PAN-OS CLI quick reference](https://docs.paloaltonetworks.com/ngfw/pan-os-cli-quick-start/use-the-cli/cli-jump-start)
