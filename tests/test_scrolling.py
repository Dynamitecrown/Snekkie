"""Scroll granularity and the Tab key.

pyte only advertises half-screen paging, which made the wheel jump twenty
lines at a time; Terminal.scroll_by borrows history.ratio to move an exact
number of rows instead. Tab has to reach the far end rather than being eaten
by Qt's focus navigation.
"""

import pytest
from PySide6.QtCore import QEvent, QPoint, Qt
from PySide6.QtGui import QKeyEvent, QWheelEvent

from pyterm.emulation import Terminal
from pyterm.ui.terminal import WHEEL_LINES, TerminalWidget


@pytest.fixture
def widget(qapp):
    w = TerminalWidget(scrollback=5000)
    w.resize(900, 600)
    w.show()
    qapp.processEvents()
    for i in range(300):
        w.feed(f"line {i}\r\n".encode())
    qapp.processEvents()
    yield w
    w.close()


def _wheel(widget, notch_eighths):
    pos = QPoint(50, 50)
    return QWheelEvent(
        pos.toPointF(), widget.mapToGlobal(pos).toPointF(), QPoint(0, 0),
        QPoint(0, notch_eighths), Qt.NoButton, Qt.NoModifier,
        Qt.ScrollUpdate, False,
    )


# -- emulation ---------------------------------------------------------------

def test_scroll_by_moves_an_exact_number_of_lines():
    term = Terminal(80, 24, scrollback=1000)
    for i in range(200):
        term.feed(f"line {i}\r\n".encode())

    assert term.scroll_by(1)
    assert term.scroll_back == 1
    assert term.scroll_by(9)
    assert term.scroll_back == 10
    assert term.scroll_by(-4)
    assert term.scroll_back == 6


def test_scroll_by_restores_the_page_ratio():
    """Borrowing history.ratio must not leave it changed for the next call."""
    term = Terminal(80, 24, scrollback=1000)
    for i in range(200):
        term.feed(f"line {i}\r\n".encode())

    before = term.screen.history.ratio
    term.scroll_by(7)
    assert term.screen.history.ratio == before


def test_scroll_to_lands_exactly_on_target():
    term = Terminal(80, 24, scrollback=1000)
    for i in range(400):
        term.feed(f"line {i}\r\n".encode())

    term.scroll_to(137)
    assert term.scroll_back == 137
    term.scroll_to(0)
    assert term.scroll_back == 0


def test_scroll_to_clamps_to_available_history():
    term = Terminal(80, 24, scrollback=1000)
    for i in range(50):
        term.feed(f"line {i}\r\n".encode())

    term.scroll_to(10_000)
    assert term.scroll_back == term.scroll_total
    term.scroll_to(-5)
    assert term.scroll_back == 0


def test_a_long_seek_does_not_leave_rows_below_the_screen():
    """pyte assumes a page fits on screen; a bigger step corrupts the buffer.

    Asking prev_page to travel further than screen.lines makes it assign
    buffer rows past the last line, and those stay there for the life of the
    session -- a scrollbar drag used to leave hundreds behind, which is both
    a leak and more for the garbage collector to walk.
    """
    term = Terminal(80, 24, scrollback=1000)
    for i in range(500):
        term.feed(f"line {i}\r\n".encode())

    term.scroll_to(300)  # far further than the 24-row screen

    assert term.scroll_back == 300
    assert len(term.screen.buffer) <= term.lines + 1, "rows left below the screen"
    assert term.scroll_total == 477, "history was consumed by the seek"
    # The view still shows 24 consecutive lines from the right place.
    assert term.line_text(0) == "line 177"
    assert term.line_text(23) == "line 200"


def test_seeking_back_and_forth_keeps_the_buffer_bounded():
    term = Terminal(80, 24, scrollback=1000)
    for i in range(500):
        term.feed(f"line {i}\r\n".encode())

    for target in (400, 0, 250, 5, 477, 0):
        term.scroll_to(target)
        assert len(term.screen.buffer) <= term.lines + 1


def test_scrolling_back_does_not_lose_history():
    term = Terminal(80, 24, scrollback=1000)
    for i in range(200):
        term.feed(f"line {i}\r\n".encode())
    total = term.scroll_total

    term.scroll_by(30)
    term.scroll_by(-30)
    assert term.scroll_total == total


# -- widget ------------------------------------------------------------------

def test_one_wheel_notch_scrolls_a_few_lines_not_half_a_screen(widget, qapp):
    rows = widget.terminal.lines
    before = widget.terminal.scroll_back
    qapp.sendEvent(widget, _wheel(widget, 120))

    moved = widget.terminal.scroll_back - before
    assert moved == WHEEL_LINES
    assert moved < rows // 2, "this is the coarse paging we moved away from"


def test_sub_notch_wheel_deltas_accumulate(widget, qapp):
    """Trackpads send fractions of a notch; rounding them away scrolls nothing."""
    before = widget.terminal.scroll_back
    for _ in range(8):
        qapp.sendEvent(widget, _wheel(widget, 15))  # 1/8 notch each
    assert widget.terminal.scroll_back > before


def test_wheel_down_scrolls_back_toward_the_live_bottom(widget, qapp):
    qapp.sendEvent(widget, _wheel(widget, 120 * 4))
    scrolled = widget.terminal.scroll_back
    assert scrolled > 0

    qapp.sendEvent(widget, _wheel(widget, -120 * 4))
    assert widget.terminal.scroll_back < scrolled


# -- Tab ---------------------------------------------------------------------

def test_tab_is_not_stolen_for_focus_navigation(widget):
    """Qt would otherwise move focus to the sidebar instead of completing."""
    assert widget.focusNextPrevChild(True) is False
    assert widget.focusNextPrevChild(False) is False


def test_tab_and_shift_tab_are_sent_to_the_far_end(widget):
    sent = []
    widget.data_typed.connect(sent.append)

    widget.keyPressEvent(QKeyEvent(QEvent.KeyPress, Qt.Key_Tab,
                                   Qt.NoModifier, "\t"))
    assert sent[-1] == b"\t"

    widget.keyPressEvent(QKeyEvent(QEvent.KeyPress, Qt.Key_Backtab,
                                   Qt.ShiftModifier, ""))
    assert sent[-1] == b"\x1b[Z"
