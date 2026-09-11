"""Global QSS for the app chrome: sidebar, menus, tabs, buttons, dialogs.

Separate from the terminal colour theme in settings.py, which only paints
the terminal canvas. This is the frame around it -- always dark regardless
of the Windows theme, same as most terminal emulators keep their own chrome
rather than following the OS. The accent colour is threaded through from
whichever terminal theme is active, so switching themes in Preferences
re-skins the whole app, not just the terminal.
"""

from __future__ import annotations

from PySide6.QtGui import QColor

BG_WINDOW = "#1b1c1b"
BG_PANEL = "#202120"
BG_INPUT = "#2a2b2a"
BG_LIST = "#1e1f1e"
BORDER = "#343432"
TEXT_PRIMARY = "#e8e8e6"
TEXT_SECONDARY = "#9a9a96"


def _lighter(hex_color: str, factor: int = 130) -> str:
    return QColor(hex_color).lighter(factor).name()


def _text_on(hex_color: str) -> str:
    """Black or near-white, whichever contrasts with the given colour."""
    color = QColor(hex_color)
    luminance = 0.299 * color.red() + 0.587 * color.green() + 0.114 * color.blue()
    return "#101110" if luminance > 140 else "#f5f5f3"


def build_stylesheet(accent: str) -> str:
    accent_hover = _lighter(accent, 115)
    accent_text = _text_on(accent)
    return f"""
    QMainWindow, QDialog, QWidget {{
        background-color: {BG_WINDOW};
        color: {TEXT_PRIMARY};
        font-size: 13px;
    }}
    QMenuBar {{ background-color: {BG_WINDOW}; border-bottom: 1px solid {BORDER}; }}
    QMenuBar::item {{ padding: 4px 10px; background: transparent; }}
    QMenuBar::item:selected {{ background-color: {BG_INPUT}; }}
    QMenu {{ background-color: {BG_PANEL}; border: 1px solid {BORDER}; }}
    QMenu::item {{ padding: 5px 20px; }}
    QMenu::item:selected {{ background-color: {accent}; color: {accent_text}; }}

    QWidget#sidebar {{
        background-color: {BG_PANEL};
        border-right: 1px solid {BORDER};
    }}
    QLabel#sectionHeading {{
        color: {TEXT_SECONDARY};
        font-weight: 500;
        font-size: 12px;
        padding-bottom: 4px;
        border-bottom: 1px solid {BORDER};
        margin-bottom: 6px;
    }}

    QLineEdit, QComboBox, QSpinBox {{
        background-color: {BG_INPUT};
        border: 1px solid {BORDER};
        border-radius: 4px;
        padding: 4px 6px;
        selection-background-color: {accent};
        selection-color: {accent_text};
    }}
    QLineEdit:focus, QComboBox:focus, QSpinBox:focus {{
        border: 1px solid {accent};
    }}
    QComboBox::drop-down {{ border: none; width: 20px; }}

    QPushButton {{
        background-color: {BG_INPUT};
        border: 1px solid {BORDER};
        border-radius: 4px;
        padding: 5px 12px;
    }}
    QPushButton:hover {{ background-color: {_lighter(BG_INPUT, 130)}; }}
    QPushButton:pressed {{ background-color: {BG_WINDOW}; }}
    QPushButton:disabled {{ color: {TEXT_SECONDARY}; }}

    QPushButton#connectButton {{
        background-color: {accent};
        color: {accent_text};
        font-weight: 500;
        border: none;
        padding: 7px 12px;
    }}
    QPushButton#connectButton:hover {{ background-color: {accent_hover}; }}

    QTabWidget::pane {{
        border: 1px solid {BORDER};
        border-radius: 4px;
        top: -1px;
    }}
    QTabBar::tab {{
        background: transparent;
        color: {TEXT_SECONDARY};
        padding: 6px 14px;
        border-bottom: 2px solid transparent;
    }}
    QTabBar::tab:selected {{
        color: {TEXT_PRIMARY};
        border-bottom: 2px solid {accent};
    }}
    QTabBar::tab:hover {{ color: {TEXT_PRIMARY}; }}

    QListWidget {{
        background-color: {BG_LIST};
        border: 1px solid {BORDER};
        border-radius: 4px;
        outline: none;
    }}
    QListWidget::item {{ padding: 5px 6px; }}
    QListWidget::item:selected {{ background-color: {accent}; color: {accent_text}; }}
    QListWidget::item:hover:!selected {{ background-color: {BG_INPUT}; }}

    QStatusBar {{
        background-color: {BG_WINDOW};
        border-top: 1px solid {BORDER};
        color: {TEXT_SECONDARY};
    }}
    QSplitter::handle {{ background-color: {BORDER}; width: 2px; }}
    QScrollBar:vertical {{ background: {BG_WINDOW}; width: 12px; }}
    QScrollBar::handle:vertical {{
        background: {BORDER};
        border-radius: 5px;
        min-height: 24px;
    }}
    QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {{ height: 0; }}
    """
