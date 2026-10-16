# Native subagents: current contract

The native subagent path is opt-in. `SubagentToolExecutor` adds four Build
tools to a base `ToolExecutor`: `spawn_agent`, `send_input`, `wait_agent`, and
`cancel_agent`.
The child `SubagentWorker` receives a separate host-selected base executor,
never its parent's wrapper. The desktop app attaches a wrapper for interactive
Build turns. `SubagentWorker::with_nested_subagents` opts a child into a new
wrapper bound to its **own** native lease. Plan mode does not advertise or
execute coordination tools.

The desktop's Subagents settings persist `subagent-settings.json` in the local
profile. They control delegation and the five existing limits for future parent
turns; running workers retain their admitted configuration. Disabling delegation
hides and rejects `spawn_agent` and `send_input` on new parent wrappers, while
`wait_agent`, `cancel_agent`, saved state and explicit recovery remain available.
It does not revoke the recursive authority of an already admitted child.
Limits retain the coordinator's hard ceilings: depth 8, active direct children
32, running children 32, total root descendants 128 and total direct tasks 32.
Changing limits changes child identity; restore the original profile to recover
existing interrupted work, or use the existing explicit authority-review flow.
Malformed settings block new admission. Reset requires explicit confirmation and
preserves the original document as a backup.

The role catalog comes from the exact table used by `spawn_agent`: `explorer`
and `reviewer` run read-only in Plan, and `worker` runs in Build. Approved plugin
personas are listed separately with their source; they are instruction resources,
not additional built-in permissions or model configurations.

## Durable identity and lifecycle

`spawn_agent` accepts a prompt and returns `agent_id`, the child session ID.
Its spawn ID is a domain-separated SHA-256 of the parent tool call's persisted
ID. `SubagentWorker::prepare_child` creates the child before its model runs;
the child session's first atomic metadata write records parent ID, depth,
spawn ID, and request digest. Repeating the same call ID and request finds
that session. A changed request with the same ID is rejected. A configurable
per-parent tool limit counts every direct child, including completed children.
The coordinator separately enforces depth (default 3), active children per
parent (default 4), running children across the store (default 8), and a
durable total of all descendants of one root session (default 32). The last
limit includes grandchildren and completed work from earlier turns. All
creation checks happen under the same OS registry lock, including after a
process restart. Hard ceilings prevent a host from configuring unbounded
depth or fanout.

Nested wrappers use the same host-selected base executor, permission policy,
run limits, token budget, model, hooks, and event bridge as the child worker.
Only Build children below the depth limit receive coordination definitions.
The child worker's request identity binds the effective catalog, permission
policy, limits, and budget. Recovery with changed authority requires explicit
review rather than silently increasing the child's capabilities. Recursive
children never receive an ancestor's `SubagentControl` or lease.

The worker starts under opaque `SubagentControl` issued while the parent held
its native session lease. This control does not retain that lease. Releasing
the parent turn's wrapper through `shutdown` or `Drop` permits the next parent
turn to acquire the lease while the child continues. A new wrapper can then
wait for or cancel the existing child. The child's own native lease and
journal preserve its run. A process crash leaves a started child
`Interrupted`; it is never started as fresh work automatically.

`wait_agent` only reads the coordinator and, for terminal work, the child's
journal. Its bounded timeout returns the current state and cancelling the
wait does not cancel the child. `cancel_agent` persists cancellation for the
direct child and descendants; a running worker polls that flag and stops
cooperatively. `child_for_call_id` is a read-only host API for finding a child
whose parent `spawn_agent` tool result was lost. It does not dispatch a worker
or assert that the parent tool outcome can be replayed automatically.

## Persistent commands owned by each child

The desktop shares one `NativeCommandRegistry` between native parent and child
workers. `SubagentWorker::with_persistent_commands` opts into a frozen
`NativeCommandProfile`; it does not inherit its parent's command manager.
Build definitions come from the profile's pure schema builder. Plan excludes
them. Reservation identity includes the profile configuration, while recovery
still tries the exact historical identity without a profile. An old reservation
therefore retains its original catalog rather than receiving new command tools.
The profile choice is also frozen for an initial Plan reservation, so a later
Build input cannot select new tools for a legacy child with no profile.

