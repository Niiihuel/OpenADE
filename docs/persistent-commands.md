# Persistent command sessions

`openide-harness::PersistentCommandManager` provides a bounded process session
that can be started, polled, written to, and terminated across tool calls. A
host may attach one manager to `LocalTools::with_persistent_commands`. This
advertises `start_command`, `poll_command`, `write_stdin`, and
`terminate_command` only when `run_command` is enabled and the manager matches
the selected isolation. `HostUnrestricted` is supported across hosts;
`WorkspaceWriteOffline` uses MXC on Windows and the
[Linux adapter](linux-command-sandbox.md) when its required helper is available. The adapter
rejects a ToolContext from another workspace.

For `HostUnrestricted`, the manager defaults to the exact pre-PTY
`start_command` definition so resumed turns can retain their recorded tool
catalog. The offline catalog has a distinct description that identifies its
platform isolation and lack of PTY support. A host can call
`PersistentCommandManager::with_pty_tools()` before wrapping it in `Arc` to
advertise `pty`, `columns`, and `rows` on `start_command`. PTY remains opt-in
per call: `pty: true` starts a terminal, with optional initial dimensions
(default 80 by 24, bounded to 20–500 columns and 5–200 rows). Passing
dimensions without `pty: true` is rejected. The legacy schema is available as
`PersistentCommandManager::legacy_start_command_definition()` for exact
catalog selection on resume; the host must reject PTY arguments on a legacy
resumed turn. This schema change never silently upgrades a committed catalog.
On Windows, `with_pty_resize_tools()` separately opts into a fifth tool,
`resize_command`, for new `HostUnrestricted` catalogs. It requires a live PTY
session ID and dimensions between 20–500 columns and 5–200 rows. Keeping this
builder separate preserves the exact prior four-tool PTY catalog. On resume,
the desktop matches the saved catalog digest and rejects `resize_command` in
turns that were committed before that tool was advertised.

The desktop's agent tool policy marks start, stdin writes, termination, and
resize as `Ask`, and polling as `Allow`. The runner still has to receive that
policy; `LocalTools` alone does not request approval. None of these tools is
certified for Plan mode. The desktop keeps one manager per native parent
session while the app process lives, including turns resumed in that process.
Changing isolation or disabling `run_command` revokes a manager, including one
still held by an active turn. Archiving or
deleting a session revokes its manager; restoring host execution creates fresh
command IDs. Child workers now share the host registry through a frozen native
command profile, with a separate manager and capture for each child owner.
Native automations still expose only local reads and do not attach a manager.

## Host registry and child workers

`NativeCommandRegistry` owns managers by native owner, canonical workspace and
exact command profile. The desktop retains this registry and uses its lookup
for parent and child Activity, controls and worktree checkout guards. Each
manager attaches `NativeCommandOutputStore` before its first spawn. Missing
native storage fails the production path; it does not select an in-memory
fallback.

`NativeCommandProfile` is a process-free host capability with a weak registry
reference, an authorization epoch, isolation, limits and explicit PTY/resize
options. Catalog queries use the adapter's pure schema builder and do not
create a manager, open a runtime or register output. Its stable identity omits
the process generation/epoch so reopening can verify configuration without
claiming process restoration. The manager and profile use the same limit
validator and schema builder.

`SubagentWorker::with_persistent_commands` is host opt-in, enabled by the
desktop only when its command permission and isolation gate allow it. Build
catalogs include that exact profile. Plan does not announce or execute command
tools. Under the claimed child lease, the worker installs an overlay whose
base executor already advertises `run_command`; reserved name collisions and
foreign store/scope bindings fail before effects. Command IDs resolve only in
that child's manager, including at nested depths. A parent or sibling ID is
not a child command. The registry retains managers across completed turns and
separate follow-up runtimes; Failed/Cancelled outcomes close them under the
current claim.

