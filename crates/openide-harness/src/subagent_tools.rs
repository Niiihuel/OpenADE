//! Build-only model tools for starting and observing durable native children.
//!
//! A child worker receives a host-selected base executor. With explicit
//! recursion enabled, it creates a fresh child-scoped wrapper under its own
//! native lease, never reusing an ancestor's control authority.
//! Spawn IDs derive from committed tool-call IDs, and wait/cancel only inspect
//! or update the existing child. No operation here replays an uncertain effect.

use crate::subagents::QueuedInputState;
use crate::{
    AgentError, SubagentCoordinator, SubagentError, SubagentRecord, SubagentStatus, SubagentWorker,
    SubagentWorkerError, ToolContext, ToolDefinition, ToolExecutor, ToolOutput,
};
use async_trait::async_trait;
use openide_core::{AgentEventKind, NativeSessionLease, SessionStore, ToolCall, TurnMode};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const MAX_SPAWNED_TASKS: usize = 32;
const MAX_CALL_ID_BYTES: usize = 256;
const MAX_WAIT_MS: u64 = 120_000;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubagentToolLimits {
    /// Maximum total distinct child sessions for this parent, including
    /// completed children. Retrying an existing call ID does not count again.
    pub max_tasks: usize,
}

impl Default for SubagentToolLimits {
    fn default() -> Self {
        Self { max_tasks: 8 }
    }
}

impl SubagentToolLimits {
    pub(crate) fn valid(self) -> bool {
        self.max_tasks > 0 && self.max_tasks <= MAX_SPAWNED_TASKS
    }
}

struct ControlState {
    parent_lease: Option<Arc<NativeSessionLease>>,
    scheduled: BTreeMap<String, CancellationToken>,
}

/// Coordination wrapper for one native Build turn. Construct its worker with
/// a separate child base executor. A recursively enabled child creates its
/// own wrapper under its own lease, so it cannot use an ancestor's authority.
/// Call `shutdown` before releasing the parent turn.
pub struct SubagentToolExecutor {
    base: Arc<dyn ToolExecutor>,
    worker: SubagentWorker,
    coordinator: Arc<SubagentCoordinator>,
    sessions: Arc<SessionStore>,
    control: Arc<Mutex<ControlState>>,
    limits: SubagentToolLimits,
}

impl SubagentToolExecutor {
    pub fn new(
        base: Arc<dyn ToolExecutor>,
        worker: SubagentWorker,
        coordinator: Arc<SubagentCoordinator>,
        sessions: Arc<SessionStore>,
        parent_lease: Arc<NativeSessionLease>,
        limits: SubagentToolLimits,
    ) -> Result<Self, AgentError> {
        if !limits.valid() {
            return Err(AgentError::InvalidRequest(
                "invalid subagent tool limits".to_owned(),
            ));
        }
        sessions
            .native_session_for_lease(&parent_lease)
            .map_err(|error| AgentError::InvalidRequest(error.to_string()))?;
        coordinator
            .issue_control(&parent_lease)
            .map_err(|error| AgentError::InvalidRequest(error.to_string()))?;
        let base_definitions = base.definitions();
        for name in ["spawn_agent", "send_input", "wait_agent", "cancel_agent"] {
            if base_definitions
                .iter()
                .any(|definition| definition.name == name)
            {
                return Err(AgentError::InvalidRequest(format!(
                    "base executor already defines {name}"
                )));
            }
        }
        Ok(Self {
            base,
            worker,
            coordinator,
            sessions,
            control: Arc::new(Mutex::new(ControlState {
                parent_lease: Some(parent_lease),
                scheduled: BTreeMap::new(),
            })),
            limits,
        })
    }

    /// Close this turn's tool surface and release its parent-lease reference.
    /// Child workers may continue under opaque control without retaining the
    /// parent's lease. A new parent turn can wait for or cancel them.
    pub async fn shutdown(&self) -> Result<(), AgentError> {
        self.control
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .parent_lease
            .take();
        Ok(())
    }

    fn lease(&self) -> Result<Arc<NativeSessionLease>, AgentError> {
        self.control
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .parent_lease
            .clone()
            .ok_or_else(|| AgentError::Runtime("subagent coordination is closed".to_owned()))
    }