Under the claimed child lease, the worker attaches the profile overlay to a
base executor that already allows `run_command`. The registry binds a manager
and durable output capture to that exact native owner, canonical workspace and
profile. Parent and sibling command IDs cannot resolve through this overlay.
Nested workers share the host profile but acquire their own owner binding.
Completed turns keep the manager for subsequent mailbox turns, including
separate native runtimes. Failed/Cancelled outcomes revoke it under the current
claim. Host restart retains capture, not processes or usable old command IDs.

The same registry actor serves UI controls and the child Runner/Resume. Its
shared notification and operation state allow explicitly confirmed stdin/EOF
while the model is waiting under an active child lease. In Plan, `actor_existing`
binds an already retained manager without creating one or changing the
model's read-only catalog. Runner and Resume use this actor to acknowledge
explicit human operations before their next journal boundary. Manual boundaries
reload permissions, rules and hooks; model execution retains its admitted
snapshot. Disabling command access, changing isolation or dropping the host
registry revokes managers and fences old profiles so they cannot start another
process. A new host configuration must explicitly issue a new profile.
The mailbox stores the requested Plan or Build mode. Model-facing `send_input`
preserves the child's first admitted mode, so read-only roles stay read-only on
every follow-up, including after reopening. A Ready child's mode comes only from
the matching committed parent response and started spawn, then its reservation
digest is checked. Prompt text never grants authority. Trusted host APIs retain
their explicit generic Plan-to-Build transition. The Plan control acceptance
seeds a real command through the trusted host before starting the initial Plan worker.

Coordinator cancellation now returns the exact affected child/descendant IDs
only after its durable commit. Model, bridge and no-bridge UI paths revoke
those owners before signalling worker tokens, including Completed/idle
descendants. Rejected cancellation leaves the registry and mailboxes unchanged.
New descendants cannot be created after their parent's durable cancellation
flag; exact historical spawn retries remain lookups. Trusted cleanup under a
current claim or committed cancellation receipt closes even a newer manager
for the cancelled owner. Model/tool cancellation outside this command registry
remains cooperative.

Local acceptance on Linux, 2026-09-30:

- `child_commands_use_owned_capture_across_followup_runtimes_and_reject_parent_and_sibling_ids`
  runs actual commands, Unicode stdin/EOF and final poll in separately built
  worker runtimes, reopens captured output and rejects foreign command IDs.
  Real pre/post hook processes verify the child session and tool payload.
- `legacy_ready_recovery_and_plan_keep_original_catalog_while_revoked_profiles_fail_before_claim`
  checks legacy Ready recovery and Resume, Plan exclusion, and rejection of a
  revoked profile before claim or model effects.
- `plan_reservations_freeze_command_profile_across_build_followup_and_host_upgrade`
  runs initial Plan and follow-up Build in separate runtimes for both an old
  child without a profile and a new opted-in child. The upgraded host leaves
  the legacy child without persistent tools or manager, while the new child
  retains its explicit profile and actual captured command output. Plan itself
  advertises no persistent tools and creates no manager.
- `plan_worker_processes_confirmed_host_fixture_controls_without_expanding_model_tools`
  seeds a real command through the host registry before an initial Plan worker.
  During model wait, exact stdin and EOF use the shared actor under the child
  lease, receive durable ACKs and allow the same Plan turn to complete. The
  model catalog remains read only. This is a host-seeded fixture, not a
  follow-up Plan admitted through the mailbox.
- `bridge_cancellation_commits_before_revoking_the_retained_child_process_and_notifying_model`
  retains a real child process during a model wait and verifies durable
  cancellation, process revocation and subsequent notification in order.
- `child_start_approval_denial_and_revocation_after_request_prevent_payload_and_capture_registration`
  verifies denied approval and an epoch revoked before Allow: neither path
  launches the payload or registers a command capture.
- Three coordinator tests cover exact cleanup targets, including terminal
  descendants, authority rejection without publication, and creation/retry
  semantics after cancellation. The full
  coordinator test module passes 29 tests; the worker module passes 41, with
  three process-cut helpers ignored by the default filter.
- The desktop acceptance
  `child_commands_share_host_controls_across_model_wait_followup_cancel_and_reopen`
  uses HTTP/SSE, real processes, child approvals and exact UI stdin/EOF under
  a busy lease. Pointer controls at 360/900 px, a rebuilt host follow-up worker,
  final poll and reopen verify shared ownership and retained capture. An extra
  idle process for no-bridge cancellation is launched directly as a fixture;
  it is not another model-issued tool call.