Reservation identity includes the configured command profile. Recovery
selects candidates with no profile when the old reservation had none; it does
not add tools to an old committed catalog. This identity also distinguishes
profiles in an initial Plan reservation: its later Build turn cannot silently
upgrade a legacy child. The Plan catalog remains read only. Normal execution,
parent Resume, child recovery and mailbox dispatch receive the same host registry.
After host restart the registry is empty: captures remain readable, while old
process IDs cannot be polled or restored.

The optional fresh manual-policy source gives the UI and a child Runner/Resume
the **same actor** for the same owner/generation. This shares notification and
active-operation state, allowing confirmed stdin/EOF during a model wait under
the worker's existing child lease. Plan uses `actor_existing` for an already
retained manager; it neither creates a manager nor adds model tools.
Runner and Resume can therefore acknowledge explicit human controls
without stranding a Requested operation under their Plan lease.
The mailbox accepts follow-up Build only. A Plan worker with an already
retained manager is tested using an explicitly host-created process fixture,
not an unsupported mailbox mode transition. The actor rereads command permission,
isolation, rules and hooks at each manual boundary. Model policy/hooks remain
the admitted snapshot. Revoke-all advances the authorization floor, closes
managers and invalidates worker profiles; those profiles cannot recreate a
process after a downgrade. Explicit new host configuration obtains a new
profile. Dropping the registry also closes trees even if a worker retains its
profile or a caller retains a manager.

Subtree cancellation returns its exact affected IDs under the same coordinator
lock after the durable commit. Bridge/model/UI paths close those managers
before notifying tokens, including Completed/idle children and descendants.
A failed cancellation does not revoke them. New descendants are rejected after
the parent's durable cancellation flag; exact historical spawn retries remain
lookups. Stale creation profiles remain blocked, while trusted cleanup with a
current lease or cancellation receipt closes even a newer manager for the
cancelled owner.

Acceptance on Linux (2026-09-30): nine default registry tests plus an explicitly
executed Bubblewrap test verify independent owners/runtimes, pure/exact
catalogs, scope/store/epoch rejection, actor identity/notification, publication,
drop containment and poisoned-lock revocation. Child worker acceptance and
the desktop's
`child_commands_share_host_controls_across_model_wait_followup_cancel_and_reopen`
exercise real processes and model turns. The latter uses HTTP/SSE, UI approval,
exact stdin and EOF while the child lease is busy, pointer controls at 360/900
px, a rebuilt host follow-up worker, final poll, cancellation without a live
bridge and captured output after reopen. The idle process used to verify UI
cancellation is started directly as a fixture, not attributed to another model
tool call. These are local transports, not provider-account acceptance or
fresh Windows ConPTY/MXC/macOS/ARM64 certification.

Native automation effects remain pending: current automations intentionally
use read-only tools and deny background approval requests. They need their own
durable origin/policy snapshot, scoped approval surface and Resume integration
before command profiles can be enabled. Interactive Settings must not silently
change a scheduled run's authority; see the roadmap for the audited limits.

Each session has a random opaque ID. Stdout and stderr are drained concurrently
as raw bytes. Polling returns stream-labelled chunks in their observed order.
If the bounded output buffer fills, it inserts an `Omitted { bytes }` marker at
the corresponding position; callers must show that marker and must not claim
the command output is complete. The process and its final output stay in the
manager until a final poll consumes them. The manager limits concurrent
sessions, command arguments, stdin writes, output memory, bytes per poll, and
process lifetime. Poll and stdin operations have bounded waits. The tool
adapter returns at most 8 KiB of raw output and 128 events per poll, in JSON
bounded to 128 KiB; valid UTF-8 is shown as text, and other bytes are encoded
as base64. A result with omitted bytes is marked as an error.
`list_sessions()` returns IDs, PTY mode, exit status, and whether output is
pending without draining output or consuming the final poll.

On Windows, a process is created suspended and assigned to a terminating Job
Object before it can run; closing the job terminates descendants. The manager
closes the job on normal parent exit, explicit termination, timeout,
`stop_all`, or manager drop. `stop_all` permanently revokes new starts while
keeping existing IDs pollable for their final output. On Unix, it uses a process
group. As with `run_command`, a Unix child
in HostUnrestricted that deliberately creates a new session can escape that
process group. Linux offline commands instead use PID namespace supervision.

