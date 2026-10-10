//! OS command isolation chosen by trusted host policy.
//!
//! On Windows, `WorkspaceWriteOffline` uses Microsoft's MIT-licensed MXC
//! ProcessContainer implementation (pinned in Cargo.toml). The policy grants
//! write access only to the canonical workspace and denies outbound and local
//! network traffic. MXC selects AppContainer or BaseProcessContainer for the
//! host, installs the access policy, and owns a terminating job. If setup
//! fails, the command is never retried on the host.

use crate::AgentError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandIsolation {
    #[default]
    HostUnrestricted,
    /// Platform OS containment with workspace writes and no network sockets.
    WorkspaceWriteOffline,
}

impl CommandIsolation {
    /// Whether this build implements the profile. This alone does not attest
    /// to installed helpers, kernel support, or successful runtime setup.
    pub const fn is_supported_on_host(self) -> bool {
        match self {
            Self::HostUnrestricted => true,
            Self::WorkspaceWriteOffline => {
                cfg!(windows)
                    || cfg!(all(
                        target_os = "linux",
                        any(target_arch = "x86_64", target_arch = "aarch64")
                    ))
            }
        }
    }

    /// Build support plus required helper presence. On Linux every launch must
    /// additionally complete namespaces/seccomp/supervision and the target gate;
    /// helper presence is not a claim that those runtime checks succeeded.
    pub fn is_available_on_host(self) -> bool {
        if !self.is_supported_on_host() {
            return false;
        }
        #[cfg(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        if self == Self::WorkspaceWriteOffline {
            return linux::helper_available();
        }
        true
    }

    pub const fn run_command_description(self) -> &'static str {
        match self {
            Self::HostUnrestricted => "Run a command with the host user's filesystem and network rights; no implicit shell.",
            Self::WorkspaceWriteOffline if cfg!(windows) => "Run a command inside a Windows workspace-write/offline OS sandbox; no implicit shell.",
            Self::WorkspaceWriteOffline => "Run a command inside the workspace-write/offline OS sandbox. Requires Bubblewrap and working user namespaces on Linux. Only the workspace, mounted system runtime and private temporary files are visible; network sockets are blocked. No implicit shell or unrestricted fallback.",
        }
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[path = "command_sandbox_linux.rs"]
pub(crate) mod linux;

pub(super) fn require_available(isolation: CommandIsolation) -> Result<(), AgentError> {
    if isolation.is_supported_on_host() {
        Ok(())
    } else {
        Err(AgentError::Tool(
            "workspace-write/offline command isolation is unavailable on this host; command was not started".into(),
        ))
    }
}

#[cfg(windows)]
pub(super) async fn run_workspace_write_offline(
    program: &str,
    args: &[&str],
    timeout_ms: u64,
    context: &crate::ToolContext,
) -> Result<crate::ToolOutput, AgentError> {
    let program = program.to_owned();
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let workspace = context.workspace.clone();
    let cancel = context.cancel.child_token();
    // Dropping the async caller must also stop the blocking MXC worker.
    let _cancel_on_drop = CancelOnDrop(cancel.clone());
    tokio::task::spawn_blocking(move || {
        windows::run(&program, &args, timeout_ms, &workspace, &cancel)
    })
    .await
    .map_err(|error| {
        AgentError::EffectUncertain(format!(
            "sandbox worker failed; command outcome is unknown: {error}"
        ))
    })?
}

