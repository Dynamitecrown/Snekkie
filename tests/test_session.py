"""Widget and end-to-end session tests.

The serial tests drive a real pty pair, so they exercise the actual pyserial
code path rather than a mock. They skip on Windows, which has no pty.
"""

import os
import sys
import time

import pytest
from PySide6.QtCore import QEvent, Qt
from PySide6.QtGui import QGuiApplication, QKeyEvent

from snekkie.profiles import Profile
from snekkie.ui.terminal import TerminalWidget

needs_pty = pytest.mark.skipif(
    sys.platform == "win32", reason="no pty on Windows"
)


@pytest.fixture
def widget(qapp):
    w = TerminalWidget(scrollback=500)
    w.resize(800, 480)
    w.show()
    qapp.processEvents()
    yield w
    w.close()


def test_widget_computes_a_sane_grid(widget):
    assert widget.terminal.columns > 40
    assert widget.terminal.lines > 10


def test_paint_does_not_raise(widget, qapp):
    """Smoke test for the renderer -- catches attribute typos in paintEvent."""
    widget.feed(
        b"\x1b[2J\x1b[HSwitch#\x1b[1;33m show run\x1b[0m\r\n"
        b"\x1b[4munderlined\x1b[0m \x1b[7mreverse\x1b[0m \x1b[38;5;208m256col\x1b[0m\r\n"
    )
    qapp.processEvents()
    widget.repaint()
    assert "Switch#" in widget.terminal.line_text(0)


def test_selection_and_copy(widget):
    widget.feed(b"Switch#show run")
    widget._sel_anchor, widget._sel_head = (0, 0), (0, 6)
    assert widget.selected_text() == "Switch#"
    widget.copy_selection()
    assert QGuiApplication.clipboard().text() == "Switch#"


def test_paste_normalises_line_endings(widget):
    sent = []
    widget.data_typed.connect(sent.append)
    QGuiApplication.clipboard().setText("conf t\r\nint gi0/1\nend\n")
    widget.paste()
    # Terminals want CR, never CRLF -- otherwise every line double-spaces.
    assert sent[-1] == b"conf t\rint gi0/1\rend\r"


def test_typing_emits_bytes(widget):
    sent = []
    widget.data_typed.connect(sent.append)
    widget.keyPressEvent(
        QKeyEvent(QEvent.KeyPress, Qt.Key_C, Qt.ControlModifier, "")
    )
    assert sent[-1] == b"\x03"


def test_ctrl_shift_c_copies_rather_than_interrupting(widget):
    """Ctrl+Shift+C must not send an interrupt to the far end."""
    sent = []
    widget.data_typed.connect(sent.append)
    widget.feed(b"hello")
    widget._sel_anchor, widget._sel_head = (0, 0), (0, 4)
    widget.keyPressEvent(QKeyEvent(
        QEvent.KeyPress, Qt.Key_C,
        Qt.ControlModifier | Qt.ShiftModifier, ""
    ))
    assert sent == []
    assert QGuiApplication.clipboard().text() == "hello"


# -- end-to-end over a real pty -------------------------------------------


@needs_pty
def test_session_round_trip(qapp, tmp_path):
    from snekkie.transport.serialport import SerialTransport
    from snekkie.ui.session import SessionTab

    master, slave = os.openpty()
    device = os.ttyname(slave)
    os.close(slave)

    log = tmp_path / "session.log"
    profile = Profile(name="lab", kind="serial", device=device,
                      baud=9600, log_path=str(log))
    transport = SerialTransport(profile)
    transport.connect()

    tab = SessionTab(profile, transport)
    tab.resize(800, 400)
    tab.show()
    qapp.processEvents()

    try:
        os.write(master, b"\x1b[2J\x1b[HSwitch>en\r\n"
                         b"\x1b[1;31m% Bad secrets\x1b[0m\r\n")
        deadline = time.time() + 5
        while time.time() < deadline:
            qapp.processEvents()
            if "Bad secrets" in tab.terminal.terminal.text():
                break
            time.sleep(0.02)

        assert tab.terminal.terminal.line_text(0) == "Switch>en"
        assert tab.terminal.terminal.buffer[1][0].fg == "red"
        assert tab.is_connected

        tab.terminal.data_typed.emit(b"show ip int brief\r")
        time.sleep(0.2)
        assert os.read(master, 200) == b"show ip int brief\r"
    finally:
        tab.shutdown()
        qapp.processEvents()
        os.close(master)

    assert not tab.is_connected
    assert b"Switch>en" in log.read_bytes()


@needs_pty
def test_reader_thread_stops_cleanly(qapp):
    """Shutdown must not hang or leave a thread running."""
    from snekkie.transport.serialport import SerialTransport
    from snekkie.ui.session import SessionTab

    master, slave = os.openpty()
    device = os.ttyname(slave)
    os.close(slave)

    profile = Profile(name="lab", kind="serial", device=device)
    transport = SerialTransport(profile)
    transport.connect()
    tab = SessionTab(profile, transport)

    start = time.time()
    tab.shutdown()
    assert time.time() - start < 3, "shutdown blocked for too long"
    os.close(master)
