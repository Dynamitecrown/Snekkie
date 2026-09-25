"""Serial port picker: detection labels and the Port drop-down."""

import pytest

from snekkie.profiles import Profile
from snekkie.transport import serialport
from snekkie.transport.serialport import PortInfo
from snekkie.ui import dialogs

CABLE_A = PortInfo("COM3", "USB Serial Port (COM3)", serial_number="A10K3X",
                   hwid="USB VID:PID=0403:6001 SER=A10K3X")
CABLE_B = PortInfo("COM10", "USB Serial Port (COM10)", serial_number="B77Q2Z",
                   hwid="USB VID:PID=0403:6001 SER=B77Q2Z")


@pytest.fixture
def ports(monkeypatch):
    """Swap in a fake port list; tests mutate it to plug/unplug cables."""
    attached: list[PortInfo] = []
    monkeypatch.setattr(dialogs, "list_ports", lambda: list(attached))
    return attached


def test_label_strips_windows_suffix_and_shows_serial_number():
    assert CABLE_A.label == "COM3 — USB Serial Port  [SN A10K3X]"


def test_label_falls_back_to_usb_location_then_bare_device():
    assert PortInfo("/dev/ttyUSB0", "FT232R", location="1-1.2").label == \
        "/dev/ttyUSB0 — FT232R  [USB 1-1.2]"
    assert PortInfo("/dev/ttyS0", "n/a").label == "/dev/ttyS0"


def test_list_ports_sorts_naturally(monkeypatch):
    class Fake:
        def __init__(self, device):
            self.device = device
            self.description = self.serial_number = self.location = self.hwid = None

    monkeypatch.setattr(serialport.serial.tools.list_ports, "comports",
                        lambda: [Fake("COM10"), Fake("COM2"), Fake("COM3")])
    assert [p.device for p in serialport.list_ports()] == ["COM2", "COM3", "COM10"]


def test_single_cable_is_picked_automatically(qapp, ports):
    ports.append(CABLE_A)
    page = dialogs.SerialPage()
    assert page.validate() == ""
    profile = Profile()
    page.apply(profile)
    assert profile.device == "COM3"


def test_several_cables_require_a_choice(qapp, ports):
    ports.extend([CABLE_A, CABLE_B])
    page = dialogs.SerialPage()
    assert page.device.currentIndex() == -1
    assert "2 detected" in page.device.placeholderText()
    assert page.validate() != ""

    page.device.setCurrentIndex(page.device.findData("COM10"))
    profile = Profile()
    page.apply(profile)
    assert profile.device == "COM10"


def test_refresh_keeps_the_chosen_cable(qapp, ports):
    ports.extend([CABLE_A, CABLE_B])
    page = dialogs.SerialPage()
    page.device.setCurrentIndex(page.device.findData("COM10"))

    ports.insert(0, PortInfo("COM1", "Communications Port"))  # new cable at the top
    page.refresh_ports()
    assert page.device.currentData() == "COM10"


def test_saved_port_that_is_unplugged_stays_selected(qapp, ports):
    ports.extend([CABLE_A, CABLE_B])
    page = dialogs.SerialPage()
    page.load(Profile(kind="serial", device="COM7"))
    assert page.device.currentText() == "COM7  (not detected)"
    profile = Profile()
    page.apply(profile)
    assert profile.device == "COM7"


def test_loading_a_new_profile_clears_the_previous_choice(qapp, ports):
    ports.extend([CABLE_A, CABLE_B])
    page = dialogs.SerialPage()
    page.load(Profile(kind="serial", device="COM3"))
    page.load(Profile())
    assert page.device.currentIndex() == -1


def test_no_ports_detected(qapp, ports):
    page = dialogs.SerialPage()
    assert page.device.placeholderText() == "No serial ports detected"
    assert page.validate() != ""


def test_other_entry_accepts_a_typed_device(qapp, ports, monkeypatch):
    ports.append(CABLE_A)
    page = dialogs.SerialPage()
    monkeypatch.setattr(dialogs.QInputDialog, "getText",
                        staticmethod(lambda *a, **k: ("/dev/pts/4", True)))
    other = page.device.findData(dialogs._OTHER)
    page.device.setCurrentIndex(other)
    page._port_activated(other)
    assert page._selected_device() == "/dev/pts/4"


def test_cancelling_other_restores_the_previous_port(qapp, ports, monkeypatch):
    ports.extend([CABLE_A, CABLE_B])
    page = dialogs.SerialPage()
    page.device.setCurrentIndex(page.device.findData("COM10"))
    monkeypatch.setattr(dialogs.QInputDialog, "getText",
                        staticmethod(lambda *a, **k: ("", False)))
    other = page.device.findData(dialogs._OTHER)
    page.device.setCurrentIndex(other)
    page._port_activated(other)
    assert page._selected_device() == "COM10"
