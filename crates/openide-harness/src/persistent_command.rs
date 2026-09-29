//! Bounded, interactive command sessions owned by one manager.
//!
//! This is a transport/lifetime primitive, not a model-facing tool. The host
//! must authorize the command and choose isolation before calling `start`.
//! Sessions survive individual tool-call cancellation, but dropping the manager
//! cancels every session it owns. Output is always drained from both pipes;
//! omitted bytes are reported in order rather than silently discarded.

use crate::command::CommandIsolation;
use crate::process_tree::ProcessTreeGuard;
use crate::AgentError;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time::{sleep, timeout, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[cfg(windows)]
#[path = "persistent_command_pty.rs"]
mod windows_pty;
#[cfg(windows)]
#[path = "persistent_command_sandbox.rs"]
mod windows_sandbox;

const MAX_COMMAND_BYTES: usize = 32 * 1024;
const MAX_ARGUMENTS: usize = 256;
const MAX_BUFFER_EVENTS: usize = 1024;
const MAX_POLL_EVENTS: usize = 128;
const READER_CHUNK_BYTES: usize = 8192;
const MAX_POLL_WAIT: Duration = Duration::from_secs(30);
const IO_WAIT: Duration = Duration::from_secs(5);
const READER_DRAIN_WAIT: Duration = Duration::from_secs(5);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Mirrors `run_command`'s explicit bootstrap environment. No ambient secret
/// variables are passed into a model-controlled command.
const HOST_BOOTSTRAP_ENV_KEYS: &[&str] = &[
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

#[derive(Clone, Copy, Debug)]
pub struct PersistentCommandLimits {
    pub max_sessions: usize,
    pub max_lifetime: Duration,
    pub max_buffered_output_bytes: usize,
    pub max_poll_output_bytes: usize,
    pub max_stdin_bytes: usize,
}

impl Default for PersistentCommandLimits {
    fn default() -> Self {
        Self {
            max_sessions: 8,
            max_lifetime: Duration::from_secs(10 * 60),
            max_buffered_output_bytes: 512 * 1024,
            max_poll_output_bytes: 64 * 1024,
            max_stdin_bytes: 8 * 1024,
        }
    }
}

impl PersistentCommandLimits {
    fn validate(self) -> Result<Self, AgentError> {
        if !(1..=16).contains(&self.max_sessions)
            || !(Duration::from_millis(100)..=Duration::from_secs(60 * 60))
                .contains(&self.max_lifetime)
            || !(4 * 1024..=1024 * 1024).contains(&self.max_buffered_output_bytes)
            || !(1024..=64 * 1024).contains(&self.max_poll_output_bytes)
            || self.max_poll_output_bytes > self.max_buffered_output_bytes
            || !(1..=8 * 1024).contains(&self.max_stdin_bytes)
        {
            return Err(AgentError::Tool("invalid persistent command limits".into()));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistentCommandStream {
    Stdout,
    Stderr,
    /// ConPTY combines stdout and stderr into one terminal stream.
    Terminal,
}

#[derive(Debug, Eq, PartialEq)]
pub enum PersistentCommandEvent {
    Output {
        stream: PersistentCommandStream,
        bytes: Vec<u8>,
    },
    /// Some bytes could not be retained within the configured memory limit.
    /// The marker stays in sequence with later output.
    Omitted { bytes: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistentCommandExit {
    Exited { code: Option<i32> },
    Terminated,
    TimedOut,
    WaitFailed(String),
}

#[derive(Debug, Eq, PartialEq)]
pub struct PersistentCommandStarted {
    pub id: String,
}

#[derive(Debug, Eq, PartialEq)]
pub struct PersistentCommandSnapshot {
    pub id: String,
    pub events: Vec<PersistentCommandEvent>,
    pub exit: Option<PersistentCommandExit>,
    pub reader_error: Option<String>,
    pub has_more_output: bool,
    /// True only when the process is closed and all output was drained. The ID
    /// is removed after this snapshot, so the terminal result is delivered once.
    pub finished: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentCommandSessionInfo {
    pub id: String,
    pub pty: bool,
    pub exit: Option<PersistentCommandExit>,
    pub has_output: bool,
}

struct Session {
    stdin: Arc<AsyncMutex<Option<Box<dyn AsyncWrite + Send + Unpin>>>>,
    output: Arc<OutputBuffer>,
    stop: CancellationToken,
    tree: Arc<Mutex<Option<SessionTree>>>,
    poll_lock: AsyncMutex<()>,
    pty: bool,
    #[cfg(windows)]
    pty_console: Option<Arc<Mutex<Option<windows_pty::PseudoConsole>>>>,
    #[cfg(windows)]
    sandbox_stdin: Option<Arc<AsyncMutex<Option<windows_sandbox::InputSender>>>>,
}

enum SessionTree {
    Pipes(ProcessTreeGuard),
    #[cfg(windows)]
    Pty(windows_pty::PtyJob),
    #[cfg(windows)]
    Sandbox(Arc<windows_sandbox::SandboxRuntime>),
}

impl SessionTree {
    fn close(self) {
        match self {
            Self::Pipes(tree) => drop(tree),
            #[cfg(windows)]
            Self::Pty(job) => drop(job),
            #[cfg(windows)]
            Self::Sandbox(runtime) => runtime.kill(),
        }
    }
}

impl Session {
    fn stop_tree(&self) {
        self.stop.cancel();
        let mut tree = self
            .tree
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(tree) = tree.take() {
            tree.close();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop_tree();
    }
}

pub struct PersistentCommandManager {
    workspace: PathBuf,
    isolation: CommandIsolation,
    limits: PersistentCommandLimits,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    closed: std::sync::atomic::AtomicBool,
    pty_tools_enabled: bool,
    pty_resize_tools_enabled: bool,
}

impl Drop for PersistentCommandManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}

impl PersistentCommandManager {
    pub fn new(
        workspace: PathBuf,
        isolation: CommandIsolation,
        limits: PersistentCommandLimits,
    ) -> Result<Self, AgentError> {
        let limits = limits.validate()?;
        let workspace = std::fs::canonicalize(&workspace).map_err(|error| {
            AgentError::Tool(format!("cannot resolve command workspace: {error}"))
        })?;
        if !workspace.is_dir() {
            return Err(AgentError::Tool(
                "command workspace must be an existing directory".into(),
            ));
        }
        Ok(Self {
            workspace,
            isolation,
            limits,
            sessions: Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            pty_tools_enabled: false,
            pty_resize_tools_enabled: false,
        })
    }

    /// Enable the PTY form of `start_command` for sessions created by this
    /// manager. Call before sharing the manager or constructing a tool catalog.
    /// The default preserves the exact legacy tool catalog for resume.
    pub fn with_pty_tools(mut self) -> Self {
        self.pty_tools_enabled = true;
        self
    }

    /// Advertise `resize_command` only for newly constructed tool catalogs.
    /// `with_pty_tools()` alone retains the exact previous four-tool catalog
    /// so interrupted turns can select its saved digest during recovery.
    pub fn with_pty_resize_tools(mut self) -> Self {
        self.pty_resize_tools_enabled = true;
        self
    }

    pub fn pty_tools_enabled(&self) -> bool {
        self.pty_tools_enabled
            && self.isolation == CommandIsolation::HostUnrestricted
            && cfg!(windows)
    }

    pub fn pty_resize_tools_enabled(&self) -> bool {
        self.pty_resize_tools_enabled && self.pty_tools_enabled()
    }

    /// Exact pre-PTY definition for selecting the recorded catalog during a
    /// resumed turn. The host must also reject `pty` arguments for that turn.
    pub fn legacy_start_command_definition() -> crate::ToolDefinition {
        crate::tools::legacy_start_command_definition()
    }

    pub fn workspace(&self) -> &std::path::Path {
        &self.workspace
    }

    pub fn isolation(&self) -> CommandIsolation {
        self.isolation
    }

    pub fn poll_output_limit(&self) -> usize {
        self.limits.max_poll_output_bytes
    }

    /// Read session status without draining output or consuming a final poll.
    pub fn list_sessions(&self) -> Result<Vec<PersistentCommandSessionInfo>, AgentError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| AgentError::Tool("persistent command registry is unavailable".into()))?
            .iter()
            .map(|(id, session)| (id.clone(), Arc::clone(session)))
            .collect::<Vec<_>>();
        let mut listed = Vec::with_capacity(sessions.len());
        for (id, session) in sessions {
            let state = session.output.state.lock().map_err(|_| {
                AgentError::EffectUncertain("persistent command output state is unavailable".into())
            })?;
            listed.push(PersistentCommandSessionInfo {
                id,
                pty: session.pty,
                exit: state.exit.clone(),
                has_output: !state.events.is_empty(),
            });
        }
        listed.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(listed)
    }

    /// Revoke the manager and synchronously close every process tree it owns.
    /// Existing session IDs can still be polled to deliver buffered output and
    /// their terminal state, but no new commands can be started.
    pub fn stop_all(&self) {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        for session in sessions.values() {
            session.stop_tree();
        }
    }

    /// Start one literal program and argument array in the fixed workspace.
    /// The registry lock is acquired before spawn, so cancellation of a caller
    /// awaiting a separate operation cannot leave an unregistered process.
    pub fn start(
        &self,
        program: &str,
        args: &[String],
        cancellation: &CancellationToken,
    ) -> Result<PersistentCommandStarted, AgentError> {
        self.start_with_terminal(program, args, None, cancellation)
    }

    /// Opt in to a terminal-backed command. The terminal is a merged output
    /// stream. Unsupported hosts reject the request before spawning the command.
    pub fn start_pty(
        &self,
        program: &str,
        args: &[String],
        columns: u16,
        rows: u16,
        cancellation: &CancellationToken,
    ) -> Result<PersistentCommandStarted, AgentError> {
        if !self.pty_tools_enabled() {
            return Err(AgentError::Tool(
                "persistent PTY commands are not enabled for this manager".into(),
            ));
        }
        if !(20..=500).contains(&columns) || !(5..=200).contains(&rows) {
            return Err(AgentError::Tool("invalid PTY terminal dimensions".into()));
        }
        self.start_with_terminal(program, args, Some((columns, rows)), cancellation)
    }

    fn start_with_terminal(
        &self,
        program: &str,
        args: &[String],
        terminal: Option<(u16, u16)>,
        cancellation: &CancellationToken,
    ) -> Result<PersistentCommandStarted, AgentError> {
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if self.isolation == CommandIsolation::WorkspaceWriteOffline && terminal.is_some() {
            return Err(AgentError::Tool(
                "PTY is unavailable in workspace-write/offline isolation; command was not started"
                    .into(),
            ));
        }
        if self.isolation == CommandIsolation::WorkspaceWriteOffline && !cfg!(windows) {
            return Err(AgentError::Tool(
                "persistent workspace-write/offline command isolation is unavailable on this host; command was not started".into(),
            ));
        }
        if program.trim().is_empty()
            || program.contains('\0')
            || args.len() > MAX_ARGUMENTS
            || args.iter().any(|arg| arg.contains('\0'))
            || args
                .iter()
                .fold(program.len(), |sum, arg| sum.saturating_add(arg.len()))
                > MAX_COMMAND_BYTES
        {
            return Err(AgentError::Tool(
                "invalid or oversized persistent command".into(),
            ));
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| AgentError::Tool("persistent commands need a Tokio runtime".into()))?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AgentError::Tool("persistent command registry is unavailable".into()))?;
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(AgentError::Tool(
                "persistent command manager has been stopped".into(),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if sessions.len() >= self.limits.max_sessions {
            return Err(AgentError::Tool(
                "persistent command session limit reached; drain or terminate an existing session"
                    .into(),
            ));
        }
        #[cfg(windows)]
        if self.isolation == CommandIsolation::WorkspaceWriteOffline {
            return windows_sandbox::start(
                self,
                program,
                args,
                cancellation,
                &runtime,
                &mut sessions,
            );
        }

        #[cfg(windows)]
        if let Some((columns, rows)) = terminal {
            return self.start_pty_locked(program, args, columns, rows, &runtime, &mut sessions);
        }
        #[cfg(not(windows))]
        if terminal.is_some() {
            return Err(AgentError::Tool(
                "persistent PTY containment is unavailable on this host; command was not started"
                    .into(),
            ));
        }

        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&self.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.env_clear();
        for key in HOST_BOOTSTRAP_ENV_KEYS {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW | crate::process_tree::CREATE_SUSPENDED);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        let tree = ProcessTreeGuard::new().map_err(|error| {
            AgentError::Tool(format!("cannot create persistent command job: {error}"))
        })?;
        let mut child = command.spawn().map_err(|error| {
            AgentError::Tool(format!("cannot start persistent command: {error}"))
        })?;
        #[cfg(windows)]
        if let Err(error) = tree.assign_and_resume(&child) {
            let _ = child.start_kill();
            return Err(AgentError::EffectUncertain(format!(
                "cannot contain persistent command after spawn: {error}"
            )));
        }
        #[cfg(unix)]
        let tree = ProcessTreeGuard::new(child.id()).map_err(|error| {
            let _ = child.start_kill();
            AgentError::EffectUncertain(format!(
                "cannot contain persistent command after spawn: {error}"
            ))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            AgentError::EffectUncertain(
                "persistent command stdin is unavailable after spawn".into(),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AgentError::EffectUncertain(
                "persistent command stdout is unavailable after spawn".into(),
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AgentError::EffectUncertain(
                "persistent command stderr is unavailable after spawn".into(),
            )
        })?;

        let output = Arc::new(OutputBuffer::new(self.limits.max_buffered_output_bytes));
        let stdin = Arc::new(AsyncMutex::new(Some(
            Box::new(stdin) as Box<dyn AsyncWrite + Send + Unpin>
        )));
        let stop = CancellationToken::new();
        let tree = Arc::new(Mutex::new(Some(SessionTree::Pipes(tree))));
        let session = Arc::new(Session {
            stdin: Arc::clone(&stdin),
            output: Arc::clone(&output),
            stop: stop.clone(),
            tree: Arc::clone(&tree),
            poll_lock: AsyncMutex::new(()),
            pty: false,
            #[cfg(windows)]
            pty_console: None,
            #[cfg(windows)]
            sandbox_stdin: None,
        });
        let stdout_reader = runtime.spawn(read_pipe(
            stdout,
            PersistentCommandStream::Stdout,
            Arc::clone(&output),
        ));
        let stderr_reader = runtime.spawn(read_pipe(
            stderr,
            PersistentCommandStream::Stderr,
            Arc::clone(&output),
        ));
        runtime.spawn(
            Monitor {
                child,
                tree,
                stdin,
                output,
                stop,
                lifetime: self.limits.max_lifetime,
                stdout_reader,
                stderr_reader,
            }
            .run(),
        );
        let id = Uuid::new_v4().to_string();
        sessions.insert(id.clone(), session);
        Ok(PersistentCommandStarted { id })
    }

    #[cfg(windows)]
    fn start_pty_locked(
        &self,
        program: &str,
        args: &[String],
        columns: u16,
        rows: u16,
        runtime: &tokio::runtime::Handle,
        sessions: &mut HashMap<String, Arc<Session>>,
    ) -> Result<PersistentCommandStarted, AgentError> {
        let process = windows_pty::spawn(program, args, &self.workspace, columns, rows).map_err(
            |failure| {
                if failure.launched {
                    AgentError::EffectUncertain(format!(
                        "PTY process was contained but setup failed after spawn: {}",
                        failure.error
                    ))
                } else {
                    AgentError::Tool(format!(
                        "cannot start contained PTY command: {}",
                        failure.error
                    ))
                }
            },
        )?;
        let windows_pty::PtyProcess {
            stdin,
            output: terminal_output,
            process,
            console,
            job,
        } = process;
        let output = Arc::new(OutputBuffer::new(self.limits.max_buffered_output_bytes));
        let stdin = Arc::new(AsyncMutex::new(Some(
            Box::new(stdin) as Box<dyn AsyncWrite + Send + Unpin>
        )));
        let stop = CancellationToken::new();
        let tree = Arc::new(Mutex::new(Some(SessionTree::Pty(job))));
        let pty_console = Arc::new(Mutex::new(Some(console)));
        let session = Arc::new(Session {
            stdin: Arc::clone(&stdin),
            output: Arc::clone(&output),
            stop: stop.clone(),
            tree: Arc::clone(&tree),
            poll_lock: AsyncMutex::new(()),
            pty: true,
            pty_console: Some(Arc::clone(&pty_console)),
            sandbox_stdin: None,
        });
        let reader_output = Arc::clone(&output);
        let terminal_reader = std::thread::Builder::new()
            .name("openide-conpty-output".into())
            .spawn(move || {
                read_pipe_blocking(
                    terminal_output,
                    PersistentCommandStream::Terminal,
                    reader_output,
                )
            })
            .map_err(|error| {
                AgentError::EffectUncertain(format!(
                    "contained PTY started but output reader could not start: {error}"
                ))
            })?;
        runtime.spawn(
            PtyMonitor {
                process,
                cleanup: PtyCleanup {
                    tree,
                    console: pty_console,
                },
                stdin,
                output,
                stop,
                lifetime: self.limits.max_lifetime,
                terminal_reader,
            }
            .run(),
        );
        let id = Uuid::new_v4().to_string();
        sessions.insert(id.clone(), session);
        Ok(PersistentCommandStarted { id })
    }

    fn session(&self, id: &str) -> Result<Arc<Session>, AgentError> {
        self.sessions
            .lock()
            .map_err(|_| AgentError::Tool("persistent command registry is unavailable".into()))?
            .get(id)
            .cloned()
            .ok_or_else(|| AgentError::Tool("persistent command session was not found".into()))
    }

    /// A poll drains at most `max_poll_output_bytes`. A terminal result stays
    /// registered until the final output is delivered.
    pub async fn poll(
        &self,
        id: &str,
        wait: Duration,
        cancellation: &CancellationToken,
    ) -> Result<PersistentCommandSnapshot, AgentError> {
        self.poll_with_output_limit(id, wait, self.limits.max_poll_output_bytes, cancellation)
            .await
    }

    /// The caller can select a smaller per-response byte limit to keep a
    /// serialized tool result bounded without discarding pending output.
    pub async fn poll_with_output_limit(
        &self,
        id: &str,
        wait: Duration,
        max_output_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<PersistentCommandSnapshot, AgentError> {
        if max_output_bytes == 0 || max_output_bytes > self.limits.max_poll_output_bytes {
            return Err(AgentError::Tool(
                "invalid persistent command poll output limit".into(),
            ));
        }
        let session = self.session(id)?;
        let _poll_guard = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
            guard = session.poll_lock.lock() => guard,
        };
        let deadline = Instant::now() + wait.min(MAX_POLL_WAIT);
        loop {
            let ready = {
                let state = session.output.state.lock().map_err(|_| {
                    AgentError::EffectUncertain(
                        "persistent command output state is unavailable".into(),
                    )
                })?;
                if state.final_delivered {
                    return Err(AgentError::Tool(
                        "persistent command session was already drained".into(),
                    ));
                }
                !state.events.is_empty() || state.exit.is_some()
            };
            if ready || Instant::now() >= deadline {
                break;
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
                _ = session.output.changed.notified() => {},
                _ = sleep(Duration::from_millis(25)) => {},
            }
        }
        let (events, exit, reader_error, has_more_output, finished) = {
            let mut state = session.output.state.lock().map_err(|_| {
                AgentError::EffectUncertain("persistent command output state is unavailable".into())
            })?;
            let events = state.drain(max_output_bytes);
            let has_more_output = !state.events.is_empty();
            let finished = state.exit.is_some() && !has_more_output;
            if finished {
                state.final_delivered = true;
            }
            (
                events,
                state.exit.clone(),
                state.reader_error.clone(),
                has_more_output,
                finished,
            )
        };
        if finished {
            let mut sessions = self.sessions.lock().map_err(|_| {
                AgentError::EffectUncertain("persistent command registry is unavailable".into())
            })?;
            if sessions
                .get(id)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                sessions.remove(id);
            }
        }
        Ok(PersistentCommandSnapshot {
            id: id.to_owned(),
            events,
            exit,
            reader_error,
            has_more_output,
            finished,
        })
    }

    /// Input is bounded. A cancelled, timed-out, or failed write may have been
    /// partial, so it stops the process and reports an uncertain effect.
    pub async fn write_stdin(
        &self,
        id: &str,
        bytes: &[u8],
        close_after: bool,
        cancellation: &CancellationToken,
    ) -> Result<(), AgentError> {
        if bytes.len() > self.limits.max_stdin_bytes {
            return Err(AgentError::Tool(
                "persistent command input exceeds limit".into(),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let session = self.session(id)?;
        #[cfg(windows)]
        if session.sandbox_stdin.is_some() {
            return windows_sandbox::write_stdin(&session, bytes, close_after, cancellation).await;
        }
        let mut stdin = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
            guard = session.stdin.lock() => guard,
        };
        if session.stop.is_cancelled() || session.output.has_exited()? {
            return Err(AgentError::Tool(
                "persistent command is no longer running".into(),
            ));
        }
        if !bytes.is_empty() {
            let writer = stdin
                .as_mut()
                .ok_or_else(|| AgentError::Tool("persistent command stdin is closed".into()))?;
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err("cancelled"),
                _ = session.stop.cancelled() => Err("terminated"),
                result = timeout(IO_WAIT, async {
                    writer.write_all(bytes).await?;
                    writer.flush().await
                }) => match result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(_)) => Err("failed"),
                    Err(_) => Err("timed out"),
                },
            };
            if let Err(reason) = result {
                session.stop_tree();
                return Err(AgentError::EffectUncertain(format!(
                    "persistent command stdin write {reason}; partial input may have been delivered"
                )));
            }
        }
        if close_after {
            stdin.take();
        }
        Ok(())
    }

    pub async fn close_stdin(
        &self,
        id: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), AgentError> {
        self.write_stdin(id, &[], true, cancellation).await
    }

    /// Resize one live Windows ConPTY. A failed, cancelled, or timed-out Win32
    /// call may already have affected the terminal, so the job is closed and
    /// the result is reported as uncertain. Pipe and MXC sessions never resize.
    pub async fn resize_pty(
        &self,
        id: &str,
        columns: u16,
        rows: u16,
        cancellation: &CancellationToken,
    ) -> Result<(), AgentError> {
        if !self.pty_resize_tools_enabled() {
            return Err(AgentError::Tool(
                "persistent PTY resize is not enabled for this manager".into(),
            ));
        }
        if !(20..=500).contains(&columns) || !(5..=200).contains(&rows) {
            return Err(AgentError::Tool("invalid PTY terminal dimensions".into()));
        }
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        #[cfg(not(windows))]
        {
            let _ = id;
            return Err(AgentError::Tool(
                "persistent PTY resize is unavailable on this host".into(),
            ));
        }
        #[cfg(windows)]
        {
            let session = self.session(id)?;
            let console = session.pty_console.as_ref().ok_or_else(|| {
                AgentError::Tool("persistent command is not a PTY session".into())
            })?;
            if session.stop.is_cancelled() || session.output.has_exited()? {
                return Err(AgentError::Tool(
                    "persistent command is no longer running".into(),
                ));
            }
            let console = Arc::clone(console);
            let mut operation = tokio::task::spawn_blocking(move || {
                let mut guard = console
                    .lock()
                    .map_err(|_| std::io::Error::other("PTY console lock is unavailable"))?;
                match guard.as_mut() {
                    Some(console) => console.resize(columns, rows).map(Some),
                    None => Ok(None),
                }
            });
            let result = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    session.stop_tree();
                    return Err(AgentError::EffectUncertain(
                        "PTY resize was cancelled; terminal size may have changed".into(),
                    ));
                }
                result = timeout(IO_WAIT, &mut operation) => result,
            };
            match result {
                Ok(Ok(Ok(Some(())))) => Ok(()),
                Ok(Ok(Ok(None))) => {
                    Err(AgentError::Tool("persistent PTY has already closed".into()))
                }
                Ok(Ok(Err(error))) => {
                    session.stop_tree();
                    Err(AgentError::EffectUncertain(format!(
                        "PTY resize failed; terminal size may have changed: {error}"
                    )))
                }
                Ok(Err(error)) => {
                    session.stop_tree();
                    Err(AgentError::EffectUncertain(format!(
                        "PTY resize task failed; terminal size may have changed: {error}"
                    )))
                }
                Err(_) => {
                    session.stop_tree();
                    Err(AgentError::EffectUncertain(
                        "PTY resize timed out; terminal size may have changed".into(),
                    ))
                }
            }
        }
    }

    /// Stop the process tree and wait for readers to finish. Output remains
    /// available through `poll` until its final snapshot is consumed.
    pub async fn terminate(&self, id: &str) -> Result<(), AgentError> {
        let session = self.session(id)?;
        session.stop_tree();
        let deadline = Instant::now() + IO_WAIT + READER_DRAIN_WAIT + Duration::from_secs(1);
        while !session.output.has_exited()? {
            if Instant::now() >= deadline {
                return Err(AgentError::EffectUncertain(
                    "persistent command termination could not be confirmed".into(),
                ));
            }
            tokio::select! {
                _ = session.output.changed.notified() => {},
                _ = sleep(Duration::from_millis(25)) => {},
            }
        }
        Ok(())
    }
}

struct OutputBuffer {
    state: Mutex<OutputState>,
    changed: Notify,
    max_bytes: usize,
}

impl OutputBuffer {
    fn new(max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(OutputState::default()),
            changed: Notify::new(),
            max_bytes,
        }
    }

    fn push(&self, stream: PersistentCommandStream, bytes: &[u8]) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.push(stream, bytes, self.max_bytes);
        self.changed.notify_waiters();
    }

    fn finish(&self, exit: PersistentCommandExit, reader_error: Option<String>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.exit = Some(exit);
        state.reader_error = reader_error;
        self.changed.notify_waiters();
    }

    fn has_exited(&self) -> Result<bool, AgentError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| {
                AgentError::EffectUncertain("persistent command output state is unavailable".into())
            })?
            .exit
            .is_some())
    }
}

