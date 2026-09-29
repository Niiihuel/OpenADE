//! Execute a native child turn while preserving its parent/child lifecycle.
//!
//! The caller owns the parent OS lease. This worker accepts only an exact
//! local tool catalog; external tool preparation requires its own pinned
//! identity before it can be added to child turns.

use crate::permission::PendingPermissions;
use crate::subagent_tools::coordination_definitions;
use crate::subagents::{QueuedInputState, QueuedSubagentInput, SubagentControl, SubagentRunLease};
use crate::tool_scheduler::trusted_local_read;
use crate::{
    native_model_context_digest, AgentError, AgentRunner, CommandHooks, CommandIsolation,
    HookValidationError, ModelBackend, PermissionChoice, PermissionPolicy, ResumeDisposition,
    ResumeEngine, ResumeError, RunLimits, SubagentCoordinator, SubagentError, SubagentOutcome,
    SubagentRecord, SubagentSpec, SubagentStatus, SubagentToolExecutor, SubagentToolLimits,
    TokenBudgetPolicy, ToolCatalog, ToolCatalogError, ToolExecutor,
};
use openide_core::{
    replay_native, AgentEvent, AgentEventKind, AgentMessage, AgentRunRequest,
    CheckpointCompatibility, CheckpointError, CheckpointFileStore, NativeSessionLease,
    PermissionDecision, ReplayError, ReplayRecovery, SessionStore, StoreError, ToolCall, TurnMode,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const TERMINAL_DRAIN_LIMIT: Duration = Duration::from_secs(5);
const BRIDGE_SUBSCRIBER_CAPACITY: usize = 256;

/// A child event reaches subscribers only after the runner has synced it to
/// the child's journal. `Closed` means the in-memory approval authority is
/// gone; consumers can reload the journal to recover missed events.
#[derive(Clone, Debug, PartialEq)]
pub enum SubagentBridgeEvent {
    Event {
        child_session_id: String,
        event: Box<AgentEvent>,
    },
    Closed {
        child_session_id: String,
    },
}

struct ActiveChild {
    generation: Uuid,
    permissions: Arc<PendingPermissions>,
    cancellation: CancellationToken,
    request_ids: HashSet<String>,
    cancelling: bool,
}

#[derive(Default)]
struct BridgeState {
    active: HashMap<String, ActiveChild>,
    subscribers: Vec<mpsc::SyncSender<SubagentBridgeEvent>>,
}

impl BridgeState {
    fn broadcast(&mut self, event: SubagentBridgeEvent) {
        // A full subscriber is disconnected, so it must replay from the
        // durable child journal instead of silently missing an event.
        self.subscribers
            .retain(|subscriber| subscriber.try_send(event.clone()).is_ok());
    }
}

/// Shareable, process-local authority for active child approvals and events.
/// The session ID and request ID must both match the same live child run.
/// A reopened process has no inherited approval authority.
#[derive(Default)]
pub struct SubagentEventBridge {
    state: Mutex<BridgeState>,
}

impl SubagentEventBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe before starting children for a complete live stream. A late
    /// subscriber should load older events from SessionStore.
    pub fn subscribe(&self) -> mpsc::Receiver<SubagentBridgeEvent> {
        let (sender, receiver) = mpsc::sync_channel(BRIDGE_SUBSCRIBER_CAPACITY);
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .subscribers
            .push(sender);
        receiver
    }

    /// Current in-process children. A new subscriber can replay their
    /// journals after it missed events or its previous channel filled.
    pub fn active_child_ids(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .active
            .keys()
            .cloned()
            .collect()
    }

    /// Atomically snapshot the live children and requests that still accept
    /// a decision. This lets a replacement UI subscriber rebuild from journal.
    pub fn live_snapshot(&self) -> Vec<(String, Vec<String>)> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .active
            .iter()
            .map(|(id, active)| {
                let requests = if active.cancelling {
                    Vec::new()
                } else {
                    active.request_ids.iter().cloned().collect()
                };
                (id.clone(), requests)
            })
            .collect()
    }

    /// Consume exactly one live request. A wrong child, duplicate response,
    /// cancelled run, or request from an earlier process returns false.
    pub fn resolve_permission(
        &self,
        child_session_id: &str,
        request_id: &str,
        choice: PermissionChoice,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(active) = state.active.get_mut(child_session_id) else {
            return false;
        };
        if active.cancelling || !active.request_ids.remove(request_id) {
            return false;
        }
        active.permissions.resolve(request_id, choice)
    }

    /// Cancel one live child. Approvals close immediately; its worker remains
    /// registered until it reports the durable terminal event or exits.
    pub fn cancel_child(&self, child_session_id: &str) -> bool {
        let cancellation = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let Some(active) = state.active.get_mut(child_session_id) else {
                return false;
            };
            if active.cancelling {
                return false;
            }
            active.cancelling = true;
            active.request_ids.clear();
            active.cancellation.clone()
        };
        cancellation.cancel();
        true
    }

    fn register(
        self: &Arc<Self>,
        child_session_id: &str,
        handle: &crate::AgentRunHandle,
    ) -> Result<ChildBridgeRegistration, SubagentWorkerError> {
        self.register_capability(
            child_session_id,
            handle.permission_capability(),
            handle.cancellation_token(),
        )
    }

    fn register_capability(
        self: &Arc<Self>,
        child_session_id: &str,
        permissions: Arc<PendingPermissions>,
        cancellation: CancellationToken,
    ) -> Result<ChildBridgeRegistration, SubagentWorkerError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.active.contains_key(child_session_id) {
            return Err(SubagentWorkerError::AlreadyRunning(
                child_session_id.to_owned(),
            ));
        }
        let generation = Uuid::new_v4();
        state.active.insert(
            child_session_id.to_owned(),
            ActiveChild {
                generation,
                permissions,
                cancellation,
                request_ids: HashSet::new(),
                cancelling: false,
            },
        );
        Ok(ChildBridgeRegistration {
            bridge: self.clone(),
            child_session_id: child_session_id.to_owned(),
            generation,
        })
    }

    fn forward(&self, child_session_id: &str, generation: Uuid, event: AgentEvent) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(active) = state.active.get_mut(child_session_id) else {
            return;
        };
        if active.generation != generation || event.session_id != child_session_id {
            return;
        }
        match &event.kind {
            AgentEventKind::PermissionRequested { request_id, .. } if !active.cancelling => {
                active.request_ids.insert(request_id.clone());
            }
            AgentEventKind::PermissionResolved { request_id, .. } => {
                active.request_ids.remove(request_id);
            }
            kind if is_terminal(kind) => {
                active.request_ids.clear();
            }
            _ => {}
        }
        state.broadcast(SubagentBridgeEvent::Event {
            child_session_id: child_session_id.to_owned(),
            event: Box::new(event),
        });
    }

    fn close(&self, child_session_id: &str, generation: Uuid) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state
            .active
            .get(child_session_id)
            .is_some_and(|active| active.generation == generation)
        {
            state.active.remove(child_session_id);
            state.broadcast(SubagentBridgeEvent::Closed {
                child_session_id: child_session_id.to_owned(),
            });
        }
    }
}

struct ChildBridgeRegistration {
    bridge: Arc<SubagentEventBridge>,
    child_session_id: String,
    generation: Uuid,
}

impl ChildBridgeRegistration {
    fn forward(&self, event: AgentEvent) {
        self.bridge
            .forward(&self.child_session_id, self.generation, event);
    }
}

