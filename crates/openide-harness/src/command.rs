use crate::tools::required_string;
use crate::{AgentError, ToolContext, ToolOutput};
use openide_core::ToolCall;
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) async fn run_command(
    call: &ToolCall,
    context: &ToolContext,
) -> Result<ToolOutput, AgentError> {
    let program = required_string(&call.arguments, "command")?;
    if program.trim().is_empty() {
        return Ok(ToolOutput::error("command is empty"));
    }
    let args = match call.arguments.get("args") {
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| AgentError::Tool("command args must be strings".into()))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Ok(ToolOutput::error("command args must be an array")),
        None => Vec::new(),
    };
    let timeout_ms = call
        .arguments
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1_000, MAX_TIMEOUT_MS);
    if context.cancel.is_cancelled() {
        return Err(AgentError::Cancelled);
    }
    let mut process = Command::new(program);
    process
        .args(args)
        .current_dir(&context.workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    process.creation_flags(CREATE_NO_WINDOW);
    // Model credentials are not made available to model-controlled commands.
    for key in [
        "OPENIDE_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "AZURE_OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
    ] {
        process.env_remove(key);
    }
    let mut child = process
        .spawn()
        .map_err(|error| AgentError::Tool(format!("cannot start command: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::Runtime("stdout pipe is unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AgentError::Runtime("stderr pipe is unavailable".into()))?;
    let stdout_task = tokio::spawn(read_capped(stdout));
    let stderr_task = tokio::spawn(read_capped(stderr));
    let status = tokio::select! {
        _ = context.cancel.cancelled() => return Err(AgentError::Cancelled),
        result = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()) => {
            match result {
                Ok(Ok(status)) => status,
                Ok(Err(error)) => return Err(AgentError::Tool(format!("command wait failed: {error}"))),
                Err(_) => return Ok(ToolOutput::error(format!("command timed out after {timeout_ms} ms"))),
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_task
        .await
        .map_err(|error| AgentError::Runtime(format!("stdout reader failed: {error}")))?
        .map_err(|error| AgentError::Tool(format!("stdout read failed: {error}")))?;
    let (stderr, stderr_truncated) = stderr_task
        .await
        .map_err(|error| AgentError::Runtime(format!("stderr reader failed: {error}")))?
        .map_err(|error| AgentError::Tool(format!("stderr read failed: {error}")))?;
    let code = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".into());
    let mut content = format!("exit code: {code}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    if stdout_truncated || stderr_truncated {
        content.push_str("\n[command output truncated]");
    }
    Ok(ToolOutput {
        content,
        is_error: !status.success(),
    })
}

async fn read_capped<R: AsyncRead + Unpin>(mut reader: R) -> std::io::Result<(String, bool)> {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn runs_argument_array_without_shell_expansion() {
        let dir = tempfile::tempdir().unwrap();
        let (program, args) = if cfg!(windows) {
            ("cmd.exe", vec!["/C", "echo", "hello"])
        } else {
            ("echo", vec!["hello"])
        };
        let call = ToolCall {
            id: "command-1".into(),
            name: "run_command".into(),
            arguments: json!({"command":program,"args":args}),
        };
        let output = run_command(
            &call,
            &ToolContext {
                workspace: dir.path().to_path_buf(),
                cancel: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("hello"));
    }
}
