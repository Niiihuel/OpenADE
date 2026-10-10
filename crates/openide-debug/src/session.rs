use crate::*;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, SyncSender},
    Arc, Mutex,
};

const MAX_EVENTS: usize = 512;
const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_SOURCE_BREAKPOINTS: usize = 4096;
pub(crate) const MAX_SOURCES: usize = 128;

pub(crate) struct Envelope {
    pub id: RequestId,
    pub command: DebugCommand,
}
#[derive(Default)]
struct Events {
    entries: VecDeque<(DebugEvent, usize)>,
    bytes: usize,
}
#[derive(Default)]
pub(crate) struct EventBuffer {
    events: Mutex<Events>,
    pub overflow: AtomicBool,
    pub closed: AtomicBool,
}
impl EventBuffer {
    pub fn emit(&self, event: DebugEvent) {
        if self.overflow.load(Ordering::Acquire) {
            return;
        }
        let bytes = serde_json::to_vec(&event).map_or(MAX_EVENT_BYTES + 1, |json| json.len());
        let Ok(mut state) = self.events.lock() else {
            self.overflow.store(true, Ordering::Release);
            return;
        };
        if state.entries.len() >= MAX_EVENTS || state.bytes.saturating_add(bytes) > MAX_EVENT_BYTES
        {
            self.overflow.store(true, Ordering::Release);
            return;
        }
        state.bytes += bytes;
        state.entries.push_back((event, bytes));
    }
    pub fn finish(&self, reason: String) {
        if let Ok(mut state) = self.events.lock() {
            if self.overflow.load(Ordering::Acquire) {
                state.entries.clear();
                state.bytes = 0;
            }
            for event in [
                DebugEvent::State(DebugState::Disconnected),
                DebugEvent::Disconnected { reason },
            ] {
                let bytes = serde_json::to_vec(&event).map_or(0, |json| json.len());
                state.bytes += bytes;
                state.entries.push_back((event, bytes));
            }
        }
        self.closed.store(true, Ordering::Release);
    }
}

