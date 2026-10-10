# Native goals: durable core contract

Status: partial. This is OpenIDE-owned Rust state, independent of Codex/OpenCode
processes/libraries. The read-only Codex reference at `33a0f766` distinguishes
thread goal status from contextual user-goal text; it informed the separation,
not the implementation or a dependency.

## Current contract

`openide_core::NativeGoalStore` stores a versioned ledger in
`<session-id>.native-goals.json`. Its `create` and `update` require the native
session lease from the matching store, then serialize writers with a per-session
OS lock. Mutations write a bounded temporary file, sync, rename and sync the
directory on Unix. Read-only `list` does not mutate the journal/ledger.

A creation ID binds an immutable full objective and ordered criteria. Creating
another objective while the latest is unfinished is rejected; completed goals
remain in history. An exact creation retry returns the existing state. Criteria
must have distinct IDs, nonblank descriptions and no prefilled evidence.

The current states are Active, Paused and Complete. Pause/Resume are explicit
user mutations; host routes must not expose those variants as model tools.
Updating requires the expected revision and a unique operation ID. All accepted
mutations are retained and exact retries, including older operations, return the
current snapshot without executing again. Reusing an operation ID with changed
arguments conflicts. Competing updates at the same revision have one winner.
Ledger reads validate IDs/revisions and reconstruct status/evidence from the
mutation history; inconsistent projections are rejected.

`RecordEvidence` binds a criterion to a nonblank summary and a turn ID with a
persisted TurnCompleted in the same native session. Unknown, admitted-only or
failed turns do not qualify. `Complete` requires Active status and evidence for
every criterion; each referenced turn is rechecked before publication. Completed
goals cannot be changed. Both ordinary and coordinated native-tree deletion
remove the goal ledger with the session.

Limits: 32 goals/session, 32 criteria/goal, 256 mutations/goal, 4 MiB ledger,
64 KiB objective and 16 KiB criterion descriptions/evidence summaries. Exceeding
limits fails instead of truncating scope or dropping history. The goal ledger
is separate from model context and never promotes evidence text to instructions.

## Acceptance observed on Linux, 2026-09-29

- `goals_reopen_immutable_pause_resume_and_retries_without_new_journal_events`:
  immutable creation, unfinished-goal exclusion, pause/resume revisions,
  separate store reopen, old-operation retry, conflict and read-only projection;
  goal updates do not add turn events; session deletion removes the ledger.
- `goal_revision_race_has_one_winner_and_foreign_lease_is_rejected`: two separate
  store handles share authority and race at one revision, with exactly one
  winner; a lease from another root is rejected.
- `invalid_goal_criteria_and_corrupt_ledger_do_not_mutate_store`: empty/duplicate
  criteria and malformed published ledger are rejected without replacement.
- `native_goal_completion_requires_all_evidence_from_real_completed_turn_and_survives_reopen`:
  an actual native AgentRunner/local-model turn, premature/unknown evidence
  rejection, all-criteria requirement, pause preventing completion, resume,
  completion retry, store reopen with exact goal/history/journal, another explicit
  goal after completion and coordinated tree deletion.

## Publication recovery acceptance

`goal_publication_process_cuts_reopen_exact_state_and_retry_one_revision` executes
fifteen subprocesses: create, pause, resume, record evidence and complete, each
terminated with an immediate test-only process exit after temporary-file sync,
after rename and after directory sync. No Rust destructors release their native
lease or writer lock. Reopen observes the old complete ledger (or no ledger for
an unpublished creation) before rename and the new complete ledger afterward.
An exact retry yields one durable revision, preserves objective/criteria and does
not append turn events. A second retry leaves published ledger bytes unchanged.

The evidence/complete cases use a core journal fixture with admitted/started/
completed markers, not provider inference or a tool effect. The separate harness
integration test above exercises an actual runner/model. These cuts demonstrate
process death recovery on Linux, not power-loss durability or Windows rename.
The publication cut hooks are absent from production builds.

