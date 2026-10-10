#!/usr/bin/env python3
"""Native editor structure checks on the private Wayland fixture only.

The observer is read-only. All folding, navigation, markers and edits originate
from real keyboard/pointer events; no debug adapter or provider is configured.
"""
import argparse
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
        time.sleep(0.04)
    raise AssertionError(label)


def fixture_source():
    lines = [
        "mod visual_fixture {",
        "    pub struct Runner;",
        "    impl Runner {",
        "        pub fn nested_case() -> usize {",
        "            let mut total = 0usize;",
        "            if true {",
        "                for number in 0..3 {",
        "                    total += number;",
    ]
    for index in range(150):
        lines.append("" if index % 17 == 0 else f"                    // Visible scope sample {index:03}")
    lines += [
        "                }", "            }", "            total", "        }", "    }", "}",
        "", "fn main() {", "    println!(\"{}\", visual_fixture::Runner::nested_case());", "}",
    ]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    assert _private_runtime() is not None, "use headless-desktop-fixture.py; user desktop input is disabled"
    options.output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="openide-editor-structure-e2e-", dir="/tmp"))
    (root / ".openide-editor-fixture").touch()
    project = root / "project"
    project.mkdir()
    source = project / "main.rs"
    original = fixture_source()
    source.write_text(original)
    (project / "Cargo.toml").write_text(
        '[package]\nname="structure_fixture"\nversion="0.1.0"\nedition="2021"\n'
        '[[bin]]\nname="fixture"\npath="main.rs"\n')
    function_row = next(index for index, line in enumerate(original.splitlines()) if "pub fn nested_case" in line)
    previous_workspace = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {entry["id"] for entry in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(index for index in range(90, 120) if index not in occupied)
    process = None
    log = None
    checks = []

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

    def reachable(value, region_id):
        return value["regions"].get(region_id, {}).get("hit") == region_id

    def point_at(region_id):
        observed = state_when(lambda value: reachable(value, region_id), f"{region_id} is not reachable")
        return region_point(active(), observed, region_id)

    def hover(region_id):
        x, y = point_at(region_id)
        move_cursor(run, active, process.pid, x, y)

    def click(region_id):
        hover(region_id)
        pointer_button(run, active, process.pid)
        time.sleep(0.15)

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        width, height = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {width}x{height}", str(options.output / name)], check=True)

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

    def scroll_editor(region_id, ticks):
        observed = observation()
        rect = observed["regions"][region_id]["rect"]
        x, y = logical_point(active(), observed, rect[0] + rect[2] * 0.6, rect[1] + rect[3] * 0.5)
        move_cursor(run, active, process.pid, x, y)
        pointer = _private_pointer()
        assert pointer is not None
        for _ in range(abs(ticks)):
            assert active().get("pid") == process.pid
            step = 1 if ticks > 0 else -1
            pointer._send(pointer.pointer, 5, 0)  # wheel source
            pointer._message(pointer.pointer, 7, struct.pack("=IIii",
                int(time.monotonic() * 1000) & 0xffffffff, 0, step * 15 * 256, step))
            pointer._send(pointer.pointer, 4)
            pointer._roundtrip()
            time.sleep(0.035)

    def unchanged(value, baseline):
        editor = value["editor"]
        assert editor["document"] == baseline["document"]
        assert editor["revision"] == baseline["revision"], "structure controls edited the document"
        assert not editor["dirty"] and not editor["text_truncated"]
        assert editor["text"] == original and source.read_text() == original

    def breakpoint_id(editor, row):
        return f'editor_breakpoint:{editor["tab"]}:{editor["document"]}:{editor["revision"]}:{row}'

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {workspace} }}))")
        log = open(options.output / "native-editor-structure.log", "w")
        process = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)],
                                   stdout=log, stderr=subprocess.STDOUT, env=os.environ.copy())
        wait(lambda: active().get("pid") == process.pid, "fixture window did not receive focus")
        initial = state_when(lambda value: value.get("editor")
                             and value["editor"]["structure_revision"] == [value["editor"]["document"], value["editor"]["revision"]]
                             and any(item["start"] == function_row for item in value["editor"]["fold_candidates"]),
                             "Tree-sitter did not publish real revision-matched fold candidates")
        baseline = initial["editor"]
        editor_id = f'code_editor:{baseline["tab"]}'
        fold = max((item for item in baseline["fold_candidates"] if item["start"] == function_row), key=lambda item: item["end"])
        function_scope = next(item for item in baseline["sticky_scopes"] if item["start"] == function_row)
        unchanged(initial, baseline)
        capture("editor-structure-initial-native.png")

        # Real keyboard selection exercises the contour across changing line
        # widths and an empty physical row. No fixture API writes selection.
        click(editor_id)
        chord("Home")
        keys(*[part for _ in range(8) for part in ("-k", "Right")])
        keys("-M", "shift", *[part for _ in range(18) for part in ("-k", "Down")], "-m", "shift")
        expected_selection = [8, sum(len(line) for line in original.splitlines(keepends=True)[:18]) + 8]
        selected = state_when(lambda value: value["editor"]["selection"] == expected_selection,
                              "Shift+Down did not preserve exact source selection endpoints")
        unchanged(selected, baseline)
        capture("editor-multiline-selection-native.png")
        wide_width, wide_height = selected["viewport"]
        narrow = resize(900, 640)
        assert narrow["editor"]["selection"] == expected_selection
        unchanged(narrow, baseline)
        state_when(lambda value: reachable(value, editor_id), "editor was lost after narrow resize")
        scroll_editor(editor_id, 6)
        scrolled_selection = state_when(lambda value: breakpoint_id(baseline, 0) not in value["regions"]
                                       and any(key.startswith("editor_breakpoint:") for key in value["regions"]),
                                       "narrow selection fixture did not scroll its source rows")
        assert scrolled_selection["editor"]["selection"] == expected_selection
        unchanged(scrolled_selection, baseline)
        capture("editor-multiline-selection-narrow-scrolled-native.png")
        resize(wide_width, wide_height)
        chord("Home")
        cleared_selection = state_when(lambda value: value["editor"]["selection"] == [0, 0]
                                       and reachable(value, fold["id"]),
                                       "selection/caret did not return to the original source start")
        unchanged(cleared_selection, baseline)
        checks.append("native Shift+Down preserves exact multiline selection endpoints through resize and scroll without changing text, revision or disk")

        click(fold["id"])
        folded = state_when(lambda value: [fold["start"], fold["end"]] in value["editor"]["folds"],
                            "fold chevron did not collapse its real source range")
        unchanged(folded, baseline)
        assert breakpoint_id(baseline, function_row + 1) not in folded["regions"], "folded body retained interactive gutter rows"
        capture("editor-folded-native.png")
        click(fold["id"])
        expanded = state_when(lambda value: not value["editor"]["folds"]
                              and breakpoint_id(baseline, function_row + 1) in value["regions"],
                              "fold chevron did not restore its body")
        unchanged(expanded, baseline)
        checks.append("native fold/unfold chevrons change visible rows without editing text, revision or disk")

        click(editor_id)
        selected = observation()["editor"]
        row = selected["caret_row"]
        keys("-k", "F9")
        marked = state_when(lambda value: row in value["editor"]["breakpoints"], "F9 did not mark the caret's physical line")
        unchanged(marked, baseline)
        assert any(marker["row"] == row and marker["state"] == "Pending"
                   for marker in marked["editor"]["debug_breakpoints"])
        assert marked["editor"]["debug_execution"] is None
        assert marked["debug"]["status"] == "Unconfigured" and not marked["debug"]["controls"]["stop"]
        marker_id = breakpoint_id(baseline, row)
        hover(marker_id)
        state_when(lambda value: "Punto de interrupción pendiente" in " ".join(" ".join(value["texts"]).split())
                   and "El adaptador todavía no confirmó este punto de interrupción." in " ".join(" ".join(value["texts"]).split()),
                   "pending breakpoint tooltip did not explain that the adapter has not confirmed it")
        capture("editor-pending-breakpoint-native.png")
        click(marker_id)
        cleared = state_when(lambda value: row not in value["editor"]["breakpoints"], "gutter click did not remove the pending marker")
        unchanged(cleared, baseline)
        click(marker_id)
        state_when(lambda value: row in value["editor"]["breakpoints"], "gutter click did not restore the local marker")
        checks.append("native F9 and gutter toggle an explicit pending marker; tooltip explains it is unconfirmed, with no configured session or execution line")

        scroll_editor(editor_id, 10)
        sticky = state_when(lambda value: reachable(value, function_scope["id"]), "scrolling did not pin the enclosing function header")
        scope_controls = [region for key, region in sticky["regions"].items() if key.startswith("editor_scope_jump:")]
        assert 1 <= len(scope_controls) <= 3, "sticky scope header count is not bounded"
        unchanged(sticky, baseline)
        capture("editor-sticky-scope-native.png")
        click(function_scope["id"])
        jumped = state_when(lambda value: value["editor"]["caret_row"] == function_row
                            and reachable(value, fold["id"]), "sticky header did not jump back to the source declaration")
        unchanged(jumped, baseline)
        capture("editor-sticky-jump-native.png")
        checks.append("native scrolling shows bounded real scope headers and clicking the function returns to its source without an edit")

        old_controls = {key for key in jumped["regions"] if key.startswith(("editor_fold_range:", "editor_scope_jump:", "editor_breakpoint:"))}
        chord("Home")
        keys(" ")
        edited = state_when(lambda value: value["editor"]["revision"] > baseline["revision"]
                            and value["editor"]["text"] == " " + original
                            and not value["editor"]["breakpoints"]
                            and not old_controls.intersection(value["regions"]),
                            "editing retained old revision-bound structure controls or breakpoint markers")
        assert edited["editor"]["dirty"] and source.read_text() == original
        assert edited["editor"]["structure_revision"] in (None, [edited["editor"]["document"], edited["editor"]["revision"]])
        chord("a")
        keys("incomplete")
        invalid = state_when(lambda value: value["editor"]["text"] == "incomplete"
                             and value["editor"]["structure_revision"] == [value["editor"]["document"], value["editor"]["revision"]]
                             and not value["editor"]["fold_candidates"] and not value["editor"]["sticky_scopes"],
                             "invalid replacement retained structure from a previous parse")
        assert not invalid["editor"]["folds"] and not invalid["editor"]["breakpoints"]
        assert not any(key.startswith(("editor_fold_range:", "editor_scope_jump:")) for key in invalid["regions"])
        assert source.read_text() == original, "unconfirmed draft changes reached disk"
        capture("editor-structure-invalidated-native.png")
        checks.append("native edits invalidate pending markers and old action identities; invalid syntax clears stale structure and preserves unsaved disk contents")

        report = {"passed": checks, "fixture": str(root), "pid": process.pid,
                  "source_lines": len(original.splitlines()), "debug_adapter": None,
                  **desktop_metadata(), "fps_claim": None}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        (options.output / "failure.json").unlink(missing_ok=True)
        (options.output / "editor-structure-failure-native.png").unlink(missing_ok=True)
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        report = {"passed": checks, "fixture": str(root), "error": str(error), "observation": observation()}
        (options.output / "failure.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        if process is not None and active().get("pid") == process.pid:
            capture("editor-structure-failure-native.png")
        print(json.dumps({"passed": checks, "fixture": str(root), "error": str(error)}, ensure_ascii=False), flush=True)
        raise
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        if log is not None:
            log.close()
        if json.loads(run("hyprctl", "activeworkspace", "-j"))["id"] == workspace:
            run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {previous_workspace} }}))")


if __name__ == "__main__":
    main()
