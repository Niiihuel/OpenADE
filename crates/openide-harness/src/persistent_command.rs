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
use openide_core::{
    NativeCommandOutputBinding, NativeCommandOutputExit, NativeCommandOutputStore,
    NativeCommandOutputStream, NativeCommandOutputTerminal,
};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
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
#[path = "persistent_command_terminal.rs"]
mod terminal;
pub use terminal::{PersistentTerminal, PersistentTerminalEvent};
#[cfg(all(test, unix))]
#[path = "persistent_command_terminal_tests.rs"]
mod terminal_tests;
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
const MAX_NATIVE_OUTPUT_ERROR_BYTES: usize = 4 * 1024;
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
    /// `None` leaves a started command running until it is stopped, revoked
    /// or the host drops its registry. A cap stays expressible for hosts and
    /// for reservations made while the default was ten minutes.
    pub max_lifetime: Option<Duration>,
    pub max_buffered_output_bytes: usize,
    pub max_poll_output_bytes: usize,
    pub max_stdin_bytes: usize,
}

impl Default for PersistentCommandLimits {
    fn default() -> Self {
        Self {
            max_sessions: 8,
            max_lifetime: None,
            max_buffered_output_bytes: 512 * 1024,
            max_poll_output_bytes: 64 * 1024,
            max_stdin_bytes: 8 * 1024,
        }
    }
}

impl PersistentCommandLimits {
    /// The cap every command had before started commands became uncapped.
    pub const LEGACY_MAX_LIFETIME: Duration = Duration::from_secs(10 * 60);

