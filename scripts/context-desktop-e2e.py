#!/usr/bin/env python3
"""Native Context popover acceptance in a private Wayland compositor only.

Launch through headless-desktop-fixture.py with an editor-e2e binary. A local
HTTP fixture reports known token usage; all actions use native pointer/keyboard.
No credentials, user workspaces, provider requests or global input are used.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from native_desktop_fixture import (
    _private_runtime, desktop_metadata, logical_point,
    move_cursor, pointer_button, read_native_state, region_point,
)


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
    assert _private_runtime() is not None, "Context E2E requires the private headless runner"
    options.output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="openide-context-desktop-e2e-", dir="/tmp"))
    (root / ".openide-editor-fixture").touch()
    project = root / "project"
    project.mkdir()
    (project / "main.rs").write_text("fn main() {}\n")
    (project / "AGENTS.md").write_text(
        "# Context desktop fixture\nUse only the synthetic local provider.\n"
        "Preserve exact reported usage and distinguish unknown from zero.\n"
    )
    for index in range(8):
        skill = project / f".agents/skills/context-{index}"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text(
            f"---\nname: context-{index}\ndescription: Synthetic context source {index}\n---\n"
            "This is local fixture data, never an attributed token measurement.\n"
        )
    server_state = {"requests": [], "errors": []}
    lock = threading.Lock()
    usages = [
        {"prompt_tokens": 5000, "completion_tokens": 1000, "total_tokens": 6000,
         "prompt_tokens_details": {"cached_tokens": 1200},
         "completion_tokens_details": {"reasoning_tokens": 250}},
        {"prompt_tokens": 18500, "completion_tokens": 500, "total_tokens": 19000},
    ]

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            body = json.dumps({"object": "list", "data": [{"id": "context-desktop-fixture",
                "object": "model", "context_window": 20000, "max_output_tokens": 4000}]}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            try:
                length = int(self.headers.get("Content-Length", 0))
                assert 0 < length <= 1024 * 1024
                request = json.loads(self.rfile.read(length))
                assert self.path == "/v1/chat/completions", self.path
                assert request["model"] == "context-desktop-fixture"
                with lock:
                    index = len(server_state["requests"])
                    assert index < len(usages), "unexpected model request"
                    server_state["requests"].append(request)
                content = f"Respuesta local de prueba {index + 1}: uso informado por el proveedor sintético."
                if request.get("stream"):
                    delta = {"id": f"context-fixture-{index}", "choices": [{"index": 0,
                        "delta": {"content": content}, "finish_reason": None}]}
                    done = {"id": f"context-fixture-{index}", "choices": [{"index": 0,
                        "delta": {}, "finish_reason": "stop"}], "usage": usages[index]}
                    body = f"data: {json.dumps(delta)}\n\ndata: {json.dumps(done)}\n\ndata: [DONE]\n\n".encode()
                    content_type = "text/event-stream"
                else:
                    body = json.dumps({"id": f"context-fixture-{index}", "choices": [{"index": 0,
                        "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
                        "usage": usages[index]}).encode()
                    content_type = "application/json"
                self.send_response(200)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(body)
            except Exception as error:
                with lock:
                    server_state["errors"].append(str(error))
                self.send_error(500, "Invalid fixture request")

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    (root / "context-fixture.json").write_text(json.dumps({
        "endpoint": f"http://127.0.0.1:{server.server_port}/v1",
    }))
    process = None
    log = None
    checks = []

    def active():
        return json.loads(run("hyprctl", "activewindow", "-j"))

    def observation():
        return read_native_state(root, process.pid) if process else None

    def state_when(predicate, label):
        def ready():
            observed = observation()
            return observed if observed and predicate(observed) else None
        return wait(ready, label)

    def keys(*args):
        assert active().get("pid") == process.pid, "fixture lost focus; no keys sent"
        subprocess.run(["rtk", options.wtype, *args], check=True)
        time.sleep(0.15)

    def click(region_id):
        observed = state_when(lambda value: region_id in value["regions"]
            and value["regions"][region_id]["hit"] == region_id, f"{region_id} is not reachable")
        x, y = region_point(active(), observed, region_id)
        move_cursor(run, active, process.pid, x, y)
        pointer_button(run, active, process.pid)
        time.sleep(0.15)

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        width, height = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {width}x{height}",
                        str(options.output / name)], check=True)

    def context_surface(observed):
        return observed["regions"]["popover_surface:composer_context_dismiss"]["rect"]

    def popup_ready(observed):
        return observed["context"]["open"] and "popover_surface:composer_context_dismiss" in observed["regions"]

    def assert_fits(observed):
        x, y, width, height = context_surface(observed)
        viewport_width, viewport_height = observed["viewport"]
        assert width > 0 and height > 0
        assert x >= -0.1 and y >= -0.1 and x + width <= viewport_width + 0.1
        assert y + height <= viewport_height + 0.1, "context popup extends outside the viewport"

    def circle_fraction(observed):
        x, y, width, height = observed["regions"]["composer_context_toggle"]["rect"]
        circles = [ring for ring in observed["context"]["rings"]
                   if x <= ring["rect"][0] + ring["rect"][2] / 2 <= x + width
                   and y <= ring["rect"][1] + ring["rect"][3] / 2 <= y + height]
        assert len(circles) == 1, f"expected exactly one painted context circle, got {circles}"
        return circles[0]["fraction"]

    def resize(width, height):
        assert active().get("pid") == process.pid
        # This IPC targets only the validated private compositor and active PID.
        run("hyprctl", "eval", f"local w = hl.get_active_window(); assert(w.pid == {process.pid}); "
            "hl.dispatch(hl.dsp.window.fullscreen_state({internal=0,client=0,window=w})); "
            "hl.dispatch(hl.dsp.window.float({action='set',window=w})); "
            f"hl.dispatch(hl.dsp.window.resize({{x={width},y={height},window=w}})); "
            "hl.dispatch(hl.dsp.window.center({window=w}))")
        return state_when(lambda value: abs(value["viewport"][0] - width) <= 2
                          and abs(value["viewport"][1] - height) <= 2, "native viewport resize failed")

    def send_message(text, expected):
        click("prompt")
        keys(text)
        click("run_toggle")
        observed = state_when(lambda value: not value["context"]["running"]
                              and value["context"]["used"] == expected
                              and any(ring["fraction"] is not None
                                      and abs(ring["fraction"] - expected / 20000) < 0.001
                                      for ring in value["context"]["rings"]),
                              f"reported usage {expected} was not projected from the native response")
        return observed

    try:
        env = os.environ.copy()
        for key in ("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "OPENROUTER_API_KEY"):
            env.pop(key, None)
        log = (options.output / "native-context.log").open("w")
        process = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)],
                                   stdout=log, stderr=subprocess.STDOUT, env=env)
        wait(lambda: active().get("pid") == process.pid, "context fixture did not receive focus")
        initial = state_when(lambda value: "composer_context_toggle" in value["regions"],
                             "context trigger did not become available")
        assert Path(initial["workspace"]) == project.resolve()
        assert initial["context"]["used"] is None and circle_fraction(initial) is None
        click("composer_context_toggle")
        unknown = state_when(popup_ready, "pointer did not open Context")
        assert_fits(unknown)
        capture("context-unknown-native.png")
        keys("-k", "Escape")
        state_when(lambda value: not value["context"]["open"]
                   and value["focused"] == "composer_context_toggle", "Escape did not restore trigger focus")
        keys("-k", "Return")
        state_when(popup_ready, "keyboard activation did not reopen Context")
        keys("-k", "Escape")
        checks.append("native pointer/keyboard open, unknown circle, Escape closes and restores focus")

        measured = send_message("Primera medición local del contexto.", 6000)
        assert measured["context"]["capacity"] == 20000
        assert abs(circle_fraction(measured) - 0.30) < 0.001
        click("composer_context_toggle")
        summary = state_when(popup_ready, "measured summary did not open")
        assert_fits(summary)
        capture("context-measured-native.png")
        # The first outside click dismisses; it must not submit the prompt.
        current = observation()
        px, py, pw, ph = context_surface(current)
        outside = next((point for point in [(20, 100), (current["viewport"][0] - 20, 100),
                                            (20, current["viewport"][1] - 20)]
                        if not (px <= point[0] <= px + pw and py <= point[1] <= py + ph)), None)
        assert outside is not None
        x, y = logical_point(active(), current, *outside)
        move_cursor(run, active, process.pid, x, y)
        pointer_button(run, active, process.pid)
        state_when(lambda value: not value["context"]["open"], "outside pointer click did not dismiss")
        assert len(server_state["requests"]) == 1
        checks.append("actual HTTP usage paints a 30% circle; outside dismissal preserves the session")

        high = send_message("Segunda medición local del contexto.", 19000)
        assert abs(circle_fraction(high) - 0.95) < 0.001
        click("composer_context_toggle")
        high = state_when(popup_ready, "high context summary did not open")
        assert_fits(high)
        capture("context-high-native.png")
        keys("-k", "Escape")
        resize(520, 420)
        click("composer_context_toggle")
        compact = state_when(popup_ready, "narrow context did not open")
        assert_fits(compact)
        capture("context-narrow-summary-native.png")
        keys("-k", "Escape")
        state_when(lambda value: not value["context"]["open"]
                   and value["focused"] == "composer_context_toggle", "narrow Escape lost trigger focus")
        checks.append("95% circle and native 520×420 viewport preserve summary and Escape")
        assert len(server_state["requests"]) == 2 and not server_state["errors"], server_state["errors"]
        report = {"passed": checks, "fixture": str(root), "pid": process.pid,
                  "http_requests": len(server_state["requests"]), "reported_usage": usages,
                  "native_viewport": observation()["viewport"], **desktop_metadata(), "fps_claim": None}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        (options.output / "failure.json").unlink(missing_ok=True)
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        report = {"passed": checks, "fixture": str(root), "error": str(error),
                  "observation": observation(), "provider_errors": server_state["errors"]}
        (options.output / "failure.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        if process and active().get("pid") == process.pid:
            capture("context-failure-native.png")
        print(json.dumps({"passed": checks, "error": str(error), "fixture": str(root)}, ensure_ascii=False), flush=True)
        raise
    finally:
        if process and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        server.shutdown()
        server.server_close()
        if log:
            log.close()


if __name__ == "__main__":
    main()