impl Drop for ChildBridgeRegistration {
    fn drop(&mut self) {
        self.bridge.close(&self.child_session_id, self.generation);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentTurnResult {
    pub child_session_id: String,
    pub status: SubagentStatus,
    pub response: Option<String>,
    pub error: Option<String>,
}

/// Human decision required before an interrupted child can continue. These
/// call IDs are copied from the child's validated event journal.
#[derive(Clone, Debug, PartialEq)]
pub enum SubagentRecoveryPause {
    NeedsPermission {
        request_id: String,
        tool_call: ToolCall,
    },
    ToolOutcomeUncertain {
        tool_call: ToolCall,
        executor_error: Option<String>,
    },
    AbortedToolBatch {
        next_tool_call: ToolCall,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum SubagentRecoveryState {
    Terminal(SubagentTurnResult),
    Paused {
        child_session_id: String,
        reason: SubagentRecoveryPause,
    },
}

#[derive(Debug, Error)]
pub enum SubagentWorkerError {
    #[error("subagent coordination: {0}")]
    Coordination(#[from] SubagentError),
    #[error("subagent session store: {0}")]
    Store(#[from] StoreError),
    #[error("subagent catalog: {0}")]
    Catalog(#[from] ToolCatalogError),
    #[error("subagent model context: {0}")]
    Context(#[from] AgentError),
    #[error("subagent hook policy: {0}")]
    Hooks(#[from] HookValidationError),
    #[error("subagent recovery: {0}")]
    Recovery(#[from] ResumeError),
    #[error("subagent checkpoint: {0}")]
    Checkpoint(#[from] CheckpointError),
    #[error("subagent journal replay: {0}")]
    Replay(#[from] ReplayError),
    #[error("subagent model has no stable request profile")]
    UnknownModelProfile,
    #[error("subagent {0} is already running")]
    AlreadyRunning(String),
    #[error("subagent {0} requires explicit recovery")]
    NeedsRecovery(String),
    #[error("subagent {0} has no matching admitted child turn")]
    InvalidChildAdmission(String),
    #[error("subagent {0} has a checkpoint but no matching compaction configuration")]
    MissingCheckpointConfiguration(String),
    #[error("subagent {0} compaction configuration differs from its provider or model")]
    InvalidCheckpointConfiguration(String),
    #[error("subagent recovery was cancelled")]
    Cancelled,
    #[error("subagent recovery returned an unexpected nonterminal state")]
    UnexpectedRecoveryState,
    #[error("subagent {0} worker exited without a committed terminal event")]
    MissingTerminal(String),
    #[error("subagent {0} terminal event was not durable")]
    UncommittedTerminal(String),
    #[error("subagent {0} still has an unresolved turn or tool effect")]
    UnresolvedChild(String),
    #[error("subagent session approvals require a configured command isolation")]
    SessionApprovalsUnavailable,
    #[error("subagent worker join failed: {0}")]
    Join(String),
}

/// Bounded native child runner. The model and local executor are shared with
/// the parent host, while every child has its own journaled session and lease.
#[derive(Clone)]
pub struct SubagentWorker {
    coordinator: Arc<SubagentCoordinator>,
    sessions: Arc<SessionStore>,
    model: Arc<dyn ModelBackend>,
    tools: Arc<dyn ToolExecutor>,
    provider: String,
    endpoint: String,
    instructions: String,
    permission_policy: PermissionPolicy,
    limits: RunLimits,
    token_budget: Option<TokenBudgetPolicy>,
    auto_compaction: Option<(Arc<CheckpointFileStore>, CheckpointCompatibility)>,
    command_hooks: Arc<CommandHooks>,
    session_approval_isolation: Option<CommandIsolation>,
    event_bridge: Option<Arc<SubagentEventBridge>>,
    nested_subagents: Option<SubagentToolLimits>,
}

impl SubagentWorker {
    pub fn new(
        coordinator: Arc<SubagentCoordinator>,
        sessions: Arc<SessionStore>,
        model: Arc<dyn ModelBackend>,
        tools: Arc<dyn ToolExecutor>,
        provider: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            coordinator,
            sessions,
            model,
            tools,
            provider: provider.into(),
            endpoint: endpoint.into(),
            instructions: String::new(),
            permission_policy: PermissionPolicy::new(),
            limits: RunLimits::default(),
            token_budget: None,
            auto_compaction: None,
            command_hooks: Arc::new(CommandHooks::default()),
            session_approval_isolation: None,
            event_bridge: None,
            nested_subagents: None,
        }
    }

    pub fn with_system_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = instructions.into();
        self
    }

    pub fn with_permission_policy(mut self, policy: PermissionPolicy) -> Self {
        self.permission_policy = policy;
        self
    }

    pub fn with_limits(mut self, limits: RunLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_token_budget_policy(mut self, policy: TokenBudgetPolicy) -> Self {
        self.token_budget = Some(policy);
        self
    }

    pub fn with_command_hooks(mut self, hooks: Arc<CommandHooks>) -> Self {
        self.command_hooks = hooks;
        self
    }

    /// Enable exact approval reuse in child sessions using this worker's
    /// journal store and the host's active command isolation.
    pub fn with_session_approvals(mut self, isolation: CommandIsolation) -> Self {
        self.session_approval_isolation = Some(isolation);
        self
    }

    pub fn with_event_bridge(mut self, bridge: Arc<SubagentEventBridge>) -> Self {
        self.event_bridge = Some(bridge);
        self
    }

    /// Permit a Build child to delegate through a fresh wrapper bound to its
    /// own native session lease. The base executor, permissions, model limits,
    /// and token budget remain host selected at every depth.
    pub fn with_nested_subagents(mut self, limits: SubagentToolLimits) -> Self {
        self.nested_subagents = Some(limits);
        self
    }

    pub fn with_auto_compaction(
        mut self,
        checkpoints: Arc<CheckpointFileStore>,
        compatibility: CheckpointCompatibility,
    ) -> Self {
        self.auto_compaction = Some((checkpoints, compatibility));
        self
    }

    /// Reserve the deterministic child ID before launching its worker. The
    /// subsequent `run` verifies the same model context and reuses this child.
    pub fn prepare_child(
        &self,
        parent_lease: &NativeSessionLease,
        spawn_id: &str,
        prompt: &str,
        mode: TurnMode,
    ) -> Result<SubagentRecord, SubagentWorkerError> {
        let parent = self.sessions.native_session_for_lease(parent_lease)?;
        let depth = parent
            .subagent
            .as_ref()
            .map_or(Some(1), |link| link.depth.checked_add(1))
            .ok_or(SubagentError::DepthLimit)?;
        let spec = self.spec_for(&parent.workspace, prompt, mode, depth)?;
        Ok(self
            .coordinator
            .create_child(parent_lease, spawn_id, &spec)?)
    }

    fn spec_for(
        &self,
        workspace: &Path,
        prompt: &str,
        mode: TurnMode,
        depth: u8,
    ) -> Result<SubagentSpec, SubagentWorkerError> {
        let catalog = self.catalog_for_mode(mode, depth)?;
        let profile = self
            .model
            .request_profile_fingerprint()
            .ok_or(SubagentWorkerError::UnknownModelProfile)?;
        let model_context_digest = native_model_context_digest(
            &self.provider,
            &self.endpoint,
            self.model.model_id(),
            mode,
            workspace,
            &catalog,
            &self.instructions,
            profile,
        )?;
        let hooks_digest = self.command_hooks.digest_hex()?;
        let mut identity = Sha256::new();
        identity.update(b"openide-subagent-runtime-v3\0");
        identity.update(model_context_digest.as_bytes());
        identity.update(hooks_digest.as_bytes());
        identity.update(self.permission_policy.digest_hex().as_bytes());
        for value in [
            self.limits.max_model_steps,
            self.limits.max_tool_calls,
            self.limits.max_tool_output_chars,
        ] {
            identity.update((value as u64).to_be_bytes());
        }
        let coordinator_limits = self.coordinator.limits();
        identity.update([coordinator_limits.max_depth]);
        for value in [
            coordinator_limits.max_children_per_parent,
            coordinator_limits.max_concurrent_children,
            coordinator_limits.max_total_descendants,
            self.nested_subagents.map_or(0, |limits| limits.max_tasks),
        ] {
            identity.update((value as u64).to_be_bytes());
        }
        if let Some(budget) = self.token_budget {
            identity.update([1]);
            identity.update(budget.context_window_tokens.to_be_bytes());
            identity.update(budget.output_reserve_tokens.to_be_bytes());
            identity.update(budget.tool_schema_reserve_tokens.to_be_bytes());
            identity.update(budget.auto_compact_token_limit.unwrap_or(0).to_be_bytes());
        } else {
            identity.update([0]);
        }
        let context_digest = format!("{:x}", identity.finalize());
        Ok(SubagentSpec {
            prompt: prompt.to_owned(),
            model: self.model.model_id().to_owned(),
            mode,
            context_digest,
        })
    }

    fn child_depth(&self, child_id: &str) -> Result<u8, SubagentWorkerError> {
        let stored = self
            .sessions
            .load(child_id)?
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.to_owned()))?;
        stored
            .session
            .subagent
            .as_ref()
            .map(|link| link.depth)
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.to_owned()))
    }

    fn legacy_v2_spec_for(
        &self,
        workspace: &Path,
        prompt: &str,
        mode: TurnMode,
        depth: u8,
    ) -> Result<SubagentSpec, SubagentWorkerError> {
        let catalog = self.catalog_for_mode(mode, depth)?;
        let profile = self
            .model
            .request_profile_fingerprint()
            .ok_or(SubagentWorkerError::UnknownModelProfile)?;
        let model_context_digest = native_model_context_digest(
            &self.provider,
            &self.endpoint,
            self.model.model_id(),
            mode,
            workspace,
            &catalog,
            &self.instructions,
            profile,
        )?;
        let mut identity = Sha256::new();
        identity.update(b"openide-subagent-runtime-v2\0");
        identity.update(model_context_digest.as_bytes());
        identity.update(self.command_hooks.digest_hex()?.as_bytes());
        Ok(SubagentSpec {
            prompt: prompt.to_owned(),
            model: self.model.model_id().to_owned(),
            mode,
            context_digest: format!("{:x}", identity.finalize()),
        })
    }

    /// Select the exact tool surface recorded when a child was reserved.
    /// Legacy reservations can recover with the former base catalog, but
    /// never acquire recursive tools merely because the host later enabled
    /// them. A completed legacy child needs a matching journaled permission
    /// policy before a fresh follow-up turn may be dispatched.
    fn worker_for_existing_child(
        &self,
        child_id: &str,
        prompt: &str,
        mode: TurnMode,
        require_permission_marker: bool,
    ) -> Result<(Self, String), SubagentWorkerError> {
        let stored = self
            .sessions
            .load(child_id)?
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.to_owned()))?;
        let link = stored
            .session
            .subagent
            .as_ref()
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.to_owned()))?;
        if require_permission_marker {
            let marker = stored.events.iter().find_map(|event| match &event.kind {
                AgentEventKind::ExecPolicyCommitted { digest, .. } => Some(digest.as_str()),
                _ => None,
            });
            if marker != Some(self.permission_policy.digest_hex().as_str()) {
                return Err(SubagentWorkerError::Context(AgentError::InvalidRequest(
                    "child permission policy cannot be verified for follow-up".to_owned(),
                )));
            }
        }
        let candidates = [
            self.clone(),
            Self {
                nested_subagents: None,
                ..self.clone()
            },
        ];
        for candidate in candidates {
            let digest = candidate
                .spec_for(&stored.session.workspace, prompt, mode, link.depth)?
                .digest_hex()?;
            if digest == link.request_digest {
                return Ok((candidate, digest));
            }
            if candidate.nested_subagents.is_none() {
                let digest = candidate
                    .legacy_v2_spec_for(&stored.session.workspace, prompt, mode, link.depth)?
                    .digest_hex()?;
                if digest == link.request_digest {
                    let marker = stored.events.iter().find_map(|event| match &event.kind {
                        AgentEventKind::ExecPolicyCommitted { digest, .. } => Some(digest.as_str()),
                        _ => None,
                    });
                    if marker != Some(self.permission_policy.digest_hex().as_str()) {
                        return Err(SubagentWorkerError::Context(AgentError::InvalidRequest(
                            "legacy child permission policy cannot be verified".to_owned(),
                        )));
                    }
                    return Ok((candidate, digest));
                }
            }
        }
        Err(SubagentError::Store(StoreError::ConflictingSubagentIdentity).into())
    }

    /// Read-only preflight before committing a follow-up input. A Ready child
    /// has no first admission yet; its original worker or explicit recovery
    /// must establish that identity before the queued input can run.
    pub fn validate_follow_up_child(&self, child_id: &str) -> Result<(), SubagentWorkerError> {
        let stored = self
            .sessions
            .load(child_id)?
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.to_owned()))?;
        if let Some((prompt, mode)) = stored.events.iter().find_map(|event| match &event.kind {
            AgentEventKind::TurnAdmitted { prompt, mode, .. } => Some((prompt.as_str(), *mode)),
            _ => None,
        }) {
            self.worker_for_existing_child(child_id, prompt, mode, true)?;
        }
        Ok(())
    }

    fn catalog_for_mode(
        &self,
        mode: TurnMode,
        depth: u8,
    ) -> Result<ToolCatalog, SubagentWorkerError> {
        if self.nested_subagents.is_some_and(|limits| !limits.valid()) {
            return Err(SubagentWorkerError::Context(AgentError::InvalidRequest(
                "invalid nested subagent tool limits".to_owned(),
            )));
        }
        let mut definitions = self.tools.definitions();
        if mode == TurnMode::Build
            && depth < self.coordinator.limits().max_depth
            && self.nested_subagents.is_some()
        {
            definitions.extend(coordination_definitions());
        }
        let definitions = definitions.into_iter().filter(|definition| {
            mode == TurnMode::Build
                || (trusted_local_read(&definition.name)
                    && self.tools.certifies_parallel_read(&definition.name)
                    && !self.command_hooks.has_hooks_for_tool(&definition.name))
        });
        Ok(ToolCatalog::new(definitions)?)
    }

    fn tools_for_claimed_turn(
        &self,
        run_lease: &SubagentRunLease,
        mode: TurnMode,
    ) -> Result<Arc<dyn ToolExecutor>, SubagentWorkerError> {
        let session = self
            .sessions
            .native_session_for_lease(run_lease.native_lease())?;
        let depth = session
            .subagent
            .as_ref()
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(session.id.clone()))?
            .depth;
        if mode != TurnMode::Build || depth >= self.coordinator.limits().max_depth {
            return Ok(self.tools.clone());
        }
        let Some(limits) = self.nested_subagents else {
            return Ok(self.tools.clone());
        };
        let nested = SubagentToolExecutor::new(
            self.tools.clone(),
            self.clone(),
            self.coordinator.clone(),
            self.sessions.clone(),
            run_lease.shared_native_lease(),
            limits,
        )?;
        Ok(Arc::new(nested))
    }