These tests do not certify an external model account, fresh Windows ConPTY/MXC,
macOS/ARM64, or every crash boundary in a child command. Registry acceptance
and the process/capture limits are documented in
[persistent-commands.md](persistent-commands.md).
Native automation command integration remains pending and is not enabled by
this child-worker profile.

## Follow-up input

`send_input` accepts `{agent_id, prompt}` for a direct child. The coordinator
verifies its first-turn runtime identity and permission marker before accepting
a follow-up, when the child has an admitted turn. It then commits an input
record under its OS registry lock before a child can admit a
new turn. The input ID is derived from the persisted parent tool-call ID;
retrying that call with the same prompt returns the same record, while a
changed prompt is rejected. A host can locate the record through
`SubagentToolExecutor::input_for_call_id` when a parent tool result was lost.
The registry keeps at most 16 inputs per child, each with at most 16 KiB of
UTF-8 prompt bytes, and caps the whole registry at 2 MiB.

An input sent while the child is Ready or Running waits for its current turn.
An input sent after Completed starts a new turn in the stored mode in the same child
session. Claiming the next input changes the registry to `Running` while the
child OS lease is held; the new `TurnAdmitted` event uses the input ID and
prompt. The model receives the prior child conversation rebuilt from replay.
`wait_agent` observes the current mailbox state and waits for pending inputs
before returning Completed. A failed or cancelled child closes the mailbox;
`cancel_agent` skips pending inputs and closes a previously completed child to
new input.

A process crash after enqueue leaves a pending entry that an exact retry of
`send_input` or a later `wait_agent` can schedule; the desktop startup dispatcher also schedules
Completed children with pending inputs and a matching original worker identity. A crash after claim marks
the child Interrupted. Recovery checks whether that input's turn was admitted:
if not, it reuses the same input ID; if admitted, `ResumeEngine` replays the
turn and does not repeat a confirmed model response or tool result. Pending
inputs then drain in order. Uncertain external effects still require human
verification. The parent turn's uncertain `send_input` tool result is not
silently replayed; its recorded input can be inspected by call ID.

## Desktop recovery

Selecting an interrupted child shows a **Reanudar subagente** action. The UI
rebuilds the original admitted prompt and spawn ID from the child's journal,
then runs `SubagentWorker::resume` under the parent's OS lease. The worker
claims the child's separate lease through `SubagentCoordinator`; it never
admits a second turn. The parent lease is released while a human decides a
pending permission. The decision is saved in the child's sidecar before
retry, and the worker replays that saved decision.

If a tool started without a confirmed result, the UI requires external
verification and records the result under the child lease; it never repeats
that tool automatically. If a failed or cancelled turn has an unstarted tool
batch, the UI can explicitly discard the remaining calls. A manually closed
child can move from `Interrupted` to its journal-confirmed terminal status
without model credentials, after replay and parent/child lease checks. The
selected child's transcript and sidebar status reload from durable storage
when recovery finishes.

## Persisted desktop overview

The parent and child surfaces show the persisted Ready, Running, Interrupted,
Completed, Failed and Cancelled states, cancellation requests, lineage depth and
count of Pending/Active mailbox entries. `list_children_snapshot_for_ui` also
returns bounded read-only `SubagentInputSnapshot` entries with exact ID, prompt,
mode and Pending/Active/Done/Skipped state. It reads all values under one registry lock, without claiming a parent or worker lease
or writing reconciliation to disk. Released worker leases appear Interrupted.
Live permission cards and explicit child recovery remain available; completed
children keep transcript navigation after the desktop app restarts. Parent
navigation and child navigation are restricted to the selected direct lineage.
An interrupted child can be opened from the parent, then explicitly resumed.
Cancellation is also offered without a live bridge for Ready/Running children
and Completed children with pending inputs. The action requires the selected
parent or child lineage, acquires the parent native lease and persists the
request through `request_cancel_from_ui` before refreshing the overview. Ready
children with an existing admission and interrupted descendants are rejected
under the same registry lock used by worker claims, preserving journal recovery.
A busy parent reports an error rather than bypassing its lease. Running workers
observe the durable flag; pending input is skipped and a completed child's
mailbox is closed. A completed child's successful status is preserved.

