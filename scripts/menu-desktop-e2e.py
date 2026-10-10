#!/usr/bin/env python3
"""Native menu and empty-editor UX on a private compositor only.

Every action is physical pointer/keyboard input. The marker-gated observer only
reports the real painted frame; it never accepts commands or modifies app state.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import time

from native_desktop_fixture import (
    _private_pointer, _private_runtime, desktop_metadata, logical_point,
    move_cursor, pointer_button, read_native_state, region_point,
)


def run(*args):
    return subprocess.check_output(["rtk", *map(str, args)], text=True)


def wait(check, label, timeout=25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.04)
    raise AssertionError(label)


def contains(outer, inner, tolerance=1):
    x, y, w, h = outer
    a, b, c, d = inner
    return a >= x-tolerance and b >= y-tolerance and a+c <= x+w+tolerance and b+d <= y+h+tolerance


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    assert _private_runtime() is not None, "private compositor required; user desktop input is disabled"
    options.output.mkdir(parents=True, exist_ok=True)
    for name in ("report.json", "failure.json"):
        (options.output / name).unlink(missing_ok=True)
    for path in options.output.glob("menu-*.png"):
        path.unlink()
    root = Path(tempfile.mkdtemp(prefix="openide-menu-e2e-", dir="/tmp"))
    (root / ".openide-editor-fixture").touch()
    (root / "menu-fixture.json").write_text("{}")
    project = root / "project"
    project.mkdir()
    source = project / "main.rs"
    original = "fn main() {\n    println!(\"Native menu fixture\");\n}\n"
    source.write_text(original)
    run("git", "-C", project, "init", "--quiet", "--initial-branch=fixture")
    previous_workspace = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {entry["id"] for entry in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(index for index in range(90, 120) if index not in occupied)
    checks, geometry, failures = [], [], []
    process = None
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

    def reachable(value, identifier):
        return value["regions"].get(identifier, {}).get("hit") == identifier

    def hover(identifier, covered=False):
        value = state_when(lambda value: identifier in value["regions"] and
                           (covered or reachable(value, identifier)), f"unreachable {identifier}")
        rect = value["regions"][identifier]["rect"]
        point = logical_point(active(), value, rect[0]+rect[2]/2, rect[1]+rect[3]/2)
        move_cursor(run, active, process.pid, *point)
        time.sleep(.2)

    def click(identifier, covered=False):
        hover(identifier, covered)
        pointer_button(run, active, process.pid)
        time.sleep(.2)

    def click_point(x, y):
        value = observation()
        move_cursor(run, active, process.pid, *logical_point(active(), value, x, y))
        pointer_button(run, active, process.pid)
        time.sleep(.2)

    def keys(*args):
        assert active().get("pid") == process.pid, "fixture lost focus; no keys sent"
        subprocess.run(["rtk", options.wtype, "-d", "35", *args], check=True)
        time.sleep(.2)

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        width, height = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {width}x{height}", str(options.output / name)], check=True)

    def resize(width, height):
        window = active()
        assert window.get("pid") == process.pid
        floating = "" if window["floating"] else "hl.dispatch(hl.dsp.window.float({window=w})); "
        run("hyprctl", "eval", f"local w=hl.get_active_window(); assert(w.pid=={process.pid}); "
            "hl.dispatch(hl.dsp.window.fullscreen_state({internal=0,client=0,window=w})); " + floating +
            f"hl.dispatch(hl.dsp.window.resize({{x={width},y={height},window=w}})); "
            "hl.dispatch(hl.dsp.window.center({window=w}))")
        state_when(lambda value: abs(value["viewport"][0]-width) <= 2 and abs(value["viewport"][1]-height) <= 2,
                   "native resize failed")
        time.sleep(.2)

    def action_id(target, required=True):
        for identifier in observation()["regions"]:
            if identifier.startswith("ide_menu_action:") and json.loads(identifier.partition(":")[2])[1] == target:
                return identifier
        assert not required, f"menu action has no region: {target}"

    def action_rows(value):
        return {identifier: region["rect"] for identifier, region in value["regions"].items()
                if identifier.startswith("ide_menu_action:") and region["hit"] == identifier}

    def highlighted(value):
        result = set()
        for identifier, rect in action_rows(value).items():
            for paint in value["menus"]["paint"]:
                if paint["kind"] == "rect" and paint["color"][3] > 0 and all(abs(a-b) <= 1 for a, b in zip(rect, paint["rect"])):
                    result.add(identifier)
        return result

    def assert_single_highlight(target):
        value = state_when(lambda value: highlighted(value) == {target}, "menu did not settle to exactly one highlighted action")
        return value

    def menu_geometry(value):
        visible_rows = action_rows(value)
        rows = {identifier: rect for identifier, rect in visible_rows.items() if rect[3] >= 25.9}
        assert rows
        viewport = [0, 0, *value["viewport"]]
        assert all(contains(viewport, rect) for rect in rows.values()), rows
        assert all(20 <= rect[3] <= 30 for rect in rows.values()), rows
        assert all(rect[2] <= 360 for rect in rows.values()), rows
        # A command's label and shortcut must be fully inside its own hit row.
        for identifier, rect in rows.items():
            painted = value["menus"]["labels"]
            # Background editor text can occupy identical coordinates. The
            # opaque menu is painted afterwards; start at its leading label.
            starts = [index for index, label in enumerate(painted)
                      if abs(label["position"][0]-(rect[0]+8)) <= 1.5 and
                      rect[1] <= label["position"][1] < rect[1]+rect[3]]
            assert starts, identifier
            labels = [label for label in painted[starts[-1]:]
                      if rect[0] <= label["position"][0] < rect[0]+rect[2] and
                      rect[1] <= label["position"][1] < rect[1]+rect[3]]
            assert labels, identifier
            assert all(contains(rect, [*label["position"], *label["bounds"]], 1.5) for label in labels), (identifier, labels)
            assert all("Ctrl-Shift" not in label["text"] for label in labels), labels
            target = json.loads(identifier.partition(":")[2])[1]
            expected_keys = {
                "commands": {"Ctrl", "Shift", "P"},
                "ide_activity:files": {"Ctrl", "Shift", "E"},
                "ide_activity:search": {"Ctrl", "Shift", "F"},
                "ide_activity:git": {"Ctrl", "Shift", "G"},
                "ide_activity:extensions": {"Ctrl", "Shift", "X"},
            }.get(target)
            if expected_keys:
                caps = [paint["rect"] for paint in value["menus"]["paint"]
                        if paint["kind"] == "stroke" and paint["radius"] == 4
                        and abs(paint["rect"][3]-20) < .1 and contains(rect, paint["rect"])]
                # The common keycap painter includes a second stroke for its
                # bottom relief. Count distinct key rectangles, not strokes.
                caps = list({tuple(cap) for cap in caps})
                assert len(caps) == len(expected_keys), (identifier, "missing menu keycaps", caps)
                cap_text = {label["text"] for label in labels
                            if any(contains(cap, [*label["position"], *label["bounds"]], 1.5) for cap in caps)}
                assert expected_keys == cap_text, (identifier, cap_text)
                leading = labels[0]
                assert min(cap[0] for cap in caps) >= leading["position"][0]+leading["bounds"][0]+6, (identifier, labels)
        geometry.append({"viewport": value["viewport"], "menu": value["menus"]["open"], "rows": rows})

    def scroll(ticks, identifier):
        hover(identifier)
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

    def closed():
        return state_when(lambda value: value["menus"]["open"] is None, "menu did not close")

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{workspace={workspace}}}))")
        spec = importlib.util.spec_from_file_location("marketplace_fixture", Path(__file__).with_name("marketplace-desktop-e2e.py"))
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        env = fixture.fixture_environment(root)
        env.pop("OPENIDE_MARKETPLACE_DESKTOP_FIXTURE", None)
        env["OPENIDE_MENU_DESKTOP_FIXTURE"] = "1"
        for key in [key for key in env if key.lower().endswith("proxy")]:
            env.pop(key)
        env.update(HTTP_PROXY="http://127.0.0.1:9", HTTPS_PROXY="http://127.0.0.1:9", ALL_PROXY="http://127.0.0.1:9", NO_PROXY="127.0.0.1,localhost")
        log = open(options.output / "native-menu.log", "w")
        process = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)],
                                   stdout=log, stderr=subprocess.STDOUT, env=env)
        wait(lambda: active().get("pid") == process.pid, "fixture did not receive focus")
        initial = state_when(lambda value: value.get("editor") and value.get("menus"), "editor fixture did not load")
        baseline = initial["editor"]
        resize(1400, 900)
        click("window_mode_toggle")
        state_when(lambda value: value["ide"], "IDE mode did not activate")
        click("ide_menu:view")
        state_when(lambda value: value["menus"]["open"] == "view", "View menu did not open")
        search = action_id("ide_activity:search")
        hover(search)
        value = assert_single_highlight(search)
        assert value["menus"]["activity"] == "files" and value["menus"]["explorer_open"]
        menu_geometry(value)
        capture("menu-view-hover-native.png")
        keys("-k", "Down")
        git = action_id("ide_activity:git")
        state_when(lambda value: value["focused"] == git, "Down did not continue from the pointer-selected row")
        assert_single_highlight(git)
        capture("menu-view-keyboard-native.png")
        keys("-k", "Return")
        state_when(lambda value: value["menus"]["open"] is None and value["menus"]["activity"] == "git", "Enter did not route the focused menu action")
        checks.append("checked rows do not retain hover fills; pointer and Down/Enter share one highlight and activate the focused destination")

        click("ide_menu:view")
        hover("ide_menu:file", covered=True)
        state_when(lambda value: value["menus"]["open"] == "file", "menubar hover did not switch category")
        keys("-k", "Right")
        state_when(lambda value: value["menus"]["open"] == "edit", "Right did not switch menubar category")
        keys("-k", "Escape")
        closed()
        click("ide_menu:view")
        click_point(1100, 700)
        closed()
        checks.append("open menubar follows adjacent pointer hover and Left/Right; Escape and outside click dismiss it")

        click("ide_menu:file")
        value = observation()
        save = next(label for label in value["menus"]["labels"] if label["text"] == "Guardar")
        click_point(save["position"][0]+save["bounds"][0]/2, save["position"][1]+save["bounds"][1]/2)
        assert observation()["menus"]["open"] == "file", "disabled Save activated or dismissed menu"
        assert observation()["editor"]["revision"] == baseline["revision"]
        click(action_id("ide_new_file"))
        state_when(lambda value: value["menus"]["new_file"] and value["focused"] == "ide_new_file_path", "enabled menu command did not open focused New File dialog")
        capture("menu-new-file-dialog-native.png")
        keys("-k", "Escape")
        state_when(lambda value: not value["menus"]["new_file"], "Escape did not cancel New File")
        checks.append("disabled clean-document Save is inert; enabled New File opens its focused dialog and cancels without creating a file")

        # The native editor right-click menu uses the same geometry and typography.
        editor_id = f'code_editor:{baseline["tab"]}'
        click(editor_id)
        pointer = _private_pointer()
        assert pointer is not None
        for down in (1, 0):
            assert active().get("pid") == process.pid
            pointer._send(pointer.pointer, 2, int(time.monotonic()*1000) & 0xffffffff, 273, down)
            pointer._send(pointer.pointer, 4)
            pointer._roundtrip()
        value = state_when(lambda value: value["menus"]["context_open"], "native right click did not open editor context menu")
        surface = value["regions"]["__context_menu:surface"]["rect"]
        assert surface[2] <= 360 and surface[3] <= 450
        assert contains([0, 0, *value["viewport"]], surface)
        capture("menu-editor-context-native.png")
        keys("-k", "Escape")
        state_when(lambda value: not value["menus"]["context_open"] and value["focused"] == editor_id,
                   "context menu Escape did not restore editor focus")
        checks.append("physical right-click opens a bounded editor context menu; Escape restores the original editor focus")

        # Long View menus remain bounded and scroll to their final action.
        resize(1400, 360)
        click("ide_menu:view")
        first = action_id("commands")
        capture("menu-view-short-initial-native.png")
        (options.output / "short-menu-initial.json").write_text(json.dumps(observation(), indent=2, ensure_ascii=False))
        short = observation()
        surface = short["regions"]["popover_surface:ide_menu_dismiss:view"]["rect"]
        anchor = short["regions"]["ide_menu:view"]["rect"]
        assert surface[1] >= anchor[1]+anchor[3], "short-window menu covers its menubar trigger"
        bottom = action_id("toggle_settings_header", required=False)
        assert bottom is None or not reachable(observation(), bottom), "short fixture should initially clip the final menu action"
        scroll(30, first)
        value = state_when(lambda value: action_id("toggle_settings_header", required=False) is not None, "View menu bottom was not reachable by scrolling")
        settings = action_id("toggle_settings_header")
        assert reachable(value, settings)
        menu_geometry(value)
        capture("menu-view-scrolled-native.png")
        keys("-k", "Escape")
        closed()
        checks.append("long menu is viewport-bounded and its final settings action is reachable through native scrolling")

        # Narrow titlebar uses an actual parent/submenu tree.
        resize(760, 640)
        click("ide_menu:all")
        hover("ide_menu_category:file")
        state_when(lambda value: action_id("ide_new_file", required=False) is not None, "category hover did not open submenu")
        capture("menu-compact-submenu-native.png")
        click(action_id("ide_new_file"))
        state_when(lambda value: value["menus"]["new_file"], "submenu click did not activate command")
        keys("-k", "Escape")
        state_when(lambda value: not value["menus"]["new_file"], "submenu dialog did not dismiss")
        click("ide_menu:all")
        keys("-k", "Down", "-k", "Right")
        state_when(lambda value: any(identifier.startswith("ide_menu_action:") for identifier in value["regions"]), "keyboard did not open compact submenu")
        keys("-k", "Left")
        child_closed = not any(identifier.startswith("ide_menu_action:") for identifier in observation()["regions"])
        keys("-k", "Escape")
        # Continue independent visual checks if this regression fails, while
        # preserving a nonzero suite result and explicit failed evidence.
        if not child_closed or observation()["menus"]["open"] is not None:
            failures.append("compact submenu Left then Escape did not close child and parent")
            capture("menu-compact-keyboard-dismiss-failure.png")
            click_point(700, 500)
            closed()
        else:
            checks.append("compact menu submenus open by pointer and keyboard and retain functional command routing and dismissal")

        # Closing the fixture's only source leaves the new empty editor surface.
        resize(1400, 900)
        tab = observation()["editor"]["tab"]
        click(f"dock_close:{tab}")
        state_when(lambda value: value["editor"] is None, "source tab did not close")
        for width, height in [(1400, 900), (680, 640)]:
            resize(width, height)
            value = observation()
            assert "Tu proyecto, abierto" not in " ".join(value["texts"])
            assert any(image["alt"] == "OpenIDE · editor vacío" for image in value["menus"]["images"]), "missing native editor watermark"
            for identifier in ("ide_empty_file", "ide_empty_commands", "ide_empty_terminal"):
                assert reachable(value, identifier), identifier
                row = value["regions"][identifier]["rect"]
                assert contains([0, 0, *value["viewport"]], row)
                caps = [paint for paint in value["menus"]["paint"] if paint["kind"] == "stroke"
                        and paint["radius"] == 4 and 22 <= paint["rect"][2] <= 60
                        and abs(paint["rect"][3]-20) < .1 and contains(row, paint["rect"])]
                assert len(caps) >= 2, (identifier, "missing native bordered shortcut keycaps", caps)
            for key in ("Ctrl", "+", "P", "Shift", "`"):
                assert key in value["texts"], f"missing boxed empty-editor shortcut: {key}"
            capture(f"menu-empty-editor-{width}-native.png")
        click("ide_empty_file")
        state_when(lambda value: value["menus"]["command_center"] and value["menus"]["command_scope"] == "Files", "empty editor file action did not open quick file search")
        capture("menu-empty-editor-file-search-native.png")
        keys("-k", "Escape")
        state_when(lambda value: not value["menus"]["command_center"], "file picker did not close")
        click("ide_empty_commands")
        state_when(lambda value: value["menus"]["command_center"], "empty editor commands action did not open command center")
        keys("-k", "Escape")
        state_when(lambda value: not value["menus"]["command_center"], "command center did not close")
        click("ide_empty_terminal")
        state_when(lambda value: value["terminal"]["visible"], "empty editor terminal action did not open bottom terminal")
        capture("menu-empty-editor-terminal-native.png")
        state_when(lambda value: value["terminal"]["running"], "fixture terminal did not start")
        click("terminal_menu_toggle")
        value = state_when(lambda value: reachable(value, "terminal_copy"), "terminal actions menu did not open")
        row = value["regions"]["terminal_copy"]["rect"]
        assert 20 <= row[3] <= 30 and row[2] <= 360
        assert contains([0, 0, *value["viewport"]], row)
        capture("menu-terminal-actions-native.png")
        keys("-k", "Escape")
        state_when(lambda value: "terminal_copy" not in value["regions"] and value["focused"] == "terminal_menu_toggle",
                   "terminal menu Escape did not return focus to its trigger")
        checks.append("terminal action menu shares compact command rows and returns trigger focus after Escape")
        assert source.read_text() == original
        assert not (root / "state/provider-credentials").exists()
        checks.append("empty editor remains visible after closing its last source at wide/narrow widths; all menu navigation preserves source bytes and uses no account credentials")
        assert not failures, failures
        report = {"passed": checks, "fixture": str(root), "geometry": geometry, **desktop_metadata()}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        (options.output / "failure.json").write_text(json.dumps({"passed": checks, "failed": failures, "error": str(error), "observation": observation()}, indent=2, ensure_ascii=False))
        if process and active().get("pid") == process.pid:
            capture("menu-failure.png")
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