`recover_temporaries` requires the matching native lease and writer lock,
validates the published ledger and deletes only regular, canonically named v4
UUID temporaries for that session. It never promotes an orphan. Creation/update
also clean such temporaries before publishing. Read-only list stays unchanged.
The fifteen cuts verify one orphan before rename, zero afterward, repeated
cleanup returning zero and published bytes/absence preserved.
`goal_orphan_recovery_is_session_scoped_and_preserves_corrupt_evidence` verifies
other sessions, unrelated names, directories and (Unix) symlinks stay intact;
a corrupt ledger prevents cleanup, and the next mutation removes a new orphan.
`recover_idle_sessions` inventories only published ledgers and canonically
named regular goal temporaries, deduplicates session IDs and takes nonblocking
native leases. It does not load every transcript. A busy session is retained in
`busy_session_ids`; a corrupt ledger, missing metadata or non-native session is
preserved and diagnosed independently while valid sessions continue. The report
contains exact session IDs and errors rather than treating skipped stores as
successfully recovered.

The desktop now calls this after coordinated deletion/subagent registry recovery
at startup, retains the report and projects error messages into the existing
session-list warning. The store stays available after individual goal errors.
`startup_goal_recovery_skips_busy_reports_each_bad_store_and_continues` verifies
healthy/busy/corrupt/missing/non-native cases, separate-store reopen, release of
the busy lease, repaired ledger and idempotent cleanup. The desktop test
`desktop_startup_goal_recovery_keeps_busy_and_corrupt_ledgers_visible_after_reopen`
uses actual app drop/reopen, leases, goal creation and a pending native admission;
it verifies cleanup counts, untouched busy/corrupt stores and admitted journal,
retained full diagnostic IDs, a rendered warning and a second reopen after repair.
The warning explicitly labels startup observations, rather than claiming a
later refresh reverified externally repaired stores. The existing sidebar
truncates the visible warning; a complete diagnostic panel
and an in-app maintenance retry remain pending with goal UI integration. Busy
sessions are retried on a later startup or explicit core maintenance call; no
autonomous continuation is scheduled by cleanup.

## Native harness tool adapter

`openide_harness::NativeGoalToolExecutor` wraps a host-selected executor for one
current unfinished native admission. Construction validates the native lease,
turn ID and persisted mode; duplicate native goal tool names in the base are
rejected. Execution revalidates that admission and workspace. Normal wrappers
lose authority after terminal events or a later admission. A trusted host that
has replayed a recoverable journal and received an explicit Resume action can
construct a fresh `new_for_recovery` wrapper for a failed/cancelled admission;
this does not reactivate the old wrapper. Completed admissions and later turns
still revoke access. `shutdown` releases its lease reference and closes goal tools.

Plan advertises `get_goal` only. `ToolExecutor::certifies_plan_read` distinguishes
nonmutating Plan authorization from concurrent-read certification. The runner,
ResumeEngine and child catalog filter require an exact trusted Plan read name,
executor certification and no configured hooks for that tool. The goal getter is certified for
Plan but never for parallel waves. MCP composites delegate certification only
for local routes; subagent/historical wrappers preserve it only for advertised
local tools. External MCP hints cannot authorize a Plan tool.
Build also advertises `record_goal_evidence` and
`complete_goal`. The tools cannot create/shrink objectives or pause/resume them;
those remain explicit host actions. Mutation args reject unknown fields and do
not accept a session ID or status override. Operation IDs hash the admitted turn
ID and provider tool-call ID, making retries stable within a turn and preventing
collisions when a later turn reuses a call ID. Mutations retain core revision and
all-criteria guards. Goal operations remain serial; base read certification and
non-goal tool execution are delegated to the host executor.

`get_goal` returns the latest goal's exact objective, status, revision and all
criteria/evidence. It omits internal mutation history to avoid repeating that
history in model context. Evidence can reference only an already completed turn
of this session; the still-running turn cannot certify itself. These tools do
not independently verify semantic claims and their descriptions state that
limit. A terminal marker alone does not demonstrate acceptance criteria.

Evidence (Linux, 2026-09-29):
- `goal_tools_enforce_plan_scope_arguments_cancellation_and_shutdown`: current
  admission binding, Plan catalog/mutation rejection, duplicate wrapper names,
  empty get args, wrong workspace, cancellation, closed authority and untouched
  revision/status.
- `build_goal_tools_reject_unknown_evidence_and_stale_turn_authority_without_mutation`:
  missing/current-turn evidence, premature completion, injected status/foreign
  goal, terminal revocation and a later turn's distinct operation identity.
  Its terminal journal is a core fixture, not provider inference.
- `native_goal_tools_run_serially_in_real_turn_and_reopen_completed_criteria`:
  actual seed runner/local-model turn and a second model-driven native turn with
  get/evidence/complete in one tool batch, three committed results, full objective
  present in model context, two model calls, Complete revision 2 and exact reopen.

