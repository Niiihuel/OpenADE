//! Bounded command hooks. This module does not schedule hooks or journal their effects.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::project_instructions::ACTIVATION_HOOK_PREFIX;

#[cfg(windows)]
use crate::process_tree;
use crate::process_tree::ProcessTreeGuard;

pub const MAX_HOOK_TIMEOUT_MS: u64 = 30_000;
pub const MAX_HOOK_OUTPUT_BYTES: usize = 64 * 1024;
pub const MAX_HOOK_INPUT_BYTES: usize = 256 * 1024;
const MAX_HOOK_ARGS: usize = 64;
const MAX_HOOK_ARG_BYTES: usize = 4 * 1024;
const MAX_HOOK_TEXT_BYTES: usize = 8 * 1024;
const MAX_HOOKS: usize = 32;
const MAX_IDENTITY_FILE_BYTES: u64 = 64 * 1024 * 1024;
const BOOTSTRAP_ENV_KEYS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "COMSPEC",
    "HOME",
    "USERPROFILE",
    "TEMP",
    "TMP",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "TZ",
];
use openide_core::HOOK_CONFIG_IDENTITY_VERSION;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command hook is attached to exactly one phase and one tool name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPhase {
    PreToolUse,
    PostToolUse,
}

/// Trusted host configuration. `validate` is also called immediately before spawn.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandHookConfig {
    pub id: String,
    pub phase: HookPhase,
    pub tool_name: String,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

impl CommandHookConfig {
    pub fn validate(&self) -> Result<(), HookValidationError> {
        if self.id.trim().is_empty() || self.id.len() > 128 {
            return Err(HookValidationError::InvalidId);
        }
        if self.id.starts_with(ACTIVATION_HOOK_PREFIX) {
            return Err(HookValidationError::ReservedId);
        }
        if self.tool_name.trim().is_empty() || self.tool_name.len() > 128 {
            return Err(HookValidationError::InvalidToolName);
        }
        if self.args.len() > MAX_HOOK_ARGS
            || self
                .args
                .iter()
                .any(|arg| arg.len() > MAX_HOOK_ARG_BYTES || arg.as_bytes().contains(&0))
        {
            return Err(HookValidationError::InvalidArguments);
        }
        if !(1..=MAX_HOOK_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(HookValidationError::InvalidTimeout);
        }
        if !(256..=MAX_HOOK_OUTPUT_BYTES).contains(&self.max_output_bytes) {
            return Err(HookValidationError::InvalidOutputLimit);
        }
        validate_existing_path(&self.executable, true)?;
        validate_existing_path(&self.cwd, false)?;
        Ok(())
    }
}

