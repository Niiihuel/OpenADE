//! Persistent ordinary-pipe commands in the same MXC ProcessContainer used by
//! `run_command`. The SDK handle owns both the process job and policy teardown.

use super::{
    read_pipe_blocking, OutputBuffer, PersistentCommandExit, PersistentCommandManager,
    PersistentCommandStarted, PersistentCommandStream, Session, SessionTree, IO_WAIT,
};
use crate::command::sandbox::spawn_workspace_write_offline_persistent;
use crate::AgentError;
use mxc_sdk::{Sandbox, WaitOutcome};
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;

const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(super) type InputSender = mpsc::Sender<InputRequest>;

pub(super) struct InputRequest {
    bytes: Vec<u8>,
    close_after: bool,
    reply: oneshot::Sender<io::Result<()>>,
}

/// All access to an MXC handle is serialized. `kill` is called synchronously
/// by manager revocation and does not release the security policy; `wait` or
/// the SDK's Drop tears down policy only after killing and reaping the tree.
pub(super) struct SandboxRuntime {
    sandbox: Mutex<Option<Sandbox>>,
    kill_error: Mutex<Option<String>>,
}

impl SandboxRuntime {
    fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox: Mutex::new(Some(sandbox)),
            kill_error: Mutex::new(None),
        }
    }

    pub(super) fn kill(&self) {
        let result = self
            .sandbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
            .map(Sandbox::kill);
        if let Some(Err(error)) = result {
            *self
                .kill_error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.to_string());
        }
    }

    fn poll(&self) -> io::Result<Option<i32>> {
        self.sandbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
            .ok_or_else(|| io::Error::other("sandbox handle was already released"))?
            .try_wait()
    }

    fn take(&self) -> Option<Sandbox> {
        self.sandbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    fn kill_error(&self) -> Option<String> {
        self.kill_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

pub(super) fn start(
    manager: &PersistentCommandManager,
    program: &str,
    args: &[String],
    cancellation: &CancellationToken,
    runtime: &tokio::runtime::Handle,
    sessions: &mut HashMap<String, Arc<Session>>,
    registration: super::NativeOutputRegistration,
) -> Result<PersistentCommandStarted, AgentError> {
    let super::NativeOutputRegistration { id, capture } = registration;
    // The Windows sandbox runner takes a mandatory deadline, so an uncapped
    // profile keeps the validator's one-hour ceiling here.
    let lifetime = manager
        .limits
        .max_lifetime
        .unwrap_or(Duration::from_secs(60 * 60));
    let timeout_ms = u64::try_from(lifetime.as_millis())
        .map_err(|_| AgentError::Tool("sandbox lifetime exceeds supported range".into()))?;
    let mut sandbox = spawn_workspace_write_offline_persistent(
        program,
        args,
        timeout_ms,
        &manager.workspace,
        cancellation,
    )?;
    if cancellation.is_cancelled() || manager.revoked.is_cancelled() {
        sandbox.kill().map_err(|error| {
            AgentError::EffectUncertain(format!(
                "cancelled sandbox start and tree kill failed: {error}"
            ))
        })?;
        sandbox.wait().map_err(|error| {
            AgentError::EffectUncertain(format!(
                "cancelled sandbox start and cleanup failed: {error}"
            ))
        })?;
        return Err(AgentError::Cancelled);
    }
    let stdin = sandbox.take_stdin().ok_or_else(|| {
        AgentError::EffectUncertain("sandbox stdin pipe is unavailable after spawn".into())
    })?;
    let stdout = sandbox.take_stdout().ok_or_else(|| {
        AgentError::EffectUncertain("sandbox stdout pipe is unavailable after spawn".into())
    })?;
    let stderr = sandbox.take_stderr().ok_or_else(|| {
        AgentError::EffectUncertain("sandbox stderr pipe is unavailable after spawn".into())
    })?;
    let output =
        Arc::new(OutputBuffer::new(manager.limits.max_buffered_output_bytes).with_capture(capture));
    // Attach containment before either reader can observe an output failure.
    // Every thread-creation failure also closes this tree before returning.
    let process = Arc::new(SandboxRuntime::new(sandbox));
    let tree = Arc::new(Mutex::new(Some(SessionTree::Sandbox(Arc::clone(&process)))));
    output.attach_tree(Arc::clone(&tree));
    let stdout_output = Arc::clone(&output);
    let stdout_reader = thread::Builder::new()
        .name("openide-mxc-stdout".into())
        .spawn(move || read_pipe_blocking(stdout, PersistentCommandStream::Stdout, stdout_output))
        .map_err(|error| {
            output.fail_capture(format!("sandbox stdout reader could not start: {error}"));
            AgentError::EffectUncertain(format!(
                "sandbox started but stdout reader could not start: {error}"
            ))
        })?;
    let stderr_output = Arc::clone(&output);
    let stderr_reader = thread::Builder::new()
        .name("openide-mxc-stderr".into())
        .spawn(move || read_pipe_blocking(stderr, PersistentCommandStream::Stderr, stderr_output))
        .map_err(|error| {
            output.fail_capture(format!("sandbox stderr reader could not start: {error}"));
            AgentError::EffectUncertain(format!(
                "sandbox started but stderr reader could not start: {error}"
            ))
        })?;
    let (input_tx, input_rx) = mpsc::channel();
    let input_worker = thread::Builder::new()
        .name("openide-mxc-stdin".into())
        .spawn(move || write_input(stdin, input_rx))
        .map_err(|error| {
            output.fail_capture(format!("sandbox stdin writer could not start: {error}"));
            AgentError::EffectUncertain(format!(
                "sandbox started but stdin writer could not start: {error}"
            ))
        })?;

    let input = Arc::new(AsyncMutex::new(Some(input_tx)));
    let stop = output.stop.clone();
    let session = Arc::new(Session {
        stdin: Arc::new(AsyncMutex::new(None)),
        output: Arc::clone(&output),
        stop: stop.clone(),
        tree: Arc::clone(&tree),
        poll_lock: AsyncMutex::new(()),
        controls: Mutex::new(super::CommandControlState::new(program, args, None)),
        pty: false,
        pty_console: None,
        resize_lock: AsyncMutex::new(()),
        sandbox_stdin: Some(Arc::clone(&input)),
    });
    runtime.spawn_blocking(move || {
        SandboxMonitor {
            process,
            tree,
            input,
            output,
            stop,
            lifetime,
            stdout_reader,
            stderr_reader,
            input_worker,
        }
        .run();
    });
    sessions.insert(id.clone(), session);
    Ok(PersistentCommandStarted { id })
}

fn write_input(mut stdin: Box<dyn Write + Send>, receiver: mpsc::Receiver<InputRequest>) {
    for request in receiver {
        let result = stdin.write_all(&request.bytes).and_then(|_| stdin.flush());
        let failed = result.is_err();
        let _ = request.reply.send(result);
        if failed || request.close_after {
            break;
        }
    }
}

struct InFlightInput<'a> {
    session: &'a Session,
    complete: bool,
}

impl Drop for InFlightInput<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.session.mark_stdin_uncertain();
            self.session.stop_tree();
        }
    }
}