The desktop installs this after durable admission for normal parent execution,
before MCP preparation, and installs a fresh recovery wrapper in ResumeWorker.
Resume freezes the saved tool digest; historical catalog variants remove goal
tools for turns predating their availability, combined with earlier subagent and
persistent-command generations. Hidden goal calls are rejected before delegation.
Legacy journals without a native admission keep their old scope because no exact
turn identity exists. The desktop opts its child workers into child-scoped goal tools; automation and
other host-path coverage remain pending.
Uncertain tool-result recovery still uses the existing journal/manual gate; no
new automatic replay of a goal side effect is enabled. Recursive hosts must
construct a fresh child-scoped wrapper, not pass an ancestor's lease.

### Exact committed-mutation inspection

`NativeGoalStore::committed_update` reads the validated ledger under the matching
native lease/writer lock. It matches goal ID, operation ID, expected revision and
full mutation arguments, including evidence. A changed payload conflicts; an
absent goal/operation returns None. Corruption remains an error, not absence.
Historical completed goals can be inspected after a newer goal was explicitly
created. A receipt distinguishes `committed_revision` from `current_revision`;
it is not a recreated original tool result or independent semantic verification.
Inspection leaves ledger and journal bytes unchanged and never cleans temporaries.

`NativeGoalToolExecutor::committed_mutation` exposes this only to the trusted
host, not as a model tool. It requires the same active Build admission, exact
turn/call identity, workspace and live lease, and rejects cancellation, closed
wrappers and nonmutation names. A missing receipt does not authorize replay.
No ToolFinished or recovery decision is synthesized by lookup.

`goal_tool_commit_cuts_reopen_exact_receipts_without_repeating_mutation` runs two
actual runner/local-model subprocesses, exiting immediately after successful
record_goal_evidence or complete_goal execution and before ToolFinished. Reopen
observes the unfinished tool journal, exact receipt and revision. Repeat lookup,
changed revision, absent operation, foreign workspace, exact execution retry and
closed authority are checked; neither lookup nor retry changes ledger/journal.
The ignored helper `goal_tool_commit_cut_child` is invoked by its parent test.
The existing publication subprocess test also checks presence/absence of exact
receipts across all twelve mutation publication cuts before any retry. Core
reopen tests distinguish older committed/current revisions and reject altered
mutations/corrupt ledgers; the real-runner lifecycle test inspects a historical
goal after creating its successor.

These are Linux process-death tests, not power-loss or real-provider acceptance.
Desktop receipt inspection is integrated as described below. Journal reconciliation
and complete UI/platform acceptance remain pending; the uncertainty gate still applies.

### Parent/Resume integration acceptance

- `plan_runner_reads_native_goal_without_advertising_mutations_or_changing_ledger`:
  a real admitted native runner/local model sees get_goal, no mutation tools,
  reads the full objective through a committed result, completes in two model
  calls and leaves goal ledger bytes unchanged. It checks getter authorization
  for Plan and absence of parallel-read certification.
- `resume_installs_goal_reader_and_preserves_frozen_plan_catalog_after_store_reopen`:
  a persisted Plan request fixture is reopened through the actual desktop worker,
  which installs tools and performs two real local-model calls plus get_goal;
  objective/context, frozen schema and original instruction snapshot are checked
  even after a user pause changed the ledger and the host supplied changed
  project text. The resumed model sees the original Active snapshot while the
  current ledger remains Paused; the tools remain authoritative for current
  revision/status. The interrupted prefix is a core journal fixture, not a
  process-cut run.
- `frozen_catalog_before_goals_blocks_new_goal_tools`: exact old catalog digest,
  no newly advertised goals, no Plan certification for hidden getter and direct
  injected get_goal rejection by the historical wrapper.
- `explicit_recovery_gets_fresh_authority_after_failure_or_cancel_but_not_later_admission`:
  failed/cancelled journal fixtures revoke normal wrappers and normal constructor;
  only a fresh trusted recovery binding can read, and a later admission revokes
  it. The existing completed-turn test also rejects fresh recovery construction.

The parent app admission path is wired in code. Native activation/controls have
app-action and framework pointer tests below; acceptance of the entire UI → model
→ evidence → completion → checkpoint cycle remains pending. No new autonomous scheduler
or uncertain-result reconciliation is implied.

