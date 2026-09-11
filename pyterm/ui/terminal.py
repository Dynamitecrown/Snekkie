"""The terminal widget: draws the pyte screen, collects keyboard/mouse input."""

from __future__ import annotations

from PySide6.QtCore import QRect, Qt, QTimer, Signal
from PySide6.QtGui import (
    QColor,
    QFont,
    QFontDatabase,
    QFontMetricsF,
    QGuiApplication,
    QPainter,
)
from PySide6.QtWidgets import QSizePolicy, QWidget

from ..emulation import Terminal
from . import keys
from .highlight import highlight_line

# --------------------------------------------------------------------------
# Palette. pyte reports colours either as a name (the 16 ANSI colours) or as
# a bare hex string for 256-colour / truecolour SGR sequences.
# --------------------------------------------------------------------------

DEFAULT_FG = "#d0d0d0"
DEFAULT_BG = "#1a1a1a"
CURSOR_COLOR = "#3ad900"
SELECTION_BG = "#3a5a80"

PALETTE = {
    "black": "#2e3436", "red": "#cc3333", "green": "#4e9a06",
    "brown": "#c4a000", "blue": "#3465a4", "magenta": "#a347ba",
    "cyan": "#06989a", "white": "#d3d7cf",
    "brightblack": "#666666", "brightred": "#ef4a4a",
    "brightgreen": "#8ae234", "brightbrown": "#fce94f",
    "brightblue": "#729fcf", "brightmagenta": "#ad7fa8",
    "brightcyan": "#34e2e2", "brightwhite": "#eeeeec",
}

#: Repaint at most this often (ms). Without throttling, a `show run` dump
#: would trigger a full repaint per network packet and the UI would crawl.
REPAINT_INTERVAL = 25

#: Distinct lines to keep syntax-highlighted results for. Big enough to cover
#: a screenful many times over, small enough that a long session can't grow it
#: without bound.
HIGHLIGHT_CACHE_MAX = 1000

#: Largest upward shift worth blitting. Beyond this, so little of the old
#: screen survives that copying it is not worth the detection cost.
MAX_BLIT_ROWS = 16

#: Lines the wheel scrolls per notch. pyte pages by half a screen, which is
#: far too coarse to read with.
WHEEL_LINES = 3


def _resolve(color: str, *, bold: bool, default: str) -> QColor:
    if color == "default":
        return QColor(default)
    if bold and color in PALETTE and not color.startswith("bright"):
        color = "bright" + color
    if color in PALETTE:
        return QColor(PALETTE[color])
    if len(color) == 6:  # pyte hands back bare hex for 256/truecolour
        return QColor("#" + color)
    return QColor(default)


