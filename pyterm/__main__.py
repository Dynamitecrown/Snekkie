"""Entry point: python -m pyterm"""

from __future__ import annotations

import gc
import sys

from PySide6.QtWidgets import QApplication

from .ui.window import MainWindow


def tune_gc() -> None:
    """Stop full collections from stalling the GUI as scrollback fills.

    Scrollback is a lot of small long-lived objects: a screenful of history
    is one dict per row plus a Char per written cell, so 5000 lines is on the
    order of 370k of them. A generation-2 pass walks all of that, and since
    it runs on the GUI thread the whole app -- tab bar and menus included --
    freezes for as long as it takes. Measured on a filled 1920x1080 window
    that is ~90ms, and it gets worse the longer a session runs.

    Two changes, both keeping cycle collection working:

    freeze() moves everything alive at startup -- Qt's widget tree, the
    imported modules -- into a permanent generation that is never scanned
    again. Those objects live for the life of the process anyway.

    Raising the gen-2 threshold makes full passes rare rather than cheap;
    the scrollback they would scan is not cyclic, so refcounting reclaims it
    either way. Cycles are still collected, just later, which trades a little
    memory for not dropping frames.

    Together these cut the worst stall about fourfold, matching what turning
    the collector off entirely achieves without leaking cycles to get it.
    """
    gen0, gen1, _ = gc.get_threshold()
    gc.set_threshold(gen0, gen1, 500)
    gc.freeze()


def main() -> int:
    app = QApplication(sys.argv)
    app.setApplicationName("PyTerm")
    app.setOrganizationName("PyTerm")

    window = MainWindow()
    window.show()
    tune_gc()  # after the widget tree exists, so it all gets frozen
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())