    /// Read-only lookup for a journaled `spawn_agent` call whose parent tool
    /// result was lost. It does not dispatch or resume the child.
    pub fn child_for_call_id(&self, call_id: &str) -> Result<Option<SubagentRecord>, AgentError> {
        let spawn_id =
            Self::spawn_id(call_id).map_err(|error| AgentError::InvalidRequest(error.content))?;
        let parent_lease = self.lease()?;
        self.coordinator
            .list_children(&parent_lease)
            .map_err(|error| AgentError::Runtime(error.to_string()))
            .map(|children| {
                children.into_iter().find(|child| {
                    child
                        .session
                        .subagent
                        .as_ref()
                        .is_some_and(|link| link.spawn_id == spawn_id)
                })
            })
    }

    fn spawn_id(call_id: &str) -> Result<String, ToolOutput> {
        if call_id.is_empty() || call_id.len() > MAX_CALL_ID_BYTES {
            return Err(ToolOutput::error("invalid subagent tool call ID"));
        }
        let mut hash = Sha256::new();
        hash.update(b"openide-spawn-agent-tool-v1\0");
        hash.update((call_id.len() as u64).to_le_bytes());
        hash.update(call_id.as_bytes());
        Ok(format!("tool-{:x}", hash.finalize()))
    }

    fn input_id(call_id: &str) -> Result<String, ToolOutput> {
        if call_id.is_empty() || call_id.len() > MAX_CALL_ID_BYTES {
            return Err(ToolOutput::error("invalid subagent tool call ID"));
        }
        let mut hash = Sha256::new();
        hash.update(b"openide-send-input-tool-v1\0");
        hash.update((call_id.len() as u64).to_le_bytes());
        hash.update(call_id.as_bytes());
        Ok(format!("input-{:x}", hash.finalize()))
    }

    /// Locate a committed input when a parent's tool result was lost. This
    /// lookup itself never starts another turn or repeats an external effect.
    pub fn input_for_call_id(
        &self,
        agent_id: &str,
        call_id: &str,
    ) -> Result<Option<String>, AgentError> {
        let input_id =
            Self::input_id(call_id).map_err(|error| AgentError::InvalidRequest(error.content))?;
        let lease = self.lease()?;
        let control = self
            .coordinator
            .issue_control(&lease)
            .map_err(|error| AgentError::Runtime(error.to_string()))?;
        let input = self
            .coordinator
            .lookup_input_control(&control, agent_id, &input_id)
            .map_err(|error| AgentError::Runtime(error.to_string()))?;
        Ok(input.map(|_| input_id))
    }

