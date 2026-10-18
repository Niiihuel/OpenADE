# openADE Remote for iPhone

The iPhone app follows and drives the tasks of openADE on the person's own
PC: the native agent, ACP agents (OpenCode, Grok…) and CLI tasks (Claude Code,
Codex) in a terminal. There is no cloud in between: the openADE window itself
serves the phone over HTTPS. The design comes from reading t3code and
OpenChamber (`docs/remote-plan.md`).

## On the PC

Settings → Remoto turns it on. The window then runs an HTTPS server on port
7787 (configurable) on every address, IPv4 and IPv6.

- **One owner.** The server lives in the window (`crates/openide-remote`,
  started by `apps/agents-window/src/remote_app.rs`). A task the phone starts is
  an ordinary task of the window: the same runner, sidebar, queue and journal.
- **Reads come from disk.** The task list is the window's sidebar, published to
  the server; a task's conversation and its live stream are read from its
  journal (`<data>/sessions/<id>.jsonl`), which is also the replay buffer: a
  phone that reconnects names the last event it has and gets what followed.
- **Commands go through the window.** Starting a task, sending a message,
  stopping, answering a permission or a question, renaming, pinning and
  archiving are queued for the window, which acts on the task the way a person
  would: it opens the task, uses the same control and opens again what was
  open. A message for a busy task waits in the task's queue and is sent when the
  turn ends, on screen or off it.
- **The window keeps working while hidden.** On Wayland a window on another
  workspace or behind a screen that is off draws nothing, and winit holds its
  redraws. The window runs through its own event loop
  (`apps/agents-window/src/background.rs`) and every wake runs the work that
  draws nothing: turns, notices, delegated calls and the phone's commands.

### Security

- The PC makes its own TLS certificate once (`<data>/remote/tls`). No authority
  signs it: the phone pins its SHA-256, which comes in the pairing code, and
  never sends its token to a server whose certificate does not match.
- Each phone has its own 256-bit token; only its SHA-256 is stored
  (`<data>/remote/devices.json`). Settings → Remoto lists each phone with when
  it was last seen, and "Quitar acceso" revokes it at once, open streams
  included.
- A pairing code works once, for ten minutes; attempts are rate-limited.
- Only projects the window knows can be read or written; file and Git edits
  check the revision the phone saw.

### Reaching the PC

The pairing code carries every route, the likeliest first:

1. The local network (`https://192.168.x.y:7787`).
2. The PC's global IPv6 address, if it has one. From outside it works when the
   router lets incoming IPv6 through to the PC.
3. A public address the person typed (a DNS name or IP, with a port they
   forwarded on their router).
4. The router's public IPv4 when "Abrir el puerto en el router (UPnP)" is on and
   the router agrees. When the router's own address is private or shared
   (CGNAT), Settings says so instead of offering it.

The phone tries the routes and keeps the first that answers as this PC (same
server id and certificate). On the developer's network (2026-10-18) the router
does not answer UPnP, so from outside only IPv6 or a forwarded port can work.

### Push notices

The phone registers its APNs token with the PC. The PC signs with the Apple
Push key in `<data>/remote/push` (`apns.p8` and `config.json` with `enabled`,
`team_id`, `key_id` and `topic` `dev.openade.remote`) and sends: a permission
or a question waiting, a turn that finished or failed. The title is the
project and task, never the conversation. Nothing is sent while the window has
the keyboard focus: the person is at the PC.

### API

`/.well-known/openade` and `POST /v2/pair` are open; everything else needs the
phone's bearer token.

| Method | Path | What |
|---|---|---|
| GET | `/v2/overview` | server, tasks, projects, agents |
| GET | `/v2/stream?task=&after=` | SSE: `overview`, `events`, `pending`, `reset` |
| GET | `/v2/tasks/{id}/history?before=` | a window of the conversation, its pending requests, the cursor |
| POST | `/v2/tasks` | start a task: project, agent, prompt, files, plan |
| POST | `/v2/tasks/{id}/messages` | send now or queue (`delivery`) |
| POST | `/v2/tasks/{id}/stop` | stop its turn |
| POST | `/v2/tasks/{id}/permissions/{request}` | `allow`, `allow_always`, `deny` |
| POST | `/v2/tasks/{id}/answers/{request}` | answer the agent's questions |
| PATCH | `/v2/tasks/{id}` | title, pinned, archived |
| POST | `/v2/tasks/{id}/read` | mark read |
| POST | `/v2/uploads` | a file to attach (body, `X-Filename`) |
| POST/DELETE | `/v2/notifications/devices` | the phone's APNs token |
| … | `/v2/projects/{id}/files`, `/file`, `/diff`, `/git/*` | files and Git |

Mutations carry a `request_id`; the same id with the same request gets the
same answer again.

## The app

`apps/ios` is SwiftUI, iOS 17 and later, with no third-party packages. See
`apps/ios/BUILD.md` for the project generator and EAS builds.

GitHub Actions builds it on every push to `main` or a `wt/remote-*` branch that
touches `apps/ios` (`.github/workflows/ios.yml`): Debug and Release simulator
builds, XCTest, and screenshots of the debug fixtures (artifact `ios-reports`).
That is the Swift compiler this project has outside a Mac.

## Checks

```sh
nix-shell --run "cargo test -p openide-remote"
nix-shell --run "cargo test -p agents-window remote"
# Settings → Remoto as PNG:
OPENADE_VISUAL_DIR=<dir> nix-shell --run "cargo test -p agents-window export_remote_visual_review -- --ignored"
# What this network's router says, without opening anything:
nix-shell --run "cargo test -p openide-remote this_networks_router -- --ignored --nocapture"
```

## Not verified yet

- A real iPhone against a real PC: pinning, routes from outside and push
  delivery have only run in tests and the simulator.
- Turning the old daemon's sessions (`<data>/remote/sessions`) into tasks of the
  window: they stay on disk, unseen.
