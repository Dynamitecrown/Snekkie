"""One tab = one session = transport + reader thread + terminal widget."""

from __future__ import annotations

from pathlib import Path

from PySide6.QtCore import Qt, QThread, Signal
from PySide6.QtWidgets import (
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QScrollBar,
    QVBoxLayout,
    QWidget,
)

from .. import transport as transport_mod
from ..profiles import Profile
from ..transport import Transport, TransportError
from .terminal import TerminalWidget


class ReaderThread(QThread):
    """Pulls bytes off the transport and hands them to the GUI thread.

    Qt signals are queued across threads, so `received` lands safely on the
    main thread where pyte and the widget live. pyte is not thread-safe and
    should only ever be touched from there.
    """

    received = Signal(bytes)
    finished_with = Signal(str)  # reason, empty if we closed on purpose

    def __init__(self, transport: Transport, parent=None):
        super().__init__(parent)
        self._transport = transport
        self._stopping = False

    def stop(self) -> None:
        self._stopping = True

    def run(self) -> None:
        reason = ""
        while not self._stopping:
            try:
                chunk = self._transport.read()
            except Exception as exc:  # defensive: never kill the thread silently
                reason = str(exc)
                break
            if chunk is None:
                reason = "" if self._stopping else "Connection closed by remote host"
                break
            if chunk:
                self.received.emit(chunk)
        self.finished_with.emit(reason)


class SessionTab(QWidget):
    """A live (or dead) session. Owns its transport and reader thread."""

    status_changed = Signal()
    title_changed = Signal(str)

    def __init__(self, profile: Profile, transport: Transport,
                 theme: dict[str, str] | None = None, parent=None):
        super().__init__(parent)
        self.profile = profile
        self.transport = transport
        self._reader: ReaderThread | None = None
        self._log = None

        self.terminal = TerminalWidget(
            scrollback=profile.scrollback,
            font_family=profile.font_family,
            font_size=profile.font_size,
            theme=theme,
            syntax=profile.device_syntax,
        )
        self.scrollbar = QScrollBar(Qt.Vertical)
        self.scrollbar.setRange(0, 0)

        self.banner = QLabel()
        self.banner.setVisible(False)
        self.banner.setStyleSheet(
            "background:#552222; color:#eee; padding:4px;"
        )

        body = QHBoxLayout()
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(0)
        body.addWidget(self.terminal, 1)
        body.addWidget(self.scrollbar)

        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)
        layout.addWidget(self.banner)
        layout.addLayout(body, 1)

        self.terminal.scroll_changed.connect(self._sync_scrollbar)
        self.scrollbar.valueChanged.connect(self._on_scrollbar_moved)
        self.terminal.data_typed.connect(self._on_typed)
        self.terminal.size_changed.connect(self._on_resized)

        self._open_log()
        self._start_reader()

    # -- wiring ------------------------------------------------------------

    def _start_reader(self) -> None:
        self._reader = ReaderThread(self.transport, self)
        self._reader.received.connect(self._on_received)
        self._reader.finished_with.connect(self._on_reader_finished)
        self._reader.start()

    def _on_typed(self, data: bytes) -> None:
        if not self.transport.is_connected:
            return
        try:
            self.transport.write(data)
        except TransportError as exc:
            self._disconnected(str(exc))

    def _on_received(self, data: bytes) -> None:
        self.terminal.feed(data)
        if self._log is not None:
            try:
                self._log.write(data)
                self._log.flush()
            except OSError:
                pass

    def _on_resized(self, cols: int, rows: int) -> None:
        if self.transport.is_connected:
            self.transport.resize(cols, rows)

    def _on_reader_finished(self, reason: str) -> None:
        self._disconnected(reason)

    # -- scrollbar -----------------------------------------------------------

    def _sync_scrollbar(self, lines_back: int, total_lines: int) -> None:
        """Update the control without feeding valueChanged back into scrolling."""
        blocked = self.scrollbar.blockSignals(True)
        try:
            self.scrollbar.setRange(0, total_lines)
            self.scrollbar.setPageStep(max(self.terminal.terminal.lines, 1))
            self.scrollbar.setSingleStep(1)  # arrows step a line, not a page
            self.scrollbar.setValue(total_lines - lines_back)
        finally:
            self.scrollbar.blockSignals(blocked)

    def _on_scrollbar_moved(self, value: int) -> None:
        self.terminal.scroll_to(self.scrollbar.maximum() - value)

    # -- state -------------------------------------------------------------

    @property
    def is_connected(self) -> bool:
        return self.transport.is_connected

    @property
    def status_text(self) -> str:
        state = "connected" if self.is_connected else "disconnected"
        return f"{self.transport.description}   [{state}]   " \
               f"{self.terminal.terminal.columns}x{self.terminal.terminal.lines}"

    def _disconnected(self, reason: str) -> None:
        if not self.transport.is_connected and self.banner.isVisible():
            return
        self.transport.close()
        message = reason or "Session closed"
        self.banner.setText(f"  {message}  —  Session ▸ Reconnect to try again")
        self.banner.setVisible(True)
        self.title_changed.emit(f"{self.profile.name} (closed)")
        self.status_changed.emit()

    # -- actions -----------------------------------------------------------

    def reconnect(self, **connect_kwargs) -> bool:
        """Rebuild the transport from the profile and start over."""
        self.shutdown()
        try:
            self.transport = transport_mod.create(self.profile, **connect_kwargs)
            self.transport.connect()
        except TransportError as exc:
            QMessageBox.warning(self, "Reconnect failed", str(exc))
            return False
        self._open_log()
        self.terminal.reset()
        self.banner.setVisible(False)
        self.title_changed.emit(self.profile.name)
        self._start_reader()
        self._on_resized(self.terminal.terminal.columns,
                         self.terminal.terminal.lines)
        self.status_changed.emit()
        return True

    def apply_theme(self, theme: dict[str, str]) -> None:
        self.terminal.set_theme(theme)

    def send_break(self) -> None:
        try:
            self.transport.send_break()
        except TransportError as exc:
            QMessageBox.information(self, "Break", str(exc))

    # -- logging -----------------------------------------------------------

    def _open_log(self) -> None:
        if not self.profile.log_path:
            return
        try:
            path = Path(self.profile.log_path)
            path.parent.mkdir(parents=True, exist_ok=True)
            self._log = open(path, "ab")
        except OSError as exc:
            QMessageBox.warning(self, "Logging",
                                f"Could not open log file: {exc}")
            self._log = None

    # -- teardown ----------------------------------------------------------

    def shutdown(self) -> None:
        if self._reader is not None:
            self._reader.stop()
            self.transport.close()  # unblocks a pending read
            self._reader.wait(2000)
            self._reader = None
        self.transport.close()
        if self._log is not None:
            try:
                self._log.close()
            except OSError:
                pass
            self._log = None