Verified on Linux on 2026-09-29: the coordinator test
`ui_snapshot_needs_no_parent_lease_and_does_not_mutate_registry` checks a queued
input and released worker after reopening the coordinator; the desktop test
`sidebar_projects_subagent_status_without_parent_lease` reopens the app after
completion and exercises parent/child navigation and rejection of an unrelated
target. `persisted_subagent_states_and_mailboxes_are_visible_without_a_worker`
checks all six states, pending counts and cancellation flags through actual
`openide-ui` layout at 360 and 900 pixels. These tests use local stores and no
provider credentials. Additional tests
`desktop_cancels_persisted_child_without_bridge_and_reopens_flags` and
`desktop_cancellation_without_bridge_notifies_running_worker_durably` exercise
the actual UI action, restart, unrelated-target rejection, busy parent and worker
flag. `desktop_cancellation_rechecks_entire_subtree_before_mutation` and
`desktop_cancellation_preserves_ready_admission_for_recovery` reject recovery
states without partially cancelling the subtree. The mailbox test
`desktop_cancellation_closes_completed_mailbox_after_reopen` verifies skipped
input, closed follow-up and another reopen. The layout test
`persisted_cancellation_controls_follow_lifecycle_without_live_bridge` checks
availability at 360/900 px. These tests do not prove scheduling via a real model,
live-provider acceptance of startup dispatch or input editing. The "Ver entradas" control
expands the full mailbox, including completed/skipped entries, in a bounded
scroll area. It is scoped to the selected parent/child and grants no edit or
dispatch authority. `mailbox_snapshot_reopens_exact_prompts_and_all_lifecycle_states`
uses actual enqueue/claim/finish/cancel operations, reopens the coordinator and
checks exact multiline Unicode prompts, stable order and status without writing
the registry. `persisted_mailbox_details_are_complete_and_scrollable` exercises
presentation and scrolling at 360/900 px; the sidebar action test checks toggle
and unrelated-target rejection. These fixtures verify stores and the local UI;
full acceptance with a real child model remains pending.
Cancelling interrupted journals and cancellation without a live bridge while
the parent lease is busy remain pending.

Live bridges carry the opaque parent control granted to the prepared worker.
`cancel_child` now returns `Result<bool, SubagentError>`: it persists cancellation
for the child and descendants before clearing approvals or cancelling the token.
It works while a later parent turn holds the native lease. An I/O or validation
failure preserves approvals and the live token, reports an error in the desktop
and permits retry. An unregistered/previously cancelling child returns false;
a bridge registration without durable authority fails closed.
`bridge_registry_failure_keeps_approval_and_token_live_for_retry` corrupts and
restores the registry, checks the pending approval and uncancelled token after
failure, then reopens the coordinator after successful retry.
`bridge_without_durable_authority_cannot_signal_cancellation` checks denial.
`cancelling_child_closes_approval_before_any_effect` uses the actual worker while
another parent lease is held and checks persisted cancellation and no tool
effect. `bridge_cancellation_process_cut_reopens_subtree_before_notification`
terminates a real subprocess with exit 86 after the registry commit and before
approval revocation/token notification. The test-only cut is not compiled into
production. After reopening the store, it verifies released leases, cancelled
child and descendant, cancellation flags and a skipped pending input. This
fixture exercises coordination and open stores, without provider inference or
an admitted child journal. Cuts inside registry publication, cancellation of
interrupted journals and platform equivalence remain pending.

## Desktop startup mailbox dispatcher

On app reopen, the desktop queues Completed children with open mailbox inputs
and no cancellation request. `poll_subagent_dispatches` builds the native worker
through the same `child_worker_request` used by explicit child recovery. It binds
the immutable first admission and spawn ID, model, endpoint, request profile,
permissions, hooks, tools, isolation and budget. It restores the first durable
SystemInstructionsCommitted rather than reloading current project/goal text.
The helper now accepts follow-up admissions; the prior single-admission desktop
restriction has been removed. The harness still validates those admissions
against the mailbox during recovery.

