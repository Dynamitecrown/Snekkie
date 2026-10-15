#!/usr/bin/env python3
"""Check the installed Flatpak's actual permissions after bundle installation."""
import configparser
import sys

metadata = configparser.ConfigParser(interpolation=None)
metadata.read(sys.argv[1], encoding="utf-8")
context = metadata["Context"]


def values(key):
    return {item for item in context.get(key, "").split(";") if item}


assert values("shared") == {"network", "ipc"}, "Wrong network/IPC permissions"
# build-finish also records "x11" with fallback-x11 for Flatpak versions that
# predate fallback-x11; current versions still use X11 only without Wayland.
assert values("sockets") - {"x11"} == {"wayland", "fallback-x11", "ssh-auth"}, "Wrong display/SSH-agent sockets"
assert values("devices") == {"all"}, "Serial tty nodes require the all-devices permission"
assert values("filesystems") == {"~/.ssh:create"}, "SSH files need scoped write access; home-wide access is forbidden"
assert "Session Bus Policy" not in metadata, "No broad D-Bus policy is needed"
assert "System Bus Policy" not in metadata, "No system-bus access is needed"
print("PASS: installed Flatpak permissions match the reviewed SSH/serial requirements")
