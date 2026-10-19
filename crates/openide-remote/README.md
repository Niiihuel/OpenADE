# openide-remote

The HTTPS server the openADE window runs so a paired iPhone can follow and
drive its tasks. The window starts it from Settings → Remoto
(`apps/agents-window/src/remote_app.rs`); there is no binary of its own.

| Module | What it does |
|---|---|
| `server` | The router, authentication, per-address limits and the TLS accept loop |
| `bridge` | What the server and the window share: commands for the window, the window's live task list, notices |
| `stream` | One SSE stream per phone: the task list and the open task's journal as it grows |
| `tasks` | The task list and a task's history, paged back from its last prompts |
| `projection` | The journal events a phone may see, and what each pending request would allow |
| `tls` | The PC's own certificate, which the phone pins |
| `pairing` | One-use pairing codes and the JSON the QR code holds |
| `auth` | Device tokens, stored hashed, and revocation |
| `push` | APNs notices to registered phones |
| `network`, `upnp` | The addresses a phone can reach the PC at |
| `workspace`, `git_workspace`, `diff` | A project's files and Git, opened without following links |

Behaviour, security rules and the API are in `docs/ios-remote.md`.

```sh
nix-shell --run "cargo test -p openide-remote"
```
