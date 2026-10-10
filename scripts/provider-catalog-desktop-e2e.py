#!/usr/bin/env python3
"""Passive plan badge and Codex catalog on an isolated native fixture.

Refresh intentionally has no credentials. No account or inference is contacted.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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

    def scroll(ticks, region_id=None):
        value = observation()
        rect = value["regions"][region_id or f'settings_page:{value["usage"]["page"]}']["rect"]
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

    def text(value):
        return " ".join(" ".join(value["texts"]).split())

    def measure_badge():
        value = observation()
        label = next(item for item in value["usage"]["labels"] if item["text"] == "Plan de ChatGPT")
        x, y = label["position"]
        badges = [paint["rect"] for paint in value["usage"]["paint"]
                  if paint["kind"] == "stroke" and abs(paint["radius"] - 10) < .1
                  and abs(paint["rect"][3] - 20) < .1
                  and contains(paint["rect"], [x, y, 1, 1])]
        assert len(badges) == 1, f"plan must have one compact rounded badge: {badges}"
        badge = badges[0]
        assert 60 < badge[2] < 220 and label["size"] == 11
        assert contains([0,0,*value["viewport"]], badge), "badge is not fully visible"
        assert abs(badge[2] - label["bounds"][0] - 16) < .5, "badge is not sized to its native text"
        assert contains(badge, [*label["position"], *label["bounds"]], tolerance=.5)
        center = [badge[0] + badge[2]/2, badge[1] + badge[3]/2, 0, 0]
        assert not any(region["kind"] == "Button" and contains(region["rect"], center)
                       for region in value["regions"].values()), "plan badge is interactive"
        assert not any(paint["kind"] == "rect" and paint["color"][3] > 0
                       and all(abs(a-b) < .2 for a,b in zip(paint["rect"], badge))
                       for paint in value["usage"]["paint"]), "plan regained a solid button-like fill"
        return badge

    model_ids = ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]

    def catalog_geometry(width):
        scroll(-100)
        found = {}
        seen_buttons = set()
        for _ in range(40):
            value = observation()
            for model in model_ids:
                details = value["regions"].get(f"catalog_model_details:{model}")
                name = next((label for label in value["usage"]["labels"] if label["text"] == model), None)
                if details and name and details["rect"][3] >= 30:
                    found[model] = name["position"][1]
                region = value["regions"].get(f"catalog_select:codex/{model}")
                if region and region["kind"] == "Button" and region["rect"][3] >= 30:
                    rect = region["rect"]
                    assert contains([0,0,*value["viewport"]], rect), (model,rect)
                    labels = [label for label in value["usage"]["labels"] if label["text"] == "Usar"
                              and contains(rect, [*label["position"], 1, 1])]
                    assert len(labels) == 1, f"Usar label clipped or detached: {model}/{rect}"
                    assert contains(rect, [*labels[0]["position"], *labels[0]["bounds"]], tolerance=.25), f"native Usar glyph box escapes: {labels[0]}/{rect}"
                    assert rect[2] >= labels[0]["bounds"][0] + 15.5 and rect[3] >= 32
                    seen_buttons.add(model)
                    geometry.append({"width":width,"model":model,"button":rect})
            # All four fit in the model list. Their visible y order must match
            # source listing_index, even when the last model is selected.
            if len(found) == 4 and all(f"catalog_model_details:{model}" in value["regions"] for model in model_ids):
                visible = [next(label["position"][1] for label in value["usage"]["labels"] if label["text"] == model)
                           for model in model_ids]
                assert visible == sorted(visible), f"server order changed: {visible}"
                assert seen_buttons == set(model_ids[:-1]), f"expected three usable model controls: {seen_buttons}"
                selected = [label for label in value["usage"]["labels"] if label["text"] == "En uso"]
                assert len(selected) == 1, "selected model status is missing or truncated"
                selected_rect = [*selected[0]["position"], *selected[0]["bounds"]]
                outlines = [paint["rect"] for paint in value["usage"]["paint"]
                            if paint["kind"] == "stroke" and abs(paint["rect"][3] - 32) < .2
                            and contains(paint["rect"], selected_rect, tolerance=.25)]
                assert len(outlines) == 1 and contains([0,0,*value["viewport"]], outlines[0]), "En uso native text does not fit its outline"
                assert not reachable(value, f"catalog_select:codex/{model_ids[-1]}"), "selected model is still actionable"
                geometry.append({"width":width,"selected_label":selected_rect,"selected_outline":outlines[0]})
                capture(f"codex-models-{width}.png")
                return
            scroll(3)
        raise AssertionError(f"all Codex model rows were not reachable at {width}px")

    proxy_requests = []
    class DenyExternal(BaseHTTPRequestHandler):
        def do_CONNECT(self):
            proxy_requests.append(self.path)
            self.send_error(503, "isolated visual fixture: external providers disabled")
        def do_GET(self):
            proxy_requests.append(self.path)
            self.send_error(503, "isolated visual fixture: external providers disabled")
        def log_message(self, *_args):
            pass
    proxy = ThreadingHTTPServer(("127.0.0.1",0), DenyExternal)
    threading.Thread(target=proxy.serve_forever, daemon=True).start()

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{workspace={workspace}}}))")
        root = Path(tempfile.mkdtemp(prefix="openide-catalog-e2e-", dir="/tmp"))
        (root / ".openide-editor-fixture").touch()
        (root / "project").mkdir()
        (root / "project/main.rs").write_text("fn main() {}\n")
        (root / "usage-fixture.json").write_text(json.dumps({"empty":True,"catalog":True}))
        env = fixture_environment(root)
        for key in [key for key in env if key.lower().endswith("proxy")]:
            env.pop(key)
        endpoint = f"http://127.0.0.1:{proxy.server_port}"
        env.update(HTTP_PROXY=endpoint, HTTPS_PROXY=endpoint, ALL_PROXY=endpoint, NO_PROXY="")
        log = open(options.output / "native-catalog.log", "w")
        process = subprocess.Popen([str(options.binary.resolve()),"--editor-e2e",str(root)],
                                   stdout=log,stderr=subprocess.STDOUT,env=env)
        wait(lambda: active().get("pid") == process.pid, "fixture did not receive focus")
        state_when(lambda value: value.get("usage") and value["usage"]["catalog"]["status"], "catalog fixture not bootstrapped")
        for width,height in [(1000,820),(520,640)]:
            resize(width,height)
            scroll(-100)
            badge = measure_badge()
            geometry.append({"width":width,"plan_badge":badge})
            capture(f"codex-plan-badge-{width}.png")
        checks.append("plan badge is passive, 20px high, round and naturally sized without a solid button background at 1000/520px")
        click("settings_nav_toggle")
        click("settings_page:providers")
        click("catalog_browse:codex")
        state_when(lambda value: value["usage"]["catalog"]["provider"] == "codex", "Codex page did not open")
        for width,height in [(1000,820),(520,640)]:
            resize(width,height)
            scroll(-100)
            for _ in range(25):
                current=observation()
                if "Catálogo de tu cuenta" in text(current) and "Actualizado ·" in text(current):
                    break
                scroll(2)
            assert "Catálogo de tu cuenta" in text(current) and "Actualizado ·" in text(current)
            capture(f"codex-source-{width}.png")
            catalog_geometry(width)
            for _ in range(30):
                current = observation()
                labels = [label for label in current["usage"]["labels"] if label["text"] == "Plan de ChatGPT"]
                if labels and labels[0]["position"][1] >= 10 and labels[0]["position"][1] + 20 <= current["viewport"][1]:
                    badge = measure_badge()
                    geometry.append({"width":width,"codex_page_plan_badge":badge})
                    capture(f"codex-page-plan-badge-{width}.png")
                    break
                scroll(3)
            else:
                raise AssertionError(f"Codex account plan card was not reachable at {width}px")
        checks.append("Codex preserves official source order with last model selected; Usar labels and hitboxes remain inside rows at 1000/520px")
        checks.append("synthetic verified source and last-check timestamp are visible on the native provider page")
        before=observation()["usage"]["catalog"]["status"]
        scroll(-100)
        click("catalog_refresh")
        updated=state_when(lambda value: not value["usage"]["catalog"]["refreshing"]
                           and value["usage"]["catalog"]["status"]["attempted_at_ms"] > before["attempted_at_ms"],
                           "credential-free refresh never completed")
        status=updated["usage"]["catalog"]["status"]
        assert status["source"] == "ReviewedSeed" and status["verified_at_ms"] == 0 and status["last_error"]
        for _ in range(25):
            current=observation()
            if "Catálogo sin verificar" in text(current):
                break
            scroll(2)
        assert "Catálogo sin verificar" in text(current)
        assert "Actualizado ·" not in text(current)
        capture("codex-refresh-no-credentials.png")
        assert not (root/"state/provider-credentials").exists()
        checks.append("real refresh without credentials invalidates the synthetic account listing and reports unverified/error state instead of claiming success")
        report={"passed":checks,"fixture":str(root),"pid":process.pid,"geometry":geometry,
                "blocked_external_hosts":sorted(set(proxy_requests)),**desktop_metadata(),"fps_claim":None}
        (options.output/"report.json").write_text(json.dumps(report,indent=2,ensure_ascii=False))
        (options.output/"failure.json").unlink(missing_ok=True)
        (options.output/"catalog-failure.png").unlink(missing_ok=True)
        print(json.dumps(report,ensure_ascii=False))
    except Exception as error:
        (options.output/"failure.json").write_text(json.dumps({"passed":checks,"error":str(error),"observation":observation()},indent=2,ensure_ascii=False))
        if process and active().get("pid") == process.pid:
            capture("catalog-failure.png")
        raise
    finally:
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        if log:
            log.close()
        proxy.shutdown()
        proxy.server_close()
        if json.loads(run("hyprctl","activeworkspace","-j"))["id"] == workspace:
            run("hyprctl","eval",f"hl.dispatch(hl.dsp.focus({{workspace={previous_workspace}}}))")


if __name__ == "__main__":
    main()
