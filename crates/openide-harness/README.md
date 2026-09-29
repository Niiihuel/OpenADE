# Native harness tool permissions

## Native model capabilities and reasoning effort

All provider adapters implement `ModelBackend`. `OpenAiCompatibleBackend` is
the Chat Completions wire adapter; its model identifier does not imply tool or
effort support. The caller supplies a `ModelRequestCapabilities` contract for
the selected provider, model, and route before running the agent:

```rust
use openide_harness::{ModelRequestCapabilities, OpenAiCompatibleBackend};

let capabilities = ModelRequestCapabilities {
    tool_calls: true,
    reasoning_efforts: vec!["low".into(), "high".into()],
    tool_call_reasoning_efforts: None,
    default_tool_call_effort: None,
};
let backend = OpenAiCompatibleBackend::new(endpoint, api_key, model_id)
    .with_capabilities(capabilities)?
    .with_reasoning_effort("high")?;
```

The effort values come from provider metadata, a locally reviewed record, or
an explicit custom connection setting. Unknown capabilities start with tools
disabled and no adjustable effort. A custom route may explicitly set
`tool_calls: true` while leaving effort lists empty; the provider then accepts
or rejects the tool request. When `tool_call_reasoning_efforts` is set, it is
the subset allowed with tools. `default_tool_call_effort` is sent only on tool
requests without an explicit choice. Leaving both effort settings empty omits
`reasoning_effort`, allowing the provider default.

Provider-specific fields on a Chat Completions route can be supplied with
`.with_request_field("thinking", serde_json::json!({"type":"disabled"}))?`.
The adapter rejects attempts to replace protocol-owned fields and records
these options in the request profile used for durable resume checks.

The Responses adapter streams text and function calls and can return response
IDs, token usage, and original output items through its detailed API. The
native runner commits the response ID, usage, assistant message, tool calls,
and original provider output items in `ModelResponseCommitted` before any
tool effect. Replay retains the provider items for later Responses requests.
The stored response ID is metadata; recovery starts a fresh model request
rather than relying on a server-side `previous_response_id` chain.

Native Plan mode advertises only `read_file`, `list_files`, and `search_files`.
The runner rejects a model call outside the catalog it advertised, even if
the model emits one anyway. Build mode advertises all enabled tools and uses
the existing approval policy.

