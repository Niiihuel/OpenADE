# CLI adapters

`CliRunner` runs a configured executable in the selected workspace and emits
`AgentEvent`s for output, completion, failure, and cancellation. Profiles can
send prompts through stdin or one `{prompt}` argument placeholder. No shell is
used to expand a prompt. On Windows, argument prompts through batch or shell
executables are rejected; use a native executable or stdin instead.

The built-in profiles are Codex (`codex exec -`), Claude (`claude -p`), Gemini
(noninteractive stdin), and ZCode (`zcode --prompt ... --mode build`). ZCode's
headless default is `yolo`; the built-in explicitly selects `build`, matching
its interactive default. Permissions and approval behavior remain controlled
by the external CLI. A custom profile can choose different CLI flags or add a
new provider.

The CLI is looked up on `PATH` at run time. Authentication and provider
settings remain in each CLI's local configuration or environment.

## Codex model and effort

The Codex profile can select a model and reasoning effort per run:

```rust
let profile = codex_profile
    .with_model("gpt-6-sol")?
    .with_reasoning_effort("high")?;
let handle = CliRunner::start(profile, request);
```

`with_model` passes `--model=<slug>`. `with_reasoning_effort` passes the Codex
configuration override `--config model_reasoning_effort="high"` as two process
arguments. Neither value is substituted into the prompt or run through a
shell. Omit either builder method to inherit the user's Codex configuration.
The other CLI profiles reject these options until their own flags are
implemented and verified.

`codex_known_models()` and `codex_supported_efforts(model)` expose the public
GPT-6 and GPT-5.6 entries observed in the installed CLI's bundled catalog for
building a selector. A newer Codex release may change availability, so Codex
remains authoritative at run time. Unknown or custom models can still be used;
their effort compatibility is checked by Codex.
