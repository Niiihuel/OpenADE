#!/usr/bin/env python3
"""Native Wayland → Rust browser host → Chromium → local HTTP fixture E2E.

Requires a build with editor-e2e, Hyprland, wtype, dotool, grim and wl-paste. Uses an
isolated marked data directory and verifies the fixture PID before every key.
No provider, account, existing browser profile or external website is used.
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
from native_desktop_fixture import desktop_metadata, read_native_state, region_point, move_cursor, pointer_button


def run(*args):
    return subprocess.check_output(["rtk", *map(str, args)], text=True)


def wait(check, label, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
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
    options.output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="openide-browser-desktop-e2e-", dir="/tmp"))
    (root / ".openide-editor-fixture").touch()
    (root / "project").mkdir()
    (root / "project/main.rs").write_text("fn main() {}\n")
    state = {"ready": False, "value": None}
    lock = threading.Lock()
    html = b'''<!doctype html><html><meta charset="utf-8"><title>OpenIDE browser E2E</title>
    <style>body{background:#181818;color:#eee;font:18px sans-serif;padding:32px}input{display:block;width:80%;padding:16px;font:18px sans-serif}p{line-height:1.6}</style>
    <h1>Navegador nativo de OpenIDE</h1><label for="value">Texto de prueba</label>
    <input id="value" autofocus><p id="echo"></p><p>Entrada y portapapeles de la ventana real.</p>
    <script>const input=document.querySelector('input');
    input.addEventListener('input',()=>{document.querySelector('#echo').textContent=input.value;fetch('/value',{method:'POST',body:input.value})});
    document.addEventListener('pointerdown',event=>{if(event.target!==input){event.preventDefault();input.focus()}});
    addEventListener('load',()=>{input.focus();fetch('/ready',{method:'POST'})});</script></html>'''

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(html)

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()
            with lock:
                if self.path == "/ready":
                    state["ready"] = True
                elif self.path == "/value":
                    state["value"] = body
            self.send_response(204)
            self.end_headers()

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    (root / "browser-url").write_text(f"http://127.0.0.1:{server.server_port}/")
    prior = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {w["id"] for w in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(i for i in range(90, 120) if i not in occupied)
    process = None
    checks = []

    def active():
        return json.loads(run("hyprctl", "activewindow", "-j"))

    def keys(*args):
        assert active().get("pid") == process.pid, "fixture lost focus; no input sent"
        subprocess.run(["rtk", options.wtype, *args], check=True)
        time.sleep(0.2)

    def chord(key):
        keys("-M", "ctrl", "-k", key, "-m", "ctrl")

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        width, height = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {width}x{height}", str(options.output / name)], check=True)

    def focus_page_input():
        window = active()
        assert window.get("pid") == process.pid, "fixture lost focus; no click sent"
        observed = wait(lambda: read_native_state(root, process.pid), "native frame observations unavailable")
        wait(lambda: "browser_viewport" in (read_native_state(root, process.pid) or {}).get("regions", {}), "browser viewport not painted")
        observed = read_native_state(root, process.pid)
        # Use actual retained viewport bounds, independent of dock chrome,
        # window size and DPI. The page fixture focuses its input on pointerdown.
        x, y = region_point(window, observed, "browser_viewport")
        move_cursor(run, active, process.pid, x, y)
        pointer_button(run, active, process.pid)
        time.sleep(0.5)

    def value_is(expected):
        with lock:
            return state["value"] == expected

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {workspace} }}))")
        log = open(options.output / "native-browser.log", "w")
        env = os.environ.copy()
        # Reproduce the user's detached Nix shell: its original temp is gone.
        for key in ("TMPDIR", "TMP", "TEMP"):
            env[key] = str(root / "deleted-shell-tmp")
        process = subprocess.Popen([str(options.binary.resolve()), "--browser-e2e", str(root)], stdout=log, stderr=subprocess.STDOUT, env=env)
        wait(lambda: active().get("pid") == process.pid, "fixture window did not open")
        wait(lambda: state["ready"], "browser failed to navigate through native host")
        time.sleep(2)
        capture("browser-initial-native.png")
        checks.append("native startup with stale TMPDIR → isolated Chromium → local HTTP navigation")
        focus_page_input()
        text = "".join(f"{n:02d} café " for n in range(32))
        keys(text)
        wait(lambda: value_is(text), "native rapid input lost or reordered text")
        checks.append("rapid native Unicode typing survives the bounded host queue")
        chord("a")
        chord("c")
        wait(lambda: run("wl-paste", "--no-newline") == text, "browser Copy did not reach system clipboard")
        checks.append("browser selection → native Copy event → system clipboard")
        chord("x")
        wait(lambda: value_is(""), "native confirmed Cut did not remove selected text")
        assert run("wl-paste", "--no-newline") == text
        checks.append("two-phase Cut preserves text in system clipboard before deletion")
        chord("v")
        wait(lambda: value_is(text), "native Paste failed to round-trip Unicode")
        checks.append("system clipboard → native Paste → Chromium preserves all text")
        time.sleep(1)
        capture("browser-clipboard-native.png")
        report = {"passed": checks, "fixture": str(root), **desktop_metadata(), "url": f"http://127.0.0.1:{server.server_port}/"}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        (options.output / "failure.json").unlink(missing_ok=True)
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        report = {"passed": checks, "fixture": str(root), "error": str(error), "state": state}
        (options.output / "failure.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        if process is not None and active().get("pid") == process.pid:
            capture("browser-failure-native.png")
        print(json.dumps(report, ensure_ascii=False), flush=True)
        raise
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        server.shutdown()
        if json.loads(run("hyprctl", "activeworkspace", "-j"))["id"] == workspace:
            run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {prior} }}))")


if __name__ == "__main__":
    main()