Windows PTY sessions use ConPTY. `CreateProcessW` receives both the
`PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` and `PROC_THREAD_ATTRIBUTE_JOB_LIST`
attributes. The process enters the terminating Job Object atomically at
creation; if ConPTY or that job attribute is unavailable, start fails before
the command runs. The ConPTY input and output pipe ends stay owned for the
console's lifetime. Poll returns one `terminal` stream containing merged
stdout, stderr, terminal control sequences, and possible input echo. The same
bounded buffer, lifetime timeout, `stop_all`, manager drop, and
`EffectUncertain` stdin semantics apply. `resize_command` calls Win32
`ResizePseudoConsole` with the console handle serialized against cleanup. It
does not restart the process. A failed, cancelled, or timed out resize stops
the contained process tree and returns `EffectUncertain`, since the console
size may have changed. The resize wait is bounded to five seconds. Pipes and
MXC sessions cannot be resized. Activity offers an explicitly confirmed numeric
resize only when the current control snapshot reports a live resize-capable
ConPTY. This does not synchronize a terminal viewport automatically.
PTY requests on non-Windows hosts are rejected before spawning the command.

The `HostUnrestricted` mode uses the same explicit bootstrap environment as
`run_command`, without inheriting arbitrary host variables. On Windows,
`WorkspaceWriteOffline` uses the same MXC ProcessContainer policy as
`run_command`: the canonical workspace is writable, writes elsewhere and
network access are denied, and the sandbox owns process tree teardown. It uses
ordinary pipes for persistent stdin, stdout, and stderr. MXC policy warnings
or failed teardown are surfaced as errors; the manager never falls back to
unrestricted execution. PTY is unavailable under MXC, even when the manager
is configured to advertise PTY for host commands. Linux offline commands use
the same gated Bubblewrap launch, filesystem view, seccomp and pidfd guard as
`run_command`, with ordinary pipes and no unrestricted fallback. Unsupported
platforms or a missing compatible helper reject offline starts. Sessions persist across
calls while their manager lives, not across app restarts. The desktop's
[independent durable capture](native-command-output.md) preserves acknowledged
output after polling and reopening; it does not restore the process. There is
no implicit shell emulation.

## Terminal tabs for started commands

On Linux and macOS the desktop's command profile sets `host_terminal`
(`native_command_sessions.rs`), for parent sessions and child workers alike. A
`HostUnrestricted` command then runs on a pseudo-terminal instead of three
pipes. What that changes for the process: it has a controlling terminal and is
its session leader (its process group is still the one the tree guard kills);
stdin, stdout and stderr are that terminal, so `isatty` is true, programs
colour and line-buffer their output and may prompt; it sees
`TERM=xterm-256color` and an initial size of 120 by 30; stdout and stderr
arrive merged as one `terminal` stream, with the terminal's echo of whatever
is typed; `close_after` is end of input. The cleared bootstrap environment,
the working directory, the approval of `start_command`, the exec rules, the
hooks and the tool schema are the same as before. The contained profile
(`WorkspaceWriteOffline`) keeps pipes. Windows is unchanged.