pub struct DebugSession {
    commands: SyncSender<Envelope>,
    events: Arc<EventBuffer>,
    cancel: Arc<AtomicBool>,
    next_id: AtomicU64,
}
impl std::fmt::Debug for DebugSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DebugSession")
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}
impl DebugSession {
    pub fn start(config: DebugConfig) -> Result<Self, DebugError> {
        validate_config(&config)?;
        let (commands, receiver) = mpsc::sync_channel(128);
        let events = Arc::new(EventBuffer::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_events = events.clone();
        let worker_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("debug-adapter".into())
            .spawn(move || {
                crate::worker::run(config, receiver, worker_events, worker_cancel);
            })
            .map_err(|e| error(format!("Cannot start debug worker: {e}")))?;
        Ok(Self {
            commands,
            events,
            cancel,
            next_id: AtomicU64::new(1),
        })
    }
    /// Queue an explicit user request. Full queues return an error, never block.
    /// Stop uses a separate cancellation flag and cannot be starved by requests.
    pub fn send(&self, command: DebugCommand) -> Result<RequestId, DebugError> {
        if self.is_closed() {
            return Err(error("Debug session is disconnected"));
        }
        validate_command(&command)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if matches!(command, DebugCommand::Stop) {
            self.cancel.store(true, Ordering::Release);
            return Ok(id);
        }
        self.commands
            .try_send(Envelope { id, command })
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => error("Debug request queue is full"),
                mpsc::TrySendError::Disconnected(_) => error("Debug session is disconnected"),
            })?;
        Ok(id)
    }
    /// Nonblocking, bounded event delivery. Only the background worker touches pipes.
    pub fn drain(&self) -> Vec<DebugEvent> {
        let Ok(mut state) = self.events.events.try_lock() else {
            return Vec::new();
        };
        let result = state.entries.drain(..).map(|(event, _)| event).collect();
        state.bytes = 0;
        result
    }
    pub fn is_closed(&self) -> bool {
        self.events.closed.load(Ordering::Acquire)
    }
}
impl Drop for DebugSession {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn validate_config(config: &DebugConfig) -> Result<(), DebugError> {
    if config.executable.as_os_str().is_empty() || config.workspace.as_os_str().is_empty() {
        return Err(error("Adapter executable and workspace are required"));
    }
    if !config.launch.is_object() {
        return Err(error("DAP launch arguments must be an object"));
    }
    if config.args.len() > 128
        || config.args.iter().map(String::len).sum::<usize>() > 65536
        || config.env.len() > 256
        || config
            .env
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
            > 65536
        || config.adapter_id.len() > 256
        || config.breakpoints.len() > MAX_SOURCES
    {
        return Err(error("Debug configuration exceeds limits"));
    }
    if config.request_timeout < Duration::from_millis(100)
        || config.request_timeout > Duration::from_secs(120)
        || config.shutdown_timeout < Duration::from_millis(100)
        || config.shutdown_timeout > Duration::from_secs(10)
    {
        return Err(error("Debug timeouts are outside supported bounds"));
    }
    protocol::frame(&config.launch)?;
    for source in &config.breakpoints {
        validate_breakpoints(source)?;
    }
    Ok(())
}
pub(crate) fn validate_breakpoints(source: &SourceBreakpoints) -> Result<(), DebugError> {
    if source.path.as_os_str().is_empty()
        || source.path.as_os_str().len() > 16384
        || source.lines.len() > MAX_SOURCE_BREAKPOINTS
        || source
            .lines
            .iter()
            .any(|line| *line == 0 || *line > i32::MAX as u32)
    {
        return Err(error(
            "Invalid source breakpoint coordinates or limit exceeded",
        ));
    }
    Ok(())
}
fn validate_command(command: &DebugCommand) -> Result<(), DebugError> {
    let valid = match command {
        DebugCommand::SetBreakpoints(source) => return validate_breakpoints(source),
        DebugCommand::Threads | DebugCommand::Stop => true,
        DebugCommand::StackTrace {
            thread_id, levels, ..
        } => *thread_id > 0 && *levels > 0 && *levels <= 500,
        DebugCommand::Scopes { frame_id } => *frame_id >= 0,
        DebugCommand::Variables {
            variables_reference,
            count,
            ..
        } => *variables_reference > 0 && *count > 0 && *count <= 1000,
        DebugCommand::Continue { thread_id }
        | DebugCommand::Pause { thread_id }
        | DebugCommand::Next { thread_id }
        | DebugCommand::StepIn { thread_id }
        | DebugCommand::StepOut { thread_id } => *thread_id > 0,
    };
    if valid {
        Ok(())
    } else {
        Err(error("Invalid debug request coordinates or page size"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_configuration_and_coordinates_are_rejected_before_process_creation() {
        let mut config = DebugConfig::new("must-not-be-launched", ".", json!({}));
        config.breakpoints.push(SourceBreakpoints {
            path: "main.rs".into(),
            lines: vec![0],
        });
        assert!(DebugSession::start(config.clone()).is_err());
        config.breakpoints[0].lines = vec![1; MAX_SOURCE_BREAKPOINTS + 1];
        assert!(DebugSession::start(config.clone()).is_err());
        config.breakpoints.clear();
        config.launch = json!({"oversized":"x".repeat(1024 * 1024)});
        assert!(DebugSession::start(config.clone()).is_err());
        config.launch = json!({});
        config.request_timeout = Duration::ZERO;
        assert!(DebugSession::start(config).is_err());
        assert!(validate_command(&DebugCommand::Variables {
            variables_reference: 0,
            start: 0,
            count: 20
        })
        .is_err());
        assert!(validate_command(&DebugCommand::StackTrace {
            thread_id: 1,
            start_frame: 0,
            levels: 501
        })
        .is_err());
    }

    #[test]
    fn event_budget_fails_explicitly_instead_of_silently_dropping_session_state() {
        let events = EventBuffer::default();
        for _ in 0..=MAX_EVENTS {
            events.emit(DebugEvent::State(DebugState::Starting));
        }
        assert!(events.overflow.load(Ordering::Acquire));
        events.finish("Debug event budget exceeded; session stopped".into());
        let state = events.events.lock().unwrap();
        assert_eq!(state.entries.len(), 2);
        assert!(
            matches!(&state.entries[1].0, DebugEvent::Disconnected {reason} if reason.contains("budget"))
        );
    }
}