### Child-scoped installation and recovery

`SubagentWorker::with_native_goals` opts a host into the exact goal schemas in
reservation/context identity. Desktop parent, Resume and mailbox dispatch worker
construction now enables it. Base child tools stay host selected; each claimed
turn creates a fresh wrapper from the child's own native lease and latest
admitted turn ID. Plan and children at maximum delegation depth still receive
goal tools, even though they cannot delegate. Recovery uses fresh recovery
binding. A parent wrapper/lease is never passed down as child goal authority.
Neither goals nor evidence are implicitly created/copied from the parent.

Existing child selection matches its recorded request digest against current and
former combinations of goal/recursive tool surfaces. Older reservations retain
their original catalog through recovery and follow-ups; mismatched models,
permissions and profiles retain their existing failure gates. The new schemas
participate in the native model context digest and child reservation identity.

- `child_goal_tools_are_scoped_to_child_in_plan_and_build_at_depth_limit` executes
  real child workers/local-model turns in both modes, at max delegation depth.
  Each reads its exact child objective, not the parent's private ledger content;
  Build attempts to close the parent's goal and to cite a successfully completed
  parent-turn fixture are rejected. The parent journal and both goal ledgers
  remain unchanged after reopen. The completed parent evidence marker is a core
  fixture, not parent inference.
- `goal_enabled_host_recovers_legacy_child_without_adding_goal_tools` reopens a
  former child reservation with an in-flight model request fixture. A newer host
  enabling goals/recursion performs real recovery inference with no goal tools
  added, completes once and preserves its one committed original catalog.
- `goal_enabled_child_recovery_reopens_own_ledger_and_exact_catalog` reopens an
  interrupted request fixture reserved with goal tools, creates an explicit
  child goal through core authority, and runs actual child recovery/model/tools.
  Two model calls, three tool results, unchanged exact catalog digest and exact
  child-goal reopen are verified. These recovery prefixes are journal fixtures,
  not process-death cuts; receipt reconciliation and the complete child goal workflow remain
  pending.

## Explicit native editor activation and user controls

The existing goal editor now projects `NativeGoalUiState` from the selected
native session. Its separate "Activar goal nativo" action requires an idle native
session, exact editor/session scope and the real session lease. The objective is
stored as entered; nonblank criterion lines become distinct ordered criteria
with trimmed descriptions and no evidence. Core bounds and empty-criteria guards
apply; the editor does not invent a single criterion or silently activate an old
text-only goal. Activation does not rewrite `composer-goals.json` or add turn
events. Unsaved criterion text is a session-scoped in-memory draft, not durable
accepted goal state. A draft without an existing session cannot activate yet.

The native section displays Active/Paused/Complete, revision, criterion count,
each description and evidence summary/turn or Pending. Existing unfinished goals
cannot be replaced. Pause/Resume buttons encode session/goal/revision/action in
the control ID. The host rechecks that snapshot and performs revision CAS under
a newly acquired native lease, with a unique operation ID. Busy sessions, wrong
selection, stale controls and corrupt ledgers produce errors without optimistic
state updates. Reopening the editor refreshes authoritative goal state; session
switches reset the native draft. The model tools still cannot manufacture these
user pause/resume actions. Clearing legacy composer text does not delete a ledger.

- `native_goal_editor_activates_exact_criteria_pause_resume_and_reopens` dispatches
  real app actions, checks explicit objective/ordered criteria/empty evidence,
  untouched journal/text-only store, pause, stale duplicate rejection, resume,
  immutable contents and actual app drop/reopen/editor restoration.
- `native_goal_editor_rejects_busy_empty_stale_and_corrupt_store_actions` checks
  empty criteria, a real externally held lease, running host, foreign selection,
  an external revision change and corrupted published bytes. No failed action
  overwrites accepted state or corruption evidence.
- `native_goal_editor_renders_activation_criteria_and_scoped_pause_action` uses
  the own UI tree/layout and pointer events at 420/900 px to reach criteria and
  activation, then verifies rendered criterion text, no replacement action for
  an active goal and a pointer event with the exact scoped pause ID.

These tests do not establish complete maximum-size/320px/keyboard/accessibility
presentation, migration, autonomous execution, receipt UI or full
model/evidence/checkpoint acceptance from a UI-created goal.
Those criteria remain pending rather than being inferred from rendered buttons.

## Frozen native goal context