    fn recovery_engine_for_claimed_turn(
        &self,
        run_lease: &SubagentRunLease,
        mode: TurnMode,
        depth: u8,
    ) -> Result<ResumeEngine, SubagentWorkerError> {
        let child_id = run_lease.session_id();
        let record = self
            .sessions
            .load(child_id)?
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.to_owned()))?;
        let (checkpoints, compatibility) =
            match &self.auto_compaction {
                Some((store, compatibility))
                    if compatibility.provider == self.provider
                        && compatibility.model == self.model.model_id() =>
                {
                    (store.clone(), compatibility.clone())
                }
                Some(_) => {
                    return Err(SubagentWorkerError::InvalidCheckpointConfiguration(
                        child_id.to_owned(),
                    ))
                }
                None => {
                    if record.events.iter().any(|event| {
                        matches!(event.kind, AgentEventKind::CheckpointCommitted { .. })
                    }) {
                        return Err(SubagentWorkerError::MissingCheckpointConfiguration(
                            child_id.to_owned(),
                        ));
                    }
                    (
                        Arc::new(CheckpointFileStore::new(self.sessions.storage_root())?),
                        CheckpointCompatibility {
                            provider: self.provider.clone(),
                            model: self.model.model_id().to_owned(),
                            context_format_key: "native-child-v1".into(),
                        },
                    )
                }
            };
        let tools = self.tools_for_claimed_turn(run_lease, mode)?;
        let mut engine = ResumeEngine::new(
            self.sessions.clone(),
            checkpoints,
            compatibility,
            self.model.clone(),
            tools,
            self.catalog_for_mode(mode, depth)?,
            self.permission_policy.clone(),
            self.token_budget,
        )
        .with_limits(self.limits)
        .with_system_instructions(self.instructions.clone())
        .with_native_runtime_identity(self.provider.clone(), self.endpoint.clone())
        .with_command_hooks(self.command_hooks.clone());
        if let Some(isolation) = self.session_approval_isolation {
            engine = engine.with_session_approvals(self.sessions.clone(), isolation);
        }
        Ok(engine)
    }

    /// Runs a child to a terminal journal event. Dropping this future requests
    /// cancellation; a started effect still follows normal uncertain recovery.
    pub async fn run(
        &self,
        parent_lease: Arc<NativeSessionLease>,
        spawn_id: String,
        prompt: String,
        mode: TurnMode,
        cancellation: CancellationToken,
    ) -> Result<SubagentTurnResult, SubagentWorkerError> {
        let worker_cancel = cancellation.child_token();
        let cancel_on_drop = CancelOnDrop(worker_cancel.clone());
        let worker = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            worker.run_blocking(parent_lease, &spawn_id, &prompt, mode, &worker_cancel)
        })
        .await
        .map_err(|error| SubagentWorkerError::Join(error.to_string()))?;
        drop(cancel_on_drop);
        result
    }

    /// Run a child already created while the parent held its native lease.
    /// The opaque control does not keep the parent's lease alive, allowing
    /// the parent to start another turn while the child works independently.
    pub async fn run_prepared(
        &self,
        control: SubagentControl,
        child_id: String,
        spawn_id: String,
        prompt: String,
        mode: TurnMode,
        cancellation: CancellationToken,
    ) -> Result<SubagentTurnResult, SubagentWorkerError> {
        let worker_cancel = cancellation.child_token();
        let cancel_on_drop = CancelOnDrop(worker_cancel.clone());
        let worker = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            worker.run_prepared_blocking(
                &control,
                &child_id,
                &spawn_id,
                &prompt,
                mode,
                &worker_cancel,
            )
        })
        .await
        .map_err(|error| SubagentWorkerError::Join(error.to_string()))?;
        drop(cancel_on_drop);
        result
    }

    /// Explicitly recover one interrupted child while the caller still owns
    /// the parent lease. No second child turn is admitted.
    #[allow(clippy::too_many_arguments)]
    pub async fn resume(
        &self,
        parent_lease: Arc<NativeSessionLease>,
        child_id: String,
        spawn_id: String,
        prompt: String,
        mode: TurnMode,
        cancellation: CancellationToken,
    ) -> Result<SubagentRecoveryState, SubagentWorkerError> {
        let control = self.coordinator.issue_control(&parent_lease)?;
        drop(parent_lease);
        self.resume_prepared(control, child_id, spawn_id, prompt, mode, cancellation)
            .await
    }

    /// Persist an exact session approval for an interrupted child's pending
    /// request, then release the child claim. The caller can resume the child
    /// normally afterward. The decision and grant sync before the resolution
    /// event, so repeating this after a crash is idempotent.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_recovery_session_approval(
        &self,
        parent_lease: Arc<NativeSessionLease>,
        child_id: &str,
        spawn_id: &str,
        prompt: &str,
        mode: TurnMode,
        request_id: &str,
    ) -> Result<(), SubagentWorkerError> {
        if self.session_approval_isolation.is_none() {
            return Err(SubagentWorkerError::SessionApprovalsUnavailable);
        }
        let control = self.coordinator.issue_control(&parent_lease)?;
        drop(parent_lease);
        let (worker, request_digest) =
            self.worker_for_existing_child(child_id, prompt, mode, false)?;
        let child =
            self.coordinator
                .prepared_child(&control, child_id, spawn_id, &request_digest)?;
        match child.status {
            SubagentStatus::Interrupted => {}
            SubagentStatus::Running => {
                return Err(SubagentWorkerError::AlreadyRunning(child_id.to_owned()))
            }
            _ => return Err(SubagentWorkerError::NeedsRecovery(child_id.to_owned())),
        }
        let depth = child
            .session
            .subagent
            .as_ref()
            .expect("prepared child has lineage")
            .depth;
        let run = worker
            .coordinator
            .resume_child_control(&control, child_id)?;
        let result = (|| {
            let engine = worker.recovery_engine_for_claimed_turn(&run, mode, depth)?;
            engine.resolve_permission(
                run.native_lease(),
                request_id,
                PermissionChoice::AllowSession,
            )?;
            Ok(())
        })();
        worker.coordinator.pause_child(&run)?;
        result
    }

    /// Recover a child created by this exact parent/spawn request. The
    /// coordinator claims its OS lease from `Interrupted` only; the journal
    /// and model/tool identities are independently checked by ResumeEngine.
    #[allow(clippy::too_many_arguments)]
    pub async fn resume_prepared(
        &self,
        control: SubagentControl,
        child_id: String,
        spawn_id: String,
        prompt: String,
        mode: TurnMode,
        cancellation: CancellationToken,
    ) -> Result<SubagentRecoveryState, SubagentWorkerError> {
        let (worker, request_digest) =
            self.worker_for_existing_child(&child_id, &prompt, mode, false)?;
        worker
            .resume_prepared_checked(
                control,
                child_id,
                spawn_id,
                prompt,
                mode,
                request_digest,
                cancellation,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn resume_prepared_checked(
        &self,
        control: SubagentControl,
        child_id: String,
        spawn_id: String,
        prompt: String,
        mode: TurnMode,
        request_digest: String,
        cancellation: CancellationToken,
    ) -> Result<SubagentRecoveryState, SubagentWorkerError> {
        let worker_cancel = cancellation.child_token();
        let _cancel_on_drop = CancelOnDrop(worker_cancel.clone());
        let child =
            self.coordinator
                .prepared_child(&control, &child_id, &spawn_id, &request_digest)?;
        let depth = child
            .session
            .subagent
            .as_ref()
            .expect("prepared direct child has lineage")
            .depth;
        let legacy_ready = child.status == SubagentStatus::Ready;
        match child.status {
            SubagentStatus::Completed | SubagentStatus::Failed => {
                return Ok(SubagentRecoveryState::Terminal(
                    self.durable_terminal(&child_id)?,
                ));
            }
            SubagentStatus::Cancelled => {
                return Ok(SubagentRecoveryState::Terminal(
                    self.cancelled_or_durable(&child_id)?,
                ));
            }
            SubagentStatus::Running => return Err(SubagentWorkerError::AlreadyRunning(child_id)),
            SubagentStatus::Ready => {}
            SubagentStatus::Interrupted => {}
        }
        let record = self
            .sessions
            .load(&child_id)?
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.clone()))?;
        let admitted = record
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::TurnAdmitted {
                    turn_id,
                    prompt,
                    model,
                    mode,
                } => Some((turn_id, prompt, model, mode)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let (_, active_input, _) = self.coordinator.input_state(&control, &child_id)?;
        let inputs = self.coordinator.inputs_control(&control, &child_id)?;
        if admitted.is_empty() {
            if legacy_ready {
                return Err(SubagentWorkerError::NeedsRecovery(child_id));
            }
            if active_input.is_some() || !record.events.is_empty() {
                return Err(SubagentWorkerError::InvalidChildAdmission(child_id));
            }
            let run = self.coordinator.resume_child_control(&control, &child_id)?;
            let worker = self.clone();
            let restored_control = control.clone();
            let restored_child_id = child_id.clone();
            let restored_prompt = prompt.clone();
            let restored_cancel = worker_cancel.clone();
            let terminal = tokio::task::spawn_blocking(move || {
                let result = (|| {
                    let admission = worker.sessions.admit_native_turn(
                        &restored_child_id,
                        &spawn_id,
                        &restored_prompt,
                        worker.model.model_id(),
                        mode,
                    )?;
                    worker.execute_claimed_turn(
                        &restored_control,
                        &run,
                        &restored_child_id,
                        restored_control.workspace(),
                        admission.sequence,
                        &restored_prompt,
                        mode,
                        Vec::new(),
                        &restored_cancel,
                    )
                })();
                drop(run);
                result
            })
            .await
            .map_err(|error| SubagentWorkerError::Join(error.to_string()))??;
            if terminal.status == SubagentStatus::Completed {
                self.drain_queued_prepared(control, child_id, worker_cancel)
                    .await?;
            }
            return Ok(SubagentRecoveryState::Terminal(terminal));
        }
        if admitted[0].0 != &spawn_id
            || admitted[0].1 != &prompt
            || admitted[0].2 != self.model.model_id()
            || admitted[0].3 != &mode
        {
            return Err(SubagentWorkerError::InvalidChildAdmission(child_id));
        }
        let active_admitted = active_input
            .as_ref()
            .is_some_and(|input| admitted.last().is_some_and(|last| last.0 == &input.id));
        let expected_inputs = inputs
            .iter()
            .filter(|input| {
                input.state == QueuedInputState::Done
                    || (input.state == QueuedInputState::Active && active_admitted)
            })
            .collect::<Vec<_>>();
        if admitted.len() != expected_inputs.len() + 1
            || !admitted
                .iter()
                .skip(1)
                .zip(expected_inputs.iter())
                .all(|(actual, input)| {
                    actual.0 == &input.id
                        && actual.1 == &input.prompt
                        && actual.2 == self.model.model_id()
                        && actual.3 == &input.mode
                })
        {
            return Err(SubagentWorkerError::InvalidChildAdmission(child_id));
        }
        if let Some(input) = &active_input {
            if !active_admitted {
                let run = self.coordinator.resume_child_control(&control, &child_id)?;
                let worker = self.clone();
                let recovered_control = control.clone();
                let recovered_cancel = worker_cancel.clone();
                let input = input.clone();
                let terminal = tokio::task::spawn_blocking(move || {
                    let result = worker.run_claimed_input(
                        &recovered_control,
                        &run,
                        &input,
                        &recovered_cancel,
                    );
                    drop(run);
                    result
                })
                .await
                .map_err(|error| SubagentWorkerError::Join(error.to_string()))??;
                if terminal.status == SubagentStatus::Completed {
                    self.drain_queued_prepared(control, child_id, worker_cancel)
                        .await?;
                }
                return Ok(SubagentRecoveryState::Terminal(terminal));
            }
        }
        if worker_cancel.is_cancelled() {
            return Err(SubagentWorkerError::Cancelled);
        }
        let run_lease = if legacy_ready {
            self.coordinator.start_child_control(&control, &child_id)?
        } else {
            self.coordinator.resume_child_control(&control, &child_id)?
        };
        let engine = match self.recovery_engine_for_claimed_turn(&run_lease, mode, depth) {
            Ok(engine) => engine,
            Err(error) => {
                self.coordinator.pause_child(&run_lease)?;
                return Err(error);
            }
        };
        let recovery = async {
            let mut step = engine
                .run_until_pause(run_lease.native_lease(), worker_cancel.clone())
                .await?;
            for _ in 0..self.limits.max_tool_calls.saturating_add(1) {
                let ResumeDisposition::NeedsPermission { request_id, .. } = &step.disposition
                else {
                    return Ok::<_, SubagentWorkerError>(step);
                };
                let recorded = self
                    .sessions
                    .load_permission_decisions(&child_id)?
                    .into_iter()
                    .find(|decision| decision.request_id == *request_id);
                let Some(recorded) = recorded else {
                    return Ok(step);
                };
                let choice = match recorded.decision {
                    PermissionDecision::Allow if recorded.grant.is_some() => {
                        if self.session_approval_isolation.is_none() {
                            return Err(SubagentWorkerError::UnexpectedRecoveryState);
                        }
                        PermissionChoice::AllowSession
                    }
                    PermissionDecision::Allow => PermissionChoice::Allow,
                    PermissionDecision::Deny => PermissionChoice::Deny,
                };
                engine.resolve_permission(run_lease.native_lease(), request_id, choice)?;
                step = engine
                    .run_until_pause(run_lease.native_lease(), worker_cancel.clone())
                    .await?;
            }
            Err(SubagentWorkerError::UnexpectedRecoveryState)
        };
        let step = match recovery.await {
            Ok(step) => step,
            Err(error) => {
                // A model retry may be interrupted without any external effect;
                // a started tool stays uncertain in replay. Both remain explicit.
                self.coordinator.pause_child(&run_lease)?;
                return Err(error);
            }
        };
        let reason = match step.disposition {
            ResumeDisposition::NeedsPermission {
                request_id,
                tool_call,
            } => Some(SubagentRecoveryPause::NeedsPermission {
                request_id,
                tool_call,
            }),
            ResumeDisposition::ToolOutcomeUncertain {
                tool_call,
                executor_error,
            } => Some(SubagentRecoveryPause::ToolOutcomeUncertain {
                tool_call,
                executor_error,
            }),
            ResumeDisposition::AbortedToolBatch { next_tool_call } => {
                Some(SubagentRecoveryPause::AbortedToolBatch { next_tool_call })
            }
            ResumeDisposition::Complete | ResumeDisposition::AbortedTurn => None,
            ResumeDisposition::Advanced => {
                self.coordinator.pause_child(&run_lease)?;
                return Err(SubagentWorkerError::UnexpectedRecoveryState);
            }
        };
        if let Some(reason) = reason {
            self.coordinator.pause_child(&run_lease)?;
            return Ok(SubagentRecoveryState::Paused {
                child_session_id: child_id,
                reason,
            });
        }
        let terminal = match self.durable_terminal(&child_id) {
            Ok(terminal) => terminal,
            Err(error) => {
                self.coordinator.pause_child(&run_lease)?;
                return Err(error);
            }
        };
        let outcome = match terminal.status {
            SubagentStatus::Completed => SubagentOutcome::Completed,
            SubagentStatus::Failed => SubagentOutcome::Failed,
            SubagentStatus::Cancelled => SubagentOutcome::Cancelled,
            _ => unreachable!("durable_terminal returns only terminal states"),
        };
        self.coordinator.finish_child(&run_lease, outcome)?;
        drop(engine);
        drop(run_lease);
        if terminal.status == SubagentStatus::Completed {
            self.drain_queued_prepared(control, child_id, worker_cancel)
                .await?;
        }
        Ok(SubagentRecoveryState::Terminal(terminal))
    }

    fn run_blocking(
        &self,
        parent_lease: Arc<NativeSessionLease>,
        spawn_id: &str,
        prompt: &str,
        mode: TurnMode,
        cancellation: &CancellationToken,
    ) -> Result<SubagentTurnResult, SubagentWorkerError> {
        let child = self.prepare_child(&parent_lease, spawn_id, prompt, mode)?;
        let control = self.coordinator.issue_control(&parent_lease)?;
        drop(parent_lease);
        self.run_prepared_blocking(
            &control,
            &child.session.id,
            spawn_id,
            prompt,
            mode,
            cancellation,
        )
    }

    fn run_prepared_blocking(
        &self,
        control: &SubagentControl,
        child_id: &str,
        spawn_id: &str,
        prompt: &str,
        mode: TurnMode,
        cancellation: &CancellationToken,
    ) -> Result<SubagentTurnResult, SubagentWorkerError> {
        let depth = self.child_depth(child_id)?;
        let spec = self.spec_for(control.workspace(), prompt, mode, depth)?;
        let child =
            self.coordinator
                .prepared_child(control, child_id, spawn_id, &spec.digest_hex()?)?;
        let child_id = child.session.id;
        match child.status {
            SubagentStatus::Completed | SubagentStatus::Failed => {
                return self.durable_terminal(&child_id);
            }
            SubagentStatus::Cancelled => {
                return self.cancelled_or_durable(&child_id);
            }
            SubagentStatus::Running => {
                return Err(SubagentWorkerError::AlreadyRunning(child_id));
            }
            SubagentStatus::Interrupted => {
                return Err(SubagentWorkerError::NeedsRecovery(child_id));
            }
            SubagentStatus::Ready => {}
        }
        if cancellation.is_cancelled() {
            self.coordinator
                .request_cancel_control(control, &child_id)?;
            return self.cancelled_or_durable(&child_id);
        }
        let run_lease = self.coordinator.start_child_control(control, &child_id)?;
        let admission = self.sessions.admit_native_turn(
            &child_id,
            spawn_id,
            prompt,
            self.model.model_id(),
            mode,
        )?;
        let result = self.execute_claimed_turn(
            control,
            &run_lease,
            &child_id,
            &child.session.workspace,
            admission.sequence,
            prompt,
            mode,
            Vec::new(),
            cancellation,
        )?;
        drop(run_lease);
        if result.status == SubagentStatus::Completed {
            self.drain_queued_prepared_blocking(control, &child_id, cancellation)?;
        }
        Ok(result)
    }

    /// Start pending follow-up turns after the first child turn, or after a
    /// parent enqueues input into a child that was already idle. A crash leaves
    /// the active mailbox entry and child lease state for explicit recovery.
    pub async fn drain_queued_prepared(
        &self,
        control: SubagentControl,
        child_id: String,
        cancellation: CancellationToken,
    ) -> Result<(), SubagentWorkerError> {
        let worker_cancel = cancellation.child_token();
        let _cancel_on_drop = CancelOnDrop(worker_cancel.clone());
        let worker = self.clone();
        tokio::task::spawn_blocking(move || {
            let stored = worker
                .sessions
                .load(&child_id)?
                .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.clone()))?;
            let Some((prompt, mode)) = stored.events.iter().find_map(|event| match &event.kind {
                AgentEventKind::TurnAdmitted { prompt, mode, .. } => Some((prompt.as_str(), *mode)),
                _ => None,
            }) else {
                // The first worker or explicit recovery owns first admission.
                return Ok(());
            };
            let (selected, _) = worker.worker_for_existing_child(&child_id, prompt, mode, true)?;
            selected.drain_queued_prepared_blocking(&control, &child_id, &worker_cancel)
        })
        .await
        .map_err(|error| SubagentWorkerError::Join(error.to_string()))?
    }

    fn drain_queued_prepared_blocking(
        &self,
        control: &SubagentControl,
        child_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), SubagentWorkerError> {
        loop {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            let (run, input) = match self.coordinator.claim_next_input_control(control, child_id) {
                Ok(Some(claimed)) => claimed,
                Ok(None) => {
                    let (status, _, pending) = self.coordinator.input_state(control, child_id)?;
                    if pending == 0
                        || matches!(status, SubagentStatus::Failed | SubagentStatus::Cancelled)
                    {
                        return Ok(());
                    }
                    if status == SubagentStatus::Interrupted {
                        return Err(SubagentWorkerError::NeedsRecovery(child_id.to_owned()));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }
                Err(SubagentError::ConcurrencyLimit) => {
                    std::thread::sleep(POLL_INTERVAL);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let result = self.run_claimed_input(control, &run, &input, cancellation);
            drop(run);
            if result?.status != SubagentStatus::Completed {
                return Ok(());
            }
        }
    }

    fn run_claimed_input(
        &self,
        control: &SubagentControl,
        run: &SubagentRunLease,
        input: &QueuedSubagentInput,
        cancellation: &CancellationToken,
    ) -> Result<SubagentTurnResult, SubagentWorkerError> {
        let child_id = run.session_id();
        let stored = self
            .sessions
            .load(child_id)?
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.to_owned()))?;
        let checkpoints = CheckpointFileStore::new(self.sessions.storage_root())?;
        let expected = stored
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                AgentEventKind::CheckpointCommitted { reference } => {
                    Some(reference.compatibility.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| CheckpointCompatibility {
                provider: String::new(),
                model: String::new(),
                context_format_key: String::new(),
            });
        let replay = replay_native(child_id, &stored.events, Some(&checkpoints), &expected)?;
        if replay.recovery != ReplayRecovery::Idle || !replay.can_start_new_turn {
            self.coordinator.pause_child(run)?;
            return Err(SubagentWorkerError::UnresolvedChild(child_id.to_owned()));
        }
        let terminal = terminal_from_events(child_id, &stored.events)?;
        if terminal.status != SubagentStatus::Completed {
            self.coordinator.pause_child(run)?;
            return Err(SubagentWorkerError::UnresolvedChild(child_id.to_owned()));
        }
        if cancellation.is_cancelled() || self.coordinator.cancellation_requested(run)? {
            self.coordinator.request_cancel_control(control, child_id)?;
            self.coordinator.pause_child(run)?;
            return Err(SubagentWorkerError::Cancelled);
        }
        let admission = self.sessions.admit_native_turn(
            child_id,
            &input.id,
            &input.prompt,
            self.model.model_id(),
            input.mode,
        )?;
        self.execute_claimed_turn(
            control,
            run,
            child_id,
            &stored.session.workspace,
            admission.sequence,
            &input.prompt,
            input.mode,
            replay.history,
            cancellation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_claimed_turn(
        &self,
        control: &SubagentControl,
        run_lease: &SubagentRunLease,
        child_id: &str,
        workspace: &Path,
        admission_sequence: u64,
        prompt: &str,
        mode: TurnMode,
        history: Vec<AgentMessage>,
        cancellation: &CancellationToken,
    ) -> Result<SubagentTurnResult, SubagentWorkerError> {
        let tools = self.tools_for_claimed_turn(run_lease, mode)?;
        let mut runner = AgentRunner::new(self.model.clone(), tools)
            .with_command_hooks(self.command_hooks.clone())
            .with_permission_policy(self.permission_policy.clone())
            .with_system_instructions(self.instructions.clone())
            .with_native_runtime_identity(self.provider.clone(), self.endpoint.clone())
            .with_event_journal(self.sessions.clone())
            .with_native_steering(self.sessions.clone())
            .with_limits(self.limits);
        if let Some(isolation) = self.session_approval_isolation {
            runner = runner.with_session_approvals(self.sessions.clone(), isolation);
        }
        if let Some(budget) = self.token_budget {
            runner = runner.with_token_budget_policy(budget);
        }
        if let Some((checkpoints, compatibility)) = &self.auto_compaction {
            runner = runner.with_auto_compaction(
                self.sessions.clone(),
                checkpoints.clone(),
                compatibility.clone(),
            );
        }
        let handle = runner.start_with_shared_lease(
            AgentRunRequest {
                session_id: Some(child_id.to_owned()),
                sequence_start: admission_sequence,
                workspace: workspace.to_path_buf(),
                history,
                prompt: prompt.to_owned(),
                mode,
            },
            run_lease.shared_native_lease(),
        );
        let registration = match &self.event_bridge {
            Some(bridge) => match bridge.register(child_id, &handle) {
                Ok(registration) => Some(registration),
                Err(error) => {
                    handle.cancel();
                    return Err(error);
                }
            },
            None => None,
        };
        let mut terminal_seen = false;
        let mut terminal_at = None;
        loop {
            if cancellation.is_cancelled() || self.coordinator.cancellation_requested(run_lease)? {
                self.coordinator.request_cancel_control(control, child_id)?;
                handle.cancel();
            }
            match handle.events.recv_timeout(POLL_INTERVAL) {
                Ok(event) => {
                    if is_terminal(&event.kind) {
                        terminal_seen = true;
                        terminal_at = Some(std::time::Instant::now());
                    }
                    if let Some(registration) = &registration {
                        registration.forward(event);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if terminal_at.is_some_and(|at| at.elapsed() > TERMINAL_DRAIN_LIMIT) {
                        handle.cancel();
                        return Err(SubagentWorkerError::MissingTerminal(child_id.to_owned()));
                    }
                }
            }
        }
        if !terminal_seen {
            return Err(SubagentWorkerError::MissingTerminal(child_id.to_owned()));
        }
        let result = self.durable_terminal(child_id)?;
        let outcome = match result.status {
            SubagentStatus::Completed => SubagentOutcome::Completed,
            SubagentStatus::Failed => SubagentOutcome::Failed,
            SubagentStatus::Cancelled => SubagentOutcome::Cancelled,
            _ => unreachable!("durable_terminal returns only terminal states"),
        };
        self.coordinator.finish_child(run_lease, outcome)?;
        Ok(result)
    }

    fn durable_terminal(&self, child_id: &str) -> Result<SubagentTurnResult, SubagentWorkerError> {
        let record = self
            .sessions
            .load(child_id)?
            .ok_or_else(|| SubagentWorkerError::UncommittedTerminal(child_id.to_owned()))?;
        terminal_from_events(child_id, &record.events)
    }

    fn cancelled_or_durable(
        &self,
        child_id: &str,
    ) -> Result<SubagentTurnResult, SubagentWorkerError> {
        match self.durable_terminal(child_id) {
            Ok(result) => Ok(result),
            Err(SubagentWorkerError::UncommittedTerminal(_)) => Ok(SubagentTurnResult {
                child_session_id: child_id.to_owned(),
                status: SubagentStatus::Cancelled,
                response: None,
                error: None,
            }),
            Err(error) => Err(error),
        }
    }
}

fn terminal_from_events(
    child_id: &str,
    events: &[AgentEvent],
) -> Result<SubagentTurnResult, SubagentWorkerError> {
    let Some(index) = events.iter().rposition(|event| is_terminal(&event.kind)) else {
        return Err(SubagentWorkerError::UncommittedTerminal(
            child_id.to_owned(),
        ));
    };
    // A manual verification or discard is allowed after a failed/cancelled
    // turn. A new request after that terminal marker is not this child turn.
    if events[index + 1..].iter().any(|event| {
        !matches!(
            event.kind,
            AgentEventKind::ToolFinished { .. } | AgentEventKind::ToolBatchDiscarded { .. }
        )
    }) {
        return Err(SubagentWorkerError::UncommittedTerminal(
            child_id.to_owned(),
        ));
    }
    let (status, response, error) = match &events[index].kind {
        AgentEventKind::TurnCompleted { response, .. } => {
            (SubagentStatus::Completed, Some(response.clone()), None)
        }
        AgentEventKind::TurnFailed { error } => (SubagentStatus::Failed, None, Some(error.clone())),
        AgentEventKind::TurnCancelled => (SubagentStatus::Cancelled, None, None),
        _ => {
            return Err(SubagentWorkerError::UncommittedTerminal(
                child_id.to_owned(),
            ))
        }
    };
    Ok(SubagentTurnResult {
        child_session_id: child_id.to_owned(),
        status,
        response,
        error,
    })
}

/// Reconcile an explicitly resolved child without a provider, credentials,
/// current tool catalog, or changed user settings. The journal is replayed
/// again while the child lease is claimed, before changing registry status.
pub fn finalize_resolved_subagent(
    sessions: &Arc<SessionStore>,
    coordinator: &SubagentCoordinator,
    parent_lease: &NativeSessionLease,
    child_id: &str,
) -> Result<SubagentTurnResult, SubagentWorkerError> {
    let control = coordinator.issue_control(parent_lease)?;
    let run = coordinator.resume_child(parent_lease, child_id)?;
    let checked = (|| {
        let record = sessions
            .load(child_id)?
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.to_owned()))?;
        let link = record
            .session
            .subagent
            .as_ref()
            .ok_or_else(|| SubagentWorkerError::InvalidChildAdmission(child_id.to_owned()))?;
        let admissions = record
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::TurnAdmitted {
                    turn_id,
                    prompt,
                    mode,
                    ..
                } => Some((turn_id.as_str(), prompt.as_str(), *mode)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let inputs = coordinator.inputs_control(&control, child_id)?;
        let committed_inputs = inputs
            .iter()
            .filter(|input| {
                matches!(
                    input.state,
                    QueuedInputState::Done | QueuedInputState::Active
                )
            })
            .collect::<Vec<_>>();
        if record.session.backend != "native"
            || admissions
                .first()
                .is_none_or(|first| first.0 != link.spawn_id)
            || admissions.len() != committed_inputs.len() + 1
            || !admissions
                .iter()
                .skip(1)
                .zip(committed_inputs.iter())
                .all(|(actual, input)| {
                    actual.0 == input.id && actual.1 == input.prompt && actual.2 == input.mode
                })
        {
            return Err(SubagentWorkerError::InvalidChildAdmission(
                child_id.to_owned(),
            ));
        }
        let expected = record
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                AgentEventKind::CheckpointCommitted { reference } => {
                    Some(reference.compatibility.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| CheckpointCompatibility {
                provider: String::new(),
                model: String::new(),
                context_format_key: String::new(),
            });
        let checkpoints = CheckpointFileStore::new(sessions.storage_root())?;
        let replay = replay_native(child_id, &record.events, Some(&checkpoints), &expected)?;
        if replay.recovery != ReplayRecovery::Idle || !replay.can_start_new_turn {
            return Err(SubagentWorkerError::UnresolvedChild(child_id.to_owned()));
        }
        terminal_from_events(child_id, &record.events)
    })();
    let terminal = match checked {
        Ok(terminal) => terminal,
        Err(error) => {
            coordinator.pause_child(&run)?;
            return Err(error);
        }
    };
    let outcome = match terminal.status {
        SubagentStatus::Completed => SubagentOutcome::Completed,
        SubagentStatus::Failed => SubagentOutcome::Failed,
        SubagentStatus::Cancelled => SubagentOutcome::Cancelled,
        _ => unreachable!("terminal_from_events only returns terminal status"),
    };
    coordinator.finish_child(&run, outcome)?;
    Ok(terminal)
}

fn is_terminal(kind: &AgentEventKind) -> bool {
    matches!(
        kind,
        AgentEventKind::TurnCompleted { .. }
            | AgentEventKind::TurnFailed { .. }
            | AgentEventKind::TurnCancelled
    )
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CommandHookConfig, HookPhase, LocalTools, ModelOutput, PermissionMode, ToolContext,
        ToolDefinition, ToolOutput,
    };
    use async_trait::async_trait;
    use openide_core::{
        AgentEvent, AgentMessage, MessageRole, PermissionResolution, ToolCall,
        HOOK_CONFIG_IDENTITY_VERSION, MODEL_CONTEXT_VERSION,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct ChildModel(AtomicUsize);

    #[async_trait]
    impl ModelBackend for ChildModel {
        fn model_id(&self) -> &str {
            "fixture-child"
        }

        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([7; 32])
        }

        async fn complete(
            &self,
            _messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                content: "child finished".into(),
                tool_calls: Vec::new(),
            })
        }
    }

    struct WaitingModel(AtomicBool);

    struct DelegatingModel {
        saw_leaf_without_coordination: AtomicBool,
        saw_plan_without_coordination: AtomicBool,
    }

    #[async_trait]
    impl ModelBackend for DelegatingModel {
        fn model_id(&self) -> &str {
            "fixture-delegating"
        }

        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([12; 32])
        }

        async fn complete(
            &self,
            messages: &[AgentMessage],
            tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            let prompt = messages
                .iter()
                .rev()
                .find(|message| message.role == MessageRole::User)
                .map(|message| message.content.as_str())
                .unwrap();
            match prompt {
                "delegate" | "delegate-denied" => {
                    assert!(tools.iter().any(|tool| tool.name == "spawn_agent"));
                    if messages
                        .iter()
                        .any(|message| message.role == MessageRole::Tool)
                    {
                        Ok(ModelOutput {
                            content: "delegation finished".into(),
                            tool_calls: Vec::new(),
                        })
                    } else {
                        Ok(ModelOutput {
                            content: String::new(),
                            tool_calls: vec![ToolCall {
                                id: "nested-spawn-1".into(),
                                name: "spawn_agent".into(),
                                arguments: json!({"prompt":"leaf"}),
                            }],
                        })
                    }
                }
                "leaf" => {
                    self.saw_leaf_without_coordination.store(
                        !tools.iter().any(|tool| tool.name == "spawn_agent"),
                        Ordering::SeqCst,
                    );
                    Ok(ModelOutput {
                        content: "leaf finished".into(),
                        tool_calls: Vec::new(),
                    })
                }
                "plan" => {
                    self.saw_plan_without_coordination.store(
                        !tools.iter().any(|tool| tool.name == "spawn_agent"),
                        Ordering::SeqCst,
                    );
                    Ok(ModelOutput {
                        content: "plan finished".into(),
                        tool_calls: Vec::new(),
                    })
                }
                other => panic!("unexpected fixture prompt: {other}"),
            }
        }
    }

    struct PermissionModel;

    #[async_trait]
    impl ModelBackend for PermissionModel {
        fn model_id(&self) -> &str {
            "fixture-child"
        }

        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([7; 32])
        }

        async fn complete(
            &self,
            messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            if messages
                .iter()
                .any(|message| message.role == MessageRole::Tool)
            {
                Ok(ModelOutput {
                    content: "child finished".into(),
                    tool_calls: vec![],
                })
            } else {
                Ok(ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "effect-1".into(),
                        name: "effect".into(),
                        arguments: json!({}),
                    }],
                })
            }
        }
    }

    struct CountingTools(AtomicUsize);

    #[async_trait]
    impl ToolExecutor for CountingTools {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "effect".into(),
                description: "Count executions".into(),
                parameters: json!({"type":"object","properties":{}}),
            }]
        }

        async fn execute(
            &self,
            _call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::success("done"))
        }
    }

    #[derive(Clone, Copy)]
    enum CrashPoint {
        ModelInFlight,
        ResponseCommitted,
        PermissionPending,
        ToolStarted,
        FailedToolBatch,
    }

    fn append_child(sessions: &SessionStore, child_id: &str, kind: AgentEventKind) {
        let stored = sessions.load(child_id).unwrap().unwrap();
        let sequence = stored.events.last().map_or(1, |event| event.sequence + 1);
        sessions
            .append_native_event(&AgentEvent::new(child_id, sequence, kind))
            .unwrap();
    }

    fn crashed_child(
        root: &Path,
        model: Arc<dyn ModelBackend>,
        tools: Arc<dyn ToolExecutor>,
        crash_point: CrashPoint,
    ) -> (String, String) {
        crashed_child_with_policy(root, model, tools, crash_point, PermissionPolicy::new())
    }

    fn crashed_child_with_policy(
        root: &Path,
        model: Arc<dyn ModelBackend>,
        tools: Arc<dyn ToolExecutor>,
        crash_point: CrashPoint,
        permission_policy: PermissionPolicy,
    ) -> (String, String) {
        let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
        let parent = sessions.create(root, "native").unwrap();
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            tools.clone(),
            "fixture",
            "https://fixture.example/v1",
        )
        .with_permission_policy(permission_policy.clone());
        let child = worker
            .prepare_child(
                &parent_lease,
                "spawn-recover",
                "Recover safely",
                TurnMode::Build,
            )
            .unwrap();
        let child_id = child.session.id;
        sessions
            .admit_native_turn(
                &child_id,
                "spawn-recover",
                "Recover safely",
                model.model_id(),
                TurnMode::Build,
            )
            .unwrap();
        let run_lease = coordinator.start_child(&parent_lease, &child_id).unwrap();
        let catalog = ToolCatalog::from_executor(tools.as_ref()).unwrap();
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::SessionStarted {
                workspace: root.to_path_buf(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::TurnStarted {
                prompt: "Recover safely".into(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::ExecPolicyCommitted {
                version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                digest: permission_policy.digest_hex(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: native_model_context_digest(
                    "fixture",
                    "https://fixture.example/v1",
                    model.model_id(),
                    TurnMode::Build,
                    root,
                    &catalog,
                    "",
                    [7; 32],
                )
                .unwrap(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: model.model_id().to_owned(),
            },
        );
        match crash_point {
            CrashPoint::ModelInFlight => {}
            CrashPoint::ResponseCommitted => {
                append_child(
                    &sessions,
                    &child_id,
                    AgentEventKind::ModelResponseCommitted {
                        step: 1,
                        assistant: AgentMessage::assistant("already committed", vec![]),
                        response_id: None,
                        usage: None,
                    },
                );
            }
            CrashPoint::PermissionPending
            | CrashPoint::ToolStarted
            | CrashPoint::FailedToolBatch => {
                let call = ToolCall {
                    id: "effect-1".into(),
                    name: "effect".into(),
                    arguments: json!({}),
                };
                append_child(
                    &sessions,
                    &child_id,
                    AgentEventKind::ModelResponseCommitted {
                        step: 1,
                        assistant: AgentMessage::assistant("", vec![call.clone()]),
                        response_id: None,
                        usage: None,
                    },
                );
                if matches!(crash_point, CrashPoint::ToolStarted) {
                    append_child(
                        &sessions,
                        &child_id,
                        AgentEventKind::ToolStarted { tool_call: call },
                    );
                } else if matches!(crash_point, CrashPoint::PermissionPending) {
                    append_child(
                        &sessions,
                        &child_id,
                        AgentEventKind::PermissionRequested {
                            request_id: "request-before-crash".into(),
                            tool_call: call,
                        },
                    );
                } else {
                    append_child(
                        &sessions,
                        &child_id,
                        AgentEventKind::TurnFailed {
                            error: "worker exited".into(),
                        },
                    );
                }
            }
        }
        drop(run_lease);
        drop(parent_lease);
        drop(worker);
        drop(coordinator);
        drop(sessions);
        (parent.id, child_id)
    }

    fn reopened_worker(
        root: &Path,
        model: Arc<dyn ModelBackend>,
        tools: Arc<dyn ToolExecutor>,
    ) -> (Arc<SessionStore>, Arc<SubagentCoordinator>, SubagentWorker) {
        let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model,
            tools,
            "fixture",
            "https://fixture.example/v1",
        );
        (sessions, coordinator, worker)
    }

    async fn next_bridge_permission(
        events: &mpsc::Receiver<SubagentBridgeEvent>,
    ) -> (String, String) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match events.try_recv() {
                    Ok(SubagentBridgeEvent::Event {
                        child_session_id,
                        event,
                    }) => {
                        if let AgentEventKind::PermissionRequested { request_id, .. } = &event.kind
                        {
                            return (child_session_id, request_id.clone());
                        }
                    }
                    Ok(SubagentBridgeEvent::Closed { .. }) => {
                        panic!("child closed before permission request")
                    }
                    Err(mpsc::TryRecvError::Disconnected) => panic!("bridge disconnected"),
                    Err(mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn live_child_permission_is_forwarded_and_resolved_once() {
        for (choice, expected_executions) in
            [(PermissionChoice::Allow, 1), (PermissionChoice::Deny, 0)]
        {
            let root = tempfile::tempdir().unwrap();
            let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
            let parent = sessions.create(root.path(), "native").unwrap();
            let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
            let coordinator =
                Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
            let bridge = Arc::new(SubagentEventBridge::new());
            let events = bridge.subscribe();
            let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
            let worker = SubagentWorker::new(
                coordinator,
                sessions.clone(),
                Arc::new(PermissionModel),
                tools.clone(),
                "fixture",
                "https://fixture.example/v1",
            )
            .with_permission_policy(
                PermissionPolicy::new().with_tool("effect", PermissionMode::Ask),
            )
            .with_event_bridge(bridge.clone());
            let child = worker
                .prepare_child(&parent_lease, "spawn-permission", "Act", TurnMode::Build)
                .unwrap();
            let child_id = child.session.id;
            let running = tokio::spawn(async move {
                worker
                    .run(
                        parent_lease,
                        "spawn-permission".into(),
                        "Act".into(),
                        TurnMode::Build,
                        CancellationToken::new(),
                    )
                    .await
            });
            let (reported_child, request_id) = next_bridge_permission(&events).await;
            assert_eq!(reported_child, child_id);
            assert!(sessions.load(&child_id).unwrap().unwrap().events.iter().any(
                |event| matches!(&event.kind, AgentEventKind::PermissionRequested { request_id: id, .. } if id == &request_id)
            ));
            assert_eq!(tools.0.load(Ordering::SeqCst), 0);
            assert!(!bridge.resolve_permission("wrong-child", &request_id, choice));
            assert!(!bridge.resolve_permission(&child_id, "wrong-request", choice));
            assert!(bridge.resolve_permission(&child_id, &request_id, choice));
            assert!(!bridge.resolve_permission(&child_id, &request_id, choice));
            let result = tokio::time::timeout(Duration::from_secs(5), running)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(result.status, SubagentStatus::Completed);
            assert_eq!(tools.0.load(Ordering::SeqCst), expected_executions);
            let durable = sessions.load(&child_id).unwrap().unwrap().events;
            let resolution = if choice == PermissionChoice::Allow {
                PermissionResolution::Allowed
            } else {
                PermissionResolution::Denied
            };
            assert!(durable.iter().any(|event| matches!(
                &event.kind,
                AgentEventKind::PermissionResolved { request_id: id, resolution: actual, .. }
                    if id == &request_id && actual == &resolution
            )));
            let mut saw_close = false;
            while let Ok(event) = events.try_recv() {
                if matches!(event, SubagentBridgeEvent::Closed { child_session_id } if child_session_id == child_id)
                {
                    saw_close = true;
                }
            }
            assert!(saw_close);
            assert!(!bridge.resolve_permission(&child_id, &request_id, choice));
        }
    }

    #[test]
    fn same_request_id_in_two_live_children_is_scoped_by_child_id() {
        let bridge = Arc::new(SubagentEventBridge::new());
        let first_permissions = Arc::new(PendingPermissions::default());
        let second_permissions = Arc::new(PendingPermissions::default());
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let (second_tx, second_rx) = tokio::sync::oneshot::channel();
        first_permissions.insert("same-request".into(), first_tx);
        second_permissions.insert("same-request".into(), second_tx);
        let first = bridge
            .register_capability("child-a", first_permissions, CancellationToken::new())
            .unwrap();
        let second = bridge
            .register_capability("child-b", second_permissions, CancellationToken::new())
            .unwrap();
        let call = ToolCall {
            id: "effect-1".into(),
            name: "effect".into(),
            arguments: json!({}),
        };
        for registration in [&first, &second] {
            registration.forward(AgentEvent::new(
                &registration.child_session_id,
                10,
                AgentEventKind::PermissionRequested {
                    request_id: "same-request".into(),
                    tool_call: call.clone(),
                },
            ));
        }
        assert!(!bridge.resolve_permission("child-c", "same-request", PermissionChoice::Allow));
        assert!(bridge.resolve_permission("child-a", "same-request", PermissionChoice::Allow));
        assert!(!bridge.resolve_permission("child-a", "same-request", PermissionChoice::Deny));
        assert!(bridge.resolve_permission("child-b", "same-request", PermissionChoice::Deny));
        assert_eq!(first_rx.blocking_recv().unwrap(), PermissionChoice::Allow);
        assert_eq!(second_rx.blocking_recv().unwrap(), PermissionChoice::Deny);
    }

    #[tokio::test]
    async fn cancelling_child_closes_approval_before_any_effect() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let bridge = Arc::new(SubagentEventBridge::new());
        let events = bridge.subscribe();
        let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
        let worker = SubagentWorker::new(
            coordinator,
            sessions.clone(),
            Arc::new(PermissionModel),
            tools.clone(),
            "fixture",
            "https://fixture.example/v1",
        )
        .with_permission_policy(PermissionPolicy::new().with_tool("effect", PermissionMode::Ask))
        .with_event_bridge(bridge.clone());
        let child = worker
            .prepare_child(&parent_lease, "spawn-cancel", "Act", TurnMode::Build)
            .unwrap();
        let child_id = child.session.id;
        let running = tokio::spawn(async move {
            worker
                .run(
                    parent_lease,
                    "spawn-cancel".into(),
                    "Act".into(),
                    TurnMode::Build,
                    CancellationToken::new(),
                )
                .await
        });
        let (_, request_id) = next_bridge_permission(&events).await;
        assert!(bridge.cancel_child(&child_id));
        assert!(!bridge.cancel_child(&child_id));
        assert!(!bridge.resolve_permission(&child_id, &request_id, PermissionChoice::Allow));
        let result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.status, SubagentStatus::Cancelled);
        assert_eq!(tools.0.load(Ordering::SeqCst), 0);
        assert!(!bridge.cancel_child(&child_id));
    }

    #[test]
    fn stale_registration_and_slow_subscriber_cannot_hide_new_child_events() {
        let bridge = Arc::new(SubagentEventBridge::new());
        let slow = bridge.subscribe();
        let first = bridge
            .register_capability(
                "child",
                Arc::new(PendingPermissions::default()),
                CancellationToken::new(),
            )
            .unwrap();
        for sequence in 1..=(BRIDGE_SUBSCRIBER_CAPACITY as u64 + 1) {
            first.forward(AgentEvent::new(
                "child",
                sequence,
                AgentEventKind::AssistantDelta {
                    content: "x".into(),
                },
            ));
        }
        drop(first);
        for _ in 0..BRIDGE_SUBSCRIBER_CAPACITY {
            assert!(slow.try_recv().is_ok());
        }
        assert!(matches!(
            slow.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        let fresh = bridge.subscribe();
        let second = bridge
            .register_capability(
                "child",
                Arc::new(PendingPermissions::default()),
                CancellationToken::new(),
            )
            .unwrap();
        second.forward(AgentEvent::new(
            "child",
            1,
            AgentEventKind::AssistantMessage {
                content: "new run".into(),
            },
        ));
        assert!(matches!(
            fresh.try_recv(),
            Ok(SubagentBridgeEvent::Event { event, .. })
                if matches!(event.kind, AgentEventKind::AssistantMessage { .. })
        ));
    }

    #[tokio::test]
    async fn reopened_bridge_cannot_approve_crashed_permission_request() {
        let root = tempfile::tempdir().unwrap();
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
        let (parent_id, child_id) = crashed_child(
            root.path(),
            model.clone(),
            tools.clone(),
            CrashPoint::PermissionPending,
        );
        let bridge = Arc::new(SubagentEventBridge::new());
        assert!(!bridge.resolve_permission(
            &child_id,
            "request-before-crash",
            PermissionChoice::Allow
        ));
        let (sessions, _, worker) = reopened_worker(root.path(), model.clone(), tools.clone());
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
        let result = worker
            .with_event_bridge(bridge)
            .resume(
                parent_lease,
                child_id.clone(),
                "spawn-recover".into(),
                "Recover safely".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(result, SubagentRecoveryState::Paused {
            reason: SubagentRecoveryPause::NeedsPermission { request_id, .. }, ..
        } if request_id == "request-before-crash"));
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        assert_eq!(tools.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn recorded_child_permission_is_replayed_after_crash() {
        for (decision, expected_executions) in [
            (PermissionDecision::Allow, 1),
            (PermissionDecision::Deny, 0),
        ] {
            let root = tempfile::tempdir().unwrap();
            let model = Arc::new(ChildModel(AtomicUsize::new(0)));
            let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
            let (parent_id, child_id) = crashed_child(
                root.path(),
                model.clone(),
                tools.clone(),
                CrashPoint::PermissionPending,
            );
            let (sessions, _, worker) = reopened_worker(root.path(), model.clone(), tools.clone());
            sessions
                .record_permission_decision(&child_id, "request-before-crash", decision)
                .unwrap();
            let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
            let result = worker
                .resume(
                    parent_lease,
                    child_id.clone(),
                    "spawn-recover".into(),
                    "Recover safely".into(),
                    TurnMode::Build,
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            assert!(matches!(
                result,
                SubagentRecoveryState::Terminal(SubagentTurnResult {
                    status: SubagentStatus::Completed,
                    ..
                })
            ));
            assert_eq!(tools.0.load(Ordering::SeqCst), expected_executions);
            let events = sessions.load(&child_id).unwrap().unwrap().events;
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::PermissionResolved { .. }))
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn child_recovery_session_approval_records_grant_before_resuming_effect() {
        let root = tempfile::tempdir().unwrap();
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
        let policy = PermissionPolicy::new().with_tool("effect", PermissionMode::Ask);
        let (parent_id, child_id) = crashed_child_with_policy(
            root.path(),
            model.clone(),
            tools.clone(),
            CrashPoint::PermissionPending,
            policy.clone(),
        );
        let (sessions, coordinator, worker) =
            reopened_worker(root.path(), model.clone(), tools.clone());
        let worker = worker
            .with_permission_policy(policy)
            .with_session_approvals(CommandIsolation::HostUnrestricted);
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
        assert!(matches!(
            worker.resolve_recovery_session_approval(
                parent_lease,
                &child_id,
                "spawn-recover",
                "Recover safely",
                TurnMode::Build,
                "wrong-request",
            ),
            Err(SubagentWorkerError::Recovery(ResumeError::StaleResolution))
        ));
        assert!(sessions
            .session_approval_grants(&child_id)
            .unwrap()
            .is_empty());
        assert_eq!(
            coordinator.list_children_for_ui(&parent_id).unwrap()[0].status,
            SubagentStatus::Interrupted
        );
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
        worker
            .resolve_recovery_session_approval(
                parent_lease,
                &child_id,
                "spawn-recover",
                "Recover safely",
                TurnMode::Build,
                "request-before-crash",
            )
            .unwrap();
        let decisions = sessions.load_permission_decisions(&child_id).unwrap();
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].grant.is_some());
        assert_eq!(tools.0.load(Ordering::SeqCst), 0);
        let events = sessions.load(&child_id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::PermissionResolved { .. }))
                .count(),
            1
        );
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
        let result = worker
            .resume(
                parent_lease,
                child_id.clone(),
                "spawn-recover".into(),
                "Recover safely".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            result,
            SubagentRecoveryState::Terminal(SubagentTurnResult {
                status: SubagentStatus::Completed,
                ..
            })
        ));
        assert_eq!(tools.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn interrupted_model_request_reopens_without_duplicate_admission_or_request() {
        let root = tempfile::tempdir().unwrap();
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
        let (parent_id, child_id) = crashed_child(
            root.path(),
            model.clone(),
            tools.clone(),
            CrashPoint::ModelInFlight,
        );
        let (sessions, coordinator, worker) = reopened_worker(root.path(), model.clone(), tools);
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
        assert_eq!(
            coordinator.list_children_for_ui(&parent_id).unwrap()[0].status,
            SubagentStatus::Interrupted
        );
        let result = worker
            .resume(
                parent_lease.clone(),
                child_id.clone(),
                "spawn-recover".into(),
                "Recover safely".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            result,
            SubagentRecoveryState::Terminal(SubagentTurnResult {
                status: SubagentStatus::Completed,
                ..
            })
        ));
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        let events = sessions.load(&child_id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. }))
                .count(),
            1
        );
        assert_eq!(
            coordinator.list_children(&parent_lease).unwrap()[0].status,
            SubagentStatus::Completed
        );
    }

    #[tokio::test]
    async fn committed_response_reopens_without_asking_model_again() {
        let root = tempfile::tempdir().unwrap();
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
        let (parent_id, child_id) = crashed_child(
            root.path(),
            model.clone(),
            tools.clone(),
            CrashPoint::ResponseCommitted,
        );
        let (sessions, _, worker) = reopened_worker(root.path(), model.clone(), tools);
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
        let result = worker
            .resume(
                parent_lease,
                child_id.clone(),
                "spawn-recover".into(),
                "Recover safely".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            result,
            SubagentRecoveryState::Terminal(SubagentTurnResult {
                status: SubagentStatus::Completed,
                ..
            })
        ));
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        let events = sessions.load(&child_id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn uncertain_tool_and_failed_batch_pause_without_reexecuting_effects() {
        for crash_point in [CrashPoint::ToolStarted, CrashPoint::FailedToolBatch] {
            let root = tempfile::tempdir().unwrap();
            let model = Arc::new(ChildModel(AtomicUsize::new(0)));
            let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
            let (parent_id, child_id) =
                crashed_child(root.path(), model.clone(), tools.clone(), crash_point);
            let (sessions, coordinator, worker) =
                reopened_worker(root.path(), model.clone(), tools.clone());
            let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
            let before = sessions.load(&child_id).unwrap().unwrap().events.len();
            for _ in 0..2 {
                let result = worker
                    .resume(
                        parent_lease.clone(),
                        child_id.clone(),
                        "spawn-recover".into(),
                        "Recover safely".into(),
                        TurnMode::Build,
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                match (crash_point, result) {
                    (
                        CrashPoint::ToolStarted,
                        SubagentRecoveryState::Paused {
                            reason: SubagentRecoveryPause::ToolOutcomeUncertain { tool_call, .. },
                            ..
                        },
                    ) => {
                        assert_eq!(tool_call.id, "effect-1");
                    }
                    (
                        CrashPoint::FailedToolBatch,
                        SubagentRecoveryState::Paused {
                            reason: SubagentRecoveryPause::AbortedToolBatch { next_tool_call },
                            ..
                        },
                    ) => {
                        assert_eq!(next_tool_call.id, "effect-1");
                    }
                    (_, other) => panic!("unexpected child recovery state: {other:?}"),
                }
                assert_eq!(
                    coordinator.list_children_for_ui(&parent_id).unwrap()[0].status,
                    SubagentStatus::Interrupted
                );
            }
            assert_eq!(tools.0.load(Ordering::SeqCst), 0);
            assert_eq!(model.0.load(Ordering::SeqCst), 0);
            assert_eq!(
                sessions.load(&child_id).unwrap().unwrap().events.len(),
                before
            );
        }
    }

    #[test]
    fn discarded_failed_child_batch_finalizes_without_model_or_tool_reexecution() {
        let root = tempfile::tempdir().unwrap();
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
        let (parent_id, child_id) = crashed_child(
            root.path(),
            model.clone(),
            tools.clone(),
            CrashPoint::FailedToolBatch,
        );
        let (sessions, coordinator, _) = reopened_worker(root.path(), model.clone(), tools.clone());
        let checkpoints = CheckpointFileStore::new(sessions.storage_root()).unwrap();
        let parent_lease = sessions.try_acquire_native_lease(&parent_id).unwrap();
        assert!(matches!(
            finalize_resolved_subagent(&sessions, &coordinator, &parent_lease, &child_id),
            Err(SubagentWorkerError::UnresolvedChild(_))
        ));
        let child_lease = sessions.try_acquire_native_lease(&child_id).unwrap();
        crate::discard_aborted_tool_batch(&sessions, &checkpoints, &child_lease, "effect-1")
            .unwrap();
        drop(child_lease);
        let terminal =
            finalize_resolved_subagent(&sessions, &coordinator, &parent_lease, &child_id).unwrap();
        assert_eq!(terminal.status, SubagentStatus::Failed);
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        assert_eq!(tools.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            coordinator.list_children(&parent_lease).unwrap()[0].status,
            SubagentStatus::Failed
        );
        let events = sessions.load(&child_id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ToolBatchDiscarded { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn verified_started_child_tool_stays_active_until_model_closes_turn() {
        let root = tempfile::tempdir().unwrap();
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
        let (parent_id, child_id) = crashed_child(
            root.path(),
            model.clone(),
            tools.clone(),
            CrashPoint::ToolStarted,
        );
        let (sessions, coordinator, _) = reopened_worker(root.path(), model.clone(), tools.clone());
        let checkpoints = CheckpointFileStore::new(sessions.storage_root()).unwrap();
        let child_lease = sessions.try_acquire_native_lease(&child_id).unwrap();
        crate::record_verified_tool_outcome(
            &sessions,
            &checkpoints,
            &child_lease,
            "effect-1",
            ToolOutput::success("checked externally"),
            RunLimits::default().max_tool_output_chars,
        )
        .unwrap();
        drop(child_lease);
        let parent_lease = sessions.try_acquire_native_lease(&parent_id).unwrap();
        assert!(matches!(
            finalize_resolved_subagent(&sessions, &coordinator, &parent_lease, &child_id),
            Err(SubagentWorkerError::UnresolvedChild(_))
        ));
        assert_eq!(tools.0.load(Ordering::SeqCst), 0);
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            coordinator.list_children(&parent_lease).unwrap()[0].status,
            SubagentStatus::Interrupted
        );
    }

    #[tokio::test]
    async fn changed_child_identity_is_rejected_before_claim_or_dispatch() {
        let root = tempfile::tempdir().unwrap();
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
        let (parent_id, child_id) = crashed_child(
            root.path(),
            model.clone(),
            tools.clone(),
            CrashPoint::ModelInFlight,
        );
        let (sessions, coordinator, worker) = reopened_worker(root.path(), model.clone(), tools);
        let changed = worker.with_system_instructions("different instructions");
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
        let result = changed
            .resume(
                parent_lease,
                child_id.clone(),
                "spawn-recover".into(),
                "Recover safely".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(SubagentWorkerError::Coordination(SubagentError::Store(
                StoreError::ConflictingSubagentIdentity
            )))
        ));
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            coordinator.list_children_for_ui(&parent_id).unwrap()[0].status,
            SubagentStatus::Interrupted
        );
    }

    #[tokio::test]
    async fn changed_hook_policy_cannot_resume_child_with_old_spawn_identity() {
        let root = tempfile::tempdir().unwrap();
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools = Arc::new(CountingTools(AtomicUsize::new(0)));
        let (parent_id, child_id) = crashed_child(
            root.path(),
            model.clone(),
            tools.clone(),
            CrashPoint::ModelInFlight,
        );
        let (sessions, coordinator, worker) =
            reopened_worker(root.path(), model.clone(), tools.clone());
        let executable = root.path().join("hook.bin");
        std::fs::write(&executable, b"hook").unwrap();
        let hooks = CommandHooks::new(vec![CommandHookConfig {
            id: "review".into(),
            phase: HookPhase::PreToolUse,
            tool_name: "effect".into(),
            executable,
            args: vec![],
            cwd: root.path().to_path_buf(),
            timeout_ms: 1000,
            max_output_bytes: 4096,
        }])
        .unwrap();
        let changed = worker.with_command_hooks(Arc::new(hooks));
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent_id).unwrap());
        let result = changed
            .resume(
                parent_lease,
                child_id,
                "spawn-recover".into(),
                "Recover safely".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(SubagentWorkerError::Coordination(SubagentError::Store(
                StoreError::ConflictingSubagentIdentity
            )))
        ));
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        assert_eq!(tools.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            coordinator.list_children_for_ui(&parent_id).unwrap()[0].status,
            SubagentStatus::Interrupted
        );
    }

    #[async_trait]
    impl ModelBackend for WaitingModel {
        fn model_id(&self) -> &str {
            "fixture-child"
        }

        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([7; 32])
        }

        async fn complete(
            &self,
            _messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            self.0.store(true, Ordering::SeqCst);
            cancel.cancelled().await;
            Err(AgentError::Cancelled)
        }
    }

    #[tokio::test]
    async fn child_turn_is_durable_and_duplicate_spawn_does_not_rerun_model() {
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
        let parent = sessions.create(workspace.path(), "native").unwrap();
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        );
        let first = worker
            .run(
                parent_lease.clone(),
                "spawn-1".into(),
                "Inspect the workspace".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(first.status, SubagentStatus::Completed);
        assert_eq!(first.response.as_deref(), Some("child finished"));
        let stored = sessions.load(&first.child_session_id).unwrap().unwrap();
        assert_eq!(
            stored.session.subagent.as_ref().unwrap().parent_session_id,
            parent.id
        );
        assert!(matches!(
            stored.events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        let second = worker
            .run(
                parent_lease.clone(),
                "spawn-1".into(),
                "Inspect the workspace".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        let children = coordinator.list_children(&parent_lease).unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].status, SubagentStatus::Completed);
    }

    #[tokio::test]
    async fn cancellation_before_child_start_persists_without_model_request() {
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
        let parent = sessions.create(workspace.path(), "native").unwrap();
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions,
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = worker
            .run(
                parent_lease.clone(),
                "spawn-cancelled".into(),
                "Inspect the workspace".into(),
                TurnMode::Build,
                cancel,
            )
            .await
            .unwrap();
        assert_eq!(result.status, SubagentStatus::Cancelled);
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        assert!(coordinator.list_children(&parent_lease).unwrap()[0].cancellation_requested);
    }

    #[tokio::test]
    async fn coordinator_cancellation_reaches_running_child_and_commits_terminal_state() {
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
        let parent = sessions.create(workspace.path(), "native").unwrap();
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let model = Arc::new(WaitingModel(AtomicBool::new(false)));
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        );
        let prepared = worker
            .prepare_child(&parent_lease, "spawn-running", "Wait", TurnMode::Build)
            .unwrap();
        let child_id = prepared.session.id;
        let run_parent_lease = parent_lease.clone();
        let running = tokio::spawn(async move {
            worker
                .run(
                    run_parent_lease,
                    "spawn-running".into(),
                    "Wait".into(),
                    TurnMode::Build,
                    CancellationToken::new(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !model.0.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        coordinator
            .request_cancel(&parent_lease, &child_id)
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.status, SubagentStatus::Cancelled);
        assert!(coordinator.list_children(&parent_lease).unwrap()[0].cancellation_requested);
        assert!(matches!(
            sessions
                .load(&child_id)
                .unwrap()
                .unwrap()
                .events
                .last()
                .unwrap()
                .kind,
            AgentEventKind::TurnCancelled
        ));
    }

    #[tokio::test]
    async fn prepared_child_runs_without_holding_parent_turn_lease() {
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
        let parent = sessions.create(workspace.path(), "native").unwrap();
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let model = Arc::new(WaitingModel(AtomicBool::new(false)));
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        );
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = worker
            .prepare_child(&parent_lease, "spawn-background", "Wait", TurnMode::Build)
            .unwrap();
        let control = coordinator.issue_control(&parent_lease).unwrap();
        drop(parent_lease);
        let child_id = child.session.id;
        let run_child_id = child_id.clone();
        let running = tokio::spawn(async move {
            worker
                .run_prepared(
                    control,
                    run_child_id,
                    "spawn-background".into(),
                    "Wait".into(),
                    TurnMode::Build,
                    CancellationToken::new(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !model.0.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let next_parent_turn = sessions.try_acquire_native_lease(&parent.id).unwrap();
        coordinator
            .request_cancel(&next_parent_turn, &child_id)
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.status, SubagentStatus::Cancelled);
    }

    #[tokio::test]
    async fn prepared_child_rejects_changed_model_context_before_dispatch() {
        let workspace = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
        let parent = sessions.create(workspace.path(), "native").unwrap();
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        );
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = worker
            .prepare_child(&parent_lease, "spawn-exact", "Inspect", TurnMode::Build)
            .unwrap();
        let control = coordinator.issue_control(&parent_lease).unwrap();
        drop(parent_lease);
        let changed = worker.with_system_instructions("changed instructions");
        let result = changed
            .run_prepared(
                control,
                child.session.id,
                "spawn-exact".into(),
                "Inspect".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(SubagentWorkerError::Coordination(SubagentError::Store(
                StoreError::ConflictingSubagentIdentity
            )))
        ));
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn queued_follow_up_recovers_after_enqueue_claim_or_admission_crash() {
        for crash_stage in [0, 1, 2] {
            let root = tempfile::tempdir().unwrap();
            let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
            let parent = sessions.create(root.path(), "native").unwrap();
            let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
            let coordinator =
                Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
            let model = Arc::new(ChildModel(AtomicUsize::new(0)));
            let tools: Arc<dyn ToolExecutor> = Arc::new(LocalTools::new());
            let worker = SubagentWorker::new(
                coordinator.clone(),
                sessions.clone(),
                model.clone(),
                tools.clone(),
                "fixture",
                "https://fixture.example/v1",
            );
            let initial = worker
                .run(
                    parent_lease.clone(),
                    "spawn-follow-up".into(),
                    "first".into(),
                    TurnMode::Build,
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(initial.status, SubagentStatus::Completed);
            assert_eq!(model.0.load(Ordering::SeqCst), 1);
            let input = coordinator
                .enqueue_input(
                    &parent_lease,
                    &initial.child_session_id,
                    "input-follow-up",
                    "second",
                    TurnMode::Build,
                )
                .unwrap();
            if crash_stage > 0 {
                let control = coordinator.issue_control(&parent_lease).unwrap();
                let (run, claimed) = coordinator
                    .claim_next_input_control(&control, &initial.child_session_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(claimed.id, input.id);
                if crash_stage > 1 {
                    sessions
                        .admit_native_turn(
                            &initial.child_session_id,
                            &input.id,
                            &input.prompt,
                            model.model_id(),
                            input.mode,
                        )
                        .unwrap();
                }
                drop(run);
            }
            drop(worker);
            drop(coordinator);
            drop(parent_lease);
            drop(sessions);

            let (sessions, coordinator, worker) =
                reopened_worker(root.path(), model.clone(), tools);
            let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
            let control = coordinator.issue_control(&parent_lease).unwrap();
            if crash_stage == 0 {
                worker
                    .drain_queued_prepared(
                        control.clone(),
                        initial.child_session_id.clone(),
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap();
            } else {
                let recovered = worker
                    .resume(
                        parent_lease.clone(),
                        initial.child_session_id.clone(),
                        "spawn-follow-up".into(),
                        "first".into(),
                        TurnMode::Build,
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                assert!(matches!(
                    recovered,
                    SubagentRecoveryState::Terminal(SubagentTurnResult {
                        status: SubagentStatus::Completed,
                        ..
                    })
                ));
            }
            let stored = sessions.load(&initial.child_session_id).unwrap().unwrap();
            assert_eq!(
                stored
                    .events
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                    .count(),
                2
            );
            assert_eq!(model.0.load(Ordering::SeqCst), 2);
            let (status, active, pending) = coordinator
                .input_state(&control, &initial.child_session_id)
                .unwrap();
            assert_eq!(status, SubagentStatus::Completed);
            assert!(active.is_none());
            assert_eq!(pending, 0);
        }
    }

    #[tokio::test]
    async fn first_child_turn_recovers_after_claim_before_admission() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools: Arc<dyn ToolExecutor> = Arc::new(LocalTools::new());
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            tools.clone(),
            "fixture",
            "https://fixture.example/v1",
        );
        let child = worker
            .prepare_child(&parent_lease, "spawn-first", "first", TurnMode::Build)
            .unwrap();
        let control = coordinator.issue_control(&parent_lease).unwrap();
        let run = coordinator
            .start_child_control(&control, &child.session.id)
            .unwrap();
        drop(run);
        drop(parent_lease);
        drop(worker);
        drop(coordinator);
        drop(sessions);

        let (sessions, _coordinator, worker) = reopened_worker(root.path(), model.clone(), tools);
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let recovered = worker
            .resume(
                parent_lease,
                child.session.id.clone(),
                "spawn-first".into(),
                "first".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            recovered,
            SubagentRecoveryState::Terminal(SubagentTurnResult {
                status: SubagentStatus::Completed,
                ..
            })
        ));
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        let stored = sessions.load(&child.session.id).unwrap().unwrap();
        assert_eq!(
            stored
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn legacy_ready_child_with_admission_recovers_without_readmission() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let model = Arc::new(ChildModel(AtomicUsize::new(0)));
        let tools: Arc<dyn ToolExecutor> = Arc::new(LocalTools::new());
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            tools.clone(),
            "fixture",
            "https://fixture.example/v1",
        );
        let child = worker
            .prepare_child(&parent_lease, "spawn-legacy", "first", TurnMode::Build)
            .unwrap();
        sessions
            .admit_native_turn(
                &child.session.id,
                "spawn-legacy",
                "first",
                model.model_id(),
                TurnMode::Build,
            )
            .unwrap();
        drop(parent_lease);
        drop(worker);
        drop(coordinator);
        drop(sessions);

        let (sessions, _coordinator, worker) = reopened_worker(root.path(), model.clone(), tools);
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let recovered = worker
            .resume(
                parent_lease,
                child.session.id.clone(),
                "spawn-legacy".into(),
                "first".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            recovered,
            SubagentRecoveryState::Terminal(SubagentTurnResult {
                status: SubagentStatus::Completed,
                ..
            })
        ));
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        let stored = sessions.load(&child.session.id).unwrap().unwrap();
        assert_eq!(
            stored
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn nested_child_uses_own_lease_and_plan_has_no_coordination_tools() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let coordinator = Arc::new(
            SubagentCoordinator::new(
                sessions.clone(),
                crate::SubagentLimits {
                    max_depth: 2,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let model = Arc::new(DelegatingModel {
            saw_leaf_without_coordination: AtomicBool::new(false),
            saw_plan_without_coordination: AtomicBool::new(false),
        });
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        )
        .with_nested_subagents(SubagentToolLimits { max_tasks: 2 });
        let parent_result = worker
            .run(
                parent_lease.clone(),
                "delegating-child".into(),
                "delegate".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(parent_result.status, SubagentStatus::Completed);
        let child_id = parent_result.child_session_id;
        let grandchild = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let children = coordinator.list_children_for_ui(&child_id).unwrap();
                if children
                    .first()
                    .is_some_and(|child| child.status == SubagentStatus::Completed)
                {
                    break children.into_iter().next().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(grandchild.session.subagent.as_ref().unwrap().depth, 2);
        assert_eq!(
            grandchild
                .session
                .subagent
                .as_ref()
                .unwrap()
                .parent_session_id,
            child_id
        );
        assert!(model.saw_leaf_without_coordination.load(Ordering::SeqCst));

        let plan_result = worker
            .run(
                parent_lease,
                "planning-child".into(),
                "plan".into(),
                TurnMode::Plan,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(plan_result.status, SubagentStatus::Completed);
        assert!(model.saw_plan_without_coordination.load(Ordering::SeqCst));
        assert!(coordinator
            .list_children_for_ui(&plan_result.child_session_id)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn nested_spawn_cannot_override_host_deny_policy() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let model = Arc::new(DelegatingModel {
            saw_leaf_without_coordination: AtomicBool::new(false),
            saw_plan_without_coordination: AtomicBool::new(false),
        });
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions,
            model,
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        )
        .with_permission_policy(
            PermissionPolicy::new().with_tool("spawn_agent", PermissionMode::Deny),
        )
        .with_nested_subagents(SubagentToolLimits::default());
        let result = worker
            .run(
                parent_lease,
                "denied-child".into(),
                "delegate-denied".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.status, SubagentStatus::Completed);
        assert!(coordinator
            .list_children_for_ui(&result.child_session_id)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn interrupted_recursive_child_recovers_with_same_catalog_and_lease() {
        let root = tempfile::tempdir().unwrap();
        let limits = crate::SubagentLimits {
            max_depth: 2,
            ..Default::default()
        };
        let model = Arc::new(DelegatingModel {
            saw_leaf_without_coordination: AtomicBool::new(false),
            saw_plan_without_coordination: AtomicBool::new(false),
        });
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator = Arc::new(SubagentCoordinator::new(sessions.clone(), limits).unwrap());
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        )
        .with_nested_subagents(SubagentToolLimits::default());
        let child = worker
            .prepare_child(
                &parent_lease,
                "recursive-recover",
                "delegate",
                TurnMode::Build,
            )
            .unwrap();
        let child_id = child.session.id;
        sessions
            .admit_native_turn(
                &child_id,
                "recursive-recover",
                "delegate",
                model.model_id(),
                TurnMode::Build,
            )
            .unwrap();
        let run = coordinator.start_child(&parent_lease, &child_id).unwrap();
        let catalog = worker.catalog_for_mode(TurnMode::Build, 1).unwrap();
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::SessionStarted {
                workspace: root.path().to_path_buf(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::TurnStarted {
                prompt: "delegate".into(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::ExecPolicyCommitted {
                version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                digest: PermissionPolicy::new().digest_hex(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: native_model_context_digest(
                    "fixture",
                    "https://fixture.example/v1",
                    model.model_id(),
                    TurnMode::Build,
                    root.path(),
                    &catalog,
                    "",
                    [12; 32],
                )
                .unwrap(),
            },
        );
        append_child(
            &sessions,
            &child_id,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: model.model_id().to_owned(),
            },
        );
        drop(run);
        drop(parent_lease);
        drop(worker);
        drop(coordinator);
        drop(sessions);

        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let coordinator = Arc::new(SubagentCoordinator::new(sessions.clone(), limits).unwrap());
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        )
        .with_nested_subagents(SubagentToolLimits::default());
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let recovered = worker
            .resume(
                parent_lease,
                child_id.clone(),
                "recursive-recover".into(),
                "delegate".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            recovered,
            SubagentRecoveryState::Terminal(SubagentTurnResult {
                status: SubagentStatus::Completed,
                ..
            })
        ));
        let grandchild = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let children = coordinator.list_children_for_ui(&child_id).unwrap();
                if children
                    .first()
                    .is_some_and(|child| child.status == SubagentStatus::Completed)
                {
                    break children.into_iter().next().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(grandchild.session.subagent.as_ref().unwrap().depth, 2);
        assert!(model.saw_leaf_without_coordination.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn legacy_child_recovers_with_its_original_nonrecursive_catalog() {
        let root = tempfile::tempdir().unwrap();
        let model = Arc::new(DelegatingModel {
            saw_leaf_without_coordination: AtomicBool::new(false),
            saw_plan_without_coordination: AtomicBool::new(false),
        });
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let base: Arc<dyn ToolExecutor> = Arc::new(LocalTools::new());
        let old_worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            base.clone(),
            "fixture",
            "https://fixture.example/v1",
        );
        let old_spec = old_worker
            .legacy_v2_spec_for(root.path(), "plan", TurnMode::Build, 1)
            .unwrap();
        let child = coordinator
            .create_child(&parent_lease, "legacy-spawn", &old_spec)
            .unwrap();
        sessions
            .admit_native_turn(
                &child.session.id,
                "legacy-spawn",
                "plan",
                model.model_id(),
                TurnMode::Build,
            )
            .unwrap();
        let run = coordinator
            .start_child(&parent_lease, &child.session.id)
            .unwrap();
        let catalog = ToolCatalog::from_executor(base.as_ref()).unwrap();
        for kind in [
            AgentEventKind::SessionStarted {
                workspace: root.path().to_path_buf(),
            },
            AgentEventKind::TurnStarted {
                prompt: "plan".into(),
            },
            AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            },
            AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
            AgentEventKind::ExecPolicyCommitted {
                version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                digest: PermissionPolicy::new().digest_hex(),
            },
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: native_model_context_digest(
                    "fixture",
                    "https://fixture.example/v1",
                    model.model_id(),
                    TurnMode::Build,
                    root.path(),
                    &catalog,
                    "",
                    [12; 32],
                )
                .unwrap(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: model.model_id().to_owned(),
            },
        ] {
            append_child(&sessions, &child.session.id, kind);
        }
        drop(run);
        drop(parent_lease);
        drop(old_worker);
        drop(coordinator);
        drop(sessions);

        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            base,
            "fixture",
            "https://fixture.example/v1",
        )
        .with_nested_subagents(SubagentToolLimits::default());
        let changed_policy = worker.clone().with_permission_policy(
            PermissionPolicy::new().with_tool("spawn_agent", PermissionMode::Deny),
        );
        assert!(matches!(
            changed_policy.worker_for_existing_child(
                &child.session.id,
                "plan",
                TurnMode::Build,
                false,
            ),
            Err(SubagentWorkerError::Context(AgentError::InvalidRequest(_)))
        ));
        assert!(changed_policy
            .validate_follow_up_child(&child.session.id)
            .is_err());
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let recovered = worker
            .resume(
                parent_lease,
                child.session.id.clone(),
                "legacy-spawn".into(),
                "plan".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            recovered,
            SubagentRecoveryState::Terminal(SubagentTurnResult {
                status: SubagentStatus::Completed,
                ..
            })
        ));
        assert!(model.saw_plan_without_coordination.load(Ordering::SeqCst));
        assert!(coordinator
            .list_children_for_ui(&child.session.id)
            .unwrap()
            .is_empty());

        model
            .saw_plan_without_coordination
            .store(false, Ordering::SeqCst);
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        coordinator
            .enqueue_input(
                &parent_lease,
                &child.session.id,
                "legacy-follow-up",
                "plan",
                TurnMode::Build,
            )
            .unwrap();
        let control = coordinator.issue_control(&parent_lease).unwrap();
        drop(parent_lease);
        worker
            .drain_queued_prepared(control, child.session.id.clone(), CancellationToken::new())
            .await
            .unwrap();
        assert!(model.saw_plan_without_coordination.load(Ordering::SeqCst));
        let stored = sessions.load(&child.session.id).unwrap().unwrap();
        assert_eq!(
            stored
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            2
        );
    }
}
