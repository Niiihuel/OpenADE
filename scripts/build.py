#!/usr/bin/env python3
"""Build the Linux desktop app incrementally and prune obsolete hashed outputs."""

import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "target"
UNIT = re.compile(r"^([A-Za-z0-9_]+-[0-9a-f]{16})(?:[.-].*)?$")


def artifact_unit(name):
    # Only library outputs have Cargo's extra `lib` prefix. A crate's actual
    # name can itself begin with `lib` (libc/libloading), including in its .d file.
    if name.startswith("lib") and Path(name).suffix in {".rlib", ".rmeta", ".so", ".dylib", ".a"}:
        name = name[3:]
    match = UNIT.fullmatch(name)
    return match.group(1) if match else None


def prune(deps, current_units, build_started):
    """Keep the successful build graph, unknown files, and newer outputs."""
    removed = 0
    size = 0
    for path in deps.iterdir():
        unit = artifact_unit(path.name)
        if not unit or unit in current_units or path.is_symlink() or not path.is_file():
            continue
        stat = path.stat()
        if stat.st_mtime_ns >= build_started:
            continue
        path.unlink()
        removed += 1
        size += stat.st_size
    return removed, size


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="launch the app after building")
    parser.add_argument("--profile", choices=["desktop", "dev", "release"], default="desktop",
                        help="desktop optimizes the interactive app; dev favors debugging")
    args = parser.parse_args()
    TARGET.mkdir(exist_ok=True)
    profile_dir = TARGET / ("debug" if args.profile == "dev" else args.profile)
    # Serialize this helper; Cargo's own lock also protects pruning from raw cargo commands.
    with (TARGET / ".openade-build.lock").open("a") as helper_lock:
        fcntl.flock(helper_lock, fcntl.LOCK_EX)
        started = time.time_ns()
        units = set()
        executable = None
        process = subprocess.Popen(
            ["cargo", "build", "--locked", "-p", "agents-window", "--profile", args.profile, "--target-dir", str(TARGET),
             "--message-format=json-render-diagnostics"],
            cwd=ROOT, stdout=subprocess.PIPE, text=True,
        )
        for line in process.stdout:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                print(line, end="", flush=True)
                continue
            if message.get("reason") == "compiler-artifact":
                for filename in message.get("filenames", []):
                    path = Path(filename)
                    if path.parent == profile_dir / "deps":
                        unit = artifact_unit(path.name)
                        if unit:
                            units.add(unit)
                if message.get("target", {}).get("name") == "agents-window":
                    executable = message.get("executable") or executable
        code = process.wait()
        if code:
            return code  # A failed build must never remove the last successful outputs.
        if not executable or not units:
            print("No complete artifact manifest; skipping cleanup.", file=sys.stderr)
            return 1
        with (profile_dir / ".cargo-lock").open("a") as cargo_lock:
            try:
                fcntl.flock(cargo_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                print("Another Cargo command is running; skipping cleanup.", file=sys.stderr)
            else:
                # Cargo reports the top-level executable; its hashed hard link also
                # lives in deps and belongs to the current build.
                for path in (profile_dir / "deps").iterdir():
                    if path.is_file() and not path.is_symlink() and os.path.samefile(path, executable):
                        unit = artifact_unit(path.name)
                        if unit:
                            units.add(unit)
                count, size = prune(profile_dir / "deps", units, started)
                print(f"Removed {count} obsolete artifacts ({size / 1024**3:.2f} GiB).")
    if args.run:
        os.execv(executable, [executable])
    return 0


if __name__ == "__main__":
    sys.exit(main())
