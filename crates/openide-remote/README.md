# OpenIDE Remote host

Headless Rust companion service for the native iOS client. The process owns
agent runs independently of the desktop window and mobile connection. It
reuses `openide-core::SessionStore`, `openide-harness::AgentRunner` and the
shared `openide-connections` Codex credential implementation.

There is **no interactive terminal**, PTY input, arbitrary HTTP command
endpoint, desktop screen control or anonymous command route. Command output
is read-only activity. A model can request `run_command` only when the local
host enables it. Commands require durable user approval unless the user chose
`full_access` for that specific turn.

## Configure and run

Use a dedicated private state directory, separate from the desktop's data and
session stores. Save `config.json` with Unix permissions `0600`; the state
directory must be private. Example using an existing desktop Codex connection:

```json
{
  "version": 1,
  "bind": "127.0.0.1:8787",
  "provider": {
    "id": "codex",
    "protocol": "responses",
    "endpoint": "https://api.openai.com/v1",
    "model": "REPLACE_WITH_YOUR_AUTHORIZED_MODEL_ID",
    "api_key_env": null,
    "desktop_data_root": "/absolute/path/to/desktop/data"
  },
  "projects": [{
    "id": "my-project",
    "name": "My project",
    "path": "/canonical/absolute/project/path"
  }],
  "allow_write": false,
  "allow_commands": false,
  "command_isolation": "host_unrestricted"
}
```

The desktop credential store remains authoritative. Refresh uses its shared
cross-process lock; credentials are never transferred to the phone. This route
is restricted to `id=codex`, `protocol=responses` and the exact official
endpoint above. Selecting `desktop_data_root` does not sign in a missing
account; connect once through desktop first. A model ID must be explicitly
selected from the account's authorized models; no fallback is invented.

Other providers can use `responses`, `chat_completions` or `anthropic` and
`api_key_env` pointing to a variable in the **host service's environment**.
Omit `desktop_data_root` for these routes. Chat Completions additionally requires
`"tool_calls": true`, an explicit declaration for that exact compatible model.
Environment keys are read by the host, never written into the config or API
responses. HTTPS is required for provider endpoints; loopback HTTP with an
explicit port is allowed for local models and tests. The app does not read
`.env` files automatically.

```sh
rtk cargo run -p openide-remote -- --state /private/openide-remote check
rtk cargo run -p openide-remote -- --state /private/openide-remote device-add iPhone --output /private/iphone-pairing.json
rtk cargo run -p openide-remote -- --state /private/openide-remote serve
```

The pairing export contains the new device ID and bearer token. Import or copy
it into the iPhone's connection settings through a trusted channel, then delete
the export. The token is never printed by the command. The server stores only
its SHA-256 digest and compares it in constant time. All provisioned devices
have the same owner access to the configured projects; per-device read-only
roles are not part of v1.

```sh
rtk cargo run -p openide-remote -- --state /private/openide-remote device-list
rtk cargo run -p openide-remote -- --state /private/openide-remote device-revoke DEVICE_ID
```

Revocation is checked on every HTTP request without restarting. It prevents
future requests; it does not undo an already accepted action.

The default listener is loopback HTTP. For the phone, place it behind HTTPS
(for example an authenticated private-network HTTPS proxy), or explicitly bind
a Tailscale/private encrypted address and use `serve --allow-remote-bind`.
Do not expose this plaintext listener directly to the public Internet. The
service does not install networking software or enable a public tunnel.

Writes and commands are disabled by default. A local config change can enable
them with `allow_write`/`allow_commands`; per-action approval is the default in Build.
The optional `approval_policy` applies only to a newly submitted turn:

- `ask` (also the omitted default): ask before writes and commands.
- `auto`: allow bounded workspace-relative text writes and valid Add/Update
  patches without file deletion or moves. Writes over 256 KiB, file deletion,
  moves, malformed edits and all commands still ask. This is a deterministic
  tool policy, not an AI risk classifier; file containment remains enforced.
- `full_access`: allow the host-enabled local tools without per-action prompts.
  It does not enable tools forbidden by configuration, change command isolation,
  grant administrator access or attach interactive command tools.

