#!/usr/bin/env python3
"""Native keymap profiles, modal priority, persistence and PTY isolation.

Only runs under the private compositor fixture. All changes are native UI input
against a disposable project; the observer is read-only and feature-gated.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import time

from native_desktop_fixture import (
    _private_runtime, _private_pointer, desktop_metadata, logical_point, move_cursor,
    pointer_button, read_native_state,
)


def run(*args):
    return subprocess.check_output(["rtk", *map(str, args)], text=True)


def wait(check, label, timeout=25):
    deadline = time.monotonic()+timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.04)
    raise AssertionError(label)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    assert _private_runtime() is not None, "private compositor required; user desktop input is disabled"
    options.output.mkdir(parents=True, exist_ok=True)
    for name in ("report.json", "failure.json"):
        (options.output/name).unlink(missing_ok=True)
    for path in options.output.glob("keymap-*.png"):
        path.unlink()
    root = Path(tempfile.mkdtemp(prefix="openide-keymap-e2e-", dir="/tmp"))
    (root/".openide-editor-fixture").touch()
    (root/"menu-fixture.json").write_text("{}")
    project = root/"project"
    project.mkdir()
    source = project/"main.rs"
    original = 'fn main() {\n    println!("Native keymap fixture");\n}\n'
    source.write_text(original)
    run("git", "-C", project, "init", "--quiet", "--initial-branch=fixture")
    sink = root/"key-sink.py"
    byte_log = root/"pty-bytes.bin"
    ready = root/"pty-ready"
    done = root/"pty-done"
    sink.write_text(
        "import os, sys, termios, tty\nfrom pathlib import Path\n"
        f"destination=Path({str(byte_log)!r})\nready=Path({str(ready)!r})\ndone=Path({str(done)!r})\n"
        "fd=sys.stdin.fileno()\noriginal=termios.tcgetattr(fd)\n"
        "try:\n    tty.setraw(fd)\n    destination.write_bytes(b'')\n"
        "    os.write(1,b'\\r\\nKEY_SINK_READY\\r\\n')\n    ready.touch()\n"
        "    while True:\n        value=os.read(fd,1)\n"
        "        if not value or value == b'q': break\n"
        "        with destination.open('ab',buffering=0) as log: log.write(value)\n"
        "finally:\n    termios.tcsetattr(fd,termios.TCSADRAIN,original)\n"
        "    os.write(1,b'\\r\\nKEY_SINK_DONE\\r\\n')\n    done.touch()\n"
    )
    previous_workspace = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {item["id"] for item in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(index for index in range(90, 120) if index not in occupied)
    process, log, env = None, None, None
    checks, observations = [], []

    def active():
        return json.loads(run("hyprctl", "activewindow", "-j"))

    def observation():
        return read_native_state(root, process.pid) if process else None

    def state_when(predicate, label):
        def ready_state():
            value = observation()
            return value if value and predicate(value) else None
        return wait(ready_state, label)

    def reachable(value, identifier):
        return value["regions"].get(identifier, {}).get("hit") == identifier

    def click(identifier):
        value = state_when(lambda value: reachable(value, identifier), f"unreachable {identifier}")
        x, y, w, h = value["regions"][identifier]["rect"]
        move_cursor(run, active, process.pid, *logical_point(active(), value, x+w/2, y+h/2))
        pointer_button(run, active, process.pid)
        time.sleep(.15)

    def keys(*args):
        assert active().get("pid") == process.pid, "fixture lost focus; no keys sent"
        subprocess.run(["rtk", options.wtype, "-d", "15", *args], check=True)
        time.sleep(.18)

    def chord(key, shift=False):
        args = ["-M", "ctrl"]
        if shift:
            args += ["-M", "shift"]
        args += ["-k", key]
        if shift:
            args += ["-m", "shift"]
        keys(*args, "-m", "ctrl")

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        w, h = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {w}x{h}", str(options.output/name)], check=True)

    def scroll_settings(ticks):
        value = state_when(lambda value: "settings_page:Shortcuts" in value["regions"],
                           "shortcuts scroll viewport unavailable")
        x, y, w, h = value["regions"]["settings_page:Shortcuts"]["rect"]
        move_cursor(run, active, process.pid, *logical_point(active(), value, x+w*.7, y+h*.6))
        pointer = _private_pointer()
        assert pointer is not None
        for _ in range(abs(ticks)):
            assert active().get("pid") == process.pid
            step = 1 if ticks > 0 else -1
            pointer._send(pointer.pointer, 5, 0)
            pointer._message(pointer.pointer, 7, struct.pack("=IIii",
                int(time.monotonic()*1000) & 0xffffffff, 0, step*15*256, step))
            pointer._send(pointer.pointer, 4)
        pointer._roundtrip()
        time.sleep(.3)

    def launch():
        nonlocal process
        process = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)],
                                   stdout=log, stderr=subprocess.STDOUT, env=env)
        wait(lambda: active().get("pid") == process.pid, "fixture did not receive focus")
        state_when(lambda value: value.get("editor") and value.get("menus", {}).get("keymap"),
                   "fixture or keymap observer not ready")
        window = active()
        floating = "" if window["floating"] else "hl.dispatch(hl.dsp.window.float({window=w})); "
        run("hyprctl", "eval", f"local w=hl.get_active_window(); assert(w.pid=={process.pid}); "
            "hl.dispatch(hl.dsp.window.fullscreen_state({internal=0,client=0,window=w})); " + floating +
            "hl.dispatch(hl.dsp.window.resize({x=1400,y=900,window=w})); hl.dispatch(hl.dsp.window.center({window=w}))")
        state_when(lambda value: value["viewport"] == [1400, 900], "native resize failed")

    def go_ide():
        if not observation()["ide"]:
            click("window_mode_toggle")
            state_when(lambda value: value["ide"], "IDE mode did not activate")

    def panel_state():
        value = observation()
        return (value["terminal"]["visible"], value["menus"]["bottom_open"], value["menus"]["bottom_panel"],
                tuple(tab["id"] for tab in value["terminal"]["tabs"]))

    def no_leak(label):
        time.sleep(.08)
        assert byte_log.read_bytes() == b"", (label, "global shortcut leaked into PTY", byte_log.read_bytes().hex())
        observations.append({"case": label, "pty_bytes": 0, "panel": panel_state()})

    def escape_palette():
        keys("-k", "Escape")
        state_when(lambda value: not value["menus"]["command_center"], "palette did not dismiss")

    def open_palette(key="p", shift=True, scope="Commands"):
        if key == "F1":
            keys("-k", "F1")
        else:
            chord(key, shift)
        state_when(lambda value: value["menus"]["command_center"]
                   and value["menus"]["command_scope"] == scope
                   and value["menus"]["command_focused"] == "command_query",
                   f"{key} did not open focused {scope} palette")

    def shell_pid(filename):
        click("terminal_screen")
        path = project/filename
        keys("printf '%s' \"$$\" > " + shlex.quote(str(path)))
        keys("-k", "Return")
        wait(path.is_file, "shell did not write its PID")
        return path.read_text()

    def menu_action(target):
        for identifier in observation()["regions"]:
            if identifier.startswith("ide_menu_action:") and json.loads(identifier.partition(":")[2])[1] == target:
                return identifier
        raise AssertionError(f"missing menu action {target}")

    def open_shortcut_settings():
        go_ide()
        click("ide_settings")
        state_when(lambda value: value["menus"]["settings_open"], "settings did not open")
        if observation()["menus"]["settings_page"] != "Shortcuts":
            if reachable(observation(), "settings_link:shortcuts"):
                click("settings_link:shortcuts")
            else:
                if not reachable(observation(), "settings_page:shortcuts"):
                    click("settings_nav_toggle")
                click("settings_page:shortcuts")
        state_when(lambda value: value["menus"]["settings_page"] == "Shortcuts", "shortcuts page did not open")
        scroll_settings(-100)

    def set_profile(profile):
        open_shortcut_settings()
        click("settings_keymap_toggle")
        capture(f"keymap-{profile}-selector-native.png")
        click(f"settings_keymap:{profile}")
        state_when(lambda value: value["menus"]["keymap"] == profile, f"profile did not change to {profile}")
        prefs = root/"state/settings-preferences.json"
        wait(lambda: prefs.is_file() and json.loads(prefs.read_text()).get("keymap_profile") == profile,
             f"{profile} preference was not persisted")
        capture(f"keymap-{profile}-settings-native.png")
        click("toggle_settings")
        state_when(lambda value: not value["menus"]["settings_open"], "settings did not close")

    def set_vim(enabled):
        open_shortcut_settings()
        if observation()["menus"]["vim_enabled"] != enabled:
            click("settings_vim_toggle")
        state_when(lambda value: value["menus"]["vim_enabled"] == enabled,
                   "Vim preference did not update")
        prefs = root/"state/settings-preferences.json"
        wait(lambda: prefs.is_file() and json.loads(prefs.read_text()).get("vim_mode") == enabled,
             "Vim preference was not persisted")
        capture("keymap-vim-settings-native.png")
        click("toggle_settings")
        state_when(lambda value: not value["menus"]["settings_open"], "Vim settings did not close")

    def editor_when(predicate, label):
        return state_when(lambda value: value.get("editor") and predicate(value["editor"]), label)["editor"]

    def vim_mode(mode):
        return editor_when(lambda editor: editor["vim_mode"] == mode, f"Vim did not enter {mode}")

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{workspace={workspace}}}))")
        spec = importlib.util.spec_from_file_location("marketplace_fixture", Path(__file__).with_name("marketplace-desktop-e2e.py"))
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        env = fixture.fixture_environment(root)
        env.pop("OPENIDE_MARKETPLACE_DESKTOP_FIXTURE", None)
        env["OPENIDE_MENU_DESKTOP_FIXTURE"] = "1"
        env["OPENIDE_TERMINAL_SHELL"] = shutil.which("sh") or "/bin/sh"
        env["PS1"], env["ENV"] = "fixture> ", "/dev/null"
        for key in [key for key in env if key.lower().endswith("proxy")]:
            env.pop(key)
        env.update(HTTP_PROXY="http://127.0.0.1:9", HTTPS_PROXY="http://127.0.0.1:9", ALL_PROXY="http://127.0.0.1:9", NO_PROXY="127.0.0.1,localhost")
        log = open(options.output/"native-keymap.log", "w")
        launch()
        assert observation()["menus"]["keymap"] == "vscode", "fresh profile is not VS Code"
        assert not observation()["ide"], "fixture should begin in Agents mode"
        open_palette("p", False, "Files")
        capture("keymap-agents-quick-open-native.png")
        escape_palette()
        open_palette()
        escape_palette()
        go_ide()
        initial_editor_id = f"code_editor:{observation()['editor']['tab']}"
        click(initial_editor_id)
        chord("Home")
        keys("-M", "shift", "-k", "End", "-m", "shift")
        initial_line = original.partition("\n")[0]
        editor_when(lambda editor: editor["selection"] == [0, len(initial_line)],
                    "fixture line selection did not receive native keys")
        chord("x")
        editor_when(lambda editor: editor["text"] == original[len(initial_line):],
                    "unmodified Ctrl+X did not cut the selected editor text")
        chord("z")
        editor_when(lambda editor: editor["text"] == original, "undo did not restore normal cut")
        chord("Home")
        keys("-M", "shift", "-k", "End", "-m", "shift")
        selected = observation()["editor"]["selection"]
        chord("x", True)
        state_when(lambda value: value["menus"]["activity"] == "extensions", "Ctrl+Shift+X did not open Extensions")
        assert observation()["editor"]["text"] == original, "Ctrl+Shift+X cut selected code"
        assert observation()["editor"]["selection"] == selected, "Ctrl+Shift+X changed code selection"
        capture("keymap-extensions-no-cut-native.png")
        chord("e", True)
        state_when(lambda value: value["menus"]["activity"] == "files", "Ctrl+Shift+E did not restore Files")
        open_palette("p", False, "Files")
        keys("main.rs")
        state_when(lambda value: "main.rs" in value["menus"]["command_query"], "file query not entered")
        keys("-k", "Return")
        state_when(lambda value: not value["menus"]["command_center"], "quick-open did not restore editor")
        checks.append("native Ctrl+X cuts and Undo restores; Ctrl+Shift+X opens Extensions without cutting selected code")
        assert not observation()["terminal"]["tabs"]
        chord("m", True)
        state_when(lambda value: value["menus"]["bottom_open"] and value["menus"]["bottom_panel"] == "Problems", "Ctrl+Shift+M did not show Problems")
        for key, panel in ((None, "Problems"), ("output", "Output"), ("console", "DebugConsole")):
            if key:
                click(f"ide_bottom:{key}")
            chord("j")
            state_when(lambda value: not value["menus"]["bottom_open"] and not value["terminal"]["visible"], "Ctrl+J did not hide current section")
            assert observation()["menus"]["bottom_panel"] == panel
            chord("j")
            value = state_when(lambda value: value["menus"]["bottom_open"] and value["menus"]["bottom_panel"] == panel, "Ctrl+J did not restore same section")
            assert not value["terminal"]["tabs"], "panel toggle created an unwanted PTY"
        capture("keymap-default-console-native.png")
        checks.append("default VS Code opens Files/Commands from Agents; Ctrl+Shift+M opens IDE Problems and Ctrl+J remembers Problems/Output/Console without creating terminals")

        chord("grave")
        value = state_when(lambda value: value["terminal"]["visible"] and value["terminal"]["running"] and value["focused"] == "terminal_screen", "Ctrl+grave did not open focused terminal")
        terminal_id = next(tab["id"] for tab in value["terminal"]["tabs"] if tab["active"])
        original_pid = shell_pid("shell-before.pid")
        keys(shlex.quote(sys.executable)+" "+shlex.quote(str(sink)))
        keys("-k", "Return")
        wait(ready.is_file, "raw PTY recorder did not become ready")
        state_when(lambda value: "KEY_SINK_READY" in (value["terminal"]["text"] or ""), "PTY readiness was not rendered")
        chord("j")
        state_when(lambda value: not value["terminal"]["visible"], "Ctrl+J from PTY did not hide panel")
        no_leak("Ctrl+J hides from PTY")
        keys("-M", "ctrl", "-P", "j", "-s", "900", "-p", "j", "-m", "ctrl")
        state_when(lambda value: value["terminal"]["visible"] and value["focused"] == "terminal_screen", "Ctrl+J reopen did not restore terminal focus")
        no_leak("held Ctrl+J reopens terminal without autorepeat leaking into PTY")
        checks.append("Ctrl+J toggles the focused terminal; a 900ms held chord reopens once without sending linefeed or autorepeat into its live PTY")

        open_palette()
        before = panel_state()
        chord("j")
        assert observation()["menus"]["command_center"] and panel_state() == before, "palette lost priority to bottom-panel shortcut"
        no_leak("palette owns Ctrl+J")
        capture("keymap-command-palette-native.png")
        escape_palette()
        open_palette("F1")
        no_leak("F1 command palette")
        escape_palette()
        open_palette("p", False, "Files")
        keys("main")
        state_when(lambda value: "main" in value["menus"]["command_query"], "quick-open query lost input")
        before = panel_state()
        chord("grave")
        assert observation()["menus"]["command_center"] and panel_state() == before, "quick-open lost priority to terminal shortcut"
        no_leak("quick-open owns input and Ctrl+grave")
        capture("keymap-quick-open-native.png")
        escape_palette()
        checks.append("Ctrl+Shift+P/F1 select Commands and Ctrl+P Files; palettes retain focus and block background shortcuts and PTY input")

        click("terminal_screen")
        chord("m", True)
        state_when(lambda value: value["menus"]["bottom_open"] and value["menus"]["bottom_panel"] == "Problems", "Problems shortcut from terminal failed")
        no_leak("Ctrl+Shift+M from raw PTY")
        chord("grave")
        state_when(lambda value: value["terminal"]["visible"] and value["focused"] == "terminal_screen", "terminal shortcut did not restore existing PTY")
        assert [tab["id"] for tab in observation()["terminal"]["tabs"]] == [terminal_id]
        click("terminal_profiles_toggle")
        state_when(lambda value: reachable(value, "terminal_new_profile:default"), "profiles popup not open")
        before = panel_state()
        chord("j")
        assert reachable(observation(), "terminal_new_profile:default") and panel_state() == before, "profile popup did not retain priority"
        no_leak("profile popup owns Ctrl+J")
        keys("-k", "Escape")
        state_when(lambda value: value["focused"] == "terminal_profiles_toggle", "popup Escape lost trigger focus")
        chord("grave")
        state_when(lambda value: value["terminal"]["visible"] and value["focused"] == "terminal_screen", "Ctrl+grave did not focus already-visible terminal")
        chord("grave")
        state_when(lambda value: not value["terminal"]["visible"], "Ctrl+grave did not hide already-focused terminal")
        chord("grave")
        state_when(lambda value: value["terminal"]["visible"] and value["focused"] == "terminal_screen", "Ctrl+grave did not reopen terminal")
        no_leak("Ctrl+grave focus/hide/reopen")
        checks.append("terminal shortcut focuses before hiding, Problems preserves its session, and profile popovers retain keyboard priority")

        click("ide_menu:file")
        click(menu_action("ide_new_file"))
        state_when(lambda value: value["menus"]["new_file"] and value["focused"] == "ide_new_file_path", "new-file dialog not focused")
        before = panel_state()
        chord("j")
        chord("grave")
        chord("m", True)
        assert observation()["menus"]["new_file"] and observation()["focused"] == "ide_new_file_path" and panel_state() == before, "modal lost focus or changed background panel"
        no_leak("new-file modal blocks background shortcuts")
        capture("keymap-modal-priority-native.png")
        keys("-k", "Escape")
        state_when(lambda value: not value["menus"]["new_file"], "new-file dialog did not cancel")
        click("terminal_screen")
        keys("q")
        wait(done.is_file, "raw PTY recorder did not receive explicit exit input")
        assert byte_log.read_bytes() == b""
        assert shell_pid("shell-after.pid") == original_pid, "shortcuts restarted the shell"
        checks.append("new-file modal blocks background shortcuts; all consumed chords sent zero bytes to the original PTY")

        set_profile("zed")
        open_palette()
        escape_palette()
        open_palette("F1")
        escape_palette()
        chord("m", True)
        state_when(lambda value: value["menus"]["bottom_open"] and value["menus"]["bottom_panel"] == "Problems", "Zed Problems shortcut failed")
        chord("j")
        state_when(lambda value: not value["menus"]["bottom_open"], "Zed panel hide shortcut failed")
        chord("j")
        state_when(lambda value: value["menus"]["bottom_open"], "Zed panel restore shortcut failed")
        checks.append("Zed profile selected through Settings is saved and routes palette, Problems and panel toggling")

        set_profile("openide")
        open_palette("k", False, "All")
        capture("keymap-classic-palette-native.png")
        escape_palette()

        set_vim(True)
        go_ide()
        editor_id = f"code_editor:{observation()['editor']['tab']}"
        click(editor_id)
        vim_mode("Normal")
        keys("gg0")
        editor_when(lambda editor: editor["selection"] == [0, 0], "Vim gg0 did not reach buffer start")
        keys("i")
        vim_mode("Insert")
        prefix = "café 🧠 "
        keys(prefix)
        editor_when(lambda editor: editor["text"] == prefix+original,
                    "Vim Insert lost or duplicated native Unicode text")
        keys("-k", "Escape")
        vim_mode("Normal")
        editor_when(lambda editor: editor["selection"] == [6, 6], "Vim Escape did not leave caret on last inserted character")
        keys("h")
        editor_when(lambda editor: editor["selection"] == [5, 5], "Vim h was omitted or doubled by Key+Text")
        keys("l")
        editor_when(lambda editor: editor["selection"] == [6, 6], "Vim l was omitted or doubled by Key+Text")
        keys("h")
        keys("x")
        editor_when(lambda editor: editor["text"] == "café  "+original,
                    "Vim x did not remove exactly one Unicode grapheme")
        keys("u")
        editor_when(lambda editor: editor["text"] == prefix+original,
                    "Vim u did not restore Unicode deletion")
        keys("v")
        vim_mode("Visual")
        keys("l")
        editor = editor_when(lambda editor: editor["selection"][0] != editor["selection"][1],
                            "Vim visual motion did not create selection")
        start, end = sorted(editor["selection"])
        assert editor["text"][start:end] == "🧠 ", ("Vim visual selection is not inclusive", editor["selection"])
        capture("keymap-vim-visual-native.png")
        keys("x")
        vim_mode("Normal")
        editor_when(lambda editor: editor["text"] == "café "+original,
                    "Vim visual deletion did not match displayed selection")
        keys("u")
        editor_when(lambda editor: editor["text"] == prefix+original,
                    "Vim undo did not restore visual selection deletion")
        before = panel_state()
        chord("j")
        assert panel_state() == before and observation()["editor"]["text"] == prefix+original, \
            "Ctrl+J escaped Vim editor ownership or inserted text"
        keys("u")
        editor_when(lambda editor: editor["text"] == original,
                    "Vim undo did not restore the complete insertion as one change")
        capture("keymap-vim-normal-native.png")
        click("ide_bottom:problems")
        state_when(lambda value: value["menus"]["bottom_open"] and value["menus"]["bottom_panel"] == "Problems", "Problems tab did not take focus")
        chord("j")
        state_when(lambda value: not value["menus"]["bottom_open"], "Ctrl+J outside Vim editor did not hide bottom panel")
        checks.append("independent Vim preference handles native Unicode Insert, single h/l motions, x/u, inclusive visual deletion and Ctrl+J editor ownership")

        process.terminate()
        process.wait(timeout=10)
        launch()
        assert observation()["menus"]["keymap"] == "openide", "chosen keymap did not survive process restart"
        assert observation()["menus"]["vim_enabled"], "Vim preference did not survive process restart"
        open_palette("k", False, "All")
        escape_palette()
        set_profile("vscode")
        assert observation()["menus"]["vim_enabled"], "changing keymap disabled independent Vim preference"
        open_palette()
        escape_palette()
        checks.append("classic Ctrl+K and independent Vim preference survive process restart; choosing VS Code restores its bindings without disabling Vim")

        assert source.read_text() == original
        assert not (root/"state/provider-credentials").exists()
        report = {"passed": checks, "fixture": str(root), "observations": observations,
                  "pty_input_hex": byte_log.read_bytes().hex(), **desktop_metadata()}
        (options.output/"report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        (options.output/"failure.json").write_text(json.dumps({"passed": checks, "error": str(error),
            "observations": observations, "observation": observation(),
            "pty_input_hex": byte_log.read_bytes().hex() if byte_log.is_file() else None}, indent=2, ensure_ascii=False))
        if process and active().get("pid") == process.pid:
            capture("keymap-failure.png")
        raise
    finally:
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        if log:
            log.close()
        if json.loads(run("hyprctl", "activeworkspace", "-j"))["id"] == workspace:
            run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{workspace={previous_workspace}}}))")


if __name__ == "__main__":
    main()