#[derive(Default)]
struct OutputState {
    events: VecDeque<PersistentCommandEvent>,
    retained_bytes: usize,
    exit: Option<PersistentCommandExit>,
    reader_error: Option<String>,
    final_delivered: bool,
}

impl OutputState {
    fn push(&mut self, stream: PersistentCommandStream, bytes: &[u8], max_bytes: usize) {
        let mut keep = max_bytes
            .saturating_sub(self.retained_bytes)
            .min(bytes.len());
        if keep > 0 {
            let can_extend_last = matches!(
                self.events.back(),
                Some(PersistentCommandEvent::Output { stream: last, bytes: existing })
                    if *last == stream && existing.len() + keep <= READER_CHUNK_BYTES
            );
            if !can_extend_last && self.events.len() >= MAX_BUFFER_EVENTS {
                keep = 0;
            }
            if keep > 0 {
                let kept = &bytes[..keep];
                if can_extend_last {
                    if let Some(PersistentCommandEvent::Output {
                        bytes: existing, ..
                    }) = self.events.back_mut()
                    {
                        existing.extend_from_slice(kept);
                    }
                } else {
                    self.events.push_back(PersistentCommandEvent::Output {
                        stream,
                        bytes: kept.to_vec(),
                    });
                }
                self.retained_bytes += keep;
            }
        }
        let omitted = bytes.len() - keep;
        if omitted > 0 {
            match self.events.back_mut() {
                Some(PersistentCommandEvent::Omitted { bytes }) => {
                    *bytes = bytes.saturating_add(omitted as u64);
                }
                _ => self.events.push_back(PersistentCommandEvent::Omitted {
                    bytes: omitted as u64,
                }),
            }
        }
    }

