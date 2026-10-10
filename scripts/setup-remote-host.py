#!/usr/bin/env python3
"""Configure a private remote host from the existing desktop Codex selection.

Only reads the non-secret native-provider.json. Credential access and refresh
remain in openide-connections. Device tokens are written privately, never printed.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile


REPO = Path(__file__).resolve().parents[1]
SERVICE_MARKER = "# Managed by openADE Remote setup"


def desktop_root():
    explicit = os.environ.get("OPENIDE_DATA_ROOT")
    if explicit:
        root = Path(explicit).expanduser()
        if not root.is_absolute():
            raise ValueError("OPENIDE_DATA_ROOT must be absolute")
        return root
    base = Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local/share")))
    return base / "openide"


def private_directory(path):
    if path.is_symlink():
        raise ValueError("Private state cannot be a symbolic link")
    path.mkdir(parents=True, exist_ok=True)
    path.chmod(0o700)


def separate_state(path, data_root):
    path = path.expanduser().absolute()
    if path.is_symlink():
        raise ValueError("Remote state must not be a symbolic link")
    resolved = path.resolve()
    if resolved == data_root.resolve():
        raise ValueError("Remote state must be separate from the desktop data root")
    return resolved


def write_new_private(path, value):
    """Exclusive creation: a second setup never replaces an existing config."""
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, 0o600)
    with os.fdopen(descriptor, "w") as output:
        json.dump(value, output, ensure_ascii=False, indent=2)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())


def configuration(data_root, projects, allow_commands=False, read_only=False, model=None, port=8787, image_input=False):
    if not 1024 <= port <= 65535:
        raise ValueError("Choose an unprivileged TCP port between 1024 and 65535")
    settings_file = data_root / "native-provider.json"
    if settings_file.stat().st_size > 64 * 1024:
        raise ValueError("Desktop provider settings exceed the allowed size")
    settings = json.loads(settings_file.read_text())
    if settings.get("provider_id") != "codex":
        raise ValueError("Automatic setup currently imports the desktop Codex connection; configure other providers explicitly")
    selected_model = model or settings.get("model_id", "")
    if not isinstance(selected_model, str) or not selected_model.strip() or len(selected_model) > 256:
        raise ValueError("Select a model in the desktop app before configuring the host")
    registered = []
    seen = set()
    for project in projects:
        root = Path(project).expanduser().resolve(strict=True)
        if not root.is_dir():
            raise ValueError("A project must be a directory")
        if root in seen:
            continue
        seen.add(root)
        registered.append({
            "id": hashlib.sha256(os.fsencode(root)).hexdigest()[:20],
            "name": root.name,
            "path": str(root),
        })
    config = {
        "version": 1,
        "bind": f"127.0.0.1:{port}",
        "provider": {
            "id": "codex",
            "protocol": "responses",
            "endpoint": "https://api.openai.com/v1",
            "model": selected_model.strip(),
            "api_key_env": None,
            "desktop_data_root": str(data_root.resolve()),
        },
        "projects": registered,
        "allow_write": not read_only,
        "allow_commands": allow_commands and not read_only,
        "command_isolation": "host_unrestricted",
    }
    if image_input:
        config["provider"]["image_input"] = True
    return config


def systemd_quote(value):
    if any(character in str(value) for character in "\n\r\x00"):
        raise ValueError("Invalid systemd argument")
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%") + '"'


def service_unit(binary, state):
    return f"""{SERVICE_MARKER}
[Unit]
Description=openADE remote task host
After=network-online.target

[Service]
Type=simple
ExecStart={systemd_quote(binary)} --state {systemd_quote(state)} serve
Restart=on-failure
RestartSec=3
TimeoutStopSec=15
KillMode=mixed
UMask=0077
NoNewPrivileges=yes

[Install]
WantedBy=default.target
"""


def install_service(binary, state):
    if sys.platform != "linux" or shutil.which("systemctl") is None:
        raise ValueError("Automatic service installation requires Linux and a user systemd session")
    destination = state / "bin"
    installed = destination / "openide-remote"
    units = Path.home() / ".config/systemd/user"
    unit = units / "openade-remote.service"
    expected = service_unit(installed, state)
    # Validate before touching the executable an existing service may use.
    if unit.is_symlink() or (unit.exists() and unit.read_text() != expected):
        raise ValueError("An existing service has a different configuration; its unit and binary were preserved")
    private_directory(destination)
    descriptor, temporary = tempfile.mkstemp(prefix=".remote-", dir=destination)
    os.close(descriptor)
    try:
        shutil.copyfile(binary, temporary)
        os.chmod(temporary, 0o700)
        os.replace(temporary, installed)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    units.mkdir(parents=True, exist_ok=True)
    unit.write_text(expected)
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
    subprocess.run(["systemctl", "--user", "enable", "openade-remote.service"], check=True)
    subprocess.run(["systemctl", "--user", "restart", "openade-remote.service"], check=True)
    print("User service installed and restarted: openade-remote.service (loopback only)")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", action="append", required=True, help="Authorized project; repeat to add more")
    parser.add_argument("--desktop-data-root", type=Path)
    parser.add_argument("--state", type=Path)
    parser.add_argument("--binary", type=Path, default=REPO / "target/debug/openide-remote")
    parser.add_argument("--model", help="Optional explicit model; otherwise preserves desktop selection")
    parser.add_argument("--port", type=int, default=8787)
    parser.add_argument("--allow-commands", action="store_true", help="Offer agent commands with per-command approval")
    parser.add_argument("--read-only", action="store_true", help="Disable write and command tools")
    parser.add_argument("--image-input", action="store_true", help="Enable photo input only after verifying this exact model supports images")
    parser.add_argument("--device-name", default="iPhone")
    parser.add_argument("--install-service", action="store_true", help="Install and restart the Linux user service; finish active turns first")
    args = parser.parse_args(argv)
    data_root = (args.desktop_data_root or desktop_root()).expanduser().resolve()
    state = separate_state(args.state or data_root / "remote", data_root)
    binary = args.binary.expanduser().resolve(strict=True)
    config = configuration(data_root, args.project, args.allow_commands, args.read_only, args.model, args.port, args.image_input)
    private_directory(state)
    path = state / "config.json"
    if path.exists() or path.is_symlink():
        if path.is_symlink() or stat.S_IMODE(path.stat().st_mode) & 0o077:
            raise ValueError("Existing host config is not a private regular file")
        if json.loads(path.read_text()) != config:
            raise ValueError("Existing host config differs; it was preserved. Review it locally before changing host permissions or projects")
    else:
        write_new_private(path, config)
    subprocess.run([str(binary), "--state", str(state), "check"], check=True)
    pairing = state / "iphone-pairing.json"
    if not pairing.exists():
        subprocess.run([str(binary), "--state", str(state), "device-add", args.device_name, "--output", str(pairing)], check=True)
    else:
        print("Existing pairing file preserved; use the host CLI for additional devices or revocation")
    print(f"Host config: {path}")
    print(f"Private device enrollment: {pairing} (token contents not printed)")
    if args.install_service:
        install_service(binary, state)
    else:
        print(f"Ready to run: {binary} --state {state} serve")
    print(f"For the iPhone, configure a private HTTPS endpoint that forwards to {config['bind']}.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Remote setup failed: {error}", file=sys.stderr)
        raise SystemExit(1)
