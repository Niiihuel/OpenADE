# openADE for iPhone

The iPhone app (`apps/ios`, SwiftUI, iOS 17+) follows and drives the tasks of
the openADE window on a PC: the native agent, ACP agents (OpenCode, Grok…)
and, read-only, CLI tasks that run in a terminal (Claude Code, Codex). The
window itself serves it over HTTPS (`crates/openide-remote`, default port
7787); there is no separate daemon. The plan and the comparison with t3code
and OpenChamber are in `docs/remote-plan.md`.

## Trust

- **Pairing.** openADE → Ajustes → Remoto shows a QR code (also a link):
  `openade-remote://pair#<base64url JSON>` with version 2, the PC's routes
  (LAN, global IPv6, an optional public address), the SHA-256 fingerprint of
  its own certificate, its `server_id`, and a one-use secret valid for ten
  minutes. The phone shows the PC's name and the first 16 hex digits of the
  fingerprint (`ABCD-1234-EF56-7890`) to compare with the PC before pairing.
- **Pinning.** Every request goes through a URLSession whose delegate accepts
  the TLS handshake only if SHA-256 of the leaf certificate's DER equals the
  pinned fingerprint; hostname and CA checks are ignored because the pin is the
  trust anchor. Redirects are refused. The bearer token is only ever sent on a
  pinned session, after `/.well-known/openade` answered with the same
  `server_id` and fingerprint.
- **Storage.** Tokens, routes and pins of every paired PC live in one Keychain
  item (`WhenUnlockedThisDeviceOnly`). The last task list and up to twelve
  recent conversations per PC are cached with complete file protection so the
  app can be read offline; forgetting a PC deletes them.
- **Revocation.** Any 401 on an authenticated request marks the PC as revoked
  and stops using its token; the app asks to pair again.
- **Lock.** "Bloquear con Face ID" (on by default) asks for Face ID or the
  passcode when the app opens and after five minutes in the background;
  content stays blurred meanwhile, and the app switcher never shows it.
  Notifications carry only the task title and cannot approve anything.

## Connection

On launch, on returning to the foreground and on network changes the app
probes every saved route in parallel (3 s, pinned) and uses the likeliest one
that answers as this PC; `/v2/overview` then refreshes the learned routes.
While the app is in the foreground it keeps one server-sent event stream open
(`/v2/stream`), watching the task on screen from the last event it has; it
reconnects with jittered backoff from 0.5 s to 30 s and closes the stream in
the background. A small floating pill says "Conectando", "Reconectando", "Sin
conexión · mostrando lo guardado" or "Acceso revocado" instead of blocking the
screen.

Mutations carry a request id; repeating the same draft repeats the id, so the
PC answers a retry instead of acting twice. Messages sent while a turn runs are
queued on the PC and survive the phone locking.

## Screens

- **Tareas**: the PC's sidebar — pinned first, then by project — with agent,
  time, "Trabajando", "Te necesita", queued count and unread dot. Swipe to pin,
  archive or rename; search; pull to refresh. The title switches between PCs.
- **Tarea**: Markdown answers that stream as they arrive, tool calls grouped
  by kind ("Leyó 4 archivos · Editó 2 archivos · Ejecutó 1 comando"), finished
  turns folded behind "Trabajó 2 min", "Cargar anterior" for older history.
  A permission ("Permitir una vez", "Permitir siempre", "Rechazar") or an
  `ask_user` question sits above the composer; the first answer wins, from the
  PC or the phone. The composer attaches photos and files, dictates on device,
  turns into Stop when the task runs and the draft is empty, and says "Se
  enviará al terminar" when it would queue. Changes and files of the task's
  project open from the header menu. CLI tasks show their state only.
- **Nueva tarea**: project, agent, Construir/Planear and the prompt.
- **Ajustes**: paired PCs, pairing another one, notifications (when the PC has
  push configured), Face ID lock and appearance.
- iPad uses a split view: the list beside the open task.

## Verification

GitHub Actions (`.github/workflows/ios.yml`, macOS runner) runs
`apps/ios/scripts/build-simulator.sh` on pushes to `wt/remote-*`: Debug and
Release builds, XCTest, and DEBUG-only `--ui-fixture=<scene>` screenshots of
every screen in dark and light appearance, uploaded as `ios-reports`. Fixtures
use local data and never a PC. Pinning against the real server, APNs delivery,
camera, dictation and Face ID still need a physical iPhone.