fn validate_existing_path(path: &Path, file: bool) -> Result<(), HookValidationError> {
    if !path.is_absolute() {
        return Err(HookValidationError::RelativePath);
    }
    let metadata = std::fs::metadata(path).map_err(|_| HookValidationError::MissingPath)?;
    if (file && !metadata.is_file()) || (!file && !metadata.is_dir()) {
        return Err(HookValidationError::WrongPathKind);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HookValidationError {
    #[error("no more than 32 hooks may be configured")]
    TooManyHooks,
    #[error("hook IDs must be unique")]
    DuplicateId,
    #[error("hook identity file is too large or unreadable")]
    UnverifiableIdentity,
    #[error("hook id must contain 1..=128 bytes")]
    InvalidId,
    #[error("hook id uses a reserved internal prefix")]
    ReservedId,
    #[error("hook tool name must contain 1..=128 bytes")]
    InvalidToolName,
    #[error("hook has too many, oversized, or NUL-containing arguments")]
    InvalidArguments,
    #[error("hook timeout must be within 1..=30000 ms")]
    InvalidTimeout,
    #[error("hook output limit must be within 256..=65536 bytes")]
    InvalidOutputLimit,
    #[error("hook executable and working directory must be absolute")]
    RelativePath,
    #[error("hook executable or working directory does not exist")]
    MissingPath,
    #[error("hook executable must be a file and working directory must be a directory")]
    WrongPathKind,
}

/// Immutable, ordered host policy. Order is included in the durable digest.
#[derive(Clone, Debug, Default)]
pub struct CommandHooks {
    configs: Vec<CommandHookConfig>,
    /// Claude Code protocol hooks. Empty leaves the digest as it always was.
    compat: Vec<crate::CompatHookConfig>,
}

impl CommandHooks {
    pub fn new(configs: Vec<CommandHookConfig>) -> Result<Self, HookValidationError> {
        if configs.len() > MAX_HOOKS {
            return Err(HookValidationError::TooManyHooks);
        }
        let mut ids = HashSet::new();
        for config in &configs {
            config.validate()?;
            if !ids.insert(&config.id) {
                return Err(HookValidationError::DuplicateId);
            }
        }
        let hooks = Self {
            configs,
            compat: Vec::new(),
        };
        hooks.digest_hex()?;
        Ok(hooks)
    }

    /// Adds hooks that speak the Claude Code protocol. IDs are unique across
    /// both kinds and the same total limit applies.
    pub fn with_compat(
        mut self,
        compat: Vec<crate::CompatHookConfig>,
    ) -> Result<Self, HookValidationError> {
        if self.configs.len() + compat.len() > MAX_HOOKS {
            return Err(HookValidationError::TooManyHooks);
        }
        let mut ids: HashSet<&String> = self.configs.iter().map(|config| &config.id).collect();
        for hook in &compat {
            hook.command.validate()?;
            if !ids.insert(&hook.command.id) {
                return Err(HookValidationError::DuplicateId);
            }
        }
        self.compat = compat;
        self.digest_hex()?;
        Ok(self)
    }

    /// Every hook for one tool call, native ones first. The flag marks a
    /// Claude Code protocol hook.
    // shortcut: read tools on the parallel path never reach this, so a compat hook on `Read` only fires for serial calls.
    pub(crate) fn tool_hooks<'a>(
        &'a self,
        phase: HookPhase,
        tool_name: &'a str,
    ) -> Vec<(&'a CommandHookConfig, bool)> {
        let event = match phase {
            HookPhase::PreToolUse => crate::CompatEvent::PreToolUse,
            HookPhase::PostToolUse => crate::CompatEvent::PostToolUse,
        };
        let (claude_name, _) = crate::claude_tool(tool_name, &Value::Null);
        self.for_call(phase, tool_name)
            .map(|config| (config, false))
            .chain(
                self.compat
                    .iter()
                    .filter(|hook| hook.event == event && hook.matches(&claude_name))
                    .map(|hook| (&hook.command, true)),
            )
            .collect()
    }

    /// Hooks for a non-tool event, matched against its subject.
    pub fn compat_for_event<'a>(
        &'a self,
        event: crate::CompatEvent,
        subject: &'a str,
    ) -> impl Iterator<Item = &'a crate::CompatHookConfig> + 'a {
        self.compat
            .iter()
            .filter(move |hook| hook.event == event && hook.matches(subject))
    }

    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    pub fn for_call<'a>(
        &'a self,
        phase: HookPhase,
        tool_name: &'a str,
    ) -> impl Iterator<Item = &'a CommandHookConfig> + 'a {
        self.configs
            .iter()
            .filter(move |config| config.phase == phase && config.tool_name == tool_name)
    }

    pub fn has_hooks_for_tool(&self, tool_name: &str) -> bool {
        self.configs
            .iter()
            .any(|config| config.tool_name == tool_name)
    }

    /// Recomputes file identities on each call. Callers compare this value to
    /// the digest synced at turn start before launching any hook.
    pub fn digest_hex(&self) -> Result<String, HookValidationError> {
        let mut hasher = Sha256::new();
        hasher.update(b"openide-command-hooks\0");
        hasher.update(HOOK_CONFIG_IDENTITY_VERSION.to_be_bytes());
        for key in BOOTSTRAP_ENV_KEYS {
            hash_text(&mut hasher, key);
            match std::env::var_os(key) {
                Some(value) => {
                    hasher.update([1]);
                    hash_text(
                        &mut hasher,
                        value
                            .to_str()
                            .ok_or(HookValidationError::UnverifiableIdentity)?,
                    );
                }
                None => hasher.update([0]),
            }
        }
        hash_len(&mut hasher, self.configs.len());
        for config in &self.configs {
            hash_config(&mut hasher, config)?;
        }
        if !self.compat.is_empty() {
            hasher.update(b"compat\0");
            hash_len(&mut hasher, self.compat.len());
            for hook in &self.compat {
                hash_text(&mut hasher, hook.event.name());
                hash_text(&mut hasher, &hook.matcher);
                hash_config(&mut hasher, &hook.command)?;
                // Hashed only when present, so hooks saved before this
                // field existed keep the identity they had.
                if !hook.env.is_empty() {
                    hash_len(&mut hasher, hook.env.len());
                    for (key, value) in &hook.env {
                        hash_text(&mut hasher, key);
                        hash_text(&mut hasher, value);
                    }
                }
            }
        }
        Ok(format!("{:x}", hasher.finalize()))
    }
}

/// Runs a tool hook with the protocol its configuration belongs to.
pub(crate) async fn run_tool_hook(
    hooks: &CommandHooks,
    config: &CommandHookConfig,
    invocation: &HookInvocation,
    cancel: &CancellationToken,
) -> HookRunResult {
    match hooks
        .compat
        .iter()
        .find(|hook| hook.command.id == config.id)
    {
        Some(hook) => crate::compat_hooks::run_compat_tool_hook(hook, invocation, cancel).await,
        None => run_command_hook(config, invocation, cancel).await,
    }
}

fn hash_config(hasher: &mut Sha256, config: &CommandHookConfig) -> Result<(), HookValidationError> {
    {
        {
            let hasher = &mut *hasher;
            config.validate()?;
            hash_text(hasher, &config.id);
            hash_text(
                hasher,
                match config.phase {
                    HookPhase::PreToolUse => "pre_tool_use",
                    HookPhase::PostToolUse => "post_tool_use",
                },
            );
            hash_text(hasher, &config.tool_name);
            hash_path(hasher, &config.executable, true)?;
            hash_path(hasher, &config.cwd, false)?;
            hasher.update(config.timeout_ms.to_be_bytes());
            hash_len(hasher, config.max_output_bytes);
            hash_len(hasher, config.args.len());
            for arg in &config.args {
                hash_text(hasher, arg);
                let path = Path::new(arg);
                let candidate = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    config.cwd.join(path)
                };
                if candidate.is_file() {
                    hash_path(hasher, &candidate, true)?;
                } else if candidate.is_dir() {
                    hash_path(hasher, &candidate, false)?;
                }
            }
        }
    }
    Ok(())
}

