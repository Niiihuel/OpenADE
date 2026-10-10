# Project instructions (`AGENTS.md`)

`openide-harness::load_project_instructions(workspace, cwd, limits)` loads project documents without changing a run. The caller supplies the authorized workspace and active working directory, then appends `LoadedProjectInstructions::text()` to the native system instructions. `project_instructions::load_project_instructions_for_target(workspace, cwd, target_directory, limits)` adds documents through an existing target directory beneath `cwd`; callers handling a file pass its parent directory. The original API retains its root-to-cwd behavior. `LoadedProjectInstructions::directory` identifies the last directory scanned, while `cwd` keeps the caller's working directory. Each entry retains its canonical source path and SHA-256 digest for inspection. Before the first native model request, the runner commits the exact assembled host, skill and project instruction text as `SystemInstructionsCommitted`, then commits `ModelContextCommitted` over the complete system prompt. The snapshot is versioned, domain hashed, limited to 512 KiB of UTF-8 and synced in the session journal; it is excluded from transcript text and session search. The JSONL reader also rejects any event record over 64 MiB.

Discovery stays inside the canonical workspace. It starts at the nearest `.git` root between the selected directory and `workspace`, or at `workspace` when no marker exists, and visits directories through `cwd` or the target in order. A nested repository marker starts a new hierarchy for a target inside it. At each directory it selects `AGENTS.override.md` if present, otherwise `AGENTS.md`. A blank override still suppresses the regular file at that directory and counts toward the file limit. Missing files are skipped; unreadable, linked, nonregular, invalid UTF-8, oversized, or escaping files fail the load. Defaults are 32 KiB per file and 32 KiB total, with at most 64 directory levels and 64 selected files. The selected document is opened without following a final symlink or Windows reparse point. Linked or reparse directories in the path from the authorized workspace to `cwd` or target are rejected before canonicalization hides them.

`agents-window` loads these documents before new interactive native turns and native automations. The child worker inherits its parent's assembled instructions. The current host calls the compatibility API with the workspace as `cwd`. Load errors are reported before admission or model dispatch. When recovering an interrupted turn, parent and child read the saved snapshot from replay, so a changed, deleted or newly invalid `AGENTS.md` does not replace the instructions already sent to the model. `ResumeEngine` uses that snapshot for context verification, every resumed model request and the system message after automatic compaction. A crash after the snapshot but before the model context commit also reuses it. Journals from before this event remain readable; they still require the current assembled text to match the old model context digest, so a changed file blocks recovery conservatively. A new turn reads current project files. The module does not load account or thread instructions.

Leased native runner and resume flows also inspect the real target of local `read_file`, `write_file`, `list_files`, `search_files`, and `apply_patch` calls. If a target introduces deeper project rules, the first call is completed without executing the file operation. Its bounded, structured tool result saves the selected rules and asks the model to retry. A reserved `HookStarted`/`HookFinished` pair binds that snapshot to the exact tool call and its content hash; configured hooks cannot use the reserved ID prefix. Subsequent model requests promote only authenticated snapshots into directory-scoped system context reconstructed from the journal. Ordinary tool output that imitates the snapshot text is ignored. The same snapshot is used after a crash, even if the document changes, and after context compaction. Path resolution rejects parent traversal, prefix collisions, links, reparse points, and targets outside the workspace. A nested `.git` marker starts a replacement scope. `run_command` and persistent commands still run with the workspace as their fixed cwd; arbitrary shell commands are not inspected for directories they may enter. External tools have no common path contract and are not covered by this preflight.

Local file tools prefer workspace-relative paths, such as `src/main.rs`. They
also accept absolute paths beneath the canonical workspace or its selected
absolute spelling; a different outside alias pointing inward is rejected.
`list_files` and `search_files` treat an omitted path, `""`, or `"."` as the
workspace root. `read_file` and `write_file` require a nonempty file path, and a
write requires an existing parent directory. The file-tool resolver and project
instruction preflight share this normalization, so absolute paths activate the
same nested rules as relative paths. Both still reject `..`, links beneath the
workspace, and sibling paths with a matching text prefix. `apply_patch` headers
remain relative-only. Full host command access does not expand this file-tool
boundary. Regression coverage includes empty roots and filenames, absolute
reads/writes/searches, nested-rule activation, selected workspace aliases,
prefix collisions, traversal, and static symlink escapes.

This is a bounded, fail-closed preflight protocol rather than exact Codex behavior. A newly encountered directory can require an extra model request before a tool effect, and a crash between the synthetic `ToolStarted` and `ToolFinished` conservatively needs manual outcome verification. It does not implement Codex's dynamic world-state instruction event or configurable project root markers.

The snapshot may contain private instructions because it stores exact model-visible text, not just a hash. It is not encrypted independently of the session journal; filesystem access to that journal must be limited to the account running OpenIDE. `SessionStore` currently inherits the journal directory's filesystem permissions, so a custom `OPENIDE_DATA_ROOT` must point to a private directory. The snapshot event is never included in session search or transcript rendering.

## Initial validation before effects

The native runner validates the initial assembled instruction byte limit, plan,
and hook configuration before preparing external tools or processing approved
human command intentions. Leased runs also validate the native model route.
Desktop runs and other leased callers with a trusted steering store or manual
actor binding validate the exact current admission, its images and canonical
workspace, and the runner, actor, journal, and lease session identity. The legacy
API without either trusted store binding retains the host's responsibility for
the journal and admission; it still rejects explicit request/sink/lease session
mismatches without publishing a terminal. The additional instruction limit is 512 KiB of UTF-8
bytes, including for the public unleased runner API. The exact instruction
snapshot is still committed after tool preparation and before the first model
request; initial validation does not change that journal order.

