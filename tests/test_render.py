"""Renderer tests: incremental repainting, caching, and highlighting.

The renderer only repaints the rows pyte reports as changed and caches the
syntax pass per row. Both are easy to break in ways that don't raise -- they
just silently draw the wrong thing -- so these tests compare against a full
redraw rather than checking for exceptions.
"""

import pytest

from snekkie.ui.highlight import COLORS, highlight_line
from snekkie.ui.terminal import TerminalWidget

CISCO = (
    b"Switch1#show running-config\r\n"
    b"interface GigabitEthernet0/1\r\n"
    b" ip address 192.168.1.1 255.255.255.0\r\n"
    b" no shutdown\r\n"
)


@pytest.fixture
def widget(qapp):
    w = TerminalWidget(scrollback=500, syntax="cisco_ios")
    w.resize(800, 480)
    w.show()
    qapp.processEvents()
    yield w
    w.close()


def _full_redraw(widget):
    """Pixels the widget produces when nothing is cached or clipped."""
    widget._repaint_all()
    widget.repaint()
    return widget.grab().toImage()


def test_incremental_paint_matches_full_redraw(widget, qapp):
    """The whole point of dirty-row painting: same pixels, less work."""
    widget.feed(CISCO)
    qapp.processEvents()
    widget.repaint()
    incremental = widget.grab().toImage()

    assert incremental == _full_redraw(widget)


def test_incremental_paint_matches_after_scrolling_burst(widget, qapp):
    """Scrolling rewrites every row; the caches have to be dropped for it."""
    for i in range(120):
        widget.feed(f"interface GigabitEthernet0/{i}\r\n".encode())
    qapp.processEvents()
    widget.repaint()
    incremental = widget.grab().toImage()

    assert incremental == _full_redraw(widget)


def test_typing_invalidates_only_the_edited_row(widget, qapp):
    """A keystroke must not invalidate the whole screen.

    Measured on the region handed to update(), which is what decides how
    much paintEvent has to redraw.
    """
    for i in range(60):
        widget.feed(f"line {i}\r\n".encode())
    qapp.processEvents()
    widget._flush_pending()  # drain the setup's backlog first
    widget.repaint()

    rows = []
    original_update = widget.update

    def record(*args):
        # update() with no rect means "the whole widget".
        rows.append(round(args[0].height() / widget._ch) if args
                    else widget.terminal.lines)
        return original_update(*args)

    widget.update = record
    try:
        for char in b"show ver":
            widget.feed(bytes([char]))
            widget._flush_pending()
    finally:
        del widget.update

    assert rows, "expected at least one repaint request"
    # Generous bound: the point is that it is a couple of rows, not ~40.
    assert max(rows) <= 3, rows


def _stream(widget, qapp, lines, prefix="ip address 10.0"):
    """Feed output through the real path, one line at a time."""
    for i in range(lines):
        widget.feed(f"{prefix}.{i % 250}.1 255.255.255.0 no shutdown\r\n".encode())
        widget._flush_pending()
        qapp.processEvents()


def _whole_pixel_rows(widget):
    """Round the row height so the blit can engage on any platform.

    The blit deliberately refuses fractional row heights, and some default
    fonts (Linux CI's among them) have one, which would leave these tests
    exercising only the fallback.
    """
    widget._ch = float(round(widget._ch))
    widget._apply_geometry()