    pub(crate) fn validate(self) -> Result<Self, AgentError> {
        if !(1..=16).contains(&self.max_sessions)
            || !self.max_lifetime.is_none_or(|lifetime| {
                (Duration::from_millis(100)..=Duration::from_secs(60 * 60)).contains(&lifetime)
            })
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistentCommandEvent {
    Output {
        stream: PersistentCommandStream,
        bytes: Vec<u8>,
    },
    /// Some bytes could not be retained within the configured retention limits.
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

/// A complete copy of the currently retained pending buffer. This observation
/// has no consumer cursor and never delivers or removes the terminal result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentCommandInspection {
    pub id: String,
    pub pty: bool,
    pub events: Vec<PersistentCommandEvent>,
    pub exit: Option<PersistentCommandExit>,
    pub reader_error: Option<String>,
    pub retained_bytes: usize,
    pub max_buffered_output_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistentCommandStdinState {
    Open,
    Closed,
    /// The transport lock is currently held by input or process cleanup.
    Busy,
    /// An input effect failed or was abandoned after dispatch. It may have
    /// delivered bytes; observing process exit does not confirm that input.
    Uncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistentCommandDimensions {
    pub columns: u16,
    pub rows: u16,
}

/// Current controls for one registered command, without reading or consuming
/// pending output. This is an observation, not authorization for a later effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersistentCommandControlInspection {
    pub generation: String,
    pub owner_session_id: Option<String>,
    pub id: String,
    pub workspace: PathBuf,
    pub isolation: CommandIsolation,
    /// Literal original program followed by its argument array, without shell
    /// parsing or executable resolution. Hosts must not log sensitive arguments.
    pub command_argv: Vec<String>,
    pub revoked: bool,
    pub stop_requested: bool,
    pub stdin: PersistentCommandStdinState,
    pub pty: bool,
    /// Last dimensions confirmed by ConPTY start/resize, not a console query.
    /// When uncertain, these remain the last confirmed dimensions only.
    pub dimensions: Option<PersistentCommandDimensions>,
    pub dimensions_uncertain: bool,
    pub resize_enabled: bool,
    pub max_stdin_bytes: usize,
    pub exit: Option<PersistentCommandExit>,
}

impl PersistentCommandControlInspection {
    pub fn stdin_open(&self) -> bool {
        self.stdin == PersistentCommandStdinState::Open
            && !self.revoked
            && !self.stop_requested
            && self.exit.is_none()
    }
}

struct CommandControlState {
    command_argv: Vec<String>,
    stdin_uncertain: bool,
    dimensions: Option<PersistentCommandDimensions>,
    dimensions_uncertain: bool,
}

impl CommandControlState {
    fn new(program: &str, args: &[String], dimensions: Option<(u16, u16)>) -> Self {
        Self {
            command_argv: std::iter::once(program.to_owned())
                .chain(args.iter().cloned())
                .collect(),
            stdin_uncertain: false,
            dimensions: dimensions
                .map(|(columns, rows)| PersistentCommandDimensions { columns, rows }),
            dimensions_uncertain: false,
        }
    }
}

struct Session {
    stdin: Arc<AsyncMutex<Option<Box<dyn AsyncWrite + Send + Unpin>>>>,
    output: Arc<OutputBuffer>,
    stop: CancellationToken,
    tree: Arc<Mutex<Option<SessionTree>>>,
    poll_lock: AsyncMutex<()>,
    controls: Mutex<CommandControlState>,
    pty: bool,
    #[cfg(unix)]
    terminal: Option<Arc<terminal::unix::Master>>,
    #[cfg(windows)]
    pty_console: Option<Arc<Mutex<Option<windows_pty::PseudoConsole>>>>,
    #[cfg(windows)]
    resize_lock: AsyncMutex<()>,
    #[cfg(windows)]
    sandbox_stdin: Option<Arc<AsyncMutex<Option<windows_sandbox::InputSender>>>>,
}

enum SessionTree {
    Pipes(ProcessTreeGuard),
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    LinuxSandbox(crate::command::sandbox::linux::LinuxSandboxGuard),
    #[cfg(windows)]
    Pty(windows_pty::PtyJob),
    #[cfg(windows)]
    Sandbox(Arc<windows_sandbox::SandboxRuntime>),
}

impl SessionTree {
    fn close(self) {
        match self {
            Self::Pipes(tree) => drop(tree),
            #[cfg(all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            ))]
            Self::LinuxSandbox(guard) => drop(guard),
            #[cfg(windows)]
            Self::Pty(job) => drop(job),
            #[cfg(windows)]
            Self::Sandbox(runtime) => runtime.kill(),
        }
    }
}

impl Session {
    fn mark_stdin_uncertain(&self) {
        self.controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .stdin_uncertain = true;
    }

    #[cfg(windows)]
    fn mark_dimensions_uncertain(&self) {
        self.controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .dimensions_uncertain = true;
    }

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

/// The reactor belongs to the manager, rather than to a model turn. In
/// particular, Tokio children and pipes must not be registered on a temporary
/// caller runtime which is dropped after the turn completes.
struct CommandRuntime {
    handle: tokio::runtime::Handle,
    shutdown: CancellationToken,
    shutdown_outputs: Arc<Mutex<Vec<Arc<OutputBuffer>>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CommandRuntime {
    fn new() -> Result<Self, String> {
        let shutdown = CancellationToken::new();
        let worker_shutdown = shutdown.clone();
        let shutdown_outputs = Arc::new(Mutex::new(Vec::<Arc<OutputBuffer>>::new()));
        let worker_outputs = Arc::clone(&shutdown_outputs);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("openide-command-reactor".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = sender.send(Err(error.to_string()));
                        return;
                    }
                };
                if sender.send(Ok(runtime.handle().clone())).is_ok() {
                    runtime.block_on(worker_shutdown.cancelled());
                    // Trees have already been closed by manager Drop. Give
                    // the monitors a bounded opportunity to reap children and
                    // close readers before cancelling runtime tasks.
                    let outputs = std::mem::take(
                        &mut *worker_outputs
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()),
                    );
                    runtime.block_on(async {
                        let _ = timeout(IO_WAIT, async {
                            while outputs
                                .iter()
                                .any(|output| !output.has_exited().unwrap_or(false))
                            {
                                sleep(Duration::from_millis(5)).await;
                            }
                        })
                        .await;
                    });
                }
                // Runtime destruction happens on its own thread, including
                // when the last manager is dropped from an async host task.
                // Blocking readers on Windows cannot delay shutdown forever.
                runtime.shutdown_timeout(IO_WAIT);
            })
            .map_err(|error| format!("cannot create command reactor: {error}"))?;
        match receiver.recv_timeout(IO_WAIT) {
            Ok(Ok(handle)) => Ok(Self {
                handle,
                shutdown,
                shutdown_outputs,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(format!("cannot initialize command reactor: {error}"))
            }
            Err(error) => {
                // If startup itself stalls, it has no command to execute. Do
                // not block the host joining an unresponsive startup thread.
                shutdown.cancel();
                Err(format!(
                    "command reactor startup was not confirmed: {error}"
                ))
            }
        }
    }
}

impl Drop for CommandRuntime {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(thread) = self.thread.take() {
            // The UI/async caller cannot wait for pipe-reader or blocking-task
            // grace periods. The detached worker owns its runtime and bounded
            // cleanup; process trees were synchronously closed before this.
            if thread.is_finished() && thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

pub struct PersistentCommandManager {
    generation: String,
    owner_session_id: Option<String>,
    workspace: PathBuf,
    isolation: CommandIsolation,
    limits: PersistentCommandLimits,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    closed: std::sync::atomic::AtomicBool,
    revoked: CancellationToken,
    pty_tools_enabled: bool,
    pty_resize_tools_enabled: bool,
    start_wait_tools: bool,
    host_terminal: bool,
    runtime: OnceLock<Result<CommandRuntime, String>>,
    native_output_store: Option<Arc<NativeCommandOutputStore>>,
}

impl Drop for PersistentCommandManager {
    fn drop(&mut self) {
        self.stop_all();
        if let Some(Ok(runtime)) = self.runtime.get() {
            *runtime
                .shutdown_outputs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = self
                .sessions
                .get_mut()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .values()
                .map(|session| Arc::clone(&session.output))
                .collect();
        }
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
            generation: Uuid::new_v4().to_string(),
            owner_session_id: None,
            workspace,
            isolation,
            limits,
            sessions: Mutex::new(HashMap::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            revoked: CancellationToken::new(),
            pty_tools_enabled: false,
            pty_resize_tools_enabled: false,
            start_wait_tools: true,
            host_terminal: false,
            runtime: OnceLock::new(),
            native_output_store: None,
        })
    }

    /// Bind an instance to one native session before any command is started.
    /// The trusted host verifies that this owner exists and is native; this
    /// primitive has no authority to inspect a session store or restore a job.
    pub fn with_native_session_owner(
        mut self,
        owner: impl Into<String>,
    ) -> Result<Self, AgentError> {
        let owner = owner.into();
        if owner.is_empty()
            || owner.len() > 256
            || !owner
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            || self.owner_session_id.is_some()
            || self.closed.load(std::sync::atomic::Ordering::Acquire)
            || !self
                .sessions
                .get_mut()
                .map_err(|_| AgentError::Tool("persistent command registry is unavailable".into()))?
                .is_empty()
        {
            return Err(AgentError::Tool(
                "persistent command native owner must be bound once before starting commands"
                    .into(),
            ));
        }
        self.owner_session_id = Some(owner);
        Ok(self)
    }

    /// Ephemeral UUID identifying this manager instance, never a durable job ID.
    pub fn generation(&self) -> &str {
        &self.generation
    }

    /// Attach the trusted native session's durable output store before starting
    /// commands. Registration still validates the owner/workspace against fresh
    /// session metadata; this builder grants no command permission.
    pub fn with_native_output_store(
        mut self,
        store: Arc<NativeCommandOutputStore>,
    ) -> Result<Self, AgentError> {
        if self.owner_session_id.is_none()
            || self.native_output_store.is_some()
            || self.runtime.get().is_some()
            || self.closed.load(std::sync::atomic::Ordering::Acquire)
            || !self
                .sessions
                .get_mut()
                .map_err(|_| AgentError::Tool("persistent command registry is unavailable".into()))?
                .is_empty()
        {
            return Err(AgentError::Tool(
                "native output store must be bound once to an owned manager before commands start"
                    .into(),
            ));
        }
        self.native_output_store = Some(store);
        Ok(self)
    }

    fn register_output(
        &self,
        id: &str,
        program: &str,
        args: &[String],
        pty: bool,
    ) -> Result<Option<Arc<NativeOutputCapture>>, AgentError> {
        let Some(store) = self.native_output_store.as_ref() else {
            return Ok(None);
        };
        let binding = NativeCommandOutputBinding {
            session_id: self.owner_session_id.clone().ok_or_else(|| {
                AgentError::Tool("native output has no verified manager owner".into())
            })?,
            workspace: self.workspace.clone(),
            manager_generation: self.generation.clone(),
            command_id: id.to_owned(),
        };
        let argv = std::iter::once(program.to_owned())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>();
        let isolation = match self.isolation {
            CommandIsolation::HostUnrestricted => "host_unrestricted",
            CommandIsolation::WorkspaceWriteOffline => "workspace_write_offline",
        };
        let registered = store
            .register(&binding, &argv, isolation, pty)
            .map_err(|error| {
                AgentError::Tool(format!(
                    "cannot register durable native output before command launch: {error}"
                ))
            })?;
        if !registered.newly_registered {
            return Err(AgentError::Tool(
                "native command output was already registered; a prior command must not be replayed"
                    .into(),
            ));
        }
        Ok(Some(Arc::new(NativeOutputCapture {
            store: Arc::clone(store),
            binding,
            cursor: Mutex::new(0),
        })))
    }

    pub fn native_session_owner(&self) -> Option<&str> {
        self.owner_session_id.as_deref()
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
    /// Host opt-in: on Unix, host-unrestricted commands run on a
    /// pseudo-terminal the host can show and type into. The tool schema, the
    /// cleared environment (plus `TERM`), approvals and the contained profile
    /// (which keeps pipes) are unchanged. No effect on other platforms.
    pub fn with_host_terminal(mut self) -> Self {
        self.host_terminal = true;
        self
    }

    /// A host tab's view of one started command: a copy of its output from
    /// the retained tail onward and, on a terminal, input and resize. Never
    /// consumes output the agent has not polled.
    pub fn attach_terminal(&self, id: &str) -> Result<PersistentTerminal, AgentError> {
        let session = self.session(id)?;
        Ok(PersistentTerminal {
            events: session.output.tap.attach(),
            #[cfg(unix)]
            master: session.terminal.clone(),
        })
    }

    /// Advertise and accept the tools exactly as they were before `timeout_ms`
    /// existed; used for children reserved under that catalog.
    pub fn with_tools_before_command_revision(mut self) -> Self {
        self.start_wait_tools = false;
        self
    }

    pub fn start_wait_tools_enabled(&self) -> bool {
        self.start_wait_tools
    }

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

    /// Copy the complete bounded pending output at one atomic observation.
    /// This neither drains bytes nor changes final delivery, registry entries,
    /// or session capacity. An agent poll may change the next observation or
    /// remove the ID; copies must replace previous previews, never form an
    /// incremental output history. No process or journal is created here.
    pub fn inspect_pending_output(
        &self,
        id: &str,
    ) -> Result<PersistentCommandInspection, AgentError> {
        let session = self.session(id)?;
        let state = session.output.state.lock().map_err(|_| {
            AgentError::Tool("persistent command output inspection is unavailable".into())
        })?;
        if state.final_delivered {
            return Err(AgentError::Tool(
                "persistent command session was already drained".into(),
            ));
        }
        Ok(PersistentCommandInspection {
            id: id.to_owned(),
            pty: session.pty,
            events: state.events.iter().cloned().collect(),
            exit: state.exit.clone(),
            reader_error: state.reader_error.clone(),
            retained_bytes: state.retained_bytes,
            max_buffered_output_bytes: self.limits.max_buffered_output_bytes,
        })
    }

    /// Observe control state without acquiring an async input lock, draining
    /// output, delivering a terminal result, or freeing a registered slot.
    /// Busy/uncertain state never grants input availability. A process, agent
    /// poll, revocation, or input operation can change the next observation.
    pub fn control_snapshot(
        &self,
        id: &str,
    ) -> Result<PersistentCommandControlInspection, AgentError> {
        let session = self.session(id)?;
        let controls = session.controls.lock().map_err(|_| {
            AgentError::Tool("persistent command control inspection is unavailable".into())
        })?;
        let stdin = if controls.stdin_uncertain {
            PersistentCommandStdinState::Uncertain
        } else {
            #[cfg(windows)]
            let observed = if let Some(input) = &session.sandbox_stdin {
                input.try_lock().map(|sender| sender.is_some())
            } else {
                session.stdin.try_lock().map(|writer| writer.is_some())
            };
            #[cfg(not(windows))]
            let observed = session.stdin.try_lock().map(|writer| writer.is_some());
            match observed {
                Ok(true) => PersistentCommandStdinState::Open,
                Ok(false) => PersistentCommandStdinState::Closed,
                Err(_) => PersistentCommandStdinState::Busy,
            }
        };
        let output = session.output.state.lock().map_err(|_| {
            AgentError::Tool("persistent command control inspection is unavailable".into())
        })?;
        if output.final_delivered {
            return Err(AgentError::Tool(
                "persistent command session was already drained".into(),
            ));
        }
        let revoked = self.closed.load(std::sync::atomic::Ordering::Acquire);
        let stop_requested = session.stop.is_cancelled();
        #[cfg(windows)]
        let console_open = session
            .pty_console
            .as_ref()
            .is_some_and(|console| console.try_lock().is_ok_and(|console| console.is_some()));
        #[cfg(not(windows))]
        let console_open = false;
        Ok(PersistentCommandControlInspection {
            generation: self.generation.clone(),
            owner_session_id: self.owner_session_id.clone(),
            id: id.to_owned(),
            workspace: self.workspace.clone(),
            isolation: self.isolation,
            command_argv: controls.command_argv.clone(),
            revoked,
            stop_requested,
            stdin,
            pty: session.pty,
            dimensions: controls.dimensions,
            dimensions_uncertain: controls.dimensions_uncertain,
            resize_enabled: self.pty_resize_tools_enabled()
                && session.pty
                && console_open
                && !controls.dimensions_uncertain
                && !revoked
                && !stop_requested
                && output.exit.is_none(),
            max_stdin_bytes: self.limits.max_stdin_bytes,
            exit: output.exit.clone(),
        })
    }

    pub fn command_argv(&self, id: &str) -> Result<Vec<String>, AgentError> {
        let session = self.session(id)?;
        let controls = session
            .controls
            .lock()
            .map_err(|_| AgentError::Tool("persistent command identity is unavailable".into()))?;
        Ok(controls.command_argv.clone())
    }

    /// Revoke the manager and synchronously close every process tree it owns.
    /// Existing session IDs can still be polled to deliver buffered output and
    /// their terminal state, but no new commands can be started.
    pub fn stop_all(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        // A sandbox handshake can hold the registry lock before dispatch.
        // Publish revocation first so that it cannot release its payload while
        // the caller waits for that lock to close existing trees.
        self.revoked.cancel();
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        if !self.isolation.is_supported_on_host() {
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
        #[cfg(not(windows))]
        if terminal.is_some() {
            return Err(AgentError::Tool(
                "persistent PTY containment is unavailable on this host; command was not started"
                    .into(),
            ));
        }
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
        let runtime = self
            .runtime
            .get_or_init(CommandRuntime::new)
            .as_ref()
            .map_err(|error| AgentError::Tool(error.clone()))?
            .handle
            .clone();
        // Register the process/pipes with the manager's reactor even when
        // `start` is invoked from a different (or no) Tokio runtime.
        let _runtime_guard = runtime.enter();
        if cancellation.is_cancelled() || self.revoked.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let id = Uuid::new_v4().to_string();
        let capture = self.register_output(&id, program, args, terminal.is_some())?;
        if cancellation.is_cancelled() || self.revoked.is_cancelled() {
            return Err(AgentError::Cancelled);
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
                NativeOutputRegistration { id, capture },
            );
        }

        #[cfg(windows)]
        if let Some((columns, rows)) = terminal {
            return self.start_pty_locked(
                program,
                args,
                (columns, rows),
                &runtime,
                &mut sessions,
                NativeOutputRegistration { id, capture },
            );
        }
        #[cfg(unix)]
        let mut host_master: Option<Arc<terminal::unix::Master>> = None;
        let (mut child, tree) = if self.isolation == CommandIsolation::WorkspaceWriteOffline {
            #[cfg(all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            ))]
            {
                self.spawn_linux_sandbox(program, args, cancellation, &runtime)?
            }
            #[cfg(not(all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            )))]
            {
                return Err(AgentError::Tool(
                    "persistent sandbox is unavailable; command was not started".into(),
                ));
            }
        } else {
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
            if self.host_terminal {
                let io_error = |error: std::io::Error| {
                    AgentError::Tool(format!("cannot open a terminal for the command: {error}"))
                };
                let (master, slave) = terminal::unix::open(120, 30).map_err(io_error)?;
                command
                    .stdin(Stdio::from(slave.try_clone().map_err(io_error)?))
                    .stdout(Stdio::from(slave.try_clone().map_err(io_error)?))
                    .stderr(Stdio::from(slave))
                    .env("TERM", "xterm-256color");
                // SAFETY: the hook only makes async-signal-safe calls.
                unsafe { command.pre_exec(terminal::unix::become_session_leader) };
                host_master = Some(master);
            } else {
                command.process_group(0);
            }
            #[cfg(windows)]
            let tree = ProcessTreeGuard::new().map_err(|error| {
                AgentError::Tool(format!("cannot create persistent command job: {error}"))
            })?;
            if let Some(error) = crate::command_line::missing_working_directory(&self.workspace) {
                return Err(error);
            }
            let spawned = command.spawn();
            // The parent's copies of the slave must close, or the master
            // never sees the end of output.
            drop(command);
            let mut child = spawned.map_err(|error| {
                crate::command_line::spawn_error("cannot start persistent command", program, &error)
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
            (child, SessionTree::Pipes(tree))
        };
        if cancellation.is_cancelled() || self.revoked.is_cancelled() {
            tree.close();
            let _ = child.start_kill();
            return Err(AgentError::EffectUncertain(
                "persistent command was cancelled after launch; it may have produced effects"
                    .into(),
            ));
        }
        let output = Arc::new(
            OutputBuffer::new(self.limits.max_buffered_output_bytes).with_capture(capture),
        );
        type Reader = tokio::task::JoinHandle<std::io::Result<()>>;
        let piped = |child: &mut Child| -> Result<_, AgentError> {
            let missing = |name: &str| {
                AgentError::EffectUncertain(format!(
                    "persistent command {name} is unavailable after spawn"
                ))
            };
            let stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
            let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
            let stderr = child.stderr.take().ok_or_else(|| missing("stderr"))?;
            let readers: (Reader, Reader) = (
                runtime.spawn(read_pipe(
                    stdout,
                    PersistentCommandStream::Stdout,
                    Arc::clone(&output),
                )),
                runtime.spawn(read_pipe(
                    stderr,
                    PersistentCommandStream::Stderr,
                    Arc::clone(&output),
                )),
            );
            Ok((
                Box::new(stdin) as Box<dyn AsyncWrite + Send + Unpin>,
                readers,
            ))
        };
        #[cfg(unix)]
        let (stdin, (stdout_reader, stderr_reader)) = match &host_master {
            // One merged terminal stream; the second reader has nothing to read.
            Some(master) => (
                Box::new(terminal::unix::MasterIo(Arc::clone(master)))
                    as Box<dyn AsyncWrite + Send + Unpin>,
                (
                    runtime.spawn(read_pipe(
                        terminal::unix::MasterIo(Arc::clone(master)),
                        PersistentCommandStream::Terminal,
                        Arc::clone(&output),
                    )),
                    runtime.spawn(async { Ok(()) }),
                ),
            ),
            None => piped(&mut child)?,
        };
        #[cfg(not(unix))]
        let (stdin, (stdout_reader, stderr_reader)) = piped(&mut child)?;
        let stdin = Arc::new(AsyncMutex::new(Some(stdin)));
        let stop = output.stop.clone();
        let tree = Arc::new(Mutex::new(Some(tree)));
        output.attach_tree(Arc::clone(&tree));
        let session = Arc::new(Session {
            stdin: Arc::clone(&stdin),
            output: Arc::clone(&output),
            stop: stop.clone(),
            tree: Arc::clone(&tree),
            poll_lock: AsyncMutex::new(()),
            controls: Mutex::new(CommandControlState::new(program, args, None)),
            #[cfg(unix)]
            pty: host_master.is_some(),
            #[cfg(not(unix))]
            pty: false,
            #[cfg(unix)]
            terminal: host_master,
            #[cfg(windows)]
            pty_console: None,
            #[cfg(windows)]
            resize_lock: AsyncMutex::new(()),
            #[cfg(windows)]
            sandbox_stdin: None,
        });
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
        sessions.insert(id.clone(), session);
        Ok(PersistentCommandStarted { id })
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    fn spawn_linux_sandbox(
        &self,
        program: &str,
        args: &[String],
        cancellation: &CancellationToken,
        runtime: &tokio::runtime::Handle,
    ) -> Result<(Child, SessionTree), AgentError> {
        let program = program.to_owned();
        let args = args.to_vec();
        let workspace = self.workspace.clone();
        let revoked = self.revoked.clone();
        let cancel = cancellation.child_token();
        let worker_cancel = cancel.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        runtime.spawn(async move {
            let result = async {
                // Filesystem validation must not block this manager's live
                // pipe readers. Dropped/cancelled preparation can finish in
                // the background, but it has no authority to spawn a target.
                let prepare_cancel = worker_cancel.clone();
                let prepare = tokio::task::spawn_blocking(move || {
                    crate::command::sandbox::linux::prepare_with_cancel(
                        &program, &args, &workspace, &prepare_cancel,
                    )
                });
                let mut launch = tokio::select! {
                    biased;
                    _ = worker_cancel.cancelled() => return Err(AgentError::Cancelled),
                    _ = revoked.cancelled() => return Err(AgentError::Cancelled),
                    result = prepare => result.map_err(|error| AgentError::Tool(format!("persistent sandbox preparation failed before launch: {error}")))??,
                };
                launch.command().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
                if worker_cancel.is_cancelled() || revoked.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                let spawn = launch.spawn(&worker_cancel);
                tokio::pin!(spawn);
                let result = tokio::select! {
                    biased;
                    _ = revoked.cancelled() => {
                        worker_cancel.cancel();
                        // Keep the backend alive to finish its bounded cleanup
                        // and preserve its classification at the target gate.
                        spawn.await
                    },
                    result = &mut spawn => result,
                }?;
                Ok((result.0, SessionTree::LinuxSandbox(result.1)))
            }.await;
            // A caller timeout cannot orphan an unregistered successful launch:
            // SendError drops both Child and namespace supervisor guard.
            let _ = sender.send(result);
        });
        match receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(result) => result,
            Err(error) => {
                cancel.cancel();
                Err(AgentError::EffectUncertain(format!(
                    "persistent sandbox setup completion was not confirmed; cleanup was requested and target outcome is unknown: {error}"
                )))
            }
        }
    }

    #[cfg(windows)]
    fn start_pty_locked(
        &self,
        program: &str,
        args: &[String],
        dimensions: (u16, u16),
        runtime: &tokio::runtime::Handle,
        sessions: &mut HashMap<String, Arc<Session>>,
        registration: NativeOutputRegistration,
    ) -> Result<PersistentCommandStarted, AgentError> {
        let (columns, rows) = dimensions;
        let NativeOutputRegistration { id, capture } = registration;
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
        let output = Arc::new(
            OutputBuffer::new(self.limits.max_buffered_output_bytes).with_capture(capture),
        );
        let stdin = Arc::new(AsyncMutex::new(Some(
            Box::new(stdin) as Box<dyn AsyncWrite + Send + Unpin>
        )));
        let stop = output.stop.clone();
        let tree = Arc::new(Mutex::new(Some(SessionTree::Pty(job))));
        output.attach_tree(Arc::clone(&tree));
        let pty_console = Arc::new(Mutex::new(Some(console)));
        let session = Arc::new(Session {
            stdin: Arc::clone(&stdin),
            output: Arc::clone(&output),
            stop: stop.clone(),
            tree: Arc::clone(&tree),
            poll_lock: AsyncMutex::new(()),
            controls: Mutex::new(CommandControlState::new(
                program,
                args,
                Some((columns, rows)),
            )),
            pty: true,
            pty_console: Some(Arc::clone(&pty_console)),
            resize_lock: AsyncMutex::new(()),
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
            let result =
                windows_sandbox::write_stdin(&session, bytes, close_after, cancellation).await;
            if matches!(result, Err(AgentError::EffectUncertain(_))) {
                session.mark_stdin_uncertain();
            }
            return result;
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
            let mut operation = InputEffectGuard {
                session: &session,
                completed: false,
            };
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
                return Err(AgentError::EffectUncertain(format!(
                    "persistent command stdin write {reason}; partial input may have been delivered"
                )));
            }
            operation.completed = true;
        }
        if close_after {
            // Dropping a terminal's writer closes nothing: end of input on a
            // terminal is the EOF character at the start of a line.
            #[cfg(unix)]
            if session.terminal.is_some() {
                if let Some(writer) = stdin.as_mut() {
                    let _ = timeout(IO_WAIT, writer.write_all(b"\x04")).await;
                }
            }
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
            Err(AgentError::Tool(
                "persistent PTY resize is unavailable on this host".into(),
            ))
        }
        #[cfg(windows)]
        {
            let session = self.session(id)?;
            let _resize_guard = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
                _ = session.stop.cancelled() => return Err(AgentError::Tool("persistent command is no longer running".into())),
                guard = session.resize_lock.lock() => guard,
            };
            let console = session.pty_console.as_ref().ok_or_else(|| {
                AgentError::Tool("persistent command is not a PTY session".into())
            })?;
            if session.stop.is_cancelled() || session.output.has_exited()? {
                return Err(AgentError::Tool(
                    "persistent command is no longer running".into(),
                ));
            }
            let console = Arc::clone(console);
            session.mark_dimensions_uncertain();
            let mut effect = ResizeEffectGuard {
                session: &session,
                completed: false,
            };
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
                Ok(Ok(Ok(Some(())))) => {
                    let mut controls = session.controls.lock().map_err(|_| {
                        AgentError::EffectUncertain(
                            "PTY resized but control state is unavailable".into(),
                        )
                    })?;
                    controls.dimensions = Some(PersistentCommandDimensions { columns, rows });
                    controls.dimensions_uncertain = false;
                    effect.completed = true;
                    Ok(())
                }
                Ok(Ok(Ok(None))) => {
                    session
                        .controls
                        .lock()
                        .map_err(|_| {
                            AgentError::Tool(
                                "persistent command control state is unavailable".into(),
                            )
                        })?
                        .dimensions_uncertain = false;
                    effect.completed = true;
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

struct InputEffectGuard<'a> {
    session: &'a Session,
    completed: bool,
}

impl Drop for InputEffectGuard<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.session.mark_stdin_uncertain();
            self.session.stop_tree();
        }
    }
}

#[cfg(windows)]
struct ResizeEffectGuard<'a> {
    session: &'a Session,
    completed: bool,
}

#[cfg(windows)]
impl Drop for ResizeEffectGuard<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.session.mark_dimensions_uncertain();
            self.session.stop_tree();
        }
    }
}

struct NativeOutputCapture {
    store: Arc<NativeCommandOutputStore>,
    binding: NativeCommandOutputBinding,
    cursor: Mutex<u64>,
}

#[cfg(windows)]
struct NativeOutputRegistration {
    id: String,
    capture: Option<Arc<NativeOutputCapture>>,
}

struct OutputBuffer {
    state: Mutex<OutputState>,
    changed: Notify,
    max_bytes: usize,
    capture: Option<Arc<NativeOutputCapture>>,
    capture_failed: std::sync::atomic::AtomicBool,
    capture_error: Mutex<Option<String>>,
    stop: CancellationToken,
    failure_tree: Mutex<Option<Arc<Mutex<Option<SessionTree>>>>>,
    /// A copy for the host's tab; independent of retention and of the cursor.
    tap: terminal::TerminalTap,
}

impl OutputBuffer {
    fn new(max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(OutputState::default()),
            changed: Notify::new(),
            max_bytes,
            capture: None,
            capture_failed: std::sync::atomic::AtomicBool::new(false),
            capture_error: Mutex::new(None),
            stop: CancellationToken::new(),
            failure_tree: Mutex::new(None),
            tap: terminal::TerminalTap::default(),
        }
    }

