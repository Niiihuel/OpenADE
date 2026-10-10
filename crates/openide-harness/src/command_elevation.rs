//! Explicit, per-command Linux administrator access through Polkit.
//!
//! The privileged entry point never opens the application. A private stdin
//! pipe is a lifetime lease, not command input: EOF cancels the root process
//! group from the privileged supervisor itself. The user process cannot
//! reliably send signals to a root child, so it never claims that it can.

use crate::{AgentError, ToolContext, ToolOutput};
use openide_core::ToolCall;

const HELPER_FLAG: &str = "--openide-elevated-command-helper-v1";
#[cfg(target_os = "linux")]
static HELPER_ENTRY_REGISTERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Read-only host capability check. It does not authenticate, start pkexec,
/// mutate system policy, or require the desktop to be running as root.
pub fn linux_administrator_commands_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::authentication_program().is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Call before initializing the desktop or reading application credentials.
/// `None` means ordinary startup; `Some(code)` means the process must exit.
/// The helper accepts only its versioned argv protocol and authenticated root.
pub fn run_elevated_command_helper_from_env() -> Option<i32> {
    #[cfg(target_os = "linux")]
    HELPER_ENTRY_REGISTERED.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_FLAG)) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0
            || std::env::var("PKEXEC_UID")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .is_none()
        {
            eprintln!("administrator helper requires authenticated Linux Polkit execution");
            return Some(125);
        }
        let arguments = arguments
            .map(|value| value.into_string())
            .collect::<Result<Vec<_>, _>>();
        let Ok(arguments) = arguments else {
            eprintln!("administrator helper arguments must be UTF-8");
            return Some(125);
        };
        Some(linux::helper_entry(&arguments))
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("administrator commands are supported only on Linux through Polkit");
        Some(125)
    }
}

