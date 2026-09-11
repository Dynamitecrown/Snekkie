"""Connection setting forms (SSH/Serial/Advanced), embedded in the sidebar."""

from __future__ import annotations

from PySide6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFileDialog,
    QFormLayout,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPushButton,
    QSpinBox,
    QWidget,
)

from ..profiles import Profile
from ..transport.serialport import BAUD_RATES, PARITY, list_ports


class SSHPage(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.host = QLineEdit()
        self.host.setPlaceholderText("hostname or IP")
        self.port = QSpinBox()
        self.port.setRange(1, 65535)
        self.port.setValue(22)
        self.username = QLineEdit()
        self.auth = QComboBox()
        self.auth.addItem("Password", "password")
        self.auth.addItem("Private key file", "key")
        self.auth.addItem("SSH agent / default keys", "agent")

        self.key_file = QLineEdit()
        self.key_file.setPlaceholderText("~/.ssh/id_ed25519")
        browse = QPushButton("Browse…")
        browse.clicked.connect(self._browse)
        key_row = QHBoxLayout()
        key_row.setContentsMargins(0, 0, 0, 0)
        key_row.addWidget(self.key_file, 1)
        key_row.addWidget(browse)
        key_widget = QWidget()
        key_widget.setLayout(key_row)

        form = QFormLayout(self)
        form.addRow("Host", self.host)
        form.addRow("Port", self.port)
        form.addRow("Username", self.username)
        form.addRow("Authentication", self.auth)
        form.addRow("Key file", key_widget)

        self.auth.currentIndexChanged.connect(self._sync)
        self._key_widget = key_widget
        self._sync()

    def _sync(self):
        self._key_widget.setEnabled(self.auth.currentData() == "key")

    def _browse(self):
        path, _ = QFileDialog.getOpenFileName(self, "Select private key")
        if path:
            self.key_file.setText(path)

    def load(self, p: Profile):
        self.host.setText(p.host)
        self.port.setValue(p.port or 22)
        self.username.setText(p.username)
        index = self.auth.findData(p.auth)
        self.auth.setCurrentIndex(max(index, 0))
        self.key_file.setText(p.key_file)

    def apply(self, p: Profile):
        p.kind = "ssh"
        p.host = self.host.text().strip()
        p.port = self.port.value()
        p.username = self.username.text().strip()
        p.auth = self.auth.currentData()
        p.key_file = self.key_file.text().strip()

    def validate(self) -> str:
        if not self.host.text().strip():
            return "Enter a host to connect to."
        return ""


class SerialPage(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.device = QComboBox()
        self.device.setEditable(True)
        refresh = QPushButton("Refresh")
        refresh.clicked.connect(self.refresh_ports)
        row = QHBoxLayout()
        row.setContentsMargins(0, 0, 0, 0)
        row.addWidget(self.device, 1)
        row.addWidget(refresh)
        device_widget = QWidget()
        device_widget.setLayout(row)

        self.baud = QComboBox()
        self.baud.setEditable(True)
        for rate in BAUD_RATES:
            self.baud.addItem(str(rate))
        self.baud.setCurrentText("9600")

        self.bytesize = QComboBox()
        for n in (5, 6, 7, 8):
            self.bytesize.addItem(str(n), n)
        self.bytesize.setCurrentText("8")

        self.parity = QComboBox()
        for name in PARITY:
            self.parity.addItem(name, name)

        self.stopbits = QComboBox()
        for value, text in ((1, "1"), (1.5, "1.5"), (2, "2")):
            self.stopbits.addItem(text, value)

        self.rtscts = QCheckBox("RTS/CTS hardware flow control")
        self.xonxoff = QCheckBox("XON/XOFF software flow control")

        form = QFormLayout(self)
        form.addRow("Port", device_widget)
        form.addRow("Speed", self.baud)
        form.addRow("Data bits", self.bytesize)
        form.addRow("Parity", self.parity)
        form.addRow("Stop bits", self.stopbits)
        form.addRow("", self.rtscts)
        form.addRow("", self.xonxoff)

        hint = QLabel("Cisco console default is 9600-8-N-1, no flow control.")
        hint.setStyleSheet("color: gray;")
        form.addRow("", hint)

        self.refresh_ports()

    def refresh_ports(self):
        current = self.device.currentText()
        self.device.clear()
        for device, description in list_ports():
            label = device if description == device else f"{device} — {description}"
            self.device.addItem(label, device)
        if current:
            self.device.setCurrentText(current)

    def _selected_device(self) -> str:
        data = self.device.currentData()
        if data:
            return data
        return self.device.currentText().split(" — ")[0].strip()

    def load(self, p: Profile):
        if p.device:
            index = self.device.findData(p.device)
            if index >= 0:
                self.device.setCurrentIndex(index)
            else:
                self.device.setCurrentText(p.device)
        self.baud.setCurrentText(str(p.baud))
        self.bytesize.setCurrentText(str(p.bytesize))
        self.parity.setCurrentText(p.parity)
        index = self.stopbits.findData(p.stopbits)
        self.stopbits.setCurrentIndex(max(index, 0))
        self.rtscts.setChecked(p.rtscts)
        self.xonxoff.setChecked(p.xonxoff)

    def apply(self, p: Profile):
        p.kind = "serial"
        p.device = self._selected_device()
        try:
            p.baud = int(self.baud.currentText())
        except ValueError:
            p.baud = 9600
        p.bytesize = self.bytesize.currentData()
        p.parity = self.parity.currentData()
        p.stopbits = self.stopbits.currentData()
        p.rtscts = self.rtscts.isChecked()
        p.xonxoff = self.xonxoff.isChecked()

    def validate(self) -> str:
        if not self._selected_device():
            return "Select a serial port."
        return ""


class TerminalPage(QWidget):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.scrollback = QSpinBox()
        self.scrollback.setRange(0, 200000)
        self.scrollback.setSingleStep(1000)
        self.scrollback.setValue(5000)

        self.font_family = QLineEdit()
        self.font_family.setPlaceholderText("(system monospace)")
        self.font_size = QSpinBox()
        self.font_size.setRange(6, 48)
        self.font_size.setValue(11)

        self.log_path = QLineEdit()
        self.log_path.setPlaceholderText("(no session logging)")
        browse = QPushButton("Browse…")
        browse.clicked.connect(self._browse)
        row = QHBoxLayout()
        row.setContentsMargins(0, 0, 0, 0)
        row.addWidget(self.log_path, 1)
        row.addWidget(browse)
        log_widget = QWidget()
        log_widget.setLayout(row)

        form = QFormLayout(self)
        form.addRow("Scrollback lines", self.scrollback)
        form.addRow("Font family", self.font_family)
        form.addRow("Font size", self.font_size)
        form.addRow("Log session to", log_widget)

    def _browse(self):
        path, _ = QFileDialog.getSaveFileName(self, "Session log file")
        if path:
            self.log_path.setText(path)

    def load(self, p: Profile):
        self.scrollback.setValue(p.scrollback)
        self.font_family.setText(p.font_family)
        self.font_size.setValue(p.font_size)
        self.log_path.setText(p.log_path)

    def apply(self, p: Profile):
        p.scrollback = self.scrollback.value()
        p.font_family = self.font_family.text().strip()
        p.font_size = self.font_size.value()
        p.log_path = self.log_path.text().strip()


def prompt_secret(parent, title: str, label: str) -> tuple[str, bool]:
    text, ok = QInputDialog.getText(parent, title, label, QLineEdit.Password)
    return text, ok


def confirm_host_key(parent, hostname: str, key) -> bool:
    fingerprint = key.get_fingerprint().hex(":")
    return QMessageBox.question(
        parent,
        "Unknown host key",
        f"The server at {hostname} presented a host key that is not in "
        f"known_hosts.\n\n"
        f"Type: {key.get_name()}\n"
        f"Fingerprint: {fingerprint}\n\n"
        f"Only accept this if you can verify the fingerprint out of band. "
        f"Accept and remember it?",
    ) == QMessageBox.Yes
