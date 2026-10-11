//! Recovery of an interrupted native turn from a verified, append-only log.
//! Each call advances at most one model or tool action. The caller retains the
//! exclusive session lease for the whole recovery workflow.

use crate::hooks::{CommandHooks, HookDecision, HookInvocation, HookPhase, HookRunResult};
use crate::native_context::{native_system_prompt_with_plan, NativeRuntimeIdentity};
use crate::permission::{effective_session_approval_policy_digest, SessionApprovalGrant};
use crate::plan_tool::{self, UPDATE_PLAN_TOOL};
use crate::user_interaction;
use crate::project_instructions::{activation_hook_id, ProjectInstructionLedger};
use crate::tool_scheduler::trusted_plan_read;
use crate::{
    AgentError, CheckpointCoordinator, CheckpointCoordinatorError, CommandIsolation, ModelBackend,
    NativeManualCommandActor, PermissionChoice, PermissionMode, PermissionPolicy, RunLimits,
    TokenBudgetDecision, TokenBudgetPolicy, TokenBudgetState, ToolCatalog, ToolCatalogError,
    ToolContext, ToolExecutor, ToolOutput,
};
use async_trait::async_trait;
use openide_core::{
    replay_native, system_instructions_digest, AgentEvent, AgentEventKind, AgentMessage,
    CheckpointBudgetSnapshot, CheckpointCompatibility, CheckpointFileStore,
    NativeCommandOperationStore, NativeReplay, NativeSessionLease, PermissionDecision,
    PermissionResolution, PlanSnapshot, ReplayError, ReplayRecovery, SessionStore, StoreError,
    StoredSession, ToolCall, TurnMode, EXEC_POLICY_IDENTITY_VERSION, HOOK_CONFIG_IDENTITY_VERSION,
    MAX_SYSTEM_INSTRUCTIONS_BYTES, MCP_RUNTIME_IDENTITY_VERSION, MODEL_CONTEXT_VERSION,
    SYSTEM_INSTRUCTIONS_VERSION,
};
use std::collections::HashSet;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ResumeError {
    #[error("session store failed: {0}")]
    Store(#[from] StoreError),
    #[error("native replay failed: {0}")]
    Replay(#[from] ReplayError),
    #[error("agent step failed: {0}")]
    Agent(#[from] AgentError),
    #[error("tool catalog failed: {0}")]
    Catalog(#[from] ToolCatalogError),
    #[error("native session is missing")]
    SessionMissing,
    #[error("recovery requires a native session")]
    NonNativeSession,
    #[error("recovery model differs from the admitted turn or checkpoint")]
    ModelMismatch,
    #[error("the interrupted turn has no verified tool catalog marker")]
    CatalogUnverified,
    #[error("the current tool catalog differs from the one offered to the model")]
    CatalogMismatch,
    #[error("the interrupted turn has no verified model context marker")]
    ContextUnverified,
    #[error("the interrupted turn uses an older model context marker without a request profile")]
    ContextVersionUnsupported,
    #[error(
        "the configured provider, endpoint, or system context differs from the interrupted turn"
    )]
    ContextMismatch,
    #[error("the configured provider, exact model, endpoint, or request profile differs from the committed route")]
    ModelRouteMismatch,
    #[error("the pending MCP server identity cannot be verified from this turn's journal")]
    McpIdentityUnverified,
    #[error("the prepared MCP server identity differs from the interrupted turn")]
    McpIdentityMismatch,
    #[error("the interrupted turn has no verified command hook policy snapshot")]
    HooksUnverified,
    #[error("the configured command hook policy differs from the interrupted turn")]
    HooksMismatch,
    #[error("the interrupted turn has no verified command permission policy snapshot")]
    ExecPolicyUnverified,
    #[error("the configured command permission policy differs from the interrupted turn")]
    ExecPolicyMismatch,
    #[error("recovery state changed before the requested resolution")]
    StaleResolution,
    #[error("event sequence exhausted")]
    SequenceExhausted,
    #[error("manual tool result exceeds the configured output limit")]
    OversizedManualResult,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResumeDisposition {
    Complete,
    Advanced,
    /// Re-prompt this same durable request ID. No earlier choice is assumed.
    NeedsPermission {
        request_id: String,
        tool_call: ToolCall,
    },
    /// The tool may already have had an external effect. Never retry it here.
    ToolOutcomeUncertain {
        tool_call: ToolCall,
        executor_error: Option<String>,
    },
    /// A failed or cancelled batch needs human disposition.
    AbortedToolBatch {
        next_tool_call: ToolCall,
    },
    /// A terminal turn is never sent back to the model.
    AbortedTurn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResumeStep {
    pub replay: NativeReplay,
    pub appended_events: Vec<AgentEvent>,
    pub disposition: ResumeDisposition,
}

const MAX_MANUAL_TOOL_RESULT_CHARS: usize = 16_000;

fn load_manual_replay(
    sessions: &SessionStore,
    checkpoints: &CheckpointFileStore,
    lease: &NativeSessionLease,
) -> Result<(StoredSession, CheckpointCompatibility, NativeReplay), ResumeError> {
    let record = sessions
        .load(lease.session_id())?
        .ok_or(ResumeError::SessionMissing)?;
    if record.session.backend != "native" {
        return Err(ResumeError::NonNativeSession);
    }
    let compatibility = record
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
            provider: "disposition-only".into(),
            model: "disposition-only".into(),
            context_format_key: "disposition-only".into(),
        });
    let replay = replay_native(
        lease.session_id(),
        &record.events,
        Some(checkpoints),
        &compatibility,
    )?;
    Ok((record, compatibility, replay))
}

/// Persist an independently verified outcome for a tool that may have run
/// before a crash. This never executes the tool or contacts the model. The
/// lease must be held; provider credentials and current settings are not used.
pub fn record_verified_tool_outcome(
    sessions: &SessionStore,
    checkpoints: &CheckpointFileStore,
    lease: &NativeSessionLease,
    call_id: &str,
    result: ToolOutput,
    max_output_chars: usize,
) -> Result<ResumeStep, ResumeError> {
    if max_output_chars == 0 || max_output_chars > MAX_MANUAL_TOOL_RESULT_CHARS {
        return Err(AgentError::InvalidRequest(format!(
            "manual tool result limit must be between 1 and {MAX_MANUAL_TOOL_RESULT_CHARS} characters"
        ))
        .into());
    }
    let (mut record, compatibility, replay) = load_manual_replay(sessions, checkpoints, lease)?;
    let ReplayRecovery::ToolOutcomeUncertain { tool_call } = &replay.recovery else {
        return Err(ResumeError::StaleResolution);
    };
    if tool_call.id != call_id {
        return Err(ResumeError::StaleResolution);
    }
    if result.content.chars().count() > max_output_chars {
        return Err(ResumeError::OversizedManualResult);
    }
    let sequence = replay
        .last_sequence
        .checked_add(1)
        .ok_or(ResumeError::SequenceExhausted)?;
    let event = AgentEvent::new(
        lease.session_id(),
        sequence,
        AgentEventKind::ToolFinished {
            tool_call_id: call_id.to_owned(),
            output: result.content,
            is_error: result.is_error,
        },
    );
    record.events.push(event.clone());
    let updated = replay_native(
        lease.session_id(),
        &record.events,
        Some(checkpoints),
        &compatibility,
    )?;
    let terminated = updated
        .active_turn
        .as_ref()
        .is_some_and(|turn| turn.terminated);
    let disposition = match &updated.recovery {
        ReplayRecovery::Idle if updated.can_start_new_turn => ResumeDisposition::Complete,
        ReplayRecovery::AbortedToolBatch { next_tool_call } if terminated => {
            ResumeDisposition::AbortedToolBatch {
                next_tool_call: next_tool_call.clone(),
            }
        }
        ReplayRecovery::ToolOutcomeUncertain { tool_call } if tool_call.id != call_id => {
            ResumeDisposition::ToolOutcomeUncertain {
                tool_call: tool_call.clone(),
                executor_error: None,
            }
        }
        ReplayRecovery::ToolReady { .. } | ReplayRecovery::ReadyForModel { .. } if !terminated => {
            ResumeDisposition::Advanced
        }
        _ => return Err(ResumeError::StaleResolution),
    };
    sessions.append_native_event(&event)?;
    Ok(ResumeStep {
        replay: updated,
        appended_events: vec![event],
        disposition,
    })
}

/// Record a human decision to abandon the remaining tool calls of a failed
/// or cancelled turn. This route needs no model, provider credentials, tool
/// executor, or current settings. The session lease must already be held.
pub fn discard_aborted_tool_batch(
    sessions: &SessionStore,
    checkpoints: &CheckpointFileStore,
    lease: &NativeSessionLease,
    call_id: &str,
) -> Result<ResumeStep, ResumeError> {
    let (mut record, compatibility, replay) = load_manual_replay(sessions, checkpoints, lease)?;
    let ReplayRecovery::AbortedToolBatch { next_tool_call } = &replay.recovery else {
        return Err(ResumeError::StaleResolution);
    };
    if call_id != next_tool_call.id {
        return Err(ResumeError::StaleResolution);
    }
    let sequence = replay
        .last_sequence
        .checked_add(1)
        .ok_or(ResumeError::SequenceExhausted)?;
    let event = AgentEvent::new(
        lease.session_id(),
        sequence,
        AgentEventKind::ToolBatchDiscarded {
            next_tool_call_id: call_id.to_owned(),
        },
    );
    record.events.push(event.clone());
    let updated = replay_native(
        lease.session_id(),
        &record.events,
        Some(checkpoints),
        &compatibility,
    )?;
    if updated.recovery != ReplayRecovery::Idle || !updated.can_start_new_turn {
        return Err(ResumeError::StaleResolution);
    }
    sessions.append_native_event(&event)?;
    Ok(ResumeStep {
        replay: updated,
        appended_events: vec![event],
        disposition: ResumeDisposition::Complete,
    })
}

/// Receives native events only after their append has succeeded. Observation
/// does not change the journal's synchronization policy: text deltas use the
/// same append contract as a normal native run. A failed observer stops the
/// current recovery; the committed prefix remains available for manual Resume.
#[async_trait]
pub trait ResumeEventObserver: Send + Sync {
    fn on_persisted_event(&self, event: &AgentEvent) -> Result<(), AgentError>;

    /// A bounded UI can wait for capacity without preventing the model future
    /// from being dropped on cancellation. The unsent persisted event is
    /// delivered synchronously only after that future has been dropped.
    async fn on_persisted_stream_event(
        &self,
        event: &AgentEvent,
        _cancel: &CancellationToken,
    ) -> Result<(), AgentError> {
        self.on_persisted_event(event)
    }
}

pub struct ResumeEngine {
    sessions: Arc<SessionStore>,
    checkpoints: Arc<CheckpointFileStore>,
    compatibility: CheckpointCompatibility,
    model: Arc<dyn ModelBackend>,
    tools: Arc<dyn ToolExecutor>,
    /// Must be the caller's validated, frozen advertised catalog.
    catalog: ToolCatalog,
    permission_policy: PermissionPolicy,
    budget_policy: Option<TokenBudgetPolicy>,
    user_interaction: bool,
    limits: RunLimits,
    system_instructions: String,
    native_runtime_identity: Option<NativeRuntimeIdentity>,
    mcp_runtime_identity_digest: Option<String>,
    command_hooks: Arc<CommandHooks>,
    session_approvals: Option<(Arc<SessionStore>, CommandIsolation)>,
    event_observer: Option<Arc<dyn ResumeEventObserver>>,
    manual_command_actor: Option<Arc<NativeManualCommandActor>>,
}

impl ResumeEngine {
    /// The caller must supply the same model/provider contract and a validated
    /// catalog and permission policy. Neither is inferred from the executor.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sessions: Arc<SessionStore>,
        checkpoints: Arc<CheckpointFileStore>,
        compatibility: CheckpointCompatibility,
        model: Arc<dyn ModelBackend>,
        tools: Arc<dyn ToolExecutor>,
        catalog: ToolCatalog,
        permission_policy: PermissionPolicy,
        budget_policy: impl Into<Option<TokenBudgetPolicy>>,
    ) -> Self {
        Self {
            sessions,
            checkpoints,
            compatibility,
            model,
            tools,
            catalog,
            permission_policy,
            budget_policy: budget_policy.into(),
            limits: RunLimits::default(),
            system_instructions: String::new(),
            native_runtime_identity: None,
            mcp_runtime_identity_digest: None,
            command_hooks: Arc::new(CommandHooks::default()),
            session_approvals: None,
            event_observer: None,
            manual_command_actor: None,
            user_interaction: false,
        }
    }

    /// Recover turns that were offered the built-in tools that wait for the
    /// user. A turn admitted before they existed keeps its own catalog.
    pub fn with_user_interaction(mut self, enabled: bool) -> Self {
        self.user_interaction = enabled;
        self
    }

    /// Drain confirmed human process controls under the retained recovery lease.
    pub fn with_manual_command_actor(mut self, actor: Arc<NativeManualCommandActor>) -> Self {
        self.manual_command_actor = Some(actor);
        self
    }

    pub fn with_limits(mut self, limits: RunLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Forward the exact committed events of this engine as they occur. The
    /// returned ResumeStep still contains those events for non-observing callers.
    pub fn with_event_observer(mut self, observer: Arc<dyn ResumeEventObserver>) -> Self {
        self.event_observer = Some(observer);
        self
    }

    pub fn with_system_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.system_instructions = instructions.into();
        self
    }

    pub fn with_native_runtime_identity(
        mut self,
        provider: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        self.native_runtime_identity = Some(NativeRuntimeIdentity::new(provider, endpoint));
        self
    }

    /// Identity of all currently prepared MCP servers, verified by the host.
    /// None leaves MCP calls available for a live turn but not auto-resumable.
    pub fn with_mcp_runtime_identity_digest(mut self, digest: Option<String>) -> Self {
        self.mcp_runtime_identity_digest = digest;
        self
    }

    pub fn with_command_hooks(mut self, hooks: Arc<CommandHooks>) -> Self {
        self.command_hooks = hooks;
        self
    }

    /// Enable durable approval reuse for this native session. Pass the same
    /// store used by this recovery engine and the active command isolation.
    pub fn with_session_approvals(
        mut self,
        sessions: Arc<SessionStore>,
        isolation: CommandIsolation,
    ) -> Self {
        self.session_approvals = Some((sessions, isolation));
        self
    }

    pub fn inspect(&self, lease: &NativeSessionLease) -> Result<NativeReplay, ResumeError> {
        let record = self
            .sessions
            .load(lease.session_id())?
            .ok_or(ResumeError::SessionMissing)?;
        if record.session.backend != "native" {
            return Err(ResumeError::NonNativeSession);
        }
        let replay = replay_native(
            lease.session_id(),
            &record.events,
            Some(&self.checkpoints),
            &self.compatibility,
        )?;
        if self.compatibility.model != self.model.model_id()
            || replay
                .active_turn
                .as_ref()
                .and_then(|turn| turn.model.as_deref())
                .is_some_and(|model| model != self.model.model_id())
        {
            return Err(ResumeError::ModelMismatch);
        }
        if let Some(turn) = &replay.active_turn {
            if let Some(saved_route) = &replay.active_model_route {
                let current = self
                    .native_runtime_identity
                    .as_ref()
                    .ok_or(ResumeError::ContextUnverified)?
                    .route_snapshot(
                        self.model.model_id(),
                        self.model
                            .request_profile_fingerprint()
                            .ok_or(ResumeError::ContextUnverified)?,
                    )?;
                if &current != saved_route {
                    return Err(ResumeError::ModelRouteMismatch);
                }
            }
            match replay.active_exec_policy_digest.as_deref() {
                Some(saved) => {
                    if replay.active_exec_policy_version != Some(EXEC_POLICY_IDENTITY_VERSION) {
                        return Err(ResumeError::ExecPolicyUnverified);
                    }
                    if self.permission_policy.digest_hex() != saved
                        && !matches!(
                            replay.recovery,
                            ReplayRecovery::ToolOutcomeUncertain { .. }
                                | ReplayRecovery::AbortedToolBatch { .. }
                        )
                    {
                        return Err(ResumeError::ExecPolicyMismatch);
                    }
                }
                None if (turn.last_step > 0 || replay.active_model_context_digest.is_some())
                    && self.catalog_for_mode(turn.mode)?.definitions().iter().any(
                        |definition| {
                            matches!(definition.name.as_str(), "run_command" | "start_command")
                        },
                    ) =>
                {
                    return Err(ResumeError::ExecPolicyUnverified);
                }
                None => {}
            }
            if let Some(saved) = replay.active_hooks_digest.as_deref() {
                if replay.active_hooks_version != Some(HOOK_CONFIG_IDENTITY_VERSION) {
                    return Err(ResumeError::HooksUnverified);
                }
                let current = self
                    .command_hooks
                    .digest_hex()
                    .map_err(|_| ResumeError::HooksUnverified)?;
                if current != saved
                    && !matches!(
                        replay.recovery,
                        ReplayRecovery::ToolOutcomeUncertain { .. }
                            | ReplayRecovery::AbortedToolBatch { .. }
                    )
                {
                    return Err(ResumeError::HooksMismatch);
                }
            }
            if let Some(saved) = replay.active_mcp_runtime_digest.as_deref() {
                if replay.active_mcp_runtime_version != Some(MCP_RUNTIME_IDENTITY_VERSION) {
                    return Err(ResumeError::McpIdentityUnverified);
                }
                if self.mcp_runtime_identity_digest.as_deref() != Some(saved) {
                    return Err(ResumeError::McpIdentityMismatch);
                }
            }
            match replay.active_model_context_digest.as_deref() {
                Some(saved) => {
                    if replay.active_model_context_version != Some(MODEL_CONTEXT_VERSION) {
                        return Err(ResumeError::ContextVersionUnsupported);
                    }
                    let catalog = self.verified_catalog(&replay)?;
                    let record_workspace =
                        record.session.workspace.canonicalize().map_err(|error| {
                            AgentError::InvalidRequest(format!("workspace: {error}"))
                        })?;
                    let system = native_system_prompt_with_plan(
                        &record_workspace,
                        catalog.definitions(),
                        turn.mode,
                        replay
                            .active_system_instructions
                            .as_deref()
                            .unwrap_or(&self.system_instructions),
                        self.plan_context_for_catalog(&catalog, &replay),
                    );
                    let identity = self
                        .native_runtime_identity
                        .as_ref()
                        .ok_or(ResumeError::ContextUnverified)?;
                    let request_profile = self
                        .model
                        .request_profile_fingerprint()
                        .ok_or(ResumeError::ContextUnverified)?;
                    let current = identity.digest_hex(
                        self.model.model_id(),
                        turn.mode,
                        &record_workspace,
                        &catalog.digest_hex(),
                        &system,
                        request_profile,
                    )?;
                    if current != saved {
                        return Err(ResumeError::ContextMismatch);
                    }
                }
                None if turn.last_step > 0 => return Err(ResumeError::ContextUnverified),
                None => {}
            }
        }
        match &self.manual_command_actor {
            Some(actor) => {
                if !Arc::ptr_eq(actor.sessions(), &self.sessions) {
                    return Err(AgentError::InvalidRequest(
                        "manual command actor and recovery use different stores".into(),
                    )
                    .into());
                }
                actor.validate_lease(lease)?;
            }
            None => NativeCommandOperationStore::new(self.sessions.clone())
                .require_no_pending(lease)
                .map_err(|error| AgentError::EffectUncertain(error.to_string()))?,
        }
        Ok(replay)
    }

    /// Run one durable action. On a model retry, reuse the existing
    /// ModelRequestStarted event and step. On a ready tool, sync ToolStarted
    /// before invoking its executor. This method never admits another turn.
    pub async fn resume_step(
        &self,
        lease: &NativeSessionLease,
        cancel: CancellationToken,
    ) -> Result<ResumeStep, ResumeError> {
        let replay = self.inspect(lease)?;
        self.process_manual_commands(lease, &cancel).await?;
        let mut events = Vec::new();
        let mut executor_error = None;
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled.into());
        }
        if replay
            .active_turn
            .as_ref()
            .is_some_and(|turn| turn.terminated)
            && !matches!(
                replay.recovery,
                ReplayRecovery::ToolOutcomeUncertain { .. }
                    | ReplayRecovery::AbortedToolBatch { .. }
            )
        {
            return self.finish_step(lease, events, None);
        }
        match &replay.recovery {
            ReplayRecovery::Idle => {}
            ReplayRecovery::AdmittedNotStarted => {
                let record = self
                    .sessions
                    .load(lease.session_id())?
                    .ok_or(ResumeError::SessionMissing)?;
                if !record
                    .events
                    .iter()
                    .any(|event| matches!(event.kind, AgentEventKind::SessionStarted { .. }))
                {
                    self.append(
                        lease,
                        replay.last_sequence,
                        &mut events,
                        AgentEventKind::SessionStarted {
                            workspace: record.session.workspace,
                        },
                    )?;
                }
                let turn = replay.active_turn.as_ref().expect("admitted turn exists");
                self.append(
                    lease,
                    replay.last_sequence,
                    &mut events,
                    AgentEventKind::TurnStarted {
                        prompt: turn.prompt.clone(),
                    },
                )?;
            }
            ReplayRecovery::ReadyForModel { next_step } => {
                self.run_model(lease, &replay, *next_step, true, cancel, &mut events)
                    .await?;
            }
            ReplayRecovery::ModelInFlight { step, model } => {
                if model != self.model.model_id() {
                    return Err(ResumeError::ModelMismatch);
                }
                self.run_model(lease, &replay, *step, false, cancel, &mut events)
                    .await?;
            }
            ReplayRecovery::ResponseCommitted { step } => {
                let record = self
                    .sessions
                    .load(lease.session_id())?
                    .ok_or(ResumeError::SessionMissing)?;
                let response = record
                    .events
                    .iter()
                    .rev()
                    .find_map(|event| match &event.kind {
                        AgentEventKind::ModelResponseCommitted {
                            step: committed,
                            assistant,
                            ..
                        } if committed == step => Some(assistant.content.clone()),
                        AgentEventKind::AssistantMessage { content } => Some(content.clone()),
                        _ => None,
                    })
                    .ok_or(ResumeError::StaleResolution)?;
                let response_start = record.events.iter().rposition(|event| {
                    matches!(&event.kind, AgentEventKind::ModelResponseCommitted { step: committed, .. } if committed == step)
                }).unwrap_or(0);
                if !record.events[response_start..]
                    .iter()
                    .any(|event| matches!(event.kind, AgentEventKind::AssistantMessage { .. }))
                {
                    self.append(
                        lease,
                        replay.last_sequence,
                        &mut events,
                        AgentEventKind::AssistantMessage {
                            content: response.clone(),
                        },
                    )?;
                }
                if replay
                    .active_turn
                    .as_ref()
                    .is_some_and(|turn| turn.mode == TurnMode::Plan)
                {
                    let start = record
                        .events
                        .iter()
                        .rposition(|event| matches!(event.kind, AgentEventKind::TurnStarted { .. }))
                        .unwrap_or(0);
                    if !record.events[start..]
                        .iter()
                        .any(|event| matches!(event.kind, AgentEventKind::PlanProposed { .. }))
                    {
                        self.append(
                            lease,
                            replay.last_sequence,
                            &mut events,
                            AgentEventKind::PlanProposed {
                                plan: PlanSnapshot::from_plan_response(&response),
                            },
                        )?;
                    }
                }
                self.finish_model_response(lease, &replay, &mut events, response, *step, &cancel)
                    .await?;
            }
            ReplayRecovery::PermissionPending { tool_call, .. } => {
                self.require_stable_tool_identity(&replay, tool_call)?;
                self.require_stable_hooks_identity(&replay)?;
                self.verified_catalog(&replay)?;
            }
            ReplayRecovery::ToolOutcomeUncertain { .. }
            | ReplayRecovery::AbortedToolBatch { .. } => {}
            ReplayRecovery::ToolReady { tool_call } => {
                executor_error = self
                    .run_tool(lease, &replay, tool_call, cancel, &mut events)
                    .await?;
            }
        }
        self.finish_step(lease, events, executor_error)
    }

    /// Continue until terminal or a human decision is required. Model and
    /// tool limits also bound the number of iterations.
    pub async fn run_until_pause(
        &self,
        lease: &NativeSessionLease,
        cancel: CancellationToken,
    ) -> Result<ResumeStep, ResumeError> {
        let mut all_events = Vec::new();
        for _ in 0..self
            .limits
            .max_model_steps
            .saturating_add(self.limits.max_tool_calls)
            .saturating_add(4)
        {
            let mut step = self.resume_step(lease, cancel.clone()).await?;
            all_events.append(&mut step.appended_events);
            if step.disposition != ResumeDisposition::Advanced {
                step.appended_events = all_events;
                return Ok(step);
            }
        }
        Err(AgentError::Limit("recovery exceeded configured step limit".into()).into())
    }

    /// Resolve only the request still pending in the verified log. The same
    /// ID is shown again after a crash; one synced resolution wins.
    pub fn resolve_permission(
        &self,
        lease: &NativeSessionLease,
        request_id: &str,
        choice: PermissionChoice,
    ) -> Result<ResumeStep, ResumeError> {
        let replay = self.inspect(lease)?;
        let ReplayRecovery::PermissionPending {
            request_id: pending,
            tool_call,
        } = &replay.recovery
        else {
            return Err(ResumeError::StaleResolution);
        };
        if pending != request_id {
            return Err(ResumeError::StaleResolution);
        }
        self.require_stable_tool_identity(&replay, tool_call)?;
        self.require_stable_hooks_identity(&replay)?;
        let catalog = self.verified_catalog(&replay)?;
        let resolution = match choice {
            PermissionChoice::Allow => PermissionResolution::Allowed,
            PermissionChoice::Deny => PermissionResolution::Denied,
            PermissionChoice::AllowSession => {
                let (Some((sessions, _)), Some(digest)) = (
                    self.session_approvals.as_ref(),
                    self.session_approval_digest(&replay, &catalog)?,
                ) else {
                    return Err(AgentError::InvalidRequest(
                        "session approvals require a matching session store and command isolation"
                            .into(),
                    )
                    .into());
                };
                if !Arc::ptr_eq(sessions, &self.sessions)
                    || (tool_call.name.starts_with("mcp__")
                        && replay.active_mcp_runtime_digest.is_none())
                {
                    return Err(AgentError::InvalidRequest(
                        "session approval runtime identity is unverified".into(),
                    )
                    .into());
                }
                let record = self
                    .sessions
                    .load(lease.session_id())?
                    .ok_or(ResumeError::SessionMissing)?;
                let workspace =
                    record.session.workspace.canonicalize().map_err(|error| {
                        AgentError::InvalidRequest(format!("workspace: {error}"))
                    })?;
                let grant = self
                    .permission_policy
                    .issue_session_grant(
                        lease.session_id(),
                        &workspace,
                        &workspace,
                        &digest,
                        tool_call,
                    )
                    .map_err(|error| {
                        AgentError::InvalidRequest(format!(
                            "cannot issue session approval: {error}"
                        ))
                    })?;
                sessions.record_permission_decision_with_grant(
                    lease.session_id(),
                    pending,
                    PermissionDecision::Allow,
                    Some(grant.into()),
                )?;
                PermissionResolution::Allowed
            }
        };
        let mut events = Vec::new();
        self.append(
            lease,
            replay.last_sequence,
            &mut events,
            AgentEventKind::PermissionResolved {
                request_id: pending.clone(),
                tool_call_id: tool_call.id.clone(),
                resolution,
            },
        )?;
        self.finish_step(lease, events, None)
    }

    /// Caller assertion: the external effect and result were independently
    /// verified. No tool is rerun. A mismatched/stale call ID is rejected.
    pub fn record_verified_tool_outcome(
        &self,
        lease: &NativeSessionLease,
        tool_call_id: &str,
        result: ToolOutput,
    ) -> Result<ResumeStep, ResumeError> {
        let step = crate::resume::record_verified_tool_outcome(
            &self.sessions,
            &self.checkpoints,
            lease,
            tool_call_id,
            result,
            self.limits.max_tool_output_chars,
        )?;
        for event in &step.appended_events {
            self.observe(event)?;
        }
        Ok(step)
    }

    pub fn discard_aborted_tool_batch(
        &self,
        lease: &NativeSessionLease,
        call_id: &str,
    ) -> Result<ResumeStep, ResumeError> {
        let step = discard_aborted_tool_batch(&self.sessions, &self.checkpoints, lease, call_id)?;
        for event in &step.appended_events {
            self.observe(event)?;
        }
        Ok(step)
    }

    fn observe(&self, event: &AgentEvent) -> Result<(), ResumeError> {
        if let Some(observer) = &self.event_observer {
            observer.on_persisted_event(event)?;
        }
        Ok(())
    }

    fn append(
        &self,
        lease: &NativeSessionLease,
        base_sequence: u64,
        events: &mut Vec<AgentEvent>,
        kind: AgentEventKind,
    ) -> Result<(), ResumeError> {
        let sequence = match events.last() {
            Some(last) => last.sequence,
            None => base_sequence,
        }
        .checked_add(1)
        .ok_or(ResumeError::SequenceExhausted)?;
        let event = AgentEvent::new(lease.session_id(), sequence, kind);
        self.sessions.append_native_event(&event)?;
        events.push(event);
        self.observe(events.last().expect("appended event"))
    }

    async fn append_stream_update(
        &self,
        lease: &NativeSessionLease,
        base_sequence: u64,
        events: &mut Vec<AgentEvent>,
        kind: AgentEventKind,
        cancel: &CancellationToken,
    ) -> Result<(), ResumeError> {
        let sequence = events
            .last()
            .map_or(base_sequence, |event| event.sequence)
            .checked_add(1)
            .ok_or(ResumeError::SequenceExhausted)?;
        let event = AgentEvent::new(lease.session_id(), sequence, kind);
        self.sessions.append_native_event(&event)?;
        events.push(event);
        if let Some(observer) = &self.event_observer {
            observer
                .on_persisted_stream_event(events.last().expect("appended stream update"), cancel)
                .await?;
        }
        Ok(())
    }

    async fn process_manual_commands(
        &self,
        lease: &NativeSessionLease,
        cancel: &CancellationToken,
    ) -> Result<(), ResumeError> {
        if let Some(actor) = &self.manual_command_actor {
            actor.process_pending(lease, cancel).await?;
        }
        Ok(())
    }

    async fn manual_command_notified(&self) {
        match &self.manual_command_actor {
            Some(actor) => actor.notified().await,
            None => std::future::pending::<()>().await,
        }
    }

    /// The store admission fence can lose a race to a newly queued intention.
    /// Drain it and retry the same append, never the model or an external effect.
    async fn append_manual_boundary(
        &self,
        lease: &NativeSessionLease,
        base_sequence: u64,
        events: &mut Vec<AgentEvent>,
        kind: AgentEventKind,
        cancel: &CancellationToken,
    ) -> Result<(), ResumeError> {
        loop {
            self.process_manual_commands(lease, cancel).await?;
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled.into());
            }
            match self.append(lease, base_sequence, events, kind.clone()) {
                Err(ResumeError::Store(StoreError::NativeCommandOperationAdmissionBlocked(_)))
                    if self.manual_command_actor.is_some() =>
                {
                    tokio::task::yield_now().await
                }
                result => return result,
            }
        }
    }

    /// Commit queued user inputs only at a replay-safe model boundary. The
    /// returned messages are added to the next provider request immediately;
    /// replay reconstructs the same messages after a crash.
    fn drain_pending_steers(
        &self,
        lease: &NativeSessionLease,
        replay: &NativeReplay,
        events: &mut Vec<AgentEvent>,
    ) -> Result<Vec<AgentMessage>, ResumeError> {
        let Some(turn_id) = replay
            .active_turn
            .as_ref()
            .and_then(|turn| turn.turn_id.as_deref())
        else {
            return Ok(Vec::new());
        };
        let pending = self
            .sessions
            .pending_native_steers(lease.session_id(), turn_id)?;
        let mut messages = Vec::with_capacity(pending.len());
        for input in pending {
            let sequence = events
                .last()
                .map_or(replay.last_sequence, |event| event.sequence)
                .checked_add(1)
                .ok_or(ResumeError::SequenceExhausted)?;
            let event =
                self.sessions
                    .ack_native_steer(lease, turn_id, &input.input_id, sequence)?;
            let message = match &event.kind {
                AgentEventKind::TurnSteered { text, .. } => AgentMessage::user(text.clone()),
                AgentEventKind::TurnSteeredWithImages { text, images, .. } => {
                    AgentMessage::user_with_images(text.clone(), images.clone()).map_err(
                        |error| AgentError::InvalidRequest(format!("steering images: {error}")),
                    )?
                }
                _ => return Err(ResumeError::StaleResolution),
            };
            messages.push(message);
            events.push(event);
            self.observe(events.last().expect("committed steer event"))?;
        }
        Ok(messages)
    }

    async fn finish_model_response(
        &self,
        lease: &NativeSessionLease,
        replay: &NativeReplay,
        events: &mut Vec<AgentEvent>,
        response: String,
        steps: usize,
        cancel: &CancellationToken,
    ) -> Result<(), ResumeError> {
        self.process_manual_commands(lease, cancel).await?;
        if !self.drain_pending_steers(lease, replay, events)?.is_empty() {
            return Ok(());
        }
        if let Some(turn_id) = replay
            .active_turn
            .as_ref()
            .and_then(|turn| turn.turn_id.as_deref())
        {
            match self.sessions.fence_native_turn_completion(lease, turn_id) {
                Ok(()) => {}
                Err(StoreError::PendingNativeSteerInput) => {
                    self.drain_pending_steers(lease, replay, events)?;
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.append_manual_boundary(
            lease,
            replay.last_sequence,
            events,
            AgentEventKind::TurnCompleted { response, steps },
            cancel,
        )
        .await
    }

    fn finish_step(
        &self,
        lease: &NativeSessionLease,
        appended_events: Vec<AgentEvent>,
        executor_error: Option<String>,
    ) -> Result<ResumeStep, ResumeError> {
        let replay = self.inspect(lease)?;
        let disposition = match &replay.recovery {
            ReplayRecovery::Idle => ResumeDisposition::Complete,
            ReplayRecovery::PermissionPending {
                request_id,
                tool_call,
            } => ResumeDisposition::NeedsPermission {
                request_id: request_id.clone(),
                tool_call: tool_call.clone(),
            },
            ReplayRecovery::ToolOutcomeUncertain { tool_call } => {
                ResumeDisposition::ToolOutcomeUncertain {
                    tool_call: tool_call.clone(),
                    executor_error,
                }
            }
            ReplayRecovery::AbortedToolBatch { next_tool_call } => {
                ResumeDisposition::AbortedToolBatch {
                    next_tool_call: next_tool_call.clone(),
                }
            }
            _ if replay
                .active_turn
                .as_ref()
                .is_some_and(|turn| turn.terminated) =>
            {
                ResumeDisposition::AbortedTurn
            }
            _ => ResumeDisposition::Advanced,
        };
        Ok(ResumeStep {
            replay,
            appended_events,
            disposition,
        })
    }

    async fn run_model(
        &self,
        lease: &NativeSessionLease,
        replay: &NativeReplay,
        step: usize,
        start_request: bool,
        cancel: CancellationToken,
        events: &mut Vec<AgentEvent>,
    ) -> Result<(), ResumeError> {
        if step == 0 || step > self.limits.max_model_steps {
            return Err(AgentError::Limit("recovery exceeded model step limit".into()).into());
        }
        let current_route = self
            .native_runtime_identity
            .as_ref()
            .ok_or(ResumeError::ContextUnverified)?
            .route_snapshot(
                self.model.model_id(),
                self.model
                    .request_profile_fingerprint()
                    .ok_or(ResumeError::ContextUnverified)?,
            )?;
        if replay
            .active_model_route
            .as_ref()
            .is_some_and(|saved| saved != &current_route)
        {
            return Err(ResumeError::ModelRouteMismatch);
        }
        let turn = replay
            .active_turn
            .as_ref()
            .ok_or(ResumeError::StaleResolution)?;
        let catalog = if start_request && step == 1 && replay.active_tool_catalog_digest.is_none() {
            let catalog = self.catalog_generations(turn.mode)?.swap_remove(0);
            self.append(
                lease,
                replay.last_sequence,
                events,
                AgentEventKind::ToolCatalogCommitted {
                    digest: catalog.digest_hex(),
                },
            )?;
            catalog
        } else {
            self.verified_catalog(replay)?
        };
        if turn.mode == TurnMode::Build
            && replay.active_mcp_runtime_digest.is_none()
            && replay.active_model_context_digest.is_none()
            && start_request
            && step == 1
        {
            if let Some(digest) = self.mcp_runtime_identity_digest.as_deref() {
                self.append(
                    lease,
                    replay.last_sequence,
                    events,
                    AgentEventKind::McpRuntimeCommitted {
                        version: MCP_RUNTIME_IDENTITY_VERSION,
                        digest: digest.to_owned(),
                    },
                )?;
            }
        }
        if start_request
            && step == 1
            && replay.active_hooks_digest.is_none()
            && replay.active_model_context_digest.is_none()
        {
            let digest = self
                .command_hooks
                .digest_hex()
                .map_err(|_| ResumeError::HooksUnverified)?;
            self.append(
                lease,
                replay.last_sequence,
                events,
                AgentEventKind::HooksCommitted {
                    version: HOOK_CONFIG_IDENTITY_VERSION,
                    digest,
                },
            )?;
        }
        if start_request
            && step == 1
            && replay.active_exec_policy_digest.is_none()
            && replay.active_model_context_digest.is_none()
        {
            self.append(
                lease,
                replay.last_sequence,
                events,
                AgentEventKind::ExecPolicyCommitted {
                    version: EXEC_POLICY_IDENTITY_VERSION,
                    digest: self.permission_policy.digest_hex(),
                },
            )?;
        }
        let record = self
            .sessions
            .load(lease.session_id())?
            .ok_or(ResumeError::SessionMissing)?;
        let workspace = record
            .session
            .workspace
            .canonicalize()
            .map_err(|error| AgentError::InvalidRequest(format!("workspace: {error}")))?;
        let turn_start_sequence = record
            .events
            .iter()
            .rev()
            .find_map(|event| {
                matches!(event.kind, AgentEventKind::TurnStarted { .. }).then_some(event.sequence)
            })
            .ok_or(ResumeError::StaleResolution)?;
        let scoped_context =
            ProjectInstructionLedger::from_session(&record, turn_start_sequence, &workspace)?
                .and_then(|ledger| ledger.model_context());
        let system_instructions = replay
            .active_system_instructions
            .as_deref()
            .unwrap_or(&self.system_instructions);
        if replay.active_system_instructions.is_none()
            && replay.active_model_context_digest.is_none()
            && start_request
            && step == 1
        {
            let digest = system_instructions_digest(system_instructions).ok_or_else(|| {
                AgentError::InvalidRequest(format!(
                    "system instructions exceed {MAX_SYSTEM_INSTRUCTIONS_BYTES} bytes"
                ))
            })?;
            self.append(
                lease,
                replay.last_sequence,
                events,
                AgentEventKind::SystemInstructionsCommitted {
                    version: SYSTEM_INSTRUCTIONS_VERSION,
                    digest,
                    content: system_instructions.to_owned(),
                },
            )?;
        }
        let system = native_system_prompt_with_plan(
            &workspace,
            catalog.definitions(),
            turn.mode,
            system_instructions,
            self.plan_context_for_catalog(&catalog, replay),
        );
        let identity = self
            .native_runtime_identity
            .as_ref()
            .ok_or(ResumeError::ContextUnverified)?;
        let request_profile = self
            .model
            .request_profile_fingerprint()
            .ok_or(ResumeError::ContextUnverified)?;
        let context_digest = identity.digest_hex(
            self.model.model_id(),
            turn.mode,
            &workspace,
            &catalog.digest_hex(),
            &system,
            request_profile,
        )?;
        if replay.active_model_context_digest.is_none() {
            self.append(
                lease,
                replay.last_sequence,
                events,
                AgentEventKind::ModelContextCommitted {
                    version: MODEL_CONTEXT_VERSION,
                    digest: context_digest.clone(),
                },
            )?;
        }
        let mut request_history = replay.history.clone();
        if replay.active_model_route.is_none() && start_request && step == 1 && turn.last_step == 0
        {
            self.append(
                lease,
                replay.last_sequence,
                events,
                current_route.clone().into_event(),
            )?;
            // The first request may be recovered from an admission/context
            // commit that preceded its route marker. `append` does not rebuild
            // this borrowed replay, so reconcile the outgoing copy immediately
            // after publishing the new route. Legacy in-flight retries keep
            // their original payloads and never gain retrospective attribution.
            openide_core::reconcile_model_history_route(
                &mut request_history,
                replay.history_model_route.as_ref(),
                &current_route,
            );
        }
        let mut messages = vec![AgentMessage::system(system.clone())];
        if let Some(context) = scoped_context.clone() {
            messages.push(context);
        }
        messages.extend(request_history);
        if start_request {
            messages.extend(self.drain_pending_steers(lease, replay, events)?);
        }
        if let Some(policy) = self.budget_policy {
            let mut state = TokenBudgetState::default();
            let assessment = policy
                .assess_model_request(
                    &mut state,
                    self.model.as_ref(),
                    &messages,
                    catalog.definitions(),
                    cancel.clone(),
                )
                .await?;
            if assessment.decision != TokenBudgetDecision::Continue {
                let limit_message = format!(
                "semantic compaction is required before recovery model request: projected {} tokens, threshold {}",
                if policy.context_window_tokens == 0 {
                    assessment.projected_input_tokens.saturating_add(policy.tool_schema_reserve_tokens)
                } else { assessment.projected_total_tokens }, assessment.auto_compact_threshold_tokens
            );
                let budget = CheckpointBudgetSnapshot {
                    input_limit_tokens: policy.input_limit_tokens,
                    context_window_tokens: policy.context_window_tokens,
                    estimated_input_tokens: assessment.estimated_input_tokens,
                    observed_input_tokens: record.events.iter().rev().find_map(|event| {
                        if let AgentEventKind::ModelResponseCommitted {
                            usage: Some(usage), ..
                        } = &event.kind
                        {
                            Some(usage.input_tokens)
                        } else {
                            None
                        }
                    }),
                    output_reserve_tokens: policy.output_reserve_tokens,
                    tool_schema_reserve_tokens: policy.tool_schema_reserve_tokens,
                    limits_unverified: false,
                };
                let coordinator = CheckpointCoordinator::new(
                    self.sessions.clone(),
                    self.checkpoints.clone(),
                    self.model.clone(),
                    self.compatibility.clone(),
                    budget,
                );
                let committed = match coordinator
                    .compact_completed_prefix(lease, cancel.clone())
                    .await
                {
                    Ok(committed) => committed,
                    Err(CheckpointCoordinatorError::NoNewHistory) => {
                        return Err(AgentError::Limit(limit_message).into());
                    }
                    Err(CheckpointCoordinatorError::Compact(error)) => return Err(error.into()),
                    Err(error) => {
                        return Err(AgentError::Runtime(format!(
                            "semantic compaction failed during recovery: {error}"
                        ))
                        .into());
                    }
                };
                if committed.replay.recovery != replay.recovery
                    || committed.replay.active_turn != replay.active_turn
                    || committed.replay.active_tool_catalog_digest.as_deref()
                        != Some(catalog.digest_hex().as_str())
                    || committed.replay.active_model_context_digest.as_deref()
                        != Some(context_digest.as_str())
                    || committed.replay.active_model_context_version != Some(MODEL_CONTEXT_VERSION)
                {
                    return Err(AgentError::Runtime(
                        "compaction changed the interrupted turn or tool catalog".into(),
                    )
                    .into());
                }
                events.push(committed.marker_event);
                self.observe(events.last().expect("committed checkpoint marker"))?;
                messages = vec![AgentMessage::system(system)];
                if let Some(context) = scoped_context {
                    messages.push(context);
                }
                messages.extend(committed.replay.history);
                state.reset();
                let reassessment = policy
                    .assess_model_request(
                        &mut state,
                        self.model.as_ref(),
                        &messages,
                        catalog.definitions(),
                        cancel.clone(),
                    )
                    .await?;
                if reassessment.decision != TokenBudgetDecision::Continue {
                    return Err(AgentError::Limit(format!(
                    "recovery request still exceeds context after compaction: projected {} tokens, threshold {}",
                    reassessment.projected_total_tokens,
                    reassessment.auto_compact_threshold_tokens
                ))
                .into());
                }
            }
        }
        if start_request {
            self.append_manual_boundary(
                lease,
                replay.last_sequence,
                events,
                AgentEventKind::ModelRequestStarted {
                    step,
                    model: self.model.model_id().to_owned(),
                },
                &cancel,
            )
            .await?;
        }
        let (sender, mut receiver) = crate::model_event_channel(cancel.clone());
        let mut pending_delivery = None;
        let mut delivery_failed = false;
        let completion_result = {
            let future =
                self.model
                    .stream_events(&messages, catalog.definitions(), cancel.clone(), sender);
            tokio::pin!(future);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break Err(ResumeError::Agent(AgentError::Cancelled)),
                    result = &mut future => break result.map_err(ResumeError::from),
                    _ = self.manual_command_notified(), if self.manual_command_actor.is_some() => {
                        let actor = self.manual_command_actor.as_ref().expect("notified actor exists");
                        if let Err(error) = actor.process_pending(lease, &cancel).await {
                            break Err(ResumeError::from(error));
                        }
                    }
                    Some(update) = receiver.recv() => {
                        if let Some(kind) = crate::runner::stream_update_kind(step, update) {
                            let previous_len = events.len();
                            if let Err(error) = self.append_stream_update(
                                lease,
                                replay.last_sequence,
                                events,
                                kind,
                                &cancel,
                            ).await {
                                if matches!(&error, ResumeError::Agent(AgentError::Cancelled))
                                    && events.len() > previous_len
                                {
                                    pending_delivery = events.last().cloned();
                                } else {
                                    delivery_failed = true;
                                }
                                break Err(error);
                            }
                        }
                    }
                }
            }
        };
        // The backend future is now completed or dropped. Close the receiver
        // so detached producers cannot extend this attempt, then preserve the
        // prefix already accepted by the channel before exposing its error or
        // cancellation. Partial text never becomes a model response commit.
        receiver.close();
        if let Some(event) = pending_delivery {
            self.observe(&event)?;
        }
        if delivery_failed {
            return completion_result.map(|_| ());
        }
        while let Ok(update) = receiver.try_recv() {
            if let Some(kind) = crate::runner::stream_update_kind(step, update) {
                self.append(lease, replay.last_sequence, events, kind)?;
            }
        }
        let completion = completion_result?;
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled.into());
        }
        let mut assistant =
            AgentMessage::assistant(completion.output.content, completion.output.tool_calls);
        assistant.provider_output_items = completion.provider_output_items;
        if assistant.tool_calls.is_empty() && assistant.content.trim().is_empty() {
            return Err(
                AgentError::Model("model returned neither text nor a tool call".into()).into(),
            );
        }
        let seen = record
            .events
            .iter()
            .flat_map(|event| match &event.kind {
                AgentEventKind::ModelResponseCommitted { assistant, .. } => assistant
                    .tool_calls
                    .iter()
                    .map(|call| call.id.as_str())
                    .collect::<Vec<_>>(),
                AgentEventKind::PermissionRequested { tool_call, .. }
                | AgentEventKind::ToolStarted { tool_call } => vec![tool_call.id.as_str()],
                _ => Vec::new(),
            })
            .collect::<HashSet<_>>();
        let mut ids = HashSet::new();
        for call in &assistant.tool_calls {
            if call.id.is_empty()
                || call.id.len() > 256
                || !ids.insert(call.id.as_str())
                || seen.contains(call.id.as_str())
            {
                return Err(AgentError::Model(
                    "model returned an empty, oversized, or duplicate tool call ID".into(),
                )
                .into());
            }
        }
        let prior_calls = self.active_turn_call_count(&record.events);
        if prior_calls.saturating_add(assistant.tool_calls.len()) > self.limits.max_tool_calls {
            return Err(AgentError::Limit("recovery exceeded tool call limit".into()).into());
        }
        self.append(
            lease,
            replay.last_sequence,
            events,
            AgentEventKind::ModelResponseCommitted {
                step,
                assistant: assistant.clone(),
                response_id: completion.response_id,
                usage: completion.usage,
            },
        )?;
        if !assistant.content.is_empty() {
            self.append(
                lease,
                replay.last_sequence,
                events,
                AgentEventKind::AssistantMessage {
                    content: assistant.content.clone(),
                },
            )?;
        }
        if assistant.tool_calls.is_empty() {
            if turn.mode == TurnMode::Plan {
                self.append(
                    lease,
                    replay.last_sequence,
                    events,
                    AgentEventKind::PlanProposed {
                        plan: PlanSnapshot::from_plan_response(&assistant.content),
                    },
                )?;
            }
            self.finish_model_response(lease, replay, events, assistant.content, step, &cancel)
                .await?;
        }
        Ok(())
    }

    async fn run_tool(
        &self,
        lease: &NativeSessionLease,
        replay: &NativeReplay,
        call: &ToolCall,
        cancel: CancellationToken,
        events: &mut Vec<AgentEvent>,
    ) -> Result<Option<String>, ResumeError> {
        self.require_stable_tool_identity(replay, call)?;
        let hooks_digest = self.require_stable_hooks_identity(replay)?;
        let catalog = self.verified_catalog(replay)?;
        let validation_error = catalog.require_valid_call(call).err();
        let record = self
            .sessions
            .load(lease.session_id())?
            .ok_or(ResumeError::SessionMissing)?;
        let workspace = record
            .session
            .workspace
            .canonicalize()
            .map_err(|error| AgentError::InvalidRequest(format!("workspace: {error}")))?;
        let turn_start_sequence = record
            .events
            .iter()
            .rev()
            .find_map(|event| {
                matches!(event.kind, AgentEventKind::TurnStarted { .. }).then_some(event.sequence)
            })
            .ok_or(ResumeError::StaleResolution)?;
        let prior_resolution = record
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                AgentEventKind::PermissionResolved {
                    tool_call_id,
                    resolution,
                    request_id,
                } if tool_call_id == &call.id => Some((*resolution, request_id.clone())),
                _ => None,
            });
        let resolved_request = prior_resolution.as_ref().map(|(_, id)| id.clone());
        let prior_resolution = prior_resolution.map(|(resolution, _)| resolution);
        let base_mode = self.permission_policy.mode_for_call(call);
        let project_preflight = if validation_error.is_none()
            && base_mode != PermissionMode::Deny
            && !matches!(
                prior_resolution,
                Some(
                    PermissionResolution::Denied
                        | PermissionResolution::TimedOut
                        | PermissionResolution::Cancelled
                )
            ) {
            ProjectInstructionLedger::from_session(&record, turn_start_sequence, &workspace)?
                .and_then(|ledger| {
                    ledger
                        .preflight(&workspace, call, self.limits.max_tool_output_chars)
                        .unwrap_or_else(|error| Some(ToolOutput::error(error.to_string())))
                })
        } else {
            None
        };
        let mode = if base_mode == PermissionMode::Ask
            && validation_error.is_none()
            && prior_resolution.is_none()
        {
            if let Some(digest) = self.session_approval_digest(replay, &catalog)? {
                let workspace =
                    record.session.workspace.canonicalize().map_err(|error| {
                        AgentError::InvalidRequest(format!("workspace: {error}"))
                    })?;
                let grants = self
                    .sessions
                    .session_approval_grants(lease.session_id())?
                    .into_iter()
                    .map(SessionApprovalGrant::from)
                    .collect::<Vec<_>>();
                self.permission_policy.mode_for_call_with_session_grants(
                    lease.session_id(),
                    &workspace,
                    &workspace,
                    &digest,
                    call,
                    &grants,
                )
            } else {
                base_mode
            }
        } else {
            base_mode
        };
        let builtin_plan = call.name == UPDATE_PLAN_TOOL
            && replay
                .active_turn
                .as_ref()
                .is_some_and(|turn| turn.mode == TurnMode::Build)
            && self.is_builtin_plan_catalog(&catalog);
        let builtin_interaction =
            self.user_interaction && user_interaction::is_builtin(&catalog, call);
        let project_activation_marker = project_preflight
            .as_ref()
            .and_then(|output| activation_hook_id(&output.content));
        let denied = if let Some(error) = validation_error {
            Some(ToolOutput::error(error.to_string()))
        } else if project_preflight.is_some() {
            project_preflight
        } else if builtin_plan {
            None
        } else if builtin_interaction {
            // Never decided by policy: the user's answer is the result.
            match (user_interaction::invalid(call), prior_resolution) {
                (Some(invalid), _) => Some(invalid),
                (None, Some(resolution)) => {
                    let answer = match (resolution, resolved_request.as_deref()) {
                        (PermissionResolution::Allowed, Some(request_id)) => self
                            .sessions
                            .load_permission_decisions(lease.session_id())?
                            .into_iter()
                            .find(|decision| decision.request_id == request_id)
                            .and_then(|decision| decision.answer),
                        _ => None,
                    };
                    Some(user_interaction::outcome(call, resolution, answer))
                }
                (None, None) => {
                    self.append(
                        lease,
                        replay.last_sequence,
                        events,
                        AgentEventKind::PermissionRequested {
                            request_id: Uuid::new_v4().to_string(),
                            tool_call: call.clone(),
                        },
                    )?;
                    return Ok(None);
                }
            }
        } else if mode == PermissionMode::Deny {
            Some(ToolOutput::error(format!(
                "Permission denied for {}",
                call.name
            )))
        } else {
            match prior_resolution {
                Some(PermissionResolution::Allowed) => None,
                Some(PermissionResolution::Denied) => Some(ToolOutput::error(format!(
                    "Permission denied for {}",
                    call.name
                ))),
                Some(PermissionResolution::TimedOut) => Some(ToolOutput::error(format!(
                    "Permission request timed out for {}",
                    call.name
                ))),
                Some(PermissionResolution::Cancelled) => Some(ToolOutput::error(format!(
                    "Permission cancelled for {}",
                    call.name
                ))),
                None if mode == PermissionMode::Ask => {
                    self.append(
                        lease,
                        replay.last_sequence,
                        events,
                        AgentEventKind::PermissionRequested {
                            request_id: Uuid::new_v4().to_string(),
                            tool_call: call.clone(),
                        },
                    )?;
                    return Ok(None);
                }
                None => None,
            }
        };
        let workspace =
            if denied.is_none() {
                Some(
                    record.session.workspace.canonicalize().map_err(|error| {
                        AgentError::InvalidRequest(format!("workspace: {error}"))
                    })?,
                )
            } else {
                None
            };
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled.into());
        }
        self.append(
            lease,
            replay.last_sequence,
            events,
            AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            },
        )?;
        let result = if let Some(denied) = denied {
            denied
        } else if builtin_plan {
            match plan_tool::parse(call) {
                Ok(plan) => {
                    self.append(
                        lease,
                        replay.last_sequence,
                        events,
                        AgentEventKind::PlanUpdated {
                            tool_call_id: call.id.clone(),
                            plan,
                        },
                    )?;
                    ToolOutput::success("Plan updated")
                }
                Err(error) => error,
            }
        } else {
            let context = ToolContext {
                workspace: workspace.expect("validated before ToolStarted"),
                cancel,
            };
            let turn_start_sequence = record
                .events
                .iter()
                .rev()
                .find_map(|event| {
                    matches!(event.kind, AgentEventKind::TurnStarted { .. })
                        .then_some(event.sequence)
                })
                .ok_or(ResumeError::StaleResolution)?;
            // Any hook or executor failure leaves ToolStarted open. Recovery
            // then requires manual verification rather than a second effect.
            match self
                .execute_with_hooks(
                    lease,
                    replay,
                    call,
                    &context,
                    turn_start_sequence,
                    &hooks_digest,
                    events,
                )
                .await
            {
                Ok(result) => result,
                Err(ResumeError::Agent(error)) => return Ok(Some(error.to_string())),
                Err(error) => return Err(error),
            }
        };
        if let Some(hook_id) = project_activation_marker.filter(|marker| {
            result.is_error && activation_hook_id(&result.content).as_ref() == Some(marker)
        }) {
            self.append(
                lease,
                replay.last_sequence,
                events,
                AgentEventKind::HookStarted {
                    tool_call_id: call.id.clone(),
                    hook_id: hook_id.clone(),
                    phase: "pre_tool_use".into(),
                },
            )?;
            self.append(
                lease,
                replay.last_sequence,
                events,
                AgentEventKind::HookFinished {
                    tool_call_id: call.id.clone(),
                    hook_id,
                    phase: "pre_tool_use".into(),
                    decision: "allow".into(),
                },
            )?;
        }
        if let Some(media) = crate::tool_media::extract_tool_result_media(call, &result.content) {
            self.append(lease, replay.last_sequence, events, media)?;
        }
        let content = result
            .content
            .chars()
            .take(self.limits.max_tool_output_chars)
            .collect();
        self.append(
            lease,
            replay.last_sequence,
            events,
            AgentEventKind::ToolFinished {
                tool_call_id: call.id.clone(),
                output: content,
                is_error: result.is_error,
            },
        )?;
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_with_hooks(
        &self,
        lease: &NativeSessionLease,
        replay: &NativeReplay,
        call: &ToolCall,
        context: &ToolContext,
        turn_start_sequence: u64,
        hooks_digest: &str,
        events: &mut Vec<AgentEvent>,
    ) -> Result<ToolOutput, ResumeError> {
        let mut contexts = Vec::new();
        // Same rewrite rule as the live runner's `execute_with_hooks`.
        let mut rewritten = call.clone();
        for (config, compat) in self
            .command_hooks
            .tool_hooks(HookPhase::PreToolUse, &call.name)
        {
            let response = self
                .run_one_hook(
                    lease,
                    replay,
                    config,
                    &rewritten,
                    None,
                    context,
                    turn_start_sequence,
                    hooks_digest,
                    events,
                )
                .await?;
            if response.decision == HookDecision::Block {
                return Ok(ToolOutput::error(format!(
                    "PreToolUse hook {} blocked {}: {}",
                    config.id,
                    call.name,
                    response.reason.unwrap_or_default()
                )));
            }
            // A Claude Code hook answers in its own input shape; one that
            // does not map back onto this tool is ignored.
            let updated = response.updated_input.and_then(|arguments| {
                if compat {
                    crate::native_input(&call.name, &rewritten.arguments, &arguments)
                } else {
                    Some(arguments)
                }
            });
            if let Some(arguments) = updated {
                rewritten.arguments = arguments;
                contexts.push(format!("Hook {} rewrote this call's input.", config.id));
            }
            if let Some(text) = response.additional_context {
                contexts.push(text);
            }
        }
        let call = &rewritten;
        let mut output = self
            .execute_streaming_tool(lease, replay, call, context, events)
            .await?;
        for (config, _) in self
            .command_hooks
            .tool_hooks(HookPhase::PostToolUse, &call.name)
        {
            let response = self
                .run_one_hook(
                    lease,
                    replay,
                    config,
                    call,
                    Some(&output),
                    context,
                    turn_start_sequence,
                    hooks_digest,
                    events,
                )
                .await?;
            if let Some(text) = response.additional_context {
                contexts.push(text);
            }
        }
        if !contexts.is_empty() {
            output.content.push_str("\nHook context:\n");
            output.content.push_str(&contexts.join("\n"));
        }
        Ok(output)
    }

    async fn execute_streaming_tool(
        &self,
        lease: &NativeSessionLease,
        replay: &NativeReplay,
        call: &ToolCall,
        context: &ToolContext,
        events: &mut Vec<AgentEvent>,
    ) -> Result<ToolOutput, ResumeError> {
        let (sender, mut receiver) =
            crate::tool_output::tool_output_channel(context.cancel.clone());
        let mut pending_delivery = None;
        let mut delivery_failed = false;
        let result = {
            let future = self.tools.execute_streaming(call, context, sender);
            tokio::pin!(future);
            loop {
                tokio::select! {
                    biased;
                    _ = context.cancel.cancelled() => break Err(ResumeError::from(AgentError::Cancelled)),
                    Some(chunk) = receiver.recv() => {
                        let previous_len = events.len();
                        if let Err(error) = self.append_stream_update(
                            lease, replay.last_sequence, events, chunk.into_event(&call.id), &context.cancel,
                        ).await {
                            if matches!(&error, ResumeError::Agent(AgentError::Cancelled)) && events.len() > previous_len {
                                pending_delivery = events.last().cloned();
                            } else {
                                delivery_failed = true;
                            }
                            break Err(error);
                        }
                    }
                    result = &mut future => break result.map_err(ResumeError::from),
                }
            }
        };
        receiver.close();
        if let Some(event) = pending_delivery {
            self.observe(&event)?;
        }
        if delivery_failed {
            return result;
        }
        while let Ok(chunk) = receiver.try_recv() {
            self.append(
                lease,
                replay.last_sequence,
                events,
                chunk.into_event(&call.id),
            )?;
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_one_hook(
        &self,
        lease: &NativeSessionLease,
        replay: &NativeReplay,
        config: &crate::CommandHookConfig,
        call: &ToolCall,
        output: Option<&ToolOutput>,
        context: &ToolContext,
        turn_start_sequence: u64,
        hooks_digest: &str,
        events: &mut Vec<AgentEvent>,
    ) -> Result<crate::HookResponse, ResumeError> {
        let current = self.command_hooks.digest_hex().map_err(|error| {
            AgentError::EffectUncertain(format!("hook identity changed after tool start: {error}"))
        })?;
        if current != hooks_digest {
            return Err(AgentError::EffectUncertain(
                "hook configuration changed after tool start".into(),
            )
            .into());
        }
        let phase = match config.phase {
            HookPhase::PreToolUse => "pre_tool_use",
            HookPhase::PostToolUse => "post_tool_use",
        };
        self.append(
            lease,
            replay.last_sequence,
            events,
            AgentEventKind::HookStarted {
                tool_call_id: call.id.clone(),
                hook_id: config.id.clone(),
                phase: phase.into(),
            },
        )?;
        let invocation = HookInvocation {
            phase: config.phase,
            session_id: lease.session_id().into(),
            turn_id: turn_start_sequence.to_string(),
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            tool_input: call.arguments.clone(),
            tool_output: output.map(
                |value| serde_json::json!({"content": value.content, "is_error": value.is_error}),
            ),
        };
        let result =
            crate::hooks::run_tool_hook(&self.command_hooks, config, &invocation, &context.cancel)
                .await;
        let (partial_stdout, partial_stderr) = match &result {
            HookRunResult::OutcomeUncertainCaptured { stdout, stderr, .. } => {
                (Some(stdout.clone()), Some(stderr.clone()))
            }
            _ => (None, None),
        };
        match result {
            HookRunResult::Completed {
                response,
                stdout,
                stderr,
            } => {
                self.append(
                    lease,
                    replay.last_sequence,
                    events,
                    AgentEventKind::HookDiagnosticCommitted {
                        tool_call_id: call.id.clone(),
                        hook_id: config.id.clone(),
                        phase: phase.into(),
                        reason: response.reason.clone(),
                        stdout: Some(stdout),
                        stderr: Some(stderr),
                        error: None,
                    },
                )?;

                self.append(
                    lease,
                    replay.last_sequence,
                    events,
                    AgentEventKind::HookFinished {
                        tool_call_id: call.id.clone(),
                        hook_id: config.id.clone(),
                        phase: phase.into(),
                        decision: match response.decision {
                            HookDecision::Allow => "allow",
                            HookDecision::Block => "block",
                        }
                        .into(),
                    },
                )?;
                Ok(response)
            }
            HookRunResult::NotStarted(failure)
            | HookRunResult::OutcomeUncertain(failure)
            | HookRunResult::OutcomeUncertainCaptured { failure, .. } => {
                self.append(
                    lease,
                    replay.last_sequence,
                    events,
                    AgentEventKind::HookDiagnosticCommitted {
                        tool_call_id: call.id.clone(),
                        hook_id: config.id.clone(),
                        phase: phase.into(),
                        reason: None,
                        stdout: partial_stdout,
                        stderr: partial_stderr,
                        error: Some(format!("{failure:?}").chars().take(2000).collect()),
                    },
                )?;
                Err(AgentError::EffectUncertain(format!(
                    "hook {} has no committed outcome after tool start: {failure:?}",
                    config.id
                ))
                .into())
            }
        }
    }

    fn catalog_for_mode(&self, mode: TurnMode) -> Result<ToolCatalog, ResumeError> {
        if mode == TurnMode::Build {
            // A caller may supply the new built-in already, or an old frozen
            // catalog with an unrelated external tool sharing the name.
            if self.catalog.get(UPDATE_PLAN_TOOL).is_some() {
                return Ok(self.catalog.clone());
            }
            let mut definitions = self.catalog.definitions().to_vec();
            definitions.push(plan_tool::definition());
            return Ok(ToolCatalog::new(definitions)?);
        }
        Ok(ToolCatalog::new(
            self.catalog
                .definitions()
                .iter()
                .filter(|definition| {
                    trusted_plan_read(&definition.name)
                        && self.tools.certifies_plan_read(&definition.name)
                        && !self.command_hooks.has_hooks_for_tool(&definition.name)
                })
                .cloned(),
        )?)
    }

    /// The catalogs a turn in `mode` may have frozen, newest first: the
    /// executor's tools with each generation of the built-ins that wait for
    /// the user, then without any.
    fn catalog_generations(&self, mode: TurnMode) -> Result<Vec<ToolCatalog>, ResumeError> {
        let base = self.catalog_for_mode(mode)?;
        let mut catalogs = if self.user_interaction {
            user_interaction::catalogs_with_user_interaction(&base)
        } else {
            Vec::new()
        };
        catalogs.push(base);
        Ok(catalogs)
    }

    fn verified_catalog(&self, replay: &NativeReplay) -> Result<ToolCatalog, ResumeError> {
        let turn = replay
            .active_turn
            .as_ref()
            .ok_or(ResumeError::StaleResolution)?;
        // An older Build turn may have committed the executor catalog alone,
        // including an unrelated external tool with this name. Honor its
        // exact saved digest before considering the new built-in definition.
        if turn.mode == TurnMode::Build
            && replay.active_tool_catalog_digest.as_deref()
                == Some(self.catalog.digest_hex().as_str())
        {
            return Ok(self.catalog.clone());
        }
        let saved = replay
            .active_tool_catalog_digest
            .as_deref()
            .ok_or(ResumeError::CatalogUnverified)?;
        self.catalog_generations(turn.mode)?
            .into_iter()
            .find(|catalog| catalog.digest_hex() == saved)
            .ok_or(ResumeError::CatalogMismatch)
    }

    fn session_approval_digest(
        &self,
        replay: &NativeReplay,
        catalog: &ToolCatalog,
    ) -> Result<Option<String>, ResumeError> {
        let Some((sessions, isolation)) = self.session_approvals.as_ref() else {
            return Ok(None);
        };
        if !Arc::ptr_eq(sessions, &self.sessions) {
            return Ok(None);
        }
        let hooks_digest = self.require_stable_hooks_identity(replay)?;
        effective_session_approval_policy_digest(
            &catalog.digest_hex(),
            replay.active_mcp_runtime_digest.as_deref(),
            &hooks_digest,
            *isolation,
            &self.permission_policy,
        )
        .map(Some)
        .map_err(|error| {
            AgentError::InvalidRequest(format!("session approval policy identity: {error}")).into()
        })
    }

    fn is_builtin_plan_catalog(&self, catalog: &ToolCatalog) -> bool {
        catalog.get(UPDATE_PLAN_TOOL) == Some(&plan_tool::definition())
    }

    fn plan_context_for_catalog<'a>(
        &self,
        catalog: &ToolCatalog,
        replay: &'a NativeReplay,
    ) -> Option<&'a PlanSnapshot> {
        self.is_builtin_plan_catalog(catalog)
            .then_some(replay.active_plan_at_turn_start.as_ref())
            .flatten()
    }

    fn require_stable_tool_identity(
        &self,
        replay: &NativeReplay,
        call: &ToolCall,
    ) -> Result<(), ResumeError> {
        if call.name.starts_with("mcp__") {
            match (
                replay.active_mcp_runtime_version,
                replay.active_mcp_runtime_digest.as_deref(),
                self.mcp_runtime_identity_digest.as_deref(),
            ) {
                (Some(MCP_RUNTIME_IDENTITY_VERSION), Some(saved), Some(current))
                    if saved == current => {}
                (_, Some(_), Some(_)) => return Err(ResumeError::McpIdentityMismatch),
                _ => return Err(ResumeError::McpIdentityUnverified),
            }
        }
        Ok(())
    }

    fn require_stable_hooks_identity(&self, replay: &NativeReplay) -> Result<String, ResumeError> {
        if replay.active_hooks_version != Some(HOOK_CONFIG_IDENTITY_VERSION) {
            return Err(ResumeError::HooksUnverified);
        }
        let saved = replay
            .active_hooks_digest
            .as_deref()
            .ok_or(ResumeError::HooksUnverified)?;
        let current = self
            .command_hooks
            .digest_hex()
            .map_err(|_| ResumeError::HooksUnverified)?;
        if current != saved {
            return Err(ResumeError::HooksMismatch);
        }
        Ok(current)
    }

    fn active_turn_call_count(&self, events: &[AgentEvent]) -> usize {
        let start = events
            .iter()
            .rposition(|event| {
                matches!(
                    event.kind,
                    AgentEventKind::TurnAdmitted { .. } | AgentEventKind::TurnStarted { .. }
                )
            })
            .unwrap_or(0);
        let mut ids = HashSet::new();
        for event in &events[start..] {
            match &event.kind {
                AgentEventKind::ModelResponseCommitted { assistant, .. } => {
                    ids.extend(assistant.tool_calls.iter().map(|call| call.id.as_str()));
                }
                AgentEventKind::PermissionRequested { tool_call, .. }
                | AgentEventKind::ToolStarted { tool_call } => {
                    ids.insert(tool_call.id.as_str());
                }
                _ => {}
            }
        }
        ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("resume_tool_output_tests.rs");
    use crate::{ModelOutput, ToolDefinition};
    use async_trait::async_trait;
    use openide_core::AgentEventKind;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tempfile::TempDir;

    struct MockModel {
        outputs: Mutex<Vec<ModelOutput>>,
        calls: AtomicUsize,
        seen_histories: Mutex<Vec<Vec<AgentMessage>>>,
        seen_tools: Mutex<Vec<Vec<ToolDefinition>>>,
        interfere: Mutex<Option<(Arc<SessionStore>, String)>>,
        request_profile: Mutex<[u8; 32]>,
        input_counts: Mutex<Vec<u64>>,
    }

    #[async_trait]
    impl ModelBackend for MockModel {
        fn model_id(&self) -> &str {
            "gpt-example"
        }

        fn supports_user_images(&self) -> bool {
            true
        }

        async fn count_input_tokens(
            &self,
            _messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<Option<u64>, AgentError> {
            let mut counts = self.input_counts.lock().unwrap();
            Ok(Some(if counts.is_empty() {
                100
            } else {
                counts.remove(0)
            }))
        }

        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some(*self.request_profile.lock().unwrap())
        }

        async fn complete(
            &self,
            messages: &[AgentMessage],
            tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen_histories.lock().unwrap().push(messages.to_vec());
            self.seen_tools.lock().unwrap().push(tools.to_vec());
            if let Some((store, session_id)) = self.interfere.lock().unwrap().take() {
                let record = store.load(&session_id).unwrap().unwrap();
                let sequence = record.events.last().unwrap().sequence + 1;
                store
                    .append_native_event(&AgentEvent::new(
                        session_id,
                        sequence,
                        AgentEventKind::ProcessOutput {
                            stream: openide_core::OutputStream::Stdout,
                            text: "concurrent append".into(),
                        },
                    ))
                    .unwrap();
            }
            let mut outputs = self.outputs.lock().unwrap();
            if outputs.is_empty() {
                return Err(AgentError::Model("mock has no remaining response".into()));
            }
            Ok(outputs.remove(0))
        }
    }

    struct MockTools {
        calls: Mutex<Vec<ToolCall>>,
        result: Mutex<Option<Result<ToolOutput, String>>>,
    }

    #[async_trait]
    impl ToolExecutor for MockTools {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![definition()]
        }

        async fn execute(
            &self,
            call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            self.calls.lock().unwrap().push(call.clone());
            match self.result.lock().unwrap().take() {
                Some(Ok(result)) => Ok(result),
                Some(Err(error)) => Err(AgentError::Tool(error)),
                None => Ok(ToolOutput::success("read complete")),
            }
        }
    }

    struct Fixture {
        _dir: TempDir,
        sessions: Arc<SessionStore>,
        checkpoints: Arc<CheckpointFileStore>,
        session_id: String,
        model: Arc<MockModel>,
        tools: Arc<MockTools>,
    }

    impl Fixture {
        fn new(outputs: Vec<ModelOutput>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
            let session = sessions.create(dir.path(), "native").unwrap();
            let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
            Self {
                _dir: dir,
                sessions,
                checkpoints,
                session_id: session.id,
                model: Arc::new(MockModel {
                    outputs: Mutex::new(outputs),
                    calls: AtomicUsize::new(0),
                    seen_histories: Mutex::new(Vec::new()),
                    seen_tools: Mutex::new(Vec::new()),
                    interfere: Mutex::new(None),
                    request_profile: Mutex::new([7; 32]),
                    input_counts: Mutex::new(Vec::new()),
                }),
                tools: Arc::new(MockTools {
                    calls: Mutex::new(Vec::new()),
                    result: Mutex::new(None),
                }),
            }
        }

        fn engine(&self, permission_mode: PermissionMode) -> ResumeEngine {
            ResumeEngine::new(
                self.sessions.clone(),
                self.checkpoints.clone(),
                compatibility(),
                self.model.clone(),
                self.tools.clone(),
                ToolCatalog::new([definition()]).unwrap(),
                PermissionPolicy::new().with_default_mode(permission_mode),
                budget(),
            )
            .with_native_runtime_identity("openai", "https://test.example/v1")
        }

        fn admit(&self) {
            self.sessions
                .admit_native_turn(
                    &self.session_id,
                    "turn-1",
                    "Inspect the file",
                    "gpt-example",
                    TurnMode::Build,
                )
                .unwrap();
        }

        fn append(&self, kind: AgentEventKind) {
            let record = self.sessions.load(&self.session_id).unwrap().unwrap();
            let sequence = record.events.last().map_or(1, |event| event.sequence + 1);
            self.sessions
                .append_native_event(&AgentEvent::new(&self.session_id, sequence, kind))
                .unwrap();
        }

        fn started(&self) {
            self.started_with_hooks(&CommandHooks::default());
        }

        fn started_with_model_route(&self, request_started: bool) {
            self.admit();
            self.append(AgentEventKind::TurnStarted {
                prompt: "Inspect the file".into(),
            });
            self.append(AgentEventKind::ToolCatalogCommitted {
                digest: ToolCatalog::new([definition()]).unwrap().digest_hex(),
            });
            self.append(AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            });
            self.append(AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: self.context_digest(),
            });
            self.append(
                openide_core::ModelRouteSnapshot::new(
                    "openai",
                    "gpt-example",
                    "https://test.example/v1",
                    [7; 32],
                )
                .unwrap()
                .into_event(),
            );
            if request_started {
                self.append(AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "gpt-example".into(),
                });
            }
        }

        fn started_with_hooks(&self, hooks: &CommandHooks) {
            self.admit();
            self.append(AgentEventKind::TurnStarted {
                prompt: "Inspect the file".into(),
            });
            self.append(AgentEventKind::ToolCatalogCommitted {
                digest: ToolCatalog::new([definition()]).unwrap().digest_hex(),
            });
            self.append(AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: hooks.digest_hex().unwrap(),
            });
            self.append(AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: self.context_digest(),
            });
            self.append(AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            });
        }

        fn started_with_instruction_snapshot(&self, instructions: &str) {
            self.admit();
            self.append(AgentEventKind::TurnStarted {
                prompt: "Inspect the file".into(),
            });
            let catalog = ToolCatalog::new([definition()]).unwrap();
            self.append(AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            });
            self.append(AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            });
            self.append(AgentEventKind::ExecPolicyCommitted {
                version: EXEC_POLICY_IDENTITY_VERSION,
                digest: PermissionPolicy::new()
                    .with_default_mode(PermissionMode::Allow)
                    .digest_hex(),
            });
            self.append(AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest: system_instructions_digest(instructions).unwrap(),
                content: instructions.into(),
            });
            let record = self.sessions.load(&self.session_id).unwrap().unwrap();
            let digest = crate::native_model_context_digest(
                "openai",
                "https://test.example/v1",
                "gpt-example",
                TurnMode::Build,
                &record.session.workspace,
                &catalog,
                instructions,
                [7; 32],
            )
            .unwrap();
            self.append(AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest,
            });
            self.append(AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            });
        }

        fn context_digest(&self) -> String {
            let record = self.sessions.load(&self.session_id).unwrap().unwrap();
            crate::native_model_context_digest(
                "openai",
                "https://test.example/v1",
                "gpt-example",
                TurnMode::Build,
                &record.session.workspace,
                &ToolCatalog::new([definition()]).unwrap(),
                "",
                [7; 32],
            )
            .unwrap()
        }

        fn tool_ready(&self) {
            self.started();
            self.append(AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![call("call-1")]),
                response_id: Some("response-1".into()),
                usage: None,
            });
        }

        fn events(&self) -> Vec<AgentEvent> {
            self.sessions
                .load(&self.session_id)
                .unwrap()
                .unwrap()
                .events
        }
    }

    fn compatibility() -> CheckpointCompatibility {
        CheckpointCompatibility {
            provider: "openai".into(),
            model: "gpt-example".into(),
            context_format_key: "responses-v1".into(),
        }
    }

    #[tokio::test]
    async fn recovery_reuses_exact_plan_snapshot_from_prior_plan_turn() {
        let fixture = Fixture::new(vec![done()]);
        let plan_text = "1. Inspect files\n2. Implement change";
        let proposed = PlanSnapshot::from_plan_response(plan_text);
        fixture
            .sessions
            .admit_native_turn(
                &fixture.session_id,
                "plan-turn",
                "Plan this",
                "gpt-example",
                TurnMode::Plan,
            )
            .unwrap();
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Plan this".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant(plan_text, vec![]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::AssistantMessage {
                content: plan_text.into(),
            },
            AgentEventKind::PlanProposed {
                plan: proposed.clone(),
            },
            AgentEventKind::TurnCompleted {
                response: plan_text.into(),
                steps: 1,
            },
        ] {
            fixture.append(kind);
        }
        fixture
            .sessions
            .admit_native_turn(
                &fixture.session_id,
                "build-turn",
                "Build this",
                "gpt-example",
                TurnMode::Build,
            )
            .unwrap();
        let catalog = ToolCatalog::new([definition(), plan_tool::definition()]).unwrap();
        let workspace = fixture
            .sessions
            .load(&fixture.session_id)
            .unwrap()
            .unwrap()
            .session
            .workspace
            .canonicalize()
            .unwrap();
        let system = native_system_prompt_with_plan(
            &workspace,
            catalog.definitions(),
            TurnMode::Build,
            "",
            Some(&proposed),
        );
        let digest = NativeRuntimeIdentity::new("openai", "https://test.example/v1")
            .digest_hex(
                "gpt-example",
                TurnMode::Build,
                &workspace,
                &catalog.digest_hex(),
                &system,
                [7; 32],
            )
            .unwrap();
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Build this".into(),
            },
            AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            },
            AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
            AgentEventKind::ExecPolicyCommitted {
                version: EXEC_POLICY_IDENTITY_VERSION,
                digest: PermissionPolicy::new()
                    .with_default_mode(PermissionMode::Allow)
                    .digest_hex(),
            },
            AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest: system_instructions_digest("").unwrap(),
                content: "".into(),
            },
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest,
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            },
        ] {
            fixture.append(kind);
        }
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Allow);
        let before = engine.inspect(&lease).unwrap();
        assert_eq!(before.active_plan_at_turn_start, Some(proposed.clone()));
        assert!(matches!(
            before.recovery,
            ReplayRecovery::ModelInFlight { .. }
        ));
        let resumed = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(resumed.disposition, ResumeDisposition::Complete);
        assert_eq!(resumed.replay.latest_plan, Some(proposed.clone()));
        let seen = fixture.model.seen_histories.lock().unwrap();
        assert!(seen[0][0]
            .content
            .contains(&serde_json::to_string(&proposed).unwrap()));
    }

    fn budget() -> TokenBudgetPolicy {
        TokenBudgetPolicy {
            input_limit_tokens: None,
            context_window_tokens: 128_000,
            output_reserve_tokens: 4_096,
            tool_schema_reserve_tokens: 1_024,
            auto_compact_token_limit: None,
        }
    }

    fn definition() -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: json!({
                "type":"object",
                "properties":{"path":{"type":"string"}},
                "required":["path"],
                "additionalProperties":false
            }),
        }
    }

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "read_file".into(),
            arguments: json!({"path":"README.md"}),
        }
    }

    fn recovery_hook(workspace: &std::path::Path, script: &str) -> CommandHooks {
        #[cfg(windows)]
        let (executable, args) = (
            std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script.into(),
            ],
        );
        #[cfg(unix)]
        let (executable, args) = (
            std::path::PathBuf::from("/bin/sh"),
            vec!["-c".into(), script.into()],
        );
        CommandHooks::new(vec![crate::CommandHookConfig {
            id: "recovery-hook".into(),
            phase: HookPhase::PreToolUse,
            tool_name: "read_file".into(),
            executable,
            args,
            cwd: workspace.into(),
            timeout_ms: 5_000,
            max_output_bytes: 4_096,
        }])
        .unwrap()
    }

    #[tokio::test]
    async fn ready_tool_runs_matching_hook_after_synced_start() {
        let fixture = Fixture::new(vec![done()]);
        #[cfg(windows)]
        let script = "[IO.File]::WriteAllText('hook-ran.txt','yes'); [Console]::Out.Write('{\"decision\":\"allow\"}')";
        #[cfg(unix)]
        let script = "printf yes > hook-ran.txt; printf '%s' '{\"decision\":\"allow\"}'";
        let hooks = Arc::new(recovery_hook(fixture._dir.path(), script));
        fixture.started_with_hooks(&hooks);
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("call-1")]),
            response_id: None,
            usage: None,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let result = fixture
            .engine(PermissionMode::Allow)
            .with_command_hooks(hooks)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.disposition, ResumeDisposition::Complete);
        assert!(fixture._dir.path().join("hook-ran.txt").exists());
        assert_eq!(fixture.tools.calls.lock().unwrap().len(), 1);
        let events = fixture.events();
        let position = |predicate: fn(&AgentEventKind) -> bool| {
            events
                .iter()
                .position(|event| predicate(&event.kind))
                .unwrap()
        };
        assert!(
            position(|kind| matches!(kind, AgentEventKind::ToolStarted { .. }))
                < position(|kind| matches!(kind, AgentEventKind::HookStarted { .. }))
        );
        assert!(
            position(|kind| matches!(kind, AgentEventKind::HookFinished { .. }))
                < position(|kind| matches!(kind, AgentEventKind::ToolFinished { .. }))
        );
    }

    #[tokio::test]
    async fn changed_or_missing_hook_snapshot_blocks_ready_tool() {
        let fixture = Fixture::new(vec![]);
        #[cfg(windows)]
        let allow_script = "[Console]::Out.Write('{\"decision\":\"allow\"}')";
        #[cfg(unix)]
        let allow_script = "printf '%s' '{\"decision\":\"allow\"}'";
        #[cfg(windows)]
        let other_script =
            "[Console]::Out.Write('{\"decision\":\"block\",\"reason\":\"changed\"}')";
        #[cfg(unix)]
        let other_script = "printf '%s' '{\"decision\":\"block\",\"reason\":\"changed\"}'";
        let original = recovery_hook(fixture._dir.path(), allow_script);
        fixture.started_with_hooks(&original);
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("call-1")]),
            response_id: None,
            usage: None,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let changed = Arc::new(recovery_hook(fixture._dir.path(), other_script));
        assert!(matches!(
            fixture
                .engine(PermissionMode::Allow)
                .with_command_hooks(changed)
                .resume_step(&lease, CancellationToken::new())
                .await,
            Err(ResumeError::HooksMismatch)
        ));
        assert!(!fixture
            .events()
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. })));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());

        let legacy = Fixture::new(vec![]);
        legacy.admit();
        legacy.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        legacy.append(AgentEventKind::ToolCatalogCommitted {
            digest: ToolCatalog::new([definition()]).unwrap().digest_hex(),
        });
        legacy.append(AgentEventKind::ModelContextCommitted {
            version: MODEL_CONTEXT_VERSION,
            digest: legacy.context_digest(),
        });
        legacy.append(AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "gpt-example".into(),
        });
        legacy.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("call-1")]),
            response_id: None,
            usage: None,
        });
        let lease = legacy
            .sessions
            .try_acquire_native_lease(&legacy.session_id)
            .unwrap();
        assert!(matches!(
            legacy
                .engine(PermissionMode::Allow)
                .resume_step(&lease, CancellationToken::new())
                .await,
            Err(ResumeError::HooksUnverified)
        ));
        assert!(legacy.tools.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn interrupted_hook_start_is_never_reexecuted() {
        let fixture = Fixture::new(vec![]);
        #[cfg(windows)]
        let script = "[IO.File]::WriteAllText('should-not-run.txt','bad'); [Console]::Out.Write('{\"decision\":\"allow\"}')";
        #[cfg(unix)]
        let script = "printf bad > should-not-run.txt; printf '%s' '{\"decision\":\"allow\"}'";
        let hooks = Arc::new(recovery_hook(fixture._dir.path(), script));
        fixture.started_with_hooks(&hooks);
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("call-1")]),
            response_id: None,
            usage: None,
        });
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("call-1"),
        });
        fixture.append(AgentEventKind::HookStarted {
            tool_call_id: "call-1".into(),
            hook_id: "recovery-hook".into(),
            phase: "pre_tool_use".into(),
        });
        fixture.append(AgentEventKind::TurnFailed {
            error: "crash".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let result = fixture
            .engine(PermissionMode::Allow)
            .with_command_hooks(hooks)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            matches!(result.disposition, ResumeDisposition::ToolOutcomeUncertain { tool_call, .. } if tool_call.id == "call-1")
        );
        assert!(!fixture._dir.path().join("should-not-run.txt").exists());
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
    }

    fn done() -> ModelOutput {
        ModelOutput {
            content: "Done".into(),
            tool_calls: vec![],
        }
    }

    #[tokio::test]
    async fn plan_recovery_does_not_advertise_uncertified_read_executor() {
        let fixture = Fixture::new(vec![done()]);
        // MockTools advertises read_file but does not certify its implementation
        // as a trusted local read.
        fixture
            .sessions
            .admit_native_turn(
                &fixture.session_id,
                "turn-plan",
                "Inspect the file",
                "gpt-example",
                TurnMode::Plan,
            )
            .unwrap();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let result = fixture
            .engine(PermissionMode::Allow)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.disposition, ResumeDisposition::Complete);
        assert_eq!(
            fixture.model.seen_tools.lock().unwrap().as_slice(),
            &[vec![]]
        );
        assert!(fixture.events().iter().any(|event| {
            matches!(&event.kind, AgentEventKind::ToolCatalogCommitted { digest }
                if digest == &ToolCatalog::new([]).unwrap().digest_hex())
        }));
        assert!(fixture
            .events()
            .iter()
            .all(|event| !matches!(event.kind, AgentEventKind::ToolStarted { .. })));
    }

    #[tokio::test]
    async fn model_in_flight_retries_same_step_without_duplicate_turn_or_request() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let result = fixture
            .engine(PermissionMode::Allow)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        let events = fixture.events();
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
                .filter(|event| matches!(
                    event.kind,
                    AgentEventKind::ModelRequestStarted { step: 1, .. }
                ))
                .count(),
            1
        );
        assert!(events
            .windows(2)
            .all(|pair| pair[0].sequence + 1 == pair[1].sequence));
        assert_eq!(
            fixture.model.seen_histories.lock().unwrap()[0]
                .iter()
                .filter(|message| message.content == "Inspect the file")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn ready_tool_runs_once_with_original_call_id_then_finishes_turn() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let result = fixture
            .engine(PermissionMode::Allow)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.disposition, ResumeDisposition::Complete);
        assert_eq!(
            fixture.tools.calls.lock().unwrap().as_slice(),
            &[call("call-1")]
        );
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        let events = fixture.events();
        assert_eq!(events.iter().filter(|event| matches!(&event.kind, AgentEventKind::ToolStarted { tool_call } if tool_call.id == "call-1")).count(), 1);
        assert_eq!(events.iter().filter(|event| matches!(&event.kind, AgentEventKind::ToolFinished { tool_call_id, .. } if tool_call_id == "call-1")).count(), 1);
        let seen = &fixture.model.seen_histories.lock().unwrap()[0];
        assert!(seen
            .iter()
            .any(|message| message.tool_calls.iter().any(|call| call.id == "call-1")));
        assert!(seen
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("call-1")));
    }

    #[tokio::test]
    async fn pending_permission_reprompts_same_id_and_resolves_once() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        fixture.append(AgentEventKind::PermissionRequested {
            request_id: "permission-1".into(),
            tool_call: call("call-1"),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Ask);
        let pending = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            pending.disposition,
            ResumeDisposition::NeedsPermission {
                request_id: "permission-1".into(),
                tool_call: call("call-1")
            }
        );
        assert!(pending.appended_events.is_empty());
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        engine
            .resolve_permission(&lease, "permission-1", PermissionChoice::Allow)
            .unwrap();
        assert!(matches!(
            engine.resolve_permission(&lease, "permission-1", PermissionChoice::Allow),
            Err(ResumeError::StaleResolution)
        ));
        let completed = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.tools.calls.lock().unwrap().len(), 1);
        let events = fixture.events();
        assert_eq!(events.iter().filter(|event| matches!(&event.kind, AgentEventKind::PermissionRequested { request_id, .. } if request_id == "permission-1")).count(), 1);
        assert_eq!(events.iter().filter(|event| matches!(&event.kind, AgentEventKind::PermissionResolved { request_id, .. } if request_id == "permission-1")).count(), 1);
    }

    #[tokio::test]
    async fn a_question_asked_before_a_restart_takes_the_recorded_answer_as_its_result() {
        let ask = ToolCall {
            id: "call-ask".into(),
            name: crate::ASK_USER_TOOL.into(),
            arguments: json!({"questions": [{
                "header": "Storage",
                "question": "Where should it be kept?",
                "options": [{"label": "SQLite"}, {"label": "Files"}],
                "multi_select": false
            }]}),
        };
        let fixture = Fixture::new(vec![
            ModelOutput {
                content: String::new(),
                tool_calls: vec![ask.clone()],
            },
            done(),
        ]);
        fixture.admit();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        // Deny by default, as the host's policy is for names it does not
        // list: the question is not a permission and is asked regardless.
        let engine = fixture.engine(PermissionMode::Deny).with_user_interaction(true);
        let paused = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        let ResumeDisposition::NeedsPermission { request_id, tool_call } = paused.disposition
        else {
            panic!("the turn waits for the user: {:?}", paused.disposition);
        };
        assert_eq!(tool_call, ask);
        let committed = fixture
            .events()
            .into_iter()
            .find_map(|event| match event.kind {
                AgentEventKind::ToolCatalogCommitted { digest } => Some(digest),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            committed,
            ToolCatalog::new([
                definition(),
                plan_tool::definition(),
                crate::ask_user_tool_definition()
            ])
            .unwrap()
            .digest_hex()
        );

        // The app restarts with the question open; the answer is recorded
        // first and then the same request is resolved.
        let reopened = Arc::new(SessionStore::open(fixture._dir.path()).unwrap());
        let mut resumed = fixture.engine(PermissionMode::Deny).with_user_interaction(true);
        resumed.sessions = reopened.clone();
        let again = resumed
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            again.disposition,
            ResumeDisposition::NeedsPermission {
                request_id: request_id.clone(),
                tool_call: ask.clone()
            }
        );
        let answer = r#"{"answered_by":"user","answers":[{"header":"Storage","selected":["Files"]}]}"#;
        reopened
            .record_user_answer(&fixture.session_id, &request_id, answer)
            .unwrap();
        resumed
            .resolve_permission(&lease, &request_id, PermissionChoice::Allow)
            .unwrap();
        let completed = resumed
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        let events = reopened.load(&fixture.session_id).unwrap().unwrap().events;
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { tool_call_id, output, is_error: false }
                if tool_call_id == "call-ask" && output == answer
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::PermissionRequested { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_turn_frozen_before_the_questions_existed_keeps_its_catalog() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Allow).with_user_interaction(true);
        let replay = engine.inspect(&lease).unwrap();
        assert_eq!(
            engine.verified_catalog(&replay).unwrap().digest_hex(),
            ToolCatalog::new([definition()]).unwrap().digest_hex()
        );
        let completed = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.tools.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn session_approval_resolution_recovers_after_decision_only_crash() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        fixture.append(AgentEventKind::PermissionRequested {
            request_id: "permission-1".into(),
            tool_call: call("call-1"),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture
            .engine(PermissionMode::Ask)
            .with_session_approvals(fixture.sessions.clone(), CommandIsolation::HostUnrestricted);
        let replay = engine.inspect(&lease).unwrap();
        let catalog = engine.verified_catalog(&replay).unwrap();
        let digest = engine
            .session_approval_digest(&replay, &catalog)
            .unwrap()
            .unwrap();
        let workspace = fixture
            .sessions
            .load(&fixture.session_id)
            .unwrap()
            .unwrap()
            .session
            .workspace;
        let grant = engine
            .permission_policy
            .issue_session_grant(
                &fixture.session_id,
                &workspace,
                &workspace,
                &digest,
                &call("call-1"),
            )
            .unwrap();
        // Simulate a crash after the synced decision, before PermissionResolved.
        fixture
            .sessions
            .record_permission_decision_with_grant(
                &fixture.session_id,
                "permission-1",
                PermissionDecision::Allow,
                Some(grant.into()),
            )
            .unwrap();
        let reopened = Arc::new(SessionStore::open(fixture._dir.path()).unwrap());
        let mut resumed = fixture.engine(PermissionMode::Ask);
        resumed.sessions = reopened.clone();
        resumed.session_approvals = Some((reopened.clone(), CommandIsolation::HostUnrestricted));
        resumed
            .resolve_permission(&lease, "permission-1", PermissionChoice::AllowSession)
            .unwrap();
        let completed = resumed
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.tools.calls.lock().unwrap().len(), 1);
        assert_eq!(
            reopened
                .session_approval_grants(&fixture.session_id)
                .unwrap()
                .len(),
            1
        );
        let events = reopened.load(&fixture.session_id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::PermissionResolved { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn replayed_session_grant_auto_allows_exact_ready_call() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture
            .engine(PermissionMode::Ask)
            .with_session_approvals(fixture.sessions.clone(), CommandIsolation::HostUnrestricted);
        let replay = engine.inspect(&lease).unwrap();
        let catalog = engine.verified_catalog(&replay).unwrap();
        let digest = engine
            .session_approval_digest(&replay, &catalog)
            .unwrap()
            .unwrap();
        let workspace = fixture
            .sessions
            .load(&fixture.session_id)
            .unwrap()
            .unwrap()
            .session
            .workspace;
        let grant = engine
            .permission_policy
            .issue_session_grant(
                &fixture.session_id,
                &workspace,
                &workspace,
                &digest,
                &call("call-1"),
            )
            .unwrap();
        fixture
            .sessions
            .record_permission_decision_with_grant(
                &fixture.session_id,
                "earlier-request",
                PermissionDecision::Allow,
                Some(grant.into()),
            )
            .unwrap();
        let reopened = Arc::new(SessionStore::open(fixture._dir.path()).unwrap());
        let mut resumed = fixture.engine(PermissionMode::Ask);
        resumed.sessions = reopened.clone();
        resumed.session_approvals = Some((reopened.clone(), CommandIsolation::HostUnrestricted));
        let completed = resumed
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.tools.calls.lock().unwrap().len(), 1);
        assert!(!reopened
            .load(&fixture.session_id)
            .unwrap()
            .unwrap()
            .events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::PermissionRequested { .. })));
    }

    #[tokio::test]
    async fn queued_image_steer_after_committed_response_reopens_into_next_model_request() {
        let fixture = Fixture::new(vec![ModelOutput {
            content: "Revised".into(),
            tool_calls: vec![],
        }]);
        fixture.started();
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("Initial", vec![]),
            response_id: None,
            usage: None,
        });
        fixture.append(AgentEventKind::AssistantMessage {
            content: "Initial".into(),
        });
        let image = openide_core::UserImage::from_bytes(
            openide_core::UserImageMime::Png,
            b"\x89PNG\r\n\x1a\nresume-image",
        )
        .unwrap();
        fixture
            .sessions
            .enqueue_native_steer_with_images(
                &fixture.session_id,
                "turn-1",
                "steer-after-response",
                "Please revise the answer",
                std::slice::from_ref(&image),
            )
            .unwrap();
        let reopened = Arc::new(SessionStore::open(fixture._dir.path()).unwrap());
        let lease = reopened
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let mut engine = fixture.engine(PermissionMode::Allow);
        engine.sessions = reopened.clone();
        let completed = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.model.seen_histories.lock().unwrap()[0]
                .iter()
                .filter(|message| message.content == "Please revise the answer")
                .count(),
            1
        );
        assert_eq!(
            fixture.model.seen_histories.lock().unwrap()[0]
                .iter()
                .find(|message| message.content == "Please revise the answer")
                .unwrap()
                .images,
            vec![image]
        );
        let events = reopened.load(&fixture.session_id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnSteeredWithImages { .. }))
                .count(),
            1
        );
        assert!(matches!(
            &events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { response, steps: 2 } if response == "Revised"
        ));
    }

    #[tokio::test]
    async fn nested_project_rules_reopen_from_tool_snapshot_after_file_changes() {
        let fixture = Fixture::new(vec![done()]);
        let nested = fixture._dir.path().join("src");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("AGENTS.md"), "Original src rule").unwrap();
        std::fs::write(nested.join("file.txt"), "payload").unwrap();
        fixture.started();
        let call = ToolCall {
            id: "nested-read".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/file.txt"}),
        };
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call]),
            response_id: None,
            usage: None,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Allow);
        let step = engine
            .resume_step(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(step.disposition, ResumeDisposition::Advanced);
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert!(matches!(
            &fixture.events().last().unwrap().kind,
            AgentEventKind::ToolFinished { output, is_error: true, .. }
                if output.starts_with("OPENIDE_PROJECT_INSTRUCTIONS_V1\n")
        ));
        drop(lease);
        std::fs::write(nested.join("AGENTS.md"), "Changed src rule").unwrap();
        let reopened = Arc::new(SessionStore::open(fixture._dir.path()).unwrap());
        let lease = reopened
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let mut engine = fixture.engine(PermissionMode::Allow);
        engine.sessions = reopened;
        let completed = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        let seen = fixture.model.seen_histories.lock().unwrap();
        assert!(seen[0].iter().any(|message| {
            message.content.contains("Original src rule")
                && !message.content.contains("Changed src rule")
        }));
    }

    #[tokio::test]
    async fn completion_fence_crash_reopens_without_a_second_model_request() {
        let fixture = Fixture::new(vec![]);
        fixture.started();
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("Finished", vec![]),
            response_id: None,
            usage: None,
        });
        fixture.append(AgentEventKind::AssistantMessage {
            content: "Finished".into(),
        });
        let first_lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        fixture
            .sessions
            .fence_native_turn_completion(&first_lease, "turn-1")
            .unwrap();
        drop(first_lease);
        let reopened = Arc::new(SessionStore::open(fixture._dir.path()).unwrap());
        assert!(reopened
            .native_turn_finish_fence(&fixture.session_id, "turn-1")
            .unwrap());
        let lease = reopened
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let mut engine = fixture.engine(PermissionMode::Allow);
        engine.sessions = reopened.clone();
        let completed = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            &reopened.load(&fixture.session_id).unwrap().unwrap().events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { response, steps: 1 } if response == "Finished"
        ));
    }

    #[tokio::test]
    async fn uncertain_tool_blocks_until_verified_manual_result() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("call-1"),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Allow);
        let blocked = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            matches!(blocked.disposition, ResumeDisposition::ToolOutcomeUncertain { tool_call, .. } if tool_call.id == "call-1")
        );
        assert!(blocked.appended_events.is_empty());
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert!(matches!(
            engine.record_verified_tool_outcome(&lease, "wrong-id", ToolOutput::success("ok")),
            Err(ResumeError::StaleResolution)
        ));
        engine
            .record_verified_tool_outcome(&lease, "call-1", ToolOutput::success("confirmed"))
            .unwrap();
        assert_eq!(
            engine
                .run_until_pause(&lease, CancellationToken::new())
                .await
                .unwrap()
                .disposition,
            ResumeDisposition::Complete
        );
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ready_tool_with_ask_policy_syncs_permission_before_any_effect() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let pending = fixture
            .engine(PermissionMode::Ask)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(
            pending.disposition,
            ResumeDisposition::NeedsPermission { .. }
        ));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert_eq!(pending.appended_events.len(), 1);
        assert!(matches!(
            pending.appended_events[0].kind,
            AgentEventKind::PermissionRequested { .. }
        ));
    }

    #[tokio::test]
    async fn executor_failure_stays_uncertain_and_is_not_retried() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        *fixture.tools.result.lock().unwrap() = Some(Err("device outcome unknown".into()));
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Allow);
        let blocked = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            matches!(blocked.disposition, ResumeDisposition::ToolOutcomeUncertain { executor_error: Some(error), .. } if error.contains("device outcome unknown"))
        );
        assert_eq!(fixture.tools.calls.lock().unwrap().len(), 1);
        let repeated = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(
            repeated.disposition,
            ResumeDisposition::ToolOutcomeUncertain { .. }
        ));
        assert_eq!(fixture.tools.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn partial_batch_does_not_repeat_completed_tool_call() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started();
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("first"), call("second")]),
            response_id: None,
            usage: None,
        });
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("first"),
        });
        fixture.append(AgentEventKind::ToolFinished {
            tool_call_id: "first".into(),
            output: "previously committed".into(),
            is_error: false,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let completed = fixture
            .engine(PermissionMode::Allow)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(
            fixture.tools.calls.lock().unwrap().as_slice(),
            &[call("second")]
        );
        let history = &fixture.model.seen_histories.lock().unwrap()[0];
        assert_eq!(
            history
                .iter()
                .filter(|message| message.tool_call_id.as_deref() == Some("first"))
                .count(),
            1
        );
        assert_eq!(
            history
                .iter()
                .filter(|message| message.tool_call_id.as_deref() == Some("second"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_append_cannot_commit_stale_model_response() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started();
        *fixture.model.interfere.lock().unwrap() =
            Some((fixture.sessions.clone(), fixture.session_id.clone()));
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Allow);
        assert!(matches!(
            engine.resume_step(&lease, CancellationToken::new()).await,
            Err(ResumeError::Store(_))
        ));
        let replay = engine.inspect(&lease).unwrap();
        assert!(matches!(
            replay.recovery,
            ReplayRecovery::ModelInFlight { step: 1, .. }
        ));
        assert!(fixture
            .events()
            .iter()
            .all(|event| !matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. })));
    }

    #[tokio::test]
    async fn invalid_catalog_call_is_recorded_as_error_without_execution() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started();
        let mut invalid = call("invalid");
        invalid.arguments = json!({"path": 17});
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![invalid.clone()]),
            response_id: None,
            usage: None,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let completed = fixture
            .engine(PermissionMode::Allow)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert!(fixture.events().iter().any(|event| matches!(&event.kind, AgentEventKind::ToolFinished { tool_call_id, is_error: true, .. } if tool_call_id == "invalid")));
    }

    #[tokio::test]
    async fn terminal_turn_never_restarts_model_after_manual_tool_resolution() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("call-1"),
        });
        fixture.append(AgentEventKind::TurnFailed {
            error: "process ended".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Allow);
        assert!(matches!(
            engine
                .run_until_pause(&lease, CancellationToken::new())
                .await
                .unwrap()
                .disposition,
            ResumeDisposition::ToolOutcomeUncertain { .. }
        ));
        let resolved = engine
            .record_verified_tool_outcome(&lease, "call-1", ToolOutput::success("verified"))
            .unwrap();
        assert_eq!(resolved.disposition, ResumeDisposition::Complete);
        assert!(resolved.replay.can_start_new_turn);
        assert_eq!(
            engine
                .run_until_pause(&lease, CancellationToken::new())
                .await
                .unwrap()
                .disposition,
            ResumeDisposition::Complete
        );
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn admitted_turn_reuses_prompt_and_id_after_restart() {
        let fixture = Fixture::new(vec![done()]);
        fixture.admit();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let completed = fixture
            .engine(PermissionMode::Allow)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        let events = fixture.events();
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
                .filter(|event| matches!(event.kind, AgentEventKind::TurnStarted { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::SessionStarted { .. }))
                .count(),
            1
        );
        assert_eq!(
            fixture.model.seen_histories.lock().unwrap()[0]
                .iter()
                .filter(|message| message.content == "Inspect the file")
                .count(),
            1
        );
        let marker = events
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ModelContextCommitted { .. }))
            .unwrap();
        let request = events
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. }))
            .unwrap();
        assert!(marker < request);
    }

    #[tokio::test]
    async fn unverified_window_can_resume_without_automatic_compaction() {
        let fixture = Fixture::new(vec![done()]);
        fixture.admit();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let mut engine = fixture.engine(PermissionMode::Allow);
        engine.budget_policy = None;
        let completed = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        assert!(!fixture
            .events()
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::CheckpointCommitted { .. })));
    }

    #[tokio::test]
    async fn committed_final_response_finishes_without_requerying_model() {
        let fixture = Fixture::new(vec![]);
        fixture.started();
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("Already answered", vec![]),
            response_id: Some("response-1".into()),
            usage: None,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let completed = fixture
            .engine(PermissionMode::Allow)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
        let events = fixture.events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. }))
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
    }

    #[tokio::test]
    async fn denied_reprompt_records_result_without_executing_tool() {
        let fixture = Fixture::new(vec![done()]);
        fixture.tool_ready();
        fixture.append(AgentEventKind::PermissionRequested {
            request_id: "permission-1".into(),
            tool_call: call("call-1"),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Ask);
        engine
            .resolve_permission(&lease, "permission-1", PermissionChoice::Deny)
            .unwrap();
        let completed = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert!(fixture.events().iter().any(|event| matches!(&event.kind, AgentEventKind::ToolFinished { tool_call_id, is_error: true, .. } if tool_call_id == "call-1")));
    }

    #[tokio::test]
    async fn legacy_model_flight_without_catalog_marker_fails_closed() {
        let fixture = Fixture::new(vec![done()]);
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "gpt-example".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        assert!(matches!(
            fixture
                .engine(PermissionMode::Allow)
                .resume_step(&lease, CancellationToken::new())
                .await,
            Err(ResumeError::ContextUnverified)
        ));
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn model_route_snapshot_resume_rejects_changed_route_before_any_append_or_effect() {
        struct OtherModel(Arc<MockModel>);
        #[async_trait]
        impl ModelBackend for OtherModel {
            fn model_id(&self) -> &str {
                "different-exact-model"
            }
            fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
                Some([7; 32])
            }
            async fn complete(
                &self,
                messages: &[AgentMessage],
                tools: &[ToolDefinition],
                cancel: CancellationToken,
            ) -> Result<ModelOutput, AgentError> {
                self.0.complete(messages, tools, cancel).await
            }
        }
        for changed in ["provider", "endpoint", "profile", "model"] {
            let fixture = Fixture::new(vec![done()]);
            fixture.started_with_model_route(true);
            let before = fixture.events();
            let mut engine = match changed {
                "provider" => fixture
                    .engine(PermissionMode::Allow)
                    .with_native_runtime_identity("different-provider", "https://test.example/v1"),
                "endpoint" => fixture
                    .engine(PermissionMode::Allow)
                    .with_native_runtime_identity("openai", "https://different.example/v1"),
                _ => fixture.engine(PermissionMode::Allow),
            };
            if changed == "profile" {
                *fixture.model.request_profile.lock().unwrap() = [8; 32];
            }
            if changed == "model" {
                engine.model = Arc::new(OtherModel(fixture.model.clone()));
            }
            let lease = fixture
                .sessions
                .try_acquire_native_lease(&fixture.session_id)
                .unwrap();
            let outcome = engine.resume_step(&lease, CancellationToken::new()).await;
            if changed == "model" {
                assert!(matches!(outcome, Err(ResumeError::ModelMismatch)));
            } else {
                assert!(
                    matches!(outcome, Err(ResumeError::ModelRouteMismatch)),
                    "{changed}: {outcome:?}"
                );
            }
            assert_eq!(
                fixture.events(),
                before,
                "changed route mutated recovery journal"
            );
            assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
            assert!(fixture.tools.calls.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn model_route_snapshot_crash_before_first_request_reopens_and_reuses_exact_route() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started_with_model_route(false);
        let before = fixture.events();
        let reopened_sessions = Arc::new(SessionStore::open(fixture._dir.path()).unwrap());
        let reopened_checkpoints = Arc::new(CheckpointFileStore::new(fixture._dir.path()).unwrap());
        let engine = ResumeEngine::new(
            reopened_sessions.clone(),
            reopened_checkpoints,
            compatibility(),
            fixture.model.clone(),
            fixture.tools.clone(),
            ToolCatalog::new([definition()]).unwrap(),
            PermissionPolicy::new().with_default_mode(PermissionMode::Allow),
            budget(),
        )
        .with_native_runtime_identity("openai", "https://test.example/v1");
        let lease = reopened_sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        assert_eq!(
            engine.inspect(&lease).unwrap().active_model_route,
            openide_core::ModelRouteSnapshot::from_event(&before.last().unwrap().kind)
        );
        let completed = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        let events = fixture.events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ModelRouteCommitted { .. }))
                .count(),
            1
        );
        assert!(completed.replay.active_model_route.is_none());
        assert_eq!(&events[..before.len()], before.as_slice());
    }

    #[tokio::test]
    async fn first_request_recovery_reconciles_prior_route_without_losing_text_or_tools() {
        for changed_route in [false, true] {
            for crash_after in ["admission", "context", "route"] {
                let fixture = Fixture::new(vec![done()]);
                let prior_model = if changed_route {
                    "prior-model"
                } else {
                    "gpt-example"
                };
                let prior_provider = if changed_route {
                    "prior-provider"
                } else {
                    "openai"
                };
                let prior_route = openide_core::ModelRouteSnapshot::new(
                    prior_provider,
                    prior_model,
                    "https://test.example/v1",
                    [7; 32],
                )
                .unwrap();
                let prior_call = ToolCall {
                    id: "prior-read".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "README.md"}),
                };
                let mut prior_assistant = AgentMessage::assistant(
                    "Reading the project instructions",
                    vec![prior_call.clone()],
                );
                prior_assistant.provider_output_items = vec![
                    json!({"type": "reasoning", "encrypted_content": "opaque-prior-route-data"}),
                    json!({"type": "function_call", "call_id": prior_call.id, "name": prior_call.name, "arguments": "{\"path\":\"README.md\"}"}),
                ];
                fixture
                    .sessions
                    .admit_native_turn(
                        &fixture.session_id,
                        "prior-turn",
                        "Inspect the existing project",
                        prior_model,
                        TurnMode::Build,
                    )
                    .unwrap();
                for event in [
                    AgentEventKind::TurnStarted {
                        prompt: "Inspect the existing project".into(),
                    },
                    AgentEventKind::ToolCatalogCommitted {
                        digest: ToolCatalog::new([definition()]).unwrap().digest_hex(),
                    },
                    AgentEventKind::ModelContextCommitted {
                        version: MODEL_CONTEXT_VERSION,
                        digest: "a".repeat(64),
                    },
                    prior_route.into_event(),
                    AgentEventKind::ModelRequestStarted {
                        step: 1,
                        model: prior_model.into(),
                    },
                    AgentEventKind::ModelResponseCommitted {
                        step: 1,
                        assistant: prior_assistant.clone(),
                        response_id: Some("prior-response".into()),
                        usage: None,
                    },
                    AgentEventKind::ToolStarted {
                        tool_call: prior_call.clone(),
                    },
                    AgentEventKind::ToolFinished {
                        tool_call_id: prior_call.id.clone(),
                        output: "Retained README contents".into(),
                        is_error: false,
                    },
                    AgentEventKind::ModelRequestStarted {
                        step: 2,
                        model: prior_model.into(),
                    },
                    AgentEventKind::ModelResponseCommitted {
                        step: 2,
                        assistant: AgentMessage::assistant(
                            "Prior project review completed",
                            vec![],
                        ),
                        response_id: None,
                        usage: None,
                    },
                    AgentEventKind::TurnCompleted {
                        response: "Prior project review completed".into(),
                        steps: 2,
                    },
                ] {
                    fixture.append(event);
                }
                fixture.admit();
                if crash_after != "admission" {
                    fixture.append(AgentEventKind::TurnStarted {
                        prompt: "Inspect the file".into(),
                    });
                    fixture.append(AgentEventKind::ToolCatalogCommitted {
                        digest: ToolCatalog::new([definition()]).unwrap().digest_hex(),
                    });
                    fixture.append(AgentEventKind::HooksCommitted {
                        version: HOOK_CONFIG_IDENTITY_VERSION,
                        digest: CommandHooks::default().digest_hex().unwrap(),
                    });
                    fixture.append(AgentEventKind::ModelContextCommitted {
                        version: MODEL_CONTEXT_VERSION,
                        digest: fixture.context_digest(),
                    });
                }
                if crash_after == "route" {
                    fixture.append(
                        openide_core::ModelRouteSnapshot::new(
                            "openai",
                            "gpt-example",
                            "https://test.example/v1",
                            [7; 32],
                        )
                        .unwrap()
                        .into_event(),
                    );
                }
                let before = fixture.events();
                let reopened = Arc::new(SessionStore::open(fixture._dir.path()).unwrap());
                let engine = ResumeEngine::new(
                    reopened.clone(),
                    Arc::new(CheckpointFileStore::new(fixture._dir.path()).unwrap()),
                    compatibility(),
                    fixture.model.clone(),
                    fixture.tools.clone(),
                    ToolCatalog::new([definition()]).unwrap(),
                    PermissionPolicy::new().with_default_mode(PermissionMode::Allow),
                    budget(),
                )
                .with_native_runtime_identity("openai", "https://test.example/v1");
                let lease = reopened
                    .try_acquire_native_lease(&fixture.session_id)
                    .unwrap();
                let outcome = engine
                    .run_until_pause(&lease, CancellationToken::new())
                    .await
                    .unwrap();
                assert_eq!(outcome.disposition, ResumeDisposition::Complete);
                assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
                assert!(
                    fixture.tools.calls.lock().unwrap().is_empty(),
                    "old reads must not run again"
                );
                let histories = fixture.model.seen_histories.lock().unwrap();
                let messages = &histories[0];
                let assistant = messages
                    .iter()
                    .find(|message| message.tool_calls == vec![prior_call.clone()])
                    .expect("the exact tool call remains");
                assert_eq!(assistant.content, prior_assistant.content);
                assert_eq!(
                    assistant.provider_output_items,
                    if changed_route {
                        Vec::new()
                    } else {
                        prior_assistant.provider_output_items.clone()
                    },
                    "changed={changed_route}, crash={crash_after}"
                );
                assert!(messages
                    .iter()
                    .any(
                        |message| message.tool_call_id.as_deref() == Some("prior-read")
                            && message.content == "Retained README contents"
                    ));
                for content in [
                    "Inspect the existing project",
                    "Prior project review completed",
                    "Inspect the file",
                ] {
                    assert!(
                        messages.iter().any(|message| message.content == content),
                        "missing {content}, crash={crash_after}"
                    );
                }
                let events = fixture.events();
                assert_eq!(
                    &events[..before.len()],
                    before.as_slice(),
                    "original journal remains immutable"
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| matches!(
                            event.kind,
                            AgentEventKind::ModelRouteCommitted { .. }
                        ))
                        .count(),
                    2
                );
                let replayed = engine.inspect(&lease).unwrap();
                let replayed_assistant = replayed
                    .history
                    .iter()
                    .find(|message| message.tool_calls == vec![prior_call.clone()])
                    .unwrap();
                assert_eq!(
                    replayed_assistant, assistant,
                    "restart and the actual request agree"
                );
            }
        }
    }

    #[tokio::test]
    async fn model_route_snapshot_legacy_retry_remains_without_retrospective_attribution() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started();
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let completed = fixture
            .engine(PermissionMode::Allow)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert!(fixture
            .events()
            .iter()
            .all(|event| !matches!(event.kind, AgentEventKind::ModelRouteCommitted { .. })));
        assert!(completed.replay.active_model_route.is_none());
    }

    #[tokio::test]
    async fn legacy_model_flight_with_catalog_but_without_context_fails_closed() {
        let fixture = Fixture::new(vec![done()]);
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ToolCatalogCommitted {
            digest: ToolCatalog::new([definition()]).unwrap().digest_hex(),
        });
        fixture.append(AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "gpt-example".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        assert!(matches!(
            fixture
                .engine(PermissionMode::Allow)
                .resume_step(&lease, CancellationToken::new())
                .await,
            Err(ResumeError::ContextUnverified)
        ));
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn changed_provider_endpoint_or_skills_cannot_retry_model() {
        for changed in ["provider", "endpoint", "skills"] {
            let fixture = Fixture::new(vec![done()]);
            fixture.started();
            let lease = fixture
                .sessions
                .try_acquire_native_lease(&fixture.session_id)
                .unwrap();
            let engine = match changed {
                "provider" => fixture
                    .engine(PermissionMode::Allow)
                    .with_native_runtime_identity("other", "https://test.example/v1"),
                "endpoint" => fixture
                    .engine(PermissionMode::Allow)
                    .with_native_runtime_identity("openai", "https://other.example/v1"),
                "skills" => fixture
                    .engine(PermissionMode::Allow)
                    .with_system_instructions("changed skill text"),
                _ => unreachable!(),
            };
            assert!(matches!(
                engine.resume_step(&lease, CancellationToken::new()).await,
                Err(ResumeError::ContextMismatch)
            ));
            assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn committed_instructions_survive_project_file_change_on_model_retry() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started_with_instruction_snapshot("Original AGENTS.md");
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let completed = fixture
            .engine(PermissionMode::Allow)
            .with_system_instructions("Changed AGENTS.md")
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        let seen = fixture.model.seen_histories.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0][0].content.contains("Original AGENTS.md"));
        assert!(!seen[0][0].content.contains("Changed AGENTS.md"));
    }

    #[tokio::test]
    async fn committed_instructions_survive_project_file_change_after_tool_result() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started_with_instruction_snapshot("Original AGENTS.md");
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("call-1")]),
            response_id: Some("response-1".into()),
            usage: None,
        });
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("call-1"),
        });
        fixture.append(AgentEventKind::ToolFinished {
            tool_call_id: "call-1".into(),
            output: "verified read".into(),
            is_error: false,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let completed = fixture
            .engine(PermissionMode::Allow)
            .with_system_instructions("Changed AGENTS.md")
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        let seen = fixture.model.seen_histories.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0][0].content.contains("Original AGENTS.md"));
        assert!(!seen[0][0].content.contains("Changed AGENTS.md"));
    }

    #[tokio::test]
    async fn crash_after_instruction_snapshot_reuses_it_before_first_model_request() {
        let fixture = Fixture::new(vec![done()]);
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ToolCatalogCommitted {
            digest: ToolCatalog::new([definition()]).unwrap().digest_hex(),
        });
        fixture.append(AgentEventKind::HooksCommitted {
            version: HOOK_CONFIG_IDENTITY_VERSION,
            digest: CommandHooks::default().digest_hex().unwrap(),
        });
        fixture.append(AgentEventKind::ExecPolicyCommitted {
            version: EXEC_POLICY_IDENTITY_VERSION,
            digest: PermissionPolicy::new()
                .with_default_mode(PermissionMode::Allow)
                .digest_hex(),
        });
        fixture.append(AgentEventKind::SystemInstructionsCommitted {
            version: SYSTEM_INSTRUCTIONS_VERSION,
            digest: system_instructions_digest("Original AGENTS.md").unwrap(),
            content: "Original AGENTS.md".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let completed = fixture
            .engine(PermissionMode::Allow)
            .with_system_instructions("Changed AGENTS.md")
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        let seen = fixture.model.seen_histories.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0][0].content.contains("Original AGENTS.md"));
        let saved = fixture.events();
        assert_eq!(
            saved
                .iter()
                .filter(|event| {
                    matches!(
                        event.kind,
                        AgentEventKind::SystemInstructionsCommitted { .. }
                    )
                })
                .count(),
            1
        );
        assert!(saved
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ModelContextCommitted { .. })));
    }

    #[tokio::test]
    async fn changed_request_profile_cannot_retry_an_in_flight_model() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started();
        *fixture.model.request_profile.lock().unwrap() = [8; 32];
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        assert!(matches!(
            fixture
                .engine(PermissionMode::Allow)
                .resume_step(&lease, CancellationToken::new())
                .await,
            Err(ResumeError::ContextMismatch)
        ));
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn v1_context_log_is_readable_and_discardable_but_cannot_retry_model() {
        let fixture = Fixture::new(vec![done()]);
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ToolCatalogCommitted {
            digest: ToolCatalog::new([definition()]).unwrap().digest_hex(),
        });
        fixture.append(AgentEventKind::ModelContextCommitted {
            version: 1,
            digest: "a".repeat(64),
        });
        fixture.append(AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "gpt-example".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let replay = replay_native(
            &fixture.session_id,
            &fixture.events(),
            Some(&fixture.checkpoints),
            &compatibility(),
        )
        .unwrap();
        assert_eq!(replay.active_model_context_version, Some(1));
        assert!(matches!(
            fixture
                .engine(PermissionMode::Allow)
                .resume_step(&lease, CancellationToken::new())
                .await,
            Err(ResumeError::ContextVersionUnsupported)
        ));
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("call-1")]),
            response_id: None,
            usage: None,
        });
        fixture.append(AgentEventKind::TurnFailed {
            error: "legacy interrupted".into(),
        });
        let discarded =
            discard_aborted_tool_batch(&fixture.sessions, &fixture.checkpoints, &lease, "call-1")
                .unwrap();
        assert!(discarded.replay.can_start_new_turn);
    }

    #[tokio::test]
    async fn committed_context_before_request_is_reused_after_crash() {
        let fixture = Fixture::new(vec![done()]);
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ToolCatalogCommitted {
            digest: ToolCatalog::new([definition()]).unwrap().digest_hex(),
        });
        fixture.append(AgentEventKind::ModelContextCommitted {
            version: MODEL_CONTEXT_VERSION,
            digest: fixture.context_digest(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let completed = fixture
            .engine(PermissionMode::Allow)
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(completed.disposition, ResumeDisposition::Complete);
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .events()
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ModelContextCommitted { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn recovery_compacts_completed_prefix_before_retrying_model() {
        let summary = ModelOutput {
            content: json!({
                "objective": "Continue the Rust harness",
                "decisions": ["Keep an append-only journal"],
                "files": [],
                "current_state": "One turn completed; another model request is in flight",
                "constraints": ["Preserve the active turn"],
                "remaining_work": ["Finish the current response"]
            })
            .to_string(),
            tool_calls: vec![],
        };
        let fixture = Fixture::new(vec![summary, done()]);
        // Count the three different requests explicitly: the original
        // transcript crosses the configured threshold, the summary request
        // fits its own hard budget, and the checkpoint-refilled request fits.
        // The default 100-token fixture would never request compaction now
        // that text requests also use the provider's counter.
        *fixture.model.input_counts.lock().unwrap() = vec![4_000, 100, 100];
        fixture
            .sessions
            .admit_native_turn(
                &fixture.session_id,
                "turn-before",
                "Old request",
                "gpt-example",
                TurnMode::Build,
            )
            .unwrap();
        let large_response = "Historical result ".repeat(250);
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Old request".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant(large_response.clone(), vec![]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::AssistantMessage {
                content: large_response.clone(),
            },
            AgentEventKind::TurnCompleted {
                response: large_response,
                steps: 1,
            },
        ] {
            fixture.append(kind);
        }
        fixture
            .sessions
            .admit_native_turn(
                &fixture.session_id,
                "turn-current",
                "Inspect the file",
                "gpt-example",
                TurnMode::Build,
            )
            .unwrap();
        let original_instructions = "Original AGENTS.md";
        let catalog = ToolCatalog::new([definition()]).unwrap();
        let record = fixture.sessions.load(&fixture.session_id).unwrap().unwrap();
        let context_digest = crate::native_model_context_digest(
            "openai",
            "https://test.example/v1",
            "gpt-example",
            TurnMode::Build,
            &record.session.workspace,
            &catalog,
            original_instructions,
            [7; 32],
        )
        .unwrap();
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Inspect the file".into(),
            },
            AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            },
            AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
            AgentEventKind::ExecPolicyCommitted {
                version: EXEC_POLICY_IDENTITY_VERSION,
                digest: PermissionPolicy::new()
                    .with_default_mode(PermissionMode::Allow)
                    .digest_hex(),
            },
            AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest: system_instructions_digest(original_instructions).unwrap(),
                content: original_instructions.into(),
            },
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: context_digest,
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            },
        ] {
            fixture.append(kind);
        }
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let mut engine = fixture
            .engine(PermissionMode::Allow)
            .with_system_instructions("Changed AGENTS.md");
        engine.budget_policy = Some(TokenBudgetPolicy {
            input_limit_tokens: None,
            context_window_tokens: 8_000,
            output_reserve_tokens: 512,
            tool_schema_reserve_tokens: 128,
            auto_compact_token_limit: Some(2_500),
        });
        let result = engine
            .run_until_pause(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.disposition, ResumeDisposition::Complete);
        assert!(fixture
            .events()
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::CheckpointCommitted { .. })));
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 2);
        assert!(fixture.model.input_counts.lock().unwrap().is_empty());
        let seen = fixture.model.seen_histories.lock().unwrap();
        assert!(seen[1][0].content.contains(original_instructions));
        assert!(!seen[1][0].content.contains("Changed AGENTS.md"));
        assert!(!seen[1]
            .iter()
            .any(|message| message.content.contains("Historical result")));
        assert!(seen[1]
            .iter()
            .any(|message| message.content.contains("Inspect the file")));
    }

    #[tokio::test]
    async fn mismatched_catalog_never_executes_ready_tool() {
        let fixture = Fixture::new(vec![done()]);
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ToolCatalogCommitted {
            digest: ToolCatalog::new([]).unwrap().digest_hex(),
        });
        fixture.append(AgentEventKind::ModelContextCommitted {
            version: MODEL_CONTEXT_VERSION,
            digest: "0".repeat(64),
        });
        fixture.append(AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "gpt-example".into(),
        });
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("call-1")]),
            response_id: None,
            usage: None,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        assert!(matches!(
            fixture
                .engine(PermissionMode::Allow)
                .resume_step(&lease, CancellationToken::new())
                .await,
            Err(ResumeError::CatalogMismatch)
        ));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert!(fixture
            .events()
            .iter()
            .all(|event| !matches!(event.kind, AgentEventKind::ToolStarted { .. })));
    }

    #[tokio::test]
    async fn pending_mcp_call_fails_closed_without_server_identity() {
        let fixture = Fixture::new(vec![done()]);
        fixture.started();
        let mut mcp = call("mcp-call");
        mcp.name = "mcp__server__read".into();
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![mcp.clone()]),
            response_id: None,
            usage: None,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine = fixture.engine(PermissionMode::Allow);
        assert!(matches!(
            engine.resume_step(&lease, CancellationToken::new()).await,
            Err(ResumeError::McpIdentityUnverified)
        ));
        fixture.append(AgentEventKind::PermissionRequested {
            request_id: "mcp-permission".into(),
            tool_call: mcp,
        });
        assert!(matches!(
            engine.resume_step(&lease, CancellationToken::new()).await,
            Err(ResumeError::McpIdentityUnverified)
        ));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert!(fixture
            .events()
            .iter()
            .all(|event| !matches!(event.kind, AgentEventKind::ToolStarted { .. })));
    }

    #[tokio::test]
    async fn command_policy_snapshot_blocks_changed_recovery_and_denies_matching_argv() {
        let fixture = Fixture::new(vec![done()]);
        let definition = ToolDefinition {
            name: "start_command".into(),
            description: "Start a command".into(),
            parameters: json!({"type":"object"}),
        };
        let catalog = ToolCatalog::new([definition]).unwrap();
        let rule = crate::ExecRule::new(
            vec![crate::ExecPatternToken::Literal("git".into())],
            crate::ExecDecision::Deny,
            None,
        )
        .unwrap();
        let policy =
            PermissionPolicy::new().with_exec_policy(crate::ExecPolicy::new(vec![rule]).unwrap());
        let mut engine = fixture.engine(PermissionMode::Allow);
        engine.catalog = catalog.clone();
        engine.permission_policy = policy.clone();
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ToolCatalogCommitted {
            digest: catalog.digest_hex(),
        });
        fixture.append(AgentEventKind::HooksCommitted {
            version: HOOK_CONFIG_IDENTITY_VERSION,
            digest: CommandHooks::default().digest_hex().unwrap(),
        });
        fixture.append(AgentEventKind::ExecPolicyCommitted {
            version: EXEC_POLICY_IDENTITY_VERSION,
            digest: policy.digest_hex(),
        });
        let record = fixture.sessions.load(&fixture.session_id).unwrap().unwrap();
        fixture.append(AgentEventKind::ModelContextCommitted {
            version: MODEL_CONTEXT_VERSION,
            digest: crate::native_model_context_digest(
                "openai",
                "https://test.example/v1",
                "gpt-example",
                TurnMode::Build,
                &record.session.workspace,
                &catalog,
                "",
                [7; 32],
            )
            .unwrap(),
        });
        fixture.append(AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "gpt-example".into(),
        });
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant(
                "",
                vec![ToolCall {
                    id: "command-1".into(),
                    name: "start_command".into(),
                    arguments: json!({"command":"git", "args":["push", "origin", "main"]}),
                }],
            ),
            response_id: None,
            usage: None,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let mut changed = fixture.engine(PermissionMode::Allow);
        changed.catalog = catalog.clone();
        assert!(matches!(
            changed.inspect(&lease),
            Err(ResumeError::ExecPolicyMismatch)
        ));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        let step = engine
            .resume_step(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert!(step.appended_events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { is_error: true, output, .. }
                if output.contains("Permission denied")
        )));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn legacy_command_turn_without_policy_marker_cannot_resume_effect() {
        let fixture = Fixture::new(vec![]);
        let catalog = ToolCatalog::new([ToolDefinition {
            name: "run_command".into(),
            description: "Run a command".into(),
            parameters: json!({"type":"object"}),
        }])
        .unwrap();
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ToolCatalogCommitted {
            digest: catalog.digest_hex(),
        });
        let record = fixture.sessions.load(&fixture.session_id).unwrap().unwrap();
        fixture.append(AgentEventKind::ModelContextCommitted {
            version: MODEL_CONTEXT_VERSION,
            digest: crate::native_model_context_digest(
                "openai",
                "https://test.example/v1",
                "gpt-example",
                TurnMode::Build,
                &record.session.workspace,
                &catalog,
                "",
                [7; 32],
            )
            .unwrap(),
        });
        let mut engine = fixture.engine(PermissionMode::Allow);
        engine.catalog = catalog;
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        assert!(matches!(
            engine.inspect(&lease),
            Err(ResumeError::ExecPolicyUnverified)
        ));
        fixture.append(AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "gpt-example".into(),
        });
        assert!(matches!(
            engine.inspect(&lease),
            Err(ResumeError::ExecPolicyUnverified)
        ));
    }

    #[tokio::test]
    async fn matching_stdio_snapshot_allows_pending_mcp_after_fresh_permission() {
        let fixture = Fixture::new(vec![done()]);
        let mut mcp_definition = definition();
        mcp_definition.name = "mcp__server__read".into();
        let catalog = ToolCatalog::new([mcp_definition]).unwrap();
        let mut mcp_call = call("mcp-call");
        mcp_call.name = "mcp__server__read".into();
        fixture.admit();
        fixture.append(AgentEventKind::TurnStarted {
            prompt: "Inspect the file".into(),
        });
        fixture.append(AgentEventKind::ToolCatalogCommitted {
            digest: catalog.digest_hex(),
        });
        let saved_identity = "a".repeat(64);
        fixture.append(AgentEventKind::McpRuntimeCommitted {
            version: MCP_RUNTIME_IDENTITY_VERSION,
            digest: saved_identity.clone(),
        });
        fixture.append(AgentEventKind::HooksCommitted {
            version: HOOK_CONFIG_IDENTITY_VERSION,
            digest: CommandHooks::default().digest_hex().unwrap(),
        });
        let record = fixture.sessions.load(&fixture.session_id).unwrap().unwrap();
        let context_digest = crate::native_model_context_digest(
            "openai",
            "https://test.example/v1",
            "gpt-example",
            TurnMode::Build,
            &record.session.workspace,
            &catalog,
            "",
            [7; 32],
        )
        .unwrap();
        fixture.append(AgentEventKind::ModelContextCommitted {
            version: MODEL_CONTEXT_VERSION,
            digest: context_digest,
        });
        fixture.append(AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "gpt-example".into(),
        });
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![mcp_call.clone()]),
            response_id: None,
            usage: None,
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let engine_with = |digest: Option<String>| {
            ResumeEngine::new(
                fixture.sessions.clone(),
                fixture.checkpoints.clone(),
                compatibility(),
                fixture.model.clone(),
                fixture.tools.clone(),
                catalog.clone(),
                PermissionPolicy::new().with_default_mode(PermissionMode::Ask),
                budget(),
            )
            .with_native_runtime_identity("openai", "https://test.example/v1")
            .with_mcp_runtime_identity_digest(digest)
        };
        let mismatched = engine_with(Some("b".repeat(64)));
        assert!(matches!(
            mismatched.inspect(&lease),
            Err(ResumeError::McpIdentityMismatch)
        ));
        assert!(!fixture
            .events()
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. })));

        let matching = engine_with(Some(saved_identity));
        let request = matching
            .resume_step(&lease, CancellationToken::new())
            .await
            .unwrap();
        let ResumeDisposition::NeedsPermission {
            request_id,
            tool_call,
        } = request.disposition
        else {
            panic!("MCP call must ask again after recovery")
        };
        assert_eq!(tool_call.id, mcp_call.id);
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        matching
            .resolve_permission(&lease, &request_id, PermissionChoice::Allow)
            .unwrap();
        matching
            .resume_step(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(fixture.tools.calls.lock().unwrap().len(), 1);
        let events = fixture.events();
        let start = events
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. }))
            .unwrap();
        let finish = events
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. }))
            .unwrap();
        assert!(start < finish);
        assert_eq!(events[start].sequence + 1, events[finish].sequence);
    }

    #[test]
    fn manual_discard_is_durable_idempotent_and_needs_no_model_settings() {
        let fixture = Fixture::new(vec![]);
        fixture.started();
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("a"), call("b")]),
            response_id: None,
            usage: None,
        });
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("a"),
        });
        fixture.append(AgentEventKind::ToolFinished {
            tool_call_id: "a".into(),
            output: "confirmed effect".into(),
            is_error: false,
        });
        fixture.append(AgentEventKind::TurnFailed {
            error: "interrupted".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let before = fixture.events().len();
        assert!(matches!(
            discard_aborted_tool_batch(&fixture.sessions, &fixture.checkpoints, &lease, "wrong"),
            Err(ResumeError::StaleResolution)
        ));
        assert_eq!(fixture.events().len(), before);
        let step = discard_aborted_tool_batch(&fixture.sessions, &fixture.checkpoints, &lease, "b")
            .unwrap();
        assert_eq!(step.disposition, ResumeDisposition::Complete);
        assert_eq!(step.appended_events.len(), 1);
        assert!(matches!(
            &step.appended_events[0].kind,
            AgentEventKind::ToolBatchDiscarded { next_tool_call_id } if next_tool_call_id == "b"
        ));
        assert!(step.replay.can_start_new_turn);
        assert!(step.replay.history.is_empty());
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            discard_aborted_tool_batch(&fixture.sessions, &fixture.checkpoints, &lease, "b"),
            Err(ResumeError::StaleResolution)
        ));
        assert_eq!(fixture.events().len(), before + 1);
        let reopened = SessionStore::open(fixture._dir.path()).unwrap();
        let loaded = reopened.load(&fixture.session_id).unwrap().unwrap();
        let replay = replay_native(
            &fixture.session_id,
            &loaded.events,
            Some(&fixture.checkpoints),
            &compatibility(),
        )
        .unwrap();
        assert!(replay.can_start_new_turn);
    }

    #[test]
    fn manual_discard_never_masks_an_uncertain_started_tool() {
        let fixture = Fixture::new(vec![]);
        fixture.tool_ready();
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("call-1"),
        });
        fixture.append(AgentEventKind::TurnCancelled);
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let before = fixture.events().len();
        assert!(matches!(
            discard_aborted_tool_batch(&fixture.sessions, &fixture.checkpoints, &lease, "call-1"),
            Err(ResumeError::StaleResolution)
        ));
        assert_eq!(fixture.events().len(), before);
    }

    #[test]
    fn manual_verified_outcome_works_after_context_change_without_model_or_tool() {
        let fixture = Fixture::new(vec![]);
        fixture.tool_ready();
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("call-1"),
        });
        fixture.append(AgentEventKind::TurnFailed {
            error: "worker stopped".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        *fixture.model.request_profile.lock().unwrap() = [8; 32];
        let changed_engine = fixture
            .engine(PermissionMode::Allow)
            .with_native_runtime_identity("other-provider", "https://other.example/v1");
        assert!(matches!(
            changed_engine.inspect(&lease),
            Err(ResumeError::ContextMismatch)
        ));

        let before = fixture.events().len();
        assert!(matches!(
            record_verified_tool_outcome(
                &fixture.sessions,
                &fixture.checkpoints,
                &lease,
                "wrong-id",
                ToolOutput::success("verified"),
                16_000,
            ),
            Err(ResumeError::StaleResolution)
        ));
        assert!(matches!(
            record_verified_tool_outcome(
                &fixture.sessions,
                &fixture.checkpoints,
                &lease,
                "call-1",
                ToolOutput::success("x".repeat(16_001)),
                16_000,
            ),
            Err(ResumeError::OversizedManualResult)
        ));
        assert!(matches!(
            record_verified_tool_outcome(
                &fixture.sessions,
                &fixture.checkpoints,
                &lease,
                "call-1",
                ToolOutput::success("verified"),
                3,
            ),
            Err(ResumeError::OversizedManualResult)
        ));
        assert!(matches!(
            record_verified_tool_outcome(
                &fixture.sessions,
                &fixture.checkpoints,
                &lease,
                "call-1",
                ToolOutput::success("verified"),
                0,
            ),
            Err(ResumeError::Agent(AgentError::InvalidRequest(_)))
        ));
        assert_eq!(fixture.events().len(), before);

        let step = record_verified_tool_outcome(
            &fixture.sessions,
            &fixture.checkpoints,
            &lease,
            "call-1",
            ToolOutput::success("verified"),
            16_000,
        )
        .unwrap();
        assert_eq!(step.disposition, ResumeDisposition::Complete);
        assert!(step.replay.can_start_new_turn);
        assert_eq!(step.appended_events.len(), 1);
        assert!(matches!(
            &step.appended_events[0].kind,
            AgentEventKind::ToolFinished { tool_call_id, output, is_error: false }
                if tool_call_id == "call-1" && output == "verified"
        ));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            record_verified_tool_outcome(
                &fixture.sessions,
                &fixture.checkpoints,
                &lease,
                "call-1",
                ToolOutput::success("verified"),
                16_000,
            ),
            Err(ResumeError::StaleResolution)
        ));
        assert_eq!(fixture.events().len(), before + 1);
        let reopened = SessionStore::open(fixture._dir.path()).unwrap();
        let saved = reopened.load(&fixture.session_id).unwrap().unwrap();
        assert!(
            replay_native(
                &fixture.session_id,
                &saved.events,
                Some(&fixture.checkpoints),
                &compatibility(),
            )
            .unwrap()
            .can_start_new_turn
        );
    }

    #[test]
    fn manual_verified_result_keeps_later_aborted_call_blocked() {
        let fixture = Fixture::new(vec![]);
        fixture.started();
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("a"), call("b")]),
            response_id: None,
            usage: None,
        });
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("a"),
        });
        fixture.append(AgentEventKind::TurnFailed {
            error: "interrupted batch".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let step = record_verified_tool_outcome(
            &fixture.sessions,
            &fixture.checkpoints,
            &lease,
            "a",
            ToolOutput::success("independently verified"),
            16_000,
        )
        .unwrap();
        assert!(matches!(
            step.disposition,
            ResumeDisposition::AbortedToolBatch { next_tool_call } if next_tool_call.id == "b"
        ));
        assert!(!step.replay.can_start_new_turn);
        assert_eq!(
            step.replay
                .pending_tool_batch
                .unwrap()
                .completed_results
                .len(),
            1
        );
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
        let discarded =
            discard_aborted_tool_batch(&fixture.sessions, &fixture.checkpoints, &lease, "b")
                .unwrap();
        assert!(discarded.replay.can_start_new_turn);
    }

    #[test]
    fn manual_verification_advances_each_uncertain_read_in_an_interrupted_wave() {
        let fixture = Fixture::new(vec![]);
        fixture.started();
        fixture.append(AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("", vec![call("a"), call("b")]),
            response_id: None,
            usage: None,
        });
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("a"),
        });
        fixture.append(AgentEventKind::ToolStarted {
            tool_call: call("b"),
        });
        fixture.append(AgentEventKind::TurnFailed {
            error: "read wave interrupted".into(),
        });
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let first = record_verified_tool_outcome(
            &fixture.sessions,
            &fixture.checkpoints,
            &lease,
            "a",
            ToolOutput::success("verified A"),
            16_000,
        )
        .unwrap();
        assert!(matches!(
            first.disposition,
            ResumeDisposition::ToolOutcomeUncertain { tool_call, executor_error: None }
                if tool_call.id == "b"
        ));
        assert!(!first.replay.can_start_new_turn);
        assert!(matches!(
            record_verified_tool_outcome(
                &fixture.sessions,
                &fixture.checkpoints,
                &lease,
                "a",
                ToolOutput::success("duplicate"),
                16_000,
            ),
            Err(ResumeError::StaleResolution)
        ));
        let last = record_verified_tool_outcome(
            &fixture.sessions,
            &fixture.checkpoints,
            &lease,
            "b",
            ToolOutput::success("verified B"),
            16_000,
        )
        .unwrap();
        assert_eq!(last.disposition, ResumeDisposition::Complete);
        assert!(last.replay.can_start_new_turn);
        assert_eq!(last.replay.history[2].tool_call_id.as_deref(), Some("a"));
        assert_eq!(last.replay.history[3].tool_call_id.as_deref(), Some("b"));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
    }

    #[derive(Clone, Copy)]
    enum StreamingTerminal {
        Error,
        Cancel,
        Complete,
    }

    struct SamePollStreamingModel(StreamingTerminal);

    #[async_trait]
    impl ModelBackend for SamePollStreamingModel {
        fn model_id(&self) -> &str {
            "gpt-example"
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
            panic!("recovery must use the streaming backend")
        }

        async fn stream_events(
            &self,
            _messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            cancel: CancellationToken,
            events: crate::ModelEventSender,
        ) -> Result<crate::ModelCompletion, AgentError> {
            // Both deltas and the terminal result become ready in one future
            // poll. This reproduces the receiver/result select race without
            // sleeps, provider timing, or a test-only engine path.
            // The typed sender must accept each event before cancellation;
            // writing into a legacy adapter's private queue is not acceptance.
            for content in ["accepted ñ🙂 ", "", "prefix 終わり"] {
                events
                    .send(openide_core::ModelStreamEvent::TextDelta {
                        content: content.into(),
                    })
                    .await
                    .unwrap();
            }
            match self.0 {
                StreamingTerminal::Error => Err(AgentError::Model("same-poll failure".into())),
                StreamingTerminal::Cancel => {
                    cancel.cancel();
                    Ok(ModelOutput {
                        content: "accepted ñ🙂 prefix 終わり".into(),
                        tool_calls: Vec::new(),
                    }
                    .into())
                }
                StreamingTerminal::Complete => Ok(ModelOutput {
                    content: "accepted ñ🙂 prefix 終わり".into(),
                    tool_calls: Vec::new(),
                }
                .into()),
            }
        }
    }

    struct JournalCheckingObserver {
        sessions: Arc<SessionStore>,
        seen: Mutex<Vec<AgentEvent>>,
        fail_on_first: bool,
        fail_on_response: bool,
        replace_journal: Option<(std::path::PathBuf, std::path::PathBuf)>,
    }

    impl ResumeEventObserver for JournalCheckingObserver {
        fn on_persisted_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
            let record = self.sessions.load(&event.session_id).unwrap().unwrap();
            assert_eq!(record.events.last(), Some(event));
            let mut seen = self.seen.lock().unwrap();
            seen.push(event.clone());
            if self.fail_on_response
                && matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. })
            {
                return Err(AgentError::Runtime(
                    "observer cut after response commit".into(),
                ));
            }
            if seen.len() == 1 {
                if let Some((journal, backup)) = &self.replace_journal {
                    std::fs::rename(journal, backup).unwrap();
                    std::fs::create_dir(journal).unwrap();
                }
                if self.fail_on_first {
                    return Err(AgentError::Runtime("observer unavailable".into()));
                }
            }
            Ok(())
        }
    }

    async fn assert_same_poll_partial_survives(terminal: StreamingTerminal) {
        let fixture = Fixture::new(vec![]);
        fixture.started_with_model_route(true);
        let prefix = fixture.events();
        let observer = Arc::new(JournalCheckingObserver {
            sessions: fixture.sessions.clone(),
            seen: Mutex::new(Vec::new()),
            fail_on_first: false,
            fail_on_response: false,
            replace_journal: None,
        });
        let mut engine = fixture
            .engine(PermissionMode::Allow)
            .with_event_observer(observer.clone());
        engine.model = Arc::new(SamePollStreamingModel(terminal));
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let result = engine.resume_step(&lease, CancellationToken::new()).await;
        match terminal {
            StreamingTerminal::Error => assert!(matches!(
                result,
                Err(ResumeError::Agent(AgentError::Model(message))) if message == "same-poll failure"
            )),
            StreamingTerminal::Cancel => assert!(matches!(
                result,
                Err(ResumeError::Agent(AgentError::Cancelled))
            )),
            StreamingTerminal::Complete => unreachable!(),
        }
        let saved = fixture.events();
        assert_eq!(&saved[..prefix.len()], &prefix);
        assert_eq!(saved.len(), prefix.len() + 2);
        assert_eq!(
            *observer.seen.lock().unwrap(),
            saved[prefix.len()..],
            "Accepted text is observed only after append and in exact order"
        );
        assert!(saved[prefix.len()..]
            .iter()
            .all(|event| matches!(event.kind, AgentEventKind::AssistantDelta { .. })));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        drop(lease);
        let reopened = SessionStore::open(fixture._dir.path()).unwrap();
        let checkpoints = CheckpointFileStore::new(fixture._dir.path()).unwrap();
        let record = reopened.load(&fixture.session_id).unwrap().unwrap();
        assert_eq!(record.events, saved);
        let replay = replay_native(
            &fixture.session_id,
            &record.events,
            Some(&checkpoints),
            &compatibility(),
        )
        .unwrap();
        assert!(matches!(
            replay.recovery,
            ReplayRecovery::ModelInFlight { step: 1, .. }
        ));
        assert!(!replay
            .history
            .iter()
            .any(|message| message.content.contains("accepted ñ🙂")));
        assert_eq!(*observer.seen.lock().unwrap(), saved[prefix.len()..]);
    }

    #[tokio::test]
    async fn recovery_streaming_same_poll_error_preserves_all_accepted_deltas_on_reopen() {
        assert_same_poll_partial_survives(StreamingTerminal::Error).await;
    }

    #[tokio::test]
    async fn recovery_streaming_same_poll_cancel_preserves_all_accepted_deltas_on_reopen() {
        assert_same_poll_partial_survives(StreamingTerminal::Cancel).await;
    }

    #[tokio::test]
    async fn recovery_streaming_observer_failure_stops_at_recoverable_saved_prefix() {
        let fixture = Fixture::new(vec![]);
        fixture.started_with_model_route(true);
        let prefix = fixture.events();
        let observer = Arc::new(JournalCheckingObserver {
            sessions: fixture.sessions.clone(),
            seen: Mutex::new(Vec::new()),
            fail_on_first: true,
            fail_on_response: false,
            replace_journal: None,
        });
        let mut engine = fixture
            .engine(PermissionMode::Allow)
            .with_event_observer(observer.clone());
        engine.model = Arc::new(SamePollStreamingModel(StreamingTerminal::Complete));
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        assert!(matches!(
            engine.resume_step(&lease, CancellationToken::new()).await,
            Err(ResumeError::Agent(AgentError::Runtime(message))) if message == "observer unavailable"
        ));
        assert_eq!(fixture.events().len(), prefix.len() + 1);
        assert_eq!(observer.seen.lock().unwrap().len(), 1);
        assert!(matches!(
            engine.inspect(&lease).unwrap().recovery,
            ReplayRecovery::ModelInFlight { step: 1, .. }
        ));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
        drop(lease);
        assert_eq!(
            SessionStore::open(fixture._dir.path())
                .unwrap()
                .load(&fixture.session_id)
                .unwrap()
                .unwrap()
                .events,
            fixture.events()
        );
    }

    #[tokio::test]
    async fn recovery_streaming_failed_append_never_notifies_or_commits_phantom_output() {
        let fixture = Fixture::new(vec![]);
        fixture.started_with_model_route(true);
        let prefix = fixture.events();
        let journal = fixture
            ._dir
            .path()
            .join(format!("{}.jsonl", fixture.session_id));
        let backup = fixture._dir.path().join("saved-prefix.backup");
        let observer = Arc::new(JournalCheckingObserver {
            sessions: fixture.sessions.clone(),
            seen: Mutex::new(Vec::new()),
            fail_on_first: false,
            fail_on_response: false,
            replace_journal: Some((journal.clone(), backup.clone())),
        });
        let mut engine = fixture
            .engine(PermissionMode::Allow)
            .with_event_observer(observer.clone());
        engine.model = Arc::new(SamePollStreamingModel(StreamingTerminal::Complete));
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        let result = engine.resume_step(&lease, CancellationToken::new()).await;
        std::fs::remove_dir(&journal).unwrap();
        std::fs::rename(&backup, &journal).unwrap();
        assert!(matches!(result, Err(ResumeError::Store(_))));
        assert_eq!(observer.seen.lock().unwrap().len(), 1);
        drop(lease);
        let reopened = SessionStore::open(fixture._dir.path()).unwrap();
        let record = reopened.load(&fixture.session_id).unwrap().unwrap();
        assert_eq!(record.events.len(), prefix.len() + 1);
        assert_eq!(&record.events[..prefix.len()], &prefix);
        assert_eq!(
            &record.events[prefix.len()..],
            &*observer.seen.lock().unwrap()
        );
        assert!(matches!(
            replay_native(
                &fixture.session_id,
                &record.events,
                Some(&fixture.checkpoints),
                &compatibility()
            )
            .unwrap()
            .recovery,
            ReplayRecovery::ModelInFlight { step: 1, .. }
        ));
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn recovery_streaming_cut_after_response_commit_reopens_and_finishes_without_model_retry()
    {
        let fixture = Fixture::new(vec![]);
        fixture.started_with_model_route(true);
        let initial = fixture.events();
        let observer = Arc::new(JournalCheckingObserver {
            sessions: fixture.sessions.clone(),
            seen: Mutex::new(Vec::new()),
            fail_on_first: false,
            fail_on_response: true,
            replace_journal: None,
        });
        let mut engine = fixture
            .engine(PermissionMode::Allow)
            .with_event_observer(observer.clone());
        engine.model = Arc::new(SamePollStreamingModel(StreamingTerminal::Complete));
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        assert!(
            matches!(engine.resume_step(&lease,CancellationToken::new()).await,Err(ResumeError::Agent(AgentError::Runtime(message))) if message=="observer cut after response commit")
        );
        let saved = fixture.events();
        assert_eq!(saved.len(), initial.len() + 3);
        assert!(matches!(
            saved.last().unwrap().kind,
            AgentEventKind::ModelResponseCommitted { step: 1, .. }
        ));
        assert_eq!(*observer.seen.lock().unwrap(), saved[initial.len()..]);
        assert!(!saved[initial.len()..].iter().any(|event| matches!(
            event.kind,
            AgentEventKind::AssistantMessage { .. } | AgentEventKind::TurnCompleted { .. }
        )));
        drop(lease);
        let reopened = Arc::new(SessionStore::open(fixture._dir.path()).unwrap());
        let mut recovery = fixture.engine(PermissionMode::Allow);
        recovery.sessions = reopened.clone();
        recovery.checkpoints = Arc::new(CheckpointFileStore::new(fixture._dir.path()).unwrap());
        let lease = reopened
            .try_acquire_native_lease(&fixture.session_id)
            .unwrap();
        assert!(matches!(
            recovery.inspect(&lease).unwrap().recovery,
            ReplayRecovery::ResponseCommitted { step: 1 }
        ));
        let step = recovery
            .resume_step(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(step.disposition, ResumeDisposition::Complete);
        assert_eq!(step.appended_events.len(), 2);
        assert!(
            matches!(&step.appended_events[0].kind,AgentEventKind::AssistantMessage { content } if content=="accepted ñ🙂 prefix 終わり")
        );
        assert!(
            matches!(&step.appended_events[1].kind,AgentEventKind::TurnCompleted { response,steps:1 } if response=="accepted ñ🙂 prefix 終わり")
        );
        let finished = reopened.load(&fixture.session_id).unwrap().unwrap().events;
        assert_eq!(&finished[..saved.len()], &saved);
        assert_eq!(
            fixture.model.calls.load(Ordering::SeqCst),
            0,
            "Confirmed response recovery must not contact the model again"
        );
        assert!(fixture.tools.calls.lock().unwrap().is_empty());
    }
}
