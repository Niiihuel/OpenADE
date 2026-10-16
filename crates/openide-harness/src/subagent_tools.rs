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
    delegation_enabled: bool,
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
            delegation_enabled: true,
        })
    }

    /// Restrict new work in this parent turn while retaining observation and
    /// cancellation of existing children. Already admitted workers keep their profile.
    pub fn with_delegation_enabled(mut self, enabled: bool) -> Self {
        self.delegation_enabled = enabled;
        self
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

    /// Recover only data from a committed response whose spawn tool actually
    /// started. The caller must still validate the child reservation digest.
    /// Returns the child's prompt, original model, the frozen project/worker
    /// instructions and the mode its role runs in. A recovered read-only child
    /// must come back read-only, so callers start it in the returned mode.
    pub fn reserved_spawn_request(
        events: &[openide_core::AgentEvent],
        spawn_id: &str,
    ) -> Result<(String, String, String, TurnMode), AgentError> {
        let mut model = None;
        let mut instructions = None;
        let mut calls = Vec::new();
        let mut found = None;
        for event in events {
            match &event.kind {
                AgentEventKind::TurnAdmitted {
                    model: admitted,
                    mode,
                    ..
                }
                | AgentEventKind::TurnAdmittedWithImages {
                    model: admitted,
                    mode,
                    ..
                } => {
                    model = (*mode == TurnMode::Build).then(|| admitted.clone());
                    instructions = None;
                    calls.clear();
                }
                AgentEventKind::SystemInstructionsCommitted {
                    content,
                    version,
                    digest,
                } => {
                    if *version != openide_core::SYSTEM_INSTRUCTIONS_VERSION
                        || openide_core::system_instructions_digest(content).as_ref()
                            != Some(digest)
                    {
                        return Err(AgentError::InvalidRequest(
                            "invalid frozen spawn instructions".into(),
                        ));
                    }
                    instructions = Some(content.clone())
                }
                AgentEventKind::ModelResponseCommitted { assistant, .. } => {
                    calls = assistant.tool_calls.clone()
                }
                AgentEventKind::ToolStarted { tool_call } if tool_call.name == "spawn_agent" => {
                    if Self::spawn_id(&tool_call.id)
                        .map_err(|e| AgentError::InvalidRequest(e.content))?
                        != spawn_id
                    {
                        continue;
                    }
                    if !calls.iter().any(|call| call == tool_call) {
                        return Err(AgentError::InvalidRequest(
                            "spawn was not in the committed model response".into(),
                        ));
                    }
                    let args: SpawnArgs = serde_json::from_value(tool_call.arguments.clone())
                        .map_err(|e| AgentError::InvalidRequest(e.to_string()))?;
                    if args.prompt.trim().is_empty() || args.prompt.len() > 64 * 1024 {
                        return Err(AgentError::InvalidRequest(
                            "invalid reserved spawn prompt".into(),
                        ));
                    }
                    let model = model.clone().ok_or_else(|| {
                        AgentError::InvalidRequest("spawn has no original model admission".into())
                    })?;
                    let instructions = instructions.clone().ok_or_else(|| {
                        AgentError::InvalidRequest("spawn has no frozen instructions".into())
                    })?;
                    let base =
                        crate::native_goal_base_instructions(&instructions).unwrap_or(instructions);
                    let (prompt, mode) =
                        crate::subagent_roles::resolve(&args.prompt, args.role.as_deref())
                            .map_err(AgentError::InvalidRequest)?;
                    let candidate = (prompt, model, base, mode);
                    if found
                        .as_ref()
                        .is_some_and(|previous| previous != &candidate)
                    {
                        return Err(AgentError::InvalidRequest(
                            "conflicting original spawn context".into(),
                        ));
                    }
                    found = Some(candidate);
                }
                _ => {}
            }
        }
        found.ok_or_else(|| {
            AgentError::InvalidRequest("original started spawn request is missing".into())
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
        let mode = match self.worker.follow_up_mode(&args.agent_id) {
            Ok(mode) => mode,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let input = match self.coordinator.enqueue_input(
            &lease,
            &args.agent_id,
            &input_id,
            &args.prompt,
            mode,
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
        let (prompt, mode) = match crate::subagent_roles::resolve(&args.prompt, args.role.as_deref()) {
            Ok(resolved) => resolved,
            Err(error) => return Ok(ToolOutput::error(error)),
        };
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
                .prepare_child(&parent_lease, &spawn_id, &prompt, mode)
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
                            mode,
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
            // A child paused on a provider limit is a normal result: the
            // parent pauses at its next model boundary and waits again later.
            let paused = self
                .sessions
                .load(&id)
                .map_err(|error| AgentError::Runtime(error.to_string()))?
                .and_then(|stored| match stored.events.last().map(|event| &event.kind) {
                    Some(AgentEventKind::TurnPaused {
                        class,
                        resets_at_ms,
                        ..
                    }) => Some((*class, *resets_at_ms)),
                    _ => None,
                });
            if let Some((class, resets_at_ms)) = paused {
                return Ok(ToolOutput::success(
                    json!({"agent_id":id,"status":"paused","reason":class,"resets_at":resets_at_ms})
                        .to_string(),
                ));
            }
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
            .request_cancel_with_affected(&parent_lease, &args.agent_id);
        drop(parent_lease);
        match result {
            Ok((child, affected)) => {
                self.worker.revoke_persistent_commands_for(&affected)?;
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
    /// Absent in every journal written before roles existed.
    #[serde(default)]
    role: Option<String>,
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
            description: format!(
                "Start a native subagent concurrently and return its stable agent ID. The child uses the tool policy configured by the host. An optional role narrows it: {}",
                crate::subagent_roles::catalog()
            ),
            parameters: json!({"type":"object","properties":{"prompt":{"type":"string","minLength":1,"maxLength":65536},"role":{"type":"string","enum":crate::subagent_roles::ROLES.iter().map(|role| role.name).collect::<Vec<_>>()}},"required":["prompt"],"additionalProperties":false}),
        },
        ToolDefinition {
            name: "wait_agent".to_owned(),
            description: "Wait for a direct subagent without restarting it. Timeout returns its current status.".to_owned(),
            parameters: json!({"type":"object","properties":{"agent_id":{"type":"string","minLength":1,"maxLength":128},"timeout_ms":{"type":"integer","minimum":0,"maximum":120000,"default":30000}},"required":["agent_id"],"additionalProperties":false}),
        },
        ToolDefinition {
            name: "send_input".to_owned(),
            description: "Queue one durable follow-up for a direct subagent, preserving its original Plan or Build mode. Read-only roles stay read-only. The same tool call ID is idempotent. A queued input waits for the child's current turn; interrupted children require explicit recovery.".to_owned(),
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
    async fn execute_streaming(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        output: crate::ToolOutputSender,
    ) -> Result<ToolOutput, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if matches!(
            call.name.as_str(),
            "spawn_agent" | "send_input" | "wait_agent" | "cancel_agent"
        ) {
            self.execute(call, context).await
        } else {
            self.base.execute_streaming(call, context, output).await
        }
    }

    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.base.definitions();
        definitions.extend(coordination_definitions().into_iter().filter(|definition|
            self.delegation_enabled || !matches!(definition.name.as_str(), "spawn_agent" | "send_input")));
        definitions
    }

    fn certifies_plan_read(&self, name: &str) -> bool {
        !matches!(
            name,
            "spawn_agent" | "send_input" | "wait_agent" | "cancel_agent"
        ) && self.base.certifies_plan_read(name)
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
        if !self.delegation_enabled && matches!(call.name.as_str(), "spawn_agent" | "send_input") {
            return Ok(ToolOutput::error("New subagent delegation is disabled in Settings"));
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
        /// Tool names offered on each call, in the same order as `messages`.
        tools: Mutex<Vec<Vec<String>>>,
        delay: Duration,
    }

    impl DelayedModel {
        fn new(delay: Duration) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                maximum_active: AtomicUsize::new(0),
                messages: Mutex::new(Vec::new()),
                tools: Mutex::new(Vec::new()),
                delay,
            }
        }
    }

    #[test]
    fn initial_reservation_source_requires_original_started_build_call_and_frozen_context() {
        let call = ToolCall {
            id: "original-call".into(),
            name: "spawn_agent".into(),
            arguments: json!({"prompt":"Complete original scope"}),
        };
        let spawn = SubagentToolExecutor::spawn_id(&call.id).unwrap();
        let kinds = vec![
            AgentEventKind::TurnAdmitted {
                turn_id: "parent-turn".into(),
                prompt: "Parent scope".into(),
                model: "exact-model".into(),
                mode: TurnMode::Build,
            },
            AgentEventKind::SystemInstructionsCommitted {
                version: openide_core::SYSTEM_INSTRUCTIONS_VERSION,
                digest: openide_core::system_instructions_digest("Frozen project").unwrap(),
                content: "Frozen project".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: openide_core::AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            },
        ];
        let events: Vec<_> = kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| openide_core::AgentEvent::new("parent", index as u64 + 1, kind))
            .collect();
        assert_eq!(
            SubagentToolExecutor::reserved_spawn_request(&events, &spawn).unwrap(),
            (
                "Complete original scope".into(),
                "exact-model".into(),
                "Frozen project".into(),
                TurnMode::Build
            )
        );
        // A role in the journaled call comes back as the same narrowed child:
        // its preamble on the prompt and, for a read-only role, Plan mode.
        let with_role = |role: serde_json::Value, only_started: bool| {
            let mut events = events.clone();
            for index in [2, 3] {
                let arguments = json!({"prompt":"Complete original scope", "role": role});
                match &mut events[index].kind {
                    AgentEventKind::ModelResponseCommitted { assistant, .. } if !only_started => {
                        assistant.tool_calls[0].arguments = arguments
                    }
                    AgentEventKind::ToolStarted { tool_call } => tool_call.arguments = arguments,
                    _ => {}
                }
            }
            SubagentToolExecutor::reserved_spawn_request(&events, &spawn)
        };
        let (prompt, model, instructions, mode) = with_role(json!("explorer"), false).unwrap();
        assert!(prompt.ends_with("Task:\nComplete original scope") && prompt.len() > 60, "{prompt}");
        assert_eq!((model.as_str(), instructions.as_str(), mode), ("exact-model", "Frozen project", TurnMode::Plan));
        assert_eq!(with_role(json!("worker"), false).unwrap().3, TurnMode::Build);
        assert!(with_role(json!("admin"), false).is_err());
        // A role the model never committed to cannot be slipped into the started call.
        assert!(with_role(json!("explorer"), true).is_err());
        assert!(SubagentToolExecutor::reserved_spawn_request(&events[..3], &spawn).is_err());
        assert!(SubagentToolExecutor::reserved_spawn_request(&events, "foreign-spawn").is_err());
        let mut plan = events.clone();
        if let AgentEventKind::TurnAdmitted { mode, .. } = &mut plan[0].kind {
            *mode = TurnMode::Plan;
        }
        assert!(SubagentToolExecutor::reserved_spawn_request(&plan, &spawn).is_err());
        let mut changed = events.clone();
        if let AgentEventKind::ToolStarted { tool_call } = &mut changed[3].kind {
            tool_call.arguments = json!({"prompt":"Changed scope"});
        }
        assert!(SubagentToolExecutor::reserved_spawn_request(&changed, &spawn).is_err());
        let mut tampered = events.clone();
        if let AgentEventKind::SystemInstructionsCommitted { content, .. } = &mut tampered[1].kind {
            *content = "Changed project".into();
        }
        assert!(SubagentToolExecutor::reserved_spawn_request(&tampered, &spawn).is_err());
        let mut unknown = events.clone();
        for index in [2, 3] {
            match &mut unknown[index].kind {
                AgentEventKind::ModelResponseCommitted { assistant, .. } => {
                    assistant.tool_calls[0].arguments =
                        json!({"prompt":"Complete original scope", "permissions":"allow-all"})
                }
                AgentEventKind::ToolStarted { tool_call } => {
                    tool_call.arguments =
                        json!({"prompt":"Complete original scope", "permissions":"allow-all"})
                }
                _ => unreachable!(),
            }
        }
        assert!(SubagentToolExecutor::reserved_spawn_request(&unknown, &spawn).is_err());
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
            tools: &[ToolDefinition],
            cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            {
                // One lock for both, so a call's messages and tools share an
                // index. Scoped: a guard must not live across the await below.
                let mut recorded = self.messages.lock().unwrap();
                recorded.push(messages.to_vec());
                self.tools
                    .lock()
                    .unwrap()
                    .push(tools.iter().map(|tool| tool.name.clone()).collect());
            }
            if messages.iter().any(|message| message.content.contains("hit the provider limit")) {
                let mut failure = crate::ProviderFailure::new(
                    crate::FailureClass::LimitReached,
                    "HTTP 429: usage_limit_reached",
                );
                failure.resets_at_ms = Some(4_000_000_000_000);
                return Err(AgentError::Provider(failure));
            }
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
        fixture_with_tools(max_tasks, delay, LocalTools::new())
    }

    fn fixture_with_tools(max_tasks: usize, delay: Duration, base: LocalTools) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().to_path_buf();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let coordinator = Arc::new(
            SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap(),
        );
        let model = Arc::new(DelayedModel::new(delay));
        let base: Arc<dyn ToolExecutor> = Arc::new(base);
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

    /// End to end through the real executor and worker: what each child's
    /// model is actually given.
    #[tokio::test]
    async fn role_e2e_an_explorer_child_runs_without_write_tools_and_a_plain_child_keeps_them() {
        // The host allows writes and commands here, so a role is the only
        // thing that can take them away from a child.
        let fixture = fixture_with_tools(
            4,
            Duration::from_millis(20),
            LocalTools::new().with_write_access(true),
        );
        let context = context(&fixture.workspace);
        let spawn = |id: &'static str, arguments: serde_json::Value| {
            let tools = &fixture.tools;
            let context = &context;
            async move { tools.execute(&call(id, "spawn_agent", arguments), context).await.unwrap() }
        };
        let refused = spawn("call-bad", json!({"prompt":"x","role":"admin"})).await;
        assert!(refused.is_error && refused.content.contains("explorer"), "{}", refused.content);

        let plain = json_output(spawn("call-plain", json!({"prompt":"change the parser"})).await);
        let explorer = json_output(
            spawn("call-explorer", json!({"prompt":"find the parser","role":"explorer"})).await,
        );
        for agent in [&plain, &explorer] {
            let waited = json_output(
                fixture
                    .tools
                    .execute(
                        &call(
                            "wait",
                            "wait_agent",
                            json!({"agent_id": agent["agent_id"], "timeout_ms": 20_000}),
                        ),
                        &context,
                    )
                    .await
                    .unwrap(),
            );
            assert_eq!(waited["response"], "child response", "{waited}");
        }
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 2, "the refused spawn must not run a child");

        let messages = fixture.model.messages.lock().unwrap().clone();
        let tools = fixture.model.tools.lock().unwrap().clone();
        let offered = |needle: &str| {
            let index = messages
                .iter()
                .position(|call| call.iter().any(|message| message.content.contains(needle)))
                .unwrap_or_else(|| panic!("no child saw {needle}"));
            (messages[index].clone(), tools[index].clone())
        };
        let (plain_messages, plain_tools) = offered("change the parser");
        let (explorer_messages, explorer_tools) = offered("find the parser");
        let writes = ["write_file", "run_command", "apply_patch", "spawn_agent"];

        // The plain child is the child the host always offered.
        assert!(plain_tools.iter().any(|name| name == "write_file"), "{plain_tools:?}");
        assert!(!plain_messages.iter().any(|message| message.content.contains("explorer subagent")));

        // The explorer got its role's instructions, can still read, and was
        // offered nothing that changes the project or spawns more agents.
        assert!(explorer_messages
            .iter()
            .any(|message| message.content.contains("You are an explorer subagent")
                && message.content.ends_with("Task:\nfind the parser")));
        assert!(explorer_tools.iter().any(|name| name == "read_file"), "{explorer_tools:?}");
        for name in writes {
            assert!(!explorer_tools.iter().any(|tool| tool == name), "{name} offered: {explorer_tools:?}");
        }

        // Its saved record says so too, which is what recovery reads.
        let record = fixture.tools.child_for_call_id("call-explorer").unwrap().unwrap();
        assert_eq!(record.session.id, explorer["agent_id"].as_str().unwrap());
    }

    #[tokio::test]
    async fn readonly_roles_remain_plan_on_followup_after_store_reopen() {
        for role in ["explorer", "reviewer"] {
            let fixture = fixture_with_tools(4, Duration::from_millis(10), LocalTools::new().with_write_access(true));
            let context = context(&fixture.workspace);
            let spawned = json_output(fixture.tools.execute(&call("readonly", "spawn_agent", json!({"prompt":"inspect source", "role":role})), &context).await.unwrap());
            let id = spawned["agent_id"].as_str().unwrap();
            let wait = call("wait", "wait_agent", json!({"agent_id":id,"timeout_ms":3000}));
            assert_eq!(json_output(fixture.tools.execute(&wait, &context).await.unwrap())["status"], "completed");
            fixture.tools.shutdown().await.unwrap();
            // Reconstruct from disk and host configuration, without retaining a role map.
            let sessions = Arc::new(SessionStore::open(fixture._root.path().join("sessions")).unwrap());
            let coordinator = Arc::new(SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap());
            let worker = SubagentWorker::new(coordinator.clone(), sessions.clone(), fixture.model.clone(), fixture.tools.base.clone(), "fixture", "https://fixture.example/v1");
            assert_eq!(worker.follow_up_mode(id).unwrap(), TurnMode::Plan);
            let tools = SubagentToolExecutor::new(fixture.tools.base.clone(), worker, coordinator, sessions.clone(), fixture.parent_lease.clone(), SubagentToolLimits { max_tasks:4 }).unwrap();
            let input = call("continue-readonly", "send_input", json!({"agent_id":id,"prompt":"inspect another file"}));
            json_output(tools.execute(&input, &context).await.unwrap());
            assert_eq!(json_output(tools.execute(&wait, &context).await.unwrap())["status"], "completed");
            assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 2);
            for offered in fixture.model.tools.lock().unwrap().iter() {
                assert!(offered.iter().any(|name| name == "read_file"), "{offered:?}");
                for denied in ["write_file", "run_command", "apply_patch", "spawn_agent", "send_input"] {
                    assert!(!offered.iter().any(|name| name == denied), "{role}: {offered:?}");
                }
            }
            let stored = sessions.load(id).unwrap().unwrap();
            let modes = stored.events.iter().filter_map(|event| match event.kind { AgentEventKind::TurnAdmitted { mode, .. } => Some(mode), _ => None }).collect::<Vec<_>>();
            assert_eq!(modes, vec![TurnMode::Plan, TurnMode::Plan]);
            tools.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn ready_followup_uses_committed_spawn_authority_and_rejects_prompt_impersonation() {
        let mut fixture = fixture_with_tools(4, Duration::from_millis(10), LocalTools::new().with_write_access(true));
        fixture.tools.worker = fixture.tools.worker.clone().with_system_instructions("Frozen project");
        let context = context(&fixture.workspace);
        let spawn_call = call("ready-readonly", "spawn_agent", json!({"prompt":"inspect source", "role":"reviewer"}));
        let spawn_id = SubagentToolExecutor::spawn_id(&spawn_call.id).unwrap();
        let (prompt, mode) = crate::subagent_roles::resolve("inspect source", Some("reviewer")).unwrap();
        let child = fixture.tools.worker.prepare_child(&fixture.parent_lease, &spawn_id, &prompt, mode).unwrap();
        let id = &child.session.id;
        let followup = call("ready-input", "send_input", json!({"agent_id":id,"prompt":"continue"}));
        assert!(fixture.tools.execute(&followup, &context).await.unwrap().is_error);
        assert_eq!(fixture.coordinator.open_input_count_for_ui(id).unwrap(), 0);
        let sessions = &fixture.tools.sessions;
        let admission = sessions.admit_native_turn(&fixture.parent.id, "parent-turn", "parent", fixture.model.model_id(), TurnMode::Build).unwrap();
        let kinds = [
            AgentEventKind::SystemInstructionsCommitted { version:openide_core::SYSTEM_INSTRUCTIONS_VERSION, digest:openide_core::system_instructions_digest("Frozen project").unwrap(), content:"Frozen project".into() },
            AgentEventKind::ModelResponseCommitted { step:1, assistant:AgentMessage::assistant("", vec![spawn_call.clone()]), response_id:None, usage:None },
            AgentEventKind::ToolStarted { tool_call:spawn_call },
        ];
        for (index, kind) in kinds.into_iter().enumerate() {
            sessions.append_native_event(&openide_core::AgentEvent::new(&fixture.parent.id, admission.sequence + index as u64 + 1, kind)).unwrap();
        }
        assert_eq!(fixture.tools.worker.follow_up_mode(id).unwrap(), TurnMode::Plan);
        json_output(fixture.tools.execute(&followup, &context).await.unwrap());
        assert_eq!(fixture.coordinator.open_input_count_for_ui(id).unwrap(), 1);
        let control = fixture.coordinator.issue_control(&fixture.parent_lease).unwrap();
        fixture.tools.worker.run_prepared(control, id.clone(), spawn_id, prompt, mode, CancellationToken::new()).await.unwrap();
        let wait = call("wait-ready", "wait_agent", json!({"agent_id":id,"timeout_ms":3000}));
        assert_eq!(json_output(fixture.tools.execute(&wait, &context).await.unwrap())["status"], "completed");
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 2);
        assert!(fixture.model.tools.lock().unwrap().iter().all(|tools| !tools.iter().any(|name| name == "write_file")));
        fixture.tools.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn disabling_delegation_preserves_wait_and_cancel_but_rejects_new_work() {
        let fixture = fixture(4, Duration::from_secs(5));
        let context = context(&fixture.workspace);
        let spawned = json_output(fixture.tools.execute(&call("active", "spawn_agent", json!({"prompt":"work"})), &context).await.unwrap());
        let id = spawned["agent_id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(3), async { while fixture.model.calls.load(Ordering::SeqCst) == 0 { tokio::time::sleep(Duration::from_millis(5)).await; } }).await.unwrap();
        let tools = fixture.tools.with_delegation_enabled(false);
        let names = tools.definitions().into_iter().map(|definition| definition.name).collect::<Vec<_>>();
        assert!(!names.iter().any(|name| name == "spawn_agent" || name == "send_input"));
        assert!(names.iter().any(|name| name == "wait_agent") && names.iter().any(|name| name == "cancel_agent"));
        for new in [call("denied-spawn", "spawn_agent", json!({"prompt":"new task"})), call("denied-input", "send_input", json!({"agent_id":id,"prompt":"new input"}))] {
            assert!(tools.execute(&new, &context).await.unwrap().is_error);
            let (output, _receiver) = crate::tool_output::tool_output_channel(context.cancel.clone());
            assert!(tools.execute_streaming(&new, &context, output).await.unwrap().is_error);
        }
        assert_eq!(fixture.coordinator.list_children(&fixture.parent_lease).unwrap().len(), 1);
        assert_eq!(fixture.coordinator.open_input_count_for_ui(id).unwrap(), 0);
        json_output(tools.execute(&call("cancel", "cancel_agent", json!({"agent_id":id})), &context).await.unwrap());
        let waited = json_output(tools.execute(&call("wait", "wait_agent", json!({"agent_id":id,"timeout_ms":3000})), &context).await.unwrap());
        assert_eq!(waited["status"], "cancelled");
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        tools.shutdown().await.unwrap();
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
    async fn waiting_for_a_child_paused_on_a_limit_is_a_normal_result_that_can_be_read_again() {
        let fixture = fixture(2, Duration::from_millis(10));
        let context = context(&fixture.workspace);
        let spawned = json_output(
            fixture
                .tools
                .execute(
                    &call("call-limit", "spawn_agent", json!({"prompt":"hit the provider limit"})),
                    &context,
                )
                .await
                .unwrap(),
        );
        let agent_id = spawned["agent_id"].as_str().unwrap();
        let wait = call("wait", "wait_agent", json!({"agent_id":agent_id,"timeout_ms":3000}));
        let mut results = Vec::new();
        for _ in 0..2 {
            let output = fixture.tools.execute(&wait, &context).await.unwrap();
            assert!(!output.is_error);
            results.push(json_output(output));
        }
        // A pure read: the second wait returns the same and asks no model.
        assert_eq!(results[0], results[1]);
        assert_eq!(
            results[0],
            json!({"agent_id":agent_id,"status":"paused","reason":"limit_reached","resets_at":4_000_000_000_000u64})
        );
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        // The parent's runner reads that committed result as its cue to pause.
        let mut assistant = AgentMessage::assistant("", vec![wait.clone()]);
        assistant.tool_calls[0].id = "wait".into();
        let history = [
            assistant,
            AgentMessage::tool("wait".to_owned(), results[0].to_string()),
        ];
        assert_eq!(
            paused_child_in_last_batch(&history).map(|(_, resets_at)| resets_at),
            Some(Some(4_000_000_000_000))
        );
        assert!(paused_child_in_last_batch(&history[..1]).is_none());
        fixture.tools.shutdown().await.unwrap();
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
        tokio::time::timeout(Duration::from_secs(3), async {
            while fixture.model.calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
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

/// The paused child reported by a `wait_agent` result of the last tool batch:
/// its reason and reset time. The committed result is the durable signal.
pub(crate) fn paused_child_in_last_batch(
    messages: &[openide_core::AgentMessage],
) -> Option<(String, Option<u64>)> {
    let results = messages
        .iter()
        .rev()
        .take_while(|message| message.role == openide_core::MessageRole::Tool)
        .collect::<Vec<_>>();
    let calls = &messages.get(messages.len().checked_sub(results.len() + 1)?)?.tool_calls;
    results.into_iter().find_map(|result| {
        let call = calls
            .iter()
            .find(|call| Some(&call.id) == result.tool_call_id.as_ref())?;
        let output: serde_json::Value = (call.name == "wait_agent")
            .then(|| serde_json::from_str(&result.content).ok())
            .flatten()?;
        (output["status"] == "paused").then(|| {
            (
                format!(
                    "subagent {} is paused: {}",
                    output["agent_id"].as_str().unwrap_or_default(),
                    output["reason"].as_str().unwrap_or_default()
                ),
                output["resets_at"].as_u64(),
            )
        })
    })
}