def test_scrolling_blits_instead_of_repainting_every_row(widget, qapp):
    """Output on a full screen must copy the pixels up, not redraw them all.

    Guards a real regression: an earlier version of the shift detection never
    matched, so it silently fell back to full repaints. Pixels stayed correct,
    which is exactly why the pixel tests alone could not catch it.
    """
    _whole_pixel_rows(widget)
    for i in range(120):
        widget.feed(f"line {i} interface GigabitEthernet0/{i}\r\n".encode())
    qapp.processEvents()
    widget.repaint()

    results = []
    original = widget._try_blit_scroll

    def spy(pending):
        outcome = original(pending)
        results.append(outcome)
        return outcome

    widget._try_blit_scroll = spy
    try:
        _stream(widget, qapp, 10)
    finally:
        del widget._try_blit_scroll

    blits = [r for r in results if r is not None]
    assert blits, "the scroll blit never engaged"
    rows = widget.terminal.lines
    # A blit should leave only the newly exposed strip to paint.
    assert all(len(b) <= rows // 2 for b in blits), [len(b) for b in blits]


def test_blitted_scroll_produces_the_same_pixels_as_a_full_redraw(widget, qapp):
    for i in range(120):
        widget.feed(f"interface GigabitEthernet0/{i}\r\n".encode())
    qapp.processEvents()
    widget.repaint()

    _stream(widget, qapp, 15)
    incremental = widget.grab().toImage()

    assert incremental == _full_redraw(widget)


def test_blit_is_skipped_when_the_screen_state_is_unknown(widget, qapp):
    """With no record of what is drawn, copying pixels would be a guess."""
    for i in range(60):
        widget.feed(f"line {i}\r\n".encode())
    qapp.processEvents()
    widget.repaint()

    widget._repaint_all()  # drops the record of what is on screen
    assert widget._try_blit_scroll(set(range(widget.terminal.lines))) is None


def test_highlight_cache_survives_scrolling(widget, qapp):
    """Keyed on text, so a line keeps its colours when it scrolls up a row."""
    widget.feed(b"no shutdown\r\n")
    qapp.processEvents()
    widget.repaint()
    entries = dict(widget._hl_cache)
    key = next(k for k in entries if k.startswith("no shutdown"))

    for i in range(5):  # push that line upward
        widget.feed(f"line {i}\r\n".encode())
    qapp.processEvents()
    widget.repaint()

    assert widget._hl_cache[key] is entries[key], "should be a cache hit, not a recompute"


def test_highlight_cache_is_bounded(widget, qapp):
    from snekkie.ui.terminal import HIGHLIGHT_CACHE_MAX

    for i in range(HIGHLIGHT_CACHE_MAX + 200):
        widget.feed(f"interface GigabitEthernet0/{i}\r\n".encode())
        widget.repaint()
    assert len(widget._hl_cache) <= HIGHLIGHT_CACHE_MAX


def test_switching_syntax_clears_cached_colours(widget, qapp):
    widget.feed(b"no shutdown")
    qapp.processEvents()
    widget.repaint()
    assert widget._hl_cache

    widget.set_syntax("none")
    assert not widget._hl_cache
    widget.repaint()  # must not resurrect stale colours


def test_default_colour_differs_for_foreground_and_background():
    """The fg/bg colour caches must not share an entry for "default"."""
    w = TerminalWidget(scrollback=50, theme={"fg": "#ff0000", "bg": "#0000ff"})
    assert w._fg_color("default", False).name() == "#ff0000"
    assert w._bg_color("default").name() == "#0000ff"


def test_scroll_signal_is_not_emitted_when_nothing_changed(widget, qapp):
    widget.feed(b"hello")
    seen = []
    widget.scroll_changed.connect(lambda *args: seen.append(args))
    widget.feed(b" world")  # same row, no new history
    assert seen == []


def test_highlight_ignores_trailing_blanks():
    padded = "no shutdown" + " " * 200
    assert highlight_line(padded, "cisco_ios") == highlight_line(
        "no shutdown", "cisco_ios")


def test_highlight_marks_keywords_values_and_negation():
    line = "no ip address 10.0.0.1 255.255.255.0"
    overrides = highlight_line(line, "cisco_ios")
    assert overrides[0] == COLORS["negate"]
    assert overrides[line.index("address")] == COLORS["keyword"]
    assert overrides[line.index("10.0.0.1")] == COLORS["value"]


def test_unknown_syntax_highlights_nothing():
    assert highlight_line("no shutdown", "not-a-device") == {}


def test_blit_rejects_identical_text_with_changed_ansi_colours(widget, qapp):
    rows = widget.terminal.lines
    widget.feed(b"\x1b[31m" + b"same text\r\n" * (rows + 5))
    qapp.processEvents()
    widget.repaint()
    widget.terminal.feed(b"\x1b[H\x1b[32m" + b"same text\r\n" * rows)
    assert widget._try_blit_scroll(set(range(rows))) is None


def test_blit_rejects_fractional_pixel_shifts(widget, qapp):
    widget.feed(b"same text\r\n" * 60)
    qapp.processEvents()
    widget.repaint()
    widget._ch = 15.1234567
    assert widget._try_blit_scroll(set(range(widget.terminal.lines))) is None


def test_scroll_blit_invalidates_the_copied_cursor(widget, qapp):
    _whole_pixel_rows(widget)
    widget.feed(b"same text\r\n" * 60)
    qapp.processEvents()
    widget.repaint()
    widget._painted_cursor_row = 5
    widget.terminal.feed(b"new line\r\n")
    dirty = widget._try_blit_scroll(set(range(widget.terminal.lines)))
    assert dirty is not None
    assert 4 in dirty


def test_colour_caches_are_bounded(widget):
    for i in range(1200):
        widget._fg_color(f"{i:06x}", False)
        widget._bg_color(f"{i:06x}")
    assert len(widget._fg_cache) <= 1024
    assert len(widget._bg_cache) <= 1024
