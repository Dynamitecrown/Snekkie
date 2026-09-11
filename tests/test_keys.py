"""Key encoding tests.

Every entry here is a sequence a real terminal expects. Getting one wrong
means a key silently does nothing, or worse, does something else.
"""

import pytest
from PySide6.QtCore import QEvent, Qt
from PySide6.QtGui import QKeyEvent

from snekkie.ui import keys


def key_event(key, mods=Qt.NoModifier, text=""):
    return QKeyEvent(QEvent.KeyPress, key, mods, text)


@pytest.mark.parametrize(
    "key,app_mode,expected",
    [
        (Qt.Key_Up, False, b"\x1b[A"),
        (Qt.Key_Down, False, b"\x1b[B"),
        (Qt.Key_Right, False, b"\x1b[C"),
        (Qt.Key_Left, False, b"\x1b[D"),
        (Qt.Key_Home, False, b"\x1b[H"),
        (Qt.Key_End, False, b"\x1b[F"),
        # Application cursor mode swaps CSI for SS3.
        (Qt.Key_Up, True, b"\x1bOA"),
        (Qt.Key_Left, True, b"\x1bOD"),
    ],
)
def test_cursor_keys(qapp, key, app_mode, expected):
    assert keys.encode(key_event(key), app_mode) == expected


@pytest.mark.parametrize(
    "key,expected",
    [
        (Qt.Key_F1, b"\x1bOP"),
        (Qt.Key_F4, b"\x1bOS"),
        (Qt.Key_F5, b"\x1b[15~"),
        (Qt.Key_F12, b"\x1b[24~"),
        (Qt.Key_Insert, b"\x1b[2~"),
        (Qt.Key_Delete, b"\x1b[3~"),
        (Qt.Key_PageUp, b"\x1b[5~"),
        (Qt.Key_PageDown, b"\x1b[6~"),
    ],
)
def test_function_and_navigation_keys(qapp, key, expected):
    assert keys.encode(key_event(key)) == expected


@pytest.mark.parametrize(
    "key,expected",
    [
        (Qt.Key_Return, b"\r"),
        (Qt.Key_Enter, b"\r"),
        (Qt.Key_Tab, b"\t"),
        (Qt.Key_Escape, b"\x1b"),
        # DEL, not BS -- matches PuTTY's default and modern shells.
        (Qt.Key_Backspace, b"\x7f"),
    ],
)
def test_simple_keys(qapp, key, expected):
    assert keys.encode(key_event(key)) == expected


@pytest.mark.parametrize(
    "key,expected",
    [
        (Qt.Key_A, b"\x01"),
        (Qt.Key_C, b"\x03"),  # the one that matters most
        (Qt.Key_Z, b"\x1a"),
        (Qt.Key_BracketLeft, b"\x1b"),
        (Qt.Key_Space, b"\x00"),
    ],
)
def test_control_characters(qapp, key, expected):
    assert keys.encode(key_event(key, Qt.ControlModifier)) == expected


def test_modified_cursor_key_uses_xterm_parameter(qapp):
    # Ctrl -> modifier code 5 (1 + 4)
    assert keys.encode(key_event(Qt.Key_Up, Qt.ControlModifier)) == b"\x1b[1;5A"
    # Shift -> modifier code 2 (1 + 1)
    assert keys.encode(key_event(Qt.Key_Right, Qt.ShiftModifier)) == b"\x1b[1;2C"


def test_alt_prefixes_escape(qapp):
    assert keys.encode(key_event(Qt.Key_B, Qt.AltModifier, "b")) == b"\x1bb"


def test_plain_text_passes_through(qapp):
    assert keys.encode(key_event(Qt.Key_A, Qt.NoModifier, "a")) == b"a"


def test_non_printing_key_is_ignored(qapp):
    assert keys.encode(key_event(Qt.Key_Shift, Qt.NoModifier, "")) is None
