# openADE Remote for iPhone

The mobile app is a native SwiftUI client for tasks running on a trusted PC.
It lets the user choose a project, send a task, inspect agent activity and code
changes, answer permission requests, and cancel the current turn. Command output
is **read-only**. There is no mobile shell, terminal keyboard, PTY input, or
arbitrary-command HTTP endpoint.

Version 3 adds shared neutral SwiftUI components, a floating composer, camera
attachments, on-device dictation, and verified model/reasoning selection to the
V2 session management, multiple PCs, QR pairing, file workspace and Git flows. It does
not yet cover the complete OpenChamber scope. A signed build is not proof of
physical-device use or complete product parity.

## Components

- `apps/ios`: SwiftUI app, networking, Keychain storage, XCTest, and native EAS
  build configuration. There is no React Native application or JavaScript UI.
- `crates/openide-remote`: authenticated HTTP service that owns agent runs,
  stores sessions on the PC, projects public events, and exposes typed file,
  Git and device operations. Disconnecting an HTTP client does not cancel its run.
- `crates/openide-connections`: the credential store and OAuth refresh logic
  shared by the desktop and remote service. Both use the same lock when rotating
  saved ChatGPT credentials. Provider credentials remain on the PC.

The first transport uses HTTPS in front of a loopback service. A private HTTPS
endpoint such as Tailscale Serve is appropriate; the iOS client rejects remote
plain HTTP and refuses redirects so its authorization header cannot move to
another host. A tunnel alone does not authorize access: every API request needs
the device bearer credential.

## Session and permission behavior

The service owns new mobile sessions independently of the desktop window. A
network disconnection leaves work running. A PC or service restart is different:
unfinished work is reported as interrupted, and uncertain external effects must
not be replayed automatically.

The app uses paginated JSON event polling, with a durable sequence cursor and
backoff on failures. It does not require a continuously connected iPhone socket.
The app stops polling in the background and catches up on return. Unknown event
types are forward-compatible. Events that contain private internal instructions
are omitted; the public cursor still advances past them.

Create-session and start-turn requests carry client-generated request IDs. On a
timeout, the client retains the ID and payload instead of sending a new mutation
blindly. Indications sent during a run are also durable and bound to that turn.
Uncertain conflicts retain the pending request for inspection instead of silently
generating another ID. Cancellation names the exact active turn. Permission decisions name the
pending request within its session; a stale approval cannot authorize a new tool
call. Device revocation is checked on each request.

The host only admits configured project roots. Enabling write or command tools
does not mean every operation is automatically approved. The harness permission
policy remains the authority. A workspace list is not itself an operating-system
sandbox, and command isolation support must be checked on the host OS.

The mobile transcript keeps a bounded recent history and labels trimmed output.
The host journal remains on the PC. The legacy aggregate diff covers tracked
changes against HEAD. The V2 workspace exposes staged, unstaged and untracked
files separately, with individual diffs and explicit truncation notices.

## Version 2 workspace

- Adaptive project/session navigation includes server-side search, pagination,
  renaming, favorites and archive/restore. Incomplete listings are labeled.
- Multiple computer profiles live in an atomic Keychain vault. Queued mutations
  retain their original host, credential, request ID and payload. Switching PCs
  never redirects an uncertain mutation to another computer.
- One-time QR/deep-link pairing expires and supports retrying the same redemption.
  The client acknowledges it only after saving the connection to Keychain.
- File browsing and search lead to an explicit text editor. Saving supplies the
  base SHA-256 revision; a stale edit is rejected and the local draft is retained.
  Host traversal uses no-follow file descriptors under an allowed project root.
- Git status, per-file diff, stage/unstage and commit use typed routes, repository
  preconditions and durable request IDs. Commit does not run repository hooks or
  publish to a remote. Branch switching, worktrees and pull/push remain separate
  future capabilities.
- Messages render Markdown and fenced code; tool output remains read-only.
  Private attachments are scoped to the device and project. UTF-8 text is limited
  to 256 KiB per file, 1 MiB per turn, and eight attachments in total. Photos are
  offered only when the exact configured model and transport support images.
  PDF/Office extraction is not implemented.