/// Start a live MXC ProcessContainer with the same policy as `run_command`.
/// The caller must retain the returned sandbox until `wait` or drop has killed
/// the process tree and torn down policy. No unrestricted fallback is used.
#[cfg(windows)]
pub(crate) fn spawn_workspace_write_offline_persistent(
    program: &str,
    args: &[String],
    timeout_ms: u64,
    workspace: &std::path::Path,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<mxc_sdk::Sandbox, AgentError> {
    windows::spawn(program, args, timeout_ms, workspace, cancel)
}

#[cfg(windows)]
struct CancelOnDrop(tokio_util::sync::CancellationToken);

#[cfg(windows)]
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(windows)]
mod windows {
    use super::super::MAX_OUTPUT_BYTES;
    use crate::{AgentError, ToolOutput};
    use mxc_sdk::{
        build_request_with_containment, configs::ProcessContainer, policy::FilesystemSection,
        spawn_sandbox, Containment, SandboxPolicy, WaitOutcome,
    };
    use std::io::{self, Read};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Path, Prefix};
    use std::thread;
    use std::time::{Duration, Instant};
    use tokio_util::sync::CancellationToken;
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

    const POLL_INTERVAL: Duration = Duration::from_millis(10);
    const POLICY_VERSION: &str = "0.7.0-alpha";
    const DRIVE_FIXED: u32 = 3; // Win32 GetDriveTypeW result.

    pub(super) fn run(
        program: &str,
        args: &[String],
        timeout_ms: u64,
        workspace: &Path,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, AgentError> {
        let mut sandbox = spawn(program, args, timeout_ms, workspace, cancel)?;
        let stdout = sandbox.take_stdout().ok_or_else(|| {
            let _ = sandbox.kill();
            AgentError::EffectUncertain("sandbox stdout pipe is unavailable after spawn".into())
        })?;
        let stderr = sandbox.take_stderr().ok_or_else(|| {
            let _ = sandbox.kill();
            AgentError::EffectUncertain("sandbox stderr pipe is unavailable after spawn".into())
        })?;
        let stdout_reader = thread::spawn(move || read_capped(stdout));
        let stderr_reader = thread::spawn(move || read_capped(stderr));
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let outcome = loop {
            if cancel.is_cancelled() {
                terminate(&mut sandbox)?;
                break SandboxOutcome::Cancelled;
            }
            match sandbox.try_wait() {
                Ok(Some(_)) => {
                    break SandboxOutcome::Finished(sandbox.wait().map_err(|error| {
                        AgentError::EffectUncertain(format!(
                            "sandbox wait failed after spawn: {error}"
                        ))
                    })?);
                }
                Ok(None) => {}
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    sandbox.wait().map_err(|cleanup| {
                        AgentError::EffectUncertain(format!(
                            "sandbox timeout cleanup failed: {cleanup}"
                        ))
                    })?;
                    break SandboxOutcome::TimedOut;
                }
                Err(error) => {
                    terminate(&mut sandbox)?;
                    return Err(AgentError::EffectUncertain(format!(
                        "sandbox wait failed after spawn: {error}"
                    )));
                }
            }
            if Instant::now() >= deadline {
                terminate(&mut sandbox)?;
                break SandboxOutcome::TimedOut;
            }
            thread::sleep(POLL_INTERVAL);
        };
        let (stdout, stdout_truncated) = join_reader(stdout_reader, "stdout")?;
        let (stderr, stderr_truncated) = join_reader(stderr_reader, "stderr")?;
        let warnings = sandbox.warnings();
        if !warnings.is_empty() {
            return Err(AgentError::EffectUncertain(format!(
                "sandbox policy or cleanup warning: {}",
                warnings.join("; ")
            )));
        }
        if matches!(outcome, SandboxOutcome::Cancelled) {
            return Err(AgentError::Cancelled);
        }
        if matches!(
            outcome,
            SandboxOutcome::TimedOut | SandboxOutcome::Finished(WaitOutcome::TimedOut)
        ) {
            return Err(AgentError::EffectUncertain(format!(
                "command timed out after {timeout_ms} ms; it may have changed files before termination"
            )));
        }
        let SandboxOutcome::Finished(WaitOutcome::Exited(code)) = outcome else {
            unreachable!()
        };
        let mut content = format!("exit code: {code}\nstdout:\n{stdout}\nstderr:\n{stderr}");
        if stdout_truncated || stderr_truncated {
            content.push_str("\n[command output truncated]");
        }
        Ok(ToolOutput {
            content,
            is_error: code != 0,
        })
    }

    pub(super) fn spawn(
        program: &str,
        args: &[String],
        timeout_ms: u64,
        workspace: &Path,
        cancel: &CancellationToken,
    ) -> Result<mxc_sdk::Sandbox, AgentError> {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let workspace = std::fs::canonicalize(workspace).map_err(|error| {
            AgentError::Tool(format!("cannot resolve sandbox workspace: {error}"))
        })?;
        if !workspace.is_dir() || workspace.parent().is_none() {
            return Err(AgentError::Tool(
                "sandbox workspace must be an existing non-root directory".into(),
            ));
        }
        require_local_fixed_drive(&workspace)?;
        let workspace = workspace.to_str().ok_or_else(|| {
            AgentError::Tool("sandbox workspace path must be valid Unicode".into())
        })?;
        let command = command_line(program, args)?;
        let timeout_ms = u32::try_from(timeout_ms)
            .map_err(|_| AgentError::Tool("sandbox timeout exceeds supported range".into()))?;
        let policy = SandboxPolicy {
            version: POLICY_VERSION.into(),
            filesystem: Some(FilesystemSection {
                readwrite_paths: vec![workspace.to_owned()],
                readonly_paths: Vec::new(),
                denied_paths: Vec::new(),
                clear_policy_on_exit: None,
            }),
            network: Some(Default::default()),
            ui: None,
            timeout_ms: Some(timeout_ms),
        };
        let mut request = build_request_with_containment(
            &policy,
            &Containment::ProcessContainer(ProcessContainer::default()),
            &command,
            None,
        )
        .map_err(|error| AgentError::Tool(format!("cannot build command sandbox: {error}")))?;
        request.set_working_directory(workspace);
        request.set_env(filtered_environment());
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let mut sandbox = spawn_sandbox(request)
            .map_err(|error| AgentError::Tool(format!("cannot start command sandbox: {error}")))?;
        if cancel.is_cancelled() {
            terminate(&mut sandbox)?;
            return Err(AgentError::Cancelled);
        }
        let warnings = sandbox.warnings();
        if !warnings.is_empty() {
            sandbox.kill().map_err(|error| {
                AgentError::EffectUncertain(format!(
                    "sandbox policy warning and kill failed: {error}"
                ))
            })?;
            let _ = sandbox.wait();
            return Err(AgentError::EffectUncertain(format!(
                "sandbox policy warning: {}",
                warnings.join("; ")
            )));
        }
        Ok(sandbox)
    }

    #[derive(Clone, Copy)]
    enum SandboxOutcome {
        Finished(WaitOutcome),
        Cancelled,
        TimedOut,
    }

    fn terminate(sandbox: &mut mxc_sdk::Sandbox) -> Result<(), AgentError> {
        sandbox.kill().map_err(|error| {
            AgentError::EffectUncertain(format!("sandbox tree kill failed: {error}"))
        })?;
        sandbox.wait().map_err(|error| {
            AgentError::EffectUncertain(format!("sandbox cleanup failed: {error}"))
        })?;
        Ok(())
    }

    fn read_capped(mut reader: Box<dyn Read + Send>) -> io::Result<(String, bool)> {
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut chunk = [0u8; 8192];
        loop {
            let count = reader.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            let remaining = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
            let keep = count.min(remaining);
            kept.extend_from_slice(&chunk[..keep]);
            truncated |= keep < count;
        }
        Ok((String::from_utf8_lossy(&kept).into_owned(), truncated))
    }

    fn join_reader(
        reader: thread::JoinHandle<io::Result<(String, bool)>>,
        stream: &str,
    ) -> Result<(String, bool), AgentError> {
        reader
            .join()
            .map_err(|_| AgentError::EffectUncertain(format!("sandbox {stream} reader panicked")))?
            .map_err(|error| {
                AgentError::EffectUncertain(format!("sandbox {stream} read failed: {error}"))
            })
    }

    fn filtered_environment() -> Vec<(String, String)> {
        // MXC requires SystemRoot and LOCALAPPDATA in explicit env blocks.
        // Other entries here are Windows process bootstrap/path resolution;
        // never forward arbitrary host environment variables to model tools.
        const BOOTSTRAP_KEYS: &[&str] = &[
            "SYSTEMROOT",
            "WINDIR",
            "SYSTEMDRIVE",
            "LOCALAPPDATA",
            "USERPROFILE",
            "TEMP",
            "TMP",
            "PATH",
            "PATHEXT",
            "COMSPEC",
        ];
        BOOTSTRAP_KEYS
            .iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| ((*key).into(), value)))
            .collect()
    }

    fn require_local_fixed_drive(workspace: &Path) -> Result<(), AgentError> {
        let drive = match workspace.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
                _ => {
                    return Err(AgentError::Tool(
                        "network or device path cannot be an offline sandbox workspace".into(),
                    ));
                }
            },
            _ => return Err(AgentError::Tool("sandbox workspace has no drive".into())),
        };
        let root = format!("{}:\\", char::from(drive).to_ascii_uppercase());
        let wide = std::ffi::OsStr::new(&root)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        if unsafe { GetDriveTypeW(wide.as_ptr()) } != DRIVE_FIXED {
            return Err(AgentError::Tool(
                "offline sandbox workspace must be on a local fixed drive".into(),
            ));
        }
        Ok(())
    }

    fn command_line(program: &str, args: &[String]) -> Result<String, AgentError> {
        if program.is_empty() || program.contains('\0') || args.iter().any(|arg| arg.contains('\0'))
        {
            return Err(AgentError::Tool(
                "command contains an invalid NUL byte".into(),
            ));
        }
        let mut command = quote_arg(program);
        for arg in args {
            command.push(' ');
            command.push_str(&quote_arg(arg));
        }
        Ok(command)
    }

    // Windows CRT argv quoting. MXC accepts a single command line, while the
    // run_command tool supplies an argument array.
    fn quote_arg(value: &str) -> String {
        let mut result = String::from("\"");
        let mut backslashes = 0;
        for ch in value.chars() {
            match ch {
                '\\' => backslashes += 1,
                '"' => {
                    result.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    result.push('"');
                    backslashes = 0;
                }
                _ => {
                    result.extend(std::iter::repeat_n('\\', backslashes));
                    result.push(ch);
                    backslashes = 0;
                }
            }
        }
        result.extend(std::iter::repeat_n('\\', backslashes * 2));
        result.push('"');
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn preserves_windows_argv_boundaries() {
            let command = command_line(
                r"C:\Program Files\app.exe",
                &["a b".into(), "plain".into(), "a\"b".into(), "tail\\".into()],
            )
            .unwrap();
            assert_eq!(
                command,
                "\"C:\\Program Files\\app.exe\" \"a b\" \"plain\" \"a\\\"b\" \"tail\\\\\""
            );
        }

        #[test]
        fn network_share_workspace_is_rejected() {
            let error =
                require_local_fixed_drive(Path::new(r"\\?\UNC\server\share\repo")).unwrap_err();
            assert!(error.to_string().contains("network or device path"));
        }
    }
}
