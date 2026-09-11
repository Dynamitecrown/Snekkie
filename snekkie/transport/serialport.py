"""Serial transport backed by pyserial."""

from __future__ import annotations

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


def list_ports() -> list[tuple[str, str]]:
    """Available ports as (device, description) pairs."""
    return [(p.device, p.description or p.device)
            for p in serial.tools.list_ports.comports()]


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
