#!/usr/bin/env python3
"""Dependency-free MCP stdio example: inspect names, never file contents."""

import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True

MAX_MESSAGE_BYTES = 64 * 1024
MAX_ENTRIES = 10_000
MAX_NAMES = 100
PROTOCOL_VERSIONS = {"2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"}


def project_info(workspace):
    counts = {"files": 0, "directories": 0, "symlinks": 0, "other": 0}
    names = []
    complete = True
    with os.scandir(workspace) as entries:
        for index, entry in enumerate(entries):
            if index >= MAX_ENTRIES:
                complete = False
                break
            if entry.is_symlink():
                kind = "symlinks"
            elif entry.is_dir(follow_symlinks=False):
                kind = "directories"
            elif entry.is_file(follow_symlinks=False):
                kind = "files"
            else:
                kind = "other"
            counts[kind] += 1
            names.append({"name": entry.name, "kind": kind})
    names.sort(key=lambda entry: entry["name"])
    return {
        "project": workspace.name,
        "counts": counts,
        "counts_complete": complete,
        "entries": names[:MAX_NAMES],
        "names_truncated": len(names) > MAX_NAMES or not complete,
        "content_read": False,
    }


def send(identifier, result=None, error=None):
    response = {"jsonrpc": "2.0", "id": identifier}
    response["error" if error is not None else "result"] = error if error is not None else result
    sys.stdout.write(json.dumps(response, ensure_ascii=False, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def fail(identifier, code, message):
    send(identifier, error={"code": code, "message": message})


def serve(workspace):
    initialized = False
    while True:
        line = sys.stdin.buffer.readline(MAX_MESSAGE_BYTES + 1)
        if not line:
            return
        if len(line) > MAX_MESSAGE_BYTES:
            while line and not line.endswith(b"\n"):
                line = sys.stdin.buffer.readline(MAX_MESSAGE_BYTES + 1)
            fail(None, -32700, "Message exceeds 64 KiB")
            continue
        try:
            request = json.loads(line)
        except (ValueError, UnicodeError):
            fail(None, -32700, "Invalid JSON")
            continue
        if not isinstance(request, dict) or request.get("jsonrpc") != "2.0":
            fail(None, -32600, "Invalid JSON-RPC request")
            continue
        # Notifications, including notifications/initialized, have no reply.
        if "id" not in request:
            continue
        identifier = request["id"]
        if isinstance(identifier, bool) or not isinstance(identifier, (str, int)):
            fail(None, -32600, "Invalid request ID")
            continue
        method = request.get("method")
        params = request.get("params", {})
        if not isinstance(params, dict):
            fail(identifier, -32602, "Expected object parameters")
        elif method == "initialize":
            version = params.get("protocolVersion")
            if not isinstance(version, str):
                fail(identifier, -32602, "Expected a protocol version string")
                continue
            initialized = True
            send(identifier, {
                "protocolVersion": version if version in PROTOCOL_VERSIONS else "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "openide-project-info", "version": "1.0.0"},
            })
        elif method == "ping":
            send(identifier, {})
        elif not initialized:
            fail(identifier, -32000, "Initialize the server first")
        elif method == "tools/list":
            send(identifier, {"tools": [{
                "name": "project_info",
                "description": "List up to 100 first-level entry names and count up to 10000 entries in the active project. Never read file contents or follow symlinks.",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
                "annotations": {"readOnlyHint": True, "destructiveHint": False, "openWorldHint": False},
            }]})
        elif method == "tools/call":
            if params.get("name") != "project_info":
                fail(identifier, -32602, "Unknown tool")
            elif params.get("arguments", {}) != {}:
                fail(identifier, -32602, "This tool takes no arguments")
            else:
                try:
                    info = project_info(workspace)
                    send(identifier, {"content": [{"type": "text", "text": json.dumps(info, ensure_ascii=False)}], "isError": False})
                except OSError:
                    send(identifier, {"content": [{"type": "text", "text": "The project directory is unavailable."}], "isError": True})
        else:
            fail(identifier, -32601, "Method not found")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("Usage: server.py WORKSPACE")
    workspace = Path(sys.argv[1]).resolve()
    if not workspace.is_dir():
        sys.exit("WORKSPACE must be an existing directory")
    try:
        serve(workspace)
    except BrokenPipeError:
        pass
