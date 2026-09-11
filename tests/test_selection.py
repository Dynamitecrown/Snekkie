"""Dragging a selection through scrollback.

Selecting a long `show run` means the drag has to scroll the view and the
copy has to reach lines that are no longer displayed, so selection endpoints
are absolute document rows rather than screen rows.
"""

import pytest
from PySide6.QtCore import QEvent, QPointF, Qt
from PySide6.QtGui import QMouseEvent

from snekkie.ui.terminal import AUTOSCROLL_MAX_LINES, TerminalWidget


@pytest.fixture
def widget(qapp):
    w = TerminalWidget(scrollback=5000)
    w.resize(800, 300)
    w.show()
    qapp.processEvents()
    for i in range(200):
        w.feed(f"line {i:03}\r\n".encode())
    qapp.processEvents()
    w.repaint()
    yield w
    w.close()


def _press(widget, x, y):
    widget.mousePressEvent(QMouseEvent(
        QEvent.MouseButtonPress, QPointF(x, y), QPointF(x, y),
        Qt.LeftButton, Qt.LeftButton, Qt.NoModifier))


def _move(widget, x, y):
    widget.mouseMoveEvent(QMouseEvent(
        QEvent.MouseMove, QPointF(x, y), QPointF(x, y),
        Qt.NoButton, Qt.LeftButton, Qt.NoModifier))


def _release(widget, x, y):
    widget.mouseReleaseEvent(QMouseEvent(
        QEvent.MouseButtonRelease, QPointF(x, y), QPointF(x, y),
        Qt.LeftButton, Qt.NoButton, Qt.NoModifier))


def _selected_numbers(widget):
    return [int(line.split()[1])
            for line in widget.selected_text().splitlines()
            if line.startswith("line ")]


# -- coordinates -------------------------------------------------------------

def test_selection_is_anchored_to_content_not_the_screen(widget):
    """Scrolling must not drag the anchor along with the view."""
    _press(widget, 10, widget.height() - 5)
    anchor = widget._sel_anchor

    widget.terminal.scroll_by(20)

    assert widget._sel_anchor == anchor


def test_selection_survives_scrolling_and_still_copies_the_same_line(widget):
    row = widget.terminal.lines - 1
    text_before = widget.terminal.line_text(row)
    widget._sel_anchor = (widget.terminal.view_top + row, 0)
    widget._sel_head = (widget.terminal.view_top + row,
                        widget.terminal.columns - 1)
    copied_before = widget.selected_text()

    widget.terminal.scroll_by(30)

    assert widget.selected_text() == copied_before
    assert copied_before.strip() == text_before.strip()


# -- auto-scroll -------------------------------------------------------------

def test_dragging_above_the_widget_starts_autoscroll(widget):
    _press(widget, 10, widget.height() - 5)
    assert not widget._autoscroll.isActive()

    _move(widget, 10, -40)

    assert widget._autoscroll.isActive()


