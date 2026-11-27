# Tunnels (SSH port forwarding)

An SSH tab can carry port forwards, like `ssh -L`, `-R` and `-D`. They start when the tab connects and stop when it closes.

## Set one up

Open the SSH session's **Advanced** page, expand **Tunnels** and click **Add tunnel**. Each rule has:

- **Kind**
  - **Local** (`-L`): a port on this computer reaches a host and port *as the server sees them*. Example: local port 8443 to `10.0.0.1:443` lets a browser here open `https://127.0.0.1:8443` to reach a device only the server can reach.
  - **Remote** (`-R`): a port on the server reaches a host and port *as this computer sees them*.
  - **Dynamic (SOCKS)** (`-D`): a SOCKS5 proxy on this computer. Each connection goes wherever the program asks, from the server. No authentication; CONNECT only; IPv4, IPv6 and host names.
- **Local port** or **Server port**: where the tunnel listens.
- **Destination**: host and port (not used by Dynamic).
- **Allow other computers to connect**: off by default, so the tunnel listens on `127.0.0.1` only. When ticked it listens on every interface (`0.0.0.0`), and anyone who can reach this computer, or the server for a Remote rule, can use it without a password. A warning stays beside the box while it is ticked. A server usually needs `GatewayPorts` for a Remote rule to listen beyond its loopback.
- The checkbox at the start of each rule turns it off without deleting it.

Tunnels go over the tab's own connection. With a [jump host](jump-hosts.md) they go over the final device's connection, not the jump host's.

## While connected

The status bar shows **Tunnels 2/3** (listening out of all) for the current tab; it turns red if a rule failed. Click it, or right-click the tab and choose **Tunnels…**, to see each rule as **listening**, **starting** or **error: …**, with its open connections. Hover a rule to see why its last connection failed.

A rule that can't start does not affect the shell or the other rules. Snekkie shows a message naming it, such as `Tunnel L 8080 → 10.0.0.1:80: could not listen on 127.0.0.1:8080: …` when the port is taken, or `Tunnel R 2222 → 127.0.0.1:22: the server refused to forward its port 2222` when the server says no (for example `AllowTcpForwarding no`).

## Offline mode

Local and Dynamic tunnels only listen here; the server makes the outgoing connections, so offline mode does not see them. A Remote tunnel's connection to its destination is made by Snekkie, so it goes through offline mode like any other connection: private and loopback destinations connect straight away, anything else asks each time.

## Importing

- **OpenSSH config:** `LocalForward`, `RemoteForward` and `DynamicForward` become tunnels when they use simple forms: `port` or `bind:port`, then `host:port`. Socket paths, a `RemoteForward` without a destination and other bind addresses stay as notes.
- **PuTTY:** `PortForwardings` entries such as `L8080=10.0.0.1:80`, `R2222=localhost:22` and `D1080` become tunnels. IPv4/IPv6-only entries (`4L…`, `6L…`) and anything else stay as notes.
- **Profile export and import** keep tunnels. Imported rules are checked, and invalid ones are skipped with a note.

An import never switches on **Allow other computers to connect**. A rule that listened on every interface (`*:8080`, `0.0.0.0:8080`) comes in listening on `127.0.0.1` only, with a note.

## Limits

- Tunnels listen on IPv4 (`127.0.0.1` or `0.0.0.0`) only, so a program that connects to `localhost` through `::1` won't reach them. Use `127.0.0.1`.
- Closing or reconnecting the tab ends its Remote forwards on the server, even when a file explorer opened from the tab still shares the connection.
- File explorer tabs don't start tunnels; only SSH shell tabs do.