Plan remains read-only for all three policies. Unknown tools remain denied.
The selected policy is frozen with the durable request and advertised through
`turn.approval_policy`; changing a retry's policy is an idempotency conflict.
Steering, reconnecting or selecting a UI option never changes an active turn.
Explicit owner actions in the file editor and typed Git API use `allow_write`
and version checks; they do not create agent permission prompts.
Plan only advertises the harness's certified read tools. The
`host_unrestricted` command profile has the OS user's real filesystem and
network rights: a project allowlist is not a command sandbox. The optional
`workspace_write_offline` profile uses the harness's Linux/Windows isolation
and fails closed if unavailable. It is not implemented for macOS. No elevated
command or persistent input tools are attached by this service.

## HTTP v1

Every application route, including health, requires `Authorization: Bearer TOKEN`.
The sole exception is `POST /v1/pairing/complete`, authenticated by a temporary
one-time pairing secret. Mutations accept JSON, reject unknown fields and bound
body size to 64 KiB; file saving permits JSON escaping overhead up to 7 MiB,
while the decoded text itself is limited to 1 MiB.
Application errors have `{ "error": { "code": "…", "message": "…" } }`.
Invalid query/path or unsupported HTTP-method rejections may use Axum's text
response; clients should retain a generic HTTP-error fallback.

- `GET /v1/health`: string `version`, configured `model`, `provider`, capabilities
  and effective local permission/isolation settings.
- `GET /v1/projects`: `{projects:[{id,name,path}]}`.
- `POST /v1/sessions`: `{project_id,request_id,title?}`; returns the session.
- `GET /v1/sessions?project_id=ID&query=TEXT&archived=all&limit=100&cursor=CURSOR`:
  `{sessions:[…],next_cursor:string|null}`. Title/ID search, archived filtering
  (`all`, `true`, `false`), descending activity time and stable ID tie-break.
  The cursor is opaque to clients; pages are live views, not frozen snapshots.
- `GET /v1/sessions/ID`: `{id,project_id,title,status,updated_at_ms,
  active_turn_id,pending_permissions,archived,favorite,origin,can_start_turn,
  can_control_active}`. Desktop-origin sessions do not support metadata PATCH.
- `PATCH /v1/sessions/ID`: `{request_id,title?,archived?,favorite?}`.
- `POST /v1/sessions/ID/turns`: `{request_id,prompt,mode:"plan"|"build",attachment_ids?:[ID],model_id?,reasoning_effort?,approval_policy?:"ask"|"auto"|"full_access"}`;
  returns `{turn_id,status}`. Prompt limit: 32 KiB.
- `POST /v1/sessions/ID/steer`: `{request_id,turn_id,prompt}`; queues a durable
  user input for a model-safe boundary of the matching active turn.
- `POST /v1/sessions/ID/cancel`: `{request_id,turn_id}`. Explicit turn binding
  prevents a delayed cancellation from terminating a later turn.
- `POST /v1/sessions/ID/permissions/REQUEST_ID`: `{decision:"allow"|"deny"}`.
  Only a live pending request can receive a new decision. Exact retries are
  idempotent; opposite decisions return 409. The decision is synced before the
  runner is released. Permissions expire after 15 minutes and default closed.
- `GET /v1/sessions/ID/events?after=0&limit=100`: `{events,next_cursor,has_more}`.
  Poll while active. Up to 200 public events and approximately 512 KiB per page.
- `GET /v1/projects/ID/diff`: `{diff,truncated,includes_untracked:false}`. Reads
  tracked changes against HEAD, including staged and unstaged changes. New
  untracked files are not included; the mobile client must display that limit.
  A repository without a HEAD commit returns an explicit error.

Public events contain `seq`, `timestamp_ms`, `type`, and optional `text`,
`turn_id`, `message_id`, `request_id`, `tool_call_id`, `tool_name`, `arguments`,
`decision`, `is_error`. Event types are `user_message`, `assistant_delta`,
`assistant_message`, `tool_started`, `tool_output`, `tool_finished`,
`permission_requested`, `permission_resolved`, `turn_completed`, `turn_failed`,
`turn_cancelled`. Unknown future types should be ignored.

Group assistant deltas by `message_id`, not just turn: a turn may have several
model steps. A committed `assistant_message` replaces that message's deltas.
`tool_finished.text` replaces the preliminary output for its tool call.
`turn_completed.text` can repeat the final assistant response; use it as status
without appending another visible message.

