#!/usr/bin/env python3
"""Exercise an explicitly supplied debugpy or LLDB adapter on a private fixture.

This is protocol validation, not the application UI test. No project is loaded.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import tempfile
import threading
import time


SOURCE = """import time
def compute(value):
    result = value + 1
    return result
counter = 3
counter = compute(counter)
print(f"counter={counter}", flush=True)
while True:
    time.sleep(0.05)
"""
RUST_SOURCE = """use std::{thread, time::Duration};
fn compute(value: i32) -> i32 {
    let result = value + 1;
    result
}
fn main() {
    let mut counter = 3;
    counter = compute(counter);
    println!("counter={counter}");
    loop { thread::sleep(Duration::from_millis(50)); }
}
"""


def main():
    parser = argparse.ArgumentParser()
    adapters = parser.add_mutually_exclusive_group(required=True)
    adapters.add_argument("--python", type=Path)
    adapters.add_argument("--lldb-dap", type=Path)
    parser.add_argument("--rustc", default="rustc")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="openide-dap-probe-", dir="/tmp"))
    rust = args.lldb_dap is not None
    source = root / ("main.rs" if rust else "main.py")
    source.write_text(RUST_SOURCE if rust else SOURCE)
    interpreter = str((args.lldb_dap if rust else args.python).absolute())
    if rust:
        version = subprocess.check_output([interpreter, "--version"], text=True).strip()
        program = root / "fixture"
        subprocess.run(["rtk", args.rustc, "-g", "-C", "opt-level=0", str(source), "-o", str(program)], check=True)
        adapter_args = [interpreter]
        launch_args = {"program": str(program), "cwd": str(root), "stopOnEntry": False, "disableASLR": False}
    else:
        version = subprocess.check_output([interpreter, "-c", "import debugpy;print(debugpy.__version__)"], text=True).strip()
        adapter_args = [interpreter, "-m", "debugpy.adapter"]
        launch_args = {"name": "Python fixture", "type": "python", "request": "launch",
            "program": str(source), "cwd": str(root), "python": [interpreter],
            "console": "internalConsole", "redirectOutput": True, "justMyCode": True,
            "subProcess": False, "stopOnEntry": False}
    call_line, print_line = (8, 9) if rust else (6, 7)
    messages = queue.Queue()
    backlog = []
    trace = []
    seq = 0
    log = open(args.output / "adapter.log", "w")
    process = subprocess.Popen(adapter_args, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=log, start_new_session=True)

    def reader():
        try:
            while True:
                headers = {}
                while True:
                    line = process.stdout.readline(8192)
                    if not line:
                        messages.put(RuntimeError("adapter stdout closed"))
                        return
                    if line == b"\r\n":
                        break
                    key, value = line.decode("ascii").split(":", 1)
                    headers[key.lower()] = value.strip()
                size = int(headers["content-length"])
                assert 0 < size <= 2 * 1024 * 1024
                payload = process.stdout.read(size)
                assert len(payload) == size
                messages.put(json.loads(payload))
        except Exception as error:
            messages.put(error)

    threading.Thread(target=reader, daemon=True).start()

    def send(command, arguments=None):
        nonlocal seq
        seq += 1
        message = {"seq": seq, "type": "request", "command": command, "arguments": arguments or {}}
        trace.append({"send": message})
        payload = json.dumps(message).encode()
        process.stdin.write(f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload)
        process.stdin.flush()
        return seq

    def receive(predicate, timeout=20):
        deadline = time.monotonic() + timeout
        while True:
            for index, message in enumerate(backlog):
                if predicate(message):
                    return backlog.pop(index)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("expected DAP message did not arrive")
            message = messages.get(timeout=remaining)
            if isinstance(message, Exception):
                raise message
            trace.append({"receive": message})
            backlog.append(message)

    def response(request):
        result = receive(lambda message: message.get("type") == "response" and message.get("request_seq") == request)
        assert result.get("success"), result
        return result.get("body", {})

    def request(command, arguments=None):
        return response(send(command, arguments))

    def event(name):
        return receive(lambda message: message.get("type") == "event" and message.get("event") == name).get("body", {})

    def frame_for(stop):
        threads = request("threads")["threads"]
        thread = stop.get("threadId") or threads[0]["id"]
        frame = request("stackTrace", {"threadId": thread, "startFrame": 0, "levels": 20})["stackFrames"][0]
        scopes = request("scopes", {"frameId": frame["id"]})["scopes"]
        values = []
        for scope in scopes:
            if not scope.get("expensive"):
                values.extend(request("variables", {"variablesReference": scope["variablesReference"]}).get("variables", []))
        return thread, frame, values

    try:
        capabilities = request("initialize", {
            "clientID": "openide-probe", "adapterID": "lldb" if rust else "debugpy", "pathFormat": "path",
            "linesStartAt1": True, "columnsStartAt1": True,
            "supportsRunInTerminalRequest": False, "supportsVariablePaging": True,
        })
        launch = send("launch", launch_args)
        event("initialized")
        points = request("setBreakpoints", {"source": {"path": str(source)}, "breakpoints": [{"line": call_line}]})["breakpoints"]
        assert points and points[0]["verified"] and points[0]["line"] == call_line, points
        request("configurationDone")
        response(launch)
        stopped = event("stopped")
        thread, frame, variables = frame_for(stopped)
        assert frame["line"] == call_line and any(v["name"] == "counter" and v["value"] == "3" for v in variables), frame
        request("stepIn", {"threadId": thread})
        thread, frame, variables = frame_for(event("stopped"))
        assert frame["line"] == 3 and any(v["name"] == "value" and v["value"] == "3" for v in variables)
        request("next", {"threadId": thread})
        thread, frame, variables = frame_for(event("stopped"))
        if rust:
            assert frame["line"] in (4, 5) and "compute" in frame["name"], frame
        else:
            assert frame["line"] == 4 and any(v["name"] == "result" and v["value"] == "4" for v in variables)
        request("stepOut", {"threadId": thread})
        thread, frame, variables = frame_for(event("stopped"))
        # Python can stop on the calling expression before assigning its result.
        if frame["line"] == call_line:
            request("next", {"threadId": thread})
            thread, frame, variables = frame_for(event("stopped"))
        assert frame["line"] == print_line and request("evaluate", {"expression": "counter", "context": "watch", "frameId": frame["id"]})["result"] == "4", frame
        request("continue", {"threadId": thread})
        receive(lambda m: m.get("event") == "output" and "counter=4" in m.get("body", {}).get("output", ""))
        request("pause", {"threadId": thread})
        paused = event("stopped")
        assert paused["reason"] in ("pause", "exception"), paused
        request("disconnect", {"terminateDebuggee": True})
        event("terminated")
        process.stdin.close()
        process.wait(timeout=15)
        report = {"adapter": interpreter, "kind": "lldb-dap" if rust else "debugpy", "version": version, "fixture": str(root),
                  "verified_breakpoint": points[0], "passed": ["initialize and late launch configuration", "verified breakpoint and locals",
                  "step in, next, step out and evaluate", "continue, output, pause and terminating disconnect"],
                  "supportsConfigurationDoneRequest": capabilities.get("supportsConfigurationDoneRequest")}
        (args.output / "report.json").write_text(json.dumps(report, indent=2))
        print(json.dumps(report))
    finally:
        (args.output / "trace.json").write_text(json.dumps(trace, indent=2))
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=10)
        log.close()


if __name__ == "__main__":
    main()