The worker records three distinct phases: authority not yet verified, current
admission verified, and effects allowed. An invalid initial request with verified
authority rejects only pending `Requested` human command receipts. Its terminal
publication uses the session store's transactional receipt fence. A new request
that wins that fence is rejected on a retry without processing commands, hooks,
MCP, or model requests. A `Started` or uncertain receipt blocks publication and
remains unchanged for explicit recovery. A failed authority check never writes
to a foreign journal or retires its receipts. When no terminal can safely be
published, the worker exposes the initial failure and boundary diagnostic and
leaves the admission available for recovery.

Coverage:
`initial_invalid_instructions_or_plan_reject_queue_before_real_mcp_hooks_stdin_and_provider`,
`initial_exact_instruction_limit_keeps_valid_effects_and_stale_retry_cannot_consume_later_queue`,
`initial_unvalidated_foreign_journal_and_started_receipt_remain_unchanged_without_effects`,
`initial_rejection_publication_race_retires_requested_but_never_started`, and
`initial_request_sink_and_lease_owner_mismatch_has_no_publication_or_intention_effect`.
These exercise real local HTTP MCP discovery and provider streaming, a real
persistent process and hook, exact-limit and oversized instructions, invalid
plans, reopened stores, foreign authority, later admissions, and receipt races.
The deterministic publication race uses a test journal wrapper around the real
session store; it does not simulate an OS crash or a live account.

The runner receives assembled instruction text from its host; it does not infer
whether arbitrary prose is trustworthy. Project and skill loaders must reject
invalid or unauthorized sources before supplying that text. MCP identity, the
dynamic tool catalogue and provider request compatibility can only be checked
after discovery, so an invalid newly discovered catalogue may already have
prepared MCP. This boundary does not add directory preflight for arbitrary
shell commands or external tool arguments and does not establish real provider
account acceptance.

The desktop also exercises the loader-to-runner boundary through
`native_initial_instructions_app_tests`. Three tests cover four real setups:
invalid UTF-8, an oversized `AGENTS.md`, a linked `AGENTS.md` on Unix, and changed
content of an explicitly selected local skill. `AgentsWindow::start_run` preserves
the draft and selection, with reopened stores containing no new session or
admission and no MCP/provider request. Repairing the same file/setup completes
real local MCP initialization, two tool-list pages, the native SSE request with
the repaired instructions and MCP catalogue, `TurnCompleted`, and MCP DELETE.
The MCP fixture returns the optional GET 405 response: it does not accept
notifications, MCP identity for Resume, another operating system or a live
provider account. Restoring a selected skill's original bytes is not automatic
acceptance of changed skill content.

## Explicit desktop inspection

Settings → Memory can explicitly inspect the saved additional initial instruction copies
of the selected native conversation. The host reads the stored journal and
validates its replay and referenced checkpoints before exposing any private
text. The projection also checks every saved instruction event's session
identity, supported version, bounded content, and exact digest, including
copies before an authoritative checkpoint. A failed read or validation shows
an error without substituting instructions from current project documents.

The modal identifies the conversation, canonical project, captured journal
sequence, and each copy's admitted turn ID (or explicit legacy absence), turn
start sequence, event sequence, timestamp, version, digest, and byte length.
A selector renders one complete copy at a time in the modal's scrollable body.
Completed turns remain inspectable after reopening the stores: their copies
come from validated events, not `NativeReplay.active_system_instructions`,
which resets when replay is idle. Changing the selected conversation or
project hides the modal. Opening the inspector is allowed during a run and
captures the journal as read at that point; closing and opening it again reads
a new snapshot. Inspection does not edit the journal, read `AGENTS.md`, change
skills or permissions, dispatch a model/tool call, or resume recovery.

This view covers `SystemInstructionsCommitted`, the assembled additional host,
project, skill, and goal instructions saved at the start of each turn. The
runner also generates workspace, tool, and mode instructions around that text
when building the model's system prompt; that wrapper is not in this event.
The view does not reconstruct that wrapper or directory-scoped
preflight activation payloads or claim to show the complete system prompt of
each later request. Those activations retain their original tool results and
are authenticated by the harness's private project instruction ledger. Old
journals with no initial instruction events show that absence explicitly.

Coverage: `instruction_inspection_retains_all_completed_turn_copies_after_reopen`,
`instruction_inspection_rejects_unvalidated_version_digest_order_and_identity`,
`instruction_inspection_selects_one_complete_copy_and_closes_with_scroll_and_focus`,
`instruction_inspection_errors_are_visible_and_foreign_scope_is_hidden`, and
`instruction_inspection_actions_read_reopened_journal_without_effects_and_keep_scope`.
These use real session stores and journals, independent changed project files,
invalid events, narrow/wide UI layouts, and unchanged journal bytes. They do
not establish live provider or platform sandbox acceptance.

The local Codex reference is pinned at `references/codex` commit `33a0f766a647208b471cfbcad889c67fd324ee04` (`codex-rs/core/src/agents_md.rs`). This implementation supports only the `.git` project marker and the two standard filenames. It does not support configurable root markers, fallback filenames, multiple execution environments, or partial truncation when a byte budget is exhausted. It rejects linked documents and directories rather than following them. The path checks and no-follow final-file open prevent static link escapes; concurrent replacement of an ancestor directory during discovery remains a filesystem race and is not presented as a hardened sandbox boundary.