pub(crate) async fn run(call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, AgentError> {
    #[cfg(target_os = "linux")]
    {
        linux::run(call, context).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (call, context);
        Err(AgentError::Tool(
            "administrator commands are supported only on Linux through Polkit".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio_util::sync::CancellationToken;

    const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
    const MAX_ARGUMENTS: usize = 256;
    const MAX_WIRE_BYTES: usize = 1024 * 1024;

    struct ElevationHost {
        pkexec: PathBuf,
        helper: PathBuf,
    }

    struct Invocation {
        timeout_ms: u64,
        workspace: PathBuf,
        program: PathBuf,
        args: Vec<String>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WireResult {
        content: String,
        is_error: bool,
        uncertain: bool,
    }

    fn invalid(message: impl Into<String>) -> AgentError {
        AgentError::Tool(message.into())
    }

    fn executable(path: &Path) -> bool {
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }

    fn resolve_program(program: &str, workspace: &Path) -> Result<PathBuf, AgentError> {
        let path = Path::new(program);
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else if path.components().count() > 1 {
            workspace.join(path)
        } else {
            std::env::var_os("PATH")
                .as_deref()
                .map(std::env::split_paths)
                .into_iter()
                .flatten()
                .map(|directory| {
                    if directory.is_absolute() {
                        directory.join(path)
                    } else {
                        workspace.join(directory).join(path)
                    }
                })
                .find(|candidate| executable(candidate))
                .ok_or_else(|| invalid("administrator command executable was not found"))?
        };
        if !executable(&candidate) {
            return Err(invalid("administrator command is not an executable file"));
        }
        candidate
            .canonicalize()
            .map_err(|_| invalid("cannot resolve administrator command executable"))
    }

    fn invocation(call: &ToolCall, context: &ToolContext) -> Result<Invocation, AgentError> {
        let program = crate::tools::required_string(&call.arguments, "command")?;
        if program.trim().is_empty() || program.contains('\0') {
            return Err(invalid("administrator command is empty or invalid"));
        }
        let args = match call.arguments.get("args") {
            None => Vec::new(),
            Some(Value::Array(values)) if values.len() <= MAX_ARGUMENTS => values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|value| !value.contains('\0'))
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("administrator command args must be valid strings"))
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(invalid(
                    "administrator command args must be an array of at most 256 strings",
                ))
            }
        };
        let timeout_ms = match call.arguments.get("timeout_ms") {
            None => 30_000,
            Some(value) => value
                .as_u64()
                .filter(|value| (1_000..=120_000).contains(value))
                .ok_or_else(|| {
                    invalid("administrator timeout_ms must be between 1000 and 120000")
                })?,
        };
        let workspace = context
            .workspace
            .canonicalize()
            .map_err(|_| invalid("cannot resolve administrator command workspace"))?;
        let program = resolve_program(program, &workspace)?;
        if workspace.to_str().is_none() || program.to_str().is_none() {
            return Err(invalid("administrator command paths must be UTF-8"));
        }
        let bytes = args.iter().map(String::len).sum::<usize>()
            + workspace.as_os_str().len()
            + program.as_os_str().len()
            + timeout_ms.to_string().len();
        if bytes > MAX_ARGUMENT_BYTES {
            return Err(invalid("administrator command arguments exceed 64 KiB"));
        }
        Ok(Invocation {
            timeout_ms,
            workspace,
            program,
            args,
        })
    }

    pub(super) fn authentication_program() -> Result<PathBuf, AgentError> {
        // Do not resolve the authentication program from a workspace or an
        // arbitrary PATH entry: a model-written executable is not Polkit.
        ["/run/wrappers/bin/pkexec", "/run/current-system/sw/bin/pkexec", "/usr/bin/pkexec", "/bin/pkexec"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| {
                std::fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file()
                        && metadata.uid() == 0
                        && metadata.mode() & libc::S_ISUID != 0
                        && metadata.mode() & 0o022 == 0
                        && metadata.mode() & 0o111 != 0
                })
            })
            .ok_or_else(|| invalid("Linux administrator access requires root-owned pkexec with its setuid wrapper enabled and a graphical Polkit authentication agent"))
    }

    fn host() -> Result<ElevationHost, AgentError> {
        if !HELPER_ENTRY_REGISTERED.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(invalid("the host executable has not registered its bounded administrator helper entry point"));
        }
        let pkexec = authentication_program()?;
        let helper = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .map_err(|_| invalid("cannot locate the administrator command supervisor"))?;
        Ok(ElevationHost { pkexec, helper })
    }

    pub(super) async fn run(
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let invocation = invocation(call, context)?;
        run_with_host(invocation, context, host()?).await
    }

    async fn run_with_host(
        invocation: Invocation,
        context: &ToolContext,
        host: ElevationHost,
    ) -> Result<ToolOutput, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let mut command = tokio::process::Command::new(host.pkexec);
        command
            .arg("--disable-internal-agent")
            .arg("--user")
            .arg("root")
            .arg(host.helper)
            .arg(HELPER_FLAG)
            .arg(invocation.timeout_ms.to_string())
            .arg(&invocation.workspace)
            .arg(&invocation.program)
            .args(&invocation.args)
            .env_clear()
            .current_dir(&invocation.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| {
            invalid(
                "cannot start Linux Polkit authentication; administrator command was not executed",
            )
        })?;
        // Do not send passwords, command input, or environment over this pipe.
        // Keeping its write end alive grants the supervisor a lifetime lease.
        let lifetime = child.stdin.take().ok_or_else(|| {
            AgentError::EffectUncertain(
                "administrator supervisor lifetime pipe is unavailable".into(),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AgentError::EffectUncertain("administrator stdout is unavailable".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AgentError::EffectUncertain("administrator stderr is unavailable".into())
        })?;
        let stdout = tokio::spawn(read_wire(stdout));
        let stderr = tokio::spawn(read_wire(stderr));
        let status = tokio::select! {
            _ = context.cancel.cancelled() => None,
            outcome = tokio::time::timeout(Duration::from_millis(invocation.timeout_ms), child.wait()) => {
                match outcome {
                    Ok(Ok(status)) => Some(Ok(status)),
                    Ok(Err(_)) => Some(Err("administrator supervisor wait failed")),
                    Err(_) => Some(Err("administrator command timed out; it may have changed system files")),
                }
            }
        };
        drop(lifetime);
        let Some(Ok(status)) = status else {
            // EOF lets the root supervisor kill its root-owned process group.
            // If authentication was still pending, it cannot launch a command
            // afterward because the supervisor checks EOF before spawning.
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            stdout.abort();
            stderr.abort();
            return match status {
                None => Err(AgentError::Cancelled),
                Some(Err(error)) => Err(AgentError::EffectUncertain(error.into())),
                Some(Ok(_)) => unreachable!(),
            };
        };
        let stdout = stdout
            .await
            .map_err(|_| AgentError::EffectUncertain("administrator output reader failed".into()))?
            .map_err(|_| {
                AgentError::EffectUncertain(
                    "administrator output exceeds its bounded protocol".into(),
                )
            })?;
        let _stderr = stderr
            .await
            .map_err(|_| AgentError::EffectUncertain("administrator error reader failed".into()))?
            .map_err(|_| {
                AgentError::EffectUncertain(
                    "administrator error output exceeds its bounded protocol".into(),
                )
            })?;
        if status.code() == Some(126) {
            return Ok(ToolOutput::error(
                "Administrator authentication was cancelled. The command was not executed.",
            ));
        }
        if status.code() == Some(127) {
            return Ok(ToolOutput::error("Polkit could not authorize this administrator command. Check the graphical authentication agent and system policy. The command was not executed."));
        }
        let result: WireResult = serde_json::from_slice(&stdout).map_err(|_| {
            AgentError::EffectUncertain(
                "administrator supervisor returned an invalid result; do not retry automatically"
                    .into(),
            )
        })?;
        if result.uncertain {
            return Err(AgentError::EffectUncertain(result.content));
        }
        Ok(ToolOutput {
            content: result.content,
            is_error: result.is_error,
        })
    }

    async fn read_wire<R: tokio::io::AsyncRead + Unpin>(mut reader: R) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let count = reader.read(&mut chunk).await?;
            if count == 0 {
                return Ok(bytes);
            }
            if bytes.len().saturating_add(count) > MAX_WIRE_BYTES {
                return Err(std::io::Error::other(
                    "bounded administrator output exceeded",
                ));
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
    }

    fn helper_invocation(arguments: &[String]) -> Result<(ToolCall, ToolContext), AgentError> {
        if arguments.len() < 3
            || arguments.len() > MAX_ARGUMENTS + 3
            || arguments.iter().map(String::len).sum::<usize>() > MAX_ARGUMENT_BYTES
        {
            return Err(invalid("invalid administrator helper argument bounds"));
        }
        let timeout_ms = arguments[0]
            .parse::<u64>()
            .ok()
            .filter(|value| (1_000..=120_000).contains(value))
            .ok_or_else(|| invalid("invalid administrator helper timeout"))?;
        let workspace = PathBuf::from(&arguments[1]);
        let program = PathBuf::from(&arguments[2]);
        if !workspace.is_absolute() || !program.is_absolute() || !executable(&program) {
            return Err(invalid(
                "administrator helper requires absolute workspace and executable paths",
            ));
        }
        Ok((
            ToolCall {
                id: "authenticated-administrator-command".into(),
                name: "run_command".into(),
                arguments: json!({"command":program,"args":&arguments[3..],"timeout_ms":timeout_ms}),
            },
            ToolContext {
                workspace,
                cancel: CancellationToken::new(),
            },
        ))
    }

    pub(super) fn helper_entry(arguments: &[String]) -> i32 {
        let result = helper_invocation(arguments).and_then(|(call, context)| {
            let cancel = context.cancel.clone();
            // poll(2) before spawn detects a lease already lost while the user
            // was answering the authentication dialog; no effects in that case.
            if !lease_is_live(0)? {
                return Err(AgentError::Cancelled);
            }
            std::thread::Builder::new()
                .name("administrator-lifetime".into())
                .spawn(move || {
                    let mut byte = [0u8; 1];
                    // Any input is a protocol violation; EOF is cancellation.
                    let _ = std::io::stdin().read(&mut byte);
                    cancel.cancel();
                })
                .map_err(|_| invalid("cannot supervise administrator lifetime lease"))?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| invalid("cannot initialize administrator command supervisor"))?;
            runtime.block_on(supervise(&call, &context))
        });
        let result = match result {
            Ok(output) => WireResult {
                content: output.content,
                is_error: output.is_error,
                uncertain: false,
            },
            Err(AgentError::EffectUncertain(message)) => WireResult {
                content: message,
                is_error: true,
                uncertain: true,
            },
            Err(error) => WireResult {
                content: error.to_string(),
                is_error: true,
                uncertain: false,
            },
        };
        let code = i32::from(result.is_error);
        if serde_json::to_writer(std::io::stdout().lock(), &result).is_err()
            || std::io::stdout().flush().is_err()
        {
            return 125;
        }
        code
    }

    fn lease_is_live(fd: std::os::fd::RawFd) -> Result<bool, AgentError> {
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        if unsafe { libc::poll(&mut descriptor, 1, 0) } < 0 {
            return Err(invalid("cannot inspect administrator lifetime lease"));
        }
        Ok(
            descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL | libc::POLLIN)
                == 0,
        )
    }

    async fn supervise(call: &ToolCall, context: &ToolContext) -> Result<ToolOutput, AgentError> {
        let timeout_ms = call.arguments["timeout_ms"]
            .as_u64()
            .ok_or_else(|| invalid("administrator supervisor timeout is missing"))?;
        // Bound the entire operation, including output-pipe completion. A
        // detached descendant can retain a pipe after the immediate child
        // exits; it must not retain the privileged supervisor indefinitely.
        tokio::select! {
            _ = context.cancel.cancelled() => Err(AgentError::Cancelled),
            result = tokio::time::timeout(
                Duration::from_millis(timeout_ms),
                crate::command::run_command_with_isolation(
                    call, context, crate::CommandIsolation::HostUnrestricted,
                ),
            ) => match result {
                Ok(result) => result,
                Err(_) => Err(AgentError::EffectUncertain(
                    "administrator supervisor timed out; system effects may already have occurred".into(),
                )),
            },
        }
    }

    #[cfg(test)]
    mod tests {
        include!("command_elevation_linux_tests.rs");
    }
}
