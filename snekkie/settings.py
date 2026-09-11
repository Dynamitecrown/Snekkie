"""App-wide preferences: colour theme and default font.

Distinct from Profile (profiles.py): a Profile is one saved connection, this
is the one set of look-and-feel settings shared by the whole app. Plain JSON
in the same config directory, same load/save shape as ProfileStore.
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field, fields
from pathlib import Path

from .profiles import config_dir

#: Keys every theme carries.
THEME_KEYS = ("fg", "bg", "cursor", "selection")

#: Preset colour themes: terminal foreground, background, cursor, and the
#: highlight colour used for selected text. The 16-colour ANSI palette
#: (terminal.py's PALETTE) stays fixed across themes -- these four are what
#: actually change the terminal's look at a glance.
THEMES: dict[str, dict[str, str]] = {
    "Snekkie Dark": {
        "fg": "#d0d0d0", "bg": "#1a1a1a",
        "cursor": "#3ad900", "selection": "#3a5a80",
    },
    "Solarized Dark": {
        "fg": "#839496", "bg": "#002b36",
        "cursor": "#268bd2", "selection": "#073642",
    },
    "Solarized Light": {
        "fg": "#657b83", "bg": "#fdf6e3",
        "cursor": "#268bd2", "selection": "#eee8d5",
    },
    "Monokai": {
        "fg": "#f8f8f2", "bg": "#272822",
        "cursor": "#a6e22e", "selection": "#49483e",
    },
    "Classic Green": {
        "fg": "#33ff33", "bg": "#0c0c0c",
        "cursor": "#33ff33", "selection": "#1f4d1f",
    },
    "High Contrast": {
        "fg": "#ffffff", "bg": "#000000",
        "cursor": "#ffff00", "selection": "#444444",
    },
}

DEFAULT_THEME = "Snekkie Dark"

#: Themes that have been renamed. A settings.json written before the app was
#: renamed still says "PyTerm Dark", and without this it would quietly fall
#: back to the default instead of resolving to the same colours.
THEME_ALIASES = {"PyTerm Dark": "Snekkie Dark"}


@dataclass
class AppSettings:
    theme: str = DEFAULT_THEME
    custom_fg: str = "#d0d0d0"
    custom_bg: str = "#1a1a1a"
    custom_cursor: str = "#3ad900"
    custom_selection: str = "#3a5a80"

    #: Themes the user saved, name -> {fg, bg, cursor, selection}. Kept
    #: separate from THEMES so a future preset can never clobber one.
    saved_themes: dict[str, dict[str, str]] = field(default_factory=dict)

    # Defaults filled into a brand-new session's Advanced tab. A saved
    # profile's own values, once set, always win over these.
    font_family: str = ""
    font_size: int = 11
    scrollback: int = 5000

    show_sidebar: bool = True

    def custom_colors(self) -> dict[str, str]:
        """The unsaved, currently-being-edited custom scheme."""
        return {
            "fg": self.custom_fg, "bg": self.custom_bg,
            "cursor": self.custom_cursor, "selection": self.custom_selection,
        }

    def colors(self) -> dict[str, str]:
        if self.theme == "Custom":
            return self.custom_colors()
        if self.theme in self.saved_themes:
            scheme = self.saved_themes[self.theme]
            # Fall back per key so a hand-edited settings.json missing one
            # colour still yields a usable theme rather than a KeyError.
            return {k: scheme.get(k, THEMES[DEFAULT_THEME][k])
                    for k in THEME_KEYS}
        name = THEME_ALIASES.get(self.theme, self.theme)
        return THEMES.get(name, THEMES[DEFAULT_THEME])

    def theme_names(self) -> list[str]:
        """Built-in presets first, then the user's own, then Custom."""
        return [*THEMES, *sorted(self.saved_themes), "Custom"]

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict) -> AppSettings:
        known = {f.name for f in fields(cls)}
        values = {k: v for k, v in data.items() if k in known}
        # settings.json is a plain text file users do edit by hand, so don't
        # trust the shape of this one -- a malformed theme here would
        # otherwise blow up at paint time, far from the cause.
        raw_themes = values.get("saved_themes")
        clean: dict[str, dict[str, str]] = {}
        if isinstance(raw_themes, dict):
            for name, scheme in raw_themes.items():
                if isinstance(name, str) and isinstance(scheme, dict):
                    colours = {k: v for k, v in scheme.items()
                               if k in THEME_KEYS and isinstance(v, str)}
                    if colours:
                        clean[name] = colours
        values["saved_themes"] = clean
        return cls(**values)


class SettingsStore:
    def __init__(self, path: Path | None = None):
        self.path = path or (config_dir() / "settings.json")

    def load(self) -> AppSettings:
        try:
            raw = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return AppSettings()
        return AppSettings.from_dict(raw)

    def save(self, settings: AppSettings) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text(json.dumps(settings.to_dict(), indent=2), encoding="utf-8")
        tmp.replace(self.path)
