#!/usr/bin/env python3
"""Native contextual tooltip placement on a private compositor only.

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
    for path in options.output.glob("tooltip-*.png"):
        path.unlink()
    root = Path(tempfile.mkdtemp(prefix="openide-tooltip-e2e-", dir="/tmp"))
    (root / ".openide-editor-fixture").touch()
    (root / "menu-fixture.json").write_text("{}")
    project = root / "project"
    project.mkdir()
    source = project / "main.rs"
    original = "fn main() {\n    println!(\"Native tooltip fixture\");\n}\n"
    source.write_text(original)
    run("git", "-C", project, "init", "--quiet", "--initial-branch=fixture")
    previous_workspace = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {entry["id"] for entry in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(index for index in range(90, 120) if index not in occupied)
    checks, geometry = [], []
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

    def neutral():
        value = observation()
        # The fixture's document is only three lines, leaving this area empty.
        move_cursor(run, active, process.pid,
                    *logical_point(active(), value, value["viewport"][0]*.58, value["viewport"][1]*.58))
        time.sleep(.12)

    def tooltip(value, title):
        # Tooltips are painted after the application. Match actual measured
        # text inside the final surface, never infer placement from UI config.
        labels = [label for label in value["menus"]["labels"]
                  if label["size"] == 12.5 and label["text"] == title]
        for label in reversed(labels):
            for paint in reversed(value["menus"]["paint"]):
                body = paint["rect"]
                if (paint["kind"] == "rect" and paint["radius"] == 7 and
                    abs(label["position"][0] - body[0] - 12) < 1 and
                    contains(body, [*label["position"], *label["bounds"]])):
                    return {"body": body, "title": label}
        return None

    def check_tooltip(name, identifier, title, side, shortcut=False):
        neutral()
        state_when(lambda value: not tooltip(value, title), "previous tooltip did not hide")
        before = observation()
        focus = before["focused"]
        assert reachable(before, identifier), (identifier, before["regions"].get(identifier))
        anchor = before["regions"][identifier]["rect"]
        started = time.monotonic()
        move_cursor(run, active, process.pid, *logical_point(active(), before,
                    anchor[0]+anchor[2]/2, anchor[1]+anchor[3]/2))
        time.sleep(.08)
        assert not tooltip(observation(), title), f"{name}: tooltip ignored its hover delay"
        value = state_when(lambda value: tooltip(value, title), f"{name}: tooltip was not painted")
        elapsed = time.monotonic()-started
        measured = tooltip(value, title)
        body = measured["body"]
        anchor = value["regions"][identifier]["rect"]
        x, y, width, height = body
        ax, ay, aw, ah = anchor
        assert contains([0, 0, *value["viewport"]], body), (name, "clipped tooltip", body)
        relation = {"Right": x >= ax+aw+6, "Left": x+width <= ax-6,
                    "Below": y >= ay+ah+6, "Above": y+height <= ay-6}
        assert relation[side], (name, side, anchor, body)
        assert value["focused"] == focus, f"{name}: passive hint stole keyboard focus"
        assert reachable(value, identifier), f"{name}: passive tooltip covered trigger hit target"
        if shortcut:
            caps = [paint for paint in value["menus"]["paint"]
                    if paint["kind"] == "stroke" and paint["radius"] == 4
                    and abs(paint["rect"][3]-20) < .1
                    and contains(body, paint["rect"])]
            assert len(caps) >= 2, f"{name}: missing bordered shortcut keycaps"
        capture(f"tooltip-{name}-native.png")
        geometry.append({"name": name, "ide": value["ide"], "id": identifier,
                         "viewport": value["viewport"], "side": side,
                         "anchor": anchor, "body": body, "delay_seconds": round(elapsed, 3)})
        neutral()
        state_when(lambda value: not tooltip(value, title), f"{name}: pointer exit retained tooltip")
        checks.append(name)

    def check_editor_menu():
        click("editor_menu")
        value = state_when(lambda value: "popover_surface:editor_menu_close" in value["regions"],
                           "editor More menu did not open")
        seen = set()
        divided = set()
        targets = {"Siguiente coincidencia": ("Ctrl", "D"), "Completar código": ("Ctrl", "Space")}
        headings = {"Navegar", "Archivo", "Editar", "Selección", "Lenguaje", "Editor"}
        for _ in range(45):
            value = observation()
            surface = value["regions"]["popover_surface:editor_menu_close"]["rect"]
            assert contains([0, 0, *value["viewport"]], surface)
            assert surface[2] <= 360 and surface[3] <= 450
            for label in value["menus"]["labels"]:
                bounds = [*label["position"], *label["bounds"]]
                if not contains(surface, bounds) or bounds[1] < surface[1]+16 or bounds[1]+bounds[3] > surface[1]+surface[3]-8:
                    continue
                title = label["text"]
                if title in headings and title not in divided:
                    separators = [paint for paint in value["menus"]["paint"]
                                  if paint["kind"] == "rect" and 0 < paint["rect"][3] <= 1.5
                                  and paint["rect"][2] >= surface[2]-40
                                  and bounds[1]-18 <= paint["rect"][1] <= bounds[1]
                                  and contains(surface, paint["rect"])]
                    assert separators, ("missing divider before editor group", title, bounds)
                    divided.add(title)
                if title not in targets or title in seen:
                    continue
                center_y = bounds[1]+bounds[3]/2
                caps = [paint["rect"] for paint in value["menus"]["paint"]
                        if paint["kind"] == "stroke" and paint["radius"] == 4
                        and abs(paint["rect"][3]-20) < .1
                        and abs(paint["rect"][1]+10-center_y) <= 3
                        and paint["rect"][0] >= bounds[0]+bounds[2]+6
                        and contains(surface, paint["rect"])]
                assert len(caps) >= 2, ("missing separated keycaps", title, caps)
                cap_labels = {text["text"] for text in value["menus"]["labels"]
                              if any(contains(cap, [*text["position"], *text["bounds"]], 1.5) for cap in caps)}
                assert set(targets[title]).issubset(cap_labels), (title, cap_labels)
                seen.add(title)
                capture(f"tooltip-editor-menu-{len(seen)}-native.png")
            if seen == set(targets) and {"Selección", "Lenguaje"}.issubset(divided):
                break
            # Physical wheel input over the current real menu surface.
            move_cursor(run, active, process.pid, *logical_point(active(), value,
                        surface[0]+surface[2]/2, surface[1]+surface[3]/2))
            pointer = _private_pointer()
            assert pointer is not None
            pointer._send(pointer.pointer, 5, 0)
            pointer._message(pointer.pointer, 7, struct.pack("=IIii",
                int(time.monotonic()*1000) & 0xffffffff, 0, 45*256, 3))
            pointer._send(pointer.pointer, 4)
            pointer._roundtrip()
            time.sleep(.14)
        assert seen == set(targets), ("did not reach all editor shortcut rows", seen)
        assert {"Selección", "Lenguaje"}.issubset(divided), divided
        keys("-k", "Escape")
        state_when(lambda value: "popover_surface:editor_menu_close" not in value["regions"], "editor More did not dismiss")
        checks.append("editor-more-keycaps-and-group-dividers")

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
        log = open(options.output / "native-tooltip.log", "w")
        process = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)],
                                   stdout=log, stderr=subprocess.STDOUT, env=env)
        wait(lambda: active().get("pid") == process.pid, "fixture did not receive focus")
        state_when(lambda value: value.get("editor") and value.get("menus"), "editor fixture did not load")
        resize(1400, 900)
        click("window_mode_toggle")
        state_when(lambda value: value["ide"], "IDE mode did not activate")
        check_tooltip("ide-files-right", "ide_activity:files", "Archivos", "Right", True)
        check_tooltip("ide-file-row-right", "file_row:0", "main.rs", "Right")
        check_tooltip("ide-settings-right", "ide_settings", "Configuración", "Right")
        check_tooltip("ide-titlebar-below", "ide_titlebar_quick_open", "Abrir un archivo del proyecto", "Below", True)
        check_tooltip("ide-status-above", "ide_editor_language", "Rust · Cambiar modo de lenguaje", "Above")
        check_tooltip("ide-right-header-below", "ide_agent_toggle", "Ocultar agente", "Below")
        check_editor_menu()
        resize(520, 360)
        check_tooltip("ide-narrow-files-right", "ide_activity:files", "Archivos", "Right", True)
        check_tooltip("ide-bottom-edge-right", "ide_settings", "Configuración", "Right")
        check_tooltip("ide-top-edge-below", "ide_titlebar_quick_open", "Abrir un archivo del proyecto", "Below", True)
        resize(1400, 900)
        click("window_mode_toggle")
        state_when(lambda value: not value["ide"], "Agents mode did not activate")
        click("toggle_sidebar_reopen")
        state_when(lambda value: reachable(value, "toggle_settings"), "Agents sidebar did not expand")
        check_tooltip("agents-sidebar-below", "toggle_sidebar", "Ocultar barra lateral", "Below", True)
        check_tooltip("agents-settings-right", "toggle_settings", "Configuración", "Right")
        check_tooltip("agents-dock-below", "toggle_side_pane", "Ocultar panel derecho", "Below")
        check_tooltip("agents-mode-below", "window_mode_toggle", "Cambiar a la ventana IDE", "Below")
        check_tooltip("agents-context-above", "composer_context_toggle", "Uso de contexto", "Above")
        click("dock_add_tab")
        click("dock_open:files")
        state_when(lambda value: reachable(value, "file_row:0"), "Agents files tab did not open")
        check_tooltip("agents-file-row-left", "file_row:0", "main.rs", "Left")
        # A real selector against the left edge requests Left for its help.
        # With no room there, the engine must flip Right, outside the menu.
        click("toggle_side_pane")
        click("toggle_sidebar")
        resize(520, 640)
        click("composer:permissions")
        state_when(lambda value: reachable(value, "composer_permissions:read_only"), "permission selector did not open")
        check_tooltip("agents-edge-fallback-right", "composer_permissions:read_only", "Sólo lectura", "Right")
        popup = observation()["regions"]["popover_surface:dismiss:composer_permissions"]["rect"]
        body = geometry[-1]["body"]
        assert popup[0] < body[2]+16, ("fixture did not force the preferred Left side to run out of room", popup, body)
        assert body[0] >= popup[0]+popup[2]+6, ("flipped help overlaps the permission menu", popup, body)
        keys("-k", "Escape")
        assert source.read_text() == original
        assert not (root / "state/provider-credentials").exists()
        report = {"passed": checks, "fixture": str(root), "geometry": geometry, **desktop_metadata()}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        (options.output / "failure.json").write_text(json.dumps({"passed": checks, "geometry": geometry,
            "error": str(error), "observation": observation()}, indent=2, ensure_ascii=False))
        if process and active().get("pid") == process.pid:
            capture("tooltip-failure.png")
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
