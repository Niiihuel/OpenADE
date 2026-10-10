use crate::tools::required_string;
use crate::{AgentError, ToolContext, ToolOutput};
use openide_core::ToolCall;
use serde_json::Value;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

#[path = "command_sandbox.rs"]
pub(crate) mod sandbox;
pub use sandbox::CommandIsolation;

#[cfg(unix)]
use unix_group::ProcessTreeGuard;
#[cfg(windows)]
use windows_job::ProcessTreeGuard;

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
// Only process bootstrap, locale, and development-tool locations. Do not add
// API keys, auth sockets, cloud credentials, proxy credentials, or arbitrary
// user-provided variables here; the model controls the executable and argv.
const HOST_BOOTSTRAP_ENV_KEYS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "COMSPEC",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "HOME",
    "USER",
    "LOGNAME",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "TEMP",
    "TMP",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TZ",
    "TERM",
    "COLORTERM",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "RUSTUP_HOME",
    "GOPATH",
    "GOROOT",
    "JAVA_HOME",
    "GRADLE_USER_HOME",
    "MAVEN_HOME",
    "NODE_PATH",
    "NPM_CONFIG_PREFIX",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "DOTNET_ROOT",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
];
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(all(test, windows))]
pub(crate) fn build_sandbox_probe(workspace: &std::path::Path) -> std::path::PathBuf {
    let source = workspace.join("sandbox_probe.rs");
    let binary = workspace.join("sandbox_probe.exe");
    std::fs::write(&source, include_str!("../tests/fixtures/sandbox_probe.rs")).unwrap();
    let target = if cfg!(target_env = "gnu") {
        "x86_64-pc-windows-gnu"
    } else {
        "x86_64-pc-windows-msvc"
    };
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = std::process::Command::new(compiler)
        .arg("--edition=2021")
        .arg("--target")
        .arg(target)
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "probe compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

#[cfg(test)]
pub(crate) async fn run_command(
    call: &ToolCall,
    context: &ToolContext,
) -> Result<ToolOutput, AgentError> {
    run_command_with_isolation(call, context, CommandIsolation::HostUnrestricted).await
}

/// The isolation choice comes from trusted host policy, never model-supplied
/// arguments. A sandbox request never falls back to an unrestricted process.
pub(crate) async fn run_command_with_isolation(
    call: &ToolCall,
    context: &ToolContext,
    isolation: CommandIsolation,
) -> Result<ToolOutput, AgentError> {
    run_command_with_policy(call, context, isolation, false).await
}

pub(crate) async fn run_command_with_policy(
    call: &ToolCall,
    context: &ToolContext,
    isolation: CommandIsolation,
    allow_elevated_commands: bool,
) -> Result<ToolOutput, AgentError> {
    run_command_streaming(call, context, isolation, allow_elevated_commands, None).await
}

pub(crate) async fn run_command_streaming(
    call: &ToolCall,
    context: &ToolContext,
    isolation: CommandIsolation,
    allow_elevated_commands: bool,
    output: Option<crate::ToolOutputSender>,
) -> Result<ToolOutput, AgentError> {
    match call.arguments.get("elevated") {
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => {
            if !allow_elevated_commands {
                return Err(AgentError::Tool(
                    "administrator command access is disabled".into(),
                ));
            }
            if isolation != CommandIsolation::HostUnrestricted {
                return Err(AgentError::Tool(
                    "administrator commands cannot run inside the workspace sandbox".into(),
                ));
            }
            return crate::command_elevation::run(call, context).await;
        }
        Some(_) => return Err(AgentError::Tool("elevated must be boolean".into())),
    }
    sandbox::require_available(isolation)?;
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
    #[cfg(windows)]
    if isolation == CommandIsolation::WorkspaceWriteOffline {
        return sandbox::run_workspace_write_offline(program, &args, timeout_ms, context).await;
    }
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    if isolation == CommandIsolation::WorkspaceWriteOffline {
        return sandbox::linux::run_streaming(program, &args, timeout_ms, context, output).await;
    }
    let mut process = Command::new(program);
    process
        .args(args)
        .current_dir(&context.workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // HostUnrestricted describes filesystem/network access, not permission to
    // expose the host's ambient credentials to model-controlled processes.
    // Commands needing extra environment must receive it through a future
    // trusted, explicit configuration rather than inheriting it implicitly.
    process.env_clear();
    for key in HOST_BOOTSTRAP_ENV_KEYS {
        if let Some(value) = std::env::var_os(key) {
            process.env(key, value);
        }
    }
    #[cfg(windows)]
    process.creation_flags(CREATE_NO_WINDOW | windows_job::CREATE_SUSPENDED);
    #[cfg(unix)]
    process.process_group(0);
    // Process-tree cleanup is not a filesystem or network sandbox.
    #[cfg(windows)]
    let tree = ProcessTreeGuard::new()
        .map_err(|error| AgentError::Tool(format!("cannot create command job: {error}")))?;
    let mut child = process
        .spawn()
        .map_err(|error| AgentError::Tool(format!("cannot start command: {error}")))?;
    #[cfg(windows)]
    tree.assign_and_resume(&child).map_err(|error| {
        AgentError::EffectUncertain(format!("cannot contain command tree after spawn: {error}"))
    })?;
    #[cfg(unix)]
    let tree = ProcessTreeGuard::new(child.id()).map_err(|error| {
        AgentError::EffectUncertain(format!("cannot contain command tree after spawn: {error}"))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        AgentError::EffectUncertain("command stdout pipe is unavailable after spawn".into())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        AgentError::EffectUncertain("command stderr pipe is unavailable after spawn".into())
    })?;
    let stdout_task = tokio::spawn(read_capped_streaming(
        stdout,
        output.clone(),
        openide_core::OutputStream::Stdout,
    ));
    let stderr_task = tokio::spawn(read_capped_streaming(
        stderr,
        output,
        openide_core::OutputStream::Stderr,
    ));
    let outcome = tokio::select! {
        _ = context.cancel.cancelled() => WaitOutcome::Cancelled,
        result = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()) => {
            match result {
                Ok(Ok(status)) => WaitOutcome::Exited(status),
                Ok(Err(error)) => WaitOutcome::WaitError(error),
                Err(_) => WaitOutcome::TimedOut,
            }
        }
    };
    // Closing the job (Windows) or killing the process group (Unix) also
    // closes pipes inherited by descendants before the readers are awaited.
    drop(tree);
    let status = match outcome {
        WaitOutcome::Exited(status) => status,
        other => {
            stdout_task.abort();
            stderr_task.abort();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            return match other {
                WaitOutcome::Cancelled => Err(AgentError::Cancelled),
                WaitOutcome::TimedOut => Err(AgentError::EffectUncertain(format!(
                    "command timed out after {timeout_ms} ms; it may have changed files before termination"
                ))),
                WaitOutcome::WaitError(error) => Err(AgentError::EffectUncertain(format!(
                    "command wait failed after spawn: {error}"
                ))),
                WaitOutcome::Exited(_) => unreachable!(),
            };
        }
    };
    let (stdout, stdout_truncated) = stdout_task
        .await
        .map_err(|error| {
            AgentError::EffectUncertain(format!("command stdout reader failed: {error}"))
        })?
        .map_err(|error| {
            AgentError::EffectUncertain(format!("command stdout read failed: {error}"))
        })?;
    let (stderr, stderr_truncated) = stderr_task
        .await
        .map_err(|error| {
            AgentError::EffectUncertain(format!("command stderr reader failed: {error}"))
        })?
        .map_err(|error| {
            AgentError::EffectUncertain(format!("command stderr read failed: {error}"))
        })?;
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

enum WaitOutcome {
    Exited(ExitStatus),
    Cancelled,
    TimedOut,
    WaitError(std::io::Error),
}

async fn read_capped<R: AsyncRead + Unpin>(mut reader: R) -> std::io::Result<(String, bool)> {
    read_capped_streaming(&mut reader, None, openide_core::OutputStream::Stdout).await
}

pub(crate) async fn read_capped_streaming<R: AsyncRead + Unpin>(
    mut reader: R,
    output: Option<crate::ToolOutputSender>,
    stream: openide_core::OutputStream,
) -> std::io::Result<(String, bool)> {
    // The end of a long output usually carries the failure, so the capture
    // keeps both ends and drops the middle. The live stream still shows only
    // the beginning; the finished tool result carries the tail.
    const HEAD_BYTES: usize = MAX_OUTPUT_BYTES / 2;
    const TAIL_BYTES: usize = MAX_OUTPUT_BYTES - HEAD_BYTES;
    let mut head = Vec::new();
    let mut tail = std::collections::VecDeque::new();
    let mut omitted = 0usize;
    let mut streamed = 0usize;
    let mut truncated = false;
    let mut decoder = crate::tool_output::OutputDecoder::default();
    let mut chunk = [0u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        let head_take = count.min(HEAD_BYTES - head.len());
        head.extend_from_slice(&chunk[..head_take]);
        tail.extend(&chunk[head_take..count]);
        let excess = tail.len().saturating_sub(TAIL_BYTES);
        tail.drain(..excess);
        omitted += excess;
        let keep = count.min(MAX_OUTPUT_BYTES - streamed);
        streamed += keep;
        if let Some(output) = &output {
            let text = decoder.decode(&chunk[..keep], false);
            output
                .send(stream, &text)
                .await
                .map_err(std::io::Error::other)?;
            if keep < count && !truncated {
                let tail = decoder.decode(&[], true);
                output
                    .send(stream, &format!("{tail}\n[command output truncated]\n"))
                    .await
                    .map_err(std::io::Error::other)?;
            }
        }
        truncated |= keep < count;
    }
    if let Some(output) = &output {
        output
            .send(stream, &decoder.decode(&[], true))
            .await
            .map_err(std::io::Error::other)?;
    }
    let mut text = String::from_utf8_lossy(&head).into_owned();
    if omitted > 0 {
        text.push_str(&format!("\n[... {omitted} bytes omitted ...]\n"));
    }
    text.push_str(&String::from_utf8_lossy(tail.make_contiguous()));
    Ok((crate::output_condense::condense_output(&text), truncated))
}

#[cfg(unix)]
mod unix_group {
    use std::io;

    pub(super) struct ProcessTreeGuard {
        pgid: i32,
    }

    impl ProcessTreeGuard {
        pub(super) fn new(pid: Option<u32>) -> io::Result<Self> {
            let pid = pid.ok_or_else(|| io::Error::other("command PID is unavailable"))?;
            let pgid = i32::try_from(pid)
                .map_err(|_| io::Error::other("command PID exceeds process-group range"))?;
            Ok(Self { pgid })
        }
    }

    impl Drop for ProcessTreeGuard {
        fn drop(&mut self) {
            // The command was started in a fresh process group. A descendant
            // which deliberately creates another session can escape it.
            unsafe {
                libc::kill(-self.pgid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
mod windows_job {
    use std::io;
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, ResumeThread, CREATE_SUSPENDED as WIN_CREATE_SUSPENDED,
        PROCESS_SET_QUOTA, PROCESS_TERMINATE, THREAD_SUSPEND_RESUME,
    };

    pub(super) const CREATE_SUSPENDED: u32 = WIN_CREATE_SUSPENDED;

    pub(super) struct ProcessTreeGuard {
        // OwnedHandle closes the Job Object on every return path, including
        // future cancellation and process exit. KILL_ON_JOB_CLOSE ends the
        // whole job rather than only the immediate child.
        job: OwnedHandle,
    }

    impl ProcessTreeGuard {
        pub(super) fn new() -> io::Result<Self> {
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            let job = owned_handle(raw)?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let success = unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if success == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { job })
        }

        pub(super) fn assign_and_resume(&self, child: &tokio::process::Child) -> io::Result<()> {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("command PID is unavailable"))?;
            let process = owned_handle(unsafe {
                OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid)
            })?;
            let success = unsafe {
                AssignProcessToJobObject(self.job.as_raw_handle(), process.as_raw_handle())
            };
            if success == 0 {
                return Err(io::Error::last_os_error());
            }
            // CREATE_SUSPENDED prevents the process from creating descendants
            // before its Job Object assignment. Resume only after assignment.
            resume_primary_thread(pid)
        }
    }

    fn resume_primary_thread(pid: u32) -> io::Result<()> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = owned_handle(snapshot)?;
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = owned_handle(unsafe {
                    OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID)
                })?;
                let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
                if previous == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                if previous != 1 {
                    return Err(io::Error::other(
                        "command primary thread had an unexpected suspend count",
                    ));
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        Err(io::Error::other("command primary thread was not found"))
    }

    fn owned_handle(raw: HANDLE) -> io::Result<OwnedHandle> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capture_keeps_both_ends_of_long_output() {
        let short = b"all of it".to_vec();
        assert_eq!(
            read_capped(short.as_slice()).await.unwrap(),
            ("all of it".to_owned(), false)
        );

        let mut long = b"START".to_vec();
        long.extend(std::iter::repeat_n(b'.', MAX_OUTPUT_BYTES * 3));
        long.extend_from_slice(b"error: the real failure");
        let (text, truncated) = read_capped(long.as_slice()).await.unwrap();
        assert!(truncated);
        assert!(text.starts_with("START"));
        assert!(text.ends_with("error: the real failure"));
        let omitted = long.len() - MAX_OUTPUT_BYTES;
        assert!(text.contains(&format!("[... {omitted} bytes omitted ...]")));
        assert!(text.len() < MAX_OUTPUT_BYTES + 64);
    }
    use serde_json::json;
    use std::path::Path;
    use std::time::Instant;
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

    #[tokio::test]
    async fn unrestricted_command_receives_bootstrap_paths_but_no_ambient_secret() {
        let dir = tempfile::tempdir().unwrap();
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe",
                vec![
                    "/C",
                    "if defined OPENIDE_HOST_SECRET_CANARY (echo leaked) else (echo absent) & if defined PATH (echo path-present)",
                ],
            )
        } else {
            (
                "sh",
                vec![
                    "-c",
                    "if [ -n \"$OPENIDE_HOST_SECRET_CANARY\" ]; then echo leaked; else echo absent; fi; if [ -n \"$PATH\" ]; then echo path-present; fi",
                ],
            )
        };
        let call = ToolCall {
            id: "host-environment".into(),
            name: "run_command".into(),
            arguments: json!({"command": program, "args": args}),
        };
        std::env::set_var("OPENIDE_HOST_SECRET_CANARY", "must-not-reach-child");
        let result = run_command(
            &call,
            &ToolContext {
                workspace: dir.path().to_path_buf(),
                cancel: CancellationToken::new(),
            },
        )
        .await;
        std::env::remove_var("OPENIDE_HOST_SECRET_CANARY");
        let output = result.unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("absent"), "{}", output.content);
        assert!(
            output.content.contains("path-present"),
            "{}",
            output.content
        );
        assert!(!output.content.contains("leaked"), "{}", output.content);
    }

    #[tokio::test]
    async fn cancellation_ends_a_spawned_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let running = spawn_tree_command(dir.path(), cancel.clone(), 30_000);
        let started = wait_for_child_start(dir.path()).await;
        cancel.cancel();
        let result = running.await.unwrap();
        assert!(matches!(result, Err(AgentError::Cancelled)));
        assert_no_descendant_survival(dir.path(), started).await;
    }

    #[tokio::test]
    async fn timeout_ends_a_spawned_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let running = spawn_tree_command(dir.path(), CancellationToken::new(), 3_000);
        let started = wait_for_child_start(dir.path()).await;
        let result = running.await.unwrap();
        assert!(
            matches!(result, Err(AgentError::EffectUncertain(message)) if message.contains("timed out"))
        );
        assert!(dir.path().join("child-started").exists());
        assert_no_descendant_survival(dir.path(), started).await;
    }

    #[cfg(not(any(
        windows,
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )))]
    #[tokio::test]
    async fn unavailable_workspace_offline_profile_refuses_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let call = ToolCall {
            id: "sandbox-probe".into(),
            name: "run_command".into(),
            arguments: json!({"command":"echo","args":["hello"]}),
        };
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let result =
            run_command_with_isolation(&call, &context, CommandIsolation::WorkspaceWriteOffline)
                .await;
        assert!(
            matches!(result, Err(AgentError::Tool(message)) if message.contains("unavailable"))
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn workspace_offline_profile_enforces_filesystem_junction_and_network() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let probe = build_sandbox_probe(&workspace);
        let junction = workspace.join("outside-link");
        let create_junction = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:OPENIDE_TEST_LINK -Target $env:OPENIDE_TEST_TARGET | Out-Null",
            ])
            .env("OPENIDE_TEST_LINK", &junction)
            .env("OPENIDE_TEST_TARGET", &outside)
            .output()
            .unwrap();
        assert!(
            create_junction.status.success(),
            "junction setup failed for {} -> {}: stdout={} stderr={}",
            junction.display(),
            outside.display(),
            String::from_utf8_lossy(&create_junction.stdout),
            String::from_utf8_lossy(&create_junction.stderr)
        );
        let inside_file = workspace.join("inside.txt");
        let outside_file = outside.join("outside.txt");
        let linked_file = junction.join("linked.txt");
        let outside_secret = outside.join("secret.txt");
        std::fs::write(&outside_secret, b"top secret test value").unwrap();
        let linked_secret = junction.join("secret.txt");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let acl_before = workspace_acl_snapshot(&workspace);
        let call = ToolCall {
            id: "sandbox-permissions".into(),
            name: "run_command".into(),
            arguments: json!({
                "command": probe,
                "args": [inside_file, outside_file, listener.local_addr().unwrap().port().to_string(), linked_file, outside_secret, linked_secret],
            }),
        };
        std::env::set_var("OPENIDE_SANDBOX_SECRET_CANARY", "must-stay-in-host");
        let output = run_command_with_isolation(
            &call,
            &ToolContext {
                workspace: workspace.clone(),
                cancel: CancellationToken::new(),
            },
            CommandIsolation::WorkspaceWriteOffline,
        )
        .await;
        std::env::remove_var("OPENIDE_SANDBOX_SECRET_CANARY");
        let output = output.unwrap();
        assert_eq!(workspace_acl_snapshot(&workspace), acl_before);
        assert!(!output.is_error, "{}", output.content);
        assert!(
            output.content.contains("inside=Ok(())"),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("outside=Err("),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("through_link=Err("),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("outside_read=Err("),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("through_link_read=Err("),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("network_denied=true"),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("credential_present=false"),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("secret_canary_present=false"),
            "{}",
            output.content
        );
        assert!(inside_file.exists());
        assert!(!outside_file.exists());
        assert!(!outside.join("linked.txt").exists());
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn workspace_offline_cancellation_ends_descendant_before_delayed_write() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let probe = build_sandbox_probe(&workspace);
        let acl_before = workspace_acl_snapshot(&workspace);
        let started = workspace.join("sandbox-child-started");
        let survived = workspace.join("sandbox-child-survived");
        let call = ToolCall {
            id: "sandbox-tree".into(),
            name: "run_command".into(),
            arguments: json!({
                "command": probe,
                "args": ["--spawn-child", started, survived],
                "timeout_ms": 20_000,
            }),
        };
        let cancel = CancellationToken::new();
        let context = ToolContext {
            workspace: workspace.clone(),
            cancel: cancel.clone(),
        };
        let mut running = tokio::spawn(async move {
            run_command_with_isolation(&call, &context, CommandIsolation::WorkspaceWriteOffline)
                .await
        });
        let wait_for_start = tokio::time::timeout(Duration::from_secs(8), async {
            while !workspace.join("sandbox-child-started").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        tokio::select! {
            started = wait_for_start => started.expect("sandbox descendant did not start"),
            result = &mut running => panic!("sandbox exited before descendant started: {result:?}"),
        }
        cancel.cancel();
        let result = running.await.unwrap();
        assert!(matches!(result, Err(AgentError::Cancelled)), "{result:?}");
        assert_eq!(workspace_acl_snapshot(&workspace), acl_before);
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!workspace.join("sandbox-child-survived").exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn workspace_offline_timeout_preserves_uncertain_prior_effect() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let probe = build_sandbox_probe(&workspace);
        let acl_before = workspace_acl_snapshot(&workspace);
        let started = workspace.join("written-before-timeout");
        let survived = workspace.join("written-after-timeout");
        let call = ToolCall {
            id: "sandbox-timeout".into(),
            name: "run_command".into(),
            arguments: json!({
                "command": probe,
                "args": ["--delayed-write", started, survived],
                "timeout_ms": 1_000,
            }),
        };
        let result = run_command_with_isolation(
            &call,
            &ToolContext {
                workspace: workspace.clone(),
                cancel: CancellationToken::new(),
            },
            CommandIsolation::WorkspaceWriteOffline,
        )
        .await;
        assert!(
            matches!(&result, Err(AgentError::EffectUncertain(message)) if message.contains("timed out")),
            "{result:?}"
        );
        assert!(workspace.join("written-before-timeout").exists());
        assert_eq!(workspace_acl_snapshot(&workspace), acl_before);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(!workspace.join("written-after-timeout").exists());
    }

    #[cfg(windows)]
    fn workspace_acl_snapshot(workspace: &Path) -> String {
        let icacls = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("icacls.exe");
        let output = std::process::Command::new(icacls)
            .arg(workspace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "ACL query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn spawn_tree_command(
        workspace: &Path,
        cancel: CancellationToken,
        timeout_ms: u64,
    ) -> tokio::task::JoinHandle<Result<ToolOutput, AgentError>> {
        std::fs::write(workspace.join(".openide-tree-test-armed"), b"armed").unwrap();
        let call = ToolCall {
            id: "command-tree".into(),
            name: "run_command".into(),
            arguments: json!({
                "command": std::env::current_exe().unwrap(),
                "args": ["--ignored", "--exact", "command::tests::tree_parent_helper"],
                "timeout_ms": timeout_ms,
            }),
        };
        let context = ToolContext {
            workspace: workspace.to_path_buf(),
            cancel,
        };
        tokio::spawn(async move { run_command(&call, &context).await })
    }

    async fn wait_for_child_start(workspace: &Path) -> Instant {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                if workspace.join("child-started").exists() {
                    return Instant::now();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("descendant should start before the command is stopped")
    }

    async fn assert_no_descendant_survival(workspace: &Path, started: Instant) {
        let deadline = started + Duration::from_millis(5_500);
        if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            tokio::time::sleep(remaining).await;
        }
        assert!(
            !workspace.join("child-survived").exists(),
            "descendant survived process-tree cleanup"
        );
    }

    #[test]
    #[ignore = "spawned only by the process-tree tests"]
    fn tree_parent_helper() {
        if !std::env::current_dir()
            .unwrap()
            .join(".openide-tree-test-armed")
            .exists()
        {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let mut descendant = std::process::Command::new(exe)
            .args(["--ignored", "--exact", "command::tests::tree_child_helper"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            assert!(
                descendant.try_wait().unwrap().is_none(),
                "descendant exited before its parent"
            );
        }
    }

    #[test]
    #[ignore = "spawned only by the process-tree tests"]
    fn tree_child_helper() {
        let workspace = std::env::current_dir().unwrap();
        if !workspace.join(".openide-tree-test-armed").exists() {
            return;
        }
        std::fs::write(workspace.join("child-started"), b"started").unwrap();
        std::thread::sleep(Duration::from_secs(5));
        std::fs::write(workspace.join("child-survived"), b"survived").unwrap();
    }
}
