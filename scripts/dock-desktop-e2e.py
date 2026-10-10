#!/usr/bin/env python3
"""Native Wayland terminal/session, browser retry and file navigation checks.

Requires an editor-e2e build, Hyprland, wtype, grim, and the existing Playwright
installation. The private headless runner provides a scoped Wayland pointer;
all data and shell commands stay in a new /tmp fixture.
Observations are one-way; actions use the real keyboard/pointer with PID checks.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import shlex
import socket
import struct
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from native_desktop_fixture import _private_pointer, _private_runtime, desktop_metadata, logical_point, move_cursor, pointer_button, read_native_state, region_point


def run(*args):
    return subprocess.check_output(["rtk", *map(str, args)], text=True)


def wait(check, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(label)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    assert _private_runtime() is not None, "run this fixture through headless-desktop-fixture.py; user desktop input is disabled"
    options.output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="openide-dock-desktop-e2e-", dir="/tmp"))
    (root / ".openide-editor-fixture").touch()
    project = root / "project"
    project.mkdir()
    source = project / "main.rs"
    source.write_text("fn main() {}\n")
    dependencies = root / "state/browser-runtime/deps"
    dependencies.parent.mkdir(parents=True)
    data_home = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share"))
    module = Path(os.environ.get("OPENIDE_PLAYWRIGHT_MODULE_DIR", data_home / "openide/browser-runtime/deps"))
    assert (module / "node_modules/playwright/package.json").is_file(), "Playwright must already be installed"
    dependencies.symlink_to(module.resolve(), target_is_directory=True)
    server_state = {"ready": 0, "fail_retry": True, "value": None}
    lock = threading.Lock()
    html = b'''<!doctype html><html><meta charset="utf-8"><title>Dock fixture</title>
    <style>body{background:#181818;color:#eee;font:18px sans-serif;padding:24px}input{display:block;width:80%;padding:14px;font:18px sans-serif}</style>
    <h1>OpenIDE dock E2E</h1><label>Native input<input autofocus></label>
    <script>const input=document.querySelector('input');
    document.addEventListener('pointerdown',e=>{if(e.target!==input){e.preventDefault();input.focus()}});
    input.addEventListener('input',()=>fetch('/value',{method:'POST',body:input.value}));
    addEventListener('load',()=>fetch('/ready',{method:'POST'}));</script></html>'''

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            with lock:
                fail = self.path.startswith("/retry") and server_state["fail_retry"]
            if fail:
                self.close_connection = True
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(html)

        def do_POST(self):
            value = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()
            with lock:
                if self.path == "/ready":
                    server_state["ready"] += 1
                elif self.path == "/value":
                    server_state["value"] = value
            self.send_response(204)
            self.end_headers()

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_port}/"
    (root / "browser-url").write_text(url)
    previous_workspace = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {workspace["id"] for workspace in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(index for index in range(90, 120) if index not in occupied)
    process = None
    checks = []
    log = None

    def active():
        return json.loads(run("hyprctl", "activewindow", "-j"))

    def observation():
        return read_native_state(root, process.pid) if process else None

    def state_when(predicate, label):
        def ready():
            value = observation()
            return value if value and predicate(value) else None
        return wait(ready, label)

    def keys(*args):
        assert active().get("pid") == process.pid, "fixture lost focus; no keys sent"
        subprocess.run(["rtk", options.wtype, *args], check=True)
        time.sleep(0.15)

    def chord(key):
        keys("-M", "ctrl", "-k", key, "-m", "ctrl")

    def click(region_id):
        observed = state_when(lambda value: region_id in value["regions"] and value["regions"][region_id]["hit"] == region_id, f"{region_id} is not reachable")
        x, y = region_point(active(), observed, region_id)
        move_cursor(run, active, process.pid, x, y)
        pointer_button(run, active, process.pid)
        time.sleep(0.15)

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        width, height = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {width}x{height}", str(options.output / name)], check=True)

    def drag_logical(observed, start, finish):
        start = logical_point(active(), observed, *start)
        finish = logical_point(active(), observed, *finish)
        move_cursor(run, active, process.pid, *start)
        pointer_button(run, active, process.pid, "down")
        try:
            move_cursor(run, active, process.pid, *finish)
            time.sleep(0.25)
        finally:
            # Always release this fixture's synthetic button, even if focus
            # changes during a drag. No further motion/press is then issued.
            pointer_button(run, active, process.pid, "up")

    def resize(width, height):
        assert active().get("pid") == process.pid
        run("hyprctl", "eval", f"local w = hl.get_active_window(); assert(w.pid == {process.pid}); "
            "hl.dispatch(hl.dsp.window.fullscreen_state({internal=0,client=0,window=w})); "
            "hl.dispatch(hl.dsp.window.float({action='set',window=w})); "
            f"hl.dispatch(hl.dsp.window.resize({{x={width},y={height},window=w}})); "
            "hl.dispatch(hl.dsp.window.center({window=w}))")
        return state_when(lambda value: abs(value["viewport"][0] - width) <= 2
                          and abs(value["viewport"][1] - height) <= 2,
                          "native viewport resize failed")

    def scroll_tabs(delta):
        observed = observation()
        rect = observed["regions"]["terminal_tabs"]["rect"]
        x, y = logical_point(active(), observed, rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5)
        move_cursor(run, active, process.pid, x, y)
        pointer = _private_pointer()
        assert pointer is not None and active().get("pid") == process.pid
        samples = []
        # A wheel needs a signed discrete step as well as its fixed value.
        # axis() alone does not provide negative wheel ticks on this compositor.
        # zwlr_virtual_pointer_v1 axis_source=5, axis_discrete=7, frame=4:
        # https://github.com/swaywm/wlr-protocols/blob/master/unstable/wlr-virtual-pointer-unstable-v1.xml
        for _ in range(8):
            assert active().get("pid") == process.pid
            pointer._send(pointer.pointer, 5, 0)  # wl_pointer.axis_source.wheel
            pointer._message(pointer.pointer, 7, struct.pack("=IIii",
                int(time.monotonic() * 1000) & 0xffffffff, 1, round(delta * 256),
                -1 if delta < 0 else 1))
            pointer._send(pointer.pointer, 4)
            pointer._roundtrip()
            deadline = time.monotonic() + 0.06
            while time.monotonic() < deadline:
                sample = observation()
                if sample and "MouseWheel" in sample.get("raw_input", ""):
                    event = {key: sample.get(key) for key in ("raw_input", "mapped_input")}
                    if event not in samples:
                        samples.append(event)
                time.sleep(0.005)
        (options.output / "native-scroll-events.json").write_text(json.dumps(samples, indent=2))

    def tab_close_reachable(value, tab):
        region = value["regions"].get(f"terminal_close:{tab}")
        return region and region["hit"] == f"terminal_close:{tab}" and region["rect"][2] >= 23.9

    def terminal_command(command, marker, filename, region_id="terminal_screen"):
        click(region_id)
        pid_path = shlex.quote(str(project / filename))
        cwd_path = shlex.quote(str(project / (filename + ".cwd")))
        # Absolute fixture outputs cannot modify another directory even if a
        # broken host were to start the shell in an unexpected working path.
        keys(command + f"; printf '%s' \"$$\" > {pid_path}; pwd -P > {cwd_path}")
        keys("-k", "Return")
        wait(lambda: (project / filename).is_file() and (project / (filename + ".cwd")).is_file(), f"terminal did not write {filename}")
        assert Path((project / (filename + ".cwd")).read_text().strip()) == project.resolve(), "terminal started outside its fixture workspace"
        state_when(lambda value: marker in (value["terminal"]["text"] or ""), f"terminal output missing {marker}")
        return (project / filename).read_text().strip()

    def active_terminal(value):
        return next((tab["id"] for tab in value["terminal"]["tabs"] if tab["active"]), None)

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {workspace} }}))")
        env = os.environ.copy()
        env["OPENIDE_TERMINAL_SHELL"] = shutil.which("sh") or "/bin/sh"
        log = open(options.output / "native-dock.log", "w")
        process = subprocess.Popen([str(options.binary.resolve()), "--browser-e2e", str(root)], stdout=log, stderr=subprocess.STDOUT, env=env)
        wait(lambda: active().get("pid") == process.pid, "fixture window did not receive focus")
        initial = state_when(lambda value: "browser_viewport" in value["regions"], "native browser did not become ready")
        assert Path(initial["workspace"]) == project.resolve(), "fixture bootstrapped an unexpected workspace"
        browser_tab = next(tab["id"] for tab in initial["dock"]["tabs"] if tab["kind"] == "Browser")
        file_tab = next(tab["id"] for tab in initial["dock"]["tabs"] if tab["kind"] == "FilePreview")
        click(f"dock_tab:{file_tab}")
        click(f"code_editor:{file_tab}")
        chord("End")
        keys("// native dock fixture\n")
        click("editor_menu")
        click(f"code_editor_save:{file_tab}")
        wait(lambda: "// native dock fixture" in source.read_text(), "editor menu Save did not reach fixture file")
        state_when(lambda value: "editor_menu_close" not in value["regions"], "Save did not dismiss the scoped editor menu")
        click(f"code_editor:{file_tab}")
        chord("End")
        keys("// native shortcut save\n")
        chord("s")
        wait(lambda: "// native shortcut save" in source.read_text(), "Ctrl+S did not save fixture file")
        checks.append("native file tab, contextual Save menu and Ctrl+S persist edits")

        chord("grave")
        first = state_when(lambda value: value["terminal"]["visible"] and value["terminal"]["running"] and len(value["terminal"]["tabs"]) == 1, "Ctrl+grave did not open a live terminal")
        first_tab = active_terminal(first)
        first_pid = terminal_command("export OPENIDE_E2E_TOKEN=kept; printf 'FIRST:%s\\n' \"$OPENIDE_E2E_TOKEN\"", "FIRST:kept", "terminal-first.pid")

        click("terminal_menu_toggle")
        click(f"terminal_rename:{first_tab}")
        rename_field = f"terminal_rename_input:{first_tab}"
        state_when(lambda value: rename_field in value["regions"] and value["focused"] == rename_field,
                   "rename dialog did not focus its real name input")
        chord("a")
        keys("Shell de prueba")
        capture("terminal-rename-native.png")
        click(f"terminal_rename_confirm:{first_tab}")
        state_when(lambda value: rename_field not in value["regions"]
                   and any(tab["id"] == first_tab and tab["title"] == "Shell de prueba" for tab in value["terminal"]["tabs"]),
                   "renaming did not update the existing terminal identity")
        renamed_pid = terminal_command("printf 'RENAMED:%s\\n' \"$OPENIDE_E2E_TOKEN\"",
                                       "RENAMED:kept", "terminal-renamed.pid")
        assert renamed_pid == first_pid, "renaming restarted the PTY or lost its environment"
        checks.append("native rename updates the session label while preserving its PID and environment")

        click("terminal_profiles_toggle")
        state_when(lambda value: "terminal_new_profile:default" in value["regions"],
                   "profile menu did not offer the resolved default profile")
        capture("terminal-profiles-native.png")
        keys("-k", "Escape")
        state_when(lambda value: "terminal_new_profile:default" not in value["regions"]
                   and value["focused"] == "terminal_profiles_toggle",
                   "Escape did not dismiss profiles and return focus to its trigger")
        click("terminal_profiles_toggle")
        click("terminal_new_profile:default")
        profile_state = state_when(lambda value: len(value["terminal"]["tabs"]) == 2
                                   and active_terminal(value) != first_tab and value["terminal"]["running"],
                                   "default profile did not create a new independent live terminal")
        profile_tab = active_terminal(profile_state)
        profile_pid = terminal_command("printf 'PROFILE_DEFAULT\\n'", "PROFILE_DEFAULT", "terminal-profile.pid")
        assert profile_pid != first_pid, "creating from a profile reused the existing shell"
        click(f"terminal_close:{profile_tab}")
        state_when(lambda value: len(value["terminal"]["tabs"]) == 1 and active_terminal(value) == first_tab
                   and all(tab["id"] != profile_tab for tab in value["terminal"]["tabs"]),
                   "closing the profile-created terminal changed the surviving session")
        checks.append("native profiles support Escape focus return and create a distinct default-profile PTY that closes independently")

        click("terminal_new")
        second = state_when(lambda value: len(value["terminal"]["tabs"]) == 2 and active_terminal(value) != first_tab and value["terminal"]["running"], "new terminal session did not become active")
        second_tab = active_terminal(second)
        second_pid = terminal_command("printf 'SECOND\\n'", "SECOND", "terminal-second.pid")
        assert first_pid != second_pid, "two tabs reused one shell"
        click("terminal_new")
        overflow = state_when(lambda value: len(value["terminal"]["tabs"]) == 3
                              and active_terminal(value) not in (first_tab, second_tab)
                              and value["terminal"]["running"],
                              "overflow fixture did not create its third terminal")
        overflow_tab = active_terminal(overflow)
        overflow_pid = terminal_command("printf 'OVERFLOW_THIRD\\n'", "OVERFLOW_THIRD", "terminal-overflow.pid")
        assert overflow_pid not in (first_pid, second_pid)
        wide = observation()
        first_width = wide["regions"][f"terminal_tab:{first_tab}"]["rect"][2]
        wide_width, wide_height = wide["viewport"]
        # The native window enforces a 520px minimum. Three genuine sessions
        # produce overflow at that supported size without changing app policy.
        resize(520, 640)
        narrow = state_when(lambda value: tab_close_reachable(value, overflow_tab),
                            "resize did not reveal the active terminal tab and its complete close target")
        # The shared tab component also caps an individual tab to the strip
        # width minus its two fade margins, independent of overflow clipping.
        narrow_first_width = min(first_width, max(0, narrow["regions"]["terminal_tabs"]["rect"][2] - 32))
        first_region = narrow["regions"].get(f"terminal_tab:{first_tab}")
        assert first_region is None or first_region["rect"][2] < narrow_first_width - 2, "narrow fixture did not overflow the terminal tab strip"
        capture("terminal-tabs-overflow-end-native.png")
        scroll_tabs(-15)
        state_when(lambda value: f"terminal_tab:{first_tab}" in value["regions"]
                   and value["regions"][f"terminal_tab:{first_tab}"]["rect"][2] >= narrow_first_width - 1,
                   "horizontal wheel did not reveal the first terminal tab")
        click(f"terminal_tab:{first_tab}")
        state_when(lambda value: active_terminal(value) == first_tab and tab_close_reachable(value, first_tab),
                   "direct tab selection did not preserve the revealed close target")
        capture("terminal-tabs-overflow-start-native.png")
        resize(wide_width, wide_height)
        state_when(lambda value: all(tab_close_reachable(value, tab) for tab in (first_tab, second_tab, overflow_tab)),
                   "restoring window width did not reveal all terminal tabs")
        click(f"terminal_close:{overflow_tab}")
        state_when(lambda value: len(value["terminal"]["tabs"]) == 2 and active_terminal(value) == first_tab,
                   "closing an inactive overflow tab changed the selected terminal")
        checks.append("native terminal tabs scroll horizontally at 520px, reveal the active close target and close an inactive tab without changing its neighbor")
        click(f"terminal_tab:{first_tab}")
        state_when(lambda value: active_terminal(value) == first_tab and "FIRST:kept" in (value["terminal"]["text"] or ""), "first session output was lost on tab switch")
        again_pid = terminal_command("printf 'AGAIN:%s\\n' \"$OPENIDE_E2E_TOKEN\"", "AGAIN:kept", "terminal-again.pid")
        assert again_pid == first_pid, "switching tabs restarted the shell"
        checks.append("two native terminal sessions have distinct PIDs; switching preserves output, PID and shell environment")
        capture("terminal-tabs-native.png")

        click("terminal_panel_hide")
        state_when(lambda value: not value["terminal"]["visible"], "terminal panel did not hide")
        chord("grave")
        state_when(lambda value: value["terminal"]["visible"] and active_terminal(value) == first_tab, "reopening changed the active session")
        kept_pid = terminal_command("printf 'KEPT:%s\\n' \"$OPENIDE_E2E_TOKEN\"", "KEPT:kept", "terminal-kept.pid")
        assert kept_pid == first_pid, "hide/reopen restarted the shell"
        checks.append("native hide/reopen preserves the active PTY and environment")

        before_resize = observation()
        old_height = before_resize["terminal"]["height"]
        width, height = before_resize["viewport"]
        boundary = height - 4.0 - old_height
        drag_logical(before_resize, (width * 0.65, boundary), (width * 0.65, max(60.0, boundary - 56.0)))
        state_when(lambda value: value["terminal"]["height"] > old_height + 20, "native terminal separator did not resize the panel")
        capture("terminal-resized-native.png")
        checks.append("native terminal separator changes the viewport height without creating sessions")
        resized_height = observation()["terminal"]["height"]
        click("terminal_menu_toggle")
        click("terminal_panel_maximize")
        maximized = state_when(lambda value: value["terminal"]["height"] > resized_height + 20,
                               "terminal did not expand from its menu")
        submit = maximized["regions"]["run_toggle"]["rect"]
        screen = maximized["regions"]["terminal_screen"]["rect"]
        assert submit[3] >= 35.9 and submit[1] + submit[3] < screen[1] - 36, "expanding terminal clipped the composer"
        click("terminal_menu_toggle")
        click("terminal_panel_maximize")
        state_when(lambda value: abs(value["terminal"]["height"] - resized_height) < 1,
                   "restoring terminal lost its preferred height")
        checks.append("native expand/restore preserves composer controls and the chosen terminal height")
        click(f"terminal_tab:{second_tab}")
        state_when(lambda value: active_terminal(value) == second_tab, "direct terminal tab did not activate the terminal to close")
        click(f"terminal_close:{second_tab}")
        state_when(lambda value: len(value["terminal"]["tabs"]) == 1 and active_terminal(value) == first_tab, "closing the second session changed the surviving one")
        surviving_pid = terminal_command("printf 'NEIGHBOR:%s\\n' \"$OPENIDE_E2E_TOKEN\"", "NEIGHBOR:kept", "terminal-neighbor.pid")
        assert surviving_pid == first_pid, "closing a terminal tab restarted its neighbor or lost its environment"

        # Split is a real second PTY viewport, not just another selector item.
        click("terminal_split")
        split = state_when(lambda value: len(value["terminal"].get("panes", [])) == 2
                           and all(pane["running"] and pane["rect"] for pane in value["terminal"]["panes"]),
                           "split did not create two visible live terminal panes")
        split_tab = next(pane["id"] for pane in split["terminal"]["panes"] if pane["id"] != first_tab)

        def pane_region(tab):
            return f"terminal_screen:{tab}"

        split_pid = terminal_command(
            "export OPENIDE_SPLIT_TOKEN=second; printf 'SPLIT_SECOND:%s\\n' \"$OPENIDE_SPLIT_TOKEN\"",
            "SPLIT_SECOND:second", "terminal-split.pid", pane_region(split_tab))
        assert split_pid != first_pid, "split reused the existing PTY process"
        original_pid = terminal_command(
            "printf 'SPLIT_FIRST:%s\\n' \"$OPENIDE_E2E_TOKEN\"", "SPLIT_FIRST:kept",
            "terminal-split-first.pid", pane_region(first_tab))
        assert original_pid == first_pid, "focusing the original split pane restarted its shell"
        isolated = state_when(lambda value: all(pane["text"] for pane in value["terminal"]["panes"]),
                              "split pane snapshots are unavailable")
        first_pane = next(pane for pane in isolated["terminal"]["panes"] if pane["id"] == first_tab)
        second_pane = next(pane for pane in isolated["terminal"]["panes"] if pane["id"] == split_tab)
        assert "SPLIT_FIRST:kept" in first_pane["text"] and "SPLIT_SECOND:second" not in first_pane["text"]
        assert "SPLIT_SECOND:second" in second_pane["text"] and "SPLIT_FIRST:kept" not in second_pane["text"]
        first_rect, second_rect = first_pane["rect"], second_pane["rect"]
        assert first_rect[2] > 80 and second_rect[2] > 80
        assert first_rect[0] + first_rect[2] <= second_rect[0] + 1 or second_rect[0] + second_rect[2] <= first_rect[0] + 1, "split viewport hit regions overlap"
        capture("terminal-split-native.png")
        click("terminal_profiles_toggle")
        click("terminal_unsplit")
        state_when(lambda value: not value["terminal"].get("panes")
                   and len(value["terminal"]["tabs"]) == 2 and active_terminal(value) == first_tab,
                   "unsplit must keep both sessions and the active shell")
        click(f"terminal_tab:{split_tab}")
        restored_pid = terminal_command(
            "printf 'SPLIT_KEPT:%s\\n' \"$OPENIDE_SPLIT_TOKEN\"", "SPLIT_KEPT:second",
            "terminal-split-kept.pid")
        assert restored_pid == split_pid, "unsplit restarted the second PTY or lost its environment"
        click(f"terminal_close:{split_tab}")
        state_when(lambda value: len(value["terminal"]["tabs"]) == 1 and active_terminal(value) == first_tab,
                   "closing the former split pane changed its surviving session")
        checks.append("native split creates distinct PTYs with isolated input/output; unsplit preserves both processes and environment")
        click("terminal_panel_hide")

        click(f"dock_tab:{browser_tab}")
        click("browser_url")
        chord("a")
        keys(url + "retry")
        keys("-k", "Return")
        error_state = state_when(lambda value: bool(value["browser"]["error"]) and "browser_viewport" not in value["regions"] and "browser_navigate" in value["regions"], "navigation error did not replace the browser viewport with a retry state")
        assert error_state["texts"].count("No se pudo abrir esta página") == 1, "browser error heading must appear once"
        capture("browser-single-error-native.png")
        with lock:
            ready_before = server_state["ready"]
            server_state["fail_retry"] = False
        click("browser_navigate")
        wait(lambda: server_state["ready"] > ready_before, "native retry did not request the fixture page")
        state_when(lambda value: not value["browser"]["error"] and "browser_viewport" in value["regions"], "retry did not restore the page")
        click("browser_viewport")
        keys("browser restored café")
        wait(lambda: server_state["value"] == "browser restored café", "restored browser did not accept native input")
        checks.append("one browser error state replaces Chromium content; native Retry restores page and input")
        capture("browser-restored-native.png")

        wide = observation()
        separator = wide["dock"]["separator_x"]
        assert separator is not None, "right dock has no resize boundary"
        old_browser_width = wide["regions"]["browser_viewport"]["rect"][2]
        drag_logical(wide, (separator, wide["viewport"][1] * 0.4), (separator + 64.0, wide["viewport"][1] * 0.4))
        state_when(lambda value: "browser_viewport" in value["regions"] and value["regions"]["browser_viewport"]["rect"][2] < old_browser_width - 20.0, "native dock resize did not change the browser viewport")
        click("browser_viewport")
        keys(" resize")
        wait(lambda: server_state["value"] == "browser restored café resize", "browser lost input after native dock resize")
        capture("browser-narrow-native.png")
        checks.append("native right-dock resize preserves browser content and pointer/input routing")

        click("dock_add_tab")
        click("dock_open:files")
        files_state = state_when(lambda value: any(row["name"] == "main.rs" for row in value["files"]), "file explorer did not list the fixture")
        main_row = next(row["id"] for row in files_state["files"] if row["name"] == "main.rs")
        capture("files-native.png")
        click(main_row)
        state_when(lambda value: f"code_editor:{file_tab}" in value["regions"], "file explorer did not reopen the existing editor")
        assert "// native dock fixture" in source.read_text()
        checks.append("Files panel opens its real row in the existing editable file tab")

        # This fixture has no account credentials. Exercise the real account
        # surface and its settings route without contacting a provider.
        click("toggle_sidebar_reopen")
        state_when(lambda value: "usage_toggle" in value["regions"], "sidebar did not reveal account usage")
        chord("grave")
        state_when(lambda value: value["terminal"]["visible"], "terminal did not reopen beside the sidebar")
        capture("workspace-rounded-corners-native.png")
        click("usage_toggle")
        usage = state_when(lambda value: "popover_surface:usage_dismiss" in value["regions"],
                           "account usage popover did not open")
        x, y, popup_width, popup_height = usage["regions"]["popover_surface:usage_dismiss"]["rect"]
        assert popup_width <= 380 and popup_height <= 320, "empty account popover reserves unnecessary space"
        assert x >= 0 and y >= 0 and x + popup_width <= usage["viewport"][0] and y + popup_height <= usage["viewport"][1]
        capture("account-usage-native.png")
        keys("-k", "Escape")
        state_when(lambda value: "popover_surface:usage_dismiss" not in value["regions"],
                   "Escape did not dismiss account usage")
        click("usage_toggle")
        click("usage_details")
        state_when(lambda value: "popover_surface:usage_dismiss" not in value["regions"]
                   and "Suscripciones y consumo" in value["texts"],
                   "account usage footer did not open its native settings page")
        checks.append("native account popover fits content, closes with Escape and opens usage settings")
        report = {"passed": checks, "fixture": str(root), "pid": process.pid, "terminal_pids": [first_pid, profile_pid, second_pid, overflow_pid, split_pid], **desktop_metadata(), "fps_claim": None}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        (options.output / "failure.json").unlink(missing_ok=True)
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        report = {"passed": checks, "fixture": str(root), "error": str(error), "observation": observation()}
        (options.output / "failure.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        if process is not None and active().get("pid") == process.pid:
            capture("dock-failure-native.png")
        print(json.dumps({"passed": checks, "fixture": str(root), "error": str(error)}, ensure_ascii=False), flush=True)
        raise
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        server.shutdown()
        if log is not None:
            log.close()
        if json.loads(run("hyprctl", "activeworkspace", "-j"))["id"] == workspace:
            run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {previous_workspace} }}))")


if __name__ == "__main__":
    main()