pub(super) async fn write_stdin(
    session: &Arc<Session>,
    bytes: &[u8],
    close_after: bool,
    cancellation: &CancellationToken,
) -> Result<(), AgentError> {
    let input = session
        .sandbox_stdin
        .as_ref()
        .expect("sandbox session has input channel");
    let mut sender = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
        guard = input.lock() => guard,
    };
    if session.stop.is_cancelled() || session.output.has_exited()? {
        return Err(AgentError::Tool(
            "persistent command is no longer running".into(),
        ));
    }
    let sender_ref = sender
        .as_ref()
        .ok_or_else(|| AgentError::Tool("persistent command stdin is closed".into()))?;
    let (reply, result) = oneshot::channel();
    sender_ref
        .send(InputRequest {
            bytes: bytes.to_vec(),
            close_after,
            reply,
        })
        .map_err(|_| AgentError::Tool("persistent command stdin is closed".into()))?;
    let mut in_flight = InFlightInput {
        session,
        complete: false,
    };
    let outcome = tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err("cancelled"),
        _ = session.stop.cancelled() => Err("terminated"),
        outcome = tokio::time::timeout(IO_WAIT, result) => match outcome {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(_))) => Err("failed"),
            Ok(Err(_)) => Err("writer stopped"),
            Err(_) => Err("timed out"),
        },
    };
    if let Err(reason) = outcome {
        sender.take();
        return Err(AgentError::EffectUncertain(format!(
            "persistent sandbox stdin write {reason}; partial input may have been delivered"
        )));
    }
    if close_after {
        sender.take();
    }
    in_flight.complete = true;
    Ok(())
}