`SubagentDispatchHandle` checks Completed/open/not-cancelled state and worker
identity, issues an opaque control under the parent lease, releases that lease
and drains inputs on an owned native worker thread. Claims and child leases stay
in the coordinator. Its bridge supplies normal child events, approvals and
cancellation to the existing UI. Dropping the host handle cancels its worker.
Dispatch never resumes an Interrupted journal or repeats an uncertain effect.
The desktop verifies Idle replay before dispatch, reports errors in the child
card and offers "Procesar entradas" for an explicit retry after fixing settings.

`startup_dispatch_reopens_http_child_and_drains_frozen_mailbox_once` runs the
initial native child against a local HTTP Chat Completions stream, inserts an
exact committed Pending mailbox fixture, drops the host and reopens the app.
It exercises both automatic startup dispatch and rejection of a wrong selected
model followed by the actual UI retry. Rejection leaves journal and registry
unchanged and makes no HTTP request. Successful dispatch sends the original full
system message despite changed AGENTS.md, admits the queued turn exactly once,
completes the entry and leaves no pending work. It also rebuilds the worker after
two admissions and rejects an unrelated retry target.
`mailbox_dispatch_diagnostic_and_retry_remain_visible_without_worker` checks
error text and reachable retry at 360/900 px on parent and child surfaces.
`competing_dispatchers_claim_one_reopened_input_and_keep_one_model_effect`
races two actual desktop dispatch handles with separate reopened SessionStore
handles. A gated local model keeps the claimed turn active while both starts
compete. The test verifies one follow-up model call, one durable admission and
a Done mailbox after reopen. The original child journal comes from real native
execution; the Pending entry is a persisted fixture. This is handle concurrency
acceptance, not a test of two complete GUI processes or an external provider.

Verified locally on Linux, 2026-09-29. This is HTTP fixture acceptance, without
external provider inference or credentials. Ready children without a first
admission can now use the original started parent spawn as documented below;
without that exact source they still need explicit recovery. No prompt is invented.
Interrupted children require explicit journal recovery. Mismatched/missing
credentials, budget/catalog discovery, permissions or profile block startup;
failed attempts need the displayed retry once their settings are corrected.
Two complete GUI processes, further process cuts in startup/effects and
platform equivalence still require further acceptance.

### Registry publication process cuts

`mailbox_publication_process_cuts_reopen_mutations_without_duplicate_turn`
runs twelve child processes: enqueue, Pending edit, omission and Pending-to-Active
claim, each killed
by a test-only immediate process exit after temporary-file sync, after rename,
and after directory sync. Each child first executes a native initial turn and
enqueues through the actual coordinator API. No destructors release its locks.
The parent reopens stores, verifies absence of an unpublished enqueue and its
idempotent retry, the old complete registry before rename or
the new complete registry after rename, and checks edit revision/prompt and
Completed versus Interrupted state. A published claim requires explicit Resume;
an unpublished claim is dispatched normally. Both paths produce one follow-up
model call and one admission, preserve the committed prompt, and reopen as Done.
The test also retries dispatch and verifies no second model effect. A published
omission survives retry/reopen as Skipped with no follow-up model call or admission;
a cut before its rename retains Pending and dispatches the original prompt.

Verified on Linux, 2026-09-29, with a local model fixture. These are process death
cuts around registry publication, not power-loss guarantees, Windows rename
acceptance, cuts during a model request/tool effect or two GUI process tests.
Test cut hooks are absent from production builds. A process death before rename
can leave an unreferenced temporary file; the reader ignores it and retains the
previous registry. `recover_registry_temporaries` now removes abandoned regular
publication files under the writer lock, after validating the published registry.
The desktop calls it during startup recovery and the next registry write also
cleans orphans before creating its own temporary. Canonical lowercase v4 UUID
names alone qualify; directories, symlinks and unrelated names remain untouched.
Corruption of the published registry prevents cleanup and is reported. Snapshot
reads do not invoke cleanup. Unix cleanup syncs the directory after deletion;
Windows/platform durability remains unverified.

`registry_temporary_recovery_is_scoped_preserves_authority_and_rejects_corruption`
verifies name/type boundaries, preserved registry bytes, corrupt-registry
rejection and idempotent recovery. On Unix it also verifies a symlink and its
external target are untouched. `registry_temporary_recovery_waits_for_live_writer_lock`
verifies that recovery cannot delete a live writer's temporary; it waits for the
same OS lock. The twelve process cuts additionally verify one orphan removed
before rename, none after rename, repeated recovery returning zero and published
registry bytes unchanged. `desktop_startup_recovers_orphan_registry_temporaries_without_changing_journal`
checks actual app reopen cleanup, unrelated-file preservation and unchanged
session events.