    fn with_capture(mut self, capture: Option<Arc<NativeOutputCapture>>) -> Self {
        self.capture = capture;
        self
    }

    fn attach_tree(&self, tree: Arc<Mutex<Option<SessionTree>>>) {
        *self
            .failure_tree
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tree);
    }

    fn fail_capture(&self, error: String) -> std::io::Error {
        self.capture_failed
            .store(true, std::sync::atomic::Ordering::Release);
        self.stop.cancel();
        if let Some(tree) = self
            .failure_tree
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            if let Some(tree) = tree
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                tree.close();
            }
        }
        let mut saved = self
            .capture_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = saved.get_or_insert(error).clone();
        // The monitor owns the tree guard and must stop it immediately rather
        // than leaving the child alive until its ordinary lifetime deadline.
        self.changed.notify_waiters();
        std::io::Error::other(original)
    }

    fn push(&self, stream: PersistentCommandStream, bytes: &[u8]) -> std::io::Result<()> {
        self.tap.push(bytes);
        let mut committed_cursor = match self.capture.as_ref() {
            Some(capture) => Some(capture.cursor.lock().map_err(|_| {
                self.fail_capture("native output producer cursor is unavailable".into())
            })?),
            None => None,
        };
        if self
            .capture_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(self.fail_capture("native output capture already failed".into()));
        }
        let (keep, omitted) = if let Some(capture) = self.capture.as_ref() {
            let cursor = committed_cursor.as_mut().expect("capture cursor exists");
            let stream = match stream {
                PersistentCommandStream::Stdout => NativeCommandOutputStream::Stdout,
                PersistentCommandStream::Stderr => NativeCommandOutputStream::Stderr,
                PersistentCommandStream::Terminal => NativeCommandOutputStream::Terminal,
            };
            let ack = capture
                .store
                .append(&capture.binding, **cursor, stream, bytes)
                .map_err(|error| {
                    self.fail_capture(format!(
                        "durable native output append was not confirmed; command stopped: {error}"
                    ))
                })?;
            if ack.retained_bytes.checked_add(ack.omitted_bytes) != Some(bytes.len())
                || (**cursor).checked_add(bytes.len() as u64) != Some(ack.observed_end)
            {
                return Err(self.fail_capture("native output ACK has invalid byte bounds".into()));
            }
            **cursor = ack.observed_end;
            (ack.retained_bytes, ack.omitted_bytes)
        } else {
            (bytes.len(), 0)
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.push(stream, &bytes[..keep], self.max_bytes);
        state.push_omitted(omitted);
        // The cursor lock stays held through publication, so a second pipe's
        // ACK cannot overtake this pipe in the observable buffer.
        self.changed.notify_waiters();
        Ok(())
    }

    fn finish(&self, exit: PersistentCommandExit, mut reader_error: Option<String>) {
        self.tap.finish(&exit);
        if let Some(error) = self
            .capture_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            reader_error = Some(match reader_error {
                Some(reader) if reader.contains(&error) => reader,
                Some(reader) => format!("{reader}; {error}"),
                None => error,
            });
        }
        if let Some(capture) = self.capture.as_ref() {
            let (kind, code) = match &exit {
                PersistentCommandExit::Exited { code } => (NativeCommandOutputExit::Exited, *code),
                PersistentCommandExit::Terminated => (NativeCommandOutputExit::Terminated, None),
                PersistentCommandExit::TimedOut => (NativeCommandOutputExit::TimedOut, None),
                PersistentCommandExit::WaitFailed(error) => (
                    NativeCommandOutputExit::WaitFailed {
                        error: bounded_native_output_error(error),
                    },
                    None,
                ),
            };
            let terminal = NativeCommandOutputTerminal {
                exit_observed: true,
                exit_kind: Some(kind),
                exit_code: code,
                reader_error: reader_error.as_deref().map(bounded_native_output_error),
            };
            let result = capture
                .cursor
                .lock()
                .map_err(|_| {
                    "native output producer cursor is unavailable at terminal observation"
                        .to_owned()
                })
                .and_then(|cursor| {
                    capture
                        .store
                        .finish(&capture.binding, *cursor, &terminal)
                        .map_err(|error| error.to_string())
                });
            if let Err(error) = result {
                let message = format!("durable native output terminal was not confirmed: {error}");
                self.fail_capture(message.clone());
                reader_error = Some(match reader_error {
                    Some(reader) => format!("{reader}; {message}"),
                    None => message,
                });
            }
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.exit = Some(exit);
        state.reader_error = reader_error;
        self.changed.notify_waiters();
    }

    async fn finish_async(
        self: &Arc<Self>,
        exit: PersistentCommandExit,
        reader_error: Option<String>,
    ) {
        if self.capture.is_none() {
            self.finish(exit, reader_error);
            return;
        }
        let output = Arc::clone(self);
        let saved_exit = exit.clone();
        if let Err(error) =
            tokio::task::spawn_blocking(move || output.finish(exit, reader_error)).await
        {
            self.fail_capture(format!("native output terminal worker failed: {error}"));
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.exit = Some(saved_exit);
            state.reader_error = self
                .capture_error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            self.changed.notify_waiters();
        }
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

fn bounded_native_output_error(error: &str) -> String {
    if error.len() <= MAX_NATIVE_OUTPUT_ERROR_BYTES {
        return error.to_owned();
    }
    let marker = "\n[diagnostic truncated to 4096 bytes]";
    let mut end = MAX_NATIVE_OUTPUT_ERROR_BYTES - marker.len();
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &error[..end])
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
        self.push_omitted(omitted);
    }

    fn push_omitted(&mut self, omitted: usize) {
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
        let count = match pipe.read(&mut chunk).await {
            Ok(count) => count,
            Err(error) => {
                output.fail_capture(error.to_string());
                return Err(error);
            }
        };
        if count == 0 {
            return Ok(());
        }
        if output.capture.is_some() {
            let captured = Arc::clone(&output);
            let bytes = chunk[..count].to_vec();
            tokio::task::spawn_blocking(move || captured.push(stream, &bytes))
                .await
                .map_err(|error| {
                    output.fail_capture(format!("native output reader worker failed: {error}"))
                })??;
        } else {
            output.push(stream, &chunk[..count])?;
        }
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
        let count = match pipe.read(&mut chunk) {
            Ok(count) => count,
            Err(error) => {
                output.fail_capture(error.to_string());
                return Err(error);
            }
        };
        if count == 0 {
            return Ok(());
        }
        output.push(stream, &chunk[..count])?;
    }
}

