"""Terminal emulation.

This is the layer that turns raw bytes into a screen. Writing it yourself is
the classic trap -- ANSI/VT100 has scroll regions, origin mode, character
sets, 256-colour and truecolour SGR, and a hundred edge cases that only show
up when you run `nano` over a flaky link. pyte already handles all of it, so
this module is just a thin, well-behaved wrapper.
"""

from __future__ import annotations

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

    def scroll_to(self, lines_back: int) -> None:
        """Move as close as possible to a target scroll depth.

        pyte only exposes paging (prev_page/next_page move a fixed chunk of
        lines at a time), not an absolute seek, so a scrollbar drag has to
        walk there one page at a time. Lands within one page of the target.
        """
        target = max(lines_back, 0)
        guard = self.scroll_total + self.screen.lines + 1  # more than enough
        while self.scroll_back < target and guard > 0:
            if not self.page_up():
                break
            guard -= 1
        while self.scroll_back > target and guard > 0:
            if not self.page_down():
                break
            guard -= 1

    # -- housekeeping ------------------------------------------------------

    def reset(self) -> None:
        """Full reset -- the equivalent of typing `reset` in a wedged shell."""
        cols, rows = self.screen.columns, self.screen.lines
        self.screen.reset()
        self.screen.resize(rows, cols)

    def clear(self) -> None:
        """Clear the visible screen but keep scrollback and cursor position."""
        self.feed(b"\x1b[2J\x1b[H")
