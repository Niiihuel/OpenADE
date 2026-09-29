# Persistent command sessions

`openide-harness::PersistentCommandManager` provides a bounded process session
that can be started, polled, written to, and terminated across tool calls. A
host may attach one manager to `LocalTools::with_persistent_commands`. This
advertises `start_command`, `poll_command`, `write_stdin`, and
`terminate_command` only when `run_command` is enabled and the manager matches
the selected isolation. `HostUnrestricted` is supported across hosts;
`WorkspaceWriteOffline` is supported on Windows through MXC. The adapter
rejects a ToolContext from another workspace.

For `HostUnrestricted`, the manager defaults to the exact pre-PTY
`start_command` definition so resumed turns can retain their recorded tool
catalog. The offline catalog has a distinct description that identifies its
MXC isolation and lack of PTY support. A host can call
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
command IDs. Child workers and native automations do not attach a manager yet.

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
that deliberately creates a new session can escape that process group.

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
MXC sessions cannot be resized. The Activity pane does not expose a manual
resize control because it has no terminal viewport whose dimensions could be
sent to ConPTY; the native agent can call the tool when it needs a new size.
PTY requests on non-Windows hosts are rejected before spawning the command.

The `HostUnrestricted` mode uses the same explicit bootstrap environment as
`run_command`, without inheriting arbitrary host variables. On Windows,
`WorkspaceWriteOffline` uses the same MXC ProcessContainer policy as
`run_command`: the canonical workspace is writable, writes elsewhere and
network access are denied, and the sandbox owns process tree teardown. It uses
ordinary pipes for persistent stdin, stdout, and stderr. MXC policy warnings
or failed teardown are surfaced as errors; the manager never falls back to
unrestricted execution. PTY is unavailable under MXC, even when the manager
is configured to advertise PTY for host commands. On non-Windows hosts,
persistent offline starts are rejected before spawn. Sessions persist across
calls while their manager lives, not across app restarts. There is no implicit
shell emulation.

A cancelled or failed stdin write may be partial; it stops the process and
returns `EffectUncertain`, so callers must not replay it automatically.

The desktop's Activity pane shows the registered commands belonging to the
selected native task: opaque ID, pipe or PTY mode, running or exit state, and
whether output is pending. It refreshes through `list_sessions()` on a
background worker and never consumes `poll_command` output. A running command
has a **Terminar** action with a confirmation dialog. Termination runs outside
the UI thread; buffered output remains available to the agent until its final
poll. Selecting another task changes the listing, and downgrading command
access clears the listing while stopping the managed process trees.