`native_goal_system_instructions` constructs a bounded version-1 envelope with
base project instructions and a projection of goal ID, full objective, status,
revision and every criterion/evidence item. Mutation history is excluded. Host
policy explicitly identifies goal/evidence fields as data, preserves the complete
scope, does not grant new tool permissions and disallows autonomous resume of
paused/completed goals. This instruction is not an implemented scheduler.
No goal leaves the old base instruction format intact. Serialization is lossless;
unknown-format/version envelopes are not interpreted as known bases.

The normal desktop parent reads the validated ledger under its native lease,
binds the full instructions before admission (so overflow/corruption cannot
create a new unfinished admission), and gives the runner that snapshot. Child
workers retain their reservation base instructions and bind only their own
ledger when executing a fresh turn. Base project instructions are not replaced
with a parent's native goal context. The existing SystemInstructionsCommitted
event and model-context digest freeze the complete result before inference.
The 512 KiB instruction limit applies to the serialized combined snapshot;
a goal valid for the larger ledger may exceed it. That turn fails rather than
truncating objectives, dropping criteria or claiming a partial scope as success.
Model-specific token-budget gates still apply afterward.

Resume uses the exact persisted instruction snapshot, never rebinding it to
later goal revisions or changed project text. A pre-context recovery without
that snapshot can bind current ledger state; a legacy ModelContextCommitted
without instructions retains its original base path and digest validation,
rather than inventing new goal context after a request had already started.
Current goal tools can observe newer status/revision than the frozen snapshot;
mutation CAS and pause/completion guards use the ledger, not instruction data.

`native_goal_base_instructions` decodes one known canonical envelope for child
reservation reconstruction. It is not goal authority. Worker selection first
tries the opaque instructions and only accepts a decoded base if the recorded
reservation/profile digest matches, across the known tool-surface generations.
This preserves original project context for recovered children and future
follow-ups without nesting or copying native goal projections repeatedly.
The desktop Resume child factory uses the decoded base; the parent engine
continues to use its original full persisted snapshot.

Evidence (Linux):
- `goal_context_preserves_all_data_and_exact_base_without_mutation_history`:
  full objective with quotes/newlines/Unicode, every criterion, Paused/revision,
  exact base including header-like literal text, no mutation-history projection,
  unchanged no-goal base and rejection of unknown envelope version.
- `oversized_goal_context_fails_without_shortening_scope`: a valid 32-criterion
  ledger exceeds the combined instruction cap; binding fails and the complete
  goal remains intact rather than being narrowed.
- The real Plan runner test now asserts objective/criterion presence in the
  first system request and the exact persisted instruction content.
- The desktop Resume test now proves frozen content despite later ledger pause
  and changed host instructions, with two actual local-model calls plus getter.
- The real child Plan/Build test asserts its own objective in the first request,
  absence of parent private goal content, and additionally reconstructs a Build
  worker from its frozen envelope, dispatches a queued follow-up and checks two
  identical instruction snapshots and a third real model call. Decoded bases
  must match the original reservation; no duplicate goal envelope is introduced.

### Selecting a single native task objective

`native_task_instructions` selects the current validated ledger projection over
old text-only composer goal data whenever a native goal exists, including
Paused and Complete. The parent gets the full native envelope; the child
reservation base retains exact project instructions and no parent/obsolete task
objective. With no ledger goal, the original legacy text behavior remains.
The composer label also selects the native objective when present. Stored
legacy text is preserved rather than silently migrated or deleted; a completed
native goal does not implicitly reactivate it.

For a Resume admission before SystemInstructionsCommitted/ModelContextCommitted,
the desktop sends project and legacy task data separately. The worker selects
against the authoritative goal ledger after acquiring the native lease. It does
not trust a pre-lease choice of legacy/native task text. After either context
marker exists, the original frozen/digest-validated recovery behavior takes
precedence; the new selector cannot substitute a different objective mid-turn.

- `native_ledger_goal_excludes_legacy_objective_from_parent_and_child_base` checks
  Active, Paused and Complete store projections, exact project base, full native
  objective/criterion, absence of stale legacy text, unchanged ledger bytes and
  the no-ledger fallback. Completion uses core admitted/completed fixture markers,
  not provider inference or semantic verification.
- `native_goal_editor_activates_exact_criteria_pause_resume_and_reopens` now starts
  with actual persisted legacy text, activates a different explicit native goal,
  verifies preserved legacy storage, native objective in composer projection and
  correct parent/base selection, then exercises pause/resume/reopen as before.