    fn drain(&mut self, max_bytes: usize) -> Vec<PersistentCommandEvent> {
        let mut result = Vec::new();
        let mut used = 0;
        while result.len() < MAX_POLL_EVENTS {
            let Some(mut event) = self.events.pop_front() else {
                break;
            };
            match &mut event {
                PersistentCommandEvent::Output { stream, bytes } => {
                    let room = max_bytes.saturating_sub(used);
                    if room == 0 {
                        self.events.push_front(event);
                        break;
                    }
                    if bytes.len() > room {
                        let tail = bytes.split_off(room);
                        self.events.push_front(PersistentCommandEvent::Output {
                            stream: *stream,
                            bytes: tail,
                        });
                    }
                    used += bytes.len();
                    self.retained_bytes -= bytes.len();
                    result.push(event);
                }
                PersistentCommandEvent::Omitted { .. } => result.push(event),
            }
        }
        result
    }
}

async fn read_pipe<R: AsyncRead + Unpin>(
    mut pipe: R,
    stream: PersistentCommandStream,
    output: Arc<OutputBuffer>,
) -> std::io::Result<()> {
    let mut chunk = [0u8; READER_CHUNK_BYTES];
    loop {
        let count = pipe.read(&mut chunk).await?;
        if count == 0 {
            return Ok(());
        }
        output.push(stream, &chunk[..count]);
    }
}

