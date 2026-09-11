"""Translate Qt key events into the byte sequences a terminal expects.

Reference behaviour is xterm, which is what nearly everything (including
Cisco IOS) is built to talk to.
"""

from __future__ import annotations

from PySide6.QtCore import Qt
from PySide6.QtGui import QKeyEvent

# Keys whose escape sequence changes between normal and application mode.
_CURSOR = {
    Qt.Key_Up: "A",
    Qt.Key_Down: "B",
    Qt.Key_Right: "C",
    Qt.Key_Left: "D",
    Qt.Key_Home: "H",
    Qt.Key_End: "F",
}

# Keys using the CSI n ~ form.
_TILDE = {
    Qt.Key_Insert: 2,
    Qt.Key_Delete: 3,
    Qt.Key_PageUp: 5,
    Qt.Key_PageDown: 6,
    Qt.Key_F5: 15,
    Qt.Key_F6: 17,
    Qt.Key_F7: 18,
    Qt.Key_F8: 19,
    Qt.Key_F9: 20,
    Qt.Key_F10: 21,
    Qt.Key_F11: 23,
    Qt.Key_F12: 24,
}

# F1-F4 are SS3-prefixed, not CSI.
_SS3_FN = {
    Qt.Key_F1: "P",
    Qt.Key_F2: "Q",
    Qt.Key_F3: "R",
    Qt.Key_F4: "S",
}

_SIMPLE = {
    Qt.Key_Return: b"\r",
    Qt.Key_Enter: b"\r",
    Qt.Key_Tab: b"\t",
    Qt.Key_Backtab: b"\x1b[Z",
    Qt.Key_Escape: b"\x1b",
    # PuTTY's default: Backspace sends DEL, which is what modern shells and
    # IOS both expect. Flip to b"\x08" if you meet an old box that disagrees.
    Qt.Key_Backspace: b"\x7f",
}


def _modifier_code(mods: Qt.KeyboardModifiers) -> int:
    """xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4)."""
    code = 1
    if mods & Qt.ShiftModifier:
        code += 1
    if mods & Qt.AltModifier:
        code += 2
    if mods & Qt.ControlModifier:
        code += 4
    return code


def encode(event: QKeyEvent, app_cursor: bool = False) -> bytes | None:
    """Return the bytes to send, or None if the key should be ignored."""
    key = event.key()
    mods = event.modifiers()
    mod = _modifier_code(mods)
    modded = mod > 1

    if key in _CURSOR:
        final = _CURSOR[key]
        if modded:
            return f"\x1b[1;{mod}{final}".encode()
        prefix = "\x1bO" if app_cursor else "\x1b["
        return f"{prefix}{final}".encode()

    if key in _TILDE:
        num = _TILDE[key]
        if modded:
            return f"\x1b[{num};{mod}~".encode()
        return f"\x1b[{num}~".encode()

    if key in _SS3_FN:
        final = _SS3_FN[key]
        if modded:
            return f"\x1b[1;{mod}{final}".encode()
        return f"\x1bO{final}".encode()

    if key in _SIMPLE:
        return _SIMPLE[key]

    text = event.text()

    # Ctrl+letter -> control character. Qt usually gives us this in text()
    # already, but not on every platform or layout, so compute it ourselves.
    if mods & Qt.ControlModifier and not (mods & Qt.AltModifier):
        if Qt.Key_A <= key <= Qt.Key_Z:
            ctrl = bytes([key - Qt.Key_A + 1])
            return b"\x1b" + ctrl if mods & Qt.AltModifier else ctrl
        extra = {
            Qt.Key_BracketLeft: b"\x1b",   # Ctrl+[
            Qt.Key_Backslash: b"\x1c",     # Ctrl+\
            Qt.Key_BracketRight: b"\x1d",  # Ctrl+]
            Qt.Key_AsciiCircum: b"\x1e",   # Ctrl+^
            Qt.Key_Underscore: b"\x1f",    # Ctrl+_
            Qt.Key_Space: b"\x00",         # Ctrl+Space -> NUL
            Qt.Key_2: b"\x00",
        }
        if key in extra:
            return extra[key]

    if not text:
        return None

    data = text.encode("utf-8", errors="replace")

    # Alt+key sends ESC then the key (xterm "meta sends escape").
    if mods & Qt.AltModifier:
        return b"\x1b" + data

    return data