- The model view reports the configured provider/model. V3 can also select
  verified models and their declared reasoning levels as described below.
- Optional APNs registration is per computer. For daemon-owned turns, the host
  sends minimal approval or completion notifications, and tapping targets the
  exact device/session. The desktop gateway does not yet emit notifications for
  desktop-owned turns. Push
  requires the host's private APNs configuration and a signed provisioning profile
  with Push enabled. Mock delivery and a successful simulator build do not verify
  delivery to a physical iPhone.

Native desktop sessions from allowlisted projects are also projected through the
gateway without repairing or copying their journals. New normal desktop turns
can expose steering, cancellation and permission decisions to the mobile client
through a fresh owner heartbeat and an occupied OS lease. A live worker without
that bridge remains read-only. Desktop session metadata and starting/resuming
desktop turns are not mutable through this gateway; session capabilities express
those limits. The daemon never creates a second runner for a desktop session.
Daemon-owned mobile work survives closing the desktop window; desktop-owned work
still depends on the desktop process. A fully shared persistent runtime remains
an architectural follow-up.

## Version 3 mobile design and task settings

The six user-provided mobile screenshots were used as design references for the
task list, environment picker, floating composer, attachment menu, permissions
panel, quick intelligence selector and advanced settings. Shared grayscale
surfaces and monochrome SF Symbols support dark/light/system appearance. Git
diffs and errors retain semantic colors. Controls remain usable with larger text.

- Start from the bottom composer on the task list, choose a project, then write
  the first message. Computer switching, task filters, search and pagination
  retain their original host scope. An empty filtered page still offers loading
  the next page when one exists.
- The plus button offers camera/photos, UTF-8 text/code attachments, a reference
  to a PC file and planning mode. Images are normalized and retain the existing
  upload limits. Camera availability is checked on the actual device.
- The microphone transcribes into the draft using on-device speech recognition.
  It requests iOS microphone/speech permission, never sends audio to the PC,
  never automatically submits the text, and stops when leaving the screen or
  backgrounding. An unsupported locale/device falls back to typing or keyboard
  dictation. Physical camera/audio capture still requires an iPhone test.
- The hand button opens the permissions panel and any pending action details.
  The new layout preserves approval of the exact reviewed action; no approval
  bypass or automatic full-access policy is introduced.
- Quick intelligence settings lead to **Avanzado**. Model and reasoning choices
  come from the PC's verified catalog for the same configured Codex account and
  endpoint. The host validates each exact model/effort pair before admission.
  Loading the catalog renews OAuth through the shared credential loader and
  refreshes official model metadata when needed, without opening the desktop.
  A separate private cache is valid for 24 hours and bound to the credential;
  refresh failures have a 60-second cooldown and safely fall back to the
  explicitly configured model. Other provider routes keep that fallback.
  Normal token rotation is automatic; switching to a different account requires
  restarting the host so all model backends use the same account identity.
- `model_id` and `reasoning_effort` are optional start-turn fields. Pending sends
  and manually queued messages persist their options; a retry cannot silently
  replace them. Changing models reconciles provider-specific history items.
  Steering an already active turn does not change its model or reasoning.

Cloud environments, autonomous goal continuation and configurable priority speed
are not implemented. The UI
does not present them as working controls. Model selection performs no inference
until the user sends a task.

V3 verification on 2026-10-07: Debug and Release iOS builds succeeded, all 30
XCTest tests passed, and 14 fixture screenshots plus the normal connection
screen were inspected, including light appearance and accessibility text size.
The remote host passed 47 tests and strict Clippy. An authenticated metadata-only
request renewed OAuth and returned seven selectable models with their exact
reasoning levels; a subsequent request used the private cache. No provider
inference or physical camera/dictation test was performed during these checks.

## Version 4 composer controls and code review

Permissions and quick intelligence controls now open in anchored panels inside
the conversation view, keeping the draft and software keyboard active. The
panels scroll within the available space for larger text and dismiss on outside
tap, accessibility escape, host change or backgrounding. The model title opens
a compact advanced sheet with grouped model/reasoning rows and normal speed.

