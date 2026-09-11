"""Transport layer.

Everything in here reduces a connection to the same thing: a bidirectional
stream of bytes. SSH, serial, and anything you add later (telnet, raw TCP,
a local shell) only has to satisfy the Transport interface below, and the
rest of the application will work with it unchanged.
"""

from __future__ import annotations

from abc import ABC, abstractmethod


class TransportError(Exception):
    """Connect/read/write failure that should be shown to the user."""


class Transport(ABC):
    #: Short identifier, set by @register. Matches Profile.kind.
    kind: str = "base"

    #: Human-readable name shown in the new-session dialog.
    label: str = "Base"

    def __init__(self, profile):
        self.profile = profile
        self._connected = False

    # -- lifecycle ---------------------------------------------------------

    @abstractmethod
    def connect(self) -> None:
        """Open the connection. Raise TransportError on failure."""

    @abstractmethod
    def read(self) -> bytes | None:
        """Read whatever is available.

        Must not block indefinitely -- use a short timeout so the reader
        thread stays responsive to shutdown.

        Returns:
            bytes: data received (may be empty if the timeout expired first)
            None:  the far end closed. The session is over.
        """

    @abstractmethod
    def write(self, data: bytes) -> None:
        """Send bytes to the far end."""

    @abstractmethod
    def close(self) -> None:
        """Tear down. Must be safe to call more than once."""

    # -- optional capabilities --------------------------------------------

    def resize(self, cols: int, rows: int) -> None:  # noqa: B027
        """Tell the far end the window changed size.

        Meaningful for SSH (SIGWINCH), meaningless for a serial line.
        Deliberately concrete and empty: transports opt in, they don't have
        to implement a no-op just to satisfy the interface.
        """

    def send_break(self) -> None:
        """Send a line break. Cisco password recovery lives here."""
        raise TransportError(f"{self.label} sessions do not support break")

    # -- state -------------------------------------------------------------

    @property
    def is_connected(self) -> bool:
        return self._connected

    @property
    def description(self) -> str:
        """One-line summary for the status bar."""
        return self.label


# --------------------------------------------------------------------------
# Registry. Adding a transport = write the class, decorate it, import it in
# _load(). Nothing else in the codebase needs to know it exists.
# --------------------------------------------------------------------------

_REGISTRY: dict[str, type[Transport]] = {}


def register(kind: str, label: str):
    def decorate(cls: type[Transport]) -> type[Transport]:
        cls.kind = kind
        cls.label = label
        _REGISTRY[kind] = cls
        return cls

    return decorate


def _load() -> None:
    from . import serialport, ssh  # noqa: F401  -- import for side effects


def create(profile, **kwargs) -> Transport:
    _load()
    try:
        cls = _REGISTRY[profile.kind]
    except KeyError:
        raise TransportError(f"Unknown session type: {profile.kind!r}") from None
    return cls(profile, **kwargs)


def available() -> dict[str, str]:
    """Mapping of kind -> label, for populating menus."""
    _load()
    return {kind: cls.label for kind, cls in sorted(_REGISTRY.items())}