enum ObservedExit {
    Exited,
    Terminated,
    TimedOut,
    WaitFailed(String),
}

struct SandboxMonitor {
    process: Arc<SandboxRuntime>,
    tree: Arc<Mutex<Option<SessionTree>>>,
    input: Arc<AsyncMutex<Option<InputSender>>>,
    output: Arc<OutputBuffer>,
    stop: CancellationToken,
    lifetime: Duration,
    stdout_reader: thread::JoinHandle<io::Result<()>>,
    stderr_reader: thread::JoinHandle<io::Result<()>>,
    input_worker: thread::JoinHandle<()>,
}

impl SandboxMonitor {
    fn run(self) {
        let SandboxMonitor {
            process,
            tree,
            input,
            output,
            stop,
            lifetime,
            stdout_reader,
            stderr_reader,
            input_worker,
        } = self;
        let deadline = Instant::now() + lifetime;
        let observed = loop {
            if stop.is_cancelled() {
                break ObservedExit::Terminated;
            }
            match process.poll() {
                Ok(Some(_)) => break ObservedExit::Exited,
                Ok(None) => {}
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    break ObservedExit::TimedOut;
                }
                Err(error) => break ObservedExit::WaitFailed(error.to_string()),
            }
            if Instant::now() >= deadline {
                break ObservedExit::TimedOut;
            }
            thread::sleep(POLL_INTERVAL);
        };
        // Kill the entire job while the SDK policy is still active, then let
        // wait/drop finish policy teardown. A manager stop races safely here:
        // whoever takes the tree first performs the kill.
        if let Some(tree) = tree
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            tree.close();
        }
        input.blocking_lock().take();
        let (wait, warnings) = match process.take() {
            Some(mut sandbox) => {
                let waited = sandbox.wait();
                let warnings = sandbox.warnings();
                (waited, warnings)
            }
            None => (
                Err(io::Error::other("sandbox handle was released before wait")),
                Vec::new(),
            ),
        };
        let mut errors = Vec::new();
        if let Some(error) = process.kill_error() {
            errors.push(format!("sandbox tree kill failed: {error}"));
        }
        if !warnings.is_empty() {
            errors.push(format!("sandbox policy warning: {}", warnings.join("; ")));
        }
        for (name, result) in [
            ("stdout", stdout_reader.join()),
            ("stderr", stderr_reader.join()),
        ] {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(format!("sandbox {name} read failed: {error}")),
                Err(_) => errors.push(format!("sandbox {name} reader panicked")),
            }
        }
        if input_worker.join().is_err() {
            errors.push("sandbox stdin writer panicked".into());
        }
        let exit = if let Some(error) = process.kill_error() {
            PersistentCommandExit::WaitFailed(format!("sandbox tree kill failed: {error}"))
        } else {
            match (observed, wait) {
                (ObservedExit::Exited, Ok(WaitOutcome::Exited(code))) => {
                    PersistentCommandExit::Exited { code: Some(code) }
                }
                (ObservedExit::TimedOut, _) | (_, Ok(WaitOutcome::TimedOut)) => {
                    PersistentCommandExit::TimedOut
                }
                (ObservedExit::Terminated, Ok(_)) => PersistentCommandExit::Terminated,
                (ObservedExit::WaitFailed(error), _) => PersistentCommandExit::WaitFailed(error),
                (_, Err(error)) => PersistentCommandExit::WaitFailed(error.to_string()),
            }
        };
        let reader_error = (!errors.is_empty()).then(|| {
            let mut joined = errors.join("; ");
            if joined.len() > 4096 {
                let boundary = (0..=4096)
                    .rev()
                    .find(|&index| joined.is_char_boundary(index))
                    .unwrap_or(0);
                joined.truncate(boundary);
            }
            joined
        });
        output.finish(exit, reader_error);
    }
}
