# Native command hooks

`CommandHooks` and `run_command_hook` provide ordered, bounded `PreToolUse` and
`PostToolUse` commands configured by a trusted host. `AgentRunner` and
`ResumeEngine` execute them during leased native turns. Settings → Hooks in
OpenIDE manages the trusted host configuration for interactive native turns.
It is stored atomically in `native-command-hooks.json` under the app data root.
The app reloads and validates it before admitting a new native turn and before
starting recovery. Invalid configuration blocks the run instead of silently
starting without hooks. CLI backends and scheduled automations have separate
policies and do not use these hooks.

## Contract

- A hook binds to one exact tool name and phase. The executable and working
  directory must be existing absolute paths. The hook is launched with exact
  arguments, never through an implicit shell.
- The Settings form accepts only host-authored entries. It validates the
  executable, working directory, arguments, timeout, output limit, and unique
  IDs before saving; tool/model output cannot activate or edit a hook.
- The runner writes one JSON `HookInvocation` to stdin. Stdout must be exactly
  one JSON `HookResponse`, such as
  `{"decision":"allow"}` or
  `{"decision":"block","reason":"Policy reason"}`. A pre-tool block needs a
  nonempty reason. Post-tool hooks cannot block a tool that already finished.
- Hook input is limited to 256 KiB, each stdout/stderr pipe to the configured
  limit (256 bytes through 64 KiB), and response text fields to 8 KiB. The
  timeout is 1 through 30,000 ms. The process is killed on timeout,
  cancellation, I/O failure, or output overflow. A process-tree guard contains
  children in a Windows Job Object or Unix process group.
- Ambient environment is cleared except for a small process bootstrap and
  locale allowlist. Hooks run with the host user's filesystem and network
  access. This runner does not provide a sandbox. An explicitly configured
  interpreter and script remain trusted host code.
- `NotStarted` means no hook process was spawned. `OutcomeUncertain` means it
  was spawned and might have changed external state, even when the direct
  process was subsequently killed. `Completed` includes the parsed decision.

## Durable ordering and recovery

The runner syncs `HooksCommitted` before the first model request, including
when the policy is empty. Its digest binds hook order, IDs, phases, exact tool
names, arguments, limits, canonical launch paths, executable and existing file
argument bytes, and the inherited bootstrap environment. Recovery requires the
same digest before a pending `ToolReady` call or permission decision can cause
an effect. A legacy turn without this marker cannot auto-run a pending tool.

For each allowed call, the runner syncs `ToolStarted` and then `HookStarted`
before launching each hook. It syncs `HookFinished` before continuing. A
blocking Pre hook produces a `ToolFinished` error without running the tool.
Post hooks receive the observed tool result, which is committed as
`ToolFinished` only after every Post hook succeeds. Post hooks cannot block
retrospectively. Hook context is appended to the eventual bounded tool result.
Hooks on the three built-in reads make them serial in Build and remove them
from Plan's advertised read-only catalog.

A crash or failure after `ToolStarted`, including a hook start without a
finish, leaves `ToolOutcomeUncertain`. Recovery does not reexecute that hook or
tool. Manual outcome verification can close the call. A crash between
`ToolStarted` and the first hook start is conservatively uncertain too.

The path check has a validation-to-spawn race if a trusted configured file is
replaced concurrently. The snapshot does not attest dynamic libraries, files
opened by a hook later, or remote services it calls. On Unix, a descendant
can escape the process group by creating a new session. Hooks still have host
filesystem and network access; process-tree cleanup is not a sandbox.

The design follows the event and bounded command-hook concepts in the
[Codex hooks source](../references/codex/codex-rs/hooks/src/engine/command_runner.rs),
with original OpenIDE implementation and protocol.
