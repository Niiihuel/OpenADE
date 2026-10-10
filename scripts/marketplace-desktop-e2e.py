#!/usr/bin/env python3
"""Marketplace starter and local skill lifecycle on a private native compositor.

No starter package is downloaded; all installation content is fixture-owned.
"""
import argparse
import hashlib
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
from urllib.parse import urlsplit, parse_qs

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
    env["OPENIDE_MARKETPLACE_DESKTOP_FIXTURE"] = "1"
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


class MarketplaceOrigin:
    """Deterministic Skills.sh/GitHub wire fixture, never executes skill content."""
    def __init__(self):
        self.version = "a"
        self.offline = False
        self.requests = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                owner.requests.append(self.path)
                if owner.offline:
                    self.send_error(503, "Fixture is offline")
                    return
                path = urlsplit(self.path)
                content = None
                if path.path == "/api/search":
                    query = parse_qs(path.query).get("q", [""])[0]
                    content = {"skills": [{"id": "example/skills/demo", "name": "demo",
                        "source": "example/skills", "installs": 23}] if query == "demo" else []}
                    time.sleep(1.2)  # Observe the actual worker's loading state.
                elif path.path == "/repos/example/skills/commits/HEAD":
                    content = {"sha": owner.version * 40, "commit": {"tree": {"sha": "f" * 40}}}
                elif path.path == "/repos/example/skills/git/trees/" + "f" * 40:
                    files = owner.files(owner.version)
                    content = {"truncated": False, "tree": [{"path": name, "mode": "100644", "type": "blob",
                        "sha": hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest(),
                        "size": len(data)} for name, data in files.items()]}
                elif path.path.startswith("/example/skills/"):
                    suffix = path.path[len("/example/skills/"):]
                    revision, _, name = suffix.partition("/")
                    if revision in ("a" * 40, "b" * 40):
                        content = owner.files(revision[0]).get(name)
                if content is None:
                    self.send_error(404, "Fixture route not found")
                    return
                raw = content if isinstance(content, bytes) else json.dumps(content).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(raw)))
                self.send_header("Content-Type", "application/octet-stream" if isinstance(content, bytes) else "application/json")
                self.end_headers()
                self.wfile.write(raw)
            def log_message(self, *_args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.origin = f"http://127.0.0.1:{self.server.server_port}"

    @staticmethod
    def files(version):
        return {
            "skills/demo/SKILL.md": ("---\nname: demo\ndescription: Synthetic Marketplace revision " + version +
                "\nlicense: MIT\n---\n\nRead references/info.txt. Do not execute commands.\n").encode(),
            "skills/demo/references/info.txt": ("Fixture reference revision " + version + "\n").encode(),
            "LICENSE": b"MIT synthetic fixture notice\n",
        }

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--wtype", default="wtype")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--ide-only", action="store_true")
    options = parser.parse_args()
    assert _private_runtime() is not None, "private compositor required; user desktop input disabled"
    options.output.mkdir(parents=True, exist_ok=True)
    for pattern in ("marketplace-*.png", "native-marketplace-*.log", "failure.json"):
        for artifact in options.output.glob(pattern):
            artifact.unlink()
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
        subprocess.run(["rtk", options.wtype, "-d", "35", *args], check=True)
        time.sleep(0.15)

    def capture(name):
        window = active()
        assert window.get("pid") == process.pid
        value = observation()
        move_cursor(run, active, process.pid,
                    *logical_point(window, value, value["viewport"][0] - 3, value["viewport"][1] - 3))
        time.sleep(.2)
        window = active()
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
        rect = value["regions"][region_id or "marketplace_page"]["rect"]
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

    def market():
        return observation()["marketplace"]

    def item(item_id):
        return next((entry for entry in market()["items"] if entry["id"] == item_id), None)

    def search(query):
        scroll(-100)
        click("marketplace_search")
        state_when(lambda value: value["focused"] == "marketplace_search", "search input did not receive focus")
        time.sleep(.2)
        keys("-M", "ctrl", "-k", "a", "-m", "ctrl")
        if query:
            keys(query)
        else:
            keys("-k", "BackSpace")
        state_when(lambda value: value["marketplace"]["active_query"] == query, "search did not update")

    def reveal(region_id):
        for _ in range(45):
            value = observation()
            if reachable(value, region_id):
                return value
            scroll(3)
        raise AssertionError(f"control not reachable by scrolling: {region_id}")

    def assert_unique_regions(value):
        duplicates = value["duplicate_region_ids"]
        assert not duplicates, duplicates

    def ide_flow(starter_ids):
        click("toggle_sidebar_reopen")
        click("new_session_sidebar")
        state_when(lambda value: value["marketplace"]["page"] == "Workspace", "workspace navigation failed")
        time.sleep(.6)
        hover("window_mode_toggle")
        time.sleep(.3)
        click("window_mode_toggle")
        state_when(lambda value: value["ide"], "IDE mode did not activate")
        click("ide_activity:extensions")
        state_when(lambda value: not value["marketplace"]["busy"]
                   and reachable(value, "ide_extensions_search"), "IDE extension activity did not load")
        for width, height in [(1000, 820), (520, 640)]:
            resize(width, height)
            if not reachable(observation(), "ide_extensions_search"):
                click("ide_activity:extensions")
            value = state_when(lambda value: reachable(value, "ide_extensions_search"),
                               "IDE extension sidebar or compact drawer unreachable")
            expected_rows = {f"ide_extension:{identifier}" for identifier in starter_ids}
            actual_rows = {identifier for identifier, region in value["regions"].items()
                           if identifier.startswith("ide_extension:") and region["hit"] == identifier}
            assert actual_rows and actual_rows <= expected_rows and len(actual_rows) <= 24, ("IDE sidebar page must only contain bounded starter rows", actual_rows)
            assert reachable(value, "ide_marketplace_page_next"), "sidebar pagination missing"
            click("ide_marketplace_page_next")
            state_when(lambda value: value["marketplace"]["catalog_page"] == 1, "sidebar next page did not activate")
            click("ide_marketplace_page_prev")
            state_when(lambda value: value["marketplace"]["catalog_page"] == 0, "sidebar previous page did not activate")
            assert_unique_regions(value)
            capture(f"marketplace-ide-sidebar-{width}.png")
            click("ide_extensions_search")
            keys("-M", "ctrl", "-k", "a", "-m", "ctrl")
            keys("frontend-design")
            value = state_when(lambda value: value["marketplace"]["query"] == "frontend-design",
                               "IDE extension query did not update")
            only = "starter-anthropic:frontend-design"
            actual_rows = {identifier for identifier, region in value["regions"].items()
                           if identifier.startswith("ide_extension:") and region["hit"] == identifier}
            assert actual_rows == {f"ide_extension:{only}"}
            assert_unique_regions(value)
            capture(f"marketplace-ide-search-{width}.png")
            click(f"ide_extension:{only}")
            value = state_when(lambda value: reachable(value, "ide_extensions_catalog")
                               and "ide_extension_detail" in value["regions"], "IDE selected skill detail did not open centrally")
            assert_unique_regions(value)
            if width == 520:
                assert "ide_extensions_search" not in value["regions"], "compact sidebar remained over the detail"
            capture(f"marketplace-ide-detail-{width}.png")
            click("ide_extensions_catalog")
            value = state_when(lambda value: reachable(value, "marketplace_search")
                               and "ide_extension_detail" not in value["regions"], "IDE detail did not return to central catalog")
            assert value["marketplace"]["query"] == "frontend-design"
            assert_unique_regions(value)
            search("")
            capture(f"marketplace-ide-catalog-{width}.png")
        checks.append("IDE Complementos presents bounded catalog pages, live sidebar search, central detail/back navigation and a dismissing compact drawer at 1000/520px with no duplicate region IDs")

    def card_geometry(width):
        value = observation()
        viewport = [0, 0, *value["viewport"]]
        assert_unique_regions(value)
        title = next(label for label in value["marketplace"]["labels"] if label["text"] == "Marketplace")
        title_rect = [*title["position"], *title["bounds"]]
        assert contains(viewport, title_rect)
        for identifier in ["toggle_sidebar_reopen", "marketplace_refresh", "marketplace_add_menu"]:
            region = value["regions"].get(identifier)
            if region:
                x, y, w, h = region["rect"]
                a, b, c, d = title_rect
                assert min(x+w, a+c) - max(x, a) <= .5 or min(y+h, b+d) - max(y, b) <= .5, ("header control overlaps title", identifier, title_rect, region["rect"])
        visible_controls = {identifier: region["rect"] for identifier, region in value["regions"].items()
                    if identifier.startswith("marketplace_install:") and region["hit"] == identifier}
        scroll_rect = value["regions"]["marketplace_page"]["rect"]
        for identifier, rect in visible_controls.items():
            if rect[3] < 27.9:
                assert abs(rect[1] + rect[3] - scroll_rect[1] - scroll_rect[3]) < 1.0, ("partial target must be clipped only by viewport", identifier, rect)
        controls = {identifier: rect for identifier, rect in visible_controls.items() if rect[3] >= 27.9}
        assert controls, "no starter installation control is visible"
        labels = value["marketplace"]["labels"]
        for identifier, rect in controls.items():
            assert contains(viewport, rect), (identifier, rect, viewport)
            assert rect[3] >= 28, (identifier, "installation control too short", rect)
            label = next((label for label in labels if label["text"] == "Instalar"
                          and contains(rect, [*label["position"], *label["bounds"]], .5)), None)
            assert label, (identifier, "native installation label clipped", rect)
        candidates = [paint["rect"] for paint in value["marketplace"]["paint"]
                      if paint["kind"] == "stroke" and abs(paint["radius"] - 8) < .1
                      and paint["rect"][2] >= 200 and paint["rect"][3] >= 70]
        cards = []
        for rect in controls.values():
            parents = [candidate for candidate in candidates if contains(candidate, rect)]
            assert parents, ("installation action has no containing card", rect)
            card = min(parents, key=lambda candidate: candidate[2] * candidate[3])
            if card not in cards:
                cards.append(card)
        assert cards and all(rect[3] <= 240 for rect in cards), ("starter cards should stay compact", cards)
        assert all(contains(viewport, rect) for rect in cards if rect[1] + rect[3] <= viewport[3]), cards
        columns = {round(rect[0]) for rect in cards}
        assert len(columns) == (2 if width >= 900 else 1), ("responsive card columns", width, cards)
        geometry.append({"width": width, "title": title_rect, "install_controls": controls, "cards": cards})

    def launch(env):
        nonlocal process, log
        log = open(options.output / f"native-marketplace-{len(fixtures)}.log", "w")
        process = subprocess.Popen([str(options.binary.resolve()), "--editor-e2e", str(root)],
                                   stdout=log, stderr=subprocess.STDOUT, env=env)
        fixtures.append({"root": str(root), "pid": process.pid})
        wait(lambda: active().get("pid") == process.pid, "fixture did not receive focus")
        return state_when(lambda value: value.get("marketplace")
                          and value["marketplace"]["page"] == "Marketplace"
                          and not value["marketplace"]["busy"]
                          and value["marketplace"]["items"], "Marketplace fixture not loaded")

    def stop():
        nonlocal process, log
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        if log:
            log.close()
            log = None
        process = None

    origin = MarketplaceOrigin()
    proxy_requests = []
    class DenyExternal(BaseHTTPRequestHandler):
        def do_CONNECT(self):
            proxy_requests.append(self.path)
            self.send_error(503, "isolated visual fixture: external downloads disabled")
        def do_GET(self):
            proxy_requests.append(self.path)
            self.send_error(503, "isolated visual fixture: external downloads disabled")
        def log_message(self, *_args):
            pass
    proxy = ThreadingHTTPServer(("127.0.0.1", 0), DenyExternal)
    threading.Thread(target=proxy.serve_forever, daemon=True).start()

    try:
        run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{workspace={workspace}}}))")
        root = Path(tempfile.mkdtemp(prefix="openide-marketplace-e2e-", dir="/tmp"))
        (root / ".openide-editor-fixture").touch()
        (root / "project").mkdir()
        (root / "project/main.rs").write_text("fn main() {}\n")
        (root / "marketplace-fixture.json").write_text("{}\n")
        skill = root / "local-skills/native-fixture-skill"
        skill.mkdir(parents=True)
        skill_text = "---\nname: native-fixture-skill\ndescription: Synthetic native Marketplace verification, no commands or credentials.\nlicense: MIT\n---\n\nRead this fixture and report its name. Do not execute commands.\n"
        (skill / "SKILL.md").write_text(skill_text)
        env = fixture_environment(root)
        for key in [key for key in env if key.lower().endswith("proxy")]:
            env.pop(key)
        endpoint = f"http://127.0.0.1:{proxy.server_port}"
        env.update(HTTP_PROXY=endpoint, HTTPS_PROXY=endpoint, ALL_PROXY=endpoint, NO_PROXY="127.0.0.1,localhost", OPENIDE_E2E_MARKETPLACE_ORIGIN=origin.origin)
        value = launch(env)
        assert not value["marketplace"]["sources"] and len(value["marketplace"]["items"]) >= 30
        assert all(entry["installed_scope"] is None for entry in market()["items"])
        starter_ids = {entry["id"] for entry in market()["items"]}
        if options.ide_only:
            resize(1000, 820)
            ide_flow(starter_ids)
            report = {"passed": checks, "fixtures": fixtures,
                      "ide_only": True, **desktop_metadata()}
            (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
            (options.output / "failure.json").unlink(missing_ok=True)
            (options.output / "marketplace-failure.png").unlink(missing_ok=True)
            print(json.dumps(report, ensure_ascii=False))
            return
        for width, height in [(1000, 820), (520, 640)]:
            resize(width, height)
            scroll(-100)
            card_geometry(width)
            capture(f"marketplace-starter-{width}.png")
        checks.append("offline recommendations are available without configured sources; paged compact native cards and labels fit 1000/520px")
        click("marketplace_page_next")
        state_when(lambda value: value["marketplace"]["catalog_page"] == 1, "catalog pagination did not advance")
        capture("marketplace-page-two-520.png")
        click("marketplace_page_prev")
        state_when(lambda value: value["marketplace"]["catalog_page"] == 0, "catalog pagination did not return")
        click("marketplace_category_toggle")
        click("marketplace_category:Diseño y frontend")
        state_when(lambda value: value["marketplace"]["category"] == "Diseño y frontend", "category did not apply")
        capture("marketplace-category-520.png")
        click("marketplace_category_toggle")
        click("marketplace_category:all")
        state_when(lambda value: value["marketplace"]["category"] is None, "category did not clear")
        checks.append("recommendation pagination and category selection work by pointer without loading more than one page")
        first_starter = next(entry["id"] for entry in market()["items"])
        click(f"marketplace_details:{first_starter}")
        state_when(lambda value: value["marketplace"]["selected_item"] == first_starter,
                   "starter detail modal did not open")
        assert any(identifier.startswith("marketplace_open_source:") for identifier in observation()["regions"])
        capture("marketplace-starter-details-520.png")
        keys("-k", "Escape")
        state_when(lambda value: value["marketplace"]["selected_item"] is None,
                   "Escape did not close starter detail modal")
        assert all(entry["installed_scope"] is None for entry in market()["items"])
        checks.append("starter details expose provenance and explicit source action; inspecting and closing the modal does not install or open an external page")

        click("marketplace_tab:installed")
        state_when(lambda value: value["marketplace"]["installed_only"], "installed filter did not activate")
        assert "Todavía no instalaste skills" in text(observation())
        capture("marketplace-empty-installed-520.png")
        click("marketplace_tab:recommended")
        search("fixture-query-with-no-matches")
        assert "Sin resultados" in text(observation())
        capture("marketplace-empty-search-520.png")
        search("")
        click("marketplace_add_menu")
        click("marketplace_source_kind:github")
        state_when(lambda value: value["marketplace"]["source_kind"] == "Github"
                   and value["focused"] == "marketplace_source_draft", "GitHub origin modal missing autofocus")
        capture("marketplace-github-origin-520.png")
        keys("-k", "Escape")
        state_when(lambda value: value["marketplace"]["source_kind"] is None
                   and value["focused"] == "marketplace_add_menu", "origin Escape did not restore trigger focus")
        checks.append("installed/search empty states and GitHub origin modal are usable at 520px; Escape restores focus without a download")

        click("marketplace_tab:explore")
        state_when(lambda value: value["marketplace"]["tab"] == "Explore", "Explore tab did not open")
        capture("marketplace-explore-initial-520.png")
        search("demo")
        assert not any(path.startswith("/api/search") for path in origin.requests), "typing must not start network discovery"
        click("marketplace_explore_search")
        state_when(lambda value: value["marketplace"]["explore"]["busy"], "real search worker loading state missing")
        capture("marketplace-explore-loading-520.png")
        state_when(lambda value: not value["marketplace"]["explore"]["busy"]
                   and value["marketplace"]["explore"]["results"] is not None, "HTTP fixture search did not finish")
        remote_id = "skills-sh:example/skills:demo"
        assert market()["explore"]["results"]["skills"] == [{"id": remote_id, "name": "demo", "source": "example/skills", "installs": 23}]
        assert item(remote_id) is None, "search must not install"
        capture("marketplace-explore-results-520.png")
        click(f"marketplace_details:{remote_id}")
        state_when(lambda value: value["marketplace"]["selected_item"] == remote_id, "remote result details missing")
        assert "example/skills" in text(observation())
        assert reachable(observation(), f"marketplace_open_source:{remote_id}")
        capture("marketplace-explore-provenance-520.png")
        keys("-k", "Escape")
        click("marketplace_scope_workspace")
        click(f"marketplace_install:{remote_id}")
        state_when(lambda value: not value["marketplace"]["busy"] and item(remote_id) is not None
                   and item(remote_id)["installed_scope"] == "Workspace", "remote fixture did not install")
        remote_files = list((root / "state/marketplace/installed/workspaces").glob("*/demo/SKILL.md"))
        assert len(remote_files) == 1
        remote_path = remote_files[0]
        assert remote_path.read_bytes() == origin.files("a")["skills/demo/SKILL.md"]
        assert (remote_path.parent / "references/info.txt").read_bytes() == origin.files("a")["skills/demo/references/info.txt"]
        click("marketplace_tab:installed")
        search("demo")
        click(f"marketplace_toggle:{remote_id}")
        state_when(lambda value: not value["marketplace"]["busy"] and not item(remote_id)["enabled"], "remote disable failed")
        origin.version = "b"
        click("marketplace_check_updates")
        state_when(lambda value: not value["marketplace"]["update_checking"]
                   and any(update["id"] == remote_id for update in value["marketplace"]["updates"]), "version check did not discover new revision")
        assert remote_path.read_bytes() == origin.files("a")["skills/demo/SKILL.md"], "checking updates must not mutate installation"
        capture("marketplace-update-available-520.png")
        click(f"marketplace_details:{remote_id}")
        state_when(lambda value: value["marketplace"]["selected_item"] == remote_id, "installed update details missing")
        scroll(24, "modal_body:marketplace_details_close")
        state_when(lambda value: "a" * 40 in text(value) and "b" * 40 in text(value), "both installed and available revisions must be reachable in detail scroll")
        assert reachable(observation(), f"marketplace_details_update:{remote_id}"), "fixed update action moved outside modal while inspecting revisions"
        capture("marketplace-update-review-520.png")
        click(f"marketplace_details_update:{remote_id}")
        state_when(lambda value: not value["marketplace"]["busy"] and not value["marketplace"]["status_error"]
                   and remote_path.read_bytes() == origin.files("b")["skills/demo/SKILL.md"], "explicit update did not replace installed bundle")
        assert not item(remote_id)["enabled"] and item(remote_id)["installed_scope"] == "Workspace"
        assert (remote_path.parent / "references/info.txt").read_bytes() == origin.files("b")["skills/demo/references/info.txt"]
        assert not any(update["id"] == remote_id for update in market()["updates"])
        capture("marketplace-updated-520.png")
        keys("-k", "Escape")
        click(f"marketplace_uninstall:{remote_id}")
        click(f"marketplace_uninstall_confirm:{remote_id}")
        state_when(lambda value: not value["marketplace"]["busy"] and item(remote_id) is None, "remote uninstall failed")
        assert not remote_path.exists()
        checks.append("loopback HTTP discovery preserves source and installs count; explicit install copies complete bundle; update check is read-only and explicit update replaces revision while preserving scope and disabled state")

        scroll(-100)
        click("marketplace_tab:explore")
        search("demo")
        keys("-k", "Return")
        state_when(lambda value: not value["marketplace"]["explore"]["busy"]
                   and value["marketplace"]["explore"]["results"] is not None, "cached discovery did not return")
        origin.offline = True
        click("marketplace_refresh")
        state_when(lambda value: not value["marketplace"]["explore"]["busy"]
                   and value["marketplace"]["explore"]["results"] is not None
                   and value["marketplace"]["explore"]["results"]["stale"], "offline refresh did not preserve stale cache explicitly")
        assert "Copia guardada sin conexión" in text(observation())
        capture("marketplace-explore-offline-520.png")
        search("not-cached")
        click("marketplace_explore_search")
        state_when(lambda value: not value["marketplace"]["explore"]["busy"]
                   and value["marketplace"]["explore"]["error"], "uncached offline query did not show error")
        assert not any(identifier.startswith("marketplace_install:") and region["hit"] == identifier
                       for identifier, region in observation()["regions"].items())
        assert reachable(observation(), "marketplace_explore_retry")
        capture("marketplace-explore-error-520.png")
        origin.offline = False
        click("marketplace_explore_retry")
        state_when(lambda value: not value["marketplace"]["explore"]["busy"]
                   and value["marketplace"]["explore"]["results"] is not None, "explicit retry did not recover")
        assert not market()["explore"]["results"]["skills"]
        capture("marketplace-explore-empty-520.png")
        click("marketplace_tab:recommended")
        search("")
        checks.append("offline refresh labels cached results honestly; uncached offline search shows retry without stale install controls; recovery empty result remains usable")

        click("marketplace_add_menu")
        click("marketplace_source_kind:local")
        state_when(lambda value: value["focused"] == "marketplace_source_draft", "local origin modal missing autofocus")
        keys(str(root / "missing-source"))
        click("marketplace_source_submit")
        state_when(lambda value: not value["marketplace"]["busy"]
                   and value["marketplace"]["status_error"]
                   and value["marketplace"]["source_kind"] == "Local", "invalid source did not produce a recoverable error")
        assert not market()["sources"]
        capture("marketplace-local-origin-error-520.png")
        click("marketplace_source_draft")
        keys("-M", "ctrl", "-k", "a", "-m", "ctrl")
        keys(str(skill.parent))
        capture("marketplace-local-origin-520.png")
        click("marketplace_source_submit")
        value = state_when(lambda value: not value["marketplace"]["busy"]
                           and len(value["marketplace"]["sources"]) == 1, "local source worker did not complete")
        source_id = value["marketplace"]["sources"][0]["id"]
        own = next(entry for entry in market()["items"] if entry["name"] == "native-fixture-skill")
        item_id = own["id"]
        search("native-fixture-skill")
        assert not any(reachable(observation(), f"marketplace_install:{identifier}") for identifier in starter_ids)
        click("marketplace_scope_workspace")
        click(f"marketplace_install:{item_id}")
        state_when(lambda value: not value["marketplace"]["busy"]
                   and any(entry["id"] == item_id and entry["installed_scope"] == "Workspace" and entry["enabled"]
                           for entry in value["marketplace"]["items"]), "local skill did not install in project scope")
        installed = list((root / "state/marketplace/installed/workspaces").glob("*/native-fixture-skill/SKILL.md"))
        assert len(installed) == 1 and installed[0].read_text() == skill_text
        assert not list((root / "state/marketplace/installed/user").glob("*/SKILL.md"))
        capture("marketplace-installed-project-520.png")
        click("marketplace_tab:installed")
        click(f"marketplace_toggle:{item_id}")
        state_when(lambda value: not value["marketplace"]["busy"] and not item(item_id)["enabled"], "disable did not persist")
        capture("marketplace-disabled-520.png")
        checks.append("pointer/keyboard add-source, query, project install, installed filter and disable operate on the real local store with exact file contents")

        stop()
        launch(env)
        resize(1000, 820)
        own = item(item_id)
        assert own and own["installed_scope"] == "Workspace" and not own["enabled"]
        search("native-fixture-skill")
        click("marketplace_tab:installed")
        click(f"marketplace_toggle:{item_id}")
        state_when(lambda value: not value["marketplace"]["busy"] and item(item_id)["enabled"], "enable did not persist")
        capture("marketplace-enabled-reopened-1000.png")
        checks.append("reopening the private native app preserves project installation and disabled state; reactivation works")

        click(f"marketplace_uninstall:{item_id}")
        state_when(lambda value: value["marketplace"]["pending_uninstall"] == item_id, "uninstall confirmation missing")
        capture("marketplace-uninstall-confirm.png")
        keys("-k", "Escape")
        state_when(lambda value: value["marketplace"]["pending_uninstall"] is None, "Escape did not cancel uninstall")
        assert installed[0].is_file() and item(item_id)["installed_scope"] == "Workspace"
        click(f"marketplace_uninstall:{item_id}")
        click(f"marketplace_uninstall_confirm:{item_id}")
        state_when(lambda value: not value["marketplace"]["busy"] and item(item_id)["installed_scope"] is None, "uninstall did not finish")
        assert not installed[0].exists() and (skill / "SKILL.md").read_text() == skill_text
        click("marketplace_tab:recommended")
        reveal(f"marketplace_remove_source:{source_id}")
        click(f"marketplace_remove_source:{source_id}")
        state_when(lambda value: value["marketplace"]["pending_remove_source"] == source_id, "source removal confirmation missing")
        capture("marketplace-source-remove-confirm.png")
        click(f"marketplace_remove_source_confirm:{source_id}")
        state_when(lambda value: not value["marketplace"]["busy"] and not value["marketplace"]["sources"], "source removal did not finish")
        assert item(item_id) is None and {entry["id"] for entry in market()["items"]} == starter_ids
        assert (skill / "SKILL.md").read_text() == skill_text
        search("")
        capture("marketplace-final-catalog-1000.png")
        checks.append("uninstall cancellation preserves files; confirmed uninstall and origin removal affect only installed copies and registry, preserving fixture source and starter catalog")
        ide_flow(starter_ids)
        assert all(entry["installed_scope"] is None for entry in market()["items"])
        assert not (root / "state/provider-credentials").exists()
        report = {"passed": checks, "fixtures": fixtures, "geometry": geometry,
                  "blocked_external_hosts": sorted(set(proxy_requests)), "fixture_http_requests": origin.requests, **desktop_metadata(), "fps_claim": None}
        (options.output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
        (options.output / "failure.json").unlink(missing_ok=True)
        (options.output / "marketplace-failure.png").unlink(missing_ok=True)
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        (options.output / "failure.json").write_text(json.dumps({"passed": checks, "error": str(error), "observation": observation()}, indent=2, ensure_ascii=False))
        if process and active().get("pid") == process.pid:
            capture("marketplace-failure.png")
        raise
    finally:
        stop()
        proxy.shutdown()
        proxy.server_close()
        origin.close()
        if json.loads(run("hyprctl", "activeworkspace", "-j"))["id"] == workspace:
            run("hyprctl", "eval", f"hl.dispatch(hl.dsp.focus({{workspace={previous_workspace}}}))")


if __name__ == "__main__":
    main()