The optional `approval_policy` turn field supports `ask`, `auto` and
`full_access`, advertised by `turn.approval_policy`. Ask remains the default.
Auto permits bounded project text edits, with commands, deletions and moves
still asking for approval. Full access skips per-action approval only for tools
enabled by the host; it does not enable extra tools, change command isolation,
elevate privileges or remove filesystem restrictions. Plan remains read-only.
Pending sends and manually queued prompts retain their exact chosen policy.
No control changes an already running turn or the host's global configuration.

The review design follows the mobile loop documented in
[OpenAI's remote engineering guide](https://developers.openai.com/blog/mastering-codex-remote-for-engineering)
and [mobile review overview](https://learn.chatgpt.com/docs/mobile), alongside the
user's composer screenshots: compact file summaries, readable diffs and feedback
returned to the conversation. These references establish interaction patterns,
not feature parity with the external product.

Git uses a flat file list with separate working-tree and staged filters. A
dedicated commit sheet shows the staged snapshot and retains its exact
`expected_revision`; intervening Git changes cannot silently alter that commit.
Resolving a pending operation closes the stale summary and reloads Git state.
The unified diff viewer supports per-file/per-hunk collapse, wrapping, additions
and deletions with distinct old/new line numbers, file opening and inline review
comments. Comments are added to the current chat draft, never automatically
sent. Deleted-line comments explicitly name the previous version. Full-source
previews share syntax coloring and offer line numbers and wrapping. Oversized
individual lines retain their text without expensive syntax coloring.

The redesign adds no push, force-push, revert or branch-changing operation.
All Git writes continue using durable requests and expected-revision checks.

V4 verification on 2026-10-07: the intermediate native app compiled Debug and
Release and passed 37 XCTest tests. Git list/diffs, inline comments and full
source previews were visually checked in simulator fixtures. Composer capture
review exposed missing secondary text on custom material surfaces; the working
source now uses explicit adaptive grayscale colors. EAS's free iOS build quota
blocked recompiling that correction, so final visual validation and V4
TestFlight delivery remain pending. The intermediate signed IPA is deliberately
not submitted. V3 is already available in the private Owner TestFlight group.

## Set up the PC

Build the host and import the desktop's currently selected Codex model. This
reads the non-secret provider selection, creates a separate private state store,
and writes the new phone credential to a private file without printing it:

```sh
rtk cargo build -p openide-remote
rtk python3 scripts/setup-remote-host.py --project "$PWD" --port 18787 --allow-commands --install-service
```

The explicit port avoids a conflict with another local service. Choose an
available port; the host binds only `127.0.0.1`. Additional projects use repeated
`--project` arguments. The helper preserves an existing configuration instead of
silently replacing its project or permission settings. `--read-only` disables
write and command tools. With `--allow-commands`, commands still need approval
under the default Ask policy and under Auto. An explicitly selected Full access
turn skips that approval; the default `host_unrestricted` execution has the
user's OS rights.
After checking official capability metadata for the exact selected model, add
`--image-input` to enable photo attachments. Keep this flag on subsequent setup
runs for that configuration; it defaults off and never assumes model support.
Provider configuration is pinned to session provenance, including capability
declarations. After changing it, create a new remote session; prior history
remains readable.

The Linux user service is `openade-remote.service`; its state defaults to
`~/.local/share/openide/remote`. The pairing export is `iphone-pairing.json` in
that directory. Copy its token through a trusted channel into the app, then
remove the export. The service stores only the token's hash. Other platforms
can run the host CLI directly; see `crates/openide-remote/README.md`.
Running the helper again with `--install-service` updates its executable and
restarts the service. Finish active turns before doing an upgrade.

```sh
rtk systemctl --user status openade-remote.service
rtk systemctl --user stop openade-remote.service
```

The user service survives closing the desktop window. It does not keep a PC
awake, and its availability after logging out depends on the OS user service
configuration. A stopped host reports interrupted work on the next start.

## Private network

Install [Tailscale on the iPhone](https://tailscale.com/docs/install/ios) and use
the same account for the PC. The Linux helper runs a dedicated userspace daemon
with no changes to the machine's DNS or routing. Install the Tailscale package
first. On NixOS, the following keeps a local package reference without changing
the system configuration:

```sh
rtk nix-build '<nixpkgs>' -A tailscale --out-link "$HOME/.local/share/openide/remote/tailscale-package"
rtk python3 scripts/setup-remote-network.py install
rtk python3 scripts/setup-remote-network.py login
rtk python3 scripts/setup-remote-network.py serve
```

Login requires the account owner to open the authentication link. The `serve`
step may request enabling HTTPS for that tailnet. It derives the loopback target
from the host config and prints the `https://…ts.net` address to enter in the app.
[Tailscale Serve](https://tailscale.com/docs/reference/tailscale-cli/serve) makes
that endpoint available inside the private tailnet. Funnel is never enabled.
The phone still needs its separate openADE device token.

```sh
rtk python3 scripts/setup-remote-network.py status
rtk python3 scripts/setup-remote-network.py stop
```

The network service is `openade-remote-network.service`; stopping it preserves
its account state. Neither service is bundled into the EAS upload.

## Native builds with EAS

EAS is the build and signing service. The app's UI and runtime remain Swift.
The checked-in generator creates an Xcode project from the Swift source without
requiring XcodeGen, CocoaPods, or React Native.

From the repository root:

```sh
rtk node apps/ios/scripts/eas.mjs inspect simulator
rtk node apps/ios/scripts/eas.mjs build simulator --non-interactive
```

The wrapper scopes the upload to `apps/ios`, inspects the archive against an
allowlist, and excludes the PC workspace, provider credentials, local state, and
other monorepo content. Build artifacts and test results establish what has
actually compiled; generating the project alone is not a successful iOS build.

A simulator build uses local ad-hoc signing for working Keychain entitlements;
it does not require an Apple account and cannot be installed on a
physical iPhone. The `device` profile is for internally distributed, signed device
builds. EAS manages the signing material, but an Apple session, appropriate
membership, and device provisioning can still require the account owner's login
or second factor. Never put an Apple password, provider key, or device token in
the repository or chat.

```sh
rtk node apps/ios/scripts/eas.mjs credentials device
rtk node apps/ios/scripts/eas.mjs build device
```

These commands do not publish the application to the App Store.

## Checks

```sh
rtk cargo test -p openide-connections --lib
rtk cargo test -p openide-remote
rtk cargo test -p agents-window provider_connections::api_provider_tests
rtk python3 -m unittest discover -s scripts -p test_setup_remote_host.py
```

The EAS simulator workflow compiles Swift, runs XCTest in a real app host,
including Keychain save/read/update/delete, and captures the actual launch UI.
The Rust tests use a local HTTP model fixture and execute approved writes in
temporary directories. A physical-iPhone/network test is a separate acceptance
step; a successful simulator build does not establish it.

Verified on 2026-10-06: the final V2 simulator build
[`eab91371-0e9a-48e2-9142-1df106fc290f`](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/eab91371-0e9a-48e2-9142-1df106fc290f)
passed 28 XCTest tests and both Debug and Release compilation. Chat, projects,
files and Git layouts were inspected using DEBUG-only local fixtures; these are
not evidence of a live phone-to-PC session. The host passed 39 tests and strict
Clippy. The deployed V2 service was checked over authenticated loopback HTTP for
files, Git, models and both desktop/remote session origins; unauthenticated
access returned 401. The setup helper passed six tests.

On 2026-10-07, Apple confirmed V1 as valid and available to the private `Owner`
group, whose single tester was marked installed. Push was then enabled and a
fresh Store profile generated using the existing certificate, APNs key and App
Store Connect API key. The V2 Store build
[`49a828eb-85dc-4743-ad46-8a97deb7c6cc`](https://expo.dev/accounts/niihuel/projects/openade-remote/builds/49a828eb-85dc-4743-ad46-8a97deb7c6cc)
finished as 0.2.0 (2). Its IPA has production Push entitlements, the AppIcon and
privacy manifest, and excludes XCTest and DEBUG fixtures. It also corrects the
message for an interrupted remote session so it is not labeled a desktop session.

The single V2 submission
[`e578d791-d3d5-4b8f-b695-b073e2521e40`](https://expo.dev/accounts/niihuel/projects/openade-remote/submissions/e578d791-d3d5-4b8f-b695-b073e2521e40)
finished at 14:43 UTC. At 18:34 UTC Apple confirmed V2 as valid and available
to the private Owner group in TestFlight. The owner has authorized this PC in
Tailscale and enabled private HTTPS Serve. The dedicated userspace daemon now
sets `--statedir` explicitly so it can store its TLS certificate. A connection
through that daemon verified the HTTPS hostname and certificate chain and
received 401 from `/v1/health` without a bearer token. This verifies the private
ingress locally, not the iPhone connection. The last real Codex request
returned a subscription usage-limit error; no successful physical-iPhone/provider
journey has been established. See `apps/ios/BUILD.md` for distribution evidence.

## Acceptance criteria

1. Connect with a provisioned device; reject missing and invalid credentials.
2. Start a task in an allowed project and observe its real events.
3. Disconnect or background the client; reconnect without losing or duplicating
   messages or resubmitting a tool effect.
4. Approve or deny a pending operation; reject stale decisions.
5. Cancel the named turn; a delayed cancellation must not stop a newer turn.
6. Inspect command output and diffs without an input path to a shell.
7. Revoke a device and reject its subsequent requests without restarting.
8. Restart the host and distinguish an interrupted task from a completed one.

Further product work includes a fully shared persistent runtime, model selection,
structured agent questions, MCP/notes, worktrees and remote Git actions, widgets
and an integrated relay. APNs, pairing and distribution must be validated on the
actual device; UI and build profiles alone do not establish those journeys.

## Scope established from OpenChamber

The reference in `../refs/openchamber` was inspected without changing or copying
its source. Its mobile app uses Capacitor and a shared React interface connected
to the same OpenChamber/OpenCode backend used by its other surfaces. This app
continues to use SwiftUI and the existing Rust harness. EAS handles builds and
signing; it has no role in session ownership or runtime synchronization.

The main remaining foundation is **a persistent runtime shared by desktop and
mobile**. V2 directs limited controls to the current desktop owner and reads its
public history; it does not transfer execution into the service. Pointing two
processes at the same storage directory cannot replace ownership, permission
waiters or the session lease. The next architecture step is to move runtime
ownership into a service used by both clients, preserving the public event
projection and keeping private journal records off the mobile API.

The broader product scope, partly implemented in V2, remains:

- Session search, names, archive/restore, pagination, child-session visibility,
  unread state, message queues and explicit recovery of interrupted work.
- A provider/model/agent catalog with real capabilities and per-session choices;
  keys remain on the PC and provider changes retain history provenance.
- Structured agent questions, photos and file attachments, project-file context,
  and richer rendering of messages and tool activity.
- Workspace browsing, search and file viewing; edits use a base version/hash and
  reject a stale save rather than overwriting concurrent desktop or agent work.
- Git status and per-file diffs, including untracked files; stage/unstage, commit,
  branches, worktrees and remote actions with explicit mutation contracts.
- Native notifications and links to the relevant session, one-time QR pairing,
  multiple saved computers and device access management.
- Applicable desktop capabilities such as MCP, subagents, notes, usage and
  configuration, with mobile navigation adapted to iPhone and iPad.

The reference's interactive terminal is deliberately excluded by the user's
instruction. Receiving read-only command output remains part of the app.
Not every desktop setting imported by OpenChamber is exposed on its mobile
surface; implementation must follow actual mobile flows, not README claims alone.

Useful source anchors in the reference are `packages/mobile/README.md`,
`packages/ui/src/apps/MobileApp.tsx`, `MobileSessionsSheet.tsx`,
`MobileFilesSurface.tsx`, `MobileChangesSurface.tsx`, `mobileConnections.ts`,
`packages/ui/src/lib/opencode/client.ts`, and the server's `lib/fs`, `lib/git`,
`lib/client-auth`, `lib/event-stream` and `lib/notifications` modules.

Acceptance for the expanded product includes starting work on desktop and
continuing that exact session on iPhone, attaching an image, selecting a model,
answering an agent question, reviewing new and modified files, preparing a
commit, receiving a background approval/completion alert, and returning to
desktop with consistent state. The current implementation does not yet pass that
complete journey. Validation must also cover stale edits, lost mutation responses, two
clients acting together, device revocation and a host restart.