fn hash_len(hasher: &mut Sha256, len: usize) {
    hasher.update((len as u64).to_be_bytes());
}

fn hash_text(hasher: &mut Sha256, text: &str) {
    hash_len(hasher, text.len());
    hasher.update(text.as_bytes());
}

fn hash_path(hasher: &mut Sha256, path: &Path, file: bool) -> Result<(), HookValidationError> {
    let canonical = path
        .canonicalize()
        .map_err(|_| HookValidationError::UnverifiableIdentity)?;
    hash_text(
        hasher,
        canonical
            .to_str()
            .ok_or(HookValidationError::UnverifiableIdentity)?,
    );
    if file {
        let mut input =
            File::open(&canonical).map_err(|_| HookValidationError::UnverifiableIdentity)?;
        let len = input
            .metadata()
            .map_err(|_| HookValidationError::UnverifiableIdentity)?
            .len();
        if len > MAX_IDENTITY_FILE_BYTES {
            return Err(HookValidationError::UnverifiableIdentity);
        }
        hasher.update(len.to_be_bytes());
        let mut buffer = [0u8; 8192];
        let mut read_total = 0u64;
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|_| HookValidationError::UnverifiableIdentity)?;
            if count == 0 {
                break;
            }
            read_total += count as u64;
            if read_total > MAX_IDENTITY_FILE_BYTES {
                return Err(HookValidationError::UnverifiableIdentity);
            }
            hasher.update(&buffer[..count]);
        }
        if read_total != len || input.stream_position().ok() != Some(len) {
            return Err(HookValidationError::UnverifiableIdentity);
        }
    }
    Ok(())
}

/// The tool call ID must come from a committed model response. Post-tool input
/// is the observed executor result; ToolFinished follows the post hooks.
#[derive(Clone, Debug, Serialize)]
pub struct HookInvocation {
    pub phase: HookPhase,
    pub session_id: String,
    pub turn_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub tool_input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<Value>,
}

/// A human control operation has its own durable authority. It must not claim
/// a model turn or a tool call from a committed model response.
#[derive(Clone, Debug, Serialize)]
pub struct ManualCommandHookInvocation {
    pub version: u32,
    pub phase: HookPhase,
    pub session_id: String,
    pub operation_id: String,
    pub command_id: String,
    pub tool_name: String,
    pub tool_input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<Value>,
}

/// Distinguish this envelope from the model-facing hook protocol in JSON.
#[derive(Serialize)]
struct ManualCommandHookEnvelope<'a> {
    source: &'static str,
    #[serde(flatten)]
    invocation: &'a ManualCommandHookInvocation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookDecision {
    Allow,
    Block,
}

/// The entire stdout stream must be one JSON object of this shape.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HookResponse {
    pub decision: HookDecision,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub additional_context: Option<String>,
    /// Replacement arguments for the tool call. Only an allowing PreToolUse
    /// hook may send it, and it must be a JSON object. The permission
    /// decision was already made on the model's own arguments, so this is
    /// for a trusted hook that wraps the same work, such as an output proxy.
    #[serde(default)]
    pub updated_input: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookFailure {
    InvalidConfig(HookValidationError),
    InvalidInvocation(String),
    Cancelled,
    TimedOut,
    Spawn(String),
    ProcessIo(String),
    OutputLimitExceeded,
    NonZeroExit(Option<i32>),
    InvalidResponse(String),
}

/// Any `OutcomeUncertain` may have performed external effects and must not be retried
/// automatically after a crash, timeout, cancellation, or protocol failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookRunResult {
    Completed {
        response: HookResponse,
        stdout: String,
        stderr: String,
    },
    NotStarted(HookFailure),
    OutcomeUncertain(HookFailure),
    /// Bytes actually consumed before stopping. No successful protocol
    /// decision is inferred from these possibly incomplete streams.
    OutcomeUncertainCaptured {
        failure: HookFailure,
        stdout: String,
        stderr: String,
    },
}

/// Runs one configured hook. It never invokes a shell implicitly and clears ambient
/// environment variables except for a small process bootstrap allowlist.
pub async fn run_command_hook(
    config: &CommandHookConfig,
    invocation: &HookInvocation,
    cancel: &CancellationToken,
) -> HookRunResult {
    if let Err(error) = config.validate() {
        return HookRunResult::NotStarted(HookFailure::InvalidConfig(error));
    }
    if config.phase != invocation.phase || config.tool_name != invocation.tool_name {
        return HookRunResult::NotStarted(HookFailure::InvalidInvocation(
            "phase or tool name does not match hook configuration".into(),
        ));
    }
    if [
        &invocation.session_id,
        &invocation.turn_id,
        &invocation.tool_call_id,
    ]
    .iter()
    .any(|id| id.is_empty() || id.len() > 128)
    {
        return HookRunResult::NotStarted(HookFailure::InvalidInvocation(
            "session, turn and tool call IDs are required and bounded".into(),
        ));
    }
    if invocation.phase == HookPhase::PostToolUse && invocation.tool_output.is_none() {
        return HookRunResult::NotStarted(HookFailure::InvalidInvocation(
            "post-tool hook requires a committed tool output".into(),
        ));
    }
    let mut input = BoundedJson::new(MAX_HOOK_INPUT_BYTES);
    if let Err(error) = serde_json::to_writer(&mut input, invocation) {
        return HookRunResult::NotStarted(HookFailure::InvalidInvocation(if error.is_io() {
            "hook input exceeds its byte limit".into()
        } else {
            error.to_string()
        }));
    }
    let input = input.into_bytes();
    run_hook_input(config, input, cancel).await
}