**Not byte-exact any more.** A terminal's line discipline sits between the
writer and the program, in canonical mode unless the program changes it: input
reaches the program line by line, a carriage return becomes a line feed,
control characters act (Ctrl+C interrupts, Ctrl+D ends input, Ctrl+Z
suspends), what is typed is echoed into the output, and the kernel cuts a line
longer than 4095 bytes. This holds for the agent's `write_stdin` and for the
confirmed input of **Controlar comando** as much as for the tab, so their
"exact bytes" contract now describes pipe-backed commands only: the contained
profile, Windows, and `run_command`, which never uses a terminal. Output is
altered too (the terminal's `\r\n`, then the plain-text filter below), so a
started command is not a way to move binary data on Linux and macOS. The
desktop tests of that contract (`native_command_output_app_tests`,
`native_command_history_app_tests`, `native_command_control_app_tests`,
`native_child_commands_app_tests`) keep their commands on pipes with the
test-only `NativeCommandSessions::keep_pipes`.

**Older sessions.** `host_terminal` is a transport. It is in neither the tool
schema nor `NativeCommandProfile::identity_digest` (current or frozen v1), so
nothing a session recorded mentions it: a turn interrupted before the desktop
turned it on selects the same catalog on resume, and a child reserved earlier
matches its reservation, including under `before_command_revision()`, which
carries the option along. `same_as` does compare it, but only between a
manager and a profile of one running app, which all come from one builder.
A process never outlives the app, so no pipe-backed command meets a
terminal-backed profile.

**Plain text for everything but the tab.** A terminal emits escape sequences
and `\r\n` line ends. `OutputBuffer::push` is the one place where they are
removed (`PlainText`, over the condenser's `AnsiStripper`; a sequence split
across reads is still removed): the poll buffer, the pending-output copy and
the durable capture, and with them `poll_command`, "Ver salida pendiente",
"Historial de salida" and the transcript's cards, read text without CSI/OSC
sequences and with `\n` line ends. The tab's copy is taken before the filter
and keeps every byte. The capture of such a command is registered as a
terminal stream; registering it as pipes made the store refuse its first
append and stop the command.

**The tab.** `NativeCommandRegistry::with_start_observer` hands the host each
started command, with its argv and a `PersistentTerminal`, before
`start_command` returns the ID, so the tab is attached before any poll can
retire the command. The desktop attaches a screen on that thread (its reader
drains the copy into a bounded grid even while the window is hidden) and, on
the next frame, opens a terminal tab owned by the conversation whose agent
started it, titled with the command and carrying the agent's mark ("Iniciada
por el agente · <tarea>"). Tabs belong to their conversation like the user's
own terminals: selecting another conversation lists that one's tabs, never
these; a child worker's commands show in the child's conversation.

What the user types in the tab (keys, paste, Ctrl+C) and the tab's size go
straight to the process through `PersistentTerminalInput`. This is the user's
own terminal: it does not pass the manual-input policy of "Operaciones", the
command hooks or the controls ledger, and leaves no receipt. The agent's
`write_stdin`, `terminate_command` and the confirmed controls of
**Controlar comando** are exactly as guarded as before. A pipe-backed command
(the contained profile, or any command on Windows) is a read-only mirror: its
tab says so and drops keys and paste.

Starting a command opens the Terminal section on its tab without moving the
keyboard focus. It does not do so while the focus is in a terminal or another
section of the IDE's bottom panel is open: the tab is then only listed. An
agent's tab takes the keyboard when the user clicks it or uses "Ver en
terminal" on the command's card or in Activity, never by itself.

Closing the tab of a running command asks first ("Detener <comando>") and then
stops its process tree with `PersistentCommandManager::stop`, directly: like
the keys, it is the user's act on their tab, not an "Operaciones" control. The
agent then finds the command terminated on its next poll. When the agent stops
the command or it exits, the tab stays with its status until the user closes
it; it is never restarted from the tab. Revoking the manager (command access
restricted, isolation changed, session archived) stops the process and the tab
says so; deleting the session removes its tabs with it.

Acceptance (Linux, 2026-10-12): `agent_terminals_app_tests` start `/bin/sh` in
a temp workspace through the app's registry and check the screen, typed input,
exit, the agent's escape-free final poll, the confirmed stop of a descendant,
the read-only mirror, the focus rule and revocation;
`host_terminal_keeps_every_recorded_identity_and_shows_started_commands_to_the_host`
checks the digests, the observer and the durable capture, and
`resume_of_a_turn_recorded_before_the_host_terminal_keeps_its_catalog` the
resumed catalog. The contained profile was not run here (no Bubblewrap helper
in the test shell): its mirror is exercised with a pipe-backed manager. No
macOS or Windows run; nothing here drives the real window.

## Live preview of a started server

A started command that turns out to be a server gets a live preview in the
conversation's browser tab (`apps/agents-window/src/preview.rs`).

**Detection.** The screen attached to the command reads the start of its
output: the first 8 KiB, for at most a minute, with escape sequences removed
(Vite colours the middle of its address). The first address matching
`http(s)://localhost|127.x.y.z|[::1]|0.0.0.0` with an optional port and path
counts, once something follows it in the output, so an address still being
printed is not cut short. Addresses of other machines, names that merely look
local (`localhost.example`), other schemes and addresses with credentials are
skipped. `0.0.0.0` becomes `localhost`.

Only a `HostUnrestricted` command is watched. A contained command has no
network of the host's, so whatever answers at an address it prints is another
program on this machine: it is neither recorded nor opened. The address comes
from the command's output, so a host command decides which loopback address
opens; it runs with the user's network already.

**Check.** A thread then connects to that port on loopback, up to 20 times
250 ms apart, and hangs up: no request is sent and no name is resolved
(`localhost` is tried as `127.0.0.1` and `::1`). Only an address that
answered is recorded on the command as its preview URL; a server that printed
an address and never listened gets none.

**Opening.** Settings › Navegador › "Abrir la vista previa de los servidores
que inicia el agente" is saved with the other preferences as
`preview_opening`:

- *Automáticamente* (default): the first time the port answers, the
  conversation's "Navegador" tab is opened, or reused, on that address, and a
  notice says "Vista previa abierta: <url>". The keyboard stays where it is.
  The tab comes to the front unless the user is typing in a field or editor
  other than the composer, which that tab might cover; then it waits among
  the tabs. A server of a conversation that is not on screen gets its tab
  there, for when the conversation is opened.
- *Preguntar*: nothing opens; the command's card and its Activity detail
  offer "Abrir vista previa" while the server runs.
- *Nunca*: nothing opens and nothing is offered.

Each command reports its address once, so it never opens twice by itself. An
address that is not loopback is never opened by any of this: the function that
opens the tab checks it again. The agent's `open_preview` tool
([browser tools](browser-tools.md)) uses the recorded address as its default.
A browser context only starts when its tab is shown, as before.

A preview whose server stopped reads "No se pudo conectar con localhost:5173
— ¿sigue corriendo el servidor?" with "Reintentar" in the browser tab.

Acceptance (Linux, 2026-10-12): `preview::tests` cover the address on typical
dev-server lines, what is skipped, the 8 KiB bound, a coloured address split
across reads, the connection against a listener of the test and a closed
port, and that the check sends nothing; `agent_terminals_app_tests` cover the
three settings with a shell that prints the address of such a listener, one
automatic opening per command, the reused tab, the keyboard and the recorded
default of `open_preview`. No browser starts in these tests and no real dev
server was run.

## Runtime lifetime across turns

The manager starts a private current-thread Tokio reactor lazily, before its
first process. A dedicated thread owns that runtime; command waits and output
readers do not depend on the temporary runtime of an individual Runner/Resume
turn. A process that still has its manager can therefore accept input and reach
EOF from a later independent turn/runtime. The runtime is not a process
restoration mechanism and does not survive application exit.

Manager drop closes process trees synchronously, cancels the reactor and leaves
its bounded reader/reaping grace on the dedicated thread. It does not wait on
that grace from the UI or drop a Tokio Runtime inside another async task. The
thread owns no manager Arc, so this does not keep the manager alive through a
cycle. Startup errors fail before a command is spawned; revoke prevents new
starts and remains visible on existing controls.

`tests/persistent_runtime_lifetime.rs` uses two separately constructed runtimes,
closes the first, then sends Unicode input/EOF and checks actual process output
and its terminal result from the second. Other cases start without any caller
runtime, revoke, drop the last manager inside async code, and force slow reader
cleanup with a deliberately escaped Host descendant while measuring that drop
does not block on the grace. The escaped fixture is explicitly killed by the
test; this does not claim stronger containment for HostUnrestricted. These
acceptances are Unix/Linux executions, not fresh Windows ConPTY/MXC acceptance.

A cancelled or failed stdin write may be partial; it stops the process and
returns `EffectUncertain`, so callers must not replay it automatically.

The desktop's Activity pane shows the registered commands belonging to the
selected native task: opaque ID, pipe or PTY mode, running or exit state, and
whether output is pending. It refreshes through `list_sessions()` on a
background worker and never consumes `poll_command` output. A running command
has a **Controlar comando** action with exact input, EOF, termination and
available PTY resize controls. Each effect uses the leased human command actor
and its durable confirmation/receipt; the former direct termination path is
removed. Buffered output remains available to the agent until its final poll. Selecting another task changes the listing, and downgrading command
access clears the listing while stopping the managed process trees.

## Read-only pending output inspection

Activity → **Ver salida pendiente** explicitly reads a copy of the selected
command's retained buffer through
`PersistentCommandManager::inspect_pending_output(id)`. This synchronous API
copies the complete bounded output state under its existing lock:
stream-labelled raw events, omission markers, exit status, reader error, and
retained/configured byte counts. It does not drain events, change
`final_delivered`, remove IDs, or free a command slot. A final result already
delivered to the agent is unavailable for inspection. The command's stdin
state and current PTY dimensions remain absent from this API.

The desktop performs copying and exact JSON formatting on a background worker.
It applies a result only while the captured request, selected native session,
canonical workspace, command ID, and manager instance still match. Closing the
modal, changing the scope, or revoking/replacing a manager discards pending
responses. **Actualizar copia** replaces the snapshot; it never appends copies
as incremental output. The modal shows full IDs, the observation date and exact
Unix milliseconds, process/exit diagnostics, reader errors, byte limits, and
the complete raw event JSON in a scrollable body. Valid UTF-8 is encoded as JSON
text; other bytes use base64. Terminal controls remain data. Omission counts
are visible and explicitly identify incomplete output. Copying the complete
bounded buffer avoids a second preview truncation; the maximum raw buffer is
1 MiB (the encoded JSON may be larger).

An agent poll racing with inspection can leave the next snapshot empty or
remove the ID after final delivery. This is expected and must be presented as
the current registration state. Pending output and committed transcript results
cannot be merged into a reliable incremental history by this API, which has no
durable byte cursor. Activity's separate **Historial de salida** reads the
producer's durable capture with absolute ranges and independent non-consuming
pages. The pending copy is not merged into it. Reopening still cannot restore a
process or its pending buffer; acknowledged capture ranges are independently
readable.

Acceptance evidence (Linux, 2026-09-29):

- Four `pending_output_inspection_*` harness tests use compiled Rust processes
  to verify exact stdout/stderr and binary bytes, bounded omission counts,
  partial poll tails, retained session capacity, terminal delivery exactly once,
  and concurrent observer-thread/agent-poll races. Reader-error propagation is
  verified with fault injection through `read_pipe`; these tests do not claim a
  deterministic operating-system pipe failure.
- Five `pending_output_inspection_*` desktop tests verify full UTF-8/base64 JSON,
  error/omission visibility and reachable document ends at 360/900 px, footer
  pointer actions, keyboard focus/Escape, and hidden foreign scopes. The real
  process host test opens the Activity action, receives an asynchronous copy,
  refreshes without duplication or changed journal/permission state, and proves
  the agent still receives the same final events. Additional host checks reject
  stale owner, workspace, request, and manager responses; reopening the desktop
  restores no process or preview.

These focused tests passed as four harness tests and five desktop tests. They
do not establish Windows ConPTY/MXC or live provider acceptance.

## Human controls and recovery (2026-09-30)

`control_snapshot(id)` observes the exact native owner, random manager generation,
literal original argv, canonical workspace, isolation, stdin state, exit,
revocation, PTY capability and last confirmed dimensions without draining output.
Open/Closed/Busy/Uncertain are distinct. A dropped in-flight input marks stdin
uncertain and stops the contained process. Dimensions are last confirmed ConPTY
calls, not a continuously queried size. The desktop binds each manager to its
native session before sharing it.

**Controlar comando** uses `NativeManualCommandActor` and a separate
`NativeCommandOperationStore`. The user prepares and confirms exact UTF-8 bytes
without an added newline; EOF and termination are separate actions. The input
editor opts into `TextArea::preserve_line_endings(true)`, retaining CRLF, CR,
Unicode and NUL through paste, copy, cut and confirmation. Those bytes reach
a pipe-backed command unchanged; a command on a host terminal receives them
through the terminal's line discipline (see *Terminal tabs*). Other textareas retain
their prior normalization. Permission/hook digests and the manager generation
are captured when preparing, checked on confirmation and checked by the lease
owner before external effects. Saved interactive deny rules remain authoritative.
A confirmation grants this operation once; it creates no session permission.

Intentions may be queued while a normal run or Resume owns the native lease.
Those owners service them through Notify during model streaming and between
completed tool groups. A tool, hook or permission wait already executing reaches
its next safe boundary before the manual action runs. An idle background actor
acquires the same lease. UI callbacks never write to the process directly.
Cancelling a still-Requested intention atomically withdraws it without needing a
second lease or stopping the model. A Start that won the race cannot be cancelled
this way; no Cancelled result is inferred for an in-flight effect.

Started and EffectUncertain receipts are never automatically retried. Activity
has **Historial de controles** even after reopening with no manager or commands.
The inspector preserves the full request, phases, timestamps, hook diagnostics,
known command result and aggregate outcome. A live actor is shown as in progress;
after it stops, an unfinished receipt requires manual inspection. Recognition
needs a note, an exact revision and an idle native lease; it records Resolved,
never an invented success, rollback or model tool result. Dismissing the modal
only hides it. The process and pending output are not restored after reopening.

See [native-command-operations.md](native-command-operations.md) for publication,
capacity reservations, cancellation races and acceptance evidence. Linux tests
exercise real pipes, hooks, HTTP streaming, Resume and reopen/cuts. Windows
ConPTY/MXC manual controls and real-provider interaction still need acceptance.
The controls ledger does not store process output. Its receipts and the
[separate output capture](native-command-output.md) retain different authority:
neither restores the process or manufactures a missing model result.

## Final poll and output recovery boundary

`poll_with_output_limit` drains the process-local buffer and can remove the
command ID before the tool adapter serializes its result and Runner/Resume
sync `ToolFinished`. The desktop now attaches `NativeCommandOutputStore` to
each owned manager before its first launch. Each producer chunk is synchronized
before it enters that buffer. Final drain and manager loss therefore leave
the acknowledged capture intact. The operation ledger continues to record
human controls and hook diagnostics, independently of stdout/stderr.

The [capture contract](native-command-output.md) binds session, canonical
workspace, manager generation and command ID; enforces independent byte,
record and serialized quotas; and shares session archive/delete transactions.
A missing terminal after reopening remains an incomplete capture, not an
invented exit or live process. Hosts that deliberately construct a legacy
manager without this builder still have only their process-local pending
buffer and do not gain durable capture retroactively.

`persistent_native_output_recovery::durable_output_survives_final_poll_publication_cuts_without_reexecution_or_synthesized_tool_result`
uses actual native HTTP/SSE requests, an approved `/bin/sh` process and two
SIGKILL cuts after its final poll removed the live ID, before/after publishing
that poll's `ToolFinished`. New stores read exact stdout/stderr, Unicode and
binary bytes through three-byte pages. The pre-commit case keeps
ToolOutcomeUncertain and appends nothing; the committed case resumes its next
HTTP request and completes. Neither repeats the command or changes capture.
This is Linux process-boundary evidence, not power loss, provider-account or
Windows acceptance. Producer failure, quotas, publication and desktop
acceptances are recorded with their limits in the capture contract.
