"""SSH transport backed by paramiko."""

from __future__ import annotations

import socket
from collections.abc import Callable
from pathlib import Path

import paramiko

from . import Transport, TransportError, register

#: How long connect() waits before giving up.
CONNECT_TIMEOUT = 12.0

#: Read timeout. Short enough that the reader thread notices shutdown quickly.
READ_TIMEOUT = 0.1

KNOWN_HOSTS = Path.home() / ".ssh" / "known_hosts"


class _PromptPolicy(paramiko.MissingHostKeyPolicy):
    """Ask the user about unknown host keys instead of silently trusting them.

    AutoAddPolicy is the usual shortcut here and it quietly defeats the entire
    point of host key verification, so we route the decision to the UI.
    """

    def __init__(self, ask: Callable[[str, paramiko.PKey], bool] | None):
        self._ask = ask

    def missing_host_key(self, client, hostname, key):
        if self._ask is None or not self._ask(hostname, key):
            raise paramiko.SSHException(
                f"Host key for {hostname} was not accepted "
                f"({key.get_name()} {key.get_fingerprint().hex()})"
            )
        client.get_host_keys().add(hostname, key.get_name(), key)
        try:
            KNOWN_HOSTS.parent.mkdir(parents=True, exist_ok=True)
            client.save_host_keys(str(KNOWN_HOSTS))
        except OSError:
            pass  # Non-fatal: we still trust it for this session.


@register("ssh", "SSH")
class SSHTransport(Transport):
    def __init__(
        self,
        profile,
        password: str = "",
        key_passphrase: str = "",
        ask_host_key: Callable[[str, paramiko.PKey], bool] | None = None,
    ):
        super().__init__(profile)
        self._password = password
        self._key_passphrase = key_passphrase
        self._ask_host_key = ask_host_key
        self._client: paramiko.SSHClient | None = None
        self._chan: paramiko.Channel | None = None

    # -- lifecycle ---------------------------------------------------------

    def connect(self) -> None:
        p = self.profile
        client = paramiko.SSHClient()
        client.load_system_host_keys()
        if KNOWN_HOSTS.exists():
            try:
                client.load_host_keys(str(KNOWN_HOSTS))
            except OSError:
                pass
        client.set_missing_host_key_policy(_PromptPolicy(self._ask_host_key))

        kwargs = dict(
            hostname=p.host,
            port=p.port,
            username=p.username or None,
            timeout=CONNECT_TIMEOUT,
            allow_agent=p.auth in ("agent", "key"),
            look_for_keys=p.auth in ("agent", "key"),
        )
        if p.auth == "password":
            kwargs.update(password=self._password, allow_agent=False,
                          look_for_keys=False)
        elif p.auth == "key" and p.key_file:
            kwargs.update(key_filename=p.key_file,
                          passphrase=self._key_passphrase or None)

        try:
            client.connect(**kwargs)
        except paramiko.AuthenticationException as exc:
            client.close()
            raise TransportError(f"Authentication failed: {exc}") from exc
        except (paramiko.SSHException, OSError) as exc:
            client.close()
            raise TransportError(f"Could not connect to {p.host}: {exc}") from exc

        # Paramiko doesn't set this itself. Without it, Nagle's algorithm
        # can sit on the single-byte packets an interactive shell sends per
        # keystroke waiting for an ACK, which is the classic "SSH typing
        # feels laggy" complaint -- nothing to do with actual network RTT.
        transport = client.get_transport()
        if transport is not None and transport.sock is not None:
            try:
                transport.sock.setsockopt(
                    socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            except OSError:
                pass

        try:
            chan = client.invoke_shell(
                term="xterm-256color", width=80, height=24
            )
        except paramiko.SSHException as exc:
            client.close()
            raise TransportError(f"Could not open a shell: {exc}") from exc

        chan.settimeout(READ_TIMEOUT)
        self._client, self._chan = client, chan
        self._connected = True

    def read(self) -> bytes | None:
        chan = self._chan
        if chan is None:
            return None
        try:
            data = chan.recv(65536)
        except TimeoutError:
            return b""  # nothing yet, ask again
        except (OSError, paramiko.SSHException):
            return None
        if not data:  # clean EOF
            return None
        return data

    def write(self, data: bytes) -> None:
        if self._chan is None:
            raise TransportError("Not connected")
        try:
            self._chan.sendall(data)
        except (OSError, paramiko.SSHException) as exc:
            raise TransportError(f"Write failed: {exc}") from exc

    def close(self) -> None:
        self._connected = False
        for obj in (self._chan, self._client):
            try:
                if obj is not None:
                    obj.close()
            except Exception:
                pass
        self._chan = self._client = None

    # -- capabilities ------------------------------------------------------

    def resize(self, cols: int, rows: int) -> None:
        if self._chan is not None:
            try:
                self._chan.resize_pty(width=cols, height=rows)
            except (OSError, paramiko.SSHException):
                pass

    @property
    def description(self) -> str:
        p = self.profile
        who = f"{p.username}@" if p.username else ""
        port = "" if p.port == 22 else f":{p.port}"
        return f"SSH  {who}{p.host}{port}"
