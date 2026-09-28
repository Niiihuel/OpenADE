# Native harness tool permissions

## Native model reasoning effort

`OpenAiCompatibleBackend` uses the Chat Completions wire format. To request a
supported effort explicitly, call `.with_reasoning_effort("high")?` before
passing the backend to `AgentRunner`. Without that call, the backend does not
send `reasoning_effort` and the provider chooses its default. The builder
rejects ordinary or unknown models rather than silently ignoring the setting.

`native_agent_supported_efforts(model)` returns effort choices safe for this
agent's Chat Completions requests, which include function tools. GPT-6 Astra
requires Responses for function calling; GPT-6 Sol and Luna can use Chat
Completions function tools only with effort `none`. For the full GPT-6 reasoning
range and agentic tool use, use Codex CLI until this harness supports Responses.

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
allow rule or project rule matcher. Permission events are serializable. The
current window writes a request to the session log before displaying approval
controls, and records the user's decision in a synced, idempotent
`SessionStore` journal before calling `resolve_permission`; a failed write
causes denial. The runner itself does not own that journal or a command
admission ACK, so recovery and state reconciliation remain separate from this
live interaction gate.

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
and at most 64 KiB each of stdout and stderr retained. It removes common
model API key environment variables from the child. **It is not an operating
system sandbox:** an enabled command can access files outside the workspace
with the user's account permissions. These permissions apply only to this
native `LocalTools` executor; external CLI profiles have their own behavior.

## Model context limit

`AgentRunner` applies `ContextPolicy::default()` before every model call. The
default budget is 120,000 approximate characters of message content and tool
call metadata; it retains the three most recent tool result bodies in the
current turn. Configure it with `with_context_policy`:

```rust
use openide_harness::ContextPolicy;

let policy = ContextPolicy {
    max_request_chars: 80_000,
    keep_recent_tool_results: 2,
};
let runner = runner.with_context_policy(policy);
```

When a request exceeds the budget, the runner removes the oldest historical
user turn together with its assistant tool calls and matching tool responses.
It then replaces older tool result bodies from the current turn with a short
omission marker, retaining the call and response IDs. The current user prompt
and system messages remain intact. If those messages and the protected recent
results still exceed the budget, the turn fails with a limit error. This
changes only the model request; streamed events and the stored session history
retain their original tool output.

The estimate counts Unicode characters, not provider tokens, and excludes tool
schemas and transport overhead. It is therefore a practical growth bound for
messages, not a guarantee that a provider will accept the request. This policy
does not generate a semantic summary, use provider usage data, or persist a
compacted history. Those are separate follow-on capabilities.