async fn lifetime_elapsed(lifetime: Option<Duration>) {
    match lifetime {
        Some(lifetime) => sleep(lifetime).await,
        None => std::future::pending().await,
    }
}

struct Monitor {
    child: Child,
    tree: Arc<Mutex<Option<SessionTree>>>,
    stdin: Arc<AsyncMutex<Option<Box<dyn AsyncWrite + Send + Unpin>>>>,
    output: Arc<OutputBuffer>,
    stop: CancellationToken,
    lifetime: Option<Duration>,
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
            _ = lifetime_elapsed(lifetime) => PersistentCommandExit::TimedOut,
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
        output.finish_async(exit, reader_error).await;
    }
}

#[cfg(windows)]
struct PtyMonitor {
    process: Arc<std::os::windows::io::OwnedHandle>,
    cleanup: PtyCleanup,
    stdin: Arc<AsyncMutex<Option<Box<dyn AsyncWrite + Send + Unpin>>>>,
    output: Arc<OutputBuffer>,
    stop: CancellationToken,
    lifetime: Option<Duration>,
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
            _ = lifetime_elapsed(lifetime) => PersistentCommandExit::TimedOut,
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
        output.finish_async(exit, reader_error).await;
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
#[path = "persistent_command_inspection_tests.rs"]
mod inspection_tests;

#[cfg(test)]
#[path = "persistent_command_control_tests.rs"]
mod control_tests;

#[cfg(test)]
#[path = "persistent_command_output_tests.rs"]
mod output_tests;

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

    #[cfg(not(any(
        windows,
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )))]
    #[test]
    fn unsupported_platform_offline_isolation_rejects_persistent_start_before_spawn() {
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
                max_lifetime: Some(Duration::from_secs(20)),
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
                max_lifetime: Some(Duration::from_millis(250)),
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
                max_lifetime: Some(Duration::from_millis(500)),
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
            max_lifetime: Some(Duration::from_millis(250)),
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