The cursor counts the authoritative journal, including omitted private records.
Always persist `next_cursor`, even when the page has no visible events. Cursor
values and timestamps are unsigned integers. No opaque provider items, frozen
instructions, checkpoint payloads, credentials or runtime authority snapshots
are exported. Model output and explicitly authorized tool output are visible
to the paired owner. Large text is clipped with a visible marker. Tool arguments
over 64 KiB cannot be approved from the phone; denial is still available.

## Durability and current scope

The daemon drains the runner's bounded event channel without a mobile client.
Every native event is synced by the harness before publication or subsequent
effects. Per-session OS leases and one host-state owner lock prevent competing
writers. Stable request IDs prevent retries from starting duplicate turns.
Session creation, steering and cancellation have their own durable operation records;
an operation interrupted before acknowledgment returns conflict for local
inspection, instead of being executed again.

Disconnecting the phone does not cancel work. Closing the desktop does not
cancel these daemon-owned sessions. SIGTERM/Ctrl+C requests cancellation and
allows a short drain period. A daemon crash stops orchestration; launched
commands may have uncertain effects, and cleanup after abrupt termination
depends on OS supervision. A PC restart terminates its processes. Unfinished
journals return `interrupted`. They are **not automatically
resumed or retried**, even if a client resends the original turn ID. Completed
sessions remain readable and can receive new turns. Uncertain effects require
local inspection; a recovery UI and Resume integration are deferred.

New remote sessions are host-owned in a separate store. When an authorized
desktop data root exists, native desktop history for allowlisted projects is
also readable, without copying or repairing its journals. Desktop controls are
available only while the owning runner has a fresh heartbeat and a busy OS
lease. The daemon does not start or resume desktop-owned sessions. Per-session
`origin`, `can_start_turn` and `can_control_active` describe that distinction.
Changing the provider configuration requires a new session to keep history
provenance explicit. Root project instructions are loaded at turn admission;
the harness retains its instruction checks for subsequent tool targets.

At most four turns run concurrently. Session/journal reads currently replay
the retained history and are not optimized for very large histories. Devices
can be provisioned locally or paired with an expiring QR invitation. Public
relay, arbitrary model selection and interactive recovery are not included in
this version.

## Private attachments

`attachments.text` permits `POST /v1/projects/ID/attachments` with
`{request_id,filename,media_type,data_base64,sha256}`. The response is
`{id,filename,media_type,size_bytes,sha256}`. IDs belong to the authenticated
device and allowlisted project; they never expose a filesystem path. Upload
requests have a separate 6 MiB body limit. Text must be UTF-8 without binary
control characters, at most 256 KiB per file and 1 MiB combined per turn.
Up to eight attachments can be included in a new turn. Steering accepts text
only. Local project references can be included in the prompt without uploading.

Images require `provider.image_input: true` for the exact configured model and
a transport that supports user images. This declaration defaults to false;
verify actual model capability before enabling it. Only then is
`attachments.images` advertised. Images use the core `UserImage` validation,
accepted JPEG/PNG/WebP/static GIF formats and a 4 MiB per-file limit.
No PDF or Office extraction is provided.

Upload retries use the same request ID and immutable payload. Admitted turns
freeze text/image content in the native journal and bind the original device,
prompt and attachment IDs; a later retry does not reload mutable uploads or
start a second turn. The iOS app stores pending upload requests and selected
references in Keychain before submission. Each device/project upload store is
bounded to 128 records and 96 MiB. There is currently no automatic garbage
collection or mobile deletion endpoint; after reaching that quota, local
cleanup is required. Removing uploads does not remove admitted journal media.

## V2 workspace additions (additive `/v1` routes)

Use `health.capabilities` to discover available UI actions. The Unix host exposes
`sessions.metadata`, `models.list`, `workspace.files.read`, and `workspace.git.read`.
With `allow_write`, it additionally exposes `workspace.files.write`,
`workspace.git.stage`, and `workspace.git.commit`. Guarded workspace IO is not
implemented for non-Unix hosts; those capabilities are absent there.

- `GET /v1/projects/ID/files?path=RELATIVE&query=NAME`: `{path,entries:[{path,name,
  kind:"file"|"directory",size_bytes}],truncated}`. Without query this lists one
  directory; a query searches filenames recursively. Limits: 1,000 results,
  20,000 visited entries, depth 32. Git internals, symlinks and special files
  are excluded.
