"""Process-level tuning done at startup.

The collector settings are not cosmetic: a filled scrollback is ~370k small
objects, and a generation-2 pass over that stalls the GUI thread for ~90ms.
"""

import gc

from snekkie.__main__ import ICON_PATH, tune_gc


def test_tune_gc_makes_full_collections_rare_without_disabling_them():
    before = gc.get_threshold()
    try:
        tune_gc()
        gen0, gen1, gen2 = gc.get_threshold()

        assert gc.isenabled(), "cycles must still be collected"
        assert gen2 >= 100, "full passes should be rare, not merely cheaper"
        # The cheap young generations are left alone; they are not the stall.
        assert (gen0, gen1) == before[:2]
    finally:
        gc.set_threshold(*before)
        gc.unfreeze()


def test_tune_gc_freezes_what_is_already_alive():
    before = gc.get_threshold()
    try:
        gc.unfreeze()
        assert gc.get_freeze_count() == 0
        tune_gc()
        assert gc.get_freeze_count() > 0, "startup objects should be frozen"
    finally:
        gc.set_threshold(*before)
        gc.unfreeze()


def test_app_icon_loads(qapp):
    """A missing or unreadable icon silently falls back to a blank one."""
    from PySide6.QtGui import QIcon

    assert ICON_PATH.is_file()
    assert not QIcon(str(ICON_PATH)).isNull()
    assert ICON_PATH.with_suffix(".ico").is_file(), "the exe build needs it"