See the [OpenAI reasoning guide](https://developers.openai.com/api/docs/guides/reasoning),
[deployment checklist](https://developers.openai.com/api/docs/guides/deployment-checklist),
and [Codex configuration reference](https://developers.openai.com/codex/config-reference)
for provider semantics.

`LocalTools::new()` advertises and runs `read_file`, `list_files`, and
`search_files`. It denies `write_file` and `run_command` by default. Configure
each tool independently before constructing an `AgentRunner`:

```rust
use openide_harness::{LocalTool, LocalTools};

let tools = LocalTools::new()
    .with_tool_permission(LocalTool::WriteFile, true)
    .with_tool_permission(LocalTool::RunCommand, false);
```

`ToolPermissions` can also be passed to `with_permissions`. The policy is
checked when tools are advertised to the model **and again when a call executes**;
a model call naming a disabled tool receives an error result. The policy is
fixed for that `LocalTools` instance. The existing
`with_write_access(true)` shorthand still enables both `write_file` and
`run_command` for callers that use the old toggle.

## Interactive permission gate

The runner can pause before a particular tool call and ask the owner of its
`AgentRunHandle` for a decision. For example:

```rust
use openide_harness::{
    AgentRunner, LocalTools, PermissionChoice, PermissionMode, PermissionPolicy,
};
use std::sync::Arc;

let tools = Arc::new(LocalTools::new().with_write_access(true));
let policy = PermissionPolicy::new()
    .with_tool("write_file", PermissionMode::Ask)
    .with_tool("run_command", PermissionMode::Ask);
let runner = AgentRunner::new(model, tools).with_permission_policy(policy);
let handle = runner.start(request);
// When handle.events yields PermissionRequested { request_id, tool_call },
// show the exact arguments and call:
// handle.resolve_permission(&request_id, PermissionChoice::Allow);
```

`PermissionPolicy::default()` is `Allow` for tools already enabled by the
executor, preserving existing behavior. A rule may also use `Deny`, which
returns a tool error without executing it. `Ask` emits `PermissionRequested`
with the complete tool call, then `PermissionResolved` before execution. Only
the matching request ID can be answered, and only once. A denied or timed-out
request returns a tool error to the model. Cancelling the run while it waits
emits a cancelled resolution and ends the turn without executing that tool.
The default wait is 60 seconds; set `PermissionPolicy::with_timeout` to change
it. An unhandled request therefore fails closed after the timeout.

The underlying `LocalTools` permission must still expose an approvable tool to
the model. `with_write_access(true)` in the example does that; the runner's
`Ask` gate then blocks execution until approval. This API has no persistent
allow rule or project rule matcher. Permission events are serializable. For
native sessions, `AgentRunner::with_event_journal` makes
the worker persist each event before forwarding it to the window or taking
the next model/tool step. The window admits the turn durably before starting
the worker and holds an OS session lease until it exits; replay is based on
that ordered journal, not on UI delivery. The permission request is synced
before the UI displays it; the window syncs the separate approval sidecar
before sending the live permission answer. A failed sidecar write denies the
request.

File tools accept relative workspace paths. They reject absolute paths,
parent traversal, and paths that resolve outside the workspace. `write_file`
also rejects a symlink as its final target and replaces files through a
temporary file. Reads and writes are limited to 256 KiB per file; directory
listings return at most 200 entries. Search scans at most 5,000 UTF-8 files
of at most 1 MiB each and returns at most 200 matches.
Path checks do not provide an operating system sandbox against concurrent
changes to workspace directories or symlinks.

`run_command` is a separate permission. It runs an argument array with the
workspace as its working directory, a timeout between 1 and 120 seconds,
and at most 64 KiB each of stdout and stderr retained. Host settings choose
`HostUnrestricted` or, on Windows, the MXC `WorkspaceWriteOffline` sandbox.
The unrestricted mode grants the user's filesystem and network rights, but
passes only an explicit bootstrap environment rather than ambient secrets.
These permissions apply only to this native `LocalTools` executor; external
CLI profiles have their own behavior.

`PersistentCommandManager` can be attached with
`LocalTools::with_persistent_commands` for sessions that need interactive
stdin and repeated output polling. It adds `start_command`, `poll_command`,
`write_stdin`, and `terminate_command` only with `run_command` enabled and
`HostUnrestricted` isolation. The host must use an approval policy that asks
for start, stdin writes, and termination; the desktop's agent tool settings
provide those rules. The desktop retains a manager per native parent session
across turns and revokes it when command access is withdrawn. See
[`docs/persistent-commands.md`](../../docs/persistent-commands.md) for bounds
and remaining isolation work.

`PermissionPolicy::with_exec_policy` evaluates bounded argv prefix rules for
`run_command` and `start_command`, and commits their policy digest before a
native model request. See
[`docs/command-exec-policy.md`](../../docs/command-exec-policy.md).

## Model context limit

`AgentRunner` preserves history by default. The older character-based
`ContextPolicy` is opt-in through `with_context_policy`; it can omit old turns
and therefore should not be used for resumable native sessions:

```rust
use openide_harness::ContextPolicy;

let policy = ContextPolicy {
    max_request_chars: 80_000,
    keep_recent_tool_results: 2,
};
let runner = runner.with_context_policy(policy);
```

With the opt-in `ContextPolicy`, when a request exceeds its character budget,
the runner removes the oldest historical
user turn together with its assistant tool calls and matching tool responses.
It then replaces older tool result bodies from the current turn with a short
omission marker, retaining the call and response IDs. The current user prompt
and system messages remain intact. If those messages and the protected recent
results still exceed the budget, the turn fails with a limit error. This
changes only the model request; streamed events and the stored session history
retain their original tool output.

The character estimate is not a provider token limit. A separate opt-in
`TokenBudgetPolicy` counts messages, opaque provider items, and tool schemas,
then calibrates the next request with reported input-token usage. Native runs
configure `with_auto_compaction` with a session store, checkpoint store, and
model/provider compatibility. At the threshold, the worker summarizes a
replay-validated prefix of **completed** turns, writes a checksummed
checkpoint candidate, commits its marker to the session journal, rebuilds
the current request, and reassesses the budget before continuing. The active
user prompt, committed response, and tool batch stay intact. If no safe
completed prefix exists, or the compacted request still exceeds the limit,
the run fails with a limit error rather than dropping active work. Callers
without a native journal and auto-compaction configuration also receive a
limit error.

`ResumeEngine` replays a native journal after interruption under the same
session lease. It verifies the model and the persisted advertised tool-catalog
digest before continuing. Before the first model request, the worker also
commits a v2 digest of provider, endpoint, model, mode, workspace,
catalog, complete system prompt (including active skills), and the backend's
request profile (wire API, reasoning effort, and protocol flags). The source
values are not written to the journal. Native runs require a backend that
declares a stable profile. Recovery rejects changed inputs and in-flight v1
turns because their marker does not cover the request profile. V1 journals
remain readable and an aborted tool batch can still be discarded manually.
A model request in flight can be retried; a model response already committed
is never regenerated. A pending permission is shown again with its original
request ID. A `ToolStarted` without a matching
result remains uncertain and requires an independently verified result; the
tool is never run again automatically. The desktop worker sends persisted
events to the UI in sequence and keeps MCP clients inside its Tokio runtime.
`record_verified_tool_outcome` records that human-verified result under the
session lease without needing the current provider, model backend, catalog,
or credentials. It validates the call ID and result bound, simulates replay,
then syncs `ToolFinished`; it never invokes the tool.
Pending MCP calls whose server identity cannot be verified are blocked before
server startup. Recovery can fail closed if the model, catalog, checkpoint
compatibility, or available completed history has changed.
The stdio transport reads bounded server notifications both between and during
requests. `notifications/tools/list_changed` permanently invalidates that
server's frozen tool catalog for the current turn, including if notification
receivers lag; the next turn performs fresh discovery. A notification never
retries an in-flight `tools/call`. Requests made before the notification is
observed still follow the journal's uncertain-effect recovery rules.

After a failed or cancelled turn leaves an unstarted tool batch, the user can
explicitly discard its remaining calls through `discard_aborted_tool_batch`
under the session lease. This syncs `ToolBatchDiscarded`, removes that entire
aborted turn from future model history, and leaves all original journal events
available for inspection. The discard route needs no model credentials or
current provider settings. A started tool with an uncertain effect cannot be
discarded this way; it still needs an independently verified outcome.

## Local skill discovery

`discover_skills` reads `SKILL.md` only from direct child directories of
project or user roots explicitly passed as `SkillRoot::project(path)` or
`SkillRoot::user(path)`. It resolves the root, skill directory, and document
to canonical paths and rejects a child or symlink that leaves its root.
Each document must be UTF-8, at most 128 KiB, and have YAML frontmatter with
a nonempty `description`; `name` defaults to the directory name. Discovery
returns the instructions, metadata, origin, canonical paths, and SHA-256
content hash in deterministic order. Duplicate names, including names that
differ only by case, fail the entire discovery. Root, entry, and total skill
counts are bounded.

This API only discovers and reads documents. It does not activate a skill,
follow references in the document, execute scripts, or insert instructions
into a model request. Callers must make activation explicit. Canonical-path
validation does not protect against a separate process replacing directories
or symlinks during discovery.
The desktop Marketplace has a separate enabled-skill path to the native
system prompt; this discovery API is not wired into that path yet.

## Native subagent session foundation

`SubagentCoordinator` creates a child as a normal native `SessionStore`
session while the caller holds the parent's OS lease. The child's parent ID
and depth are committed in its initial session metadata. Creation requires a
stable `spawn_id` and `SubagentSpec` (prompt, model, mode, and execution-context
digest). Retrying the same pair after a crash returns the same child session;
reusing the ID with a changed request is rejected. A separate registry
under an OS file lock tracks `Ready`, `Running`, `Interrupted`, and terminal
states across store handles and process restarts. The coordinator enforces
configured depth, active children per parent, and global concurrent child
limits. A child can create grandchildren only while its own run lease is held.

Dropping a child run lease without recording a terminal outcome leaves it
interrupted on the next inspection; restarting it requires an explicit
`resume_child` call. Cancellation is persisted for a child and its descendants
before workers observe it. Idle children close as cancelled immediately;
running workers must poll the request, stop cooperatively, and record their
outcome with `finish_child`. Reopening and listing children reads the same
durable metadata and registry. Session journals remain the authority for any
model or tool effects during a future resume.
If a worker confirms completion after cancellation was requested, its
`Completed` outcome wins and `cancellation_requested` stays visible, preserving
the race instead of relabeling confirmed work. If the worker disappears first,
reopening closes the cancelled child as `Cancelled`.

The coordinator itself stores lifecycle state. `SubagentWorker` now runs a
child's journaled native turn with a host-selected tool policy and polls the
cancellation flag, but cannot forcibly terminate an uncooperative model or
tool. An interrupted child still needs explicit journal recovery. The desktop
app now constructs this worker and tool surface for interactive Build turns.
Codex-style agent control remains pending: `send_input`, event notifications,
close, and fork behavior need worker and UI
integration with tests before this can claim feature parity.

`SubagentToolExecutor` now provides an opt-in Build tool surface with
`spawn_agent`, `wait_agent`, and `cancel_agent`. It wraps a base executor and
must be paired with a `SubagentWorker` constructed with a separate host-selected
child tool policy, never this parent-scoped wrapper. The desktop integration
currently gives children read-only local tools until child permission requests
can be routed to the UI.
The stable spawn ID is derived from the journaled tool-call ID; retrying that
ID uses the existing child and never dispatches it twice. Each parent has a
configurable total child-task cap. Spawn returns the child session ID before
the model turn finishes, and wait reads coordinator state and the terminal
journal without executing the child. `child_for_call_id` lets recovery inspect
an already created child when the parent tool result was lost, without
dispatching it. Cancellation is durable and cooperative.
The wrapper's `shutdown` or `Drop` closes this turn's tool surface and
releases its parent lease reference;
the child continues under an opaque control issued while the parent held that
lease. A later parent turn can inspect or cancel the same child with a new
wrapper and lease. The interactive desktop Build path now exposes these tools.
`send_input`, fork, notifications, and full Codex agent-control parity remain
pending.
