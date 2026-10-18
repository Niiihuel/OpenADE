use crate::error::{ApiError, ApiResult};
use serde_json::{json, Value};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn read_diff(workspace: &Path) -> ApiResult<Value> {
    // Git receives no user-controlled arguments. Disable external diff drivers,
    // text conversion and pagers; cap both retained output and elapsed time.
    let mut child = Command::new("git")
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.external=",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "HEAD",
            "--",
        ])
        .current_dir(workspace)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ApiError::bad("Git is unavailable on the host"))?;
    let stdout = child.stdout.take().ok_or_else(ApiError::unavailable)?;
    let reader = std::thread::spawn(move || {
        let mut reader = stdout;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut truncated = false;
        while let Ok(count) = reader.read(&mut chunk) {
            if count == 0 {
                break;
            }
            let keep = count.min((512 * 1024usize).saturating_sub(bytes.len()));
            bytes.extend_from_slice(&chunk[..keep]);
            truncated |= keep < count;
        }
        (bytes, truncated)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(ApiError::bad("Git diff timed out"));
            }
        }
    };
    let (bytes, truncated) = reader.join().map_err(|_| ApiError::unavailable())?;
    let status = result?;
    if !status.success() {
        return Err(ApiError::bad(
            "Diff requires a Git repository with a HEAD commit",
        ));
    }
    Ok(
        json!({"diff":String::from_utf8_lossy(&bytes),"truncated":truncated,"includes_untracked":false}),
    )
}
