use crate::session::{Envelope, EventBuffer, MAX_SOURCES};
use crate::*;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
    Arc,
};
use std::time::Instant;

const MAX_PENDING: usize = 128;
enum Incoming {
    Message(Value),
    Closed,
    Failed(String),
    Stderr(String),
}
struct Process {
    child: Child,
    cleaned: bool,
}
impl Process {
    fn kill(&mut self) {
        if self.cleaned {
            return;
        }
        self.cleaned = true;
        // This group was created solely for this explicit session. Never signal
        // an inherited process group or an unrelated externally attached process.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.kill();
    }
}

fn spawn(
    config: &DebugConfig,
) -> Result<(Process, SyncSender<Vec<u8>>, Receiver<Incoming>), DebugError> {
    let mut command = Command::new(&config.executable);
    command
        .args(&config.args)
        .envs(&config.env)
        .current_dir(&config.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut process = Process {
        child: command
            .spawn()
            .map_err(|e| error(format!("Cannot start debug adapter: {e}")))?,
        cleaned: false,
    };
    let mut input = process
        .child
        .stdin
        .take()
        .ok_or_else(|| error("Adapter stdin unavailable"))?;
    let output = process
        .child
        .stdout
        .take()
        .ok_or_else(|| error("Adapter stdout unavailable"))?;
    let mut stderr = process
        .child
        .stderr
        .take()
        .ok_or_else(|| error("Adapter stderr unavailable"))?;
    let (outgoing, writes) = mpsc::sync_channel::<Vec<u8>>(16);
    let (incoming, reads) = mpsc::sync_channel(16);
    let failed = incoming.clone();
    std::thread::Builder::new()
        .name("dap-write".into())
        .spawn(move || {
            while let Ok(bytes) = writes.recv() {
                if let Err(error) = input.write_all(&bytes).and_then(|_| input.flush()) {
                    let _ = failed.send(Incoming::Failed(format!("DAP write failed: {error}")));
                    break;
                }
            }
            // Closing stdin after disconnect is required by adapters such as debugpy.
        })
        .map_err(|e| error(format!("Cannot start DAP writer: {e}")))?;
    let reader_events = incoming.clone();
    std::thread::Builder::new()
        .name("dap-read".into())
        .spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let message = match protocol::read_message(&mut reader) {
                    Ok(Some(message)) => Incoming::Message(message),
                    Ok(None) => {
                        let _ = reader_events.send(Incoming::Closed);
                        break;
                    }
                    Err(error) => {
                        let _ = reader_events.send(Incoming::Failed(error.message));
                        break;
                    }
                };
                if reader_events.send(message).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| error(format!("Cannot start DAP reader: {e}")))?;
    std::thread::Builder::new()
        .name("dap-stderr".into())
        .spawn(move || {
            let mut bytes = [0_u8; 4096];
            while let Ok(count) = stderr.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                if incoming
                    .send(Incoming::Stderr(
                        String::from_utf8_lossy(&bytes[..count]).into_owned(),
                    ))
                    .is_err()
                {
                    break;
                }
            }
        })
        .map_err(|e| error(format!("Cannot start DAP stderr reader: {e}")))?;
    Ok((process, outgoing, reads))
}

#[derive(Clone)]
struct DesiredSource {
    lines: Vec<u32>,
    generation: u64,
    request_id: Option<RequestId>,
}
enum Kind {
    Initialize,
    Launch,
    Configuration,
    ConfigurationDone,
    Disconnect,
    Breakpoints {
        path: PathBuf,
        lines: Vec<u32>,
        generation: u64,
        startup: bool,
    },
    Threads,
    Stack {
        thread_id: i64,
    },
    Scopes {
        frame_id: i64,
    },
    Variables {
        reference: i64,
    },
    Resume {
        thread_id: i64,
    },
    Pause,
}
struct Pending {
    id: Option<RequestId>,
    command: &'static str,
    kind: Kind,
    deadline: Instant,
    stop_id: Option<u64>,
}
struct Engine {
    config: DebugConfig,
    writer: Option<SyncSender<Vec<u8>>>,
    events: Arc<EventBuffer>,
    sequence: i64,
    pending: HashMap<i64, Pending>,
    capabilities: DebugCapabilities,
    initialized: bool,
    launch_sent: bool,
    launch_accepted: bool,
    configuration_started: bool,
    configuration_done_sent: bool,
    configured: bool,
    configuring: usize,
    configuration_queue: VecDeque<PathBuf>,
    desired: HashMap<PathBuf, DesiredSource>,
    breakpoint_ids: HashMap<i64, (PathBuf, u64)>,
    state: DebugState,
    stop_id: u64,
    frames: HashSet<i64>,
    references: HashSet<i64>,
    threads: HashSet<i64>,
    thread_details: Vec<Thread>,
    resume_pending: bool,
    shutting_down: Option<Instant>,
    disconnect_acknowledged: bool,
    startup_deadline: Instant,
}

