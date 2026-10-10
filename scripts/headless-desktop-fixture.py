#!/usr/bin/env python3
"""Run a native fixture in private KWin-virtual → Hyprland, never the live seat.

Hyprland's Aquamarine build needs a parent with compositor/xdg-shell v6;
Weston 15 and Sway 1.12 cannot host this build. KWin 6.7 virtual can.
No uinput is used: fixtures use a persistent private Wayland virtual pointer.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
from native_desktop_fixture import open_private_pointer


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kwin", type=Path, required=True)
    parser.add_argument("--hyprland", type=Path, default=Path("/run/current-system/sw/bin/Hyprland"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--width", type=int, default=1600)
    parser.add_argument("--height", type=int, default=1000)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    assert command, "supply a fixture command after --"
    assert 800 <= args.width <= 3840 and 600 <= args.height <= 2160
    args.output.mkdir(parents=True, exist_ok=True)
    # Hyprland's instance signature is long: keep the UNIX socket prefix short.
    root = Path(tempfile.mkdtemp(prefix="oh-", dir="/tmp"))
    root.chmod(0o700)
    for name in ("config", "cache"):
        (root / name).mkdir(mode=0o700)
    env = os.environ.copy()
    for key in ("WAYLAND_DISPLAY", "DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE",
                "DBUS_SESSION_BUS_ADDRESS", "NOTIFY_SOCKET", "WATCHDOG_PID", "WATCHDOG_USEC", "SWAYSOCK"):
        env.pop(key, None)
    env.update(XDG_RUNTIME_DIR=str(root), XDG_CONFIG_HOME=str(root / "config"),
               XDG_CONFIG_DIRS=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"),
               HYPRLAND_NO_SD_VARS="1", HYPRLAND_NO_SD_NOTIFY="1",
               # An unknown explicit libseat backend fails closed. Aquamarine
               # must use the private Wayland parent, never DRM/VT/session input.
               LIBSEAT_BACKEND="openide_disabled", OPENIDE_FIXTURE_POINTER="virtual-pointer",
               OPENIDE_FIXTURE_WIDTH=str(args.width), OPENIDE_FIXTURE_HEIGHT=str(args.height))
    config = root / "hyprland.lua"
    config.write_text(f'''hl.monitor({{output="",mode="{args.width}x{args.height}@60",position="0x0",scale=1}})
hl.config({{animations={{enabled=false}},general={{gaps_in=0,gaps_out=0,border_size=0}},misc={{disable_hyprland_logo=true,disable_splash_rendering=true}},cursor={{no_hardware_cursors=true}}}})
''')
    processes = []
    logs = []
    pointer = None
    probes = {}
    phase = "private KWin"

    def launch(argv, filename, launch_env=env):
        log = (args.output / filename).open("w")
        logs.append(log)
        process = subprocess.Popen(["rtk", *map(str, argv)], env=launch_env,
                                   stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        processes.append(process)
        return process

    def wait_for(check, process, label):
        nonlocal phase
        phase = label
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            failed = next((child for child in processes if child.poll() is not None), None)
            if failed is not None:
                raise RuntimeError(f"{label}: private process {failed.pid} exited ({failed.returncode})")
            result = check()
            if result:
                return result
            time.sleep(0.1)
        raise TimeoutError(f"{label} was not ready after 30s; last probe: {probes.get(label)}")

    def ipc(*arguments, label):
        try:
            result = subprocess.run(["rtk", "hyprctl", *arguments], env=env,
                                    capture_output=True, text=True, timeout=2)
            probes[label] = {"returncode": result.returncode,
                             "stdout": result.stdout[-4096:], "stderr": result.stderr[-4096:]}
            return result
        except subprocess.TimeoutExpired:
            probes[label] = {"error": "hyprctl did not respond within 2s"}
            return None

    def failure_report(error):
        if env.get("HYPRLAND_INSTANCE_SIGNATURE"):
            ipc("configerrors", label="configuration")
        tails = {}
        paths = [args.output / "headless-kwin.log", args.output / "headless-hyprland.log"]
        paths.extend((root / "hypr").glob("*/hyprland.log"))
        for path in paths:
            try:
                with path.open("rb") as log:
                    log.seek(max(0, path.stat().st_size - 8192))
                    tails[str(path)] = log.read().decode(errors="replace")
            except OSError as log_error:
                tails[str(path)] = str(log_error)
        report = {"phase": phase, "error": str(error), "runtime": str(root),
                  "expected_output": {"width": args.width, "height": args.height},
                  "processes": [{"pid": process.pid, "exit_code": process.poll()}
                                for process in processes], "probes": probes, "log_tails": tails}
        path = args.output / "headless-failure.json"
        path.write_text(json.dumps(report, indent=2))
        print(f"Private desktop failed during {phase}: {error}; diagnostics: {path}", flush=True)

    try:
        parent = launch(["dbus-run-session", "--", args.kwin, "--virtual", "--width", args.width,
                         "--height", args.height, "--socket", "kwin-fixture", "--no-lockscreen",
                         "--no-kactivities", "--no-global-shortcuts"], "headless-kwin.log")
        wait_for(lambda: (root / "kwin-fixture").is_socket(), parent, "private KWin")
        env["WAYLAND_DISPLAY"] = "kwin-fixture"
        compositor = launch([args.hyprland, "--config", config], "headless-hyprland.log")
        socket = wait_for(lambda: next((root / "hypr").glob("*/.socket.sock"), None), compositor, "private Hyprland")
        env["HYPRLAND_INSTANCE_SIGNATURE"] = socket.parent.name
        env["OPENIDE_FIXTURE_PRIVATE_RUNTIME"] = str(root)
        (root / ".openide-headless-fixture").write_text(socket.parent.name)

        def monitors():
            result = ipc("monitors", "-j", label="private output")
            try:
                outputs = json.loads(result.stdout) if result is not None and result.returncode == 0 else []
                if not isinstance(outputs, list):
                    probes["private output"]["error"] = "hyprctl response was not an output list"
                    return None
                return outputs if any(output["width"] == args.width and output["height"] == args.height
                                      for output in outputs) else None
            except (json.JSONDecodeError, KeyError, TypeError) as error:
                probes["private output"]["error"] = str(error)
                return None

        outputs = wait_for(monitors, compositor, "private output")
        displays = [path for path in root.glob("wayland-*") if path.is_socket()]
        assert len(displays) == 1, "ambiguous private Hyprland socket"
        env["WAYLAND_DISPLAY"] = displays[0].name
        # Keep a real pointer device present before clients bind wl_seat. IPC
        # cursor warps alone do not advertise pointer capability to native apps.
        pointer = open_private_pointer(env)
        (args.output / "headless-session.json").write_text(json.dumps({
            "runtime": str(root), "wayland_display": env["WAYLAND_DISPLAY"],
            "hyprland_instance": env["HYPRLAND_INSTANCE_SIGNATURE"], "outputs": outputs,
            "input": "private Wayland virtual keyboard + persistent virtual pointer",
        }, indent=2))
        print(f"Private desktop ready: {root}, {outputs[0]['width']}×{outputs[0]['height']}", flush=True)
        phase = "native fixture"
        fixture = subprocess.Popen(["rtk", *command], env=env, start_new_session=True)
        processes.append(fixture)
        result = fixture.wait()
        if result == 0:
            (args.output / "headless-failure.json").unlink(missing_ok=True)
        else:
            failure_report(RuntimeError(f"native fixture exited ({result})"))
        return result
    except BaseException as error:
        failure_report(error)
        raise
    finally:
        # Kill only process groups this invocation created, in reverse order.
        # Keep logs/runtime artifacts for diagnosing a failure.
        for process in reversed(processes):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
        for log in logs:
            log.close()
        if pointer is not None:
            pointer.close()


if __name__ == "__main__":
    raise SystemExit(main())
