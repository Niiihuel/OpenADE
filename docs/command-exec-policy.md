# Command argv rules

The native harness can apply host supplied prefix rules to the exact `command` and `args` in a `run_command` or `start_command` tool call. The rules are an additional permission gate. They do not replace tool permissions or the command sandbox.

The implementation follows the literal and union prefix model described in [Codex Rules](https://learn.chatgpt.com/docs/agent-configuration/rules) and uses `references/codex/codex-rs/execpolicy` as an architectural reference. It is a smaller, original Rust policy, without the Codex Starlark loader or shell script parser.

## Host API

The host constructs validated rules and injects them before a native turn:

```rust
use openide_harness::{
    ExecDecision, ExecPatternToken, ExecPolicy, ExecRule, PermissionPolicy,
};

let rules = ExecPolicy::new(vec![ExecRule::new(
    vec![
        ExecPatternToken::Literal("git".into()),
        ExecPatternToken::AnyOf(vec!["push".into(), "reset".into()]),
    ],
    ExecDecision::Deny,
    Some("Do not push or reset through the agent".into()),
)?])?;
let permission_policy = PermissionPolicy::new().with_exec_policy(rules);
```

`ExecRule::new` and `ExecPolicy::new` enforce nonempty prefixes and bounded token, rule, and justification sizes. Rules currently have no editable Settings file or UI. The application must explicitly supply a validated `PermissionPolicy` to both `AgentRunner` and `ResumeEngine`; an absent policy has no prefix rules. A future Settings adapter can store rule patterns, decisions, and justifications, then rebuild them through these constructors. Deserializing an `ExecRule` alone is not a validation step.

The tool call's first argv token is `command`; subsequent tokens are `args`. A rule matches a prefix. A union matches any listed literal at that position. Matching decisions combine as `Deny > Ask > Allow`, irrespective of rule order. An `Allow` rule never lowers an existing host `Ask` or `Deny`, and it does not currently grant automatic execution. The current host mode remains authoritative. Malformed command arguments are denied.

For restrictive `Ask` and `Deny` rules, a program token such as `git` also matches an absolute path ending in `git.exe`, ignoring ASCII case. `Allow` matching remains exact. The harness does not resolve executable identity from `PATH`; a future automatic allowance would need that verification.

Shells and known interpreters, including `cmd`, PowerShell, `sh`, Python, Node, Ruby, Perl, PHP, Lua, Deno, and Bun, are opaque. With rules active, they require `Ask`; when any `Deny` rule exists, they are denied. `write_stdin` is likewise `Ask` or `Deny` because it can send commands to a live process without exposing a new argv. This policy does not parse compound shell commands, executable aliases, scripts, build systems, or arbitrary binaries that can launch other processes. The wrapper list cannot prove that an unmatched executable is harmless; use host permissions and OS isolation for broader control.

## Durable recovery

Before the first model request of a native turn, the runner syncs `ExecPolicyCommitted { version, digest }` after the tool catalog and command hook markers. The digest binds the complete effective `PermissionPolicy`: default mode, per-tool modes, timeout, and ordered argv rules. Recovery compares the current policy with this snapshot before continuing a model request, permission request, or tool call. A changed policy blocks automatic recovery. Older turns that advertised `run_command` or `start_command` but lack this marker cannot resume once a model request began. A newly admitted turn can write the marker before its first model request. Manual verification of an already uncertain tool outcome remains available without rerunning the effect.

The marker is an append-only event. Existing session and checkpoint schemas are not rewritten. The journal reader accepts older turns, but the recovery gate above prevents them from executing a command with unverified policy.
