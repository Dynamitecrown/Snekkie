"""Keyword-based syntax highlighting for the terminal display.

Purely a rendering overlay: it recolours words the screen buffer happens to
contain that match the chosen device's command syntax. It doesn't touch
what's sent to the remote, doesn't parse anything, and can't tell your typed
command from the device's echo of it -- both show up in the buffer the same
way, so both get highlighted the same way. Add a new device by adding an
entry to SYNTAXES.
"""

from __future__ import annotations

import re

Rule = tuple[re.Pattern, str]

#: category -> hex colour, applied over whatever the terminal's own theme
#: draws. Kept device-agnostic so every syntax shares one look.
COLORS: dict[str, str] = {
    "keyword": "#4fa8e0",
    "value": "#c9a86a",
    "prompt": "#5fd97a",
    "negate": "#e0665a",
}

_CISCO_IOS_KEYWORDS = (
    "show", "configure", "terminal", "interface", "shutdown", "ip", "ipv6",
    "address", "hostname", "enable", "disable", "exit", "write", "copy",
    "running-config", "startup-config", "vlan", "switchport", "access",
    "access-list", "permit", "deny", "router", "network", "description",
    "duplex", "speed", "spanning-tree", "channel-group", "line", "vty",
    "console", "password", "secret", "login", "banner", "end", "reload",
    "ping", "traceroute", "clock", "logging", "snmp-server", "crypto",
    "key", "route", "default-gateway", "mode", "trunk", "encapsulation",
    "mtu", "version", "service", "boot", "system", "do", "wr", "conf",
)

SYNTAXES: dict[str, list[Rule]] = {
    "cisco_ios": [
        (re.compile(r"^no\b", re.IGNORECASE), "negate"),
        (re.compile(r"\b(" + "|".join(_CISCO_IOS_KEYWORDS) + r")\b",
                    re.IGNORECASE), "keyword"),
        (re.compile(r"\b\d{1,3}(?:\.\d{1,3}){3}(?:/\d{1,2})?\b"), "value"),
        (re.compile(r"^\S+[>#]\s*$"), "prompt"),
    ],
}

#: value -> label, in display order. "none" always means no highlighting.
SYNTAX_LABELS: dict[str, str] = {
    "none": "None",
    "cisco_ios": "Cisco IOS",
}


def highlight_line(text: str, syntax: str) -> dict[int, str]:
    """Column index -> hex colour for one line of terminal text."""
    rules = SYNTAXES.get(syntax)
    if not rules or not text.strip():
        return {}
    # Trailing blanks are most of a terminal row and can never match, so
    # only run the regexes over the part of the line that holds text.
    end = len(text.rstrip())
    if not end:
        return {}
    text = text[:end]
    overrides: dict[int, str] = {}
    for pattern, category in rules:
        color = COLORS[category]
        for match in pattern.finditer(text):
            overrides.update(
                dict.fromkeys(range(match.start(), match.end()), color))
    return overrides