## Omitting one pending input

`SubagentCoordinator::skip_pending_input` requires the direct parent's live
native lease and the expected input revision. Under the same registry lock as
claim/edit, it changes only Pending to Skipped and increments the revision.
Active/Done entries and stale revisions are rejected. The original request
digest, effective prompt, mode and ordering are retained; `send_input` retries
cannot recreate the omitted work. An exact skip retry after an ambiguous commit
returns the same snapshot. Other entries and historical child success remain
unchanged; the child is not cancelled or its mailbox closed.

The desktop offers "Omitir entrada" for Pending inputs. The action includes the
rendered revision and validates selected parent/child scope, obtains the parent
lease and calls the coordinator. A successful omission closes an editor of that
entry; a failed omission preserves the draft and displays the error. A busy
parent requires retry after it releases its lease; this action does not bypass
that authority through the worker bridge.

Evidence on Linux, 2026-09-29:
- `pending_skip_reopens_preserves_retries_and_only_removes_one_entry`: edited
  prompt/revision, stale and foreign-parent rejection, reopen/idempotent skip,
  immutable original enqueue retry and claim of only the retained input.
- `pending_skip_and_claim_share_one_atomic_winner_across_handles`: eight races
  using separate coordinator/store handles, with exactly one skip/claim winner.
- `skipped_mailbox_input_is_never_admitted_or_sent_to_model_after_reopen`: real
  native initial and retained turns through a local model; drop/reopen, repeated
  dispatch, no omitted admission and only one retained follow-up model call.
- `mailbox_skip_actions_reject_busy_stale_unrelated_and_reopen_one_omission`:
  actual desktop actions, busy parent, wrong target, concurrent edit, child
  selection, colon in input ID, editor closure, retry and app reopen. Its Pending
  mailbox is a persisted fixture; harness tests enqueue through the coordinator.
- `mailbox_skip_control_is_reachable_only_for_pending_entries`: focus/geometry
  at 360/900 px on parent/child surfaces; no omission control for Active inputs.

The registry publication acceptance above also covers skip at all three cut
stages, including idempotent retry and zero admission/model calls after a
published omission. Platform equivalence remains pending.

## Editing pending input

The desktop offers "Editar entrada" for a Pending mailbox entry. The host
claims the parent native lease, rechecks the selected direct lineage and invokes
`edit_pending_input` under the registry lock used by worker claims. Blank prompts
and more than 16 KiB of UTF-8 text are rejected. Active/Done/Skipped entries and
closed mailboxes are immutable. An expected revision prevents overwriting a
concurrent edit; an exact retry of a committed edit is idempotent while the input
remains Pending. A failed save keeps the desktop draft for review/retry.

The original `send_input` digest remains immutable. Repeating its original tool
call returns the same entry with its edited effective prompt; reusing that call
ID with different original arguments still conflicts. The worker claims the
current prompt atomically and uses it for TurnAdmitted and model context.
Registry version 3 stores a revision (default zero for older inputs), reads v1/v2
and is accepted by coordinated native deletion. Older binaries reject v3.

`pending_edit_reopens_preserves_original_retry_and_blocks_claimed_changes`
checks reopen, ambiguous-commit retry, unchanged original digest, original-call
retry, conflicting content/revision, wrong parent, bounds and immutable claimed
entries. `pending_edit_and_claim_share_one_atomic_winner_across_handles` races
an editor and claimant with separate store handles; the claimed prompt is either
the original with revision zero or the committed edit with revision one.
`queued_follow_up_recovers_after_enqueue_claim_or_admission_crash` now edits the
queued prompt, reopens at each stage and verifies a single admission of the edit.
`mailbox_edit_actions_migrate_reopen_and_preserve_conflicting_draft` exercises
actual desktop actions with a v2 fixture, validation, busy parent, v3 migration,
restart, unrelated target and a concurrent edit without losing the local draft.
`mailbox_edit_form_is_reachable_on_small_parent_and_child_surfaces` verifies the
editor and save/discard controls through scroll/focus at 360/900 px.