#[cfg(windows)]
fn read_pipe_blocking<R: std::io::Read>(
    mut pipe: R,
    stream: PersistentCommandStream,
    output: Arc<OutputBuffer>,
) -> std::io::Result<()> {
    let mut chunk = [0u8; READER_CHUNK_BYTES];
    loop {
        let count = pipe.read(&mut chunk)?;
        if count == 0 {
            return Ok(());
        }
        output.push(stream, &chunk[..count]);
    }
}

struct Monitor {
    child: Child,
    tree: Arc<Mutex<Option<SessionTree>>>,
    stdin: Arc<AsyncMutex<Option<Box<dyn AsyncWrite + Send + Unpin>>>>,
    output: Arc<OutputBuffer>,
    stop: CancellationToken,
    lifetime: Duration,
    stdout_reader: tokio::task::JoinHandle<std::io::Result<()>>,
    stderr_reader: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Monitor {
    async fn run(self) {
        let Monitor {
            mut child,
            tree,
            stdin,
            output,
            stop,
            lifetime,
            mut stdout_reader,
            mut stderr_reader,
        } = self;
        let exit = tokio::select! {
            biased;
            _ = stop.cancelled() => PersistentCommandExit::Terminated,
            result = child.wait() => match result {
                Ok(status) => PersistentCommandExit::Exited { code: status.code() },
                Err(error) => PersistentCommandExit::WaitFailed(error.to_string()),
            },
            _ = sleep(lifetime) => PersistentCommandExit::TimedOut,
        };
        // The job/group is closed on every outcome, including normal parent exit,
        // so descendants cannot hold the pipes or continue without their parent.
        if let Some(tree) = tree
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            tree.close();
        }
        if !matches!(exit, PersistentCommandExit::Exited { .. }) {
            let _ = timeout(IO_WAIT, child.wait()).await;
        }
        stdin.lock().await.take();
        let readers = timeout(READER_DRAIN_WAIT, async {
            tokio::join!(&mut stdout_reader, &mut stderr_reader)
        })
        .await;
        let reader_error = match readers {
            Ok((Ok(Ok(())), Ok(Ok(())))) => None,
            Ok((stdout, stderr)) => Some(format!(
                "persistent command output reader failed: stdout={}; stderr={}",
                reader_result(&stdout),
                reader_result(&stderr)
            )),
            Err(_) => {
                stdout_reader.abort();
                stderr_reader.abort();
                let _ = tokio::join!(stdout_reader, stderr_reader);
                Some("persistent command output readers did not close within limit".into())
            }
        };
        output.finish(exit, reader_error);
    }
}

