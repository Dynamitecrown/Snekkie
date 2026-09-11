"""Main window: tab bar, menus, status bar."""

from __future__ import annotations

from functools import partial

from PySide6.QtCore import Qt
from PySide6.QtGui import QAction, QKeySequence
from PySide6.QtWidgets import (
    QApplication,
    QMainWindow,
    QMessageBox,
    QSplitter,
    QTabWidget,
)

from .. import transport as transport_mod
from ..profiles import Profile, ProfileStore
from ..settings import SettingsStore
from ..transport import TransportError
from .dialogs import confirm_host_key, prompt_secret
from .preferences import PreferencesDialog
from .session import SessionTab
from .sidebar import SessionSidebar
from .style import build_stylesheet


class MainWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Snekkie")
        self.resize(1000, 640)

        self.store = ProfileStore()
        self.settings_store = SettingsStore()
        self.settings = self.settings_store.load()

        QApplication.instance().setStyleSheet(
            build_stylesheet(self.settings.colors()["cursor"]))

        self.sidebar = SessionSidebar(self.store, defaults=self.settings)
        self.sidebar.setObjectName("sidebar")
        self.sidebar.connect_requested.connect(self.open_profile)

        self.tabs = QTabWidget()
        self.tabs.setTabsClosable(True)
        self.tabs.setMovable(True)
        self.tabs.setDocumentMode(True)
        self.tabs.tabCloseRequested.connect(self.close_tab)
        self.tabs.currentChanged.connect(self._refresh_status)

        self.splitter = QSplitter(Qt.Horizontal)
        self.splitter.addWidget(self.sidebar)
        self.splitter.addWidget(self.tabs)
        self.splitter.setStretchFactor(0, 0)
        self.splitter.setStretchFactor(1, 1)
        self.splitter.setSizes([300, 700])
        self.setCentralWidget(self.splitter)
        self.sidebar.setVisible(self.settings.show_sidebar)

        self.statusBar().showMessage("No session")
        self._build_menus()

    # -- menus -------------------------------------------------------------

    def _build_menus(self) -> None:
        session = self.menuBar().addMenu("&Session")
        self._add(session, "&New session…", "Ctrl+Shift+N", self.new_session)

        self.saved_menu = session.addMenu("&Open saved")
        self.saved_menu.aboutToShow.connect(self._populate_saved)

        self._add(session, "&Duplicate", "Ctrl+Shift+D", self.duplicate_session)
        self._add(session, "&Reconnect", "Ctrl+Shift+R", self.reconnect_session)
        session.addSeparator()
        self._add(session, "&Close tab", "Ctrl+W",
                  lambda: self.close_tab(self.tabs.currentIndex()))
        self._add(session, "E&xit", "Ctrl+Q", self.close)

        edit = self.menuBar().addMenu("&Edit")
        self._add(edit, "&Copy", "Ctrl+Shift+C",
                  lambda: self._on_current("copy_selection"))
        self._add(edit, "&Paste", "Ctrl+Shift+V",
                  lambda: self._on_current("paste"))
        self._add(edit, "Select &all", "Ctrl+Shift+A",
                  lambda: self._on_current("select_all"))

        terminal = self.menuBar().addMenu("&Terminal")
        self._add(terminal, "C&lear screen", "Ctrl+Shift+L",
                  lambda: self._on_current("clear"))
        self._add(terminal, "&Reset terminal", None,
                  lambda: self._on_current("reset"))
        terminal.addSeparator()
        self._add(terminal, "Send &break", "Ctrl+Shift+B", self.send_break)

        view = self.menuBar().addMenu("&View")
        self.toggle_sidebar_action = self._add(
            view, "Toggle &sidebar", "Ctrl+B", self.toggle_sidebar)
        self.toggle_sidebar_action.setCheckable(True)
        self.toggle_sidebar_action.setChecked(self.settings.show_sidebar)

        settings_menu = self.menuBar().addMenu("&Settings")
        self._add(settings_menu, "&Preferences…", "Ctrl+,", self.open_preferences)

        help_menu = self.menuBar().addMenu("&Help")
        self._add(help_menu, "&Keyboard shortcuts", None, self.show_help)

    def _add(self, menu, text, shortcut, slot) -> QAction:
        action = QAction(text, self)
        if shortcut:
            action.setShortcut(QKeySequence(shortcut))
        action.triggered.connect(slot)
        menu.addAction(action)
        return action

    def _populate_saved(self) -> None:
        self.saved_menu.clear()
        if not self.store.profiles:
            action = self.saved_menu.addAction("(none saved yet)")
            action.setEnabled(False)
            return
        for profile in self.store.profiles:
            action = self.saved_menu.addAction(f"{profile.name}  —  {profile.kind}")
            action.triggered.connect(partial(self.open_profile, profile))

    # -- session management ------------------------------------------------

    def new_session(self) -> None:
        if not self.sidebar.isVisible():
            self.toggle_sidebar()
        self.sidebar.reset_form()

    def duplicate_session(self) -> None:
        tab = self.current_tab()
        if tab is not None:
            self.open_profile(tab.profile.copy())

    def open_profile(self, profile: Profile) -> None:
        kwargs = self._credentials_for(profile)
        if kwargs is None:
            return
        try:
            transport = transport_mod.create(profile, **kwargs)
        except TransportError as exc:
            QMessageBox.warning(self, "Session", str(exc))
            return

        # Connecting on the GUI thread keeps the host-key and password prompts
        # simple. It blocks the UI for up to CONNECT_TIMEOUT seconds -- moving
        # this to a worker thread is the first thing to improve.
        QApplication.setOverrideCursor(Qt.WaitCursor)
        try:
            transport.connect()
        except TransportError as exc:
            QMessageBox.warning(self, "Connection failed", str(exc))
            return
        finally:
            QApplication.restoreOverrideCursor()

        tab = SessionTab(profile, transport, theme=self.settings.colors(),
                         parent=self)
        index = self.tabs.addTab(tab, profile.name)
        self.tabs.setCurrentIndex(index)
        tab.title_changed.connect(partial(self._set_title, tab))
        tab.status_changed.connect(self._refresh_status)
        tab.terminal.size_changed.connect(lambda *_: self._refresh_status())
        tab.terminal.setFocus()
        self._refresh_status()

    def _credentials_for(self, profile: Profile) -> dict | None:
        """Collect anything we deliberately don't persist. None = cancelled."""
        if profile.kind != "ssh":
            return {}
        kwargs: dict = {
            "ask_host_key": lambda host, key: confirm_host_key(self, host, key)
        }
        if profile.auth == "password":
            password, ok = prompt_secret(
                self, "SSH password",
                f"Password for {profile.username or '(user)'}@{profile.host}:")
            if not ok:
                return None
            kwargs["password"] = password
        elif profile.auth == "key" and profile.key_file:
            passphrase, ok = prompt_secret(
                self, "Key passphrase",
                "Passphrase (leave blank if the key is not encrypted):")
            if not ok:
                return None
            kwargs["key_passphrase"] = passphrase
        return kwargs

    def reconnect_session(self) -> None:
        tab = self.current_tab()
        if tab is None:
            return
        kwargs = self._credentials_for(tab.profile)
        if kwargs is None:
            return
        QApplication.setOverrideCursor(Qt.WaitCursor)
        try:
            tab.reconnect(**kwargs)
        finally:
            QApplication.restoreOverrideCursor()
        self._refresh_status()

    def close_tab(self, index: int) -> None:
        tab = self.tabs.widget(index)
        if tab is None:
            return
        if tab.is_connected:
            if QMessageBox.question(
                self, "Close session",
                f"“{tab.profile.name}” is still connected. Close it?"
            ) != QMessageBox.Yes:
                return
        tab.shutdown()
        self.tabs.removeTab(index)
        tab.deleteLater()
        self._refresh_status()

    def send_break(self) -> None:
        tab = self.current_tab()
        if tab is not None:
            tab.send_break()

    # -- view / settings -----------------------------------------------------

    def toggle_sidebar(self) -> None:
        visible = not self.sidebar.isVisible()
        self.sidebar.setVisible(visible)
        self.toggle_sidebar_action.setChecked(visible)
        self.settings.show_sidebar = visible
        self.settings_store.save(self.settings)

    def open_preferences(self) -> None:
        dialog = PreferencesDialog(self.settings, parent=self)
        if dialog.exec() and dialog.result_settings:
            self.settings = dialog.result_settings
            self.settings_store.save(self.settings)
            self.sidebar.set_defaults(self.settings)
            theme = self.settings.colors()
            for i in range(self.tabs.count()):
                self.tabs.widget(i).apply_theme(theme)
            QApplication.instance().setStyleSheet(build_stylesheet(theme["cursor"]))

    # -- helpers -----------------------------------------------------------

    def current_tab(self) -> SessionTab | None:
        return self.tabs.currentWidget()

    def _on_current(self, method: str) -> None:
        tab = self.current_tab()
        if tab is not None:
            getattr(tab.terminal, method)()

    def _set_title(self, tab: SessionTab, title: str) -> None:
        index = self.tabs.indexOf(tab)
        if index >= 0:
            self.tabs.setTabText(index, title)

    def _refresh_status(self) -> None:
        tab = self.current_tab()
        self.statusBar().showMessage(tab.status_text if tab else "No session")

    def show_help(self) -> None:
        QMessageBox.information(self, "Keyboard shortcuts", (
            "Ctrl+Shift+N   New session\n"
            "Ctrl+Shift+D   Duplicate current session\n"
            "Ctrl+Shift+R   Reconnect\n"
            "Ctrl+W         Close tab\n\n"
            "Ctrl+Shift+C   Copy      (selecting also copies)\n"
            "Ctrl+Shift+V   Paste     (right-click also pastes)\n"
            "Shift+PgUp/Dn  Scroll back through history\n"
            "Ctrl+Shift+L   Clear screen\n"
            "Ctrl+Shift+B   Send break (serial only)\n\n"
            "Ctrl+B         Toggle sidebar\n"
            "Ctrl+,         Preferences"
        ))

    def closeEvent(self, event) -> None:
        live = [i for i in range(self.tabs.count())
                if self.tabs.widget(i).is_connected]
        if live and QMessageBox.question(
            self, "Quit", f"{len(live)} session(s) still connected. Quit anyway?"
        ) != QMessageBox.Yes:
            event.ignore()
            return
        for i in range(self.tabs.count()):
            self.tabs.widget(i).shutdown()
        event.accept()