/// Execute the same bounded configured hook under an explicitly human source.
/// The caller owns a native lease and syncs the operation/phase before this
/// function. This function never publishes a result or retries an effect.
pub async fn run_manual_command_hook(
    config: &CommandHookConfig,
    invocation: &ManualCommandHookInvocation,
    cancel: &CancellationToken,
) -> HookRunResult {
    if let Err(error) = config.validate() {
        return HookRunResult::NotStarted(HookFailure::InvalidConfig(error));
    }
    if invocation.version != 1
        || config.phase != invocation.phase
        || config.tool_name != invocation.tool_name
        || !matches!(
            invocation.tool_name.as_str(),
            "write_stdin" | "resize_command" | "terminate_command"
        )
        || [
            &invocation.session_id,
            &invocation.operation_id,
            &invocation.command_id,
        ]
        .iter()
        .any(|id| id.is_empty() || id.len() > 128)
        || (invocation.phase == HookPhase::PostToolUse && invocation.tool_output.is_none())
    {
        return HookRunResult::NotStarted(HookFailure::InvalidInvocation(
            "manual command hook requires version, owner, operation, command and exact phase"
                .into(),
        ));
    }
    let mut input = BoundedJson::new(MAX_HOOK_INPUT_BYTES);
    if let Err(error) = serde_json::to_writer(
        &mut input,
        &ManualCommandHookEnvelope {
            source: "manual_command",
            invocation,
        },
    ) {
        return HookRunResult::NotStarted(HookFailure::InvalidInvocation(if error.is_io() {
            "manual hook input exceeds its byte limit".into()
        } else {
            error.to_string()
        }));
    }
    run_hook_input(config, input.into_bytes(), cancel).await
}

pub(crate) struct HookProcessOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs the configured process to completion within its limits. Interpreting
/// the exit status and output belongs to the caller's protocol.
pub(crate) async fn capture_hook_process(
    config: &CommandHookConfig,
    input: Vec<u8>,
    cancel: &CancellationToken,
    extra_env: &[(&str, &std::ffi::OsStr)],
) -> Result<HookProcessOutput, HookRunResult> {
    if cancel.is_cancelled() {
        return Err(HookRunResult::NotStarted(HookFailure::Cancelled));
    }

    let mut process = Command::new(&config.executable);
    process
        .args(&config.args)
        .current_dir(&config.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for key in BOOTSTRAP_ENV_KEYS {
        if let Some(value) = std::env::var_os(key) {
            process.env(key, value);
        }
    }
    process.envs(extra_env.iter().copied());
    #[cfg(windows)]
    process.creation_flags(CREATE_NO_WINDOW | process_tree::CREATE_SUSPENDED);
    #[cfg(unix)]
    process.process_group(0);

    #[cfg(windows)]
    let tree = match ProcessTreeGuard::new() {
        Ok(tree) => tree,
        Err(error) => {
            return Err(HookRunResult::NotStarted(HookFailure::Spawn(format!(
                "cannot create process job: {error}"
            ))));
        }
    };
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return Err(HookRunResult::NotStarted(HookFailure::Spawn(error.to_string()))),
    };
    #[cfg(windows)]
    if let Err(error) = tree.assign_and_resume(&child) {
        drop(tree);
        let _ = child.kill().await;
        return Err(HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(format!(
            "cannot contain hook process tree: {error}"
        ))));
    }
    #[cfg(unix)]
    let tree = match ProcessTreeGuard::new(child.id()) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill().await;
            return Err(HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(format!(
                "cannot contain hook process tree: {error}"
            ))));
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        drop(tree);
        let _ = child.kill().await;
        return Err(HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(
            "stdin pipe unavailable after spawn".into(),
        )));
    };
    let Some(stdout) = child.stdout.take() else {
        drop(tree);
        let _ = child.kill().await;
        return Err(HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(
            "stdout pipe unavailable after spawn".into(),
        )));
    };
    let Some(stderr) = child.stderr.take() else {
        drop(tree);
        let _ = child.kill().await;
        return Err(HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(
            "stderr pipe unavailable after spawn".into(),
        )));
    };
    let limit = config.max_output_bytes;
    let captured = Arc::new(Mutex::new(HookCapture::default()));
    let uncertain = |failure| {
        let captured = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        HookRunResult::OutcomeUncertainCaptured {
            failure,
            stdout: String::from_utf8_lossy(&captured.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&captured.stderr).into_owned(),
        }
    };
    // Readers own the pipes across cancellation so buffered diagnostics can be
    // drained after the process tree has been stopped.
    let mut readers = HookOutputReaders {
        stdout: tokio::spawn(read_capped(stdout, limit, captured.clone(), false)),
        stderr: tokio::spawn(read_capped(stderr, limit, captured.clone(), true)),
    };
    let run = async {
        let write = async {
            stdin.write_all(&input).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<(), std::io::Error>(())
        };
        tokio::try_join!(
            write,
            async { (&mut readers.stdout).await.map_err(std::io::Error::other)? },
            async { (&mut readers.stderr).await.map_err(std::io::Error::other)? },
            child.wait()
        )
    };
    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(HookFailure::Cancelled),
        _ = tokio::time::sleep(Duration::from_millis(config.timeout_ms)) => Err(HookFailure::TimedOut),
        result = run => result.map_err(|error| {
            if error.kind() == std::io::ErrorKind::FileTooLarge {
                HookFailure::OutputLimitExceeded
            } else {
                HookFailure::ProcessIo(error.to_string())
            }
        }),
    };
    drop(tree);
    let ((), (), (), status) = match outcome {
        Ok(value) => value,
        Err(failure) => {
            let _ = child.kill().await;
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            drain_hook_reader(&mut readers.stdout).await;
            drain_hook_reader(&mut readers.stderr).await;
            return Err(uncertain(failure));
        }
    };
    let (stdout, stderr) = {
        let captured = captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (captured.stdout.clone(), captured.stderr.clone())
    };
    if stdout.len() + stderr.len() > limit {
        return Err(uncertain(HookFailure::OutputLimitExceeded));
    }
    Ok(HookProcessOutput {
        status,
        stdout,
        stderr,
    })
}