def test_drag_back_inside_stops_autoscroll(widget):
    _press(widget, 10, widget.height() - 5)
    _move(widget, 10, -40)
    assert widget._autoscroll.isActive()

    _move(widget, 10, widget.height() // 2)

    assert not widget._autoscroll.isActive()


def test_releasing_stops_autoscroll(widget):
    _press(widget, 10, widget.height() - 5)
    _move(widget, 10, -40)

    _release(widget, 10, -40)

    assert not widget._autoscroll.isActive()
    assert widget._selecting is False


def test_autoscroll_speed_ramps_with_distance(widget):
    near = widget._autoscroll_lines(QPointF(10, -2))
    far = widget._autoscroll_lines(QPointF(10, -500))

    assert near >= 1
    assert far > near
    assert far <= AUTOSCROLL_MAX_LINES, "runaway scrolling"


def test_autoscroll_direction_follows_the_edge(widget):
    widget.terminal.scroll_by(40)  # room to move both ways
    above = widget._autoscroll_lines(QPointF(10, -30))
    below = widget._autoscroll_lines(QPointF(10, widget.height() + 30))
    inside = widget._autoscroll_lines(QPointF(10, widget.height() // 2))

    assert above > 0, "dragging up should head toward older output"
    assert below < 0
    assert inside == 0


def test_autoscroll_stops_at_the_top_of_the_history(widget):
    _press(widget, 10, widget.height() - 5)
    _move(widget, 10, -400)
    for _ in range(500):  # far more than the history holds
        widget._autoscroll_step()

    assert not widget._autoscroll.isActive()
    assert widget.terminal.view_top == 0


# -- what gets copied --------------------------------------------------------

def test_drag_past_the_edge_selects_beyond_one_screen(widget):
    rows = widget.terminal.lines
    _press(widget, 10, widget.height() - 5)
    _move(widget, 10, -40)
    for _ in range(15):
        widget._autoscroll_step()
    _release(widget, 10, -40)

    numbers = _selected_numbers(widget)
    assert len(numbers) > rows, "selection never grew past the visible screen"
    assert numbers == list(range(numbers[0], numbers[-1] + 1)), "copied gaps"


def test_copy_reaches_lines_that_scrolled_off(widget):
    """The point of the exercise: text no longer on screen still copies."""
    widget._sel_anchor = (0, 0)
    widget._sel_head = (30, widget.terminal.columns - 1)

    numbers = _selected_numbers(widget)

    assert numbers[0] == 0, "oldest scrollback line was not reachable"
    assert 0 < widget.terminal.view_top, "fixture should have real history"


def test_select_all_covers_the_whole_scrollback(widget):
    widget.select_all()

    assert _selected_numbers(widget) == list(range(200))


def test_highlight_is_drawn_on_the_selected_lines_after_scrolling(qapp):
    """paintEvent maps screen rows to document rows; off-by-one shows here."""
    w = TerminalWidget(scrollback=5000, theme={
        "fg": "#ffffff", "bg": "#000000",
        "cursor": "#00ff00", "selection": "#ff0000"})
    w.resize(800, 300)
    w.show()
    qapp.processEvents()
    for i in range(200):
        w.feed(f"line {i:03}\r\n".encode())
    qapp.processEvents()
    w.repaint()

    def highlighted_rows():
        image = w.grab().toImage()
        found = []
        for row in range(w.terminal.lines):
            y = int(row * w._ch + w._ch / 2)
            if any(image.pixelColor(x, y).name() == "#ff0000"
                   for x in range(2, 200, 7)):
                found.append(row)
        return found

    top = w.terminal.view_top
    selected = range(top + 3, top + 9)
    w._sel_anchor = (selected[0], 0)
    w._sel_head = (selected[-1], w.terminal.columns - 1)
    w._repaint_all()
    w.repaint()
    assert highlighted_rows() == [3, 4, 5, 6, 7, 8]

    w.terminal.scroll_by(5)
    w._repaint_all()
    w.repaint()
    moved = w.terminal.view_top
    assert highlighted_rows() == [r - moved for r in selected]
    # Same content, drawn five rows further down.
    assert w.terminal.line_text(selected[0] - moved).strip() == \
        f"line {selected[0] - top + 181:03}"
    w.close()


def test_selection_inside_one_screen_still_works(widget):
    """The ordinary case has to keep behaving."""
    top = widget.terminal.view_top
    widget._sel_anchor = (top, 0)
    widget._sel_head = (top, 7)

    assert widget.selected_text() == widget.terminal.line_text(0)[:8]


def test_selection_uses_columns_after_wide_character(qapp):
    from snekkie.ui.terminal import TerminalWidget

    widget = TerminalWidget()
    try:
        widget.feed("\u754cabc".encode())
        widget._sel_anchor = (0, 2)
        widget._sel_head = (0, 3)
        assert widget.selected_text() == "ab"
    finally:
        widget.close()
