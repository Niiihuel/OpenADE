#!/usr/bin/env python3
"""Real debugpy + native UI on the private Wayland fixture only.

All debugger actions use rendered controls or keyboard events. The observer
captures production state; no DAP responses or editor states are injected.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import time

from native_desktop_fixture import (
    _private_pointer, _private_runtime, desktop_metadata, logical_point, move_cursor, pointer_button,
    read_native_state, region_point,
)


SOURCE = """import os
import time
from pathlib import Path
Path("debuggee.pid").write_text(str(os.getpid()))
def compute(value):
    result = value + 1
    return result
counter = 3
counter = compute(counter)
print(f"counter={counter}", flush=True)
while True:
    time.sleep(0.05)
"""


def run(*args):
    return subprocess.check_output(["rtk", *map(str, args)], text=True)


def wait(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.04)
    raise AssertionError(label)


def fixture_environment(root):
    env = {key: value for key, value in os.environ.items()
           if not any(part in key.upper() for part in ("API_KEY", "TOKEN", "SECRET", "PASSWORD"))}
    for key in ("OPENIDE_MODEL", "OPENIDE_API_BASE"):
        env.pop(key, None)
    tools = root / "fixture-bin"
    tools.mkdir()
    real_git = shutil.which("git")
    assert real_git
    (tools / "gh").write_text(f"#!{sys.executable}\nraise SystemExit(1)\n")
    (tools / "git").write_text(
        f"#!{sys.executable}\nimport os, sys\n"
        "if sys.argv[1:2] == ['credential-manager']:\n    raise SystemExit(1)\n"
        f"real_git = {real_git!r}\nos.execv(real_git, [real_git, *sys.argv[1:]])\n")
    for path in tools.iterdir():
        path.chmod(0o700)
    env["PATH"] = str(tools) + os.pathsep + env.get("PATH", "")
    (root / "gh-config").mkdir()
    env["GH_CONFIG_DIR"] = str(root / "gh-config")
    return env


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--python", type=Path, required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    assert _private_runtime() is not None, "private compositor required; user desktop input is disabled"
    options.output.mkdir(parents=True, exist_ok=True)
    (options.output / "report.json").unlink(missing_ok=True)
    root = Path(tempfile.mkdtemp(prefix="openide-debug-desktop-e2e-", dir="/tmp"))
    (root / ".openide-editor-fixture").touch()
    project = root / "project"
    project.mkdir()
    (project / "main.rs").write_text("fn main() {}\n")
    source = project / "main.py"
    source.write_text(SOURCE)
    config_directory = project / ".openide"
    config_directory.mkdir()
    interpreter = str(options.python.absolute())
    version = run(interpreter, "-c", "import debugpy; print(debugpy.__version__)").strip()
    (config_directory / "debug.json").write_text(json.dumps({
        "version": 1, "configurations": [{
            "name": "Python native fixture",
            "adapter": {"command": interpreter, "args": ["-m", "debugpy.adapter"], "id": "debugpy"},
            "request": "launch", "arguments": {
                "program": "${workspaceFolder}/main.py", "cwd": "${workspaceFolder}",
                "python": [interpreter], "console": "internalConsole", "redirectOutput": True,
                "justMyCode": True, "subProcess": False, "stopOnEntry": False,
            },
        }],
    }, indent=2))
    previous_workspace = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {entry["id"] for entry in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(index for index in range(90, 120) if index not in occupied)
    process = None
    log = None
    debuggee_pid = None
    checks = []

    def active():
        return json.loads(run("hyprctl", "activewindow", "-j"))

    def observation():
        return read_native_state(root, process.pid) if process else None

    def state_when(predicate, label):
        def check():
            value = observation()
            return value if value and predicate(value) else None
        return wait(check, label)

    def reachable(value, region_id):
        return value["regions"].get(region_id, {}).get("hit") == region_id

    def hover(region_id):
        value = state_when(lambda state: reachable(state, region_id), f"unreachable native control: {region_id}")
        x, y = region_point(active(), value, region_id)
        move_cursor(run, active, process.pid, x, y)

    def click(region_id):
        hover(region_id)
        pointer_button(run, active, process.pid)
        time.sleep(0.15)

    def keys(*args):
        assert active().get("pid") == process.pid, "fixture lost focus; no keys sent"
        subprocess.run(["rtk", options.wtype, *args], check=True)
        time.sleep(0.15)

    def chord(key):
        keys("-M", "ctrl", "-k", key, "-m", "ctrl")

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        width, height = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {width}x{height}", str(options.output / name)], check=True)

    def scoped(kind, key, state=None):
        current = state or observation()
        return f'debug_{kind}:' + json.dumps([current["debug"]["scope"], key], separators=(",", ":"), ensure_ascii=False)

    def control(operation):
        value = state_when(lambda state: state["debug"]["controls"].get(operation),
                           f"debug action not enabled: {operation}")
        click(scoped("control", operation, value))

    def scroll_debug(scroll_id):
        value = observation()
        rect = value["regions"][scroll_id]["rect"]
        x, y = logical_point(active(), value, rect[0] + rect[2] * 0.75, rect[1] + rect[3] * 0.65)
        move_cursor(run, active, process.pid, x, y)
        pointer = _private_pointer()
        assert pointer is not None and active().get("pid") == process.pid
        pointer._send(pointer.pointer, 5, 0)
        pointer._message(pointer.pointer, 7, struct.pack("=IIii",
            int(time.monotonic() * 1000) & 0xffffffff, 0, 15 * 256, 1))
        pointer._send(pointer.pointer, 4)
        pointer._roundtrip()
        time.sleep(0.1)

    def reveal_debug_row(region_id):
        for _ in range(20):
            value = observation()
            region = value["regions"].get(region_id)
            if region and region["rect"][3] >= 20 and region["hit"] == region_id:
                return value
            scroll_debug("debug_sidebar:" + value["debug"]["scope"])
        raise AssertionError(f"debug sidebar could not reveal its real row: {region_id}")

    def reveal_console_text(expected):
        if not reachable(observation(), "ide_bottom:console"):
            # The header button reveals the current bottom surface. Selecting
            # Console then changes presentation without touching the debuggee.
            click("ide_terminal_toggle")
        click("ide_bottom:console")
        state_when(lambda value: "debug_console:" + value["debug"]["scope"] in value["regions"],
                   "IDE bottom console did not expose its real output viewport")
        for _ in range(24):
            value = observation()
            if any(expected in text for text in value["texts"]):
                assert any(key.startswith("debug_console_line:") for key in value["regions"])
                return value
            scroll_debug("debug_console:" + value["debug"]["scope"])
        raise AssertionError(f"real DAP output was not rendered in the bottom console: {expected}")

    def stopped_at(line):
        return state_when(lambda value: value["debug"]["status"] == "Stopped"
                          and any(frame["selected"] and frame["line"] == line and frame["path"]
                                  and Path(frame["path"]).name == "main.py" for frame in value["debug"]["frames"]),
                          f"real adapter did not stop at source line {line}")

    def variable(name, expected):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            value = observation()
            rows = value["debug"]["variables"]
            found = next((item for item in rows if item["name"] == name and item["value"] == expected), None)
            if found:
                return reveal_debug_row(scoped("expand", found["key"], value))
            locals_row = next((item for item in rows if item["name"] in ("Locals", "Locales")
                               and item["has_children"] and not item["expanded"] and not item["loading"]), None)
            if locals_row:
                click(scoped("expand", locals_row["key"], value))
            time.sleep(0.05)
        raise AssertionError(f"real locals did not contain {name}={expected}")

    def intact(value):
        assert source.read_text() == SOURCE, "debugger unexpectedly wrote the source file"
        editor = value.get("editor")
        if editor and Path(editor["path"]).name == "main.py":
            assert editor["text"] == SOURCE and not editor["dirty"], "debugger changed the editor buffer"

    def is_alive(pid):
        try:
            os.kill(pid, 0)
            stat = Path(f"/proc/{pid}/stat")
            return not stat.exists() or stat.read_text().split(") ", 1)[1].split()[0] != "Z"
        except ProcessLookupError:
            return False

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {workspace} }}))")
        log = open(options.output / "native-debug.log", "w")
        process = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)],
                                   stdout=log, stderr=subprocess.STDOUT, env=fixture_environment(root))
        wait(lambda: active().get("pid") == process.pid, "fixture window did not receive focus")
        state_when(lambda value: value.get("editor"), "initial editor did not load")
        click("dock_add_tab")
        click("dock_open:files")
        files = state_when(lambda value: any(row["name"] == "main.py" for row in value["files"]),
                           "Files did not list the private Python source")
        click(next(row["id"] for row in files["files"] if row["name"] == "main.py"))
        opened = state_when(lambda value: value.get("editor") and Path(value["editor"]["path"]).name == "main.py",
                            "Files did not open the actual Python editor")
        editor_id = f'code_editor:{opened["editor"]["tab"]}'
        assert not opened["ide"] and not opened["debug"]["open"]
        click("dock_add_tab")
        agent_menu = state_when(lambda value: "dock_open:terminal" in value["regions"]
                               or "dock_open:files" in value["regions"], "Agent dock menu did not open")
        assert "dock_open:debugger" not in agent_menu["regions"]
        keys("-k", "Escape")
        click(editor_id)
        chord("Home")
        keys(*[part for _ in range(8) for part in ("-k", "Down")])
        keys("-k", "F5", "-k", "F9")
        agent = observation()
        assert not agent["ide"] and not agent["debug"]["open"]
        assert not agent["editor"]["breakpoints"] and not agent["editor"]["breakpoint_glyphs"]
        assert not agent["debug"]["controls"]["stop"] and not (project / "debuggee.pid").exists()
        intact(agent)
        capture("debug-agent-unavailable-native.png")
        click("window_mode_toggle")
        ide = state_when(lambda value: value["ide"] and value.get("editor")
                         and Path(value["editor"]["path"]).name == "main.py", "IDE mode did not retain the Python editor")
        assert (ide["editor"]["document"], ide["editor"]["revision"]) == (opened["editor"]["document"], opened["editor"]["revision"])
        click(editor_id)
        chord("Home")
        keys(*[part for _ in range(8) for part in ("-k", "Down")])
        marker_id = f'editor_breakpoint:{ide["editor"]["tab"]}:{ide["editor"]["document"]}:{ide["editor"]["revision"]}:8'
        hover(marker_id)
        preview = state_when(lambda value: any(glyph["kind"] == "fill"
                             and glyph["color"] == [206, 113, 113, 90]
                             and glyph["rect"][2:] == [8.0, 8.0]
                             for glyph in value["editor"]["breakpoint_glyphs"].get(marker_id, [])),
                             "IDE gutter hover did not draw the passive breakpoint preview")
        assert not preview["editor"]["breakpoints"] and not (project / "debuggee.pid").exists()
        intact(preview)
        capture("debug-gutter-hover-native.png")
        keys("-k", "F9")
        pending = state_when(lambda value: value.get("editor") and 8 in value["editor"]["breakpoints"],
                             "F9 did not create the real source breakpoint")
        intact(pending)
        assert not (project / "debuggee.pid").exists(), "config/breakpoint setup executed the program before Start"
        capture("debug-pending-native.png")

        click("ide_activity:debug")
        state_when(lambda value: value["debug"]["open"] and value["debug"]["controls"]["start"],
                   "debugger did not load the explicit launch configuration")
        assert not (project / "debuggee.pid").exists(), "opening the debugger auto-executed its launch config"
        capture("debug-ready-native.png")
        control("start")
        first = stopped_at(9)
        debuggee_pid = int((project / "debuggee.pid").read_text())
        assert is_alive(debuggee_pid)
        first = variable("counter", "3")
        intact(first)
        verified = state_when(lambda value: value.get("editor")
                              and any(marker["row"] == 8 and marker["state"] == "Verified"
                                      for marker in value["editor"]["debug_breakpoints"]),
                              "adapter acknowledgement did not produce a verified source marker")
        assert verified["editor"]["debug_execution"]["row"] == 8
        capture("debug-breakpoint-locals-native.png")
        checks.append("Agent F5/F9 expose no debugger or markers; IDE keeps the document, previews a gutter dot, and explicit Start obtains a verified breakpoint, stack and counter=3")

        control("step_into")
        entered = stopped_at(6)
        variable("value", "3")
        intact(entered)
        capture("debug-step-into-native.png")
        control("step_over")
        next_state = stopped_at(7)
        variable("result", "4")
        intact(next_state)
        control("step_out")
        returned = state_when(lambda value: value["debug"]["status"] == "Stopped"
                              and any(frame["selected"] and frame["line"] in (9, 10) for frame in value["debug"]["frames"]),
                              "Step Out did not return to the caller")
        selected_frame = next(frame for frame in returned["debug"]["frames"] if frame["selected"])
        if selected_frame["line"] == 9:
            control("step_over")
            stopped_at(10)
        computed = variable("counter", "4")
        intact(computed)
        capture("debug-step-result-native.png")
        checks.append("native Step Into, Step Over and Step Out use the adapter's real frames and updated local values without editing source")

        control("resume")
        running = state_when(lambda value: value["debug"]["status"] == "Running"
                             and any("counter=4" in line["text"] for line in value["debug"]["output"]),
                             "Continue did not resume and deliver the program's real stdout")
        assert not running["debug"]["frames"] and not running["debug"]["variables"], "paused object references remained visible after Continue"
        assert len(running["debug"]["output"]) < 16, "fixture stdout exceeded the observer's explicit bound"
        reveal_console_text("counter=4")
        capture("debug-running-output-native.png")
        control("pause")
        paused = state_when(lambda value: value["debug"]["status"] == "Stopped" and value["debug"]["frames"],
                            "Pause did not obtain a new real suspended stack")
        intact(paused)
        capture("debug-paused-native.png")
        control("stop")
        ended = state_when(lambda value: value["debug"]["status"] in ("Ended", "Idle")
                           and not value["debug"]["controls"]["stop"], "Stop did not complete the debug session")
        wait(lambda: not is_alive(debuggee_pid), "Stop left the launched debuggee alive")
        assert not ended["debug"]["frames"] and not ended["debug"]["variables"]
        intact(ended)
        capture("debug-ended-native.png")
        checks.append("native Continue delivers stdout and clears stale inspection; Pause refreshes the stack; Stop terminates the owned debuggee")

        # Exercise the real Wayland close request while another launched process
        # is paused. No signal/fixture cleanup may satisfy these assertions.
        control("start")
        restarted = stopped_at(9)
        closing_debuggee_pid = int((project / "debuggee.pid").read_text())
        assert closing_debuggee_pid != debuggee_pid and is_alive(closing_debuggee_pid)
        intact(restarted)
        assert any(marker["row"] == 8 and marker["state"] == "Verified"
                   for marker in restarted["editor"]["debug_breakpoints"])
        preserved_breakpoints = restarted["editor"]["breakpoints"]
        preserved_document = (restarted["editor"]["document"], restarted["editor"]["revision"])
        click("window_mode_toggle")
        agent_paused = state_when(lambda value: not value["ide"]
                                 and not any(key.startswith("debug_control:") for key in value["regions"]),
                                 "Agent mode exposed live debugger controls")
        assert agent_paused["debug"]["status"] == "Stopped"
        assert not agent_paused["editor"]["breakpoint_glyphs"]
        keys("-k", "F5", "-k", "F9")
        still_paused = observation()
        assert still_paused["debug"]["status"] == "Stopped" and is_alive(closing_debuggee_pid)
        assert still_paused["editor"]["breakpoints"] == preserved_breakpoints
        assert (still_paused["editor"]["document"], still_paused["editor"]["revision"]) == preserved_document
        assert any(frame["selected"] and frame["line"] == 9 for frame in still_paused["debug"]["frames"])
        intact(still_paused)
        capture("debug-agent-paused-native.png")
        click("window_mode_toggle")
        restored = state_when(lambda value: value["ide"] and value["debug"]["status"] == "Stopped"
                              and value["debug"]["open"], "returning to IDE lost the paused session")
        assert (restored["editor"]["document"], restored["editor"]["revision"]) == preserved_document
        assert restored["editor"]["breakpoints"] == preserved_breakpoints
        capture("debug-before-window-close-native.png")
        assert _private_runtime() is not None and active().get("pid") == process.pid
        run("hyprctl", "eval", f"local w = hl.get_active_window(); assert(w.pid == {process.pid}); "
            "hl.dispatch(hl.dsp.window.close({ window = w }))")
        wait(lambda: process.poll() is not None, "native close request did not finish the app and debugger cleanup")
        assert process.returncode == 0, f"native window close exited abnormally: {process.returncode}"
        wait(lambda: not is_alive(closing_debuggee_pid), "native window close left the restarted debuggee alive")
        assert source.read_text() == SOURCE, "native window close unexpectedly wrote the source file"
        checks.append("paused session survives Agent mode with F5/F9 inert and no debugger UI; returning to IDE preserves source and breakpoints, and native window close terminates the paused debuggee")
        report = {"passed": checks, "fixture": str(root), "pid": process.pid,
                  "debuggee_pid": debuggee_pid, "adapter_python": interpreter, "debugpy": version,
                  "closed_debuggee_pid": closing_debuggee_pid, "native_close_exit_code": process.returncode,
                  **desktop_metadata(), "fps_claim": None}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        (options.output / "failure.json").unlink(missing_ok=True)
        (options.output / "debug-failure-native.png").unlink(missing_ok=True)
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        (options.output / "failure.json").write_text(json.dumps({"passed": checks, "fixture": str(root),
            "error": str(error), "observation": observation()}, indent=2, ensure_ascii=False))
        if process is not None and active().get("pid") == process.pid:
            capture("debug-failure-native.png")
        print(json.dumps({"passed": checks, "fixture": str(root), "error": str(error)}, ensure_ascii=False), flush=True)
        raise
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        # Failure cleanup is restricted to the process whose real command line
        # still names this private fixture. It never matches a user's Python.
        pid_file = project / "debuggee.pid"
        if pid_file.exists():
            owned_pid = int(pid_file.read_text())
            command_line = Path(f"/proc/{owned_pid}/cmdline")
            if is_alive(owned_pid) and command_line.exists() and os.fsencode(source) in command_line.read_bytes():
                os.kill(owned_pid, signal.SIGTERM)
        if log is not None:
            log.close()
        if json.loads(run("hyprctl", "activeworkspace", "-j"))["id"] == workspace:
            run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {previous_workspace} }}))")


if __name__ == "__main__":
    main()
