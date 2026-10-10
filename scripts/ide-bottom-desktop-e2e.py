#!/usr/bin/env python3
"""Bottom-panel tab metrics and real PTY spacing on a private native desktop.

Uses physical pointer/keyboard input and the existing read-only, marker-gated
frame observer. No browser, account, network service or user desktop is needed.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time

from native_desktop_fixture import (
    _private_runtime, desktop_metadata, logical_point, move_cursor,
    pointer_button, read_native_state,
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


def center(rect):
    x, y, w, h = rect
    return x+w/2, y+h/2


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
    for path in options.output.glob("bottom-*.png"):
        path.unlink()
    root = Path(tempfile.mkdtemp(prefix="openide-bottom-e2e-", dir="/tmp"))
    (root / ".openide-editor-fixture").touch()
    (root / "menu-fixture.json").write_text("{}")
    project = root / "project"
    project.mkdir()
    source = project / "main.rs"
    original = "fn main() {\n    println!(\"Bottom panel fixture\");\n}\n"
    source.write_text(original)
    run("git", "-C", project, "init", "--quiet", "--initial-branch=fixture")
    previous_workspace = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {item["id"] for item in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(index for index in range(90, 120) if index not in occupied)
    checks, geometry = [], []
    process = None
    log = None
    titles = {"terminal": "Terminal", "problems": "Problemas", "output": "Salida", "console": "Consola"}

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

    def hover(identifier):
        value = state_when(lambda value: reachable(value, identifier), f"unreachable {identifier}")
        move_cursor(run, active, process.pid,
                    *logical_point(active(), value, *center(value["regions"][identifier]["rect"])))
        time.sleep(.14)

    def click(identifier):
        hover(identifier)
        pointer_button(run, active, process.pid)
        time.sleep(.15)

    def keys(*args):
        assert active().get("pid") == process.pid, "fixture lost focus; no keys sent"
        subprocess.run(["rtk", options.wtype, "-d", "10", *args], check=True)
        time.sleep(.15)

    def chord(key):
        keys("-M", "ctrl", "-k", key, "-m", "ctrl")

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        w, h = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {w}x{h}", str(options.output / name)], check=True)

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

    def active_terminal(value):
        return next((tab["id"] for tab in value["terminal"]["tabs"] if tab["active"]), None)

    def labels_in(value, rectangle):
        return [label for label in value["menus"]["labels"]
                if contains(rectangle, [*label["position"], *label["bounds"]])]

    def section_geometry(name, compact=False):
        value = observation()
        records = []
        for key, title in titles.items():
            identifier = f"ide_bottom:{key}"
            assert reachable(value, identifier), (name, "unreachable section", identifier)
            rect = value["regions"][identifier]["rect"]
            assert contains([0, 0, *value["viewport"]], rect), (name, identifier, rect)
            assert rect[3] >= 23.9, (name, "section click target too short", rect)
            entry = {"id": identifier, "rect": rect}
            fills = [paint for paint in value["menus"]["paint"]
                     if paint["kind"] == "rect" and paint["radius"] >= 4
                     and all(abs(a-b) <= .5 for a, b in zip(rect, paint["rect"]))]
            if fills:
                entry["background"] = fills[-1]["color"]
            if not compact:
                labels = [label for label in labels_in(value, rect) if label["text"] == title]
                assert len(labels) == 1, (name, title, "missing unclipped label", labels)
                label = labels[0]
                text_rect = [*label["position"], *label["bounds"]]
                dx, dy = [abs(a-b) for a, b in zip(center(rect), center(text_rect))]
                assert dx <= 1.5 and dy <= 1.5, (name, title, "label not centered", rect, text_rect, dx, dy)
                entry.update(text=text_rect, center_error=[dx, dy])
            records.append(entry)
        ys = [center(record["rect"])[1] for record in records]
        assert max(ys)-min(ys) <= 1, (name, "tab centers differ", records)
        for a, b in zip(records, records[1:]):
            assert a["rect"][0]+a["rect"][2] <= b["rect"][0]+.1, (name, "tab hit areas overlap", a, b)
        for identifier in ("terminal_new", "terminal_sessions_toggle", "terminal_panel_hide", "ide_bottom_hide"):
            if identifier in value["regions"]:
                rect = value["regions"][identifier]["rect"]
                assert abs(center(rect)[1]-ys[0]) <= 1.5, (name, "toolbar is not centered with sections", identifier, rect, ys)
        geometry.append({"name": name, "viewport": value["viewport"], "tabs": records})
        return value

    def shell_marker(marker, filename, screen="terminal_screen"):
        click(screen)
        destination = shlex.quote(str(project / filename))
        cwd_destination = shlex.quote(str(project / (filename+".cwd")))
        # Genuine PTY output homes and clears the screen. The first painted row
        # is deterministic without trimming whitespace produced by a real shell.
        keys(f"printf '%s' \"$$\" > {destination}; pwd -P > {cwd_destination}; printf '\\033[2J\\033[H{marker}\\nSECOND_LINE\\n'")
        keys("-k", "Return")
        wait(lambda: (project / filename).is_file(), f"PTY did not execute {filename}")
        assert Path((project / (filename+".cwd")).read_text().strip()) == project.resolve()
        state_when(lambda value: any(marker in label["text"] for label in value["menus"]["labels"]),
                   f"first PTY row was not painted: {marker}")
        return (project / filename).read_text().strip()

    def select_first_rows(screen, first_marker):
        # The second row distinguishes the compact 4 px origin from the former
        # 12 px pointer offset; near the first row both would clamp to row zero.
        for row, marker in enumerate((first_marker, "SECOND_LINE")):
            value = observation()
            rect = value["regions"][screen]["rect"]
            matches = [label for label in labels_in(value, rect) if label["text"] == marker]
            assert matches, ("selection marker not found", marker)
            label = matches[0]
            y = rect[1]+4+row*18+3
            x = label["position"][0]
            width = label["bounds"][0]
            move_cursor(run, active, process.pid, *logical_point(active(), value, x+.1, y))
            pointer_button(run, active, process.pid, "down")
            try:
                for fraction in (.3, .65, 1):
                    move_cursor(run, active, process.pid, *logical_point(active(), value, x+width*fraction, y))
                    time.sleep(.06)
            finally:
                pointer_button(run, active, process.pid, "up")
            selected = state_when(lambda value: value["terminal"].get("selection") == marker,
                                  f"native drag selected wrong row/columns: {marker}")
            geometry.append({"name": f"selection-row-{row+1}", "screen": screen,
                             "drag_y": y, "selection": selected["terminal"]["selection"],
                             "native_text_width": width})
        capture("bottom-first-rows-selection-native.png")

    def terminal_geometry(name, markers, compact=False):
        value = section_geometry(name, compact) if observation()["ide"] else observation()
        # One section/header row only. The first real terminal glyph must follow
        # the header within the small viewport inset, never after another row.
        header_id = "ide_bottom:terminal" if value["ide"] else next(
            identifier for identifier in value["regions"] if identifier.startswith("terminal_tab:"))
        header = value["regions"][header_id]["rect"]
        records = []
        for screen, marker in markers:
            assert reachable(value, screen), (name, "terminal body unreachable", screen)
            rect = value["regions"][screen]["rect"]
            labels = [label for label in labels_in(value, rect) if marker in label["text"]]
            assert labels, (name, "missing first-row marker", screen, marker)
            text = min(labels, key=lambda label: label["position"][1])
            inset = text["position"][1]-rect[1]
            after_tab = rect[1]-(header[1]+header[3])
            # Vertical padding is 4 px; allow the font's centering within its
            # 18 px cell, but reject the former 12 px all-sides padding.
            assert 0 <= inset <= 8, (name, "extra blank PTY row or old vertical padding", rect, text, inset)
            assert 0 <= after_tab <= 12, (name, "second header or dead vertical gap", header, rect, after_tab)
            records.append({"screen": screen, "rect": rect, "first_row_y": text["position"][1],
                            "body_inset": inset, "gap_after_tab": after_tab})
        assert max(record["rect"][1] for record in records)-min(record["rect"][1] for record in records) <= 1
        geometry.append({"name": name+"-grid", "viewport": value["viewport"], "panes": records})
        capture(f"bottom-{name}-native.png")

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{workspace={workspace}}}))")
        spec = importlib.util.spec_from_file_location("marketplace_fixture", Path(__file__).with_name("marketplace-desktop-e2e.py"))
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        env = fixture.fixture_environment(root)
        env.pop("OPENIDE_MARKETPLACE_DESKTOP_FIXTURE", None)
        env["OPENIDE_MENU_DESKTOP_FIXTURE"] = "1"
        env["OPENIDE_TERMINAL_SHELL"] = shutil.which("sh") or "/bin/sh"
        env["PS1"] = "fixture> "
        env["ENV"] = "/dev/null"
        for key in [key for key in env if key.lower().endswith("proxy")]:
            env.pop(key)
        env.update(HTTP_PROXY="http://127.0.0.1:9", HTTPS_PROXY="http://127.0.0.1:9", ALL_PROXY="http://127.0.0.1:9", NO_PROXY="127.0.0.1,localhost")
        log = open(options.output / "native-bottom.log", "w")
        process = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)], stdout=log, stderr=subprocess.STDOUT, env=env)
        wait(lambda: active().get("pid") == process.pid, "fixture did not receive focus")
        state_when(lambda value: value.get("editor") and value.get("menus"), "editor fixture did not load")
        resize(1400, 900)
        click("window_mode_toggle")
        state_when(lambda value: value["ide"], "IDE mode did not activate")
        chord("grave")
        first = state_when(lambda value: value["terminal"]["visible"] and value["terminal"]["running"], "terminal did not open")
        first_id = active_terminal(first)
        first_pid = shell_marker("FIRST_ROW", "first.pid")
        terminal_geometry("wide-terminal", [("terminal_screen", "FIRST_ROW")])
        checks.append("wide section labels and toolbar share their center; first PTY row follows one header")
        select_first_rows("terminal_screen", "FIRST_ROW")
        checks.append("native drag near the top of PTY rows one and two selects the exact text with the compact coordinate origin")

        expected = {"problems": "main.rs", "output": "Servicios del proyecto", "console": "La salida del adaptador aparecerá aquí"}
        for key, text in expected.items():
            click(f"ide_bottom:{key}")
            value = state_when(lambda value: not value["terminal"]["visible"]
                               and (key != "problems" or "ide_problems" in value["regions"])
                               and any(text in label for label in value["texts"]),
                               f"{key} did not route to its body")
            section_geometry("wide-"+key)
            hover("ide_bottom:terminal")
            value = state_when(lambda value: value["menus"]["hovered"] == "ide_bottom:terminal", "hover did not reach terminal tab")
            section_geometry("hover-"+key)
            backgrounds = {item["id"].split(":")[-1]: item.get("background") for item in geometry[-1]["tabs"]}
            assert backgrounds["terminal"] is not None, ("hover fill does not match tab hit area", backgrounds)
            inactive = [backgrounds[other] for other in titles if other not in ("terminal", key)]
            assert all(color != backgrounds["terminal"] for color in inactive), ("hover overlaps unrelated tabs", backgrounds)
            capture(f"bottom-wide-{key}-native.png")
        click("ide_bottom:terminal")
        state_when(lambda value: value["terminal"]["visible"] and active_terminal(value) == first_id, "switching sections replaced terminal")
        assert shell_marker("RESTORED_ROW", "restored.pid") == first_pid
        checks.append("all four section clicks route correctly; hover preserves centered labels and switching preserves the PTY")

        click("terminal_new")
        second = state_when(lambda value: len(value["terminal"]["tabs"]) == 2 and active_terminal(value) != first_id and value["terminal"]["running"], "new session not ready")
        second_id = active_terminal(second)
        second_pid = shell_marker("SECOND_ROW", "second.pid")
        assert first_pid != second_pid
        click("terminal_sessions_toggle")
        click(f"terminal_tab_menu:{first_id}")
        state_when(lambda value: active_terminal(value) == first_id and "RESTORED_ROW" in (value["terminal"]["text"] or ""), "sessions menu did not restore first terminal")
        terminal_geometry("session-switch", [("terminal_screen", "RESTORED_ROW")])
        checks.append("session selector switches independent PTYs without adding another header row")

        click("terminal_split")
        split = state_when(lambda value: len(value["terminal"]["panes"]) == 2 and all(pane["running"] for pane in value["terminal"]["panes"]), "split PTY not ready")
        split_ids = [pane["id"] for pane in split["terminal"]["panes"]]
        split_pids = []
        for index, identifier in enumerate(split_ids):
            split_pids.append(shell_marker(f"PANE_{index}_ROW", f"pane-{index}.pid", f"terminal_screen:{identifier}"))
        assert len(set(split_pids)) == 2
        terminal_geometry("split-terminal", [(f"terminal_screen:{identifier}", f"PANE_{index}_ROW") for index, identifier in enumerate(split_ids)])
        checks.append("split panes align their first row and route keyboard input to separate live PTYs")
        click("terminal_split")
        state_when(lambda value: not value["terminal"]["panes"] and reachable(value, "terminal_screen"), "unsplit did not retain one surface")

        resize(520, 640)
        shell_marker("NARROW_ROW", "narrow.pid")
        terminal_geometry("narrow-terminal", [("terminal_screen", "NARROW_ROW")], compact=True)
        for key in ("problems", "output", "console", "terminal"):
            click(f"ide_bottom:{key}")
            section_geometry("narrow-"+key, compact=True)
        state_when(lambda value: value["terminal"]["visible"], "compact terminal tab did not reopen body")
        click("terminal_sessions_toggle")
        value = state_when(lambda value: reachable(value, f"terminal_tab_menu:{second_id}"), "narrow sessions menu is unreachable")
        surface = value["regions"]["popover_surface:terminal_sessions_dismiss"]["rect"]
        assert contains([0, 0, *value["viewport"]], surface), ("narrow sessions popup escapes viewport", surface)
        capture("bottom-narrow-session-menu-native.png")
        keys("-k", "Escape")
        state_when(lambda value: value["focused"] == "terminal_sessions_toggle", "sessions Escape did not restore focus")
        checks.append("compact tabs remain reachable at 520px; session menu is bounded and restores focus")

        resize(1400, 900)
        click("terminal_panel_maximize_button")
        shell_marker("MAXIMIZED_ROW", "maximized.pid")
        terminal_geometry("maximized-terminal", [("terminal_screen", "MAXIMIZED_ROW")])
        click("terminal_panel_maximize_button")
        live_id = active_terminal(observation())
        live_pid = shell_marker("HIDE_ROW", "hide-before.pid")
        click("terminal_panel_hide")
        state_when(lambda value: not value["terminal"]["visible"] and "ide_bottom:terminal" not in value["regions"], "panel hide did not remove header and body")
        chord("grave")
        state_when(lambda value: value["terminal"]["visible"] and active_terminal(value) == live_id and value["focused"] == "terminal_screen", "reopen did not focus surviving terminal")
        assert shell_marker("REOPEN_ROW", "hide-after.pid") == live_pid
        terminal_geometry("reopened-terminal", [("terminal_screen", "REOPEN_ROW")])
        checks.append("maximize restores compact spacing; hide/reopen preserves the session and terminal keyboard focus")

        click("window_mode_toggle")
        state_when(lambda value: not value["ide"] and reachable(value, "terminal_screen"), "Agents terminal did not remain visible")
        shell_marker("AGENTS_ROW", "agents.pid")
        value = observation()
        tab_rects = [region["rect"] for identifier, region in value["regions"].items()
                     if identifier.startswith("terminal_tab:") and region["hit"] == identifier]
        assert tab_rects
        for rect in tab_rects:
            assert abs(rect[3]-28) <= 1, ("Agents session tab changed height", rect)
            for label in labels_in(value, rect):
                assert abs(center(rect)[1]-(label["position"][1]+label["bounds"][1]/2)) <= 1.5, (
                    "Agents session label is not vertically centered", rect, label)
        terminal_geometry("agents-session-strip", [("terminal_screen", "AGENTS_ROW")])
        checks.append("shared Agents session strip still uses one header before the PTY grid")
        assert source.read_text() == original
        assert not (root / "state/provider-credentials").exists()
        report = {"passed": checks, "fixture": str(root), "geometry": geometry, **desktop_metadata()}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        (options.output / "failure.json").write_text(json.dumps({"passed": checks, "geometry": geometry, "error": str(error), "observation": observation()}, indent=2, ensure_ascii=False))
        if process and active().get("pid") == process.pid:
            capture("bottom-failure.png")
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
