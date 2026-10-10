# Durable native command output

This contract belongs to `openide-core`, `openide-harness` and the native
desktop. It records output observed from an authorized persistent command;
it does not authorize a command, restore a process, reconstruct a missing
`ToolFinished`, or resolve an uncertain tool outcome.

## Independent producer and readers

`NativeCommandOutputStore` is attached to an owned
`PersistentCommandManager` before its first command. The desktop attaches
the selected native session's store, not a store chosen by tool arguments.
The exact binding contains the native session, canonical workspace, manager
generation UUID and command UUID. A replacement manager has a new generation.

Before spawning, the manager allocates the command ID and synchronizes a
descriptor with literal argv, isolation and pipe/PTY mode. Registration reserves
capacity for its terminal observation. Repeated registration is a lookup, not
permission to spawn again. A descriptor alone proves neither launch nor a live
process: cancellation, launch failure or a cut can leave it without a terminal.

The two pipe readers serialize their observations through one capture object.
For every observed chunk, the producer publishes and synchronizes its retained
bytes or exact omission before notifying a live observer or adding that output
to the agent's pending buffer. Polling remains a separate consumer. Its final
drain may remove the live ID, but does not remove the durable capture.

The cursor counts published observations, including omitted bytes. Each raw record has
its stream and an absolute half-open range `[start, end)`. These are capture
order, not a reconstruction of simultaneous writes or a timestamp order across
stdout/stderr. Terminal mode is a distinct merged stream. An omission counts
only bytes actually read. A chunk without a confirmed publication and bytes
never read after a cut are not invented in the recovered cursor.

Append uses an expected cursor and retains the last append's exact digest,
stream and cursor. A retry of that same last operation returns its existing
receipt, including when its bytes were omitted. A conflicting or older retry
does not append a duplicate. The terminal advances the revision, not the byte
cursor, and is returned even when reading at the end of observed output.

## Bounded storage and publication

Retention is a stable prefix per command, with an explicit omitted tail.
The limits are independent:

- At most 64 descriptors per native session.
- At most 8 MiB of retained raw bytes per session.
- At most 4096 retained data records; each record contains at most 8 KiB.
- At most 16 MiB for the serialized sidecar, including reservations.
- Literal argv follows the existing command bound: 32 KiB and at most
  256 arguments in addition to the program.
- Each read is bounded to 256 KiB raw bytes, 256 records and 1 MiB of
  encoded page data. The desktop requests at most 16 KiB and 32 records.

Exhausting byte, record or serialized capacity records the exact omitted
range. Those limits do not turn an incomplete capture into complete output.
A bounded read does not consume records or change retention. The desktop
keeps one page in memory and can visit prior/next pages; it does not retain an
unbounded concatenation while navigating.

The sidecar is versioned and checksummed, parsed under a file-size bound and
published using a private temporary file, file synchronization and atomic
replacement. Unix also synchronizes the containing directory. The checksum
detects inconsistent stored content; it is not an authentication mechanism
against a user who can edit the storage directory. Invalid, foreign or orphaned
content is not silently treated as an empty history.

Every producer transaction checks fresh session metadata and deletion state
under the same journal transaction used by session operations, then takes the
output lock. It does not acquire or retain a turn lease. A new registration
rejects an archived session; an already registered capture can publish its
remaining output and close after archive. Deletion removes owned output and
publication artifacts before metadata while retaining the transaction locks.
A late producer cannot recreate a deleted session's capture. Lock files may
remain as synchronization artifacts; they contain no recovered output.

## Failures and recovery

A failed durable append cancels the contained process immediately. Bytes without
an acknowledgement are not exposed as saved output. Earlier acknowledged ranges
remain readable. Terminal records distinguish observed exit, termination,
timeout and wait failure, with capture diagnostics. Persisted reader/wait
diagnostics are bounded to 4 KiB each, retaining valid UTF-8 and an explicit
truncation marker when needed; this does not alter the process outcome.
If publishing the terminal fails, reopening still has no confirmed terminal;
the UI must not invent one.

Missing terminal means **incomplete capture**. It does not mean “running”,
“succeeded”, “killed” or “safe to repeat”. Persistent processes and their stdin
are not restored after application exit. The existing Runner/Resume journal
remains authoritative for tool recovery: saved stdout cannot manufacture the
missing model tool result or bypass human verification.

## Desktop

Activity exposes **Historial de salida**, including after reopening with no
manager. A live command can open its exact binding. The inspector shows capture
status, byte/range totals and revision; command provenance and full argv are in
its scrollable details. The default page shows readable monospace output with
stream and byte-range labels. It uses UTF-8 only for a valid complete segment;
binary data and split UTF-8 segments are labelled base64. Newlines are preserved
and other control characters are escaped as data, without terminal actions.
**Ver datos exactos** opens the secondary JSON representation. Omitted ranges
remain visible. The 560 × 470 px modal adapts to a 336 px width in a 360 px
viewport; paging and close controls stay outside its scrollable content.