class TerminalWidget(QWidget):
    """Renders a Terminal and emits the bytes the user types."""

    data_typed = Signal(bytes)
    size_changed = Signal(int, int)  # cols, rows
    scroll_changed = Signal(int, int)  # lines_back, total_lines

    def __init__(self, scrollback: int = 5000, font_family: str = "",
                 font_size: int = 11, theme: dict[str, str] | None = None,
                 syntax: str = "none", parent=None):
        super().__init__(parent)
        self.terminal = Terminal(80, 24, scrollback)
        self._syntax = syntax

        self.setFocusPolicy(Qt.StrongFocus)
        self.setAttribute(Qt.WA_OpaquePaintEvent, True)
        self.setCursor(Qt.IBeamCursor)
        self.setSizePolicy(QSizePolicy.Expanding, QSizePolicy.Expanding)

        self._set_font(font_family, font_size)
        # Always goes through _apply_theme, even with no theme passed, so the
        # derived QColors and caches it builds are never missing.
        self._apply_theme(theme or {})

        self._sel_anchor: tuple[int, int] | None = None
        self._sel_head: tuple[int, int] | None = None
        self._selecting = False

        self._last_cursor_row: int | None = None

        # Rows whose repaint is waiting out the throttle interval. Painting
        # only these, instead of the whole widget, is what keeps a keystroke
        # from costing a full-screen redraw 25 ms after it echoes.
        self._pending_rows: set[int] = set()
        #: Cells of each row as currently drawn on screen, or None where that
        #: is unknown. Drives the scroll-blit check in _try_blit_scroll.
        self._painted_rows: list[tuple | None] = []
        self._painted_cursor_row: int | None = None
        #: last (scroll_back, scroll_total, lines) handed to scroll_changed
        self._last_scroll: tuple[int, int, int] | None = None
        #: sub-notch wheel movement not yet worth a whole line
        self._wheel_remainder = 0
        #: row text -> {column: hex colour} from the syntax pass. Recomputing
        #: this per paint dominated rendering. Keyed on the text rather than
        #: the row number so it survives scrolling: a line that scrolls up a
        #: row is the same string and keeps its colours, where a row-indexed
        #: cache would miss on every line of a `show run` dump.
        self._hl_cache: dict[str, dict[int, str]] = {}

        self._blink_on = True
        self._blink = QTimer(self)
        self._blink.timeout.connect(self._toggle_blink)
        self._blink.start(530)

        self._repaint = QTimer(self)
        self._repaint.setSingleShot(True)
        self._repaint.timeout.connect(self._flush_pending)

    # -- font / metrics ----------------------------------------------------

    def _set_font(self, family: str, size: int) -> None:
        if family:
            font = QFont(family, size)
        else:
            font = QFontDatabase.systemFont(QFontDatabase.FixedFont)
            font.setPointSize(size)
        font.setFixedPitch(True)
        font.setStyleHint(QFont.Monospace)
        self._font = font
        self._font_bold = QFont(font)
        self._font_bold.setBold(True)
        # (bold, italic, underline) -> QFont. paintEvent used to build a fresh
        # QFont for every styled run; there are only eight possible
        # combinations, so build each once and hand back the same object.
        self._font_cache: dict[tuple[bool, bool, bool], QFont] = {}

        metrics = QFontMetricsF(font)
        self._cw = max(metrics.horizontalAdvance("M"), 1.0)
        self._ch = max(metrics.height(), 1.0)
        self._baseline = metrics.ascent()

    def _styled_font(self, bold: bool, italic: bool, underline: bool) -> QFont:
        key = (bold, italic, underline)
        font = self._font_cache.get(key)
        if font is None:
            font = QFont(self._font_bold if bold else self._font)
            font.setItalic(italic)
            font.setUnderline(underline)
            self._font_cache[key] = font
        return font

    def set_font_config(self, family: str, size: int) -> None:
        self._set_font(family, size)
        self._apply_geometry()
        self._invalidate_caches()
        self.update()

    # -- theme ---------------------------------------------------------------

    def _apply_theme(self, theme: dict[str, str]) -> None:
        self._fg = theme.get("fg", DEFAULT_FG)
        self._bg = theme.get("bg", DEFAULT_BG)
        self._cursor_color = theme.get("cursor", CURSOR_COLOR)
        self._selection_bg = theme.get("selection", SELECTION_BG)
        # Prebuilt so paintEvent never constructs a QColor per drawn run.
        self._bg_qcolor = QColor(self._bg)
        self._sel_qcolor = QColor(self._selection_bg)
        self._cursor_qcolor = QColor(self._cursor_color)
        # Separate caches: "default" resolves to the theme's foreground in one
        # and its background in the other, so they cannot share a key.
        self._fg_cache: dict[tuple[str, bool], QColor] = {}
        self._bg_cache: dict[str, QColor] = {}

    def _fg_color(self, color: str, bold: bool) -> QColor:
        key = (color, bold)
        resolved = self._fg_cache.get(key)
        if resolved is None:
            resolved = _resolve(color, bold=bold, default=self._fg)
            if len(self._fg_cache) >= 1024:
                self._fg_cache.clear()
            self._fg_cache[key] = resolved
        return resolved

    def _bg_color(self, color: str) -> QColor:
        resolved = self._bg_cache.get(color)
        if resolved is None:
            resolved = _resolve(color, bold=False, default=self._bg)
            if len(self._bg_cache) >= 1024:
                self._bg_cache.clear()
            self._bg_cache[color] = resolved
        return resolved

    def _invalidate_caches(self) -> None:
        """Drop derived render state after something structural changes."""
        self._hl_cache.clear()
        # What is on screen can no longer be trusted to match these, so the
        # blit check must not reuse it.
        self._painted_rows = []

    def set_theme(self, theme: dict[str, str]) -> None:
        self._apply_theme(theme)
        self._invalidate_caches()
        self.update()

    def set_syntax(self, syntax: str) -> None:
        self._syntax = syntax
        self._invalidate_caches()
        self.update()

    def sizeHint(self):
        from PySide6.QtCore import QSize
        return QSize(int(self._cw * 80) + 4, int(self._ch * 24) + 4)

    # -- incoming data -----------------------------------------------------

    def feed(self, data: bytes) -> None:
        self.terminal.feed(data)
        self._clear_selection()

        # pyte tells us exactly which rows changed -- usually just the one
        # line being typed on. Repainting only that (plus wherever the
        # cursor was and now is) means a keystroke's echo doesn't have to
        # redraw, or re-run syntax highlighting over, the whole visible
        # scrollback on every character.
        dirty_rows = self.terminal.pop_dirty()
        cursor_row = self.terminal.cursor.y
        if self._last_cursor_row is not None:
            dirty_rows.add(self._last_cursor_row)
        dirty_rows.add(cursor_row)
        self._last_cursor_row = cursor_row
        self._pending_rows |= dirty_rows
        # Scroll state must stay accurate even mid-burst, unlike painting --
        # the throttle below only governs how often we redraw, not this.
        self._emit_scroll()

        if self._repaint.isActive():
            return  # already queued; _flush_pending will pick these up too
        self._flush_pending()
        self._repaint.start(REPAINT_INTERVAL)

    def _row_cells(self, y: int) -> tuple:
        """Immutable cell snapshot, including ANSI attributes and wide cells."""
        line = self.terminal.buffer[y]
        return tuple(line.get(i, line.default)
                     for i in range(self.terminal.columns))

    def _row_text(self, y: int) -> str:
        # line.get(i, blank) rather than line[i]: pyte rows only store cells
        # that were written, and indexing a missing one goes through
        # __missing__ on every blank cell.
        line = self.terminal.buffer[y]
        get = line.get
        blank = line.default
        return "".join([get(i, blank).data
                        for i in range(self.terminal.columns)])

    def _try_blit_scroll(self, pending: set[int]) -> set[int] | None:
        """Scroll the screen's pixels up instead of redrawing every row.

        When output arrives on a full screen, every row's text shifts up one
        and pyte marks the whole screen dirty -- so the naive thing is a full
        repaint per line, which is what made a maximised window crawl.

        Compare immutable cell snapshots, including their ANSI attributes,
        before copying pixels. Unknown rows and fractional pixel shifts fall
        back to repainting. The copied cursor is explicitly invalidated.

        Returns the rows still needing paint, or None to repaint normally.
        """
        painted = self._painted_rows
        rows = self.terminal.lines
        if len(painted) != rows or not self.isVisible():
            return None
        # Only worth attempting when most of the screen is dirty; a keystroke
        # touches one row and would just pay for the text comparison.
        if len(pending) <= rows // 2:
            return None

        if self._selection_range() is not None:
            return None
        current = [self._row_cells(y) for y in range(rows)]
        # Enough of the screen has to survive the shift to be worth copying.
        min_prefix = rows // 2
        for n in range(1, min(MAX_BLIT_ROWS, rows - 1) + 1):
            # How far down does the shifted screen still match what is drawn?
            # Not all the way, normally: the line that triggered the scroll was
            # written into the bottom row first, so it lands inside this range
            # already changed. Everything above it is still a pure shift.
            # QWidget.scroll takes integer logical pixels. A fractional shift
            # cannot reproduce the original glyph baselines exactly.
            distance = n * self._ch
            if not distance.is_integer():
                continue
            limit = rows - n
            matched = 0
            while matched < limit and current[matched] == painted[matched + n]:
                matched += 1
            if matched >= min_prefix:
                self.scroll(0, -int(n * self._ch))
                # Rows that survived the blit are correct on screen now; the
                # strip at the bottom is not drawn yet, so mark it unknown.
                self._painted_rows = painted[n:] + [None] * n
                dirty = set(range(matched, rows))
                if self._painted_cursor_row is not None:
                    shifted_cursor = self._painted_cursor_row - n
                    if shifted_cursor >= 0:
                        dirty.add(shifted_cursor)
                dirty.add(self.terminal.cursor.y)
                return dirty
        return None

    def _flush_pending(self) -> None:
        """Repaint the rows changed since the last paint, and only those."""
        rows = self._pending_rows
        if not rows:
            return
        blitted = self._try_blit_scroll(rows)
        if blitted is not None:
            rows = blitted
        self._pending_rows = set()
        top = min(rows) * self._ch
        height = (max(rows) - min(rows) + 1) * self._ch
        self.update(QRect(0, int(top), self.width(), int(height) + 1))

    def clear(self) -> None:
        self.terminal.clear()
        self._repaint_all()
        self._emit_scroll()

    def reset(self) -> None:
        self.terminal.reset()
        self._repaint_all()
        self._emit_scroll()

    def _repaint_all(self) -> None:
        """Every row may have changed: drop cached rows and redraw in full."""
        self._invalidate_caches()
        self._pending_rows.clear()
        self.update()

    # -- scrollback ----------------------------------------------------------

    def _emit_scroll(self) -> None:
        # Typing doesn't add history, so the scroll state is usually identical
        # to last time. Skipping the unchanged case keeps every keystroke from
        # driving the scrollbar's setRange/setValue and its repaint.
        state = (self.terminal.scroll_back, self.terminal.scroll_total,
                 self.terminal.lines)
        if state == self._last_scroll:
            return
        self._last_scroll = state
        self.scroll_changed.emit(state[0], state[1])

    def scroll_to(self, lines_back: int) -> None:
        """Used by an external scrollbar; wheel/keys call page_up/page_down
        on self.terminal directly since those already move by a whole page."""
        self.terminal.scroll_to(lines_back)
        self._repaint_all()
        self._emit_scroll()

    # -- geometry ----------------------------------------------------------

    def _apply_geometry(self) -> None:
        cols = max(int(self.width() / self._cw), 2)
        rows = max(int(self.height() / self._ch), 2)
        if (cols, rows) != (self.terminal.columns, self.terminal.lines):
            self.terminal.resize(cols, rows)
            self._invalidate_caches()  # every row's text just moved
            self._pending_rows.clear()
            self.size_changed.emit(cols, rows)
            self._emit_scroll()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._apply_geometry()

    # -- painting ----------------------------------------------------------

    def _toggle_blink(self) -> None:
        self._blink_on = not self._blink_on
        if self.hasFocus():
            cur = self.terminal.cursor
            y = int(cur.y * self._ch)
            self.update(QRect(0, y, self.width(), int(self._ch) + 1))

    def _row_overrides(self, row_text: str) -> dict[int, str]:
        """Syntax colours for one row, computed at most once per distinct line."""
        cache = self._hl_cache
        cached = cache.get(row_text)
        if cached is None:
            if len(cache) >= HIGHLIGHT_CACHE_MAX:
                cache.clear()  # cheaper than tracking an LRU for this
            cached = highlight_line(row_text, self._syntax)
            cache[row_text] = cached
        return cached

    def paintEvent(self, event):
        painter = QPainter(self)
        rect = event.rect()
        painter.fillRect(rect, self._bg_qcolor)

        term = self.terminal
        buffer = term.buffer
        cols, rows = term.columns, term.lines
        cw, ch = self._cw, self._ch
        bg_default = self._bg_qcolor
        baseline = self._baseline
        highlighting = self._syntax != "none"

        first = max(int(rect.top() / ch), 0)
        last = min(int(rect.bottom() / ch) + 1, rows)
        sel = self._selection_range()
        sel_start, sel_end = sel if sel is not None else (-1, -1)

        # Qt re-resolves the font on every setFont, so only call it when the
        # style actually changes rather than once per run.
        cur_font = cur_pen = None
        # Hoisted out of the per-cell loop: attribute lookups on self are not
        # free when they happen a few thousand times per paint.
        fg_color = self._fg_color
        bg_color = self._bg_color
        styled_font = self._styled_font
        row_overrides = self._row_overrides
        col_range = range(cols)

        # Tracks what is actually on screen, so _try_blit_scroll can tell
        # whether the pixels it wants to copy are still trustworthy. Only
        # rows this paint touches are recorded; the rest keep their entry.
        if len(self._painted_rows) != rows:
            self._painted_rows = [None] * rows
        painted_rows = self._painted_rows

        for y in range(first, last):
            line = buffer[y]
            top = y * ch
            # pyte lines are dicts that materialise blanks on access. Pulling
            # the row out once turns two dict lookups per cell (here and in
            # the run scan below) into one, and list indexing after that.
            chars = [line.get(i, line.default) for i in col_range]
            row_text = "".join([c.data for c in chars])
            # Partial exposure does not establish that a whole row is valid.
            row_rect = QRect(0, int(top), self.width(), int(ch) + 1)
            painted_rows[y] = (tuple(chars) if event.region().contains(row_rect)
                               else None)
            overrides = row_overrides(row_text) if highlighting else {}
            # Syntax offsets are Unicode string offsets; terminal columns
            # include empty continuation cells for wide glyphs.
            column_overrides = {}
            offset = 0
            for column, cell in enumerate(chars):
                if offset in overrides and cell.data:
                    column_overrides[column] = overrides[offset]
                offset += len(cell.data)
            get_override = column_overrides.get
            row_base = y * cols
            x = 0
            while x < cols:
                char = chars[x]
                c_fg, c_bg = char.fg, char.bg
                c_bold, c_italics = char.bold, char.italics
                c_under, c_reverse = char.underscore, char.reverse
                selected = sel_start <= row_base + x <= sel_end
                override = get_override(x)

                # Coalesce the run of cells sharing this style, so a full line
                # of plain text is one drawText call instead of eighty.
                # Compared field by field: building a tuple per cell for this
                # cost more than the comparison itself.
                run_end = x + 1
                while run_end < cols:
                    nxt = chars[run_end]
                    if (nxt.fg != c_fg or nxt.bg != c_bg
                            or nxt.bold != c_bold or nxt.italics != c_italics
                            or nxt.underscore != c_under
                            or nxt.reverse != c_reverse
                            or get_override(run_end) != override
                            or (sel_start <= row_base + run_end <= sel_end)
                            != selected):
                        break
                    run_end += 1

                fg = fg_color(c_fg, c_bold)
                bg = bg_color(c_bg)
                if c_reverse:
                    fg, bg = bg, fg
                if override and not selected:
                    fg = QColor(override)
                if selected:
                    bg = self._sel_qcolor

                if bg != bg_default:
                    painter.fillRect(
                        QRect(int(x * cw), int(top),
                              int((run_end - x) * cw) + 1, int(ch) + 1), bg)

                text = "".join(c.data for c in chars[x:run_end])
                if text.strip():
                    font = styled_font(c_bold, c_italics, c_under)
                    if font is not cur_font:
                        painter.setFont(font)
                        cur_font = font
                    if fg != cur_pen:
                        painter.setPen(fg)
                        cur_pen = fg
                    painter.drawText(int(x * cw), int(top + baseline), text)

                x = run_end

        self._paint_cursor(painter)
        self._painted_cursor_row = (term.cursor.y if term.cursor_visible
                                    and not term.scrolled_back else None)
        painter.end()

    def _paint_cursor(self, painter: QPainter) -> None:
        term = self.terminal
        if not term.cursor_visible or term.scrolled_back:
            return
        cur = term.cursor
        if not (0 <= cur.y < term.lines and 0 <= cur.x < term.columns):
            return

        rect = QRect(int(cur.x * self._cw), int(cur.y * self._ch),
                     int(self._cw) + 1, int(self._ch))
        if not self.hasFocus():
            painter.setPen(self._cursor_qcolor)
            painter.drawRect(rect.adjusted(0, 0, -1, -1))
            return
        if not self._blink_on:
            return

        painter.fillRect(rect, self._cursor_qcolor)
        char = term.buffer[cur.y][cur.x]
        if char.data.strip():
            painter.setFont(self._font_bold if char.bold else self._font)
            painter.setPen(self._bg_qcolor)
            painter.drawText(int(cur.x * self._cw),
                             int(cur.y * self._ch + self._baseline), char.data)

    # -- keyboard ----------------------------------------------------------

    def keyPressEvent(self, event):
        mods = event.modifiers()
        ctrl_shift = (mods & Qt.ControlModifier) and (mods & Qt.ShiftModifier)

        if ctrl_shift and event.key() == Qt.Key_C:
            self.copy_selection()
            return
        if ctrl_shift and event.key() == Qt.Key_V:
            self.paste()
            return

        # Shift+PgUp/PgDn scroll locally instead of going to the far end.
        if mods & Qt.ShiftModifier and event.key() in (Qt.Key_PageUp,
                                                       Qt.Key_PageDown):
            if event.key() == Qt.Key_PageUp:
                self.terminal.page_up()
            else:
                self.terminal.page_down()
            self._repaint_all()
            self._emit_scroll()
            return

        data = keys.encode(event, self.terminal.application_cursor_keys)
        if data:
            self._blink_on = True
            self.data_typed.emit(data)
        else:
            super().keyPressEvent(event)

    def focusNextPrevChild(self, next_child: bool) -> bool:
        """Keep Tab for the far end instead of moving focus.

        Qt consumes Tab and Shift+Tab for focus navigation before they ever
        reach keyPressEvent, so pressing Tab to complete a command jumped to
        the sidebar instead. Refusing here sends them on to keyPressEvent,
        which encodes them as TAB and CSI Z -- what IOS completion and shell
        completion both expect.
        """
        return False

    def wheelEvent(self, event):
        """Scroll a few lines per notch, the way every other app does.

        angleDelta is in eighths of a degree and a normal notch is 120, but
        high-resolution wheels and trackpads send much smaller increments, so
        the leftover is carried rather than rounded away -- otherwise fine
        scrolling registers as nothing at all.
        """
        delta = event.angleDelta().y()
        if not delta:
            return
        self._wheel_remainder += delta * WHEEL_LINES
        lines = int(self._wheel_remainder / 120)  # truncates toward zero
        self._wheel_remainder -= lines * 120
        if lines and self.terminal.scroll_by(lines):
            self._repaint_all()
            self._emit_scroll()

    # -- mouse / selection -------------------------------------------------

    def _cell_at(self, pos) -> tuple[int, int]:
        col = min(max(int(pos.x() / self._cw), 0), self.terminal.columns - 1)
        row = min(max(int(pos.y() / self._ch), 0), self.terminal.lines - 1)
        return row, col

    def _selection_range(self) -> tuple[int, int] | None:
        if self._sel_anchor is None or self._sel_head is None:
            return None
        cols = self.terminal.columns
        a = self._sel_anchor[0] * cols + self._sel_anchor[1]
        b = self._sel_head[0] * cols + self._sel_head[1]
        return (a, b) if a <= b else (b, a)

    def _clear_selection(self) -> None:
        if self._sel_anchor is not None:
            self._sel_anchor = self._sel_head = None
            self.update()
            # A whole-widget repaint is now outstanding, so what is on screen
            # no longer matches the per-row record. Forget it rather than let
            # the scroll blit copy pixels that are about to be redrawn.
            self._painted_rows = []

    def mousePressEvent(self, event):
        if event.button() == Qt.RightButton:
            self.paste()  # PuTTY habit: right-click pastes
            return
        if event.button() == Qt.LeftButton:
            self._selecting = True
            self._sel_anchor = self._sel_head = self._cell_at(event.position())
            self.update()

    def mouseMoveEvent(self, event):
        if self._selecting:
            self._sel_head = self._cell_at(event.position())
            self.update()

    def mouseReleaseEvent(self, event):
        if event.button() == Qt.LeftButton and self._selecting:
            self._selecting = False
            self._sel_head = self._cell_at(event.position())
            if self._sel_anchor == self._sel_head:
                self._sel_anchor = self._sel_head = None
            else:
                self.copy_selection()  # PuTTY habit: selecting copies
            self.update()

    def mouseDoubleClickEvent(self, event):
        """Double-click selects the whole line."""
        row, _ = self._cell_at(event.position())
        self._sel_anchor = (row, 0)
        self._sel_head = (row, self.terminal.columns - 1)
        self.copy_selection()
        self.update()

    # -- clipboard ---------------------------------------------------------

    def selected_text(self) -> str:
        sel = self._selection_range()
        if sel is None:
            return ""
        cols = self.terminal.columns
        start, end = sel
        lines = []
        for y in range(start // cols, end // cols + 1):
            lo = start - y * cols if y == start // cols else 0
            hi = end - y * cols if y == end // cols else cols - 1
            line = self.terminal.buffer[y]
            lines.append("".join(line[x].data
                                 for x in range(lo, hi + 1)).rstrip())
        return "\n".join(lines)

    def copy_selection(self) -> None:
        text = self.selected_text()
        if text:
            QGuiApplication.clipboard().setText(text)

    def paste(self) -> None:
        text = QGuiApplication.clipboard().text()
        if text:
            # Normalise line endings; terminals want CR, not CRLF.
            self.data_typed.emit(
                text.replace("\r\n", "\r").replace("\n", "\r").encode("utf-8")
            )

    def select_all(self) -> None:
        self._sel_anchor = (0, 0)
        self._sel_head = (self.terminal.lines - 1, self.terminal.columns - 1)
        self.update()

    # -- focus -------------------------------------------------------------

    def focusInEvent(self, event):
        super().focusInEvent(event)
        self._blink_on = True
        self.update()

    def focusOutEvent(self, event):
        super().focusOutEvent(event)
        self.update()
