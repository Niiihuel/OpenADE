"""Read-only fixture observations and PID-checked native pointer positioning."""
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import time


def _private_runtime(environment=None):
    """Validate the private compositor before connecting or emitting input."""
    environment = os.environ if environment is None else environment
    mode = environment.get("OPENIDE_FIXTURE_POINTER")
    private = environment.get("OPENIDE_FIXTURE_PRIVATE_RUNTIME")
    if mode is None and private is None:
        return None
    assert mode == "virtual-pointer" and private, "private pointer must fail closed"
    runtime = Path(environment["XDG_RUNTIME_DIR"])
    assert str(runtime) == private and runtime.name.startswith("oh-")
    assert runtime.stat().st_uid == os.getuid() and runtime.stat().st_mode & 0o077 == 0
    signature = environment["HYPRLAND_INSTANCE_SIGNATURE"]
    assert (runtime / ".openide-headless-fixture").read_text() == signature
    assert (runtime / "hypr" / signature / ".socket.sock").is_socket()
    display = Path(environment["WAYLAND_DISPLAY"])
    display = (runtime / display).resolve()
    assert display.parent == runtime.resolve() and display.is_socket()
    # The parent KWin socket must never receive fixture input.
    assert display.name.startswith("wayland-")
    return display


class _PrivatePointer:
    """Persistent zwlr_virtual_pointer_v1 client for the isolated fixture.

    Wire requests follow the upstream protocol, without libinput or /dev/uinput:
    https://gitlab.freedesktop.org/wlroots/wlr-protocols/-/blob/master/unstable/wlr-virtual-pointer-unstable-v1.xml
    A persistent object is required to retain button state during resize drags.
    """
    def __init__(self, display, width, height):
        self.display = display
        self.connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.connection.settimeout(5)
        self.connection.connect(str(display))
        self.buffer = bytearray()
        self.next_id = 3
        self.globals = {}
        self.width = int(width)
        self.height = int(height)
        assert 800 <= self.width <= 3840 and 600 <= self.height <= 2160
        self._send(1, 1, 2)  # wl_display.get_registry
        self._roundtrip()
        interface = "zwlr_virtual_pointer_manager_v1"
        assert interface in self.globals, "private compositor has no virtual pointer protocol"
        name, version = self.globals[interface]
        manager = self._id()
        encoded = interface.encode() + b"\0"
        payload = struct.pack("=II", name, len(encoded)) + encoded
        payload += b"\0" * (-len(encoded) % 4)
        payload += struct.pack("=II", min(version, 1), manager)
        self._message(2, 0, payload)  # wl_registry.bind
        self.pointer = self._id()
        self._send(manager, 0, 0, self.pointer)  # create_virtual_pointer(default seat)
        self._roundtrip()

    def _id(self):
        value = self.next_id
        self.next_id += 1
        return value

    def _message(self, object_id, opcode, payload):
        self.connection.sendall(struct.pack("=II", object_id, ((8 + len(payload)) << 16) | opcode) + payload)

    def _send(self, object_id, opcode, *values):
        self._message(object_id, opcode, struct.pack("=" + "I" * len(values), *values))

    def _event(self):
        while len(self.buffer) < 8:
            self._receive()
        object_id, header = struct.unpack_from("=II", self.buffer)
        size, opcode = header >> 16, header & 0xffff
        assert 8 <= size <= 65535 and size % 4 == 0, "invalid private Wayland event"
        while len(self.buffer) < size:
            self._receive()
        payload = bytes(self.buffer[8:size])
        del self.buffer[:size]
        return object_id, opcode, payload

    def _receive(self):
        data = self.connection.recv(65536)
        if not data:
            raise RuntimeError("private pointer compositor disconnected")
        self.buffer.extend(data)

    def _roundtrip(self):
        callback = self._id()
        self._send(1, 0, callback)  # wl_display.sync
        while True:
            object_id, opcode, payload = self._event()
            if object_id == 1 and opcode == 0:
                raise RuntimeError(f"private Wayland protocol error: {payload!r}")
            if object_id == 2 and opcode == 0:
                name, length = struct.unpack_from("=II", payload)
                interface = payload[8:8 + length - 1].decode()
                version, = struct.unpack_from("=I", payload, 8 + ((length + 3) // 4) * 4)
                self.globals[interface] = (name, version)
            if object_id == callback and opcode == 0:
                return

    def move(self, x, y):
        assert 0 <= x < self.width and 0 <= y < self.height
        self._send(self.pointer, 1, int(time.monotonic() * 1000) & 0xffffffff,
                   round(x), round(y), self.width, self.height)
        self._send(self.pointer, 4)  # frame
        self._roundtrip()

    def button(self, state):
        self._send(self.pointer, 2, int(time.monotonic() * 1000) & 0xffffffff,
                   272, 1 if state == "down" else 0)
        self._send(self.pointer, 4)
        self._roundtrip()

    def close(self):
        self.connection.close()


def open_private_pointer(environment):
    """Advertise the pointer seat capability before launching the fixture app."""
    display = _private_runtime(environment)
    assert display is not None
    return _PrivatePointer(display, environment["OPENIDE_FIXTURE_WIDTH"],
                           environment["OPENIDE_FIXTURE_HEIGHT"])


_pointer = None


def _private_pointer():
    global _pointer
    display = _private_runtime()
    if display is None:
        return None
    if _pointer is None:
        _pointer = open_private_pointer(os.environ)
    assert _pointer.display == display, "private pointer compositor changed"
    return _pointer


def read_native_state(root, pid):
    try:
        value = json.loads((root / "native-state.json").read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    return value if value.get("pid") == pid else None


def logical_point(window, observed, x, y):
    assert window.get("pid") == observed["pid"], "fixture lost focus; no input sent"
    width, height = observed["viewport"]
    assert width > 0 and height > 0
    assert 0 <= x < width and 0 <= y < height
    return (
        round(window["at"][0] + x * window["size"][0] / width),
        round(window["at"][1] + y * window["size"][1] / height),
    )


def region_point(window, observed, region_id, fraction=(0.5, 0.5)):
    region = observed["regions"][region_id]
    assert region["hit"] == region_id, f"{region_id} is covered by {region['hit']}"
    x, y, width, height = region["rect"]
    assert width > 0 and height > 0
    return logical_point(window, observed, x + width * fraction[0], y + height * fraction[1])


def move_cursor(run, active, pid, x, y):
    assert active().get("pid") == pid, "fixture lost focus; no pointer movement sent"
    pointer = _private_pointer()
    if pointer is not None:
        pointer.move(x, y)
    else:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.cursor.move({{ x = {x}, y = {y} }}))")
    position = json.loads(run("hyprctl", "cursorpos", "-j"))
    assert abs(position["x"] - x) <= 1 and abs(position["y"] - y) <= 1
    assert active().get("pid") == pid, "fixture lost focus; no click sent"


def pointer_button(run, active, pid, state="click"):
    """Private fixtures use their Wayland virtual pointer, never global uinput."""
    assert state in ("click", "down", "up")
    pointer = _private_pointer()
    if pointer is not None:
        if state != "up":
            assert active().get("pid") == pid, "fixture lost focus; no button sent"
        for transition in (("down", "up") if state == "click" else (state,)):
            pointer.button(transition)
        return
    if state != "up":
        assert active().get("pid") == pid, "fixture lost focus; no button sent"
    command = {"click": "click left", "down": "buttondown left", "up": "buttonup left"}[state]
    subprocess.run(["rtk", "dotool"], input=command + "\n", text=True, check=True)


def desktop_metadata():
    private = bool(os.environ.get("OPENIDE_FIXTURE_PRIVATE_RUNTIME"))
    return {
        "renderer": "eframe/wgpu native Wayland",
        "compositor": "private KWin virtual → Hyprland nested" if private else "user Hyprland session",
        "input": "private wtype + persistent Wayland virtual pointer" if private else "wtype + dotool",
    }