pub(crate) fn run(
    config: DebugConfig,
    commands: Receiver<Envelope>,
    events: Arc<EventBuffer>,
    cancel: Arc<AtomicBool>,
) {
    events.emit(DebugEvent::State(DebugState::Starting));
    let result = spawn(&config);
    let (mut process, writer, incoming) = match result {
        Ok(result) => result,
        Err(error) => {
            events.finish(error.message);
            return;
        }
    };
    let mut engine = Engine::new(config, writer, events.clone());
    let result = engine.run(&mut process, &commands, &incoming, cancel);
    let reason = if events.overflow.load(Ordering::Acquire) {
        "Debug event budget exceeded; session stopped".to_owned()
    } else {
        result.unwrap_or_else(|error| error.message)
    };
    // Error paths also ask the adapter to terminate its launch before falling
    // back to killing the isolated adapter process group. This wait is worker-only.
    if !engine.disconnect_acknowledged && engine.writer.is_some() {
        if engine.shutting_down.is_none() {
            let _ = engine.shutdown();
        }
        let deadline = engine.shutting_down.unwrap_or_else(Instant::now);
        while Instant::now() < deadline && !engine.disconnect_acknowledged {
            match incoming.recv_timeout(Duration::from_millis(10)) {
                Ok(Incoming::Message(message)) => {
                    let _ = engine.message(message);
                }
                Ok(Incoming::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
        }
    }
    for (_, pending) in engine.pending.drain() {
        events.emit(DebugEvent::RequestFailed {
            request_id: pending.id,
            command: pending.command.into(),
            message: reason.clone(),
        });
    }
    engine.writer.take();
    process.kill();
    events.finish(reason);
}

impl Engine {
    fn new(config: DebugConfig, writer: SyncSender<Vec<u8>>, events: Arc<EventBuffer>) -> Self {
        let desired = config
            .breakpoints
            .iter()
            .map(|source| {
                (
                    source.path.clone(),
                    DesiredSource {
                        lines: source.lines.clone(),
                        generation: 0,
                        request_id: None,
                    },
                )
            })
            .collect();
        let startup_deadline = Instant::now() + config.request_timeout;
        Self {
            config,
            writer: Some(writer),
            events,
            sequence: 0,
            pending: HashMap::new(),
            capabilities: DebugCapabilities::default(),
            initialized: false,
            launch_sent: false,
            launch_accepted: false,
            configuration_started: false,
            configuration_done_sent: false,
            configured: false,
            configuring: 0,
            configuration_queue: VecDeque::new(),
            desired,
            breakpoint_ids: HashMap::new(),
            state: DebugState::Starting,
            stop_id: 0,
            frames: HashSet::new(),
            references: HashSet::new(),
            threads: HashSet::new(),
            thread_details: Vec::new(),
            resume_pending: false,
            shutting_down: None,
            disconnect_acknowledged: false,
            startup_deadline,
        }
    }
    fn state(&mut self, state: DebugState) {
        if self.state != state {
            self.state = state;
            self.events.emit(DebugEvent::State(state));
        }
    }
    fn write(&self, message: Value) -> Result<(), DebugError> {
        let bytes = protocol::frame(&message)?;
        self.writer
            .as_ref()
            .ok_or_else(|| error("Adapter stdin is closed"))?
            .try_send(bytes)
            .map_err(|e| {
                error(match e {
                    mpsc::TrySendError::Full(_) => "DAP writer queue is full",
                    mpsc::TrySendError::Disconnected(_) => "DAP writer disconnected",
                })
            })
    }
    fn request(
        &mut self,
        command: &'static str,
        arguments: Value,
        kind: Kind,
        id: Option<RequestId>,
        stop_id: Option<u64>,
    ) -> Result<(), DebugError> {
        if self.pending.len() >= MAX_PENDING {
            return Err(error("Too many pending debug requests"));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| error("DAP sequence exhausted"))?;
        self.write(
            json!({"seq":self.sequence,"type":"request","command":command,"arguments":arguments}),
        )?;
        self.pending.insert(
            self.sequence,
            Pending {
                id,
                command,
                kind,
                deadline: Instant::now() + self.config.request_timeout,
                stop_id,
            },
        );
        Ok(())
    }
    fn fail(&self, id: Option<RequestId>, command: &str, message: impl Into<String>) {
        self.events.emit(DebugEvent::RequestFailed {
            request_id: id,
            command: command.into(),
            message: message.into(),
        });
    }
    fn refresh_threads(&mut self) -> Result<(), DebugError> {
        if self
            .pending
            .values()
            .any(|pending| pending.id.is_none() && matches!(pending.kind, Kind::Threads))
        {
            return Ok(());
        }
        self.request("threads", json!({}), Kind::Threads, None, None)
    }
    fn run(
        &mut self,
        process: &mut Process,
        commands: &Receiver<Envelope>,
        incoming: &Receiver<Incoming>,
        cancel: Arc<AtomicBool>,
    ) -> Result<String, DebugError> {
        self.state(DebugState::Initializing);
        self.request(
            "initialize",
            json!({
                "clientID":"openide", "clientName":"OpenIDE", "adapterID":self.config.adapter_id,
                "pathFormat":"path", "linesStartAt1":true, "columnsStartAt1":true,
                "supportsVariableType":true, "supportsVariablePaging":true,
                "supportsRunInTerminalRequest":false, "supportsStartDebuggingRequest":false,
                "supportsProgressReporting":false, "supportsInvalidatedEvent":true
            }),
            Kind::Initialize,
            None,
            None,
        )?;
        loop {
            if (cancel.load(Ordering::Acquire) || self.events.overflow.load(Ordering::Acquire))
                && self.shutting_down.is_none()
            {
                self.shutdown()?;
            }
            if self
                .shutting_down
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Ok("Debug session stopped after adapter shutdown timeout".into());
            }
            if self.shutting_down.is_none() {
                for _ in 0..32 {
                    match commands.try_recv() {
                        Ok(envelope) => self.command(envelope),
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            self.shutdown()?;
                            break;
                        }
                    }
                }
            }
            let mut idle = false;
            match incoming.recv_timeout(Duration::from_millis(10)) {
                Ok(Incoming::Message(message)) => self.message(message)?,
                Ok(Incoming::Stderr(output)) => self.events.emit(DebugEvent::Output {
                    category: "adapter".into(),
                    output,
                }),
                Ok(Incoming::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Ok(
                        if self.shutting_down.is_some() || self.disconnect_acknowledged {
                            "Debug session disconnected".into()
                        } else {
                            "Debug adapter closed its output unexpectedly".into()
                        },
                    );
                }
                Ok(Incoming::Failed(message)) => return Err(error(message)),
                Err(mpsc::RecvTimeoutError::Timeout) => idle = true,
            }
            let expired: Vec<_> = self
                .pending
                .iter()
                .filter(|(_, pending)| pending.deadline <= Instant::now())
                .map(|(seq, _)| *seq)
                .collect();
            for seq in expired {
                let pending = self.pending.remove(&seq).unwrap();
                self.fail(
                    pending.id,
                    pending.command,
                    "Debug adapter request timed out",
                );
                if matches!(
                    pending.kind,
                    Kind::Initialize
                        | Kind::Launch
                        | Kind::Configuration
                        | Kind::ConfigurationDone
                        | Kind::Breakpoints { startup: true, .. }
                ) {
                    return Err(error(format!("DAP {} timed out", pending.command)));
                }
                if matches!(pending.kind, Kind::Resume { .. }) {
                    self.resume_pending = false;
                }
            }
            if self.shutting_down.is_none()
                && !self.configuration_started
                && Instant::now() >= self.startup_deadline
            {
                return Err(error("Debug adapter did not finish initialization"));
            }
            // Read the final queued messages before observing adapter exit.
            if idle {
                if let Some(status) = process
                    .child
                    .try_wait()
                    .map_err(|e| error(format!("Adapter status: {e}")))?
                {
                    if self.shutting_down.is_some() || status.success() {
                        return Ok("Debug adapter exited".into());
                    }
                    return Err(error(format!("Debug adapter exited with {status}")));
                }
            }
        }
    }
    fn shutdown(&mut self) -> Result<(), DebugError> {
        self.state(DebugState::Stopping);
        self.shutting_down = Some(Instant::now() + self.config.shutdown_timeout);
        self.frames.clear();
        self.references.clear();
        // This client only launches its own debuggee. DAP disconnect explicitly
        // terminates that debuggee, then closing stdin lets stdio adapters exit.
        self.request(
            "disconnect",
            json!({"restart":false,"terminateDebuggee":true}),
            Kind::Disconnect,
            None,
            None,
        )
    }
    fn maybe_configure(&mut self) -> Result<(), DebugError> {
        if !self.initialized || !self.launch_sent || self.configuration_started {
            return Ok(());
        }
        self.configuration_started = true;
        self.state(DebugState::Configuring);
        self.configuration_queue
            .extend(self.desired.keys().cloned());
        if let Some(filters) = self
            .capabilities
            .additional
            .get("exceptionBreakpointFilters")
            .and_then(Value::as_array)
        {
            if !filters.is_empty() {
                let filters: Vec<_> = filters
                    .iter()
                    .filter(|filter| filter["default"].as_bool() == Some(true))
                    .filter_map(|filter| filter["filter"].as_str())
                    .map(str::to_owned)
                    .collect();
                self.configuring += 1;
                self.request(
                    "setExceptionBreakpoints",
                    json!({"filters":filters}),
                    Kind::Configuration,
                    None,
                    None,
                )?;
            }
        }
        self.maybe_configuration_done()
    }
    fn set_breakpoints(
        &mut self,
        path: PathBuf,
        source: DesiredSource,
        startup: bool,
    ) -> Result<(), DebugError> {
        if startup {
            self.configuring += 1;
        }
        let source_json =
            json!({"path":path,"name":path.file_name().and_then(|name| name.to_str())});
        let points: Vec<_> = source
            .lines
            .iter()
            .map(|line| json!({"line":line}))
            .collect();
        let result = self.request(
            "setBreakpoints",
            json!({"source":source_json,"breakpoints":points,"sourceModified":false}),
            Kind::Breakpoints {
                path,
                lines: source.lines,
                generation: source.generation,
                startup,
            },
            source.request_id,
            None,
        );
        if result.is_err() && startup {
            self.configuring = self.configuring.saturating_sub(1);
        }
        result
    }
    fn maybe_configuration_done(&mut self) -> Result<(), DebugError> {
        if !self.configuration_started || self.configuration_done_sent {
            return Ok(());
        }
        // A workspace can have more marked sources than the transport queue.
        // Refill a small window only as responses free slots, rather than
        // treating a valid large configuration as writer backpressure.
        while self.configuring < 8 {
            let Some(path) = self.configuration_queue.pop_front() else {
                break;
            };
            if let Some(source) = self.desired.get(&path).cloned() {
                self.set_breakpoints(path, source, true)?;
            }
        }
        if self.configuring != 0 || !self.configuration_queue.is_empty() {
            return Ok(());
        }
        self.configuration_done_sent = true;
        if self.capabilities.supports_configuration_done_request {
            self.request(
                "configurationDone",
                json!({}),
                Kind::ConfigurationDone,
                None,
                None,
            )
        } else {
            self.configured = true;
            self.maybe_running();
            Ok(())
        }
    }
    fn maybe_running(&mut self) {
        if self.configured
            && self.launch_accepted
            && self.state != DebugState::Stopped
            && self.shutting_down.is_none()
        {
            self.state(DebugState::Running);
        }
    }
    fn command(&mut self, envelope: Envelope) {
        let id = Some(envelope.id);
        if let DebugCommand::SetBreakpoints(source) = envelope.command {
            if !self.desired.contains_key(&source.path) && self.desired.len() >= MAX_SOURCES {
                self.fail(id, "setBreakpoints", "Too many breakpoint sources");
                return;
            }
            let old = self.desired.get(&source.path);
            let generation = old.map_or(1, |source| source.generation.saturating_add(1));
            if !self.configuration_started || self.configuration_queue.contains(&source.path) {
                if let Some(previous) = old.and_then(|source| source.request_id) {
                    self.fail(
                        Some(previous),
                        "setBreakpoints",
                        "Superseded by a newer breakpoint set",
                    );
                }
            }
            let desired = DesiredSource {
                lines: source.lines,
                generation,
                request_id: id,
            };
            self.desired.insert(source.path.clone(), desired.clone());
            if self.configuration_started {
                if !self.configuration_done_sent {
                    if !self.configuration_queue.contains(&source.path) {
                        self.configuration_queue.push_back(source.path);
                    }
                    if let Err(error) = self.maybe_configuration_done() {
                        self.fail(id, "setBreakpoints", error.message);
                        let _ = self.shutdown();
                    }
                } else if let Err(error) = self.set_breakpoints(source.path, desired, false) {
                    self.fail(id, "setBreakpoints", error.message);
                }
            }
            return;
        }
        let (command, arguments, kind, suspended, resumes) = match envelope.command {
            DebugCommand::Threads => ("threads", json!({}), Kind::Threads, false, false),
            DebugCommand::StackTrace {
                thread_id,
                start_frame,
                levels,
            } => (
                "stackTrace",
                json!({"threadId":thread_id,"startFrame":start_frame,"levels":levels}),
                Kind::Stack { thread_id },
                true,
                false,
            ),
            DebugCommand::Scopes { frame_id } => {
                if !self.frames.contains(&frame_id) {
                    self.fail(
                        id,
                        "scopes",
                        "Stack frame is no longer valid; reload the stack",
                    );
                    return;
                }
                (
                    "scopes",
                    json!({"frameId":frame_id}),
                    Kind::Scopes { frame_id },
                    true,
                    false,
                )
            }
            DebugCommand::Variables {
                variables_reference,
                start,
                count,
            } => {
                if !self.references.contains(&variables_reference) {
                    self.fail(
                        id,
                        "variables",
                        "Variable reference is no longer valid; reload scopes",
                    );
                    return;
                }
                (
                    "variables",
                    json!({"variablesReference":variables_reference,"start":start,"count":count}),
                    Kind::Variables {
                        reference: variables_reference,
                    },
                    true,
                    false,
                )
            }
            DebugCommand::Continue { thread_id } => (
                "continue",
                json!({"threadId":thread_id}),
                Kind::Resume { thread_id },
                true,
                true,
            ),
            DebugCommand::Next { thread_id } => (
                "next",
                json!({"threadId":thread_id}),
                Kind::Resume { thread_id },
                true,
                true,
            ),
            DebugCommand::StepIn { thread_id } => (
                "stepIn",
                json!({"threadId":thread_id}),
                Kind::Resume { thread_id },
                true,
                true,
            ),
            DebugCommand::StepOut { thread_id } => (
                "stepOut",
                json!({"threadId":thread_id}),
                Kind::Resume { thread_id },
                true,
                true,
            ),
            DebugCommand::Pause { thread_id } => (
                "pause",
                json!({"threadId":thread_id}),
                Kind::Pause,
                false,
                false,
            ),
            DebugCommand::Stop | DebugCommand::SetBreakpoints(_) => return,
        };
        if self.shutting_down.is_some()
            || !matches!(self.state, DebugState::Running | DebugState::Stopped)
        {
            self.fail(id, command, "Debug session is not ready");
            return;
        }
        if suspended && (self.state != DebugState::Stopped || self.resume_pending) {
            self.fail(id, command, "The debuggee is not suspended");
            return;
        }
        if command == "pause" && self.state != DebugState::Running {
            self.fail(id, command, "The debuggee is not running");
            return;
        }
        if let Some(thread) = arguments["threadId"].as_i64() {
            if !self.threads.contains(&thread) {
                self.fail(id, command, "Unknown debug thread; refresh threads");
                return;
            }
        }
        let stop = suspended.then_some(self.stop_id);
        match self.request(command, arguments, kind, id, stop) {
            Ok(()) if resumes => self.resume_pending = true,
            Ok(()) => {}
            Err(error) => self.fail(id, command, error.message),
        }
    }
    fn message(&mut self, message: Value) -> Result<(), DebugError> {
        match message["type"].as_str() {
            Some("response") => self.response(message),
            Some("event") => self.event(message),
            Some("request") => {
                let request_seq = message["seq"]
                    .as_i64()
                    .ok_or_else(|| error("Reverse DAP request lacks sequence"))?;
                let command = message["command"].as_str().unwrap_or("unknown");
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| error("DAP sequence exhausted"))?;
                self.write(json!({"seq":self.sequence,"type":"response","request_seq":request_seq,
                    "command":command,"success":false,"message":format!("OpenIDE does not support reverse request {command}; use an internal debug console") }))?;
                self.fail(
                    None,
                    command,
                    format!(
                        "Unsupported adapter request: {command}. No external process was started."
                    ),
                );
                Ok(())
            }
            _ => Err(error("Unsupported DAP message type")),
        }
    }
    fn response(&mut self, message: Value) -> Result<(), DebugError> {
        let sequence = message["request_seq"]
            .as_i64()
            .ok_or_else(|| error("DAP response lacks request_seq"))?;
        // Adapters such as lldb-dap legitimately send seq=0. Only request_seq
        // correlates a response; an already expired response cannot regain scope.
        let Some(pending) = self.pending.remove(&sequence) else {
            return Ok(());
        };
        if message["command"].as_str() != Some(pending.command) {
            self.fail(
                pending.id,
                pending.command,
                "Mismatched adapter response command",
            );
            return Err(error("Mismatched DAP response"));
        }
        if self.shutting_down.is_some() && !matches!(pending.kind, Kind::Disconnect) {
            return Ok(());
        }
        if let Kind::Breakpoints {
            path,
            generation,
            startup,
            ..
        } = &pending.kind
        {
            if self
                .desired
                .get(path)
                .is_none_or(|desired| desired.generation != *generation)
            {
                if *startup {
                    self.configuring = self.configuring.saturating_sub(1);
                }
                self.fail(
                    pending.id,
                    pending.command,
                    "Discarded superseded breakpoint response",
                );
                self.maybe_configuration_done()?;
                return Ok(());
            }
        }
        if message["success"].as_bool() != Some(true) {
            let message = message["message"]
                .as_str()
                .unwrap_or("Debug adapter rejected the request")
                .to_owned();
            self.fail(pending.id, pending.command, message.clone());
            if matches!(pending.kind, Kind::Resume { .. }) {
                self.resume_pending = false;
            }
            if matches!(
                pending.kind,
                Kind::Initialize
                    | Kind::Launch
                    | Kind::Configuration
                    | Kind::ConfigurationDone
                    | Kind::Breakpoints { startup: true, .. }
            ) {
                return Err(error(format!("{}: {message}", pending.command)));
            }
            if matches!(pending.kind, Kind::Disconnect) {
                self.writer.take();
                self.disconnect_acknowledged = true;
            }
            return Ok(());
        }
        if pending
            .stop_id
            .is_some_and(|stop| stop != self.stop_id || self.state != DebugState::Stopped)
        {
            if !matches!(pending.kind, Kind::Resume { .. }) {
                self.fail(
                    pending.id,
                    pending.command,
                    "Discarded response from an earlier suspended state",
                );
            }
            return Ok(());
        }
        if self.resume_pending
            && matches!(
                pending.kind,
                Kind::Stack { .. } | Kind::Scopes { .. } | Kind::Variables { .. }
            )
        {
            self.fail(
                pending.id,
                pending.command,
                "Discarded inspection while execution is resuming",
            );
            return Ok(());
        }
        let body = message.get("body").cloned().unwrap_or_else(|| json!({}));
        match pending.kind {
            Kind::Initialize => {
                self.capabilities = serde_json::from_value(body)
                    .map_err(|e| error(format!("Invalid DAP capabilities: {e}")))?;
                self.events
                    .emit(DebugEvent::Capabilities(self.capabilities.clone()));
                self.startup_deadline = Instant::now() + self.config.request_timeout;
                self.launch_sent = true;
                self.request(
                    "launch",
                    self.config.launch.clone(),
                    Kind::Launch,
                    None,
                    None,
                )?;
                self.maybe_configure()?;
            }
            Kind::Launch => {
                self.launch_accepted = true;
                self.maybe_running();
                self.refresh_threads()?;
            }
            Kind::Configuration => {
                self.configuring = self.configuring.saturating_sub(1);
                self.maybe_configuration_done()?;
            }
            Kind::ConfigurationDone => {
                self.configured = true;
                self.maybe_running();
            }
            Kind::Disconnect => {
                self.disconnect_acknowledged = true;
                self.writer.take();
            }
            Kind::Breakpoints {
                path,
                lines,
                generation,
                startup,
            } => {
                if startup {
                    self.configuring = self.configuring.saturating_sub(1);
                }
                if self
                    .desired
                    .get(&path)
                    .is_some_and(|desired| desired.generation == generation)
                {
                    let actual = body["breakpoints"]
                        .as_array()
                        .ok_or_else(|| error("setBreakpoints response lacks breakpoints"))?;
                    if actual.len() != lines.len() {
                        return Err(error("Adapter returned a different number of breakpoints"));
                    }
                    let mut breakpoints: Vec<Breakpoint> =
                        serde_json::from_value(body["breakpoints"].clone())
                            .map_err(|e| error(format!("Invalid adapter breakpoints: {e}")))?;
                    self.breakpoint_ids.retain(|_, (source, _)| source != &path);
                    for (breakpoint, requested) in breakpoints.iter_mut().zip(lines) {
                        breakpoint.requested_line = Some(requested);
                        if let Some(id) = breakpoint.id {
                            self.breakpoint_ids.insert(id, (path.clone(), generation));
                        }
                    }
                    self.events.emit(DebugEvent::Breakpoints {
                        request_id: pending.id,
                        source: path,
                        generation,
                        breakpoints,
                    });
                } else {
                    self.fail(
                        pending.id,
                        "setBreakpoints",
                        "Discarded superseded breakpoint response",
                    );
                }
                self.maybe_configuration_done()?;
            }
            Kind::Threads => {
                let mut threads: Vec<Thread> = parse_list(&body, "threads")?;
                threads.truncate(512);
                self.threads = threads.iter().map(|thread| thread.id).collect();
                self.thread_details = threads.clone();
                self.events.emit(DebugEvent::Threads {
                    request_id: pending.id,
                    threads,
                });
            }
            Kind::Stack { thread_id } => {
                let mut frames: Vec<StackFrame> = parse_list(&body, "stackFrames")?;
                frames.truncate(500);
                for frame in &frames {
                    if self.frames.len() < 16384 {
                        self.frames.insert(frame.id);
                    }
                }
                self.events.emit(DebugEvent::StackTrace {
                    request_id: pending.id.unwrap(),
                    stop_id: self.stop_id,
                    thread_id,
                    frames,
                    total_frames: body["totalFrames"].as_u64().and_then(|n| n.try_into().ok()),
                });
            }
            Kind::Scopes { frame_id } => {
                let mut scopes: Vec<Scope> = parse_list(&body, "scopes")?;
                scopes.truncate(512);
                for scope in &scopes {
                    if scope.variables_reference > 0 && self.references.len() < 16384 {
                        self.references.insert(scope.variables_reference);
                    }
                }
                self.events.emit(DebugEvent::Scopes {
                    request_id: pending.id.unwrap(),
                    stop_id: self.stop_id,
                    frame_id,
                    scopes,
                });
            }
            Kind::Variables { reference } => {
                let mut variables: Vec<Variable> = parse_list(&body, "variables")?;
                variables.truncate(1000);
                for variable in &variables {
                    if variable.variables_reference > 0 && self.references.len() < 16384 {
                        self.references.insert(variable.variables_reference);
                    }
                }
                self.events.emit(DebugEvent::Variables {
                    request_id: pending.id.unwrap(),
                    stop_id: self.stop_id,
                    variables_reference: reference,
                    variables,
                });
            }
            Kind::Resume { thread_id } => {
                self.resume_pending = false;
                self.frames.clear();
                self.references.clear();
                self.state(DebugState::Running);
                self.events.emit(DebugEvent::Continued {
                    thread_id: Some(thread_id),
                    all_threads_continued: body["allThreadsContinued"]
                        .as_bool()
                        .unwrap_or(pending.command == "continue"),
                });
            }
            Kind::Pause => {}
        }
        Ok(())
    }
    fn event(&mut self, message: Value) -> Result<(), DebugError> {
        let name = message["event"]
            .as_str()
            .ok_or_else(|| error("DAP event lacks name"))?;
        let body = message.get("body").cloned().unwrap_or_else(|| json!({}));
        match name {
            "initialized" => {
                self.initialized = true;
                self.maybe_configure()?;
            }
            "stopped" if self.shutting_down.is_none() => {
                self.stop_id = self
                    .stop_id
                    .checked_add(1)
                    .ok_or_else(|| error("Stop sequence exhausted"))?;
                self.frames.clear();
                self.references.clear();
                self.resume_pending = false;
                self.state(DebugState::Stopped);
                let thread_id = body["threadId"].as_i64();
                if let Some(thread_id) = thread_id {
                    self.threads.insert(thread_id);
                }
                self.events.emit(DebugEvent::Stopped {
                    stop_id: self.stop_id,
                    reason: body["reason"].as_str().unwrap_or("unknown").into(),
                    description: body["description"].as_str().map(str::to_owned),
                    thread_id,
                    all_threads_stopped: body["allThreadsStopped"].as_bool().unwrap_or(false),
                });
                self.refresh_threads()?;
            }
            "continued" if self.shutting_down.is_none() => {
                self.frames.clear();
                self.references.clear();
                self.resume_pending = false;
                self.state(DebugState::Running);
                self.events.emit(DebugEvent::Continued {
                    thread_id: body["threadId"].as_i64(),
                    all_threads_continued: body["allThreadsContinued"].as_bool().unwrap_or(true),
                });
            }
            "thread" if self.shutting_down.is_none() && self.launch_sent => {
                if body["reason"].as_str() == Some("exited") {
                    // A last-thread event precedes normal process exit in
                    // debugpy. Querying the already shutting-down debuggee
                    // here reports a misleading server-disconnected failure.
                    if let Some(id) = body["threadId"].as_i64() {
                        self.threads.remove(&id);
                        self.thread_details.retain(|thread| thread.id != id);
                        self.events.emit(DebugEvent::Threads {
                            request_id: None,
                            threads: self.thread_details.clone(),
                        });
                        if self.threads.is_empty() {
                            // Retire automatic refreshes already in flight.
                            // Explicit requests still receive their response.
                            self.pending.retain(|_, pending| {
                                pending.id.is_some() || !matches!(pending.kind, Kind::Threads)
                            });
                        }
                    }
                } else {
                    if body["reason"].as_str() == Some("started") {
                        if let Some(id) = body["threadId"].as_i64() {
                            // Preserve a newly reported thread even while its
                            // name/list refresh is still in flight.
                            self.threads.insert(id);
                        }
                    }
                    self.refresh_threads()?;
                }
            }
            "breakpoint" => {
                let breakpoint: Breakpoint = serde_json::from_value(body["breakpoint"].clone())
                    .map_err(|e| error(format!("Invalid breakpoint event: {e}")))?;
                // Unknown/retired adapter IDs cannot overwrite a newer row set.
                if breakpoint
                    .id
                    .and_then(|id| self.breakpoint_ids.get(&id))
                    .is_some_and(|(path, generation)| {
                        self.desired
                            .get(path)
                            .is_some_and(|desired| desired.generation == *generation)
                    })
                {
                    self.events.emit(DebugEvent::BreakpointChanged {
                        reason: body["reason"].as_str().unwrap_or("changed").into(),
                        breakpoint,
                    });
                }
            }
            "capabilities" => {
                let mut merged =
                    serde_json::to_value(&self.capabilities).map_err(|e| error(e.to_string()))?;
                if let (Some(previous), Some(update)) =
                    (merged.as_object_mut(), body["capabilities"].as_object())
                {
                    previous.extend(update.clone());
                    self.capabilities =
                        serde_json::from_value(merged).map_err(|e| error(e.to_string()))?;
                    self.events
                        .emit(DebugEvent::Capabilities(self.capabilities.clone()));
                }
            }
            "invalidated" => {
                self.frames.clear();
                self.references.clear();
                self.events.emit(DebugEvent::Invalidated {
                    areas: body["areas"]
                        .as_array()
                        .map(|areas| {
                            areas
                                .iter()
                                .filter_map(Value::as_str)
                                .take(32)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    thread_id: body["threadId"].as_i64(),
                    stack_frame_id: body["stackFrameId"].as_i64(),
                });
                // These requests belong to the same stop but an older adapter
                // cache. Removing them prevents late replies restoring IDs
                // which the invalidated event has explicitly retired.
                let invalidated: Vec<_> = self
                    .pending
                    .iter()
                    .filter_map(|(seq, pending)| {
                        matches!(
                            pending.kind,
                            Kind::Stack { .. } | Kind::Scopes { .. } | Kind::Variables { .. }
                        )
                        .then_some(*seq)
                    })
                    .collect();
                for seq in invalidated {
                    let pending = self.pending.remove(&seq).unwrap();
                    self.fail(
                        pending.id,
                        pending.command,
                        "Adapter invalidated inspection data",
                    );
                }
            }
            "output" => {
                let output = body["output"].as_str().unwrap_or("");
                let mut start = 0;
                while start < output.len() {
                    let mut end = (start + 16 * 1024).min(output.len());
                    while !output.is_char_boundary(end) {
                        end -= 1;
                    }
                    self.events.emit(DebugEvent::Output {
                        category: body["category"].as_str().unwrap_or("console").into(),
                        output: output[start..end].to_owned(),
                    });
                    start = end;
                }
            }
            "exited" => self.events.emit(DebugEvent::Exited {
                exit_code: body["exitCode"].as_i64(),
            }),
            "terminated" => {
                self.events.emit(DebugEvent::Terminated);
                if self.shutting_down.is_none() {
                    self.shutdown()?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn parse_list<T: for<'de> serde::Deserialize<'de>>(
    body: &Value,
    field: &str,
) -> Result<Vec<T>, DebugError> {
    serde_json::from_value(body[field].clone())
        .map_err(|e| error(format!("Invalid DAP {field}: {e}")))
}
