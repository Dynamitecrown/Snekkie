"""Left-hand panel: new-session form + saved sessions.

Replaces the old modal ConnectDialog. It's always visible instead of
popping up over the terminal, so starting a second session never means
closing what you're looking at first.
"""

from __future__ import annotations

from PySide6.QtCore import Signal
from PySide6.QtWidgets import (
    QComboBox,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QListWidget,
    QMessageBox,
    QPushButton,
    QSizePolicy,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)

from ..profiles import Profile, ProfileStore
from ..settings import AppSettings
from .dialogs import SerialPage, SSHPage, TerminalPage
from .highlight import SYNTAX_LABELS


class SessionSidebar(QWidget):
    """Emits a Profile via connect_requested when the user hits Connect."""

    connect_requested = Signal(Profile)

    def __init__(self, store: ProfileStore, defaults: AppSettings | None = None,
                 parent=None):
        super().__init__(parent)
        self.store = store
        self._defaults = defaults
        self._current_name = ""

        # -- new session ---------------------------------------------------
        self.kind = QComboBox()
        self.kind.addItem("SSH", "ssh")
        self.kind.addItem("Serial", "serial")
        self.kind.currentIndexChanged.connect(self._sync_kind)

        self.device_syntax = QComboBox()
        for value, label in SYNTAX_LABELS.items():
            self.device_syntax.addItem(label, value)

        self.ssh_page = SSHPage()
        self.serial_page = SerialPage()
        self.term_page = TerminalPage()

        self.tabs = QTabWidget()
        self.tabs.addTab(self.ssh_page, "SSH")
        self.tabs.addTab(self.serial_page, "Serial")
        self.tabs.addTab(self.term_page, "Advanced")
        # QTabWidget sizes its pane to fit the *tallest* page by default, so
        # switching to the shorter SSH tab used to leave a dead gap where
        # Serial's extra rows would have been. Only the visible page keeps
        # a real size hint; the rest are told to take no vertical space.
        self.tabs.currentChanged.connect(self._sync_tab_sizes)
        self._sync_tab_sizes(self.tabs.currentIndex())

        connect_btn = QPushButton("Connect")
        connect_btn.setObjectName("connectButton")
        connect_btn.clicked.connect(self._connect_clicked)

        kind_row = QHBoxLayout()
        kind_row.addWidget(QLabel("Type"))
        kind_row.addWidget(self.kind, 1)

        device_row = QHBoxLayout()
        device_row.addWidget(QLabel("Device"))
        device_row.addWidget(self.device_syntax, 1)

        heading = QLabel("New session")
        heading.setObjectName("sectionHeading")

        form_box = QVBoxLayout()
        form_box.addWidget(heading)
        form_box.addLayout(kind_row)
        form_box.addLayout(device_row)
        form_box.addWidget(self.tabs)
        form_box.addWidget(connect_btn)

        # -- saved sessions --------------------------------------------------
        self.saved = QListWidget()
        self.saved.itemDoubleClicked.connect(self._load_and_connect)

        load_btn = QPushButton("Load")
        save_btn = QPushButton("Save…")
        del_btn = QPushButton("Delete")
        load_btn.clicked.connect(self._load_selected)
        save_btn.clicked.connect(self._save)
        del_btn.clicked.connect(self._delete)

        saved_row = QHBoxLayout()
        for button in (load_btn, save_btn, del_btn):
            saved_row.addWidget(button)

        saved_heading = QLabel("Saved sessions")
        saved_heading.setObjectName("sectionHeading")

        saved_box = QVBoxLayout()
        saved_box.addWidget(saved_heading)
        saved_box.addWidget(self.saved, 1)
        saved_box.addLayout(saved_row)

        outer = QVBoxLayout(self)
        outer.addLayout(form_box)
        outer.addSpacing(10)
        outer.addLayout(saved_box, 1)

        self.setMinimumWidth(260)
        self.setMaximumWidth(420)

        self.refresh_saved()
        self.reset_form()

    # -- defaults ------------------------------------------------------------

    def set_defaults(self, settings: AppSettings) -> None:
        self._defaults = settings

    # -- profile <-> widgets ---------------------------------------------

    def reset_form(self) -> None:
        profile = Profile()
        if self._defaults is not None:
            profile.font_family = self._defaults.font_family
            profile.font_size = self._defaults.font_size
            profile.scrollback = self._defaults.scrollback
        self._apply_profile(profile)
        self.ssh_page.host.setFocus()

    def _apply_profile(self, p: Profile) -> None:
        index = self.kind.findData(p.kind)
        self.kind.setCurrentIndex(max(index, 0))
        syntax_index = self.device_syntax.findData(p.device_syntax)
        self.device_syntax.setCurrentIndex(max(syntax_index, 0))
        self.ssh_page.load(p)
        self.serial_page.load(p)
        self.term_page.load(p)
        self._current_name = p.name
        self._sync_kind()

    def _collect(self) -> Profile:
        p = Profile(name=self._current_name)
        kind = self.kind.currentData()
        # Apply both pages so switching type doesn't lose the other's settings.
        self.ssh_page.apply(p)
        self.serial_page.apply(p)
        self.term_page.apply(p)
        p.kind = kind
        p.device_syntax = self.device_syntax.currentData()
        return p

    def _sync_tab_sizes(self, index: int) -> None:
        for i in range(self.tabs.count()):
            page = self.tabs.widget(i)
            policy = page.sizePolicy()
            policy.setVerticalPolicy(
                QSizePolicy.Preferred if i == index else QSizePolicy.Ignored)
            page.setSizePolicy(policy)
        self.tabs.updateGeometry()

    def _sync_kind(self) -> None:
        kind = self.kind.currentData()
        self.tabs.setCurrentIndex(0 if kind == "ssh" else 1)
        self.tabs.setTabEnabled(0, kind == "ssh")
        self.tabs.setTabEnabled(1, kind == "serial")

    # -- saved list --------------------------------------------------------

    def refresh_saved(self) -> None:
        self.saved.clear()
        self.saved.addItems(self.store.names())

    def _load_selected(self) -> None:
        item = self.saved.currentItem()
        if item is None:
            return
        profile = self.store.get(item.text())
        if profile is not None:
            self._apply_profile(profile)

    def _load_and_connect(self, item) -> None:
        profile = self.store.get(item.text())
        if profile is not None:
            self._apply_profile(profile)
            self._connect_clicked()

    def _save(self) -> None:
        profile = self._collect()
        name, ok = QInputDialog.getText(self, "Save session", "Name:",
                                        text=profile.name)
        if not ok or not name.strip():
            return
        profile.name = name.strip()
        self._current_name = profile.name
        self.store.put(profile)
        self.refresh_saved()

    def _delete(self) -> None:
        item = self.saved.currentItem()
        if item is None:
            return
        if QMessageBox.question(self, "Delete session",
                                f"Delete “{item.text()}”?") != QMessageBox.Yes:
            return
        self.store.remove(item.text())
        self.refresh_saved()

    # -- connect -------------------------------------------------------------

    def _connect_clicked(self) -> None:
        page = self.ssh_page if self.kind.currentData() == "ssh" else self.serial_page
        error = page.validate()
        if error:
            QMessageBox.warning(self, "Incomplete", error)
            return
        profile = self._collect()
        if profile.name in ("", "New session"):
            profile.name = (profile.host or profile.device or "session")
        self.connect_requested.emit(profile)