    fn schedule_input_drain(
        &self,
        control: crate::SubagentControl,
        child_id: String,
    ) -> Result<(), AgentError> {
        let worker = self.worker.clone();
        let name = format!("subagent-input-{}", &child_id[..child_id.len().min(12)]);
        std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    let _ = runtime.block_on(worker.drain_queued_prepared(
                        control,
                        child_id,
                        CancellationToken::new(),
                    ));
                }
            })
            .map_err(|error| {
                AgentError::EffectUncertain(format!(
                    "subagent input was committed but its worker could not start: {error}"
                ))
            })?;
        Ok(())
    }

    fn send_input(&self, call: &ToolCall) -> Result<ToolOutput, AgentError> {
        let args: SendInputArgs = match serde_json::from_value(call.arguments.clone()) {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolOutput::error(format!(
                    "invalid send_input arguments: {error}"
                )))
            }
        };
        let input_id = match Self::input_id(&call.id) {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let lease = self.lease()?;
        let control = self
            .coordinator
            .issue_control(&lease)
            .map_err(|error| AgentError::Runtime(error.to_string()))?;
        if let Err(error) = self.coordinator.input_state(&control, &args.agent_id) {
            return Ok(ToolOutput::error(error.to_string()));
        }
        if let Err(error) = self.worker.validate_follow_up_child(&args.agent_id) {
            return Ok(ToolOutput::error(error.to_string()));
        }
        let input = match self.coordinator.enqueue_input(
            &lease,
            &args.agent_id,
            &input_id,
            &args.prompt,
            TurnMode::Build,
        ) {
            Ok(input) => input,
            Err(
                error @ (SubagentError::InvalidInput
                | SubagentError::ConflictingInput
                | SubagentError::InputLimit
                | SubagentError::InputClosed(_)
                | SubagentError::NotDirectChild { .. }),
            ) => {
                return Ok(ToolOutput::error(error.to_string()));
            }
            Err(error) => {
                return Err(AgentError::EffectUncertain(format!(
                    "subagent input {} could not be verified: {error}",
                    call.id
                )));
            }
        };
        drop(lease);
        if input.state == QueuedInputState::Pending {
            self.schedule_input_drain(control, args.agent_id.clone())?;
        }
        Ok(ToolOutput::success(
            json!({
                "agent_id":args.agent_id,
                "input_id":input.id,
                "state":input.state
            })
            .to_string(),
        ))
    }

    fn spawn_agent(
        &self,
        call: &ToolCall,
        _context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        let args: SpawnArgs = match serde_json::from_value(call.arguments.clone()) {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolOutput::error(format!(
                    "invalid spawn_agent arguments: {error}"
                )))
            }
        };
        if args.prompt.trim().is_empty() || args.prompt.len() > 64 * 1024 {
            return Ok(ToolOutput::error(
                "subagent prompt must contain 1..65536 bytes",
            ));
        }
        let spawn_id = match Self::spawn_id(&call.id) {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let mut control = self
            .control
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let parent_lease = control
            .parent_lease
            .clone()
            .ok_or_else(|| AgentError::Runtime("subagent coordination is closed".to_owned()))?;
        let children = self
            .coordinator
            .list_children(&parent_lease)
            .map_err(|error| AgentError::Runtime(error.to_string()))?;
        let existing = children.iter().any(|record| {
            record
                .session
                .subagent
                .as_ref()
                .is_some_and(|link| link.spawn_id == spawn_id)
        });
        if !existing && children.len() >= self.limits.max_tasks {
            return Ok(ToolOutput::error(format!(
                "subagent task limit reached ({})",
                self.limits.max_tasks
            )));
        }
        let child =
            match self
                .worker
                .prepare_child(&parent_lease, &spawn_id, &args.prompt, TurnMode::Build)
            {
                Ok(child) => child,
                Err(SubagentWorkerError::Coordination(
                    error @ (SubagentError::ParentChildLimit(_)
                    | SubagentError::DepthLimit
                    | SubagentError::TotalTaskLimit(_)
                    | SubagentError::InvalidSpawnSpec
                    | SubagentError::InvalidTransition { .. }),
                )) => return Ok(ToolOutput::error(error.to_string())),
                Err(error) => {
                    return Err(AgentError::EffectUncertain(format!(
                        "subagent reservation for call {} could not be verified: {error}",
                        call.id
                    )))
                }
            };
        let child_id = child.session.id;
        if child.status == SubagentStatus::Ready && !control.scheduled.contains_key(&child_id) {
            let child_control = self
                .coordinator
                .issue_control(&parent_lease)
                .map_err(|error| AgentError::EffectUncertain(error.to_string()))?;
            // A prepared child outlives the spawning turn. Only an explicit
            // cancel_agent request should cancel its independent worker.
            let cancellation = CancellationToken::new();
            control
                .scheduled
                .insert(child_id.clone(), cancellation.clone());
            let worker = self.worker.clone();
            let spawned = spawn_id.clone();
            let prompt = args.prompt;
            let cleanup_control = Arc::clone(&self.control);
            let cleanup_child_id = child_id.clone();
            let child_id_for_worker = child_id.clone();
            if let Err(error) = std::thread::Builder::new()
                .name(format!("subagent-{}", &child_id[4..12]))
                .spawn(move || {
                    let _cleanup = ScheduledCleanup {
                        control: cleanup_control,
                        child_id: cleanup_child_id,
                    };
                    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        let _ = runtime.block_on(worker.run_prepared(
                            child_control,
                            child_id_for_worker,
                            spawned,
                            prompt,
                            TurnMode::Build,
                            cancellation,
                        ));
                    }
                })
            {
                control.scheduled.remove(&child_id);
                return Err(AgentError::EffectUncertain(format!(
                    "subagent {child_id} was reserved but its worker could not start: {error}"
                )));
            }
        }
        Ok(ToolOutput::success(
            json!({"agent_id":child_id,"status":child.status}).to_string(),
        ))
    }

    async fn wait_agent(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        let args: WaitArgs = match serde_json::from_value(call.arguments.clone()) {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolOutput::error(format!(
                    "invalid wait_agent arguments: {error}"
                )))
            }
        };
        if args.timeout_ms > MAX_WAIT_MS {
            return Ok(ToolOutput::error("wait_agent timeout exceeds 120000 ms"));
        }
        let deadline = Instant::now() + Duration::from_millis(args.timeout_ms);
        let mut drain_scheduled = false;
        loop {
            if context.cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let parent_lease = self.lease()?;
            let children = self
                .coordinator
                .list_children(&parent_lease)
                .map_err(|error| AgentError::Runtime(error.to_string()))?;
            let Some(mut child) = children
                .into_iter()
                .find(|child| child.session.id == args.agent_id)
            else {
                return Ok(ToolOutput::error("unknown direct subagent ID"));
            };
            let control = self
                .coordinator
                .issue_control(&parent_lease)
                .map_err(|error| AgentError::Runtime(error.to_string()))?;
            let (current_status, _, pending_inputs) = self
                .coordinator
                .input_state(&control, &args.agent_id)
                .map_err(|error| AgentError::Runtime(error.to_string()))?;
            child.status = current_status;
            drop(parent_lease);
            if pending_inputs > 0 && child.status == SubagentStatus::Completed && !drain_scheduled {
                self.schedule_input_drain(control, args.agent_id.clone())?;
                drain_scheduled = true;
            }
            if matches!(
                child.status,
                SubagentStatus::Completed
                    | SubagentStatus::Failed
                    | SubagentStatus::Cancelled
                    | SubagentStatus::Interrupted
            ) && (pending_inputs == 0 || child.status != SubagentStatus::Completed)
            {
                return self.observed_child(child);
            }
            if Instant::now() >= deadline {
                return Ok(ToolOutput::success(
                    json!({"agent_id":args.agent_id,"status":child.status,"timed_out":true})
                        .to_string(),
                ));
            }
            tokio::select! {
                biased;
                _ = context.cancel.cancelled() => return Err(AgentError::Cancelled),
                _ = tokio::time::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()))) => {}
            }
        }
    }

    fn observed_child(&self, child: crate::SubagentRecord) -> Result<ToolOutput, AgentError> {
        let id = child.session.id;
        if child.status == SubagentStatus::Interrupted {
            return Ok(ToolOutput::success(
                json!({"agent_id":id,"status":child.status,"needs_recovery":true}).to_string(),
            ));
        }
        let stored = self
            .sessions
            .load(&id)
            .map_err(|error| AgentError::Runtime(error.to_string()))?
            .ok_or_else(|| AgentError::Runtime(format!("subagent {id} disappeared")))?;
        let terminal = stored.events.last().map(|event| &event.kind);
        let output = match (child.status, terminal) {
            (SubagentStatus::Completed, Some(AgentEventKind::TurnCompleted { response, .. })) => {
                json!({"agent_id":id,"status":child.status,"response":response})
            }
            (SubagentStatus::Failed, Some(AgentEventKind::TurnFailed { error })) => {
                json!({"agent_id":id,"status":child.status,"error":error})
            }
            (SubagentStatus::Cancelled, Some(AgentEventKind::TurnCancelled)) => {
                json!({"agent_id":id,"status":child.status})
            }
            (SubagentStatus::Cancelled, None | Some(AgentEventKind::TurnAdmitted { .. })) => {
                // Cancellation before the worker started has no terminal turn.
                json!({"agent_id":id,"status":child.status,"turn_started":false})
            }
            _ => {
                return Ok(ToolOutput::error(format!(
                    "subagent {id} terminal status does not match its durable journal"
                )))
            }
        };
        Ok(ToolOutput::success(output.to_string()))
    }

    fn cancel_agent(&self, call: &ToolCall) -> Result<ToolOutput, AgentError> {
        let args: AgentIdArgs = match serde_json::from_value(call.arguments.clone()) {
            Ok(args) => args,
            Err(error) => {
                return Ok(ToolOutput::error(format!(
                    "invalid cancel_agent arguments: {error}"
                )))
            }
        };
        let parent_lease = self.lease()?;
        let result = self
            .coordinator
            .request_cancel(&parent_lease, &args.agent_id);
        drop(parent_lease);
        match result {
            Ok(child) => {
                if let Some(token) = self
                    .control
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .scheduled
                    .get(&args.agent_id)
                {
                    token.cancel();
                }
                Ok(ToolOutput::success(
                    json!({"agent_id":args.agent_id,"status":child.status,"cancellation_requested":child.cancellation_requested}).to_string(),
                ))
            }
            Err(SubagentError::NotDirectChild { .. })
            | Err(SubagentError::Store(openide_core::StoreError::UnknownSession(_))) => {
                Ok(ToolOutput::error("unknown direct subagent ID"))
            }
            Err(error) => Err(AgentError::Runtime(error.to_string())),
        }
    }
}

