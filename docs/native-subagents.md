# Native subagents: current contract

The native subagent path is opt-in. `SubagentToolExecutor` adds four Build
tools to a base `ToolExecutor`: `spawn_agent`, `send_input`, `wait_agent`, and
`cancel_agent`.
The child `SubagentWorker` receives a separate host-selected base executor,
never its parent's wrapper. The desktop app attaches a wrapper for interactive
Build turns. `SubagentWorker::with_nested_subagents` opts a child into a new
wrapper bound to its **own** native lease. Plan mode does not advertise or
execute coordination tools.

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
An input sent after Completed starts a new Build turn in the same child
session. Claiming the next input changes the registry to `Running` while the
child OS lease is held; the new `TurnAdmitted` event uses the input ID and
prompt. The model receives the prior child conversation rebuilt from replay.
`wait_agent` observes the current mailbox state and waits for pending inputs
before returning Completed. A failed or cancelled child closes the mailbox;
`cancel_agent` skips pending inputs and closes a previously completed child to
new input.

A process crash after enqueue leaves a pending entry that an exact retry of
`send_input` or a later `wait_agent` can schedule. A crash after claim marks
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
  retries `send_input` or calls `wait_agent`; there is no startup dispatcher.
- Cancellation is cooperative. A model or tool that ignores cancellation can
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
