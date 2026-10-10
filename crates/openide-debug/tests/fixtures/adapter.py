"""Deterministic stdio adapter for offline protocol/lifecycle integration tests."""
import json
import os
import sys

mode, log_path = sys.argv[1:3]
launch = None
program = None
deferred_breakpoints = None
deferred_variables = None
terminated = False
invalidated = False


def log(value):
    with open(log_path, "a", encoding="utf-8") as output:
        output.write(json.dumps(value) + "\n")


def send(value):
    payload = json.dumps(value, ensure_ascii=False).encode("utf-8")
    wire = f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload
    # Exercise framing across arbitrary stream boundaries, including Unicode.
    for index in range(0, len(wire), 37):
        sys.stdout.buffer.write(wire[index:index + 37])
    sys.stdout.buffer.flush()


def response(request, body=None, success=True):
    send({"seq": 0, "type": "response", "request_seq": request["seq"],
          "command": request["command"], "success": success,
          "body": body or {}, "message": "fixture rejection" if not success else ""})


def event(name, body=None):
    send({"seq": 0, "type": "event", "event": name, "body": body or {}})


def stopped(reason="breakpoint"):
    body = {"reason": reason, "allThreadsStopped": True}
    if mode != "threadless":
        body["threadId"] = 1
    event("stopped", body)


def points(request):
    return {"breakpoints": [{"id": 1000 + item["line"], "verified": item["line"] != 13,
                              "line": item["line"] + (1 if item["line"] == 7 else 0),
                              "source": request["arguments"]["source"],
                              "message": "not executable" if item["line"] == 13 else ""}
                             for item in request["arguments"]["breakpoints"]]}


log({"pid": os.getpid()})
while True:
    header = sys.stdin.buffer.readline()
    if not header:
        log({"stdin_closed": True})
        break
    size = None
    while header != b"\r\n":
        if header.lower().startswith(b"content-length:"):
            size = int(header.split(b":", 1)[1])
        header = sys.stdin.buffer.readline()
        if not header:
            raise SystemExit("truncated header")
    request = json.loads(sys.stdin.buffer.read(size))
    log(request)
    if request["type"] == "response":
        assert request["command"] == "runInTerminal" and request["success"] is False
        event("initialized")
        continue
    command = request["command"]
    arguments = request.get("arguments", {})
    if command == "initialize":
        if mode == "malformed":
            sys.stdout.buffer.write(b"Content-Length: 999999999\r\n\r\n")
            sys.stdout.buffer.flush()
        elif mode != "timeout":
            response(request, {} if mode == "no_config" else {"supportsConfigurationDoneRequest": True}, mode != "initialize_fail")
    elif command == "launch":
        launch = request
        program = arguments["program"]
        if mode == "reverse":
            send({"seq": 999, "type": "request", "command": "runInTerminal",
                  "arguments": {"args": ["never-execute-this"], "cwd": os.getcwd()}})
        elif mode == "missing_initialized":
            response(request)
        else:
            event("initialized")
    elif command == "setBreakpoints":
        lines = [item["line"] for item in arguments["breakpoints"]]
        if mode == "stale_breakpoints" and lines == [40]:
            deferred_breakpoints = request
        else:
            response(request, points(request))
            if deferred_breakpoints is not None:
                response(deferred_breakpoints, points(deferred_breakpoints))
                event("breakpoint", {"reason": "changed", "breakpoint": {
                    "id": 1040, "verified": True, "line": 40}})
                deferred_breakpoints = None
            if mode == "no_config":
                response(launch)
                stopped()
    elif command == "configurationDone":
        response(request)
        response(launch)
        if mode == "flood":
            for _ in range(800):
                event("output", {"category": "stdout", "output": "x" * 8192})
        elif mode == "unicode":
            event("output", {"category": "stdout", "output": "a" * 16383 + "🦀 café"})
        stopped()
    elif command == "threads":
        response(request, {"threads": [{"id": 1, "name": "main"}]}, not (mode == "thread_exit" and terminated))
    elif command == "stackTrace":
        response(request, {"stackFrames": [{"id": 10, "name": "main", "source": {"path": program}, "line": 4, "column": 1}], "totalFrames": 1})
    elif command == "scopes":
        response(request, {"scopes": [{"name": "Locals", "variablesReference": 100, "expensive": False}]}, mode != "scope_fail")
    elif command == "variables":
        if mode == "late_variables":
            deferred_variables = request
        elif mode == "invalidated" and not invalidated:
            invalidated = True
            event("invalidated", {"areas": ["stacks", "variables"], "threadId": 1, "stackFrameId": 10})
            response(request, {"variables": [{"name": "OLD", "value": "stale", "variablesReference": 0}]})
        else:
            response(request, {"variables": [{"name": "counter", "value": "3", "type": "int", "variablesReference": 0},
                                             {"name": "child", "value": "{…}", "variablesReference": 200}]})
    elif command in ("next", "stepIn", "stepOut"):
        response(request)
        event("continued", {"threadId": 1, "allThreadsContinued": True})
        stopped("step")
    elif command == "continue":
        event("continued", {"threadId": 1, "allThreadsContinued": True})
        response(request, {"allThreadsContinued": True})
        if mode == "late_variables":
            stopped("breakpoint")
            if deferred_variables:
                response(deferred_variables, {"variables": [{"name": "OLD", "value": "stale", "variablesReference": 0}]})
                deferred_variables = None
        elif mode != "running":
            if mode == "thread_exit":
                event("thread", {"reason": "exited", "threadId": 1})
            event("output", {"category": "stdout", "output": "done\n"})
            event("exited", {"exitCode": 0})
            event("terminated")
            terminated = True
    elif command == "pause":
        response(request)
        stopped("pause")
    elif command == "disconnect":
        assert arguments.get("terminateDebuggee") is True
        if mode != "ignore_disconnect":
            response(request)
            if not terminated:
                event("terminated")
            # Deliberately stay alive until the client closes its stdin.
    else:
        response(request, success=False)
