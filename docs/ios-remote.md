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
- Each phone has its own random token; only its SHA-256 is stored
  (`<data>/remote/devices.json`). Settings → Remoto lists each phone with when
  it was last seen, and "Quitar acceso" revokes it at once, open streams
  included.
- A pairing code works once, for five minutes, and only the last one shown:
  closing it or making another withdraws it. The PC says so when a phone
  pairs. Attempts are limited per address.
- "Always allow" from the phone saves exactly the command prefix the phone
  was shown (`always` in each pending request), or nothing. A request whose
  arguments are too large to show on the phone can only be refused.
- Connections are capped (64, 8 per address) before TLS, idle clients time
  out, each phone has at most four streams, uploads share a 200 MB quota and
  the window serves a few commands per frame.
- `/.well-known/openade` names nothing: only the API version, the server id
  and the certificate's fingerprint, which TLS shows anyway.
- Only projects the window knows can be read or written, never the
  filesystem root; file and Git edits check the revision the phone saw.

### Reaching the PC

The pairing code carries every route, the likeliest first:

1. The local network (`https://192.168.x.y:7787`).
2. The PC's stable global IPv6 address, if it has one (not the privacy
   address it goes out with, which changes every day). From outside it works
   when the router lets incoming IPv6 through to the PC. If the provider
   changes the network prefix, the phone learns the new address the next time
   it reaches the PC some other way.
3. A public address the person typed (a DNS name or IP, with a port they
   forwarded on their router).
4. The router's public IPv4 when "Abrir el puerto en el router (UPnP)" is on and
   the router agrees. When the router's own address is private or shared
   (CGNAT), Settings says so instead of offering it.

The PC's own firewall must let TCP 7787 in, for IPv4 and IPv6 (on NixOS,
`networking.firewall.allowedTCPPorts`). From the PC itself the port answers
even when it is closed to everyone else, so try it from the phone.

The phone tries the routes and keeps the first that answers as this PC (same
server id and certificate). On the developer's network (2026-10-18) the router
does not answer UPnP, so from outside only IPv6 or a forwarded port can work.

### Push notices

The phone registers its APNs token with the PC. The PC signs with the Apple
Push key in `<data>/remote/push` (`apns.p8` and `config.json` with `enabled`,
`team_id`, `key_id` and `topic` `dev.openade.remote`) and sends: a permission
or a question waiting, a turn that finished or failed. The lock screen shows
"openADE" and what happened, never the project, the task or the conversation. Nothing is sent while the window has
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
| POST | `/v2/tasks/{id}/permissions/{request}` | `allow`, `allow_session`, `allow_always` with the `always` prefix shown, `deny` |
| POST | `/v2/tasks/{id}/answers/{request}` | answer the agent's questions |
| PATCH | `/v2/tasks/{id}` | title, pinned, archived |
| POST | `/v2/tasks/{id}/read` | mark read |
| POST | `/v2/uploads` | a file to attach (body, `X-Filename`) |
| POST/DELETE | `/v2/notifications/devices` | the phone's APNs token |
| … | `/v2/projects/{id}/files`, `/file`, `/diff`, `/git/*` | files and Git |

Mutations carry a `request_id`, per device; the same id with the same request
gets the same answer again, and a retry while the first is still with the
window waits for it instead of running the command twice.

## The app

`apps/ios` is SwiftUI, iOS 17 and later, with no third-party packages; on the
iPhone it is called openADE. See `apps/ios/BUILD.md` for the project generator
and EAS builds.

### On the phone

- **Pairing.** The QR code is the raw JSON payload; the app registers no URL
  scheme, so no other app can receive a pairing secret, and a code only enters
  through the app's own scanner or paste field (an older
  `openade-remote://pair#…` text is still accepted there). The payload is
  untrusted input: version 2, 1–8 `https` routes, a 64-digit lowercase hex
  fingerprint and secret, UUIDs (compared lowercase) and an expiry in the
  future; the label loses control and bidi characters. The review screen shows
  the PC's label and the first 16 digits of the fingerprint
  (`ABCD-1234-EF56-7890`) to compare with Settings → Remoto; a PC that shares a
  saved PC's name shows as "Nueva PC" with both codes. A saved PC never gets
  another certificate by pairing again: it must be forgotten first.
- **Pinning.** Every request, the stream and uploads go through one URLSession
  per fingerprint whose delegate accepts the TLS handshake only if SHA-256 of
  the leaf certificate's DER is the pinned value; redirects are refused. The
  token is only sent after `/.well-known/openade` answered as API 2 with the
  same server id and fingerprint. An event of the stream is capped at 4 MB.
- **Permissions.** The card shows the whole command (program and every
  argument, quoted as a shell needs), every other argument such as `env` or
  `elevated`, and always offers the full arguments. "Permitir siempre" names
  the exact prefix the PC will keep allowing; for commands, the per-task grant
  reads "Permitir todos los comandos en esta tarea".
- **Storage.** Tokens, routes and pins live in one Keychain item
  (`WhenUnlockedThisDeviceOnly`); a reinstall starts unpaired. The last task
  list and twelve recent conversations per PC are cached with complete file
  protection for offline reading. Forgetting a PC deletes them and first asks
  that PC to drop this phone's push token; a revoked PC's cache, conversations
  and list are deleted at once.
- **Lock.** "Bloquear con Face ID" (on by default) starts locked and asks for
  Face ID or the passcode when a paired app opens and after five minutes away;
  turning it off asks too. The lock and the app-switcher cover are a window
  above every sheet and alert, the keyboard is dismissed under it, and a tapped
  notice waits for the unlock. Links in agent text open web pages only, after
  showing the full address; copied code stays on this iPhone for two minutes.
- **Connection.** On launch, on returning and on network changes the phone
  probes every route in parallel (3 s) and keeps the likeliest that answers as
  this PC; `/v2/overview` refreshes the learned routes. One stream is open
  while the app is in the foreground, following the task on screen from the
  last event the phone has, with jittered backoff from 0.5 s to 30 s. A small
  pill says "Conectando", "Reconectando", "Sin conexión · mostrando lo guardado"
  or "Acceso revocado" instead of blocking the screen; a 401 means revoked.

### Screens

- **Tareas**: the PC's sidebar — pinned first, then by project — with agent,
  time, "Trabajando", "Te necesita", queued messages and unread dot; swipe to
  pin, archive or rename; search; the title switches between PCs.
- **Tarea**: Markdown answers streaming in, tool calls grouped by kind
  ("Leyó 4 archivos · Ejecutó 1 comando · Editó 2 archivos"), finished turns
  folded behind "Trabajó 2 min", "Cargar anterior". A permission ("Permitir una
  vez", "Rechazar", "Permitir siempre" with the exact prefix, or "Permitir en
  esta tarea") or a question sits above the composer. The composer attaches
  photos and files, dictates on device, turns into Stop while the task runs
  with an empty draft and says "Se enviará al terminar" when it would queue.
  Changes and files of the project open from the header. CLI tasks show their
  state only.
- **Nueva tarea**: project, agent, Construir/Planear, prompt.
- **Ajustes**: paired PCs, pairing another, notifications, Face ID, appearance.
- iPad shows the list beside the open task.

GitHub Actions builds it on every push to `main` or a `wt/remote-*` branch that
touches `apps/ios` (`.github/workflows/ios.yml`): Debug and Release simulator
builds, XCTest, and screenshots of every screen's debug fixture in dark and
light appearance (artifact `ios-reports`). That is the Swift compiler this
project has outside a Mac.

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