These tests exercise local stores, worker execution and the UI framework; they
do not constitute provider inference acceptance. Per-entry removal, editing
while the parent lease is busy, and process cuts inside edit publication remain
pending. Editing does not dispatch an otherwise dormant mailbox.

## Current limits

- The desktop host uses its configured local tool permissions for children and
  relays live approvals to the parent UI. It must opt into recursion on both
  creation and recovery workers with identical limits. Without that setting,
  a child has no coordination tools.
- A changed provider, endpoint, request profile, hooks, instructions, tool
  catalog, permission policy, or limit can block active recovery because the
  spawn identity must match. An earlier version 2 child can recover with its
  original nonrecursive catalog when its recorded identity and permission
  marker both match. The fallback never advertises nested tools in recovery
  or follow-up turns. A missing or changed permission marker fails closed.
  Manual outcome verification and batch discard do not need those settings.
  Recovery also handles a claimed first child turn interrupted before its
  first `TurnAdmitted` event and older Ready children whose first admission
  was committed before the worker claimed the lease.
- There is no fork, push notification, or model-facing close operation yet.
  `wait_agent` polls; an interrupted child needs explicit recovery through its
  journal. A pending input after a process restart is scheduled when a parent
  retries `send_input` or calls `wait_agent`; the desktop startup dispatcher also
  handles Completed and Ready children with matching original worker identity.
  Ready requires the committed/started parent spawn; interrupted children still
  require explicit journal recovery.
- Cancellation is cooperative for models and tools outside the host command
  registry. Persistent child commands are also revoked by that registry after
  durable cancellation. A model or other tool that ignores cancellation can
  keep running, although it cannot hold the parent's lease through the
  opaque control.
- The desktop Build path constructs the wrapper, but end-to-end agent-control
  behavior still needs broader UI and crash testing. These guarantees do not
  amount to Codex agent-control parity.

The lifecycle tests cover atomic lineage, crash/reopen, total limits across
nested completed work, concurrent children, idempotent retry, cross-turn lease
handoff, nested cancellation across restart, child-scoped recursive dispatch,
host permission denial, Plan catalog exclusion, a cancelled wait, and
follow-up recovery after enqueue, claim, and admission.
Session deletion constraints are documented in
[session-lineage-deletion.md](session-lineage-deletion.md).

## Native goals in child workers

The desktop now opts workers into `with_native_goals`. Goal schemas contribute
to child reservation/context identity. Each admitted child turn, including Plan
and the maximum delegation depth, receives a fresh child-lease/turn wrapper.
Recovery uses fresh recovery authority, never an ancestor wrapper. Parent ledger
content and completion authority are not inherited; child goals require explicit
host activation. Older reservations match former goal/recursive catalog variants
and retain their original advertised tools. See `docs/native-goals.md` for the
three worker acceptance tests, exact scopes and fixture/process-cut limits.
The goal editor now permits explicit native session activation and pause/resume;
complete child UI workflows, receipt reconciliation and autonomous continuation
remain pending; this does not replace the existing composer goal text contract.

Native goal context now binds each child's own ledger to its frozen instruction
snapshot. Reservation identity still binds the original project/base instructions;
restoring from a known envelope accepts a decoded base only after matching the
recorded reservation/profile. A real Build follow-up test verifies reconstruction,
a third model call and identical nonnested snapshots. Parent native goal data is
not copied into a child's base by the desktop Resume factory. See the goal context
contract for legacy marker handling, 512 KiB limits and remaining dedicated cuts.

## Initial Ready reservation dispatch after desktop reopen

Startup now includes noncancelled Ready children, including an empty mailbox.
The initial journal must be empty. `validate_initial_child` checks spawn ID and
the exact reserved prompt/mode/model/runtime profile before a claim; it never
admits a turn. The dispatcher uses `run_prepared` for the initial task, which
claims the child OS lease and drains queued input only after successful completion.
Completed children keep their existing mailbox dispatch path. Interrupted/Running
children are not automatically resumed by this change.

Without a first child admission, the desktop reconstructs the request from the
parent journal using `reserved_spawn_request`: exact hashed spawn call ID, Build
admission/model, committed model tool call and its matching ToolStarted, strict
spawn arguments and version/digest-validated frozen instructions. Native goal
envelopes contribute only the project base; the child retains its own goal scope.
Current project instructions cannot silently replace this frozen source. The
reservation digest validates the reconstructed runtime; a missing source, unknown
profile, changed settings or mismatched model remains a visible dispatch error.
Legacy reservations without this source are not guessed or upgraded.

