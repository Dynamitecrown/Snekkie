# Jump hosts

A jump host (OpenSSH's `ProxyJump`, often called a bastion) is a server you log in to first so you can reach devices behind it. Snekkie connects through saved SSH sessions.

## Set one up

1. Save the jump host as an ordinary SSH session, for example `bastion`.
2. Open the session for the device behind it, go to **Advanced** and choose `bastion` under **Jump host**. **None (direct)** connects directly, which is the default.
3. Connect as usual.

The jump host can have a jump host of its own, so chains work: Snekkie logs in to the first hop, then the next, then the device. A chain may have at most 5 jump hosts. If the jump hosts loop back (A through B through A), or a jump host is not a saved SSH session (for example after renaming it), Snekkie says so and does not connect.

## What happens when you connect

- **Each hop logs in on its own.** A jump host using a password asks for it in a dialog titled **SSH password** that names the jump host, such as `Jump host bastion (10.0.0.1)`. An encrypted key file asks for its passphrase the same way. Nothing is stored. The device's own password is asked for as before.
- **Each hop's host key is checked** against `known_hosts` under that hop's own host name and port. Unknown keys get the usual question, and accepted keys are recorded.
- **The connection runs through the jump host.** Snekkie asks the jump host to open a channel to the next host's SSH port (a direct-tcpip channel, like `ssh -J`); the jump host must allow TCP forwarding. The device sees the connection come from the jump host.
- **Jump connections last as long as the tab.** Closing the tab hangs them up. If a jump host drops, the tab closes with a reason that names it, such as `Connection lost at jump host bastion: …`.
- **Errors name the hop**, for example `Jump host bastion: Authentication failed for admin@10.0.0.1` or `Jump host bastion could not reach 192.168.10.1:22`.

The file explorer, quick transfers (SFTP and SCP) and agent file tools of a jump-hosted SSH tab use the same connection, so they also go through the jump hosts.

## Offline mode

Offline mode checks the connections Snekkie makes from this computer. With a jump host that is only the first hop: Snekkie opens one TCP connection, to the first jump host, and that address is what offline mode allows or blocks. Later hops and the device are reached by the jump host, not by this computer, so offline mode does not see or check their addresses.

## Importing

The OpenSSH config import sets the jump host when `ProxyJump` names another `Host` in the same file. Other forms, such as `user@host:port` or a comma-separated chain, are listed as unsupported, as is `ProxyCommand`. The PuTTY import does not set jump hosts. Profile export and import keep the jump host by name; if an imported jump host is renamed because the name is taken, sessions imported with it follow the new name.