- `GET /v1/projects/ID/file?path=RELATIVE`: `{path,content,revision,language:null,
  truncated:false}`. UTF-8 text only, maximum 1 MiB; large/binary files fail
  explicitly instead of returning editable truncated text.
- `PUT /v1/projects/ID/file`: `{request_id,path,content,expected_revision}`.
  `expected_revision` is the SHA-256 returned by GET, or `null` to create a new
  file in an existing directory. Returns the saved file snapshot. Paths are
  relative and reject `..`, `.git`, symlink parents and special files.
- `GET /v1/projects/ID/git/status`: `{revision,branch,files:[{path,index_status,
  worktree_status,staged,untracked}],clean,commit:null}`. Includes staged,
  unstaged and untracked rows, and works before the first commit. Project root
  must equal the Git worktree root. At most 2,000 changed files are supported.
- `GET /v1/projects/ID/git/diff?path=RELATIVE&staged=false`: `{path,diff,truncated}`.
  Includes textual untracked files. Output is capped at 512 KiB.
- `POST /v1/projects/ID/git/stage` and `/git/unstage`:
  `{request_id,paths:[RELATIVE],expected_revision}`; return Git status.
- `POST /v1/projects/ID/git/commit`: `{request_id,message,expected_revision}`;
  returns Git status with the new `commit` ID. Only the current index is
  committed; the operation does not stage additional working files or push.
- `GET /v1/models`: `{models:[{id,name,provider,capabilities}],default_model_id}`.
  This is the actually configured model, with no account discovery calls or
  invented alternatives. `models.select` is intentionally absent.

Workspace mutations have a private durable request ledger. Repeat the exact
request ID and body after losing an ACK; never automatically invent a new ID.
`file_conflict` and `git_conflict` mean the request did not publish a change;
the client can explicitly discard that attempt and reload. `operation_uncertain`
requires inspection; the host never repeats it. Git revisions incorporate HEAD,
the index and dirty-file contents, so another edit to an already-modified file
invalidates the revision. A real `index.lock` excludes competing Git writers,
and commits publish through an expected-old-value ref transaction.

Git mutations use bounded plumbing commands without a shell, filters or hooks.
Staging accepts regular files up to 1 MiB and deletions; symlink/submodule
staging is unavailable. Commit signing and repository hooks are not run; use
local Git for workflows requiring those policies. An existing external
`index.lock` is never removed. Temporary object creation can leave unreachable
objects when a later precondition rejects publication, as with interrupted Git.

File saves serialize EAS clients, check content hashes twice and atomically
replace the file through an opened parent descriptor. New-file publication is
no-clobber. Existing-file hash checks detect normal external edits, but cannot
provide universal atomic compare-and-swap against external editors that ignore
coordination and write between the last check and rename.

## QR pairing and native push

`pairing-start --base-url HTTPS_URL --label NAME --output PRIVATE_JSON
--qr-output PRIVATE_SVG` creates an invitation lasting ten minutes. Import its
`openade-remote://pair#…` link/QR from the phone. Only the secret hash is retained before
redemption. `POST /v1/pairing/complete` takes
`{pairing_id,secret,request_id,device_name}` and returns the device bearer token.
Persist the same request ID before sending: exact retries recover the same
device, including a crash between token preparation and device insertion.
After storing the token in Keychain, `POST /v1/pairing/ID/ack` with that device's
bearer token deletes the short-lived response-secret cache. Normal device
revocation still applies. Invitation files and QR images are credentials;
transfer privately and remove after use.

`notifications.push` is advertised only when the private APNs configuration
loads and the host can dispatch notifications. `POST /v1/notifications/devices`
takes `{apns_token,environment:"production"|"sandbox",bundle_id}`; DELETE removes
only the authenticated device's registration. Approval/completion notifications
contain a generic message and opaque session ID, never prompts, tool output or
project names. Delivery is best effort; polling/replay remains authoritative.
See the `push` module for private configuration and fixed Apple endpoints.

## Verification

```sh
rtk cargo test -p openide-remote
```

The tests use temporary private stores and a real loopback HTTP model fixture,
not account credentials or a mock production backend. They exercise HTTP auth,
revocation, durable approval before writing a real temporary file, duplicate
requests, stale cancellation, disconnect/replay, interrupted admissions and
private-event filtering. No live provider calls are needed.
