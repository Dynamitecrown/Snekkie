"""Serial transport backed by pyserial."""

from __future__ import annotations

import re
from dataclasses import dataclass

import serial
import serial.tools.list_ports

from . import Transport, TransportError, register

READ_TIMEOUT = 0.1

BAUD_RATES = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400]

PARITY = {
    "None": serial.PARITY_NONE,
    "Even": serial.PARITY_EVEN,
    "Odd": serial.PARITY_ODD,
    "Mark": serial.PARITY_MARK,
    "Space": serial.PARITY_SPACE,
}

BYTESIZE = {5: serial.FIVEBITS, 6: serial.SIXBITS,
            7: serial.SEVENBITS, 8: serial.EIGHTBITS}

STOPBITS = {1: serial.STOPBITS_ONE,
            1.5: serial.STOPBITS_ONE_POINT_FIVE,
            2: serial.STOPBITS_TWO}


@dataclass(frozen=True)
class PortInfo:
    """One detected serial port, labelled for the port picker."""

    device: str
    description: str = ""
    serial_number: str = ""
    location: str = ""
    hwid: str = ""

    @property
    def label(self) -> str:
        """e.g. "COM4 — USB Serial Port  [SN A10K3X]".

        Two identical USB console cables share a description, so the serial
        number (or, failing that, the USB location) is what tells them apart.
        """
        # Windows already appends "(COM4)" to the description; don't repeat it.
        desc = self.description
        suffix = f"({self.device})"
        if desc.endswith(suffix):
            desc = desc[:-len(suffix)].rstrip()
        if desc in ("", "n/a", self.device):
            text = self.device
        else:
            text = f"{self.device} — {desc}"
        if self.serial_number:
            text += f"  [SN {self.serial_number}]"
        elif self.location:
            text += f"  [USB {self.location}]"
        return text


def _natural_key(device: str) -> list:
    """Sort COM2 before COM10 and ttyUSB2 before ttyUSB10."""
    return [int(part) if part.isdigit() else part.lower()
            for part in re.split(r"(\d+)", device)]


def list_ports() -> list[PortInfo]:
    """Currently attached serial ports, in natural device order."""
    ports = [PortInfo(device=p.device,
                      description=p.description or "",
                      serial_number=p.serial_number or "",
                      location=p.location or "",
                      hwid=p.hwid or "")
             for p in serial.tools.list_ports.comports()]
    return sorted(ports, key=lambda port: _natural_key(port.device))


@register("serial", "Serial")
class SerialTransport(Transport):
    def __init__(self, profile, **_ignored):
        super().__init__(profile)
        self._port: serial.Serial | None = None

    def connect(self) -> None:
        p = self.profile
        try:
            self._port = serial.Serial(
                port=p.device,
                baudrate=p.baud,
                bytesize=BYTESIZE.get(p.bytesize, serial.EIGHTBITS),
                parity=PARITY.get(p.parity, serial.PARITY_NONE),
                stopbits=STOPBITS.get(p.stopbits, serial.STOPBITS_ONE),
                timeout=READ_TIMEOUT,
                write_timeout=2.0,
                rtscts=p.rtscts,
                xonxoff=p.xonxoff,
            )
        except (serial.SerialException, ValueError, OSError) as exc:
            raise TransportError(f"Could not open {p.device}: {exc}") from exc
        self._connected = True

    def read(self) -> bytes | None:
        port = self._port
        if port is None:
            return None
        try:
            # read(1) blocks up to the timeout; in_waiting grabs the rest of a
            # burst in one go so a `show run` dump doesn't crawl byte by byte.
            data = port.read(1)
            waiting = port.in_waiting
            if waiting:
                data += port.read(waiting)
            return data
        except (serial.SerialException, OSError):
            return None

    def write(self, data: bytes) -> None:
        if self._port is None:
            raise TransportError("Not connected")
        try:
            self._port.write(data)
        except (serial.SerialException, OSError) as exc:
            raise TransportError(f"Write failed: {exc}") from exc

    def close(self) -> None:
        self._connected = False
        if self._port is not None:
            try:
                self._port.close()
            except Exception:
                pass
        self._port = None

    def send_break(self) -> None:
        if self._port is None:
            raise TransportError("Not connected")
        try:
            self._port.send_break(duration=0.3)
        except (serial.SerialException, OSError) as exc:
            raise TransportError(f"Break failed: {exc}") from exc

    @property
    def description(self) -> str:
        p = self.profile
        return (f"Serial  {p.device}  {p.baud}-{p.bytesize}"
                f"{p.parity[0].upper()}{int(p.stopbits) if p.stopbits == int(p.stopbits) else p.stopbits}")
