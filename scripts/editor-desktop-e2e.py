#!/usr/bin/env python3
"""Opt-in Wayland desktop E2E against an isolated, feature-gated native app.

Requires Hyprland, wtype and grim. Builds are separate. Never targets the user's
existing OpenIDE window or workspace. Every keyboard action verifies window PID.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def run(*args):
    return subprocess.check_output(["rtk", *map(str, args)], text=True)


def wait(check, label, timeout=15):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(label)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    options.output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="openide-desktop-e2e-", dir="/tmp"))
    (root / ".openide-editor-fixture").touch()
    project = root / "project"
    project.mkdir()
    source = project / "main.rs"
    source.write_text("""use std::path::Path;

/// Comandos disponibles en el proyecto.
#[derive(Default)]
struct CommandRegistry {
    commands: Vec<String>,
}

impl CommandRegistry {
    fn register(&mut self, command: &str) {
        self.commands.push(command.to_owned());
    }

    fn search(&self, query: &str) -> Vec<&str> {
        self.commands.iter()
            .filter(|command| command.contains(query))
            .map(String::as_str)
            .collect()
    }
}

fn main() {
    let mut registry = CommandRegistry::default();
    registry.register("cargo test --workspace");
    let project = Path::new("openADE");
    println!("{}: {:?}", project.display(), registry.search("cargo"));
}
""")
    (project / "Cargo.toml").write_text('[package]\nname="editor_fixture"\nversion="0.1.0"\nedition="2021"\n[[bin]]\nname="fixture"\npath="main.rs"\n')
    prior = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
    occupied = {w["id"] for w in json.loads(run("hyprctl", "workspaces", "-j"))}
    workspace = next(i for i in range(90, 120) if i not in occupied)
    process = None
    checks = []

    def active():
        return json.loads(run("hyprctl", "activewindow", "-j"))

    def keys(*args):
        assert active().get("pid") == process.pid, "fixture lost keyboard focus; no keys sent"
        subprocess.run(["rtk", options.wtype, *args], check=True)
        time.sleep(0.15)

    def chord(key, shift=False):
        args = ["-M", "ctrl"]
        if shift:
            args += ["-M", "shift"]
        args += ["-k", key]
        if shift:
            args += ["-m", "shift"]
        args += ["-m", "ctrl"]
        keys(*args)

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        x, y = window["at"]
        width, height = window["size"]
        subprocess.run(["rtk", "grim", "-g", f"{x},{y} {width}x{height}", str(options.output / name)], check=True)

    def launch(log_name):
        log = open(options.output / log_name, "w")
        child = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)], stdout=log, stderr=subprocess.STDOUT, env=os.environ.copy())
        wait(lambda: active().get("pid") == child.pid, "native fixture window did not receive focus")
        time.sleep(2)
        return child

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {workspace} }}))")
        process = launch("native-start.log")
        capture("editor-initial-native.png")
        chord("a")
        keys("// E2E café 🧠")
        chord("s")
        wait(lambda: source.read_text() == "// E2E café 🧠", "native Unicode typing/save failed")
        checks.append("Wayland keyboard → native event mapper → editor → atomic file save")
        chord("f")
        keys("café")
        keys("-k", "Return")
        keys("-k", "Escape")
        keys("mundo")
        chord("s")
        wait(lambda: source.read_text() == "// E2E mundo 🧠", "native search selection/replacement failed")
        checks.append("native search, Enter selection, Escape focus restoration, replace, save")
        chord("z")
        chord("s")
        wait(lambda: source.read_text() == "// E2E café 🧠", "native undo/save failed")
        checks.append("native undo and save")
        capture("editor-native.png")
        chord("a")
        chord("x")
        chord("s")
        wait(lambda: source.read_text() == "", "native cut/clipboard failed")
        chord("v")
        chord("s")
        wait(lambda: source.read_text() == "// E2E café 🧠", "native clipboard paste failed")
        checks.append("native system clipboard cut/paste preserves Unicode")
        keys("-M", "shift", "-k", "F10", "-m", "shift")
        capture("editor-context-native.png")
        keys("-k", "Escape")
        checks.append("native keyboard context menu opens and Escape restores editor input")
        chord("End")
        keys(" · borrador recuperable")
        time.sleep(2)
        # Only the process created by this fixture is killed to simulate a crash.
        process.kill()
        process.wait(timeout=10)
        process = launch("native-restart.log")
        chord("s")
        wait(lambda: source.read_text() == "// E2E café 🧠 · borrador recuperable", "crash/restart draft recovery failed")
        checks.append("native crash, durable draft recovery, restart, explicit save")
        capture("editor-recovered-native.png")
        report = {"passed": checks, "fixture": str(root), "renderer": "eframe/wgpu native Wayland", "input": "wtype Wayland virtual keyboard", "fps_claim": None}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        for stale in ["failure.json", "editor-failure-native.png"]:
            (options.output / stale).unlink(missing_ok=True)
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        report = {"passed": checks, "fixture": str(root), "error": str(error), "source_after_failure": source.read_text()}
        (options.output / "failure.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        if process is not None and active().get("pid") == process.pid:
            capture("editor-failure-native.png")
        print(json.dumps(report, ensure_ascii=False), flush=True)
        raise
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        current = json.loads(run("hyprctl", "activeworkspace", "-j"))["id"]
        if current == workspace:
            run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{ workspace = {prior} }}))")


if __name__ == "__main__":
    main()
