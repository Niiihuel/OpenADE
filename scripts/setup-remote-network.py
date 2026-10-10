#!/usr/bin/env python3
"""Manage the private Tailscale transport for openADE Remote on Linux.

Uses a dedicated userspace daemon, without changing system routing or DNS.
The account owner completes Tailscale login. Serve is private to the tailnet;
this script never enables Funnel or publishes a port to the public internet.
"""

import argparse
import json
import ipaddress
import os
from pathlib import Path
import shutil
import subprocess
import sys
from urllib.parse import urlsplit


MARKER = "# Managed by openADE Remote network setup"
UNIT = "openade-remote-network.service"


def quote(value):
    if any(char in str(value) for char in "\n\r\x00"):
        raise ValueError("Invalid service argument")
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%") + '"'


def run(arguments, **kwargs):
    return subprocess.run(arguments, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["install", "login", "status", "serve", "stop"])
    parser.add_argument("--state", type=Path)
    parser.add_argument("--tailscale", type=Path)
    parser.add_argument("--tailscaled", type=Path)
    args = parser.parse_args()
    if sys.platform != "linux":
        raise ValueError("This service helper is for Linux; other systems can use their Tailscale app")
    data_base = Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local/share")))
    data_root = Path(os.environ.get("OPENIDE_DATA_ROOT", str(data_base / "openide")))
    state = (args.state or data_root / "remote/network").expanduser().absolute()
    if state.is_symlink():
        raise ValueError("Network state must not be a symlink")
    package = state.parent / "tailscale-package/bin"
    cli = args.tailscale or shutil.which("tailscale") or package / "tailscale"
    daemon = args.tailscaled or shutil.which("tailscaled") or package / "tailscaled"
    # Nix's tailscale is a multicall symlink to tailscaled; preserve argv[0].
    cli = Path(cli).expanduser().absolute()
    if not cli.is_file():
        raise ValueError("Install Tailscale or supply --tailscale with its executable path")
    socket = state / "tailscaled.sock"
    command = [str(cli), f"--socket={socket}"]

    if args.action == "install":
        daemon = Path(daemon).resolve(strict=True)
        state.mkdir(parents=True, exist_ok=True)
        state.chmod(0o700)
        units = Path.home() / ".config/systemd/user"
        units.mkdir(parents=True, exist_ok=True)
        unit = units / UNIT
        if unit.is_symlink() or (unit.exists() and not unit.read_text().startswith(MARKER)):
            raise ValueError("An unmanaged network service already exists; it was preserved")
        text = f"""{MARKER}
[Unit]
Description=Private Tailscale transport for openADE Remote
After=network-online.target

[Service]
Type=simple
ExecStart={quote(daemon)} --tun=userspace-networking --port=0 {quote('--statedir=' + str(state))} {quote('--state=' + str(state / 'tailscaled.state'))} {quote('--socket=' + str(socket))}
Restart=on-failure
RestartSec=3
UMask=0077
NoNewPrivileges=yes

[Install]
WantedBy=default.target
"""
        if unit.exists() and unit.read_text() != text:
            raise ValueError("Existing network service differs; review it before replacing")
        unit.write_text(text)
        run(["systemctl", "--user", "daemon-reload"])
        run(["systemctl", "--user", "enable", "--now", UNIT])
        print("Private network service installed. Complete login, then enable Serve.")
    elif args.action == "login":
        run(command + ["up", "--accept-dns=false", "--accept-routes=false", "--hostname=openade-remote", "--timeout=60s"])
    elif args.action == "serve":
        status = json.loads(run(command + ["status", "--json"], capture_output=True, text=True).stdout)
        if status.get("BackendState") != "Running":
            raise ValueError("Complete Tailscale login before enabling the private HTTPS endpoint")
        config = json.loads((state.parent / "config.json").read_text())
        target = urlsplit("http://" + config["bind"])
        if not ipaddress.ip_address(target.hostname).is_loopback or target.port is None:
            raise ValueError("The private proxy requires a loopback host listener with an explicit port")
        run(command + ["serve", "--bg", "--https=443", target.geturl()])
        name = status.get("Self", {}).get("DNSName", "").rstrip(".")
        if name:
            print(f"iPhone host address: https://{name}")
    elif args.action == "status":
        result = subprocess.run(command + ["status", "--json"], capture_output=True, text=True)
        try:
            status = json.loads(result.stdout)
        except json.JSONDecodeError:
            raise ValueError("Network daemon is unavailable; install/start its user service first")
        print(json.dumps({"state": status.get("BackendState"), "dns_name": status.get("Self", {}).get("DNSName"), "health": status.get("Health", [])}, indent=2))
        return result.returncode
    else:
        run(["systemctl", "--user", "disable", "--now", UNIT])
        print("Private network service stopped; saved account state was preserved")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Remote network setup failed: {error}", file=sys.stderr)
        raise SystemExit(1)