impl Drop for SubagentToolExecutor {
    fn drop(&mut self) {
        let mut control = self
            .control
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        control.parent_lease.take();
    }
}

struct ScheduledCleanup {
    control: Arc<Mutex<ControlState>>,
    child_id: String,
}

impl Drop for ScheduledCleanup {
    fn drop(&mut self) {
        self.control
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .scheduled
            .remove(&self.child_id);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentIdArgs {
    agent_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendInputArgs {
    agent_id: String,
    prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    agent_id: String,
    #[serde(default = "default_wait_ms")]
    timeout_ms: u64,
}

fn default_wait_ms() -> u64 {
    30_000
}

pub(crate) fn coordination_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "spawn_agent".to_owned(),
            description: "Start a native Build subagent concurrently and return its stable agent ID. The child uses the tool policy configured by the host.".to_owned(),
            parameters: json!({"type":"object","properties":{"prompt":{"type":"string","minLength":1,"maxLength":65536}},"required":["prompt"],"additionalProperties":false}),
        },
        ToolDefinition {
            name: "wait_agent".to_owned(),
            description: "Wait for a direct subagent without restarting it. Timeout returns its current status.".to_owned(),
            parameters: json!({"type":"object","properties":{"agent_id":{"type":"string","minLength":1,"maxLength":128},"timeout_ms":{"type":"integer","minimum":0,"maximum":120000,"default":30000}},"required":["agent_id"],"additionalProperties":false}),
        },
        ToolDefinition {
            name: "send_input".to_owned(),
            description: "Queue one durable Build follow-up for a direct subagent. The same tool call ID is idempotent. A queued input waits for the child's current turn; interrupted children require explicit recovery.".to_owned(),
            parameters: json!({"type":"object","properties":{"agent_id":{"type":"string","minLength":1,"maxLength":128},"prompt":{"type":"string","minLength":1,"maxLength":16384}},"required":["agent_id","prompt"],"additionalProperties":false}),
        },
        ToolDefinition {
            name: "cancel_agent".to_owned(),
            description: "Request durable cancellation of a direct subagent and its descendants.".to_owned(),
            parameters: json!({"type":"object","properties":{"agent_id":{"type":"string","minLength":1,"maxLength":128}},"required":["agent_id"],"additionalProperties":false}),
        },
    ]
}

