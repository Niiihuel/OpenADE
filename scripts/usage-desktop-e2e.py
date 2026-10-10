#!/usr/bin/env python3
"""Synthetic account Usage and task charts on the private Wayland fixture only.

No credentials are written. External-account buttons are measured but
never activated; their URL dispatch is covered by host/component tests.
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
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


def fixture_environment(root):
    # Do not inherit any provider/CLI authentication into the synthetic process.
    env = {key: value for key, value in os.environ.items()
           if not any(part in key.upper() for part in ("API_KEY", "TOKEN", "SECRET", "PASSWORD"))}
    for key in ("OPENIDE_MODEL", "OPENIDE_API_BASE"):
        env.pop(key, None)
    env["OPENIDE_USAGE_DESKTOP_FIXTURE"] = "1"
    # The normal host probes GitHub during construction, before the visual
    # bootstrap. Deny only its external credential probes in this child.
    # Neither gh's saved accounts nor the system credential manager are read.
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


def contains(outer, inner, tolerance=1):
    x, y, width, height = outer
    a, b, w, h = inner
    return a >= x - tolerance and b >= y - tolerance and a + w <= x + width + tolerance and b + h <= y + height + tolerance


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    assert _private_runtime() is not None, "private compositor required; user desktop input disabled"
    options.output.mkdir(parents=True, exist_ok=True)
    (options.output / "report.json").unlink(missing_ok=True)
    (options.output / "resize-requests.jsonl").unlink(missing_ok=True)
    previous_workspace = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {entry["id"] for entry in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(index for index in range(90, 120) if index not in occupied)
    process = None
    log = None
    root = None
    checks, fixtures, geometry = [], [], []
    expected_actions = set()

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
        value = state_when(lambda state: reachable(state, region_id), f"unreachable: {region_id}")
        move_cursor(run, active, process.pid, *region_point(active(), value, region_id))

    def click(region_id):
        hover(region_id)
        pointer_button(run, active, process.pid)
        time.sleep(0.15)

    def keys(*args):
        assert active().get("pid") == process.pid, "fixture lost focus; no keys sent"
        subprocess.run(["rtk", options.wtype, *args], check=True)
        time.sleep(0.15)

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        width, height = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {width}x{height}", str(options.output / name)], check=True)

    def resize(width, height):
        window = active()
        assert window.get("pid") == process.pid
        # This compositor's float dispatcher toggles even with action='set'.
        # Query its actual state so repeated responsive sizes remain floating.
        float_command = "" if window["floating"] else "hl.dispatch(hl.dsp.window.float({window=w})); "
        resize_result = run("hyprctl", "eval", f"local w=hl.get_active_window(); assert(w.pid=={process.pid}); "
            "hl.dispatch(hl.dsp.window.fullscreen_state({internal=0,client=0,window=w})); "
            + float_command +
            f"hl.dispatch(hl.dsp.window.resize({{x={width},y={height},window=w}})); "
            "hl.dispatch(hl.dsp.window.center({window=w}))")
        with (options.output / "resize-requests.jsonl").open("a") as record:
            record.write(json.dumps({"requested": [width, height], "result": resize_result, "window": active()}) + "\n")
        state_when(lambda value: abs(value["viewport"][0] - width) <= 2
                   and abs(value["viewport"][1] - height) <= 2, "native resize failed")
        time.sleep(0.2)

    def scroll(ticks):
        value = observation()
        rect = value["regions"]["settings_page:Usage"]["rect"]
        x, y = logical_point(active(), value, rect[0] + rect[2] * .75, rect[1] + rect[3] * .6)
        move_cursor(run, active, process.pid, x, y)
        pointer = _private_pointer()
        assert pointer is not None
        for _ in range(abs(ticks)):
            assert active().get("pid") == process.pid
            direction = 1 if ticks > 0 else -1
            pointer._send(pointer.pointer, 5, 0)
            pointer._message(pointer.pointer, 7, struct.pack("=IIii",
                int(time.monotonic() * 1000) & 0xffffffff, 0, direction * 15 * 256, direction))
            pointer._send(pointer.pointer, 4)
        pointer._roundtrip()
        time.sleep(0.2)

    def visible_actions(value):
        found = {}
        for action in expected_actions:
            region = value["regions"].get(action)
            if not region or region["hit"] != action:
                continue
            rect = region["rect"]
            if rect[3] < 35.5:
                continue
            assert abs(rect[2] - 148) <= .1 and abs(rect[3] - 36) <= .1, (action, rect)
            assert contains([0, 0, *value["viewport"]], rect), (action, rect)
            cards = [paint["rect"] for paint in value["usage"]["paint"]
                     if paint["radius"] >= 8 and paint["rect"][2] > rect[2] + 24
                     and paint["rect"][3] > 72 and contains(paint["rect"], rect)]
            assert cards, f"action has no enclosing painted account card: {action}"
            card = min(cards, key=lambda entry: entry[2] * entry[3])
            found[action] = {"action": rect, "card": card}
        return found

    def scan(width, empty=False):
        scroll(-100)
        seen = {}
        chart = None
        captured_top = False
        for _ in range(64):
            value = observation()
            assert value["usage"]["page"] == "Usage" and not value["usage"]["worker_running"]
            current = visible_actions(value)
            if current and not captured_top:
                capture(f"usage-cards-{width}-top.png")
                captured_top = True
            for left in current.values():
                for right in current.values():
                    if abs(left["card"][1] - right["card"][1]) < 1:
                        assert abs(left["card"][3] - right["card"][3]) < 1, "same-row account cards differ in height"
                        assert abs(left["action"][1] - right["action"][1]) < 1, "same-row footers differ in baseline"
            for action, measurements in current.items():
                seen[action] = measurements
            if len(seen) == len(expected_actions) and chart is None:
                capture(f"usage-cards-{width}.png")
            if empty:
                if "0 tareas visibles" in value["texts"] and "No hay tareas en esta vista" in value["texts"]:
                    assert not any(key.startswith("usage-tasks:") for key in value["regions"])
                    chart = value
            elif "6 tareas visibles" in value["texts"] and all(
                    reachable(value, f"usage-tasks:{backend}")
                    and value["regions"][f"usage-tasks:{backend}"]["rect"][3] >= 219.5
                    for backend in ("native", "codex", "claude")):
                chart = value
            if len(seen) == len(expected_actions) and chart is not None:
                break
            scroll(4)
        assert set(seen) == expected_actions, f"not all account actions reachable at {width}px: {seen.keys()}"
        assert chart is not None, f"task chart not reachable at {width}px"
        geometry.append({"width": width, "empty": empty, "accounts": seen})
        if empty:
            assert "No hay tareas en esta vista" in chart["texts"]
            capture(f"usage-empty-{width}.png")
            return
        assert Counter(chart["usage"]["tasks"]) == {"native": 3, "codex": 2, "claude": 1}
        assert "6 tareas visibles" in chart["texts"]
        columns = [chart["regions"][f"usage-tasks:{backend}"]["rect"] for backend in ("native", "codex", "claude")]
        assert columns[0][0] < columns[1][0] < columns[2][0], "task categories are not ordered columns"
        bars = []
        for column in columns:
            candidates = [paint["rect"] for paint in chart["usage"]["paint"]
                          if paint["kind"] == "corner" and 0 < paint["rect"][2] <= 48
                          and paint["rect"][3] > 0 and contains(column, paint["rect"])]
            assert len(candidates) == 1, f"task column must paint one measured bar: {candidates}"
            bars.append(candidates[0])
        baseline = bars[0][1] + bars[0][3]
        assert all(abs(bar[1] + bar[3] - baseline) <= .1 for bar in bars), "task bars do not share a zero baseline"
        assert abs(bars[0][3] / bars[2][3] - 3) < .01 and abs(bars[1][3] / bars[2][3] - 2) < .01, "painted task heights disagree with 3/2/1 history"
        geometry[-1]["bars"] = bars
        capture(f"usage-chart-{width}.png")
        for backend, name, count in [("native", "Agente nativo", 3), ("codex", "Codex", 2),
                                     ("claude", "Agente con un nombre deliberadamente largo", 1)]:
            hover(f"usage-tasks:{backend}")
            suffix = "tarea" if count == 1 else "tareas"
            state_when(lambda value: name in " ".join(" ".join(value["texts"]).split())
                       and f"{count} {suffix} en el historial visible" in " ".join(" ".join(value["texts"]).split()),
                       f"chart hover did not expose the complete agent/count at {width}px")
        capture(f"usage-chart-hover-{width}.png")

    def launch(empty):
        nonlocal root, process, log
        root = Path(tempfile.mkdtemp(prefix="openide-usage-e2e-", dir="/tmp"))
        (root / ".openide-editor-fixture").touch()
        (root / "project").mkdir()
        (root / "project/main.rs").write_text("fn main() {}\n")
        (root / "usage-fixture.json").write_text(json.dumps({"empty": empty}))
        log_path = options.output / f"native-usage-{len(fixtures) + 1}-{'empty' if empty else 'populated'}.log"
        log = open(log_path, "w")
        process = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)],
                                   stdout=log, stderr=subprocess.STDOUT, env=fixture_environment(root))
        wait(lambda: active().get("pid") == process.pid, "fixture did not receive focus")
        initial = state_when(lambda value: value.get("usage") and value["usage"]["page"] == "Usage", "synthetic Usage bootstrap not active")
        providers = set(initial["usage"]["providers"])
        assert {"codex", "openrouter", "opencode", "nvidia", "minimax", "zai"} <= providers
        expected_actions.clear()
        expected_actions.update(
            f'provider_usage_{"account" if provider in {"codex", "openrouter", "opencode"} else "connect"}:{provider}'
            for provider in providers)
        fixtures.append({"path": str(root), "pid": process.pid, "empty": empty, "log": str(log_path)})

    def shutdown():
        nonlocal process, log
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        if log:
            log.close()
        process = None
        log = None

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{workspace={workspace}}}))")
        launch(False)
        for width, height in [(1600, 1000), (1000, 820), (640, 700), (520, 640)]:
            resize(width, height)
            scan(width)
        checks.append("native Usage shows all six synthetic providers with 148×36 action hitboxes, aligned same-row cards/footers and reachable controls from 1600 to 520px")
        checks.append("real task-history categories 3/2/1 render ordered columns and complete hover labels/counts at wide and narrow widths")
        for provider in ["nvidia", "minimax", "zai"]:
            if provider != "nvidia":
                # A fresh isolated fixture avoids a settings-route refresh
                # replacing synthetic snapshots between independent forms.
                shutdown()
                launch(False)
                resize(640, 700)
            scroll(-100)
            for _ in range(64):
                if reachable(observation(), f"provider_usage_connect:{provider}"):
                    break
                scroll(4)
            click(f"provider_usage_connect:{provider}")
            state_when(lambda value: value["usage"]["provider_connect"] == provider
                       and not value["usage"]["worker_running"] and "provider_api_key" in value["regions"],
                       f"Connect did not open the native credential-free {provider} form")
            capture(f"usage-connect-{provider}.png")
            click("provider_connect_cancel")
            state_when(lambda value: value["usage"]["provider_connect"] is None, "connection form did not close")
            assert not (root / "state/provider-credentials").exists(), "fixture unexpectedly wrote credentials"
        checks.append("NVIDIA, MiniMax and Z.ai Connect route to native key forms and cancel without credentials/authentication workers; external-account actions are not activated")
        shutdown()
        launch(True)
        resize(640, 700)
        scan(640, empty=True)
        checks.append("empty real task history displays zero and an empty state without fabricated task columns")
        report = {"passed": checks, "fixtures": fixtures, "geometry": geometry, **desktop_metadata(), "fps_claim": None}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        (options.output / "failure.json").unlink(missing_ok=True)
        (options.output / "usage-failure.png").unlink(missing_ok=True)
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        (options.output / "failure.json").write_text(json.dumps({"passed": checks, "fixtures": fixtures,
            "error": str(error), "observation": observation()}, indent=2, ensure_ascii=False))
        if process and active().get("pid") == process.pid:
            capture("usage-failure.png")
        print(json.dumps({"passed": checks, "error": str(error)}, ensure_ascii=False), flush=True)
        raise
    finally:
        shutdown()
        if json.loads(run("hyprctl", "activeworkspace", "-j"))["id"] == workspace:
            run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{workspace={previous_workspace}}}))")


if __name__ == "__main__":
    main()
