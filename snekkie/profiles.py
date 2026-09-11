"""Saved sessions.

Profiles are plain JSON in the user's config directory. Passwords are
deliberately *not* stored -- if you want that later, hook the `keyring`
package into SessionTab rather than writing secrets into this file.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
from dataclasses import asdict, dataclass, fields
from pathlib import Path

#: What the config directory was called before the app was renamed. Kept so
#: an existing install's saved sessions survive the change.
LEGACY_DIR_NAME = "pyterm"


def config_dir() -> Path:
    if sys.platform == "win32":
        base = Path(os.environ.get("APPDATA", Path.home() / "AppData" / "Roaming"))
    elif sys.platform == "darwin":
        base = Path.home() / "Library" / "Application Support"
    else:
        base = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
    return base / "snekkie"


def migrate_legacy_config() -> None:
    """Carry saved sessions and settings over from the old pyterm directory.

    Renaming the app moved where it looks for its config, which would present
    as every saved session having vanished. Copies rather than moves: if
    anything here goes wrong the originals are still sitting in the old
    directory, and a stale copy costs nothing.

    Only runs when the new directory has no config of its own, so it can
    never overwrite newer settings.
    """
    new = config_dir()
    old = new.with_name(LEGACY_DIR_NAME)
    try:
        if old == new or not old.is_dir():
            return
        if any(new.glob("*.json")):
            return  # already set up here; leave it alone
        new.mkdir(parents=True, exist_ok=True)
        for item in old.glob("*.json"):
            shutil.copy2(item, new / item.name)
    except OSError:
        pass  # non-fatal: worst case the sessions get re-created by hand


@dataclass
class Profile:
    name: str = "New session"
    kind: str = "ssh"  # matches Transport.kind

    # SSH
    host: str = ""
    port: int = 22
    username: str = ""
    auth: str = "password"  # password | key | agent
    key_file: str = ""

    # Serial
    device: str = ""
    baud: int = 9600
    bytesize: int = 8
    parity: str = "None"
    stopbits: float = 1
    rtscts: bool = False
    xonxoff: bool = False

    # Terminal
    scrollback: int = 5000
    font_family: str = ""  # empty = platform default monospace
    font_size: int = 11
    log_path: str = ""  # empty = no logging
    device_syntax: str = "none"  # see ui/highlight.py SYNTAX_LABELS

    def copy(self) -> Profile:
        return Profile(**asdict(self))

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict) -> Profile:
        known = {f.name for f in fields(cls)}
        return cls(**{k: v for k, v in data.items() if k in known})


class ProfileStore:
    """Ordered collection of profiles, persisted to sessions.json."""

    def __init__(self, path: Path | None = None):
        self.path = path or (config_dir() / "sessions.json")
        self.profiles: list[Profile] = []
        self.load()

    def load(self) -> None:
        try:
            raw = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            self.profiles = []
            return
        self.profiles = [Profile.from_dict(d) for d in raw.get("sessions", [])]

    def save(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        payload = {"version": 1,
                   "sessions": [p.to_dict() for p in self.profiles]}
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        tmp.replace(self.path)

    # -- collection ops ----------------------------------------------------

    def get(self, name: str) -> Profile | None:
        return next((p for p in self.profiles if p.name == name), None)

    def put(self, profile: Profile) -> None:
        """Insert or overwrite by name, then persist."""
        existing = self.get(profile.name)
        if existing is not None:
            self.profiles[self.profiles.index(existing)] = profile
        else:
            self.profiles.append(profile)
            self.profiles.sort(key=lambda p: p.name.lower())
        self.save()

    def remove(self, name: str) -> None:
        profile = self.get(name)
        if profile is not None:
            self.profiles.remove(profile)
            self.save()

    def names(self) -> list[str]:
        return [p.name for p in self.profiles]