- `resume_before_context_commit_selects_native_goal_under_lease_and_preserves_legacy_fallback`
  reopens two admitted Plan journal fixtures before a context/model request,
  then executes the actual desktop worker/local model. It verifies exact project
  context, native-over-legacy selection or no-ledger fallback, one actual model
  call, persisted selected instructions and unchanged goal ledgers. These are
  fixture prefixes rather than process cuts.

Dedicated process cuts at goal-context publication, goal-context checkpoint/
compaction acceptance and the complete UI-created goal/model/evidence cycle
remain pending; existing generic snapshot/HTTP recovery acceptance is not
claimed as dedicated goal-context acceptance.

## Remaining work

Evidence summaries are assertions associated with successfully completed turns;
this association does not independently prove that a semantic acceptance
criterion was satisfied. A completed fixture turn is not provider acceptance or
functional parity. Trusted verification/host tooling still needs to validate the
claimed behavior and represent unresolved criteria honestly.

The composer retains its existing `composer-goals.json` text contract and does
not silently migrate it. The editor now has separate explicit native activation
and pause/resume controls, and displays native status/criteria/evidence. The new
ledger is now included in frozen native instruction snapshots through the
contract below. Autonomous continuation remains pending.
The adapter is installed for normal desktop parent runs, Resume and opted-in
child workers; automation and other host routes remain pending. Blocked audits, usage/token/time budgets and
limits, cancellation interactions, interrupted-turn/checkpoint recovery,
further cuts during execution/checkpoint recovery, complete diagnostic/retry UI and platform acceptance also remain.
The roadmap goal row must remain Partial until those criteria are implemented
and verified. No native sessions should depend on reference process/library code.

Última corrida completa verde anterior a la edición visual concurrente siguiente:
`cargo test --workspace` pasó 1229 pruebas,
con 14 ignoradas, en 12 suites. `cargo fmt --all -- --check` y `git diff --check`
pasaron; Clippy terminó sin errores y con 46 advertencias de estilo existentes
en la app. Estas cifras incluyen trabajo visual concurrente y no se atribuyen
por completo a goals; el contrato tiene seis tests core, tres tests del adaptador de herramientas,
cuatro de runner nativo, tres de worker hijo, dos del worker de Resume/catálogo
y uno de startup desktop, más tres de editor/acciones/pointer y dos de
serialización/límites de contexto, más dos helpers de subproceso
ignorados que sus pruebas padre ejecutan explícitamente.

Verificación posterior del selector de objetivo: 73 pruebas nativas de la app
pasaron. La última corrida workspace durante edición visual compartida falló
en `disconnected_provider_shows_one_connection_action_instead_of_models`
(514 pasaron, 1 falló, 2 ignoradas en la app). Clippy terminó sin errores con 47
advertencias, incluida una de argumentos del tooltip nuevo; fmt se ejecutó pero
el check vio cambios concurrentes nuevos sin formato en Settings. Diff check
pasó. La corrida verde de 1229 indicada arriba es evidencia histórica, no el
estado integrado de esta última corrida.

Verificación final de formato de este bloque: después de aplicar fmt de nuevo,
`cargo fmt --all -- --check` y `git diff --check` pasaron sobre el worktree
observado. No se repitió la suite completa sólo por cambios de formato.

### Contexto después de compactación y reapertura

La aceptación HTTP local
`plan_build_uncertain_effect_reopens_resumes_and_replays_checkpoint` comprueba
objetivo y dos criterios completos en Plan/Build. Después de pausar el ledger,
Resume mantiene las instrucciones Active congeladas del turno anterior.
Compactación semántica y checkpoint resumen el historial; un siguiente Plan
explícito tras reabrir recibe el goal Paused completo desde el ledger. El único
efecto de archivo no se repite y el goal no se modifica automáticamente.
Pasó el 2026-09-29. No instala goal tools en este fixture, no registra evidencia
ni completa criterios, y el corte de ese caso se simula omitiendo el marcador
terminal. Los cortes reales y el cierre integral siguen siendo pruebas distintas.

