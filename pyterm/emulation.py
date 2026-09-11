"""Terminal emulation.

This is the layer that turns raw bytes into a screen. Writing it yourself is
the classic trap -- ANSI/VT100 has scroll regions, origin mode, character
sets, 256-colour and truecolour SGR, and a hundred edge cases that only show
up when you run `nano` over a flaky link. pyte already handles all of it, so
this module is just a thin, well-behaved wrapper.
"""

from __future__ import annotations

from itertools import islice

import pyte

#: DEC private mode 1 (cursor keys application mode). pyte stores private
#: modes shifted left by 5, whether or not it recognises them by name.
DECCKM = 1 << 5

#: DEC private mode 25 -- cursor visible.
DECTCEM = pyte.modes.DECTCEM


class Terminal:
    def __init__(self, cols: int = 80, rows: int = 24, scrollback: int = 5000):
        self.screen = pyte.HistoryScreen(
            max(cols, 2), max(rows, 2), history=max(scrollback, 0), ratio=0.5
        )
        self.stream = pyte.ByteStream(self.screen)
        self._scrollback = scrollback

    # -- input -------------------------------------------------------------

    def feed(self, data: bytes) -> None:
        self.stream.feed(data)

    def pop_dirty(self) -> set[int]:
        """Row indices touched since the last call, then reset for the next.

        pyte already tracks this internally (Screen.dirty) for exactly this
        purpose: so a renderer doesn't have to redraw rows nothing touched.
        """
        dirty = self.screen.dirty
        rows = set(dirty)
        dirty.clear()
        return rows

    # -- geometry ----------------------------------------------------------

    @property
    def columns(self) -> int:
        return self.screen.columns

    @property
    def lines(self) -> int:
        return self.screen.lines

    def resize(self, cols: int, rows: int) -> None:
        cols, rows = max(cols, 2), max(rows, 2)
        if (cols, rows) != (self.screen.columns, self.screen.lines):
            self.screen.resize(rows, cols)

    # -- state queries used by the renderer --------------------------------

    @property
    def cursor(self):
        return self.screen.cursor

    @property
    def cursor_visible(self) -> bool:
        return DECTCEM in self.screen.mode

    @property
    def application_cursor_keys(self) -> bool:
        """True when the far end wants ESC O A instead of ESC [ A."""
        return DECCKM in self.screen.mode

    @property
    def buffer(self):
        return self.screen.buffer

    def line_text(self, row: int) -> str:
        """Plain text of one visible row, trailing blanks stripped."""
        line = self.screen.buffer[row]
        cols = self.screen.columns
        return "".join(line[x].data for x in range(cols)).rstrip()

    def text(self) -> str:
        return "\n".join(self.line_text(y) for y in range(self.screen.lines))

    # -- the whole document (scrollback + screen) --------------------------
    #
    # pyte keeps three pieces: history.top holds what scrolled off above the
    # screen (oldest first), buffer holds the visible rows, history.bottom
    # holds what sits below when scrolled back (nearest first). Concatenated
    # they are the document, and an "absolute" row index addresses it, which
    # is what a selection needs -- a screen row number stops meaning anything
    # the moment the view scrolls under it.

    @property
    def view_top(self) -> int:
        """Absolute index of the topmost visible row."""
        try:
            return len(self.screen.history.top)
        except Exception:
            return 0

    @property
    def total_lines(self) -> int:
        try:
            history = self.screen.history
            return (len(history.top) + self.screen.lines
                    + len(history.bottom))
        except Exception:
            return self.screen.lines

    def document_rows(self, start: int, end: int) -> list:
        """Rows at absolute indices [start, end], inclusive and clamped."""
        try:
            history = self.screen.history
            top, bottom = history.top, history.bottom
        except Exception:
            return []
        lines = self.screen.lines
        top_len = len(top)
        start = max(start, 0)
        end = min(end, top_len + lines + len(bottom) - 1)
        if start > end:
            return []

        rows: list = []
        # islice rather than deque[i]: indexing a deque walks to the index, so
        # doing it per row would make copying a long selection quadratic.
        if start < top_len:
            rows.extend(islice(top, start, min(end, top_len - 1) + 1))
        screen_end = top_len + lines - 1
        if end >= top_len and start <= screen_end:
            first = max(start, top_len) - top_len
            last = min(end, screen_end) - top_len
            buffer = self.screen.buffer
            rows.extend(buffer[y] for y in range(first, last + 1))
        if end > screen_end:
            first = max(start, screen_end + 1) - screen_end - 1
            rows.extend(islice(bottom, first, end - screen_end))
        return rows

    def document_line_text(self, start: int, end: int) -> list[str]:
        """Plain text of absolute rows [start, end], trailing blanks kept."""
        cols = self.screen.columns
        out = []
        for row in self.document_rows(start, end):
            blank = row.default
            get = row.get
            out.append("".join([get(x, blank).data for x in range(cols)]))
        return out

    # -- scrollback --------------------------------------------------------

    def page_up(self) -> bool:
        try:
            before = len(self.screen.history.top)
            self.screen.prev_page()
            return len(self.screen.history.top) != before
        except Exception:
            return False

    def page_down(self) -> bool:
        try:
            before = len(self.screen.history.bottom)
            self.screen.next_page()
            return len(self.screen.history.bottom) != before
        except Exception:
            return False

    @property
    def scrolled_back(self) -> bool:
        try:
            return len(self.screen.history.bottom) > 0
        except Exception:
            return False

    @property
    def scroll_back(self) -> int:
        """Lines currently scrolled back from live. 0 == at the bottom."""
        try:
            return len(self.screen.history.bottom)
        except Exception:
            return 0

    @property
    def scroll_total(self) -> int:
        """Lines available to scroll through right now.

        top + bottom is stable while scrolled back: pyte snaps to the live
        bottom before processing any new data (see HistoryScreen.before_event),
        so nothing is added to either queue mid-scroll to throw this off.
        """
        try:
            history = self.screen.history
            return len(history.top) + len(history.bottom)
        except Exception:
            return 0

    def scroll_by(self, lines: int) -> bool:
        """Scroll up (positive) or down (negative) by exactly `lines` rows.

        pyte only advertises half-screen paging, but the distance prev_page
        and next_page move is just `ceil(screen.lines * history.ratio)`, so
        borrowing the ratio for the duration of one call makes it travel
        whatever distance we ask for -- a line at a time for the wheel, or
        straight to a target in one hop for a scrollbar drag.
        """
        if not lines:
            return False
        screen = self.screen
        try:
            before = self.scroll_back
            up = lines > 0
            remaining = abs(lines)
            saved = screen.history.ratio
            try:
                while remaining > 0:
                    # Never ask for more than a screenful in one call. pyte
                    # swaps rows between the screen and history assuming the
                    # distance fits on screen, and a larger step makes it
                    # write buffer rows below the last line -- which stay in
                    # the buffer for good, so a long scrollbar drag would
                    # leave hundreds of them behind.
                    step = min(remaining, screen.lines)
                    screen.history = screen.history._replace(
                        ratio=step / screen.lines)
                    at = self.scroll_back
                    if up:
                        screen.prev_page()
                    else:
                        screen.next_page()
                    moved = abs(self.scroll_back - at)
                    if not moved:
                        break  # ran out of history to scroll through
                    remaining -= moved
            finally:
                # Re-read history first: the page calls replaced it to record
                # the new position, which must not be rolled back with it.
                screen.history = screen.history._replace(ratio=saved)
            return self.scroll_back != before
        except Exception:
            return False

    def scroll_to(self, lines_back: int) -> None:
        """Jump straight to a target scroll depth. 0 == the live bottom."""
        target = max(0, min(lines_back, self.scroll_total))
        self.scroll_by(target - self.scroll_back)

    # -- housekeeping ------------------------------------------------------

    def reset(self) -> None:
        """Full reset -- the equivalent of typing `reset` in a wedged shell."""
        cols, rows = self.screen.columns, self.screen.lines
        self.screen.reset()
        self.screen.resize(rows, cols)

    def clear(self) -> None:
        """Clear the visible screen but keep scrollback and cursor position."""
        self.feed(b"\x1b[2J\x1b[H")
