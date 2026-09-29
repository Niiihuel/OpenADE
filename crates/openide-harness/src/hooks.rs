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
        let hooks = Self { configs };
        hooks.digest_hex()?;
        Ok(hooks)
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
            config.validate()?;
            hash_text(&mut hasher, &config.id);
            hash_text(
                &mut hasher,
                match config.phase {
                    HookPhase::PreToolUse => "pre_tool_use",
                    HookPhase::PostToolUse => "post_tool_use",
                },
            );
            hash_text(&mut hasher, &config.tool_name);
            hash_path(&mut hasher, &config.executable, true)?;
            hash_path(&mut hasher, &config.cwd, false)?;
            hasher.update(config.timeout_ms.to_be_bytes());
            hash_len(&mut hasher, config.max_output_bytes);
            hash_len(&mut hasher, config.args.len());
            for arg in &config.args {
                hash_text(&mut hasher, arg);
                let path = Path::new(arg);
                let candidate = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    config.cwd.join(path)
                };
                if candidate.is_file() {
                    hash_path(&mut hasher, &candidate, true)?;
                } else if candidate.is_dir() {
                    hash_path(&mut hasher, &candidate, false)?;
                }
            }
        }
        Ok(format!("{:x}", hasher.finalize()))
    }
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
        stderr: String,
    },
    NotStarted(HookFailure),
    OutcomeUncertain(HookFailure),
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
    if cancel.is_cancelled() {
        return HookRunResult::NotStarted(HookFailure::Cancelled);
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
    #[cfg(windows)]
    process.creation_flags(CREATE_NO_WINDOW | process_tree::CREATE_SUSPENDED);
    #[cfg(unix)]
    process.process_group(0);

    #[cfg(windows)]
    let tree = match ProcessTreeGuard::new() {
        Ok(tree) => tree,
        Err(error) => {
            return HookRunResult::NotStarted(HookFailure::Spawn(format!(
                "cannot create process job: {error}"
            )));
        }
    };
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return HookRunResult::NotStarted(HookFailure::Spawn(error.to_string())),
    };
    #[cfg(windows)]
    if let Err(error) = tree.assign_and_resume(&child) {
        drop(tree);
        let _ = child.kill().await;
        return HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(format!(
            "cannot contain hook process tree: {error}"
        )));
    }
    #[cfg(unix)]
    let tree = match ProcessTreeGuard::new(child.id()) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill().await;
            return HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(format!(
                "cannot contain hook process tree: {error}"
            )));
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        drop(tree);
        let _ = child.kill().await;
        return HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(
            "stdin pipe unavailable after spawn".into(),
        ));
    };
    let Some(stdout) = child.stdout.take() else {
        drop(tree);
        let _ = child.kill().await;
        return HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(
            "stdout pipe unavailable after spawn".into(),
        ));
    };
    let Some(stderr) = child.stderr.take() else {
        drop(tree);
        let _ = child.kill().await;
        return HookRunResult::OutcomeUncertain(HookFailure::ProcessIo(
            "stderr pipe unavailable after spawn".into(),
        ));
    };
    let limit = config.max_output_bytes;
    let run = async {
        let write = async {
            stdin.write_all(&input).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<(), std::io::Error>(())
        };
        tokio::try_join!(
            write,
            read_capped(stdout, limit),
            read_capped(stderr, limit),
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
    let ((), stdout, stderr, status) = match outcome {
        Ok(value) => value,
        Err(failure) => {
            let _ = child.kill().await;
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            return HookRunResult::OutcomeUncertain(failure);
        }
    };
    if stdout.len() + stderr.len() > limit {
        return HookRunResult::OutcomeUncertain(HookFailure::OutputLimitExceeded);
    }
    if !status.success() {
        return HookRunResult::OutcomeUncertain(HookFailure::NonZeroExit(status.code()));
    }
    let stdout = match String::from_utf8(stdout) {
        Ok(stdout) => stdout,
        Err(error) => {
            return HookRunResult::OutcomeUncertain(HookFailure::InvalidResponse(
                error.to_string(),
            ));
        }
    };
    let response: HookResponse = match serde_json::from_str(&stdout) {
        Ok(response) => response,
        Err(error) => {
            return HookRunResult::OutcomeUncertain(HookFailure::InvalidResponse(
                error.to_string(),
            ));
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
    if text_too_large
        || (config.phase == HookPhase::PostToolUse && response.decision == HookDecision::Block)
        || (response.decision == HookDecision::Block
            && response
                .reason
                .as_ref()
                .is_none_or(|reason| reason.trim().is_empty()))
    {
        return HookRunResult::OutcomeUncertain(HookFailure::InvalidResponse(
            "hook decision or response text is invalid for this phase".into(),
        ));
    }
    HookRunResult::Completed {
        response,
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

async fn read_capped(reader: impl AsyncRead + Unpin, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "hook output byte limit exceeded",
        ));
    }
    Ok(bytes)
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
                HookRunResult::OutcomeUncertain(HookFailure::OutputLimitExceeded)
            ),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn timeout_after_side_effect_is_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let script = "[IO.File]::WriteAllText('started.txt','yes'); Start-Sleep -Seconds 10";
        #[cfg(unix)]
        let script = "printf yes > started.txt; sleep 10";
        let mut config = command_config(dir.path(), script);
        config.timeout_ms = 1_000;
        let result = run_command_hook(&config, &invocation(), &CancellationToken::new()).await;
        assert!(
            matches!(
                result,
                HookRunResult::OutcomeUncertain(HookFailure::TimedOut)
            ),
            "{result:?}"
        );
        assert!(dir.path().join("started.txt").exists());
    }

    #[tokio::test]
    async fn cancellation_after_launch_is_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let script = "[IO.File]::WriteAllText('started.txt','yes'); Start-Sleep -Seconds 10";
        #[cfg(unix)]
        let script = "printf yes > started.txt; sleep 10";
        let config = command_config(dir.path(), script);
        let cancel = CancellationToken::new();
        let runner_cancel = cancel.clone();
        let running =
            tokio::spawn(
                async move { run_command_hook(&config, &invocation(), &runner_cancel).await },
            );
        tokio::time::timeout(Duration::from_secs(4), async {
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
                HookRunResult::OutcomeUncertain(HookFailure::Cancelled)
            ),
            "{result:?}"
        );
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
                HookRunResult::OutcomeUncertain(HookFailure::InvalidResponse(_))
            ),
            "{result:?}"
        );
    }
}
