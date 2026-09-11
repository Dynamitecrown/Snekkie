"""Preferences dialog: colour theme and default font for new sessions."""

from __future__ import annotations

from PySide6.QtGui import QColor, QFontDatabase
from PySide6.QtWidgets import (
    QColorDialog,
    QComboBox,
    QDialog,
    QDialogButtonBox,
    QFontComboBox,
    QFormLayout,
    QGridLayout,
    QGroupBox,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QMessageBox,
    QPushButton,
    QSpinBox,
    QVBoxLayout,
)

from ..settings import DEFAULT_THEME, THEME_KEYS, THEMES, AppSettings


def _swatch_style(hex_color: str) -> str:
    return f"background-color: {hex_color}; border: 1px solid #666; min-height: 20px;"


class PreferencesDialog(QDialog):
    """Returns the edited settings via .result_settings once accepted."""

    def __init__(self, settings: AppSettings, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Preferences")
        self.resize(420, 440)
        self.result_settings: AppSettings | None = None
        self._show_sidebar = settings.show_sidebar
        self._colors = dict(settings.custom_colors())
        self._saved_themes = {name: dict(scheme)
                              for name, scheme in settings.saved_themes.items()}

        # -- theme -----------------------------------------------------------
        self.theme = QComboBox()
        self._reload_themes(settings.theme)
        self.theme.currentIndexChanged.connect(self._sync)

        self.save_theme_btn = QPushButton("Save as…")
        self.save_theme_btn.clicked.connect(self._save_theme)
        self.delete_theme_btn = QPushButton("Delete")
        self.delete_theme_btn.clicked.connect(self._delete_theme)

        theme_row = QHBoxLayout()
        theme_row.addWidget(self.theme, 1)
        theme_row.addWidget(self.save_theme_btn)
        theme_row.addWidget(self.delete_theme_btn)

        self.preview = QLabel("user@host:~$ ls -la")
        self.preview.setFont(QFontDatabase.systemFont(QFontDatabase.FixedFont))

        self._swatches: dict[str, QPushButton] = {}
        custom_grid = QGridLayout()
        self.custom_box = QGroupBox("Custom colours")
        for row, (key, label) in enumerate((
            ("fg", "Text"), ("bg", "Background"),
            ("cursor", "Cursor"), ("selection", "Selection"),
        )):
            btn = QPushButton()
            btn.setFixedWidth(60)
            btn.clicked.connect(lambda _checked, k=key: self._pick_color(k))
            self._swatches[key] = btn
            custom_grid.addWidget(QLabel(label), row, 0)
            custom_grid.addWidget(btn, row, 1)
        self.custom_box.setLayout(custom_grid)

        # -- font --------------------------------------------------------------
        self.font_family = QFontComboBox()
        self.font_family.setFontFilters(QFontComboBox.MonospacedFonts)
        if settings.font_family:
            self.font_family.setCurrentFont(settings.font_family)

        self.font_size = QSpinBox()
        self.font_size.setRange(6, 48)
        self.font_size.setValue(settings.font_size)

        self.scrollback = QSpinBox()
        self.scrollback.setRange(0, 200000)
        self.scrollback.setSingleStep(1000)
        self.scrollback.setValue(settings.scrollback)

        form = QFormLayout()
        form.addRow("Colour theme", theme_row)
        form.addRow("Preview", self.preview)
        form.addRow(self.custom_box)
        form.addRow("Font (new sessions)", self.font_family)
        form.addRow("Font size", self.font_size)
        form.addRow("Default scrollback", self.scrollback)

        buttons = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        buttons.accepted.connect(self._accept)
        buttons.rejected.connect(self.reject)

        outer = QVBoxLayout(self)
        outer.addLayout(form)
        outer.addWidget(buttons)

        self._sync()

    # -- theme list ----------------------------------------------------------

    def _reload_themes(self, select: str) -> None:
        """Rebuild the combo, keeping `select` chosen if it still exists."""
        blocked = self.theme.blockSignals(True)
        try:
            self.theme.clear()
            for name in THEMES:
                self.theme.addItem(name, name)
            for name in sorted(self._saved_themes):
                self.theme.addItem(name, name)
            self.theme.addItem("Custom", "Custom")
            index = self.theme.findData(select)
            self.theme.setCurrentIndex(max(index, 0))
        finally:
            self.theme.blockSignals(blocked)

    def _save_theme(self) -> None:
        """Store the colours on screen under a name of the user's choosing."""
        current = self.theme.currentData()
        suggested = "" if current in THEMES or current == "Custom" else current
        name, ok = QInputDialog.getText(self, "Save theme", "Theme name:",
                                        text=suggested)
        name = name.strip()
        if not ok or not name:
            return
        if name in THEMES:
            QMessageBox.warning(
                self, "Save theme",
                f"“{name}” is a built-in theme. Pick another name.")
            return
        if name in self._saved_themes and QMessageBox.question(
            self, "Save theme", f"Replace the saved theme “{name}”?"
        ) != QMessageBox.Yes:
            return
        self._saved_themes[name] = dict(self._current_colors())
        self._reload_themes(name)
        self._sync()

    def _delete_theme(self) -> None:
        name = self.theme.currentData()
        if name not in self._saved_themes:
            QMessageBox.information(
                self, "Delete theme",
                "Only themes you saved yourself can be deleted.")
            return
        if QMessageBox.question(self, "Delete theme",
                                f"Delete “{name}”?") != QMessageBox.Yes:
            return
        del self._saved_themes[name]
        self._reload_themes(DEFAULT_THEME)
        self._sync()

    # -- preview -------------------------------------------------------------

    def _current_colors(self) -> dict[str, str]:
        name = self.theme.currentData()
        if name == "Custom":
            return self._colors
        if name in self._saved_themes:
            return {k: self._saved_themes[name].get(k, THEMES[DEFAULT_THEME][k])
                    for k in THEME_KEYS}
        return THEMES.get(name, THEMES[DEFAULT_THEME])

    def _sync(self) -> None:
        name = self.theme.currentData()
        self.custom_box.setEnabled(name == "Custom")
        self.delete_theme_btn.setEnabled(name in self._saved_themes)
        colors = self._current_colors()
        for key, btn in self._swatches.items():
            btn.setStyleSheet(_swatch_style(colors[key]))
        self.preview.setStyleSheet(
            f"padding: 8px; border: 1px solid #444; "
            f"color: {colors['fg']}; background-color: {colors['bg']};"
        )

    def _pick_color(self, key: str) -> None:
        color = QColorDialog.getColor(QColor(self._colors[key]), self,
                                      "Choose colour")
        if color.isValid():
            self._colors[key] = color.name()
            self._sync()

    def _accept(self) -> None:
        self.result_settings = AppSettings(
            theme=self.theme.currentData(),
            custom_fg=self._colors["fg"],
            custom_bg=self._colors["bg"],
            custom_cursor=self._colors["cursor"],
            custom_selection=self._colors["selection"],
            font_family=self.font_family.currentFont().family()
                if self.font_family.currentText().strip() else "",
            font_size=self.font_size.value(),
            scrollback=self.scrollback.value(),
            show_sidebar=self._show_sidebar,
            saved_themes=self._saved_themes,
        )
        self.accept()
