"""Terminal emulation tests.

These are the highest-value tests in the suite: emulation bugs show up as
garbled output that is painful to diagnose by eye, and they are easy to
introduce when swapping or upgrading the emulator underneath.
"""

from snekkie.emulation import Terminal


def test_plain_text_lands_on_the_screen():
    t = Terminal(80, 24)
    t.feed(b"Switch#show run\r\n")
    assert t.line_text(0) == "Switch#show run"


def test_sgr_colour_and_bold():
    t = Terminal(80, 24)
    t.feed(b"\x1b[1;32mBuilding configuration...\x1b[0m\r\n")
    assert t.line_text(0) == "Building configuration..."
    char = t.buffer[0][0]
    assert char.fg == "green"
    assert char.bold is True


def test_256_colour_reports_hex():
    t = Terminal(80, 24)
    t.feed(b"\x1b[38;5;208morange\x1b[0m")
    # pyte hands back bare hex for 256/truecolour; the renderer prefixes '#'.
    assert t.buffer[0][0].fg == "ff8700"


def test_reset_attributes_returns_to_default():
    t = Terminal(80, 24)
    t.feed(b"\x1b[31mred\x1b[0m plain")
    assert t.buffer[0][0].fg == "red"
    assert t.buffer[0][4].fg == "default"


def test_clear_screen_and_home_cursor():
    t = Terminal(80, 24)
    t.feed(b"junk on screen\r\nmore junk\r\n")
    t.feed(b"\x1b[H\x1b[2J")
    assert t.line_text(0) == ""
    assert (t.cursor.x, t.cursor.y) == (0, 0)


def test_cursor_addressing():
    t = Terminal(80, 24)
    t.feed(b"\x1b[10;20H")
    # CUP is 1-indexed on the wire, 0-indexed internally.
    assert (t.cursor.y, t.cursor.x) == (9, 19)


def test_application_cursor_key_mode_toggles():
    t = Terminal(80, 24)
    assert t.application_cursor_keys is False
    t.feed(b"\x1b[?1h")
    assert t.application_cursor_keys is True
    t.feed(b"\x1b[?1l")
    assert t.application_cursor_keys is False


def test_cursor_visibility_toggles():
    t = Terminal(80, 24)
    assert t.cursor_visible is True
    t.feed(b"\x1b[?25l")
    assert t.cursor_visible is False
    t.feed(b"\x1b[?25h")
    assert t.cursor_visible is True


def test_resize_keeps_the_screen_usable():
    t = Terminal(80, 24)
    t.feed(b"hello")
    t.resize(132, 40)
    assert (t.columns, t.lines) == (132, 40)
    assert t.line_text(0) == "hello"


def test_resize_clamps_to_a_minimum():
    t = Terminal(80, 24)
    t.resize(0, 0)
    assert t.columns >= 2 and t.lines >= 2


def test_scrollback_paging():
    t = Terminal(80, 24, scrollback=500)
    for i in range(120):
        t.feed(f"line {i}\r\n".encode())
    assert t.page_up() is True
    assert t.scrolled_back is True
    while t.page_down():
        pass
    assert t.scrolled_back is False


def test_utf8_split_across_feeds():
    """A multi-byte character arriving in two TCP segments must not corrupt."""
    t = Terminal(80, 24)
    t.feed(b"\xc3")
    t.feed(b"\xa9")
    assert t.line_text(0) == "é"


def test_reset_restores_geometry():
    t = Terminal(100, 30)
    t.feed(b"\x1b[31mstuff")
    t.reset()
    assert (t.columns, t.lines) == (100, 30)
    assert t.line_text(0) == ""