Background results are tied to the captured store, native session, canonical
workspace, binding, request ID, requested cursor and observed revision. Changing
selection, closing the inspector or superseding a request discards its answer.
The host schedules repaint while an inspector response is pending, even without
a live manager or active turn. The live pending-output snapshot remains a
separate inspection with its existing consumption semantics; it is not merged
into this history.

## Acceptance and limits

Acceptance executed on Linux on 2026-09-30:

- Twelve core tests in `native_command_output_tests.rs` verify binary pages,
  terminal-at-end reads, exact retries, two-store serialization, archive/delete
  races, quotas, semantic corruption with a recomputed checksum, exact stream
  domains, private files, owned temporary cleanup and foreign/orphan evidence.
  `durable_command_output_real_process_cuts_preserve_register_append_omission_and_terminal`
  executes twelve SIGKILL cuts: register, append, omission and terminal, each
  after temporary-file sync, rename and directory sync. Reopening validates the
  previous or next complete artifact and retries without duplication. Quota
  tests seed valid snapshots near capacity and exercise actual public
  append/read/reopen boundaries; they do not claim a process streamed 8 MiB.
- Seven real-process producer tests in `tests/persistent_native_output.rs`
  verify binary stdout/stderr, separate turn runtimes, final poll/removal,
  registration failure before payload, ACK-before-observation, exact omission
  at the record quota and capture failure stopping the process. A publication
  failure preserves the acknowledged artifact byte-for-byte and reopens it
  without repair or retry. Two corruption tests explicitly restore their saved
  valid artifact to inspect its last confirmed state; that is fixture repair,
  not automatic product recovery. A terminal-publication failure keeps the
  observed live exit and diagnostic but no confirmed durable terminal.
- Two private producer tests check bounded Unicode diagnostics and reject a
  repeated registration as permission for another process launch. The opt-in
  `actual_linux_offline_output_capture_survives_turn_runtimes_and_reopens_without_process`
  was executed with Bubblewrap: binary/Unicode output, EOF across runtimes and
  reopening without a manager. This is separate from default workspace tests.
- `durable_output_survives_final_poll_publication_cuts_without_reexecution_or_synthesized_tool_result`
  runs an actual Runner with local HTTP/SSE and approved shell tools. It kills
  the process before and after the final poll's `ToolFinished` publication,
  after the live ID has already been removed. Three-byte pages after reopening
  preserve exact binary/Unicode streams and terminal. The earlier cut stays
  `ToolOutcomeUncertain` without requests or a synthesized result; the later
  cut resumes through the actual next HTTP request. The shell effect occurs
  once in both cases. A replacement manager cannot poll the old ID.
- Five `native_command_history_app_tests` execute real processes and App
  actions, final polling and store reopening without a manager. They verify
  one-page navigation and readable/exact representations, full provenance and
  wait diagnostics through scroll at 360/900 px, incomplete descriptors, and
  rejection of stale scope/store/binding/cursor/request/revision results.
  `durable_output_reopened_history_requests_repaint_and_applies_response_through_real_egui_frame`
  checks actual `App::ui` frames: a quiet reopened host has no manager/turn,
  the pending-response barrier schedules repaint within 150 ms, and a real
  stored page is applied through the next frame without manual polling.
  `durable_output_history_reads_capture_without_repairing_torn_model_journal`
  verifies that opening the actual capture worker preserves a truncated JSONL
  journal byte-for-byte instead of repairing it as a side effect of inspection.
  Native Frame/SVG/resvg exports at those widths were also explicitly generated
  and inspected; this is not a fresh live GUI certification on another host.

The Windows reader/containment adapters use the same publication API, but were
not executed on Windows in this acceptance. Native child workers now attach
per-owner captures through the shared host registry; see
[persistent-commands.md](persistent-commands.md) for their separate acceptance.
Native automation workers remain read-only and do not attach this capture.
No model-provider account, Windows
ConPTY/MXC run, ARM64 host, power loss, within-syscall cut, arbitrary hostile
ancestor/hardlink replacement, latency/RSS bound or unobserved output is
certified by these Linux tests. The global gate and remaining roadmap criteria
are recorded in [native-harness-roadmap.md](native-harness-roadmap.md).

The read-only Codex reference at `33a0f766` was consulted in
`core/src/unified_exec/process.rs` and `async_watcher.rs`: its pending and
completion buffers and streaming consumer clarify why polling and observing
need separate ownership. The native API, storage protocol, quotas and desktop
projection here are OpenIDE's own implementation, without importing its process
or libraries.