`initial_ready_dispatch_reopens_validates_scope_and_claims_one_worker` passed
with a real reservation, separate reopened store handles, competing dispatchers,
wrong prompt/mode/spawn rejected without events, and one admitted/completed turn
with one local-model request. The initial model counter is offset solely to gate
its first request; exactly one increment is observed.
`startup_dispatch_reopens_http_child_and_drains_frozen_mailbox_once` now passes
four scenarios: Completed/Ready, each with automatic dispatch or an initial wrong
model and explicit retry. Ready uses a parent journal fixture and a real child
reservation; mailbox entries are fixtures, execution and HTTP transport are real.
`initial_reservation_source_requires_original_started_build_call_and_frozen_context`
rejects missing start, foreign spawn, Plan, changed response/start arguments,
changed instruction digest and injected permission fields.

These tests do not prove power loss, new real desktop process cuts, provider
inference or compatibility with all legacy Ready profiles. Those remain pending.

Verificación del arranque inicial de reservas Ready: la aceptación HTTP startup
pasó cuatro escenarios y la suite de dispatch handles pasó dos pruebas; la
prueba de fuente del spawn también pasó. openide-harness completo pasó
415 pruebas, con 11 ignoradas, en 5 suites. Workspace se detuvo en la misma
prueba del picker `disconnected_provider_shows_one_connection_action_instead_of_models`
(518 passed, 1 failed, 2 ignored en app); no es una corrida verde de todo el
workspace. Clippy terminó sin errores y con 47 advertencias. Fmt/diff check pasaron.
No se cuentan los fixtures de journal/mailbox como inferencia ni como cortes
reales del desktop. Los demás criterios pendientes del roadmap siguen abiertos.

## Process-death acceptance for initial child claim/admission

`initial_child_dispatch_process_cuts_reopen_one_admission_and_exact_context`
passed four real subprocess exits (code 86, without Rust destructors): before
claim, after claim, after the first durable admission, and after TurnCompleted
before registry finalization. Test-only cut hooks
exist only under cfg(test) and match this fixture spawn/point. The child reservation
and coordinator/worker publication are real, not an injected journal prefix.

After reopen, before-claim remains Ready with no admission; after-claim becomes
Interrupted without admission; after-admission becomes Interrupted with exactly
one admission. No model request was sent before the first three cuts. The fourth has one
request/committed response/terminal event, with the registry still Running. Normal run_prepared
rejects both Interrupted states without changing events or requesting a model.
Explicit Resume completes both with exactly one admission, one local-model
request, one terminal event and the original complete system instructions. For the terminal cut, Resume changes only the registry, sends no new model
request and leaves the journal events identical. The
child lease is released and the registry projects Completed after recovery.
The ignored `initial_dispatch_cut_child` is invoked by the parent test.

This proves worker process death at claim/admission, not desktop GUI process death,
provider inference, syscall interruption or post-admission tool-effect boundaries.
Those acceptance requirements remain open.

Verificación de cortes del arranque/cierre inicial: openide-harness pasó
416 pruebas, con 12 ignoradas (5 suites), antes de ampliar el cuarto corte;
la prueba enfocada ampliada pasó los cuatro cortes. Los helpers ignorados se
ejecutan desde sus pruebas padre. Clippy terminó sin errores y con 47
advertencias; fmt/diff check pasaron. Workspace continúa fallando en
`disconnected_provider_shows_one_connection_action_instead_of_models`
(518 passed, 1 failed, 2 ignored en app), por lo que no se presenta como
aceptación verde de todo workspace.

El cuarto corte ocurre después de TurnCompleted y antes de finish_child: tras
reopen el registro proyecta Interrupted, y Resume explícito reconcilia Completed
sin otra petición al modelo ni eventos nuevos de journal. Son cortes reales
del worker, independientes de la aceptación HTTP desktop con fixtures del padre.
No demuestran cortes dentro de syscalls, efectos de herramientas posteriores,
dos procesos GUI completos ni proveedor externo. Los criterios restantes siguen abiertos.