#[async_trait]
impl ToolExecutor for SubagentToolExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.base.definitions();
        definitions.extend(coordination_definitions());
        definitions
    }

    fn certifies_parallel_read(&self, name: &str) -> bool {
        if matches!(
            name,
            "spawn_agent" | "send_input" | "wait_agent" | "cancel_agent"
        ) {
            false
        } else {
            self.base.certifies_parallel_read(name)
        }
    }

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        match call.name.as_str() {
            "spawn_agent" => self.spawn_agent(call, context),
            "send_input" => self.send_input(call),
            "wait_agent" => self.wait_agent(call, context).await,
            "cancel_agent" => self.cancel_agent(call),
            _ => self.base.execute(call, context).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalTools, ModelBackend, ModelOutput, SubagentLimits};
    use async_trait::async_trait;
    use openide_core::{AgentMessage, Session};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DelayedModel {
        calls: AtomicUsize,
        active: AtomicUsize,
        maximum_active: AtomicUsize,
        messages: Mutex<Vec<Vec<AgentMessage>>>,
        delay: Duration,
    }

    impl DelayedModel {
        fn new(delay: Duration) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                maximum_active: AtomicUsize::new(0),
                messages: Mutex::new(Vec::new()),
                delay,
            }
        }
    }

    #[async_trait]
    impl ModelBackend for DelayedModel {
        fn model_id(&self) -> &str {
            "fixture-subagent-model"
        }

        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([11; 32])
        }

        async fn complete(
            &self,
            messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.messages.lock().unwrap().push(messages.to_vec());
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum_active.fetch_max(active, Ordering::SeqCst);
            let result = tokio::select! {
                _ = cancel.cancelled() => Err(AgentError::Cancelled),
                _ = tokio::time::sleep(self.delay) => Ok(ModelOutput {
                    content: "child response".to_owned(),
                    tool_calls: Vec::new(),
                }),
            };
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        parent: Session,
        parent_lease: Arc<NativeSessionLease>,
        coordinator: Arc<SubagentCoordinator>,
        model: Arc<DelayedModel>,
        tools: SubagentToolExecutor,
        workspace: std::path::PathBuf,
    }

    fn fixture(max_tasks: usize, delay: Duration) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().to_path_buf();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let coordinator = Arc::new(
            SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap(),
        );
        let model = Arc::new(DelayedModel::new(delay));
        let base: Arc<dyn ToolExecutor> = Arc::new(LocalTools::new());
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            base.clone(),
            "fixture",
            "https://fixture.example/v1",
        );
        let tools = SubagentToolExecutor::new(
            base,
            worker,
            coordinator.clone(),
            sessions.clone(),
            parent_lease.clone(),
            SubagentToolLimits { max_tasks },
        )
        .unwrap();
        Fixture {
            _root: root,
            parent,
            parent_lease,
            coordinator,
            model,
            tools,
            workspace,
        }
    }

    fn context(workspace: &std::path::Path) -> ToolContext {
        ToolContext {
            workspace: workspace.to_path_buf(),
            cancel: CancellationToken::new(),
        }
    }

    fn call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments,
        }
    }

    fn json_output(output: ToolOutput) -> serde_json::Value {
        assert!(!output.is_error, "{}", output.content);
        serde_json::from_str(&output.content).unwrap()
    }

    #[tokio::test]
    async fn spawn_is_concurrent_idempotent_and_bounded_by_total_tasks() {
        let fixture = fixture(2, Duration::from_secs(1));
        for name in ["spawn_agent", "wait_agent", "cancel_agent"] {
            assert!(!fixture.tools.certifies_parallel_read(name));
        }
        let context = context(&fixture.workspace);
        let first = json_output(
            fixture
                .tools
                .execute(
                    &call("call-a", "spawn_agent", json!({"prompt":"first task"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        let second = json_output(
            fixture
                .tools
                .execute(
                    &call("call-b", "spawn_agent", json!({"prompt":"second task"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_ne!(first["agent_id"], second["agent_id"]);
        assert_eq!(
            fixture
                .tools
                .child_for_call_id("call-a")
                .unwrap()
                .unwrap()
                .session
                .id,
            first["agent_id"].as_str().unwrap()
        );
        assert!(fixture
            .tools
            .child_for_call_id("unknown-call")
            .unwrap()
            .is_none());
        let repeated = json_output(
            fixture
                .tools
                .execute(
                    &call("call-a", "spawn_agent", json!({"prompt":"first task"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_eq!(first["agent_id"], repeated["agent_id"]);
        let denied = fixture
            .tools
            .execute(
                &call("call-c", "spawn_agent", json!({"prompt":"third task"})),
                &context,
            )
            .await
            .unwrap();
        assert!(denied.is_error);
        assert!(denied.content.contains("task limit"));
        let changed = fixture
            .tools
            .execute(
                &call("call-a", "spawn_agent", json!({"prompt":"changed task"})),
                &context,
            )
            .await;
        assert!(matches!(changed, Err(AgentError::EffectUncertain(_))));

        for id in [
            first["agent_id"].as_str().unwrap(),
            second["agent_id"].as_str().unwrap(),
        ] {
            let result = json_output(
                fixture
                    .tools
                    .execute(
                        &call(
                            "wait",
                            "wait_agent",
                            json!({"agent_id":id,"timeout_ms":3000}),
                        ),
                        &context,
                    )
                    .await
                    .unwrap(),
            );
            assert_eq!(result["status"], "completed");
            assert_eq!(result["response"], "child response");
        }
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 2);
        assert!(fixture.model.maximum_active.load(Ordering::SeqCst) >= 2);
        fixture.tools.shutdown().await.unwrap();
        drop(fixture.tools);
        drop(fixture.parent_lease);
        let other = SessionStore::open(fixture._root.path().join("sessions")).unwrap();
        other.try_acquire_native_lease(&fixture.parent.id).unwrap();
    }

    #[tokio::test]
    async fn cancel_running_child_and_wait_without_replaying_it() {
        let fixture = fixture(2, Duration::from_secs(5));
        let context = context(&fixture.workspace);
        let spawned = json_output(
            fixture
                .tools
                .execute(
                    &call("call-cancel", "spawn_agent", json!({"prompt":"long task"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        let agent_id = spawned["agent_id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while fixture.model.calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let cancelled = json_output(
            fixture
                .tools
                .execute(
                    &call("cancel", "cancel_agent", json!({"agent_id":agent_id})),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert!(cancelled["cancellation_requested"].as_bool().unwrap());
        let result = json_output(
            fixture
                .tools
                .execute(
                    &call(
                        "wait",
                        "wait_agent",
                        json!({"agent_id":agent_id,"timeout_ms":3000}),
                    ),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_eq!(result["status"], "cancelled");
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        let retry = json_output(
            fixture
                .tools
                .execute(
                    &call("call-cancel", "spawn_agent", json!({"prompt":"long task"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_eq!(retry["agent_id"], spawned["agent_id"]);
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        fixture.tools.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn waiting_can_be_cancelled_without_cancelling_the_child() {
        let fixture = fixture(1, Duration::from_secs(5));
        let context = context(&fixture.workspace);
        let spawned = json_output(
            fixture
                .tools
                .execute(
                    &call("call-wait", "spawn_agent", json!({"prompt":"long task"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        let wait_cancel = CancellationToken::new();
        let wait_context = ToolContext {
            workspace: fixture.workspace.clone(),
            cancel: wait_cancel.clone(),
        };
        let wait_call = call(
            "wait",
            "wait_agent",
            json!({"agent_id":spawned["agent_id"],"timeout_ms":3000}),
        );
        let waiting = fixture.tools.execute(&wait_call, &wait_context);
        tokio::pin!(waiting);
        tokio::select! {
            result = &mut waiting => panic!("wait ended before cancellation: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        wait_cancel.cancel();
        assert!(matches!(waiting.await, Err(AgentError::Cancelled)));
        assert!(
            !fixture
                .coordinator
                .list_children(&fixture.parent_lease)
                .unwrap()[0]
                .cancellation_requested
        );
        fixture
            .tools
            .execute(
                &call(
                    "cancel",
                    "cancel_agent",
                    json!({"agent_id":spawned["agent_id"]}),
                ),
                &context,
            )
            .await
            .unwrap();
        fixture
            .tools
            .execute(
                &call(
                    "wait-again",
                    "wait_agent",
                    json!({"agent_id":spawned["agent_id"],"timeout_ms":3000}),
                ),
                &context,
            )
            .await
            .unwrap();
        fixture.tools.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_keeps_running_across_parent_lease_handoff() {
        let Fixture {
            _root,
            parent,
            parent_lease,
            coordinator,
            model,
            tools,
            workspace,
        } = fixture(1, Duration::from_millis(1_000));
        let context = context(&workspace);
        let spawned = json_output(
            tools
                .execute(
                    &call("cross-turn", "spawn_agent", json!({"prompt":"continue"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while model.active.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let next_base = tools.base.clone();
        let next_worker = tools.worker.clone();
        drop(tools);
        drop(parent_lease);

        let next_store = Arc::new(SessionStore::open(_root.path().join("sessions")).unwrap());
        let next_lease = Arc::new(next_store.try_acquire_native_lease(&parent.id).unwrap());
        assert_eq!(model.active.load(Ordering::SeqCst), 1);
        let next_tools = SubagentToolExecutor::new(
            next_base,
            next_worker,
            coordinator,
            next_store,
            next_lease,
            SubagentToolLimits { max_tasks: 1 },
        )
        .unwrap();
        let result = json_output(
            next_tools
                .execute(
                    &call(
                        "wait-next-turn",
                        "wait_agent",
                        json!({"agent_id":spawned["agent_id"],"timeout_ms":3000}),
                    ),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_eq!(result["status"], "completed");
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        next_tools.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn next_parent_turn_can_cancel_existing_child_without_reexecution() {
        let Fixture {
            _root,
            parent,
            parent_lease,
            coordinator,
            model,
            tools,
            workspace,
        } = fixture(1, Duration::from_secs(5));
        let context = context(&workspace);
        let spawned = json_output(
            tools
                .execute(
                    &call("cross-cancel", "spawn_agent", json!({"prompt":"long task"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while model.calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let next_base = tools.base.clone();
        let next_worker = tools.worker.clone();
        drop(tools);
        drop(parent_lease);

        let next_store = Arc::new(SessionStore::open(_root.path().join("sessions")).unwrap());
        let next_lease = Arc::new(next_store.try_acquire_native_lease(&parent.id).unwrap());
        let next_tools = SubagentToolExecutor::new(
            next_base,
            next_worker,
            coordinator,
            next_store,
            next_lease,
            SubagentToolLimits { max_tasks: 1 },
        )
        .unwrap();
        let agent_id = spawned["agent_id"].as_str().unwrap();
        let cancelled = json_output(
            next_tools
                .execute(
                    &call("cancel-next", "cancel_agent", json!({"agent_id":agent_id})),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert!(cancelled["cancellation_requested"].as_bool().unwrap());
        let result = json_output(
            next_tools
                .execute(
                    &call(
                        "wait-next",
                        "wait_agent",
                        json!({"agent_id":agent_id,"timeout_ms":3000}),
                    ),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_eq!(result["status"], "cancelled");
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        next_tools.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn send_input_during_running_turn_executes_once_after_it_and_retries_are_idempotent() {
        let fixture = fixture(2, Duration::from_millis(120));
        let context = context(&fixture.workspace);
        let spawned = json_output(
            fixture
                .tools
                .execute(
                    &call(
                        "spawn-follow-up",
                        "spawn_agent",
                        json!({"prompt":"first task"}),
                    ),
                    &context,
                )
                .await
                .unwrap(),
        );
        let child_id = spawned["agent_id"].as_str().unwrap();
        let input = call(
            "input-follow-up",
            "send_input",
            json!({"agent_id":child_id,"prompt":"second task"}),
        );
        let queued = json_output(fixture.tools.execute(&input, &context).await.unwrap());
        assert_eq!(queued["state"], "pending");
        assert_eq!(
            fixture
                .tools
                .input_for_call_id(child_id, "input-follow-up")
                .unwrap()
                .as_deref(),
            queued["input_id"].as_str()
        );
        let waited = json_output(
            fixture
                .tools
                .execute(
                    &call(
                        "wait-follow-up",
                        "wait_agent",
                        json!({"agent_id":child_id,"timeout_ms":5000}),
                    ),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_eq!(waited["status"], "completed");
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 2);
        {
            let messages = fixture.model.messages.lock().unwrap();
            assert!(messages[1]
                .iter()
                .any(|message| message.content == "first task"));
            assert!(messages[1]
                .iter()
                .any(|message| message.content == "second task"));
        }
        let repeated = json_output(fixture.tools.execute(&input, &context).await.unwrap());
        assert_eq!(repeated["state"], "done");
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 2);
        let conflict = fixture
            .tools
            .execute(
                &call(
                    "input-follow-up",
                    "send_input",
                    json!({"agent_id":child_id,"prompt":"changed"}),
                ),
                &context,
            )
            .await
            .unwrap();
        assert!(conflict.is_error);
        let stored = fixture.tools.sessions.load(child_id).unwrap().unwrap();
        assert_eq!(
            stored
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn send_input_after_completed_turn_starts_a_new_turn() {
        let fixture = fixture(2, Duration::from_millis(25));
        let context = context(&fixture.workspace);
        let spawned = json_output(
            fixture
                .tools
                .execute(
                    &call("spawn-idle", "spawn_agent", json!({"prompt":"first task"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        let child_id = spawned["agent_id"].as_str().unwrap();
        let first = json_output(
            fixture
                .tools
                .execute(
                    &call(
                        "wait-idle-first",
                        "wait_agent",
                        json!({"agent_id":child_id,"timeout_ms":5000}),
                    ),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_eq!(first["status"], "completed");
        let queued = json_output(
            fixture
                .tools
                .execute(
                    &call(
                        "idle-input",
                        "send_input",
                        json!({"agent_id":child_id,"prompt":"second task"}),
                    ),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_eq!(queued["state"], "pending");
        let second = json_output(
            fixture
                .tools
                .execute(
                    &call(
                        "wait-idle-second",
                        "wait_agent",
                        json!({"agent_id":child_id,"timeout_ms":5000}),
                    ),
                    &context,
                )
                .await
                .unwrap(),
        );
        assert_eq!(second["status"], "completed");
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 2);
    }
}