#[cfg(windows)]
struct PtyMonitor {
    process: Arc<std::os::windows::io::OwnedHandle>,
    cleanup: PtyCleanup,
    stdin: Arc<AsyncMutex<Option<Box<dyn AsyncWrite + Send + Unpin>>>>,
    output: Arc<OutputBuffer>,
    stop: CancellationToken,
    lifetime: Duration,
    terminal_reader: std::thread::JoinHandle<std::io::Result<()>>,
}

#[cfg(windows)]
struct PtyCleanup {
    tree: Arc<Mutex<Option<SessionTree>>>,
    console: Arc<Mutex<Option<windows_pty::PseudoConsole>>>,
}

#[cfg(windows)]
impl PtyCleanup {
    fn close_job(&self) {
        if let Some(tree) = self
            .tree
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            tree.close();
        }
    }

    fn close_console(&mut self) {
        self.console
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }
}

#[cfg(windows)]
impl Drop for PtyCleanup {
    fn drop(&mut self) {
        // Runtime shutdown may abort the monitor before its normal cleanup.
        self.close_job();
        self.close_console();
    }
}

#[cfg(windows)]
impl PtyMonitor {
    async fn run(self) {
        let PtyMonitor {
            process,
            mut cleanup,
            stdin,
            output,
            stop,
            lifetime,
            terminal_reader,
        } = self;
        let mut waiter = tokio::task::spawn_blocking(move || windows_pty::wait_process(process));
        let exit = tokio::select! {
            biased;
            _ = stop.cancelled() => PersistentCommandExit::Terminated,
            result = &mut waiter => match result {
                Ok(Ok(code)) => PersistentCommandExit::Exited { code: Some(code) },
                Ok(Err(error)) => PersistentCommandExit::WaitFailed(error.to_string()),
                Err(error) => PersistentCommandExit::WaitFailed(error.to_string()),
            },
            _ = sleep(lifetime) => PersistentCommandExit::TimedOut,
        };
        // Every path closes the job before exposing a terminal snapshot.
        cleanup.close_job();
        stdin.lock().await.take();
        // Give ConPTY's host a bounded window to publish its final bytes
        // before closing the pseudoconsole, which otherwise discards them.
        sleep(Duration::from_millis(150)).await;
        // ConPTY's output pipe may remain open after the foreground process
        // exits until the pseudoconsole itself is closed.
        cleanup.close_console();
        if !matches!(exit, PersistentCommandExit::Exited { .. }) {
            let _ = timeout(IO_WAIT, &mut waiter).await;
        }
        let reader = tokio::task::spawn_blocking(move || match terminal_reader.join() {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("persistent PTY output reader failed: {error}")),
            Err(_) => Some("persistent PTY output reader panicked".into()),
        });
        let reader_error = match timeout(READER_DRAIN_WAIT, reader).await {
            Ok(Ok(error)) => error,
            Ok(Err(error)) => Some(format!("persistent PTY output join failed: {error}")),
            Err(_) => Some("persistent PTY output reader did not close within limit".into()),
        };
        output.finish(exit, reader_error);
    }
}