async fn run_hook_input(
    config: &CommandHookConfig,
    input: Vec<u8>,
    cancel: &CancellationToken,
) -> HookRunResult {
    let HookProcessOutput {
        status,
        stdout,
        stderr,
    } = match capture_hook_process(config, input, cancel, &[]).await {
        Ok(output) => output,
        Err(result) => return result,
    };
    let captured = (
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    );
    let uncertain = |failure| HookRunResult::OutcomeUncertainCaptured {
        failure,
        stdout: captured.0.clone(),
        stderr: captured.1.clone(),
    };
    if !status.success() {
        return uncertain(HookFailure::NonZeroExit(status.code()));
    }
    let stdout = match String::from_utf8(stdout) {
        Ok(stdout) => stdout,
        Err(error) => {
            return uncertain(HookFailure::InvalidResponse(error.to_string()));
        }
    };
    let response: HookResponse = match serde_json::from_str(&stdout) {
        Ok(response) => response,
        Err(error) => {
            return uncertain(HookFailure::InvalidResponse(error.to_string()));
        }
    };
    let text_too_large = response
        .reason
        .as_ref()
        .is_some_and(|value| value.len() > MAX_HOOK_TEXT_BYTES)
        || response
            .additional_context
            .as_ref()
            .is_some_and(|value| value.len() > MAX_HOOK_TEXT_BYTES);
    let invalid_rewrite = response.updated_input.as_ref().is_some_and(|input| {
        !input.is_object()
            || config.phase != HookPhase::PreToolUse
            || response.decision != HookDecision::Allow
    });
    if text_too_large
        || invalid_rewrite
        || (config.phase == HookPhase::PostToolUse && response.decision == HookDecision::Block)
        || (response.decision == HookDecision::Block
            && response
                .reason
                .as_ref()
                .is_none_or(|reason| reason.trim().is_empty()))
    {
        return uncertain(HookFailure::InvalidResponse(
            "hook decision or response text is invalid for this phase".into(),
        ));
    }
    HookRunResult::Completed {
        response,
        stdout,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

struct BoundedJson {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedJson {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedJson {
    fn write(&mut self, value: &[u8]) -> std::io::Result<usize> {
        if value.len() > self.limit - self.bytes.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "hook JSON exceeds byte limit",
            ));
        }
        self.bytes.extend_from_slice(value);
        Ok(value.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct HookOutputReaders {
    stdout: tokio::task::JoinHandle<std::io::Result<()>>,
    stderr: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Drop for HookOutputReaders {
    fn drop(&mut self) {
        // Dropping the hook future (including task abort) must not detach
        // readers that could wait forever on inherited descendant pipes.
        self.stdout.abort();
        self.stderr.abort();
    }
}

async fn drain_hook_reader(reader: &mut tokio::task::JoinHandle<std::io::Result<()>>) {
    // A finished handle may already have been consumed by try_join!.
    if !reader.is_finished()
        && tokio::time::timeout(Duration::from_secs(2), &mut *reader)
            .await
            .is_err()
    {
        reader.abort();
        let _ = reader.await;
    }
}

#[derive(Default)]
struct HookCapture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn read_capped(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
    capture: Arc<Mutex<HookCapture>>,
    stderr: bool,
) -> std::io::Result<()> {
    let mut chunk = [0u8; 4096];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        let mut captured = capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let available =
            limit.saturating_sub(captured.stdout.len().saturating_add(captured.stderr.len()));
        let keep = read.min(available);
        let destination = if stderr {
            &mut captured.stderr
        } else {
            &mut captured.stdout
        };
        destination.extend_from_slice(&chunk[..keep]);
        if read > available {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "hook combined output byte limit exceeded",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command_config(dir: &Path, script: &str) -> CommandHookConfig {
        #[cfg(windows)]
        let (executable, args) = {
            let executable = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe");
            let args = vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script.into(),
            ];
            (executable, args)
        };
        #[cfg(unix)]
        let (executable, args) = (PathBuf::from("/bin/sh"), vec!["-c".into(), script.into()]);
        CommandHookConfig {
            id: "guard-tool".into(),
            phase: HookPhase::PreToolUse,
            tool_name: "read_file".into(),
            executable,
            args,
            cwd: dir.to_path_buf(),
            timeout_ms: 5_000,
            max_output_bytes: 4_096,
        }
    }

    fn invocation() -> HookInvocation {
        HookInvocation {
            phase: HookPhase::PreToolUse,
            session_id: "session-1".into(),
            turn_id: "turn-1".into(),
            tool_call_id: "call-1".into(),
            tool_name: "read_file".into(),
            tool_input: json!({"path":"example.txt"}),
            tool_output: None,
        }
    }

    fn manual_invocation() -> ManualCommandHookInvocation {
        ManualCommandHookInvocation {
            version: 1,
            phase: HookPhase::PreToolUse,
            session_id: "native-session".into(),
            operation_id: uuid::Uuid::new_v4().to_string(),
            command_id: uuid::Uuid::new_v4().to_string(),
            tool_name: "write_stdin".into(),
            tool_input: json!({"input":"résumé 🌱\n", "close_after":true}),
            tool_output: None,
        }
    }

    #[tokio::test]
    async fn manual_command_hooks_preserve_exact_input_and_never_claim_model_authority() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let script = "cat > manual-invocation.json; printf '%s' '{\"decision\":\"allow\"}'";
        #[cfg(windows)]
        let script = "[IO.File]::WriteAllText('manual-invocation.json', [Console]::In.ReadToEnd()); [Console]::Out.Write('{\"decision\":\"allow\"}')";
        let mut config = command_config(dir.path(), script);
        config.tool_name = "write_stdin".into();
        let mut invocation = manual_invocation();
        for phase in [HookPhase::PreToolUse, HookPhase::PostToolUse] {
            config.phase = phase;
            invocation.phase = phase;
            invocation.tool_output =
                (phase == HookPhase::PostToolUse).then(|| json!({"stdin_closed":true}));
            let result =
                run_manual_command_hook(&config, &invocation, &CancellationToken::new()).await;
            assert!(
                matches!(
                    result,
                    HookRunResult::Completed {
                        response: HookResponse {
                            decision: HookDecision::Allow,
                            ..
                        },
                        ..
                    }
                ),
                "{result:?}"
            );
            let value: Value = serde_json::from_slice(
                &std::fs::read(dir.path().join("manual-invocation.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(value["source"], "manual_command");
            assert_eq!(value["operation_id"], invocation.operation_id);
            assert_eq!(value["command_id"], invocation.command_id);
            assert_eq!(value["tool_input"], invocation.tool_input);
            assert!(value.get("turn_id").is_none());
            assert!(value.get("tool_call_id").is_none());
            assert_eq!(value.get("tool_output"), invocation.tool_output.as_ref());
        }
    }

    #[tokio::test]
    async fn manual_command_hooks_reject_invalid_or_cancelled_input_before_spawn() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let script = "printf executed > effects.log; printf '%s' '{\"decision\":\"allow\"}'";
        #[cfg(windows)]
        let script = "[IO.File]::WriteAllText('effects.log','executed'); [Console]::Out.Write('{\"decision\":\"allow\"}')";
        let mut config = command_config(dir.path(), script);
        config.tool_name = "write_stdin".into();
        let valid = manual_invocation();
        let mut invalid = valid.clone();
        invalid.version = 2;
        assert!(matches!(
            run_manual_command_hook(&config, &invalid, &CancellationToken::new()).await,
            HookRunResult::NotStarted(HookFailure::InvalidInvocation(_))
        ));
        invalid = valid.clone();
        invalid.tool_input = json!({"input":"x".repeat(MAX_HOOK_INPUT_BYTES)});
        assert!(matches!(
            run_manual_command_hook(&config, &invalid, &CancellationToken::new()).await,
            HookRunResult::NotStarted(HookFailure::InvalidInvocation(_))
        ));
        config.phase = HookPhase::PostToolUse;
        invalid = valid.clone();
        invalid.phase = HookPhase::PostToolUse;
        assert!(matches!(
            run_manual_command_hook(&config, &invalid, &CancellationToken::new()).await,
            HookRunResult::NotStarted(HookFailure::InvalidInvocation(_))
        ));
        config.phase = HookPhase::PreToolUse;
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            run_manual_command_hook(&config, &valid, &cancel).await,
            HookRunResult::NotStarted(HookFailure::Cancelled)
        ));
        assert!(!dir.path().join("effects.log").exists());
    }

    #[test]
    fn user_hook_cannot_claim_project_instruction_marker_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = command_config(dir.path(), "");
        config.id = format!("{ACTIVATION_HOOK_PREFIX}example");
        assert!(matches!(
            config.validate(),
            Err(HookValidationError::ReservedId)
        ));
    }

    #[cfg(windows)]
    const ALLOW_SCRIPT: &str = "$payload = [Console]::In.ReadToEnd() | ConvertFrom-Json; if ($payload.tool_call_id -eq 'call-1' -and $payload.tool_input.path -eq 'example.txt') { [Console]::Out.Write('{\"decision\":\"allow\"}') } else { [Console]::Out.Write('{\"decision\":\"block\",\"reason\":\"wrong payload\"}') }";
    #[cfg(unix)]
    const ALLOW_SCRIPT: &str = "payload=$(cat); case \"$payload\" in *'\"tool_call_id\":\"call-1\"'*'\"path\":\"example.txt\"'*) printf '%s' '{\"decision\":\"allow\"}' ;; *) printf '%s' '{\"decision\":\"block\",\"reason\":\"wrong payload\"}' ;; esac";

    #[cfg(unix)]
    #[tokio::test]
    async fn only_an_allowing_pre_hook_may_rewrite_the_input_with_an_object() {
        let dir = tempfile::tempdir().unwrap();
        let run = |phase, body: &'static str| {
            let mut config = command_config(dir.path(), "");
            config.phase = phase;
            config.args = vec!["-c".into(), format!("cat >/dev/null; printf '%s' '{body}'")];
            let mut invocation = invocation();
            invocation.phase = phase;
            if phase == HookPhase::PostToolUse {
                invocation.tool_output = Some(json!({"content":"x","is_error":false}));
            }
            async move { run_command_hook(&config, &invocation, &CancellationToken::new()).await }
        };
        let accepted = run(
            HookPhase::PreToolUse,
            r#"{"decision":"allow","updated_input":{"path":"other.txt"}}"#,
        )
        .await;
        assert!(matches!(
            accepted,
            HookRunResult::Completed { response: HookResponse { updated_input: Some(input), .. }, .. }
                if input == json!({"path":"other.txt"})
        ));
        for (phase, body) in [
            (HookPhase::PreToolUse, r#"{"decision":"allow","updated_input":"rm -rf"}"#),
            (HookPhase::PreToolUse, r#"{"decision":"block","reason":"no","updated_input":{}}"#),
            (HookPhase::PostToolUse, r#"{"decision":"allow","updated_input":{}}"#),
        ] {
            assert!(
                matches!(
                    run(phase, body).await,
                    HookRunResult::OutcomeUncertainCaptured {
                        failure: HookFailure::InvalidResponse(_),
                        ..
                    }
                ),
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn real_process_receives_typed_input_and_allows() {
        let dir = tempfile::tempdir().unwrap();
        let config = command_config(dir.path(), ALLOW_SCRIPT);
        let result = run_command_hook(&config, &invocation(), &CancellationToken::new()).await;
        assert!(
            matches!(
                result,
                HookRunResult::Completed {
                    response: HookResponse {
                        decision: HookDecision::Allow,
                        ..
                    },
                    ..
                }
            ),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn invalid_configuration_and_pre_cancel_never_launch() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = command_config(dir.path(), ALLOW_SCRIPT);
        config.executable = PathBuf::from("relative-executable");
        assert!(matches!(
            run_command_hook(&config, &invocation(), &CancellationToken::new()).await,
            HookRunResult::NotStarted(HookFailure::InvalidConfig(
                HookValidationError::RelativePath
            ))
        ));
        config = command_config(dir.path(), ALLOW_SCRIPT);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            run_command_hook(&config, &invocation(), &cancel).await,
            HookRunResult::NotStarted(HookFailure::Cancelled)
        ));
    }

    #[tokio::test]
    async fn oversized_json_input_rejects_before_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let config = command_config(dir.path(), ALLOW_SCRIPT);
        let mut invocation = invocation();
        invocation.tool_input = json!({"data": "x".repeat(MAX_HOOK_INPUT_BYTES)});
        let result = run_command_hook(&config, &invocation, &CancellationToken::new()).await;
        assert!(matches!(
            result,
            HookRunResult::NotStarted(HookFailure::InvalidInvocation(_))
        ));
    }

    #[test]
    fn policy_digest_tracks_hook_order_and_script_file_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hook-script.txt");
        std::fs::write(&script, b"first").unwrap();
        let mut first = command_config(dir.path(), ALLOW_SCRIPT);
        first.args.push(script.to_string_lossy().into_owned());
        let initial = CommandHooks::new(vec![first.clone()])
            .unwrap()
            .digest_hex()
            .unwrap();
        std::fs::write(&script, b"other").unwrap();
        let changed = CommandHooks::new(vec![first.clone()])
            .unwrap()
            .digest_hex()
            .unwrap();
        assert_ne!(initial, changed);
        let mut second = first.clone();
        second.id = "second".into();
        let forward = CommandHooks::new(vec![first.clone(), second.clone()])
            .unwrap()
            .digest_hex()
            .unwrap();
        let reverse = CommandHooks::new(vec![second, first])
            .unwrap()
            .digest_hex()
            .unwrap();
        assert_ne!(forward, reverse);
    }

    #[tokio::test]
    async fn oversized_output_is_bounded_and_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let script = "[Console]::Out.Write('X' * 2048)";
        #[cfg(unix)]
        let script = "printf '%02048d' 0";
        let mut config = command_config(dir.path(), script);
        config.max_output_bytes = 256;
        let result = run_command_hook(&config, &invocation(), &CancellationToken::new()).await;
        assert!(
            matches!(
                result,
                HookRunResult::OutcomeUncertainCaptured {
                    failure: HookFailure::OutputLimitExceeded,
                    ..
                }
            ),
            "{result:?}"
        );
        let HookRunResult::OutcomeUncertainCaptured { stdout, stderr, .. } = result else {
            unreachable!()
        };
        assert_eq!(stdout.len() + stderr.len(), 256);
    }

    #[tokio::test]
    async fn timeout_after_side_effect_is_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let script = "[Console]::Out.Write('partial-out'); [Console]::Out.Flush(); [Console]::Error.Write('partial-err'); [Console]::Error.Flush(); [IO.File]::WriteAllText('started.txt','yes'); Start-Sleep -Seconds 30";
        #[cfg(unix)]
        let script =
            "printf partial-out; printf partial-err >&2; printf yes > started.txt; sleep 30";
        let mut config = command_config(dir.path(), script);
        // Parallel workspace tests may delay scheduling the spawned shell.
        // Keep the operation sleeping longer than the deadline while allowing
        // startup enough time to publish the effect this test must observe.
        config.timeout_ms = 5_000;
        let result = run_command_hook(&config, &invocation(), &CancellationToken::new()).await;
        assert!(
            matches!(
                result,
                HookRunResult::OutcomeUncertainCaptured {
                    failure: HookFailure::TimedOut,
                    ..
                }
            ),
            "{result:?}"
        );
        assert!(dir.path().join("started.txt").exists());
        let HookRunResult::OutcomeUncertainCaptured { stdout, stderr, .. } = result else {
            unreachable!()
        };
        assert_eq!(stdout, "partial-out");
        assert_eq!(stderr, "partial-err");
    }

    #[tokio::test]
    async fn cancellation_after_launch_is_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let script = "[Console]::Out.Write('partial-out'); [Console]::Out.Flush(); [Console]::Error.Write('partial-err'); [Console]::Error.Flush(); [IO.File]::WriteAllText('started.txt','yes'); Start-Sleep -Seconds 30";
        #[cfg(unix)]
        let script =
            "printf partial-out; printf partial-err >&2; printf yes > started.txt; sleep 30";
        let mut config = command_config(dir.path(), script);
        config.timeout_ms = 30_000;
        let cancel = CancellationToken::new();
        let runner_cancel = cancel.clone();
        let running =
            tokio::spawn(
                async move { run_command_hook(&config, &invocation(), &runner_cancel).await },
            );
        tokio::time::timeout(Duration::from_secs(15), async {
            while !dir.path().join("started.txt").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("hook process did not start");
        cancel.cancel();
        let result = running.await.unwrap();
        assert!(
            matches!(
                result,
                HookRunResult::OutcomeUncertainCaptured {
                    failure: HookFailure::Cancelled,
                    ..
                }
            ),
            "{result:?}"
        );
        let HookRunResult::OutcomeUncertainCaptured { stdout, stderr, .. } = result else {
            unreachable!()
        };
        assert_eq!(stdout, "partial-out");
        assert_eq!(stderr, "partial-err");
    }

    #[tokio::test]
    async fn dropped_hook_readers_release_open_pipes_without_eof() {
        let captured = Arc::new(Mutex::new(HookCapture::default()));
        let (stdout_writer, stdout) = tokio::io::duplex(64);
        let (stderr_writer, stderr) = tokio::io::duplex(64);
        let readers = HookOutputReaders {
            stdout: tokio::spawn(read_capped(stdout, 64, captured.clone(), false)),
            stderr: tokio::spawn(read_capped(stderr, 64, captured.clone(), true)),
        };
        assert_eq!(Arc::strong_count(&captured), 3);
        drop(readers);
        tokio::time::timeout(Duration::from_secs(2), async {
            while Arc::strong_count(&captured) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("abandoned hook readers must release pipes even while writers remain open");
        drop((stdout_writer, stderr_writer));
    }

    #[tokio::test]
    async fn failed_process_retains_both_diagnostic_streams() {
        let dir = tempfile::tempdir().unwrap();
        for nonzero in [false, true] {
            #[cfg(windows)]
            let script = if nonzero {
                "[Console]::Out.Write('invalid-json'); [Console]::Error.Write('failure-details'); exit 7"
            } else {
                "[Console]::Out.Write('invalid-json'); [Console]::Error.Write('failure-details')"
            };
            #[cfg(unix)]
            let script = if nonzero {
                "printf invalid-json; printf failure-details >&2; exit 7"
            } else {
                "printf invalid-json; printf failure-details >&2"
            };
            let config = command_config(dir.path(), script);
            let result = run_command_hook(&config, &invocation(), &CancellationToken::new()).await;
            let HookRunResult::OutcomeUncertainCaptured {
                failure,
                stdout,
                stderr,
            } = result
            else {
                panic!("failed hook must retain diagnostics: {result:?}");
            };
            if nonzero {
                assert_eq!(failure, HookFailure::NonZeroExit(Some(7)));
            } else {
                assert!(matches!(failure, HookFailure::InvalidResponse(_)));
            }
            assert_eq!(stdout, "invalid-json");
            assert_eq!(stderr, "failure-details");
        }
    }

    #[tokio::test]
    async fn post_tool_hook_rejects_block_decision() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let script = "[Console]::Out.Write('{\"decision\":\"block\",\"reason\":\"too late\"}')";
        #[cfg(unix)]
        let script = "printf '%s' '{\"decision\":\"block\",\"reason\":\"too late\"}'";
        let mut config = command_config(dir.path(), script);
        config.phase = HookPhase::PostToolUse;
        let mut invocation = invocation();
        invocation.phase = HookPhase::PostToolUse;
        invocation.tool_output = Some(json!({"content":"done"}));
        let result = run_command_hook(&config, &invocation, &CancellationToken::new()).await;
        assert!(
            matches!(
                result,
                HookRunResult::OutcomeUncertainCaptured {
                    failure: HookFailure::InvalidResponse(_),
                    ..
                }
            ),
            "{result:?}"
        );
    }
}