Verificación posterior: la aceptación de cortes de `native_http_cycle` pasó
con 21 interrupciones reales (13 básicas y 8 de hooks en Unix). Incluye
SystemInstructionsCommitted y ModelContextCommitted; el servidor comprueba el
objetivo y criterio completos en cada petición de inferencia, y tras checkpoint
y reapertura se verifican dos snapshots exactos Plan/Build, ledger revision 0
y ningún criterio completado artificialmente. El efecto sigue siendo único.
No instala goal tools en este fixture ni demuestra el ciclo autónomo o UI integral.
La corrida workspace vuelve a fallar en
`disconnected_provider_shows_one_connection_action_instead_of_models`
(514 passed, 1 failed, 2 ignored de app); el trabajo visual sigue concurrente.

La suite completa de openide-harness pasó 414 pruebas, con 11 ignoradas,
en 5 suites. Después se añadió la solicitud del siguiente Plan explícito tras
checkpoint/reapertura en cada corte: la suite native_http_cycle pasó 4 pruebas,
con 2 helpers ignorados en la enumeración normal (el padre los invoca). Ese Plan
recibe por HTTP el goal completo y el historial resumido; ledger y efecto quedan
sin nuevas modificaciones. Workspace sigue detenido por la prueba visual indicada.
Clippy terminó sin errores, con 47 advertencias; fmt/diff check pasaron.

### Desktop receipt inspection during uncertain recovery

ResumeWorker keeps the child-scoped goal adapter separately from prepared MCP
tools and inspects only the two native mutation names when ResumeEngine pauses
with an uncertain result. `GoalReceiptInspection` distinguishes Found, NotFound
and Failed. Found contains goal ID, operation ID, committed/current revisions;
lookup failure never becomes absence. The existing verification card presents
these fields in a scroll area and retains the original arguments, executor error
and hooks. It does not prefill or confirm a result, replay the tool, synthesize
ToolFinished, or count semantic evidence as independently verified.

`native_goal_receipt_recovery_reports_exact_state_without_replaying_or_publishing_result`
passed with real goal mutation/store reopen and the three inspection states.
The completed evidence turn is a journal fixture, not model inference. While
waiting, journal events and ledger bytes are unchanged; cancelling absence/corruption
retains those bytes. For a found receipt, explicit manually supplied output records
one ToolFinished and completes the turn with the local model without replaying the
mutation; the ledger bytes remain unchanged.
the lease remains held during inspection and is released when the worker ends.
`native_goal_receipt_details_preserve_manual_verification_controls` passed with
all three descriptions, revision differences, scroll and output/cancel/confirm
controls at 420/900 px. Confirm remains unavailable until manual text is supplied.
Real desktop process cuts and exact reconstruction of the original tool result
are still pending.

Verificación de integración de recibos: 13 pruebas de ResumeWorker pasaron;
las dos pruebas nuevas de recibo/controles UI también pasaron. La continuación
del caso guardado usa salida escrita explícitamente por el verificador manual,
registra un único ToolFinished, completa con modelo local y conserva el ledger.
Workspace sigue fallando en
`disconnected_provider_shows_one_connection_action_instead_of_models`
(516 passed, 1 failed, 2 ignored de app); no se atribuye esa corrida a aceptación
completa. Clippy terminó sin errores, con 48 advertencias; fmt/diff check pasaron.
El cierre original de evidencia del fixture es core, sin inferencia; la recuperación
desktop y su siguiente llamada al modelo se ejecutan realmente. Faltan cortes
reales desktop/UI y proveedores externos para esta ruta.

### Maintenance report and retry in the Goal editor

The Goal editor exposes the last NativeGoalRecoveryReport separately from the
selected goal. It includes full busy session IDs, full diagnostic session/error
strings and the count of removed temporaries, in native_goal_recovery_details.
Changing selection or opening a draft preserves this global report.
The explicit native_goal_maintenance_retry action is available only while the
editor is open and execution/recovery is stopped. It calls recover_idle_sessions,
updates the report and refreshes session warnings; the domain store retains its
per-session native lease/writer-lock, corruption and filename/type checks.
It does not repair corrupt ledgers or manufacture goal evidence.

The startup acceptance has been expanded to retry while busy/corrupt, reject
action injection during execution/outside the editor, preserve the report across
selection changes, then clean two owned temporaries after releasing/repairing
the fixtures without changing journal or ledger bytes. A UI test checks full
long diagnostics, native scroll, pointer retry and disabled execution state at
420/900 px. These expanded tests passed in the 15-test desktop goal selection. Initial
attempts stopped on concurrent TooltipSection::IconRow and icon renderer omissions;
the visual changes were completed before the successful run.