fn reader_result(result: &Result<std::io::Result<()>, tokio::task::JoinError>) -> &'static str {
    match result {
        Ok(Ok(())) => "ok",
        Ok(Err(_)) => "io error",
        Err(_) => "task error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_output_reports_exact_omission_in_sequence() {
        let mut state = OutputState::default();
        state.push(PersistentCommandStream::Stdout, b"abcd", 4);
        state.push(PersistentCommandStream::Stderr, b"123", 4);
        assert_eq!(
            state.drain(4),
            vec![
                PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Stdout,
                    bytes: b"abcd".to_vec(),
                },
                PersistentCommandEvent::Omitted { bytes: 3 },
            ]
        );
        state.push(PersistentCommandStream::Stdout, b"xy", 4);
        assert_eq!(
            state.drain(4),
            vec![PersistentCommandEvent::Output {
                stream: PersistentCommandStream::Stdout,
                bytes: b"xy".to_vec(),
            }]
        );
        for i in 0..MAX_BUFFER_EVENTS {
            let stream = if i % 2 == 0 {
                PersistentCommandStream::Stdout
            } else {
                PersistentCommandStream::Stderr
            };
            state.push(stream, b"x", MAX_BUFFER_EVENTS + 1);
        }
        state.push(PersistentCommandStream::Stdout, b"z", MAX_BUFFER_EVENTS + 1);
        assert!(matches!(
            state.events.back(),
            Some(PersistentCommandEvent::Omitted { bytes: 1 })
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn offline_isolation_rejects_persistent_start_before_spawn() {
        let workspace = tempfile::tempdir().unwrap();
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let error = manager
            .start("definitely-not-a-command", &[], &CancellationToken::new())
            .unwrap_err();
        assert!(error.to_string().contains("isolation is unavailable"));
    }

    #[cfg(windows)]
    #[test]
    fn offline_isolation_rejects_pty_before_spawn() {
        let workspace = tempfile::tempdir().unwrap();
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_pty_tools();
        let error = manager
            .start_pty(
                "definitely-not-a-command",
                &[],
                80,
                24,
                &CancellationToken::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("not enabled"));
        assert!(manager.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn pty_requires_manager_opt_in_before_spawn() {
        let workspace = tempfile::tempdir().unwrap();
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let error = manager
            .start_pty(
                "definitely-not-a-command",
                &[],
                80,
                24,
                &CancellationToken::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("not enabled"));
    }

    #[cfg(windows)]
    fn compile_probe() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("persistent_probe.rs");
        let binary = dir.path().join("persistent_probe.exe");
        std::fs::write(
            &source,
            r#"
use std::io::{IsTerminal, Read, Write};
use std::{env, fs, process, thread, time::Duration};
fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref().unwrap() {
        "echo" => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).unwrap();
            print!("OUT:{input}");
            eprint!("ERR:{input}");
        }
        "pty_echo" => {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).unwrap();
            print!("PTY:{};OUT:{input}", std::io::stdout().is_terminal());
            eprint!("ERR:{input}");
        }
        "pty_size" => {
            #[repr(C)]
            struct Coord { x: i16, y: i16 }
            #[repr(C)]
            struct SmallRect { left: i16, top: i16, right: i16, bottom: i16 }
            #[repr(C)]
            struct ConsoleScreenBufferInfo {
                size: Coord,
                cursor: Coord,
                attributes: u16,
                window: SmallRect,
                maximum: Coord,
            }
            #[link(name = "kernel32")]
            extern "system" {
                fn GetStdHandle(kind: u32) -> *mut std::ffi::c_void;
                fn GetConsoleScreenBufferInfo(
                    handle: *mut std::ffi::c_void,
                    info: *mut ConsoleScreenBufferInfo,
                ) -> i32;
            }
            fn terminal_size() -> String {
                let mut info = std::mem::MaybeUninit::<ConsoleScreenBufferInfo>::uninit();
                let handle = unsafe { GetStdHandle(-11_i32 as u32) };
                let ok = unsafe { GetConsoleScreenBufferInfo(handle, info.as_mut_ptr()) };
                if ok == 0 { return "unavailable".into(); }
                let info = unsafe { info.assume_init() };
                format!("{}x{}", info.window.right - info.window.left + 1,
                    info.window.bottom - info.window.top + 1)
            }
            println!("READY:{}", terminal_size());
            std::io::stdout().flush().unwrap();
            loop {
                let mut input = String::new();
                if std::io::stdin().read_line(&mut input).unwrap() == 0 { break; }
                if input.trim() == "quit" { break; }
                println!("SIZE:{}", terminal_size());
                std::io::stdout().flush().unwrap();
            }
        }
        "flood" => {
            std::io::stdout().write_all(&vec![b'o'; 1_000_000]).unwrap();
            std::io::stderr().write_all(&vec![b'e'; 1_000_000]).unwrap();
        }
        "pty_flood" => {
            std::io::stdout().write_all(&vec![b'o'; 100_000]).unwrap();
            std::io::stderr().write_all(&vec![b'e'; 100_000]).unwrap();
        }
        "parent" => {
            let path = args.next().unwrap();
            process::Command::new(env::current_exe().unwrap())
                .arg("child").arg(path).spawn().unwrap();
            thread::sleep(Duration::from_secs(60));
        }
        "child" => {
            let path = args.next().unwrap();
            fs::write(path, process::id().to_string()).unwrap();
            thread::sleep(Duration::from_secs(60));
        }
        "sleep" => thread::sleep(Duration::from_secs(60)),
        _ => panic!("invalid probe mode"),
    }
}
"#,
        )
        .unwrap();
        let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let result = std::process::Command::new(compiler)
            .arg("--edition=2021")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "probe compile failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        (dir, binary)
    }

    #[cfg(windows)]
    async fn collect_final(
        manager: &PersistentCommandManager,
        id: &str,
    ) -> Vec<PersistentCommandSnapshot> {
        let mut snapshots = Vec::new();
        for _ in 0..100 {
            let snapshot = manager
                .poll(id, Duration::from_millis(100), &CancellationToken::new())
                .await
                .unwrap();
            let finished = snapshot.finished;
            snapshots.push(snapshot);
            if finished {
                return snapshots;
            }
        }
        panic!(
            "persistent probe did not finish: {:?}",
            snapshots
                .iter()
                .filter(|snapshot| !snapshot.events.is_empty() || snapshot.exit.is_some())
                .take(10)
                .collect::<Vec<_>>()
        );
    }

    #[cfg(windows)]
    async fn wait_for_child_pid(path: &std::path::Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if let Ok(pid) = contents.trim().parse::<u32>() {
                    return pid;
                }
            }
            assert!(Instant::now() < deadline, "child PID was not written");
            sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_interactive_stdin_and_both_output_streams() {
        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let started = manager
            .start(
                binary.to_str().unwrap(),
                &["echo".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        manager
            .write_stdin(&started.id, b"hello\n", true, &CancellationToken::new())
            .await
            .unwrap();
        let snapshots = collect_final(&manager, &started.id).await;
        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();
        for event in snapshots.iter().flat_map(|snapshot| &snapshot.events) {
            match event {
                PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Stdout,
                    bytes,
                } => stdout.extend_from_slice(bytes),
                PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Stderr,
                    bytes,
                } => stderr.extend_from_slice(bytes),
                PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Terminal,
                    ..
                } => panic!("pipe command unexpectedly returned terminal output"),
                PersistentCommandEvent::Omitted { .. } => panic!("unexpected omitted output"),
            }
        }
        assert_eq!(stdout, b"OUT:hello\n");
        assert_eq!(stderr, b"ERR:hello\n");
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::Exited { code: Some(0) })
        );
        assert!(snapshots.last().unwrap().reader_error.is_none());
        assert!(manager
            .poll(&started.id, Duration::ZERO, &CancellationToken::new())
            .await
            .is_err());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_pty_is_terminal_and_merges_both_streams() {
        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_pty_tools();
        let started = manager
            .start_pty(
                binary.to_str().unwrap(),
                &["pty_echo".into()],
                80,
                24,
                &CancellationToken::new(),
            )
            .unwrap();
        let listed = manager.list_sessions().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, started.id);
        assert!(listed[0].pty);
        assert_eq!(listed[0].exit, None);
        manager
            .write_stdin(&started.id, b"hello\r\n", false, &CancellationToken::new())
            .await
            .unwrap();
        let snapshots = collect_final(&manager, &started.id).await;
        let mut terminal = Vec::new();
        for event in snapshots.iter().flat_map(|snapshot| &snapshot.events) {
            match event {
                PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Terminal,
                    bytes,
                } => terminal.extend_from_slice(bytes),
                other => panic!("unexpected PTY event: {other:?}"),
            }
        }
        let text = String::from_utf8_lossy(&terminal);
        assert!(
            text.contains("PTY:true"),
            "PTY was not a terminal: {text}; final={:?}",
            snapshots.last()
        );
        assert!(text.contains("OUT:hello"), "stdout missing: {text}");
        assert!(text.contains("ERR:hello"), "stderr missing: {text}");
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::Exited { code: Some(0) })
        );
        assert!(snapshots.last().unwrap().reader_error.is_none());
        assert!(manager.list_sessions().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_pty_resize_changes_child_console_and_rejects_pipes() {
        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_pty_tools()
        .with_pty_resize_tools();
        let started = manager
            .start_pty(
                binary.to_str().unwrap(),
                &["pty_size".into()],
                80,
                24,
                &CancellationToken::new(),
            )
            .unwrap();
        let mut output = String::new();
        for _ in 0..50 {
            let snapshot = manager
                .poll(
                    &started.id,
                    Duration::from_millis(100),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            for event in snapshot.events {
                if let PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Terminal,
                    bytes,
                } = event
                {
                    output.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            if output.contains("READY:80x24") {
                break;
            }
        }
        assert!(output.contains("READY:80x24"), "initial console: {output}");
        let invalid = manager
            .resize_pty(&started.id, 501, 30, &CancellationToken::new())
            .await;
        assert!(matches!(invalid, Err(AgentError::Tool(_))));
        manager
            .resize_pty(&started.id, 100, 30, &CancellationToken::new())
            .await
            .unwrap();
        manager
            .write_stdin(&started.id, b"size\r\n", false, &CancellationToken::new())
            .await
            .unwrap();
        let mut resized = String::new();
        for _ in 0..50 {
            let snapshot = manager
                .poll(
                    &started.id,
                    Duration::from_millis(100),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            for event in snapshot.events {
                if let PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Terminal,
                    bytes,
                } = event
                {
                    resized.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            if resized.contains("SIZE:100x30") {
                break;
            }
        }
        assert!(
            resized.contains("SIZE:100x30"),
            "resized console: {resized}"
        );
        manager.terminate(&started.id).await.unwrap();
        assert!(
            collect_final(&manager, &started.id)
                .await
                .last()
                .unwrap()
                .finished
        );

        let pipe = manager
            .start(
                binary.to_str().unwrap(),
                &["sleep".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        let rejected = manager
            .resize_pty(&pipe.id, 100, 30, &CancellationToken::new())
            .await;
        assert!(matches!(rejected, Err(AgentError::Tool(_))));
        manager.terminate(&pipe.id).await.unwrap();
        assert!(
            collect_final(&manager, &pipe.id)
                .await
                .last()
                .unwrap()
                .finished
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_pty_job_terminates_descendant() {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let pid_file = workspace.path().join("pty-child.pid");
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_pty_tools();
        let started = manager
            .start_pty(
                binary.to_str().unwrap(),
                &["parent".into(), pid_file.to_string_lossy().into_owned()],
                80,
                24,
                &CancellationToken::new(),
            )
            .unwrap();
        let child_pid = wait_for_child_pid(&pid_file).await;
        manager.terminate(&started.id).await.unwrap();
        assert_eq!(
            collect_final(&manager, &started.id)
                .await
                .last()
                .unwrap()
                .exit,
            Some(PersistentCommandExit::Terminated)
        );
        let mut stopped = false;
        for _ in 0..100 {
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, child_pid) };
            if handle.is_null() {
                stopped = true;
                break;
            }
            let mut code = 0;
            let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
            unsafe { CloseHandle(handle) };
            if ok != 0 && code != STILL_ACTIVE as u32 {
                stopped = true;
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        assert!(stopped, "PTY child process survived termination of the job");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_pty_output_is_bounded_and_timeout_closes_process() {
        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits {
                max_buffered_output_bytes: 4 * 1024,
                max_poll_output_bytes: 4 * 1024,
                max_lifetime: Duration::from_secs(20),
                ..PersistentCommandLimits::default()
            },
        )
        .unwrap()
        .with_pty_tools();
        let started = manager
            .start_pty(
                binary.to_str().unwrap(),
                &["pty_flood".into()],
                80,
                24,
                &CancellationToken::new(),
            )
            .unwrap();
        sleep(Duration::from_millis(500)).await;
        let snapshots = collect_final(&manager, &started.id).await;
        let omitted: u64 = snapshots
            .iter()
            .flat_map(|snapshot| &snapshot.events)
            .filter_map(|event| match event {
                PersistentCommandEvent::Omitted { bytes } => Some(*bytes),
                _ => None,
            })
            .sum();
        assert!(omitted > 0, "PTY flood was not bounded");
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::Exited { code: Some(0) })
        );

        let timeout_manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits {
                max_lifetime: Duration::from_millis(250),
                ..PersistentCommandLimits::default()
            },
        )
        .unwrap()
        .with_pty_tools();
        let sleeper = timeout_manager
            .start_pty(
                binary.to_str().unwrap(),
                &["sleep".into()],
                80,
                24,
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            collect_final(&timeout_manager, &sleeper.id)
                .await
                .last()
                .unwrap()
                .exit,
            Some(PersistentCommandExit::TimedOut)
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_sandbox_persistent_stdin_and_output() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let probe = crate::command::build_sandbox_probe(&workspace);
        let manager = PersistentCommandManager::new(
            workspace,
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let started = manager
            .start(
                probe.to_str().unwrap(),
                &["--echo-stdin".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        manager
            .write_stdin(&started.id, b"hello\n", true, &CancellationToken::new())
            .await
            .unwrap();
        let snapshots = collect_final(&manager, &started.id).await;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        for event in snapshots.iter().flat_map(|snapshot| &snapshot.events) {
            match event {
                PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Stdout,
                    bytes,
                } => stdout.extend_from_slice(bytes),
                PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Stderr,
                    bytes,
                } => stderr.extend_from_slice(bytes),
                other => panic!("unexpected sandbox output: {other:?}"),
            }
        }
        assert!(String::from_utf8_lossy(&stdout).contains("OUT:hello"));
        assert!(String::from_utf8_lossy(&stderr).contains("ERR:hello"));
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::Exited { code: Some(0) })
        );
        assert!(snapshots.last().unwrap().reader_error.is_none());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_sandbox_persistent_output_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let probe = crate::command::build_sandbox_probe(&workspace);
        let manager = PersistentCommandManager::new(
            workspace,
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits {
                max_buffered_output_bytes: 8 * 1024,
                max_poll_output_bytes: 8 * 1024,
                ..PersistentCommandLimits::default()
            },
        )
        .unwrap();
        let started = manager
            .start(
                probe.to_str().unwrap(),
                &["--flood".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        sleep(Duration::from_millis(300)).await;
        let snapshots = collect_final(&manager, &started.id).await;
        let omitted: u64 = snapshots
            .iter()
            .flat_map(|snapshot| &snapshot.events)
            .filter_map(|event| match event {
                PersistentCommandEvent::Omitted { bytes } => Some(*bytes),
                _ => None,
            })
            .sum();
        assert!(omitted > 0, "sandbox flood did not report omitted bytes");
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::Exited { code: Some(0) })
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_sandbox_persistent_blocks_outside_write_and_network() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let probe = crate::command::build_sandbox_probe(&workspace);
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
            "junction setup failed: {}",
            String::from_utf8_lossy(&create_junction.stderr)
        );
        let inside_file = workspace.join("inside.txt");
        let outside_file = outside.join("outside.txt");
        let outside_secret = outside.join("secret.txt");
        std::fs::write(&outside_secret, b"secret").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let manager = PersistentCommandManager::new(
            workspace,
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let started = manager
            .start(
                probe.to_str().unwrap(),
                &[
                    inside_file.to_string_lossy().into_owned(),
                    outside_file.to_string_lossy().into_owned(),
                    listener.local_addr().unwrap().port().to_string(),
                    junction.join("linked.txt").to_string_lossy().into_owned(),
                    outside_secret.to_string_lossy().into_owned(),
                    junction.join("secret.txt").to_string_lossy().into_owned(),
                ],
                &CancellationToken::new(),
            )
            .unwrap();
        let snapshots = collect_final(&manager, &started.id).await;
        let stdout = snapshots
            .iter()
            .flat_map(|snapshot| &snapshot.events)
            .filter_map(|event| match event {
                PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Stdout,
                    bytes,
                } => Some(bytes.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let stdout = String::from_utf8_lossy(&stdout);
        assert!(stdout.contains("inside=Ok(())"), "{stdout}");
        assert!(stdout.contains("outside=Err("), "{stdout}");
        assert!(stdout.contains("through_link=Err("), "{stdout}");
        assert!(stdout.contains("outside_read=Err("), "{stdout}");
        assert!(stdout.contains("through_link_read=Err("), "{stdout}");
        assert!(stdout.contains("network_denied=true"), "{stdout}");
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::Exited { code: Some(0) })
        );
        assert!(inside_file.exists());
        assert!(!outside_file.exists());
        assert!(!outside.join("linked.txt").exists());
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_sandbox_persistent_terminate_kills_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let probe = crate::command::build_sandbox_probe(&workspace);
        let started_file = workspace.join("child-started");
        let survived_file = workspace.join("child-survived");
        let manager = PersistentCommandManager::new(
            workspace,
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let started = manager
            .start(
                probe.to_str().unwrap(),
                &[
                    "--spawn-child".into(),
                    started_file.to_string_lossy().into_owned(),
                    survived_file.to_string_lossy().into_owned(),
                ],
                &CancellationToken::new(),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            while !started_file.exists() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("sandbox descendant did not start");
        manager.terminate(&started.id).await.unwrap();
        let snapshots = collect_final(&manager, &started.id).await;
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::Terminated)
        );
        sleep(Duration::from_secs(3)).await;
        assert!(
            !survived_file.exists(),
            "sandbox descendant survived termination"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_sandbox_stop_all_revokes_starts_and_kills_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let probe = crate::command::build_sandbox_probe(&workspace);
        let started_file = workspace.join("child-started");
        let survived_file = workspace.join("child-survived");
        let manager = PersistentCommandManager::new(
            workspace,
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let started = manager
            .start(
                probe.to_str().unwrap(),
                &[
                    "--spawn-child".into(),
                    started_file.to_string_lossy().into_owned(),
                    survived_file.to_string_lossy().into_owned(),
                ],
                &CancellationToken::new(),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            while !started_file.exists() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("sandbox descendant did not start");
        manager.stop_all();
        assert!(manager
            .start(probe.to_str().unwrap(), &[], &CancellationToken::new())
            .unwrap_err()
            .to_string()
            .contains("stopped"));
        let snapshots = collect_final(&manager, &started.id).await;
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::Terminated)
        );
        sleep(Duration::from_secs(3)).await;
        assert!(
            !survived_file.exists(),
            "sandbox descendant survived stop_all"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_sandbox_manager_drop_kills_owned_tree() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let probe = crate::command::build_sandbox_probe(&workspace);
        let started_file = workspace.join("written-before-drop");
        let survived_file = workspace.join("written-after-drop");
        let manager = PersistentCommandManager::new(
            workspace,
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        manager
            .start(
                probe.to_str().unwrap(),
                &[
                    "--delayed-write".into(),
                    started_file.to_string_lossy().into_owned(),
                    survived_file.to_string_lossy().into_owned(),
                ],
                &CancellationToken::new(),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            while !started_file.exists() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("sandbox command did not start");
        drop(manager);
        sleep(Duration::from_secs(3)).await;
        assert!(
            !survived_file.exists(),
            "sandbox process survived manager drop"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_sandbox_persistent_lifetime_timeout_kills_tree() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let probe = crate::command::build_sandbox_probe(&workspace);
        let started_file = workspace.join("written-before-timeout");
        let survived_file = workspace.join("written-after-timeout");
        let manager = PersistentCommandManager::new(
            workspace,
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits {
                max_lifetime: Duration::from_millis(500),
                ..PersistentCommandLimits::default()
            },
        )
        .unwrap();
        let started = manager
            .start(
                probe.to_str().unwrap(),
                &[
                    "--delayed-write".into(),
                    started_file.to_string_lossy().into_owned(),
                    survived_file.to_string_lossy().into_owned(),
                ],
                &CancellationToken::new(),
            )
            .unwrap();
        let snapshots = collect_final(&manager, &started.id).await;
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::TimedOut)
        );
        assert!(started_file.exists());
        sleep(Duration::from_secs(2)).await;
        assert!(!survived_file.exists(), "sandbox process survived timeout");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_flood_drains_pipes_and_reports_omitted_bytes() {
        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let limits = PersistentCommandLimits {
            max_buffered_output_bytes: 8 * 1024,
            max_poll_output_bytes: 8 * 1024,
            ..PersistentCommandLimits::default()
        };
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            limits,
        )
        .unwrap();
        let started = manager
            .start(
                binary.to_str().unwrap(),
                &["flood".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        sleep(Duration::from_millis(300)).await;
        let snapshots = collect_final(&manager, &started.id).await;
        let omitted: u64 = snapshots
            .iter()
            .flat_map(|snapshot| &snapshot.events)
            .filter_map(|event| match event {
                PersistentCommandEvent::Omitted { bytes } => Some(*bytes),
                _ => None,
            })
            .sum();
        assert!(omitted > 0);
        let retained: u64 = snapshots
            .iter()
            .flat_map(|snapshot| &snapshot.events)
            .filter_map(|event| match event {
                PersistentCommandEvent::Output { bytes, .. } => Some(bytes.len() as u64),
                PersistentCommandEvent::Omitted { .. } => None,
            })
            .sum();
        assert_eq!(
            retained + omitted,
            2_000_000,
            "output loss was not reported"
        );
        assert_eq!(
            snapshots.last().unwrap().exit,
            Some(PersistentCommandExit::Exited { code: Some(0) })
        );
        assert!(snapshots.last().unwrap().reader_error.is_none());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_terminate_kills_descendant_process() {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let pid_file = workspace.path().join("child.pid");
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let started = manager
            .start(
                binary.to_str().unwrap(),
                &["parent".into(), pid_file.to_string_lossy().into_owned()],
                &CancellationToken::new(),
            )
            .unwrap();
        let child_pid = wait_for_child_pid(&pid_file).await;
        manager.terminate(&started.id).await.unwrap();
        assert_eq!(
            collect_final(&manager, &started.id)
                .await
                .last()
                .unwrap()
                .exit,
            Some(PersistentCommandExit::Terminated)
        );
        let mut stopped = false;
        for _ in 0..100 {
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, child_pid) };
            if handle.is_null() {
                stopped = true;
                break;
            }
            let mut code = 0;
            let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
            unsafe { CloseHandle(handle) };
            if ok != 0 && code != STILL_ACTIVE as u32 {
                stopped = true;
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        assert!(stopped, "child process survived termination of the job");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_stop_all_revokes_manager_and_preserves_final_polls() {
        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let first = manager
            .start(
                binary.to_str().unwrap(),
                &["sleep".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        let second = manager
            .start(
                binary.to_str().unwrap(),
                &["sleep".into()],
                &CancellationToken::new(),
            )
            .unwrap();

        manager.stop_all();
        manager.stop_all();
        let error = manager
            .start(
                binary.to_str().unwrap(),
                &["sleep".into()],
                &CancellationToken::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("manager has been stopped"));
        for id in [&first.id, &second.id] {
            assert_eq!(
                collect_final(&manager, id).await.last().unwrap().exit,
                Some(PersistentCommandExit::Terminated)
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_lifetime_timeout_stops_unpolled_process() {
        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let limits = PersistentCommandLimits {
            max_lifetime: Duration::from_millis(250),
            ..PersistentCommandLimits::default()
        };
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            limits,
        )
        .unwrap();
        let started = manager
            .start(
                binary.to_str().unwrap(),
                &["sleep".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        sleep(Duration::from_millis(350)).await;
        assert_eq!(
            collect_final(&manager, &started.id)
                .await
                .last()
                .unwrap()
                .exit,
            Some(PersistentCommandExit::TimedOut)
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_manager_drop_stops_owned_descendant() {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let pid_file = workspace.path().join("drop-child.pid");
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        manager
            .start(
                binary.to_str().unwrap(),
                &["parent".into(), pid_file.to_string_lossy().into_owned()],
                &CancellationToken::new(),
            )
            .unwrap();
        let child_pid = wait_for_child_pid(&pid_file).await;
        drop(manager);
        let mut stopped = false;
        for _ in 0..100 {
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, child_pid) };
            if handle.is_null() {
                stopped = true;
                break;
            }
            let mut code = 0;
            let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
            unsafe { CloseHandle(handle) };
            if ok != 0 && code != STILL_ACTIVE as u32 {
                stopped = true;
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        assert!(stopped, "child process survived manager drop");
    }
}
