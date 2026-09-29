use crate::hooks::{CommandHooks, HookDecision, HookInvocation, HookPhase, HookRunResult};
use crate::native_context::{native_system_prompt_with_plan, NativeRuntimeIdentity};
use crate::permission::{
    effective_session_approval_policy_digest, PendingPermissions, SessionApprovalGrant,
};
use crate::plan_tool::{self, UPDATE_PLAN_TOOL};
use crate::project_instructions::{activation_hook_id, ProjectInstructionLedger};
use crate::tool_scheduler::{
    trusted_local_read, ReadWaveAdmission, ScheduleGroup, ScheduledCall, ToolEffect, ToolScheduler,
};
use crate::{
    AgentError, CheckpointCoordinator, CheckpointCoordinatorError, CommandIsolation, ContextPolicy,
    ModelBackend, PermissionChoice, PermissionMode, PermissionPolicy, TokenBudgetDecision,
    TokenBudgetPolicy, TokenBudgetState, ToolCatalog, ToolContext, ToolDefinition, ToolExecutor,
    ToolPreparation,
};
use openide_core::{
    system_instructions_digest, AgentEvent, AgentEventKind, AgentMessage, AgentRunRequest,
    CheckpointBudgetSnapshot, CheckpointCompatibility, CheckpointFileStore, MessageRole,
    NativeEventJournal, NativeSessionLease, PermissionDecision, PermissionResolution, PlanSnapshot,
    ReplayRecovery, SessionStore, StoreError, ToolCall, TurnMode, UserImage,
    MAX_SYSTEM_INSTRUCTIONS_BYTES, MODEL_CONTEXT_VERSION, SYSTEM_INSTRUCTIONS_VERSION,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub struct RunLimits {
    pub max_model_steps: usize,
    pub max_tool_calls: usize,
    pub max_tool_output_chars: usize,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_model_steps: 24,
            max_tool_calls: 64,
            max_tool_output_chars: 16_000,
        }
    }
}

#[derive(Clone)]
pub struct AgentRunner {
    model: Arc<dyn ModelBackend>,
    tools: Arc<dyn ToolExecutor>,
    tool_preparation: Option<Arc<dyn ToolPreparation>>,
    command_hooks: Arc<CommandHooks>,
    limits: RunLimits,
    context_policy: Option<ContextPolicy>,
    token_budget_policy: Option<TokenBudgetPolicy>,
    permission_policy: PermissionPolicy,
    system_instructions: String,
    plan_snapshot: Option<PlanSnapshot>,
    native_runtime_identity: Option<NativeRuntimeIdentity>,
    event_journal: Option<Arc<dyn NativeEventJournal>>,
    auto_compaction: Option<AutoCompaction>,
    session_approvals: Option<(Arc<SessionStore>, CommandIsolation)>,
    native_steering_store: Option<Arc<SessionStore>>,
}

#[derive(Clone)]
struct AutoCompaction {
    sessions: Arc<SessionStore>,
    checkpoints: Arc<CheckpointFileStore>,
    compatibility: CheckpointCompatibility,
}

pub struct AgentRunHandle {
    pub session_id: String,
    pub events: mpsc::Receiver<AgentEvent>,
    cancel: CancellationToken,
    pending_permissions: Arc<PendingPermissions>,
    journal_error: Arc<Mutex<Option<String>>>,
}

struct ActiveTools<'a> {
    workspace: PathBuf,
    executor: Arc<dyn ToolExecutor>,
    ask_tool_names: &'a [String],
    mcp_resume_identity_digest: Option<&'a str>,
    permission_policy: PermissionPolicy,
}

struct SessionApprovalContext<'a> {
    workspace: &'a Path,
    digest: Option<&'a str>,
    mcp_identity_verified: bool,
}

struct NativeRunContext<'a> {
    lease: Option<&'a NativeSessionLease>,
    request_profile: Option<[u8; 32]>,
}

struct NativeSteeringContext<'a> {
    store: Arc<SessionStore>,
    lease: &'a NativeSessionLease,
    turn_id: String,
}

impl NativeSteeringContext<'_> {
    fn project_instruction_ledger(
        &self,
        turn_start_sequence: u64,
        workspace: &Path,
    ) -> Result<ProjectInstructionLedger, AgentError> {
        let record = self
            .store
            .load(self.lease.session_id())
            .map_err(|error| AgentError::Runtime(format!("project instruction journal: {error}")))?
            .ok_or_else(|| AgentError::Runtime("native session is missing".into()))?;
        ProjectInstructionLedger::from_events(&record.events, turn_start_sequence, workspace)
    }

    fn admitted_images(
        &self,
        sequence: u64,
        prompt: &str,
        model: &str,
        mode: TurnMode,
    ) -> Result<Vec<UserImage>, AgentError> {
        let record = self
            .store
            .load(self.lease.session_id())
            .map_err(|error| AgentError::Runtime(format!("native admission: {error}")))?
            .ok_or_else(|| AgentError::InvalidRequest("native session is missing".into()))?;
        let admitted = record
            .events
            .iter()
            .find(|event| event.sequence == sequence)
            .ok_or_else(|| AgentError::InvalidRequest("native admission is missing".into()))?;
        match &admitted.kind {
            AgentEventKind::TurnAdmittedWithImages {
                turn_id,
                prompt: saved_prompt,
                model: saved_model,
                mode: saved_mode,
                images,
            } if turn_id == &self.turn_id
                && saved_prompt == prompt
                && saved_model == model
                && *saved_mode == mode =>
            {
                Ok(images.clone())
            }
            AgentEventKind::TurnAdmitted {
                turn_id,
                prompt: saved_prompt,
                model: saved_model,
                mode: saved_mode,
            } if turn_id == &self.turn_id
                && saved_prompt == prompt
                && saved_model == model
                && *saved_mode == mode =>
            {
                Ok(Vec::new())
            }
            _ => Err(AgentError::InvalidRequest(
                "native request differs from its admitted turn".into(),
            )),
        }
    }

    fn drain(
        &self,
        sink: &mut EventSink,
        messages: &mut Vec<AgentMessage>,
    ) -> Result<usize, AgentError> {
        let pending = self
            .store
            .pending_native_steers(&sink.session_id, &self.turn_id)
            .map_err(|error| AgentError::Runtime(format!("steering inbox: {error}")))?;
        let mut drained = 0;
        for input in pending {
            let sequence = sink.sequence.checked_add(1).ok_or_else(|| {
                AgentError::Limit("event sequence exceeded the supported range".into())
            })?;
            let event = self
                .store
                .ack_native_steer(self.lease, &self.turn_id, &input.input_id, sequence)
                .map_err(|error| {
                    AgentError::Runtime(format!("steering acknowledgement: {error}"))
                })?;
            let message = match &event.kind {
                AgentEventKind::TurnSteered { text, .. } => AgentMessage::user(text.clone()),
                AgentEventKind::TurnSteeredWithImages { text, images, .. } => {
                    AgentMessage::user_with_images(text.clone(), images.clone()).map_err(
                        |error| AgentError::InvalidRequest(format!("steering images: {error}")),
                    )?
                }
                _ => {
                    return Err(AgentError::Runtime(
                        "steering acknowledgement returned a different event".into(),
                    ))
                }
            };
            sink.forward_persisted(event)?;
            messages.push(message);
            drained += 1;
        }
        Ok(drained)
    }

    fn fence_completion(&self) -> Result<bool, AgentError> {
        match self
            .store
            .fence_native_turn_completion(self.lease, &self.turn_id)
        {
            Ok(()) => Ok(true),
            Err(StoreError::PendingNativeSteerInput) => Ok(false),
            Err(error) => Err(AgentError::Runtime(format!(
                "cannot finish native turn: {error}"
            ))),
        }
    }
}

impl AgentRunHandle {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Resolve a permission request emitted by this run. Returns false if the
    /// request already settled or never belonged to this handle.
    pub fn resolve_permission(&self, request_id: &str, choice: PermissionChoice) -> bool {
        self.pending_permissions.resolve(request_id, choice)
    }

    /// Share the live, run-scoped authority with the subagent event bridge.
    /// The bridge still requires the child ID and an observed request ID.
    pub(crate) fn permission_capability(&self) -> Arc<PendingPermissions> {
        self.pending_permissions.clone()
    }

    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// A diagnostic channel outside the event log, used only when that log
    /// itself cannot record a terminal failure.
    pub fn journal_error(&self) -> Option<String> {
        self.journal_error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

impl AgentRunner {
    pub fn new(model: Arc<dyn ModelBackend>, tools: Arc<dyn ToolExecutor>) -> Self {
        Self {
            model,
            tools,
            tool_preparation: None,
            command_hooks: Arc::new(CommandHooks::default()),
            limits: RunLimits::default(),
            context_policy: None,
            token_budget_policy: None,
            permission_policy: PermissionPolicy::default(),
            system_instructions: String::new(),
            plan_snapshot: None,
            native_runtime_identity: None,
            event_journal: None,
            auto_compaction: None,
            session_approvals: None,
            native_steering_store: None,
        }
    }

    pub fn with_limits(mut self, limits: RunLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_context_policy(mut self, policy: ContextPolicy) -> Self {
        self.context_policy = Some(policy);
        self
    }

    /// Gate requests with the model's verified context window. A configured
    /// checkpoint store can compact a complete historical prefix on demand.
    pub fn with_token_budget_policy(mut self, policy: TokenBudgetPolicy) -> Self {
        self.token_budget_policy = Some(policy);
        self
    }

    pub fn with_permission_policy(mut self, policy: PermissionPolicy) -> Self {
        self.permission_policy = policy;
        self
    }

    /// Prepare external tools inside the run worker's Tokio runtime. Build
    /// turns discover them before the first model request and keep their
    /// session alive until the turn is finished and its teardown completes.
    pub fn with_tool_preparation(mut self, preparation: Arc<dyn ToolPreparation>) -> Self {
        self.tool_preparation = Some(preparation);
        self
    }

    /// Installs a host-validated ordered hook policy. Native hooks require a
    /// leased, journaled run so starts and outcomes are synced before effects.
    pub fn with_command_hooks(mut self, hooks: Arc<CommandHooks>) -> Self {
        self.command_hooks = hooks;
        self
    }

    /// Adds workspace capabilities to the model's system context without
    /// changing the user prompt recorded in the session event log.
    pub fn with_system_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.system_instructions = instructions.into();
        self
    }

    /// Supply the latest replayed plan for a new Build turn. The selected
    /// snapshot is frozen in the turn's model context digest and can be
    /// reconstructed from PlanProposed/PlanUpdated after a crash.
    pub fn with_plan_snapshot(mut self, plan: Option<PlanSnapshot>) -> Self {
        self.plan_snapshot = plan;
        self
    }

    /// Bind a native turn to the exact provider endpoint used for its model
    /// requests. Only a digest is persisted; credentials and endpoint text
    /// stay out of the journal. Required for leased native runs.
    pub fn with_native_runtime_identity(
        mut self,
        provider: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        self.native_runtime_identity = Some(NativeRuntimeIdentity::new(provider, endpoint));
        self
    }

    /// Persist worker events before they reach the UI or the agent advances.
    /// Native desktop runs supply their session store here; callers without a
    /// store can still use the in-memory event stream.
    pub fn with_event_journal(mut self, journal: Arc<dyn NativeEventJournal>) -> Self {
        self.event_journal = Some(journal);
        self
    }

    /// Enable exact, durable approval reuse for this native session. The
    /// isolation must match the trusted command executor configuration.
    pub fn with_session_approvals(
        mut self,
        sessions: Arc<SessionStore>,
        isolation: CommandIsolation,
    ) -> Self {
        self.session_approvals = Some((sessions, isolation));
        self
    }

    /// Consume durable second user inputs at model boundaries. The store
    /// must be the same journal used for this leased native run.
    pub fn with_native_steering(mut self, sessions: Arc<SessionStore>) -> Self {
        self.native_steering_store = Some(sessions);
        self
    }

    /// Enable durable compaction inside a native run. The session store also
    /// becomes the event journal, so the checkpoint marker and worker events
    /// share one ordered log. Call this after `with_event_journal` if both are
    /// used.
    pub fn with_auto_compaction(
        mut self,
        sessions: Arc<SessionStore>,
        checkpoints: Arc<CheckpointFileStore>,
        compatibility: CheckpointCompatibility,
    ) -> Self {
        self.event_journal = Some(sessions.clone());
        self.auto_compaction = Some(AutoCompaction {
            sessions,
            checkpoints,
            compatibility,
        });
        self
    }

    /// Starts a worker thread. Callers may poll `handle.events` from a synchronous UI loop.
    pub fn start(&self, request: AgentRunRequest) -> AgentRunHandle {
        self.start_with_optional_lease(request, None)
    }

    /// Holds a cross-process session lease until the native worker exits.
    /// Acquire it before admitting the turn so competing processes cannot
    /// interleave their admissions or worker events.
    pub fn start_with_lease(
        &self,
        request: AgentRunRequest,
        lease: NativeSessionLease,
    ) -> AgentRunHandle {
        self.start_with_shared_lease(request, Arc::new(lease))
    }

    /// Start a native worker with a shared OS lease. A subagent coordinator
    /// keeps another reference until it commits the child's terminal state.
    /// All references must be dropped before a competing worker can acquire
    /// the session again.
    pub fn start_with_shared_lease(
        &self,
        request: AgentRunRequest,
        lease: Arc<NativeSessionLease>,
    ) -> AgentRunHandle {
        assert_eq!(
            request.session_id.as_deref(),
            Some(lease.session_id()),
            "native session lease must match the admitted session"
        );
        self.start_with_optional_lease(request, Some(lease))
    }

    fn start_with_optional_lease(
        &self,
        request: AgentRunRequest,
        lease: Option<Arc<NativeSessionLease>>,
    ) -> AgentRunHandle {
        let session_id = request
            .session_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let cancel = CancellationToken::new();
        let pending_permissions = Arc::new(PendingPermissions::default());
        let journal_error = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::channel();
        let worker = self.clone();
        let worker_cancel = cancel.clone();
        let worker_pending = pending_permissions.clone();
        let worker_journal_error = journal_error.clone();
        let worker_session_id = session_id.clone();
        std::thread::spawn(move || {
            let session_lease = lease;
            let mut sink = EventSink {
                session_id: worker_session_id,
                sequence: request.sequence_start,
                sender,
                journal: worker.event_journal.clone(),
                journal_failed: false,
                journal_error: worker_journal_error,
            };
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = sink.emit(AgentEventKind::TurnFailed {
                        error: format!("cannot start async runtime: {error}"),
                    });
                    return;
                }
            };
            let result = runtime.block_on(worker.run_inner(
                request,
                worker_cancel,
                worker_pending,
                &mut sink,
                session_lease.as_deref(),
            ));
            match result {
                Ok(()) => {}
                Err(AgentError::Cancelled) => {
                    let _ = sink.emit(AgentEventKind::TurnCancelled);
                }
                Err(error) => {
                    let _ = sink.emit(AgentEventKind::TurnFailed {
                        error: error.to_string(),
                    });
                }
            }
            // The UI treats channel disconnection as the turn's final ACK.
            // Release every executor-held and direct OS lease before closing
            // the sender so the next parent turn can start immediately.
            drop(worker);
            drop(session_lease);
        });
        AgentRunHandle {
            session_id,
            events: receiver,
            cancel,
            pending_permissions,
            journal_error,
        }
    }

    async fn run_inner(
        &self,
        request: AgentRunRequest,
        cancel: CancellationToken,
        pending_permissions: Arc<PendingPermissions>,
        sink: &mut EventSink,
        lease: Option<&NativeSessionLease>,
    ) -> Result<(), AgentError> {
        sink.emit(AgentEventKind::SessionStarted {
            workspace: request.workspace.clone(),
        })?;
        sink.emit(AgentEventKind::TurnStarted {
            prompt: request.prompt.clone(),
        })?;
        if request.prompt.trim().is_empty() {
            return Err(AgentError::InvalidRequest("prompt is empty".into()));
        }
        let workspace = canonical_workspace(&request.workspace)?;
        // Reject an unverifiable native request before starting an external
        // tool service during Build preparation.
        let request_profile = if lease.is_some() {
            if self.native_runtime_identity.is_none() {
                return Err(AgentError::InvalidRequest(
                    "native provider and endpoint identity are required".into(),
                ));
            }
            Some(self.model.request_profile_fingerprint().ok_or_else(|| {
                AgentError::InvalidRequest(
                    "native model backend has no stable request profile fingerprint".into(),
                )
            })?)
        } else {
            None
        };
        let mode = request.mode;
        // Preparation and shutdown run on the same Tokio runtime as every
        // model and tool step. In Plan mode external tools are not advertised,
        // so starting their processes would serve no purpose.
        let prepared = if mode == TurnMode::Build {
            match &self.tool_preparation {
                Some(preparation) => Some(
                    preparation
                        .prepare(&workspace, self.tools.clone(), &cancel)
                        .await?,
                ),
                None => None,
            }
        } else {
            None
        };
        let tools = prepared
            .as_ref()
            .map(|session| session.tools())
            .unwrap_or_else(|| self.tools.clone());
        let mut permission_policy = self.permission_policy.clone();
        let ask_tool_names = prepared
            .as_ref()
            .map(|session| session.ask_tool_names())
            .unwrap_or(&[]);
        if let Some(session) = &prepared {
            for name in session.ask_tool_names() {
                permission_policy.require_ask_unless_denied(name);
            }
        }
        let active_tools = ActiveTools {
            workspace,
            executor: tools,
            ask_tool_names,
            mcp_resume_identity_digest: prepared
                .as_ref()
                .and_then(|session| session.mcp_resume_identity_digest()),
            permission_policy,
        };
        let result = self
            .run_with_tools(
                request,
                &active_tools,
                cancel.clone(),
                pending_permissions,
                sink,
                NativeRunContext {
                    lease,
                    request_profile,
                },
            )
            .await;
        let shutdown_result = match &prepared {
            Some(session) => session.shutdown().await,
            None => Ok(()),
        };
        match (result, shutdown_result) {
            (Err(AgentError::Cancelled), _) => Err(AgentError::Cancelled),
            (Err(error), Err(shutdown_error)) => Err(AgentError::Runtime(format!(
                "{error}; tool shutdown failed: {shutdown_error}"
            ))),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(_), Ok(())) if cancel.is_cancelled() => Err(AgentError::Cancelled),
            (Ok((response, steps)), Ok(())) => {
                if mode == TurnMode::Plan {
                    sink.emit(AgentEventKind::PlanProposed {
                        plan: PlanSnapshot::from_plan_response(&response),
                    })?;
                }
                sink.emit(AgentEventKind::TurnCompleted { response, steps })
            }
        }
    }

    async fn run_with_tools(
        &self,
        request: AgentRunRequest,
        active_tools: &ActiveTools<'_>,
        cancel: CancellationToken,
        pending_permissions: Arc<PendingPermissions>,
        sink: &mut EventSink,
        native: NativeRunContext<'_>,
    ) -> Result<(String, usize), AgentError> {
        let mode = request.mode;
        if !self.command_hooks.is_empty() && (native.lease.is_none() || sink.journal.is_none()) {
            return Err(AgentError::InvalidRequest(
                "command hooks require a leased native journal".into(),
            ));
        }
        let steering = match (&self.native_steering_store, native.lease, &sink.journal) {
            (None, _, _) => None,
            (Some(store), Some(lease), Some(journal)) => {
                let store_journal: Arc<dyn NativeEventJournal> = store.clone();
                if !Arc::ptr_eq(journal, &store_journal) {
                    return Err(AgentError::InvalidRequest(
                        "steering store differs from the native event journal".into(),
                    ));
                }
                let turn_id = store
                    .active_native_turn_id(&sink.session_id)
                    .map_err(|error| AgentError::Runtime(format!("active turn: {error}")))?
                    .ok_or_else(|| {
                        AgentError::InvalidRequest(
                            "native steering requires an admitted turn".into(),
                        )
                    })?;
                Some(NativeSteeringContext {
                    store: store.clone(),
                    lease,
                    turn_id,
                })
            }
            (Some(_), _, _) => {
                return Err(AgentError::InvalidRequest(
                    "native steering requires a leased session store journal".into(),
                ))
            }
        };
        let turn_start_sequence = sink.sequence;
        let hooks_digest = self.command_hooks.digest_hex().map_err(|error| {
            AgentError::InvalidRequest(format!("command hook identity: {error}"))
        })?;
        // The catalog for this turn is frozen before the first model request.
        // Plan mode requires both an exact local read name and an explicit
        // implementation-level read certification from the executor.
        let mut definitions = active_tools
            .executor
            .definitions()
            .into_iter()
            .filter(|definition| {
                mode == TurnMode::Build
                    || (trusted_local_read(&definition.name)
                        && active_tools
                            .executor
                            .certifies_parallel_read(&definition.name)
                        && !self.command_hooks.has_hooks_for_tool(&definition.name))
            })
            .collect::<Vec<_>>();
        if mode == TurnMode::Build && native.lease.is_some() && sink.journal.is_some() {
            if definitions
                .iter()
                .any(|definition| definition.name == UPDATE_PLAN_TOOL)
            {
                return Err(AgentError::InvalidRequest(
                    "tool executor collides with built-in update_plan".into(),
                ));
            }
            definitions.push(plan_tool::definition());
        }
        let catalog = ToolCatalog::new(definitions)
            .map_err(|error| AgentError::InvalidRequest(format!("tool catalog: {error}")))?;
        for name in active_tools.ask_tool_names {
            if !catalog
                .definitions()
                .iter()
                .any(|definition| &definition.name == name)
            {
                return Err(AgentError::InvalidRequest(format!(
                    "tool preparation marked an unadvertised tool for permission: {name}"
                )));
            }
        }
        let catalog_digest = catalog.digest_hex();
        let approval_store_is_journal = match (&self.session_approvals, &sink.journal) {
            (Some((sessions, _)), Some(journal)) => {
                let session_journal: Arc<dyn NativeEventJournal> = sessions.clone();
                Arc::ptr_eq(journal, &session_journal)
            }
            _ => false,
        };
        let approval_digest = if native.lease.is_some() && approval_store_is_journal {
            self.session_approvals
                .as_ref()
                .map(|(_, isolation)| {
                    effective_session_approval_policy_digest(
                        &catalog_digest,
                        active_tools.mcp_resume_identity_digest,
                        &hooks_digest,
                        *isolation,
                        &active_tools.permission_policy,
                    )
                    .map_err(|error| {
                        AgentError::InvalidRequest(format!(
                            "session approval policy identity: {error}"
                        ))
                    })
                })
                .transpose()?
        } else {
            None
        };
        sink.emit(AgentEventKind::ToolCatalogCommitted {
            digest: catalog_digest.clone(),
        })?;
        if let Some(digest) = active_tools.mcp_resume_identity_digest {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(AgentError::InvalidRequest(
                    "prepared MCP identity digest is invalid".into(),
                ));
            }
            sink.emit(AgentEventKind::McpRuntimeCommitted {
                version: openide_core::MCP_RUNTIME_IDENTITY_VERSION,
                digest: digest.to_owned(),
            })?;
        }
        if native.lease.is_some() {
            sink.emit(AgentEventKind::HooksCommitted {
                version: openide_core::HOOK_CONFIG_IDENTITY_VERSION,
                digest: hooks_digest.clone(),
            })?;
            sink.emit(AgentEventKind::ExecPolicyCommitted {
                version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                digest: active_tools.permission_policy.digest_hex(),
            })?;
            let digest =
                system_instructions_digest(&self.system_instructions).ok_or_else(|| {
                    AgentError::InvalidRequest(format!(
                        "system instructions exceed {MAX_SYSTEM_INSTRUCTIONS_BYTES} bytes"
                    ))
                })?;
            sink.emit(AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest,
                content: self.system_instructions.clone(),
            })?;
        }
        if self
            .plan_snapshot
            .as_ref()
            .is_some_and(|plan| !plan.is_valid())
        {
            return Err(AgentError::InvalidRequest(
                "plan snapshot is invalid".into(),
            ));
        }
        let system = native_system_prompt_with_plan(
            &active_tools.workspace,
            catalog.definitions(),
            mode,
            &self.system_instructions,
            self.plan_snapshot.as_ref(),
        );
        if native.lease.is_some() {
            let identity = self.native_runtime_identity.as_ref().ok_or_else(|| {
                AgentError::InvalidRequest(
                    "native provider and endpoint identity are required".into(),
                )
            })?;
            let request_profile = native.request_profile.ok_or_else(|| {
                AgentError::InvalidRequest(
                    "native model backend has no stable request profile fingerprint".into(),
                )
            })?;
            let digest = identity.digest_hex(
                self.model.model_id(),
                mode,
                &active_tools.workspace,
                &catalog_digest,
                &system,
                request_profile,
            )?;
            sink.emit(AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest,
            })?;
        }
        let mut messages = vec![AgentMessage::system(system)];
        messages.extend(request.history);
        let mut current_prompt_index = messages.len();
        let active_prompt = request.prompt.clone();
        let admitted_images = steering
            .as_ref()
            .map(|context| {
                context.admitted_images(
                    request.sequence_start,
                    &request.prompt,
                    self.model.model_id(),
                    mode,
                )
            })
            .transpose()?
            .unwrap_or_default();
        messages.push(
            AgentMessage::user_with_images(request.prompt, admitted_images)
                .map_err(|error| AgentError::InvalidRequest(format!("admitted images: {error}")))?,
        );
        let mut tool_count = 0usize;
        let mut budget_state = TokenBudgetState::default();
        for step in 1..=self.limits.max_model_steps {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if let Some(steering) = &steering {
                if steering.drain(sink, &mut messages)? > 0 {
                    current_prompt_index = messages.len() - 1;
                }
            }
            if let Some(policy) = self.context_policy {
                policy.compact(&mut messages, &mut current_prompt_index)?;
            }
            if let Some(policy) = self.token_budget_policy {
                let assessment = policy
                    .assess_model_request(
                        &mut budget_state,
                        self.model.as_ref(),
                        &messages,
                        catalog.definitions(),
                        cancel.clone(),
                    )
                    .await?;
                if assessment.decision != TokenBudgetDecision::Continue {
                    let limit_message = format!(
                        "semantic compaction is required before the next model request: projected {} tokens, threshold {}",
                        assessment.projected_total_tokens,
                        assessment.auto_compact_threshold_tokens
                    );
                    let (Some(config), Some(lease)) = (&self.auto_compaction, native.lease) else {
                        return Err(AgentError::Limit(limit_message));
                    };
                    let budget = CheckpointBudgetSnapshot {
                        context_window_tokens: policy.context_window_tokens,
                        estimated_input_tokens: assessment.estimated_input_tokens,
                        observed_input_tokens: budget_state.observed_input_tokens(),
                        output_reserve_tokens: policy.output_reserve_tokens,
                        tool_schema_reserve_tokens: policy.tool_schema_reserve_tokens,
                        limits_unverified: false,
                    };
                    let coordinator = CheckpointCoordinator::new(
                        config.sessions.clone(),
                        config.checkpoints.clone(),
                        self.model.clone(),
                        config.compatibility.clone(),
                        budget,
                    );
                    let committed = match coordinator
                        .compact_completed_prefix(lease, cancel.clone())
                        .await
                    {
                        Ok(committed) => committed,
                        Err(CheckpointCoordinatorError::NoNewHistory) => {
                            return Err(AgentError::Limit(limit_message));
                        }
                        Err(CheckpointCoordinatorError::Compact(error)) => return Err(error),
                        Err(error) => {
                            return Err(AgentError::Runtime(format!(
                                "semantic compaction failed: {error}"
                            )));
                        }
                    };
                    if committed.replay.recovery
                        != (ReplayRecovery::ReadyForModel { next_step: step })
                        || committed
                            .replay
                            .active_turn
                            .as_ref()
                            .is_none_or(|turn| turn.prompt != active_prompt)
                        || committed.replay.active_tool_catalog_digest.as_deref()
                            != Some(catalog.digest_hex().as_str())
                    {
                        return Err(AgentError::Runtime(
                            "compaction did not preserve the active turn and tool catalog".into(),
                        ));
                    }
                    sink.forward_persisted(committed.marker_event)?;
                    let system = messages
                        .first()
                        .cloned()
                        .ok_or_else(|| AgentError::Runtime("system context is missing".into()))?;
                    messages = vec![system];
                    messages.extend(committed.replay.history);
                    current_prompt_index = messages
                        .iter()
                        .rposition(|message| message.role == MessageRole::User)
                        .ok_or_else(|| {
                            AgentError::Runtime("compaction lost the active user input".into())
                        })?;
                    if let Some(context_policy) = self.context_policy {
                        context_policy.compact(&mut messages, &mut current_prompt_index)?;
                    }
                    budget_state.reset();
                    let reassessment = policy
                        .assess_model_request(
                            &mut budget_state,
                            self.model.as_ref(),
                            &messages,
                            catalog.definitions(),
                            cancel.clone(),
                        )
                        .await?;
                    if reassessment.decision != TokenBudgetDecision::Continue {
                        return Err(AgentError::Limit(format!(
                            "semantic compaction could not fit the next model request: projected {} tokens, threshold {}",
                            reassessment.projected_total_tokens,
                            reassessment.auto_compact_threshold_tokens
                        )));
                    }
                }
            }
            let mut model_messages = messages.clone();
            if let Some(steering) = &steering {
                let ledger = steering
                    .project_instruction_ledger(turn_start_sequence, &active_tools.workspace)?;
                if let Some(context) = ledger.model_context() {
                    model_messages.insert(1, context);
                }
            }
            if let Some(policy) = self.token_budget_policy {
                let mut request_budget = budget_state.clone();
                if policy
                    .assess_model_request(
                        &mut request_budget,
                        self.model.as_ref(),
                        &model_messages,
                        catalog.definitions(),
                        cancel.clone(),
                    )
                    .await?
                    .decision
                    != TokenBudgetDecision::Continue
                {
                    return Err(AgentError::Limit(
                        "scoped project instructions exceed the model context budget".into(),
                    ));
                }
                // Usage calibrates the actual request, including scoped
                // instructions, rather than the earlier compaction probe.
                budget_state = request_budget;
            }
            sink.emit(AgentEventKind::ModelRequestStarted {
                step,
                model: self.model.model_id().to_owned(),
            })?;
            let completion = self
                .model_step(&model_messages, catalog.definitions(), cancel.clone(), sink)
                .await?;
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let mut assistant =
                AgentMessage::assistant(completion.output.content, completion.output.tool_calls);
            assistant.provider_output_items = completion.provider_output_items;
            let mut call_ids = HashSet::new();
            for call in &assistant.tool_calls {
                if call.id.is_empty() || call.id.len() > 256 || !call_ids.insert(call.id.as_str()) {
                    return Err(AgentError::Model(
                        "model returned an empty, oversized, or duplicate tool call ID".into(),
                    ));
                }
            }
            if self.token_budget_policy.is_some() {
                if let Some(usage) = &completion.usage {
                    budget_state.observe_provider_input_tokens(usage.input_tokens)?;
                }
            }
            sink.emit(AgentEventKind::ModelResponseCommitted {
                step,
                assistant: assistant.clone(),
                response_id: completion.response_id,
                usage: completion.usage,
            })?;
            if assistant.tool_calls.is_empty() {
                if assistant.content.trim().is_empty() {
                    return Err(AgentError::Model(
                        "model returned neither text nor a tool call".into(),
                    ));
                }
                sink.emit(AgentEventKind::AssistantMessage {
                    content: assistant.content.clone(),
                })?;
                if let Some(steering) = &steering {
                    let response = assistant.content.clone();
                    messages.push(assistant);
                    if steering.drain(sink, &mut messages)? > 0 {
                        current_prompt_index = messages.len() - 1;
                        continue;
                    }
                    if !steering.fence_completion()? {
                        if steering.drain(sink, &mut messages)? == 0 {
                            return Err(AgentError::Runtime(
                                "pending steering input disappeared before completion".into(),
                            ));
                        }
                        current_prompt_index = messages.len() - 1;
                        continue;
                    }
                    return Ok((response, step));
                }
                return Ok((assistant.content, step));
            }
            if tool_count.saturating_add(assistant.tool_calls.len()) > self.limits.max_tool_calls {
                return Err(AgentError::Limit(format!(
                    "more than {} tool calls in one run",
                    self.limits.max_tool_calls
                )));
            }
            if !assistant.content.is_empty() {
                sink.emit(AgentEventKind::AssistantMessage {
                    content: assistant.content.clone(),
                })?;
            }
            let calls = assistant.tool_calls.clone();
            messages.push(assistant);
            let project_ledger = steering
                .as_ref()
                .map(|steering| {
                    steering
                        .project_instruction_ledger(turn_start_sequence, &active_tools.workspace)
                })
                .transpose()?;
            let project_preflights = calls
                .iter()
                .map(|call| {
                    if catalog.require_valid_call(call).is_err()
                        || active_tools.permission_policy.mode_for_call(call)
                            == PermissionMode::Deny
                    {
                        return None;
                    }
                    project_ledger.as_ref().and_then(|ledger| {
                        ledger
                            .preflight(
                                &active_tools.workspace,
                                call,
                                self.limits.max_tool_output_chars,
                            )
                            .unwrap_or_else(|error| {
                                Some(crate::ToolOutput::error(error.to_string()))
                            })
                    })
                })
                .collect::<Vec<_>>();
            // Only the three built-in reads with an allowed, validated call
            // can enter a concurrent wave. Native journal and lease are
            // required because every start must be durable before execution.
            let concurrent_reads = native.lease.is_some() && sink.journal.is_some();
            let scheduled = calls
                .into_iter()
                .enumerate()
                .map(|(index, call)| {
                    let effect = if concurrent_reads
                        && project_preflights[index].is_none()
                        && trusted_local_read(&call.name)
                        && active_tools.executor.certifies_parallel_read(&call.name)
                        && !self.command_hooks.has_hooks_for_tool(&call.name)
                        && active_tools.permission_policy.mode_for_call(&call)
                            == PermissionMode::Allow
                        && catalog.require_valid_call(&call).is_ok()
                    {
                        ToolEffect::ReadOnly
                    } else {
                        ToolEffect::Mutating
                    };
                    ScheduledCall { call, effect }
                })
                .collect::<Vec<_>>();
            let scheduler = ToolScheduler::new(4)?;
            for group in scheduler.plan(&scheduled)? {
                if cancel.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                let context = ToolContext {
                    workspace: active_tools.workspace.clone(),
                    cancel: cancel.clone(),
                };
                if let ScheduleGroup::ReadWave(indices) = &group {
                    // All starts are synced before any read begins. A crash
                    // can leave several uncertain IDs; replay advances them
                    // one by one only after independently verified outcomes.
                    for &index in indices {
                        sink.emit(AgentEventKind::ToolStarted {
                            tool_call: scheduled[index].call.clone(),
                        })?;
                    }
                    let admission = ReadWaveAdmission::after_persisted_starts(
                        sink.sequence,
                        &scheduled,
                        indices,
                    );
                    let outputs = scheduler
                        .execute_read_wave(
                            &scheduled,
                            &group,
                            admission,
                            active_tools.executor.as_ref(),
                            &context,
                        )
                        .await?;
                    for (&index, result) in indices.iter().zip(outputs) {
                        let call = &scheduled[index].call;
                        let content =
                            truncate_chars(&result.content, self.limits.max_tool_output_chars);
                        sink.emit(AgentEventKind::ToolFinished {
                            tool_call_id: call.id.clone(),
                            output: content.clone(),
                            is_error: result.is_error,
                        })?;
                        messages.push(AgentMessage::tool(call.id.clone(), content));
                    }
                    tool_count += indices.len();
                    continue;
                }
                let ScheduleGroup::Mutation(index) = group else {
                    unreachable!("all scheduler groups are covered")
                };
                let call = &scheduled[index].call;
                tool_count += 1;
                // The call is checked against the frozen catalog before any
                // permission prompt or external tool effect.
                let denied = match catalog.require_valid_call(call) {
                    Err(error) => Some(crate::ToolOutput::error(error.to_string())),
                    Ok(_) if project_preflights[index].is_some() => {
                        project_preflights[index].clone()
                    }
                    Ok(_) if call.name == UPDATE_PLAN_TOOL => None,
                    Ok(_) => {
                        self.check_permission(
                            call,
                            &active_tools.permission_policy,
                            SessionApprovalContext {
                                workspace: &active_tools.workspace,
                                digest: approval_digest.as_deref(),
                                mcp_identity_verified: active_tools
                                    .mcp_resume_identity_digest
                                    .is_some(),
                            },
                            &cancel,
                            &pending_permissions,
                            sink,
                        )
                        .await?
                    }
                };
                if cancel.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                sink.emit(AgentEventKind::ToolStarted {
                    tool_call: call.clone(),
                })?;
                let result = match denied {
                    Some(denied) => Ok(denied),
                    None if call.name == UPDATE_PLAN_TOOL => match plan_tool::parse(call) {
                        Ok(plan) => {
                            sink.emit(AgentEventKind::PlanUpdated {
                                tool_call_id: call.id.clone(),
                                plan,
                            })?;
                            Ok(crate::ToolOutput::success("Plan updated"))
                        }
                        Err(error) => Ok(error),
                    },
                    None => {
                        self.execute_with_hooks(
                            call,
                            &context,
                            active_tools.executor.as_ref(),
                            sink,
                            turn_start_sequence,
                            &hooks_digest,
                        )
                        .await
                    }
                };
                let result = match result {
                    Ok(result) => result,
                    Err(AgentError::Cancelled) => return Err(AgentError::Cancelled),
                    // The tool may have changed the world before losing its
                    // reply. Leave ToolStarted open so replay requires a
                    // manually verified result instead of repeating it.
                    Err(error @ AgentError::EffectUncertain(_)) => return Err(error),
                    Err(error) => crate::ToolOutput::error(error.to_string()),
                };
                if project_preflights[index].as_ref() == Some(&result) {
                    if let Some(hook_id) = activation_hook_id(&result.content) {
                        sink.emit(AgentEventKind::HookStarted {
                            tool_call_id: call.id.clone(),
                            hook_id: hook_id.clone(),
                            phase: "pre_tool_use".into(),
                        })?;
                        sink.emit(AgentEventKind::HookFinished {
                            tool_call_id: call.id.clone(),
                            hook_id,
                            phase: "pre_tool_use".into(),
                            decision: "allow".into(),
                        })?;
                    }
                }
                let content = truncate_chars(&result.content, self.limits.max_tool_output_chars);
                sink.emit(AgentEventKind::ToolFinished {
                    tool_call_id: call.id.clone(),
                    output: content.clone(),
                    is_error: result.is_error,
                })?;
                messages.push(AgentMessage::tool(call.id.clone(), content));
            }
        }
        Err(AgentError::Limit(format!(
            "more than {} model steps in one run",
            self.limits.max_model_steps
        )))
    }

    async fn execute_with_hooks(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        executor: &dyn ToolExecutor,
        sink: &mut EventSink,
        turn_start_sequence: u64,
        hooks_digest: &str,
    ) -> Result<crate::ToolOutput, AgentError> {
        let mut contexts = Vec::new();
        for config in self
            .command_hooks
            .for_call(HookPhase::PreToolUse, &call.name)
        {
            let response = self
                .run_one_hook(
                    config,
                    call,
                    None,
                    context,
                    sink,
                    turn_start_sequence,
                    hooks_digest,
                )
                .await?;
            if response.decision == HookDecision::Block {
                return Ok(crate::ToolOutput::error(format!(
                    "PreToolUse hook {} blocked {}: {}",
                    config.id,
                    call.name,
                    response.reason.unwrap_or_default()
                )));
            }
            if let Some(text) = response.additional_context {
                contexts.push(text);
            }
        }
        let mut output = match executor.execute(call, context).await {
            Ok(output) => output,
            Err(error @ AgentError::Cancelled) | Err(error @ AgentError::EffectUncertain(_)) => {
                return Err(error);
            }
            Err(error) => crate::ToolOutput::error(error.to_string()),
        };
        for config in self
            .command_hooks
            .for_call(HookPhase::PostToolUse, &call.name)
        {
            let response = self
                .run_one_hook(
                    config,
                    call,
                    Some(&output),
                    context,
                    sink,
                    turn_start_sequence,
                    hooks_digest,
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

    #[allow(clippy::too_many_arguments)]
    async fn run_one_hook(
        &self,
        config: &crate::CommandHookConfig,
        call: &ToolCall,
        output: Option<&crate::ToolOutput>,
        context: &ToolContext,
        sink: &mut EventSink,
        turn_start_sequence: u64,
        hooks_digest: &str,
    ) -> Result<crate::HookResponse, AgentError> {
        let current = self.command_hooks.digest_hex().map_err(|error| {
            AgentError::EffectUncertain(format!("hook identity changed after tool start: {error}"))
        })?;
        if current != hooks_digest {
            return Err(AgentError::EffectUncertain(
                "hook configuration changed after tool start".into(),
            ));
        }
        let phase = match config.phase {
            HookPhase::PreToolUse => "pre_tool_use",
            HookPhase::PostToolUse => "post_tool_use",
        };
        sink.emit(AgentEventKind::HookStarted {
            tool_call_id: call.id.clone(),
            hook_id: config.id.clone(),
            phase: phase.into(),
        })?;
        let invocation = HookInvocation {
            phase: config.phase,
            session_id: sink.session_id.clone(),
            turn_id: turn_start_sequence.to_string(),
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            tool_input: call.arguments.clone(),
            tool_output: output.map(
                |value| serde_json::json!({"content": value.content, "is_error": value.is_error}),
            ),
        };
        match crate::run_command_hook(config, &invocation, &context.cancel).await {
            HookRunResult::Completed { response, .. } => {
                sink.emit(AgentEventKind::HookFinished {
                    tool_call_id: call.id.clone(),
                    hook_id: config.id.clone(),
                    phase: phase.into(),
                    decision: match response.decision {
                        HookDecision::Allow => "allow",
                        HookDecision::Block => "block",
                    }
                    .into(),
                })?;
                Ok(response)
            }
            HookRunResult::NotStarted(failure) | HookRunResult::OutcomeUncertain(failure) => {
                Err(AgentError::EffectUncertain(format!(
                    "hook {} has no committed outcome after tool start: {failure:?}",
                    config.id
                )))
            }
        }
    }

    async fn check_permission(
        &self,
        call: &ToolCall,
        permission_policy: &PermissionPolicy,
        approval: SessionApprovalContext<'_>,
        cancel: &CancellationToken,
        pending: &PendingPermissions,
        sink: &mut EventSink,
    ) -> Result<Option<crate::ToolOutput>, AgentError> {
        let may_use_grants = !call.name.starts_with("mcp__") || approval.mcp_identity_verified;
        let base_mode = permission_policy.mode_for_call(call);
        let mode = if base_mode == PermissionMode::Ask && may_use_grants {
            match (self.session_approvals.as_ref(), approval.digest) {
                (Some((sessions, _)), Some(digest)) => {
                    let grants = sessions
                        .session_approval_grants(&sink.session_id)
                        .map_err(|error| {
                            AgentError::Runtime(format!("session approval load failed: {error}"))
                        })?
                        .into_iter()
                        .map(SessionApprovalGrant::from)
                        .collect::<Vec<_>>();
                    permission_policy.mode_for_call_with_session_grants(
                        &sink.session_id,
                        approval.workspace,
                        approval.workspace,
                        digest,
                        call,
                        &grants,
                    )
                }
                _ => base_mode,
            }
        } else {
            base_mode
        };
        match mode {
            PermissionMode::Allow => Ok(None),
            PermissionMode::Deny => Ok(Some(crate::ToolOutput::error(format!(
                "Permission denied for {}",
                call.name
            )))),
            PermissionMode::Ask => {
                let request_id = Uuid::new_v4().to_string();
                let (reply_sender, reply_receiver) = tokio::sync::oneshot::channel();
                pending.insert(request_id.clone(), reply_sender);
                if let Err(error) = sink.emit(AgentEventKind::PermissionRequested {
                    request_id: request_id.clone(),
                    tool_call: call.clone(),
                }) {
                    pending.remove(&request_id);
                    return Err(error);
                }
                let outcome = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => (None, false),
                    reply = reply_receiver => match reply {
                        Ok(choice) => (Some(choice), false),
                        Err(_) => (Some(PermissionChoice::Deny), false),
                    },
                    _ = tokio::time::sleep(permission_policy.timeout()) => {
                        (None, true)
                    }
                };
                pending.remove(&request_id);
                let resolution = match outcome {
                    (None, true) => PermissionResolution::TimedOut,
                    (None, false) => PermissionResolution::Cancelled,
                    (Some(PermissionChoice::Allow), _) => PermissionResolution::Allowed,
                    (Some(PermissionChoice::Deny), _) => PermissionResolution::Denied,
                    (Some(PermissionChoice::AllowSession), _) => {
                        if !may_use_grants {
                            return Err(AgentError::InvalidRequest(
                                "session approval requires a verified MCP runtime".into(),
                            ));
                        }
                        let (Some((sessions, _)), Some(digest)) =
                            (self.session_approvals.as_ref(), approval.digest)
                        else {
                            return Err(AgentError::InvalidRequest(
                                "session approvals require a leased native journal and session store"
                                    .into(),
                            ));
                        };
                        let grant = permission_policy
                            .issue_session_grant(
                                &sink.session_id,
                                approval.workspace,
                                approval.workspace,
                                digest,
                                call,
                            )
                            .map_err(|error| {
                                AgentError::InvalidRequest(format!(
                                    "cannot issue session approval: {error}"
                                ))
                            })?;
                        sessions
                            .record_permission_decision_with_grant(
                                &sink.session_id,
                                &request_id,
                                PermissionDecision::Allow,
                                Some(grant.into()),
                            )
                            .map_err(|error| {
                                AgentError::Runtime(format!(
                                    "session approval journal failed: {error}"
                                ))
                            })?;
                        PermissionResolution::Allowed
                    }
                };
                sink.emit(AgentEventKind::PermissionResolved {
                    request_id,
                    tool_call_id: call.id.clone(),
                    resolution,
                })?;
                match resolution {
                    PermissionResolution::Allowed => Ok(None),
                    PermissionResolution::Denied => Ok(Some(crate::ToolOutput::error(format!(
                        "Permission denied for {}",
                        call.name
                    )))),
                    PermissionResolution::TimedOut => Ok(Some(crate::ToolOutput::error(format!(
                        "Permission request timed out for {}",
                        call.name
                    )))),
                    PermissionResolution::Cancelled => Err(AgentError::Cancelled),
                }
            }
        }
    }

    async fn model_step(
        &self,
        messages: &[AgentMessage],
        definitions: &[ToolDefinition],
        cancel: CancellationToken,
        sink: &mut EventSink,
    ) -> Result<crate::ModelCompletion, AgentError> {
        let (delta_sender, mut delta_receiver) = tokio::sync::mpsc::unbounded_channel();
        let future = self.model.stream_complete_detailed(
            messages,
            definitions,
            cancel.clone(),
            delta_sender,
        );
        tokio::pin!(future);
        let output = loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(AgentError::Cancelled),
                result = &mut future => break result?,
                Some(content) = delta_receiver.recv() => {
                    sink.emit(AgentEventKind::AssistantDelta { content })?;
                }
            }
        };
        while let Ok(content) = delta_receiver.try_recv() {
            sink.emit(AgentEventKind::AssistantDelta { content })?;
        }
        Ok(output)
    }
}

fn canonical_workspace(path: &Path) -> Result<PathBuf, AgentError> {
    let path = path
        .canonicalize()
        .map_err(|error| AgentError::InvalidRequest(format!("workspace: {error}")))?;
    if !path.is_dir() {
        return Err(AgentError::InvalidRequest(
            "workspace is not a directory".into(),
        ));
    }
    Ok(path)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}\n[tool output truncated]")
    } else {
        prefix
    }
}

struct EventSink {
    session_id: String,
    sequence: u64,
    sender: mpsc::Sender<AgentEvent>,
    journal: Option<Arc<dyn NativeEventJournal>>,
    journal_failed: bool,
    journal_error: Arc<Mutex<Option<String>>>,
}

impl EventSink {
    /// Forward a checkpoint marker that the coordinator already synced to
    /// the same journal. Never append it a second time.
    fn forward_persisted(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        if self.journal_failed
            || event.session_id != self.session_id
            || self.sequence.checked_add(1) != Some(event.sequence)
        {
            self.journal_failed = true;
            let message = "persisted checkpoint marker is out of event sequence".to_owned();
            *self
                .journal_error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(message.clone());
            return Err(AgentError::Runtime(message));
        }
        self.sequence = event.sequence;
        self.sender.send(event).map_err(|_| AgentError::Cancelled)
    }

    fn emit(&mut self, kind: AgentEventKind) -> Result<(), AgentError> {
        if self.journal_failed {
            return Err(AgentError::Runtime(
                "session journal failed earlier in this run".into(),
            ));
        }
        let sequence = self.sequence.checked_add(1).ok_or_else(|| {
            AgentError::Limit("event sequence exceeded the supported range".into())
        })?;
        let event = AgentEvent::new(&self.session_id, sequence, kind);
        if let Some(journal) = &self.journal {
            if let Err(error) = journal.append_native_event(&event) {
                self.journal_failed = true;
                let message = format!("session journal could not persist event: {error}");
                *self
                    .journal_error
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Some(message.clone());
                return Err(AgentError::Runtime(message));
            }
        }
        self.sequence = sequence;
        self.sender.send(event).map_err(|_| AgentError::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelOutput, ResponsesBackend, ToolOutput};
    use async_trait::async_trait;
    use openide_core::{
        replay_native, CheckpointCompatibility, CheckpointFileStore, MessageRole, SessionStore,
        StoreError, ToolCall,
    };
    use serde_json::json;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    struct ScriptedModel {
        outputs: Mutex<Vec<ModelOutput>>,
        seen: Mutex<Vec<Vec<AgentMessage>>>,
    }

    #[async_trait]
    impl ModelBackend for ScriptedModel {
        fn model_id(&self) -> &str {
            "scripted"
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
            self.seen.lock().unwrap().push(messages.to_vec());
            Ok(self.outputs.lock().unwrap().remove(0))
        }
    }

    struct EchoTool;

    struct UnprofiledModel {
        calls: AtomicUsize,
    }

    struct ForbiddenPreparation;

    #[async_trait]
    impl ToolPreparation for ForbiddenPreparation {
        async fn prepare(
            &self,
            _workspace: &Path,
            _base_tools: Arc<dyn ToolExecutor>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn crate::PreparedToolSession>, AgentError> {
            panic!("MCP preparation must not run without a request profile");
        }
    }

    #[async_trait]
    impl ModelBackend for UnprofiledModel {
        fn model_id(&self) -> &str {
            "scripted"
        }

        async fn complete(
            &self,
            _messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                content: "Should not run".into(),
                tool_calls: vec![],
            })
        }
    }

    #[async_trait]
    impl ToolExecutor for EchoTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo text".into(),
                parameters: json!({"type":"object"}),
            }]
        }

        async fn execute(
            &self,
            call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            Ok(ToolOutput::success(
                call.arguments["text"].as_str().unwrap(),
            ))
        }
    }

    struct CountingTool {
        executions: AtomicUsize,
    }

    struct WaveReadTools {
        store: Arc<SessionStore>,
        session_id: String,
        parallel_safe: bool,
        executions: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
        completion_order: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ToolExecutor for WaveReadTools {
        fn certifies_parallel_read(&self, name: &str) -> bool {
            self.parallel_safe && name == "read_file"
        }

        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: json!({"type":"object"}),
            }]
        }

        async fn execute(
            &self,
            call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            if self.parallel_safe {
                let saved = self
                    .store
                    .load(&self.session_id)
                    .map_err(|error| {
                        AgentError::Runtime(format!("cannot inspect durable starts: {error}"))
                    })?
                    .ok_or_else(|| AgentError::Runtime("session disappeared".into()))?;
                let started = saved
                    .events
                    .iter()
                    .filter_map(|event| match &event.kind {
                        AgentEventKind::ToolStarted { tool_call } => Some(tool_call.id.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if started != ["read-a", "read-b"]
                    || saved
                        .events
                        .iter()
                        .any(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. }))
                {
                    return Err(AgentError::Runtime(
                        "read wave started before all markers synced".into(),
                    ));
                }
            }
            struct Active<'a>(&'a AtomicUsize);
            impl Drop for Active<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            self.executions.fetch_add(1, Ordering::SeqCst);
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _active = Active(&self.active);
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(if call.id == "read-a" {
                50
            } else {
                10
            }))
            .await;
            self.completion_order.lock().unwrap().push(call.id.clone());
            Ok(ToolOutput::success(call.id.clone()))
        }
    }

    struct UncertainTool {
        executions: AtomicUsize,
    }

    #[async_trait]
    impl ToolExecutor for UncertainTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            EchoTool.definitions()
        }

        async fn execute(
            &self,
            _call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Err(AgentError::EffectUncertain(
                "tool may have completed before its connection failed".into(),
            ))
        }
    }

    #[async_trait]
    impl ToolExecutor for CountingTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo text".into(),
                parameters: json!({"type":"object"}),
            }]
        }

        async fn execute(
            &self,
            call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::success(
                call.arguments["text"].as_str().unwrap(),
            ))
        }
    }

    fn model_with_tool_call() -> Arc<ScriptedModel> {
        Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_permission".into(),
                        name: "echo".into(),
                        arguments: json!({"text":"hello"}),
                    }],
                },
                ModelOutput {
                    content: "Done".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        })
    }

    fn model_with_two_reads() -> Arc<ScriptedModel> {
        Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![
                        ToolCall {
                            id: "read-a".into(),
                            name: "read_file".into(),
                            arguments: json!({"path":"a.txt"}),
                        },
                        ToolCall {
                            id: "read-b".into(),
                            name: "read_file".into(),
                            arguments: json!({"path":"b.txt"}),
                        },
                    ],
                },
                ModelOutput {
                    content: "Done".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        })
    }

    fn command_hook_config(
        workspace: &Path,
        phase: HookPhase,
        script: &str,
        timeout_ms: u64,
    ) -> crate::CommandHookConfig {
        #[cfg(windows)]
        let (executable, args) = (
            PathBuf::from(std::env::var_os("SystemRoot").unwrap())
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
        let (executable, args) = (PathBuf::from("/bin/sh"), vec!["-c".into(), script.into()]);
        crate::CommandHookConfig {
            id: "hook-1".into(),
            phase,
            tool_name: "echo".into(),
            executable,
            args,
            cwd: workspace.into(),
            timeout_ms,
            max_output_bytes: 4_096,
        }
    }

    #[test]
    fn pre_hook_block_is_synced_and_prevents_tool_effect() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        #[cfg(windows)]
        let script = "[Console]::Out.Write('{\"decision\":\"block\",\"reason\":\"host denied\"}')";
        #[cfg(unix)]
        let script = "printf '%s' '{\"decision\":\"block\",\"reason\":\"host denied\"}'";
        let hooks = Arc::new(
            CommandHooks::new(vec![command_hook_config(
                dir.path(),
                HookPhase::PreToolUse,
                script,
                5_000,
            )])
            .unwrap(),
        );
        let tools = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tools.clone())
            .with_command_hooks(hooks)
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone());
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert_eq!(tools.executions.load(Ordering::SeqCst), 0);
        let saved = store.load(&session_id).unwrap().unwrap().events;
        let position = |predicate: fn(&AgentEventKind) -> bool| {
            saved
                .iter()
                .position(|event| predicate(&event.kind))
                .unwrap()
        };
        let catalog = position(|kind| matches!(kind, AgentEventKind::ToolCatalogCommitted { .. }));
        let policy = position(|kind| matches!(kind, AgentEventKind::HooksCommitted { .. }));
        let exec_policy =
            position(|kind| matches!(kind, AgentEventKind::ExecPolicyCommitted { .. }));
        let model = position(|kind| matches!(kind, AgentEventKind::ModelRequestStarted { .. }));
        let tool = position(|kind| matches!(kind, AgentEventKind::ToolStarted { .. }));
        let hook_start = position(|kind| matches!(kind, AgentEventKind::HookStarted { .. }));
        let hook_finish = position(|kind| matches!(kind, AgentEventKind::HookFinished { .. }));
        let finish = position(|kind| matches!(kind, AgentEventKind::ToolFinished { .. }));
        assert!(catalog < policy && policy < exec_policy && exec_policy < model);
        assert!(tool < hook_start && hook_start < hook_finish && hook_finish < finish);
        assert!(
            matches!(&saved[finish].kind, AgentEventKind::ToolFinished {is_error: true, output, ..} if output.contains("host denied"))
        );
    }

    #[test]
    fn post_hook_timeout_after_tool_effect_requires_manual_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        #[cfg(windows)]
        let script = "[IO.File]::WriteAllText('post-started.txt','yes'); Start-Sleep -Seconds 10";
        #[cfg(unix)]
        let script = "printf yes > post-started.txt; sleep 10";
        // PowerShell startup can exceed a second during a parallel test run;
        // leave time for the marker write before testing the uncertain timeout.
        #[cfg(windows)]
        let timeout_ms = 5_000;
        #[cfg(unix)]
        let timeout_ms = 1_000;
        let hooks = Arc::new(
            CommandHooks::new(vec![command_hook_config(
                dir.path(),
                HookPhase::PostToolUse,
                script,
                timeout_ms,
            )])
            .unwrap(),
        );
        let tools = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tools.clone())
            .with_command_hooks(hooks)
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone());
        let events = receive_until_terminal_with_timeout(
            &runner.start_with_lease(request, lease),
            Duration::from_secs(15),
        );
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnFailed { .. }
        ));
        assert_eq!(tools.executions.load(Ordering::SeqCst), 1);
        assert!(dir.path().join("post-started.txt").exists());
        let saved = store.load(&session_id).unwrap().unwrap().events;
        assert!(saved.iter().any(|event| matches!(&event.kind, AgentEventKind::HookStarted {hook_id, ..} if hook_id == "hook-1")));
        assert!(!saved.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::HookFinished { .. } | AgentEventKind::ToolFinished { .. }
        )));
        let replay = replay_native(
            &session_id,
            &saved,
            None,
            &CheckpointCompatibility {
                provider: "test".into(),
                model: "scripted".into(),
                context_format_key: "responses-v1".into(),
            },
        )
        .unwrap();
        assert!(
            matches!(replay.recovery, ReplayRecovery::ToolOutcomeUncertain { tool_call } if tool_call.id == "call_permission")
        );
        assert!(!replay.can_start_new_turn);
    }

    fn receive_until_terminal(handle: &AgentRunHandle) -> Vec<AgentEvent> {
        receive_until_terminal_with_timeout(handle, Duration::from_secs(5))
    }

    fn receive_until_terminal_with_timeout(
        handle: &AgentRunHandle,
        timeout: Duration,
    ) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        loop {
            let event = handle.events.recv_timeout(timeout).expect("terminal event");
            let terminal = matches!(
                event.kind,
                AgentEventKind::TurnCompleted { .. }
                    | AgentEventKind::TurnFailed { .. }
                    | AgentEventKind::TurnCancelled
            );
            events.push(event);
            if terminal {
                return events;
            }
        }
    }

    #[test]
    fn native_event_channel_closes_after_session_lease_is_released() {
        let workspace = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
        let request = journaled_request(&store, workspace.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![ModelOutput {
                content: "done".into(),
                tool_calls: Vec::new(),
            }]),
            seen: Mutex::new(Vec::new()),
        });
        let runner = AgentRunner::new(model, Arc::new(EchoTool))
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone());
        let handle = runner.start_with_lease(request, lease);
        let mut terminal = false;
        loop {
            match handle.events.recv_timeout(Duration::from_secs(5)) {
                Ok(event) => {
                    terminal |= matches!(event.kind, AgentEventKind::TurnCompleted { .. });
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(error) => panic!("worker channel did not close: {error}"),
            }
        }
        assert!(terminal);
        assert!(store.try_acquire_native_lease(&session_id).is_ok());
    }

    #[derive(Clone, Copy)]
    enum JournalFailure {
        ModelRequest,
        ModelResponse,
        McpRuntime,
        ToolStarted,
        ToolFinished,
    }

    struct FailingJournal {
        store: Arc<SessionStore>,
        failure: JournalFailure,
    }

    impl NativeEventJournal for FailingJournal {
        fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
            let fail = matches!(
                (self.failure, &event.kind),
                (
                    JournalFailure::ModelRequest,
                    AgentEventKind::ModelRequestStarted { .. }
                ) | (
                    JournalFailure::ModelResponse,
                    AgentEventKind::ModelResponseCommitted { .. }
                ) | (
                    JournalFailure::McpRuntime,
                    AgentEventKind::McpRuntimeCommitted { .. }
                ) | (
                    JournalFailure::ToolStarted,
                    AgentEventKind::ToolStarted { .. }
                ) | (
                    JournalFailure::ToolFinished,
                    AgentEventKind::ToolFinished { .. }
                )
            );
            if fail {
                return Err(StoreError::Io(std::io::Error::other(
                    "injected journal failure",
                )));
            }
            self.store.append_native_event(event)
        }
    }

    fn journaled_request(store: &SessionStore, workspace: &Path) -> AgentRunRequest {
        let session = store.create(workspace, "native").unwrap();
        let admitted = store
            .admit_native_turn(
                &session.id,
                "turn-1",
                "Say hello",
                "scripted",
                TurnMode::Build,
            )
            .unwrap();
        let mut request = request(workspace.to_path_buf());
        request.session_id = Some(session.id);
        request.sequence_start = admitted.sequence;
        request
    }

    #[test]
    fn nested_project_rules_are_journaled_before_the_file_tool_runs() {
        let workspace = tempfile::tempdir().unwrap();
        let nested = workspace.path().join("src");
        std::fs::create_dir(&nested).unwrap();
        let rule = format!("Inspect src files before editing. {}", "x".repeat(1_800));
        std::fs::write(nested.join("AGENTS.md"), &rule).unwrap();
        std::fs::write(nested.join("file.txt"), "file payload").unwrap();
        let store = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
        let request = journaled_request(&store, workspace.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let call = |id: &str| ToolCall {
            id: id.into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/file.txt"}),
        };
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![call("first")],
                },
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![call("second")],
                },
                ModelOutput {
                    content: "Done".into(),
                    tool_calls: Vec::new(),
                },
            ]),
            seen: Mutex::new(Vec::new()),
        });
        let runner = AgentRunner::new(model.clone(), Arc::new(crate::LocalTools::new()))
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone())
            .with_native_steering(store.clone())
            .with_context_policy(ContextPolicy {
                max_request_chars: 1_200,
                keep_recent_tool_results: 0,
            });
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        let outputs = events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::ToolFinished { output, .. } => Some(output.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(outputs.len(), 2);
        assert!(outputs[0].starts_with("OPENIDE_PROJECT_INSTRUCTIONS_V1\n"));
        assert!(outputs[1].contains("file payload"));
        let seen = model.seen.lock().unwrap();
        assert!(seen[1].iter().any(|message| {
            message.role == MessageRole::System
                && message
                    .content
                    .contains("Inspect src files before editing.")
        }));
        assert!(seen[1].iter().any(|message| {
            message.tool_call_id.as_deref() == Some("first")
                && message.content == "[Earlier tool result omitted from model context]"
        }));
    }

    struct IdentityPreparation;

    struct IdentitySession {
        tools: Arc<dyn ToolExecutor>,
        digest: String,
    }

    #[async_trait]
    impl ToolPreparation for IdentityPreparation {
        async fn prepare(
            &self,
            _workspace: &Path,
            base_tools: Arc<dyn ToolExecutor>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn crate::PreparedToolSession>, AgentError> {
            Ok(Box::new(IdentitySession {
                tools: base_tools,
                digest: "a".repeat(64),
            }))
        }
    }

    #[async_trait]
    impl crate::PreparedToolSession for IdentitySession {
        fn tools(&self) -> Arc<dyn ToolExecutor> {
            self.tools.clone()
        }

        fn ask_tool_names(&self) -> &[String] {
            &[]
        }

        fn mcp_resume_identity_digest(&self) -> Option<&str> {
            Some(&self.digest)
        }

        async fn shutdown(&self) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[test]
    fn prepared_identity_is_synced_before_first_model_request() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let runner = AgentRunner::new(model_with_tool_call(), Arc::new(EchoTool))
            .with_tool_preparation(Arc::new(IdentityPreparation))
            .with_system_instructions("Original AGENTS.md for this turn")
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone());
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        let saved = store.load(&session_id).unwrap().unwrap().events;
        let catalog = saved
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ToolCatalogCommitted { .. }))
            .unwrap();
        let mcp = saved
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::McpRuntimeCommitted { .. }))
            .unwrap();
        let hooks = saved
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::HooksCommitted { .. }))
            .unwrap();
        let instructions = saved
            .iter()
            .position(|event| {
                matches!(
                    event.kind,
                    AgentEventKind::SystemInstructionsCommitted { .. }
                )
            })
            .unwrap();
        match &saved[instructions].kind {
            AgentEventKind::SystemInstructionsCommitted {
                version,
                digest,
                content,
            } => {
                assert_eq!(*version, SYSTEM_INSTRUCTIONS_VERSION);
                assert_eq!(content, "Original AGENTS.md for this turn");
                assert_eq!(system_instructions_digest(content).as_ref(), Some(digest));
            }
            _ => unreachable!(),
        }
        let context = saved
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ModelContextCommitted { .. }))
            .unwrap();
        let model = saved
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. }))
            .unwrap();
        assert!(
            catalog < mcp
                && mcp < hooks
                && hooks < instructions
                && instructions < context
                && context < model
        );
    }

    #[test]
    fn failed_mcp_identity_commit_prevents_first_model_request() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let model = model_with_tool_call();
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool))
            .with_tool_preparation(Arc::new(IdentityPreparation))
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(Arc::new(FailingJournal {
                store: store.clone(),
                failure: JournalFailure::McpRuntime,
            }));
        let handle = runner.start_with_lease(request, lease);
        receive_until_disconnect(&handle);
        assert!(model.seen.lock().unwrap().is_empty());
        assert!(handle.journal_error().is_some());
        let saved = store.load(&session_id).unwrap().unwrap().events;
        assert!(!saved
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. })));
    }

    #[test]
    fn oversized_system_instructions_never_reach_native_model() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let model = model_with_tool_call();
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool))
            .with_system_instructions("x".repeat(MAX_SYSTEM_INSTRUCTIONS_BYTES + 1))
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone());
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnFailed { .. }
        ));
        assert!(model.seen.lock().unwrap().is_empty());
        let saved = store.load(&session_id).unwrap().unwrap().events;
        assert!(!saved.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::SystemInstructionsCommitted { .. }
                | AgentEventKind::ModelRequestStarted { .. }
        )));
    }

    #[test]
    fn native_read_wave_syncs_all_starts_and_commits_results_in_model_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let tools = Arc::new(WaveReadTools {
            store: store.clone(),
            session_id: session_id.clone(),
            parallel_safe: true,
            executions: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            completion_order: Mutex::new(Vec::new()),
        });
        let model = model_with_two_reads();
        let runner = AgentRunner::new(model.clone(), tools.clone())
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone());
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert_eq!(tools.executions.load(Ordering::SeqCst), 2);
        assert_eq!(tools.peak.load(Ordering::SeqCst), 2);
        assert_eq!(
            *tools.completion_order.lock().unwrap(),
            ["read-b", "read-a"]
        );
        let saved = store.load(&session_id).unwrap().unwrap().events;
        let starts = saved
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::ToolStarted { tool_call } => {
                    Some((event.sequence, tool_call.id.as_str()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let finishes = saved
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::ToolFinished { tool_call_id, .. } => {
                    Some((event.sequence, tool_call_id.as_str()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            starts.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
            ["read-a", "read-b"]
        );
        assert_eq!(
            finishes.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
            ["read-a", "read-b"]
        );
        assert!(starts[1].0 < finishes[0].0);
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[1]
                .iter()
                .filter_map(|message| message.tool_call_id.as_deref())
                .collect::<Vec<_>>(),
            ["read-a", "read-b"]
        );
    }

    #[test]
    fn executor_with_read_file_name_but_no_read_certification_runs_serially() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let tools = Arc::new(WaveReadTools {
            store: store.clone(),
            session_id,
            parallel_safe: false,
            executions: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            completion_order: Mutex::new(Vec::new()),
        });
        let runner = AgentRunner::new(model_with_two_reads(), tools.clone())
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store);
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert_eq!(tools.executions.load(Ordering::SeqCst), 2);
        assert_eq!(tools.peak.load(Ordering::SeqCst), 1);
        assert_eq!(
            *tools.completion_order.lock().unwrap(),
            ["read-a", "read-b"]
        );
        let kinds = events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::ToolStarted { tool_call } => {
                    Some(format!("start:{}", tool_call.id))
                }
                AgentEventKind::ToolFinished { tool_call_id, .. } => {
                    Some(format!("finish:{tool_call_id}"))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "start:read-a",
                "finish:read-a",
                "start:read-b",
                "finish:read-b"
            ]
        );
    }

    struct FailSecondStartJournal {
        store: Arc<SessionStore>,
        starts: AtomicUsize,
    }

    impl NativeEventJournal for FailSecondStartJournal {
        fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
            if matches!(event.kind, AgentEventKind::ToolStarted { .. })
                && self.starts.fetch_add(1, Ordering::SeqCst) == 1
            {
                return Err(StoreError::Io(std::io::Error::other(
                    "injected second start failure",
                )));
            }
            self.store.append_native_event(event)
        }
    }

    #[test]
    fn failed_second_start_prevents_all_read_effects_and_leaves_first_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let tools = Arc::new(WaveReadTools {
            store: store.clone(),
            session_id: session_id.clone(),
            parallel_safe: true,
            executions: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            completion_order: Mutex::new(Vec::new()),
        });
        let runner = AgentRunner::new(model_with_two_reads(), tools.clone())
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(Arc::new(FailSecondStartJournal {
                store: store.clone(),
                starts: AtomicUsize::new(0),
            }));
        let handle = runner.start_with_lease(request, lease);
        receive_until_disconnect(&handle);
        assert_eq!(tools.executions.load(Ordering::SeqCst), 0);
        assert!(handle.journal_error().is_some());
        let saved = store.load(&session_id).unwrap().unwrap().events;
        let replay = replay_native(
            &session_id,
            &saved,
            None,
            &CheckpointCompatibility {
                model: "scripted".into(),
                provider: "test".into(),
                context_format_key: "test".into(),
            },
        )
        .unwrap();
        assert!(
            matches!(replay.recovery, ReplayRecovery::ToolOutcomeUncertain { tool_call } if tool_call.id == "read-a")
        );
        assert!(!replay.can_start_new_turn);
    }

    fn receive_until_disconnect(handle: &AgentRunHandle) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        loop {
            match handle.events.recv_timeout(Duration::from_secs(5)) {
                Ok(event) => events.push(event),
                Err(mpsc::RecvTimeoutError::Disconnected) => return events,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("worker did not stop"),
            }
        }
    }

    #[test]
    fn journal_failure_before_model_request_prevents_model_call() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let model = model_with_tool_call();
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool)).with_event_journal(
            Arc::new(FailingJournal {
                store: store.clone(),
                failure: JournalFailure::ModelRequest,
            }),
        );
        let handle = runner.start(request);
        let events = receive_until_disconnect(&handle);
        assert!(model.seen.lock().unwrap().is_empty());
        assert!(!events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::ModelRequestStarted { .. } | AgentEventKind::TurnFailed { .. }
        )));
        let saved = store.load(&handle.session_id).unwrap().unwrap().events;
        assert_eq!(saved.len(), events.len() + 1);
    }

    #[test]
    fn leased_native_turn_without_request_profile_fails_before_model_request() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let lease = store
            .try_acquire_native_lease(request.session_id.as_deref().unwrap())
            .unwrap();
        let model = Arc::new(UnprofiledModel {
            calls: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool))
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_tool_preparation(Arc::new(ForbiddenPreparation))
            .with_event_journal(store.clone());
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            &events.last().unwrap().kind,
            AgentEventKind::TurnFailed { error } if error.contains("request profile")
        ));
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert!(!events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::ModelContextCommitted { .. }
                | AgentEventKind::ModelRequestStarted { .. }
        )));
        let saved = store.load(&events[0].session_id).unwrap().unwrap();
        assert_eq!(saved.events.len(), events.len() + 1);
    }

    #[test]
    fn token_budget_stops_before_model_without_dropping_history() {
        let dir = tempfile::tempdir().unwrap();
        let model = model_with_tool_call();
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool)).with_token_budget_policy(
            TokenBudgetPolicy {
                context_window_tokens: 200,
                output_reserve_tokens: 20,
                tool_schema_reserve_tokens: 10,
                auto_compact_token_limit: None,
            },
        );
        let handle = runner.start(request(dir.path().to_path_buf()));
        let events = receive_until_terminal(&handle);
        assert!(model.seen.lock().unwrap().is_empty());
        assert!(matches!(
            &events.last().unwrap().kind,
            AgentEventKind::TurnFailed { error } if error.contains("semantic compaction")
        ));
    }

    #[test]
    fn image_budget_uses_http_count_and_rejects_before_model_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /v1/responses/input_tokens HTTP/1.1"));
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0u8; length];
            reader.read_exact(&mut bytes).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["input"][1]["content"][0]["type"], "input_text");
            assert_eq!(body["input"][1]["content"][1]["type"], "input_image");
            assert_eq!(body["tools"][0]["name"], "echo");
            let response = r#"{"object":"response.input_tokens","input_tokens":190}"#;
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{response}",
                response.len()
            )
            .unwrap();
            socket.flush().unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let mut request = request(dir.path().to_path_buf());
        let image =
            UserImage::from_bytes(openide_core::UserImageMime::Png, b"\x89PNG\r\n\x1a\nimage")
                .unwrap();
        request.history = vec![AgentMessage::user_with_images("earlier", vec![image]).unwrap()];
        let model = Arc::new(ResponsesBackend::new(
            format!("http://{address}/v1"),
            None,
            "mock",
        ));
        let runner = AgentRunner::new(model, Arc::new(EchoTool)).with_token_budget_policy(
            TokenBudgetPolicy {
                context_window_tokens: 200,
                output_reserve_tokens: 20,
                tool_schema_reserve_tokens: 0,
                auto_compact_token_limit: None,
            },
        );
        let events = receive_until_terminal(&runner.start(request));
        server.join().unwrap();
        assert!(matches!(
            &events.last().unwrap().kind,
            AgentEventKind::TurnFailed { error } if error.contains("semantic compaction")
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. })));
    }

    fn run_auto_compaction_case(reject_before_compaction: bool) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let store = Arc::new(SessionStore::open(&root).unwrap());
        let checkpoints = Arc::new(CheckpointFileStore::new(&root).unwrap());
        let session = store.create(dir.path(), "native").unwrap();
        let prior_response = "earlier confirmed result ".repeat(300);
        let first = store
            .admit_native_turn(
                &session.id,
                "turn-1",
                "Prior task",
                "scripted",
                TurnMode::Build,
            )
            .unwrap();
        let mut sequence = first.sequence;
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Prior task".into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "scripted".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant(&prior_response, vec![]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::AssistantMessage {
                content: prior_response.clone(),
            },
            AgentEventKind::TurnCompleted {
                response: prior_response.clone(),
                steps: 1,
            },
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(&session.id, sequence, kind))
                .unwrap();
        }
        let compatibility = CheckpointCompatibility {
            provider: "test".into(),
            model: "scripted".into(),
            context_format_key: "test-v1".into(),
        };
        let record = store.load(&session.id).unwrap().unwrap();
        let prior_history = replay_native(
            &session.id,
            &record.events,
            Some(&checkpoints),
            &compatibility,
        )
        .unwrap()
        .history;
        let lease = store.try_acquire_native_lease(&session.id).unwrap();
        let admission = store
            .admit_native_turn(
                &session.id,
                "turn-2",
                "Continue the task",
                "scripted",
                TurnMode::Build,
            )
            .unwrap();
        let mut request = request(dir.path().to_path_buf());
        request.session_id = Some(session.id.clone());
        request.sequence_start = admission.sequence;
        request.history = prior_history;
        request.prompt = "Continue the task".into();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call-mid-turn".into(),
                        name: "echo".into(),
                        arguments: json!({"text":"t".repeat(2_000)}),
                    }],
                },
                ModelOutput {
                    content: json!({
                        "objective":"Continue the task",
                        "decisions":["Use the native Rust runner"],
                        "files":[],
                        "current_state":"Earlier task complete; active tool returned",
                        "constraints":[],
                        "remaining_work":["Finish the active turn"]
                    })
                    .to_string(),
                    tool_calls: vec![],
                },
                ModelOutput {
                    content: "Finished".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        });
        let budget = TokenBudgetPolicy {
            context_window_tokens: if reject_before_compaction {
                12_000
            } else {
                30_000
            },
            output_reserve_tokens: 100,
            tool_schema_reserve_tokens: 100,
            auto_compact_token_limit: (!reject_before_compaction).then_some(10_000),
        };
        let runner = AgentRunner::new(model.clone(), tool.clone())
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_token_budget_policy(budget)
            .with_auto_compaction(store.clone(), checkpoints.clone(), compatibility.clone());
        let handle = runner.start_with_lease(request, lease);
        let events = receive_until_terminal(&handle);
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { steps: 2, .. }
        ));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 1);
        let marker = events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::CheckpointCommitted { .. }))
            .collect::<Vec<_>>();
        assert_eq!(marker.len(), 1);
        let record = store.load(&session.id).unwrap().unwrap();
        assert_eq!(record.events.len(), events.len() + 7);
        assert!(record
            .events
            .iter()
            .enumerate()
            .all(|(index, event)| event.sequence == index as u64 + 1));
        let replay = replay_native(
            &session.id,
            &record.events,
            Some(&checkpoints),
            &compatibility,
        )
        .unwrap();
        assert!(replay.can_start_new_turn);
        assert_eq!(replay.checkpoint.unwrap().covered_event_sequence, 6);
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen[0]
            .iter()
            .any(|message| message.content == prior_response));
        assert!(seen[1][1].content.contains("covered event sequence"));
        assert!(!seen[2]
            .iter()
            .any(|message| message.content == prior_response));
        assert!(seen[2]
            .iter()
            .any(|message| message.role == MessageRole::User
                && message.content == "Continue the task"));
        assert!(seen[2]
            .iter()
            .any(|message| message.role == MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("call-mid-turn")));
    }

    #[test]
    fn auto_compaction_resumes_mid_turn_after_soft_threshold() {
        run_auto_compaction_case(false);
    }

    #[test]
    fn auto_compaction_recovers_a_request_over_hard_limit() {
        run_auto_compaction_case(true);
    }

    #[test]
    fn auto_compaction_without_a_safe_prefix_fails_before_model() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let store = Arc::new(SessionStore::open(&root).unwrap());
        let checkpoints = Arc::new(CheckpointFileStore::new(&root).unwrap());
        let request = journaled_request(&store, dir.path());
        let lease = store
            .try_acquire_native_lease(request.session_id.as_deref().unwrap())
            .unwrap();
        let model = model_with_tool_call();
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool))
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_token_budget_policy(TokenBudgetPolicy {
                context_window_tokens: 200,
                output_reserve_tokens: 20,
                tool_schema_reserve_tokens: 10,
                auto_compact_token_limit: None,
            })
            .with_auto_compaction(
                store.clone(),
                checkpoints,
                CheckpointCompatibility {
                    provider: "test".into(),
                    model: "scripted".into(),
                    context_format_key: "test-v1".into(),
                },
            );
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnFailed { .. }
        ));
        assert!(model.seen.lock().unwrap().is_empty());
        let saved = store.load(&events[0].session_id).unwrap().unwrap();
        assert!(!saved
            .events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::CheckpointCommitted { .. })));
    }

    #[test]
    fn journal_failure_before_tool_started_prevents_tool_effect() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone()).with_event_journal(
            Arc::new(FailingJournal {
                store: store.clone(),
                failure: JournalFailure::ToolStarted,
            }),
        );
        let handle = runner.start(request);
        let events = receive_until_disconnect(&handle);
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
        assert!(!events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::ToolStarted { .. } | AgentEventKind::TurnFailed { .. }
        )));
        let saved = store.load(&handle.session_id).unwrap().unwrap().events;
        assert!(!saved
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. })));
    }

    #[test]
    fn journal_failure_on_model_response_prevents_tool_effect() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone()).with_event_journal(
            Arc::new(FailingJournal {
                store: store.clone(),
                failure: JournalFailure::ModelResponse,
            }),
        );
        let handle = runner.start(request);
        let events = receive_until_disconnect(&handle);
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
        assert!(!events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::ModelResponseCommitted { .. }
                | AgentEventKind::ToolStarted { .. }
                | AgentEventKind::TurnFailed { .. }
        )));
        let saved = store.load(&handle.session_id).unwrap().unwrap().events;
        assert!(!saved
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. })));
    }

    #[test]
    fn unfinished_tool_after_journal_failure_is_not_reexecuted_on_same_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone()).with_event_journal(
            Arc::new(FailingJournal {
                store: store.clone(),
                failure: JournalFailure::ToolFinished,
            }),
        );
        let handle = runner.start(request.clone());
        let events = receive_until_disconnect(&handle);
        assert_eq!(tool.executions.load(Ordering::SeqCst), 1);
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. })));
        let saved = store.load(&handle.session_id).unwrap().unwrap().events;
        assert!(saved
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. })));
        assert!(!saved
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. })));

        // Reusing an admitted request starts at its original sequence. The
        // journal rejects that stale sequence before another model/tool call.
        let retry_model = model_with_tool_call();
        let retry = AgentRunner::new(retry_model.clone(), tool.clone())
            .with_event_journal(store.clone())
            .start(request);
        let retry_events = receive_until_disconnect(&retry);
        assert!(retry_events.is_empty());
        assert!(retry_model.seen.lock().unwrap().is_empty());
        assert_eq!(tool.executions.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn uncertain_tool_effect_remains_open_for_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let model = model_with_tool_call();
        let tool = Arc::new(UncertainTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model.clone(), tool.clone())
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone());
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));

        assert_eq!(tool.executions.load(Ordering::SeqCst), 1);
        assert_eq!(model.seen.lock().unwrap().len(), 1);
        assert!(matches!(
            &events.last().unwrap().kind,
            AgentEventKind::TurnFailed { error } if error.contains("may have completed")
        ));
        let saved = store.load(&session_id).unwrap().unwrap().events;
        assert!(saved
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. })));
        assert!(!saved
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. })));
        let replay = replay_native(
            &session_id,
            &saved,
            None,
            &CheckpointCompatibility {
                provider: "test".into(),
                model: "scripted".into(),
                context_format_key: "test-v1".into(),
            },
        )
        .unwrap();
        assert!(matches!(
            replay.recovery,
            ReplayRecovery::ToolOutcomeUncertain { tool_call } if tool_call.id == "call_permission"
        ));
    }

    fn wait_for_permission(handle: &AgentRunHandle) -> String {
        loop {
            let event = handle
                .events
                .recv_timeout(Duration::from_secs(5))
                .expect("permission request");
            if let AgentEventKind::PermissionRequested {
                request_id,
                tool_call,
            } = event.kind
            {
                assert_eq!(tool_call.id, "call_permission");
                assert_eq!(tool_call.arguments, json!({"text":"hello"}));
                return request_id;
            }
        }
    }

    fn request(workspace: PathBuf) -> AgentRunRequest {
        AgentRunRequest {
            session_id: Some("session_1".into()),
            sequence_start: 7,
            workspace,
            history: vec![],
            prompt: "Say hello".into(),
            mode: TurnMode::Build,
        }
    }

    struct RuntimeEcho {
        requests:
            tokio::sync::mpsc::UnboundedSender<(String, tokio::sync::oneshot::Sender<String>)>,
    }

    #[async_trait]
    impl ToolExecutor for RuntimeEcho {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "echo".into(),
                description: "Echo through the active Tokio worker".into(),
                parameters: json!({"type":"object","properties":{"text":{"type":"string"}}}),
            }]
        }

        async fn execute(
            &self,
            call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            self.requests
                .send((call.arguments["text"].as_str().unwrap().to_owned(), sender))
                .map_err(|_| AgentError::Runtime("prepared tool worker stopped".into()))?;
            let answer = receiver
                .await
                .map_err(|_| AgentError::Runtime("prepared tool worker stopped".into()))?;
            Ok(ToolOutput::success(answer))
        }
    }

    struct TrackingPreparation {
        closed: Arc<std::sync::atomic::AtomicBool>,
        fail_prepare: bool,
        fail_shutdown: bool,
    }

    struct TrackingToolSession {
        tools: Arc<dyn ToolExecutor>,
        names: Vec<String>,
        worker: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
        closed: Arc<std::sync::atomic::AtomicBool>,
        fail_shutdown: bool,
    }

    #[async_trait]
    impl ToolPreparation for TrackingPreparation {
        async fn prepare(
            &self,
            _workspace: &Path,
            _base_tools: Arc<dyn ToolExecutor>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn crate::PreparedToolSession>, AgentError> {
            if self.fail_prepare {
                return Err(AgentError::Runtime("discovery failed".into()));
            }
            let (requests, mut receiver) = tokio::sync::mpsc::unbounded_channel::<(
                String,
                tokio::sync::oneshot::Sender<String>,
            )>();
            let worker = tokio::spawn(async move {
                while let Some((text, reply)) = receiver.recv().await {
                    let _ = reply.send(text);
                }
            });
            Ok(Box::new(TrackingToolSession {
                tools: Arc::new(RuntimeEcho { requests }),
                names: vec!["echo".into()],
                worker: tokio::sync::Mutex::new(Some(worker)),
                closed: self.closed.clone(),
                fail_shutdown: self.fail_shutdown,
            }))
        }
    }

    #[async_trait]
    impl crate::PreparedToolSession for TrackingToolSession {
        fn tools(&self) -> Arc<dyn ToolExecutor> {
            self.tools.clone()
        }

        fn ask_tool_names(&self) -> &[String] {
            &self.names
        }

        async fn shutdown(&self) -> Result<(), AgentError> {
            if let Some(worker) = self.worker.lock().await.take() {
                worker.abort();
                let _ = worker.await;
            }
            self.closed.store(true, Ordering::SeqCst);
            if self.fail_shutdown {
                Err(AgentError::Runtime("shutdown failed".into()))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn prepared_tools_stay_on_worker_runtime_and_require_confirmation() {
        let workspace = tempfile::tempdir().unwrap();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let preparation = Arc::new(TrackingPreparation {
            closed: closed.clone(),
            fail_prepare: false,
            fail_shutdown: false,
        });
        let runner = AgentRunner::new(model_with_tool_call(), Arc::new(EchoTool))
            .with_tool_preparation(preparation);
        let handle = runner.start(request(workspace.path().to_path_buf()));
        let permission_id = wait_for_permission(&handle);
        assert!(!closed.load(Ordering::SeqCst));
        assert!(handle.resolve_permission(&permission_id, PermissionChoice::Allow));
        let events = receive_until_terminal(&handle);
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(closed.load(Ordering::SeqCst));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { output, is_error: false, .. } if output == "hello"
        )));
    }

    #[test]
    fn preparation_failure_prevents_the_first_model_request() {
        let workspace = tempfile::tempdir().unwrap();
        let model = model_with_tool_call();
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool)).with_tool_preparation(
            Arc::new(TrackingPreparation {
                closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                fail_prepare: true,
                fail_shutdown: false,
            }),
        );
        let events = receive_until_terminal(&runner.start(request(workspace.path().to_path_buf())));
        assert!(matches!(
            &events.last().unwrap().kind,
            AgentEventKind::TurnFailed { error } if error.contains("discovery failed")
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. })));
        assert!(model.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn cancellation_closes_prepared_tools_before_terminal_event() {
        let workspace = tempfile::tempdir().unwrap();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runner = AgentRunner::new(model_with_tool_call(), Arc::new(EchoTool))
            .with_tool_preparation(Arc::new(TrackingPreparation {
                closed: closed.clone(),
                fail_prepare: false,
                fail_shutdown: false,
            }));
        let handle = runner.start(request(workspace.path().to_path_buf()));
        wait_for_permission(&handle);
        handle.cancel();
        let events = receive_until_terminal(&handle);
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCancelled
        ));
        assert!(closed.load(Ordering::SeqCst));
    }

    #[test]
    fn teardown_error_is_visible_as_turn_failure() {
        let workspace = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![ModelOutput {
                content: "Done".into(),
                tool_calls: vec![],
            }]),
            seen: Mutex::new(vec![]),
        });
        let runner = AgentRunner::new(model, Arc::new(EchoTool)).with_tool_preparation(Arc::new(
            TrackingPreparation {
                closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                fail_prepare: false,
                fail_shutdown: true,
            },
        ));
        let events = receive_until_terminal(&runner.start(request(workspace.path().to_path_buf())));
        assert!(matches!(
            &events.last().unwrap().kind,
            AgentEventKind::TurnFailed { error } if error.contains("shutdown failed")
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. })));
    }

    #[test]
    fn plan_mode_filters_and_rejects_mutating_tool_calls() {
        struct ToolAttemptModel {
            steps: AtomicUsize,
            seen_tools: Mutex<Vec<Vec<String>>>,
        }
        #[async_trait]
        impl ModelBackend for ToolAttemptModel {
            fn model_id(&self) -> &str {
                "tool-attempt"
            }

            async fn complete(
                &self,
                _messages: &[AgentMessage],
                tools: &[ToolDefinition],
                _cancel: CancellationToken,
            ) -> Result<ModelOutput, AgentError> {
                self.seen_tools.lock().unwrap().push(
                    tools
                        .iter()
                        .map(|definition| definition.name.clone())
                        .collect(),
                );
                if self.steps.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(ModelOutput {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "forbidden".into(),
                            name: "write_file".into(),
                            arguments: json!({"path":"x","content":"bad"}),
                        }],
                    })
                } else {
                    Ok(ModelOutput {
                        content: "Planned".into(),
                        tool_calls: vec![],
                    })
                }
            }
        }

        struct ReadAndWriteTools(AtomicUsize);
        #[async_trait]
        impl ToolExecutor for ReadAndWriteTools {
            fn certifies_parallel_read(&self, name: &str) -> bool {
                name == "read_file"
            }

            fn definitions(&self) -> Vec<ToolDefinition> {
                ["read_file", "write_file"]
                    .into_iter()
                    .map(|name| ToolDefinition {
                        name: name.into(),
                        description: name.into(),
                        parameters: json!({"type":"object"}),
                    })
                    .collect()
            }

            async fn execute(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, AgentError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::success("executed"))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ToolAttemptModel {
            steps: AtomicUsize::new(0),
            seen_tools: Mutex::new(Vec::new()),
        });
        let tools = Arc::new(ReadAndWriteTools(AtomicUsize::new(0)));
        let runner = AgentRunner::new(model.clone(), tools.clone());
        let mut run_request = request(dir.path().to_path_buf());
        run_request.mode = TurnMode::Plan;
        let handle = runner.start(run_request);
        let events = receive_until_terminal(&handle);
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { tool_call_id, is_error: true, output }
                if tool_call_id == "forbidden" && output.contains("not advertised")
        )));
        assert_eq!(tools.0.load(Ordering::SeqCst), 0);
        assert!(model
            .seen_tools
            .lock()
            .unwrap()
            .iter()
            .all(|names| names.len() == 1 && names[0] == "read_file"));
    }

    #[test]
    fn plan_mode_rejects_update_plan_and_commits_proposal() {
        let workspace = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "forbidden-plan".into(),
                        name: UPDATE_PLAN_TOOL.into(),
                        arguments: json!({"plan": [{"step": "Write code", "status": "in_progress"}]}),
                    }],
                },
                ModelOutput {
                    content: "1. Inspect files\n2. Write code".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(Vec::new()),
        });
        let mut run_request = request(workspace.path().to_path_buf());
        run_request.mode = TurnMode::Plan;
        let events =
            receive_until_terminal(&AgentRunner::new(model, Arc::new(EchoTool)).start(run_request));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { tool_call_id, is_error: true, output }
                if tool_call_id == "forbidden-plan" && output.contains("not advertised")
        )));
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::PlanUpdated { .. })));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::PlanProposed { plan } if plan.plan.len() == 2
        )));
    }

    #[test]
    fn build_handoff_commits_structured_plan_progress() {
        let workspace = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
        let session = store.create(workspace.path(), "native").unwrap();
        let plan_admission = store
            .admit_native_turn(
                &session.id,
                "plan-turn",
                "Plan this",
                "scripted",
                TurnMode::Plan,
            )
            .unwrap();
        let mut plan_request = request(workspace.path().to_path_buf());
        plan_request.session_id = Some(session.id.clone());
        plan_request.sequence_start = plan_admission.sequence;
        plan_request.prompt = "Plan this".into();
        plan_request.mode = TurnMode::Plan;
        let plan_model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![ModelOutput {
                content: "1. Inspect files\n2. Implement change".into(),
                tool_calls: vec![],
            }]),
            seen: Mutex::new(Vec::new()),
        });
        let plan_lease = store.try_acquire_native_lease(&session.id).unwrap();
        let plan_events = receive_until_terminal(
            &AgentRunner::new(plan_model, Arc::new(EchoTool))
                .with_native_runtime_identity("test", "https://test.example/v1")
                .with_event_journal(store.clone())
                .start_with_lease(plan_request, plan_lease),
        );
        assert!(matches!(
            plan_events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        let saved = store.load(&session.id).unwrap().unwrap();
        let replay = replay_native(
            &session.id,
            &saved.events,
            None,
            &CheckpointCompatibility {
                provider: "test".into(),
                model: "scripted".into(),
                context_format_key: "test".into(),
            },
        )
        .unwrap();
        let proposed = replay.latest_plan.clone().unwrap();
        let updated = PlanSnapshot {
            explanation: Some("Inspection complete".into()),
            plan: vec![
                openide_core::PlanStep {
                    step: "Inspect files".into(),
                    status: openide_core::PlanStepStatus::Completed,
                },
                openide_core::PlanStep {
                    step: "Implement change".into(),
                    status: openide_core::PlanStepStatus::InProgress,
                },
            ],
        };
        let build_admission = store
            .admit_native_turn(
                &session.id,
                "build-turn",
                "Build this",
                "scripted",
                TurnMode::Build,
            )
            .unwrap();
        let mut build_request = request(workspace.path().to_path_buf());
        build_request.session_id = Some(session.id.clone());
        build_request.sequence_start = build_admission.sequence;
        build_request.prompt = "Build this".into();
        build_request.history = replay.history;
        let build_model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "update-1".into(),
                        name: UPDATE_PLAN_TOOL.into(),
                        arguments: serde_json::to_value(&updated).unwrap(),
                    }],
                },
                ModelOutput {
                    content: "Done".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(Vec::new()),
        });
        let build_lease = store.try_acquire_native_lease(&session.id).unwrap();
        let build_events = receive_until_terminal(
            &AgentRunner::new(build_model.clone(), Arc::new(EchoTool))
                .with_plan_snapshot(Some(proposed.clone()))
                .with_native_runtime_identity("test", "https://test.example/v1")
                .with_event_journal(store.clone())
                .start_with_lease(build_request, build_lease),
        );
        assert!(matches!(
            build_events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(build_events.iter().any(|event| matches!(&event.kind,
            AgentEventKind::PlanUpdated { tool_call_id, plan }
                if tool_call_id == "update-1" && plan == &updated
        )));
        let seen = build_model.seen.lock().unwrap();
        assert!(seen[0][0]
            .content
            .contains(&serde_json::to_string(&proposed).unwrap()));
        drop(seen);
        let saved = store.load(&session.id).unwrap().unwrap();
        let replay = replay_native(
            &session.id,
            &saved.events,
            None,
            &CheckpointCompatibility {
                provider: "test".into(),
                model: "scripted".into(),
                context_format_key: "test".into(),
            },
        )
        .unwrap();
        assert_eq!(replay.latest_plan, Some(updated));
    }

    #[test]
    fn plan_mode_does_not_advertise_uncertified_tool_named_read_file() {
        struct CatalogModel(Mutex<Vec<String>>);
        #[async_trait]
        impl ModelBackend for CatalogModel {
            fn model_id(&self) -> &str {
                "catalog-model"
            }

            async fn complete(
                &self,
                _messages: &[AgentMessage],
                tools: &[ToolDefinition],
                _cancel: CancellationToken,
            ) -> Result<ModelOutput, AgentError> {
                *self.0.lock().unwrap() = tools.iter().map(|tool| tool.name.clone()).collect();
                Ok(ModelOutput {
                    content: "Plan ready".into(),
                    tool_calls: vec![],
                })
            }
        }
        struct UncertifiedRead;
        #[async_trait]
        impl ToolExecutor for UncertifiedRead {
            fn definitions(&self) -> Vec<ToolDefinition> {
                vec![ToolDefinition {
                    name: "read_file".into(),
                    description: "arbitrary implementation".into(),
                    parameters: json!({"type":"object"}),
                }]
            }

            async fn execute(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, AgentError> {
                panic!("uncertified read should not execute in Plan mode")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(CatalogModel(Mutex::new(Vec::new())));
        let runner = AgentRunner::new(model.clone(), Arc::new(UncertifiedRead));
        let mut run_request = request(dir.path().to_path_buf());
        run_request.mode = TurnMode::Plan;
        let events = receive_until_terminal(&runner.start(run_request));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(model.0.lock().unwrap().is_empty());
    }

    #[test]
    fn plan_mode_omits_certified_read_when_it_has_a_command_hook() {
        struct CatalogModel(Mutex<Vec<String>>);
        #[async_trait]
        impl ModelBackend for CatalogModel {
            fn model_id(&self) -> &str {
                "catalog-model"
            }
            fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
                Some([7; 32])
            }
            async fn complete(
                &self,
                _messages: &[AgentMessage],
                tools: &[ToolDefinition],
                _cancel: CancellationToken,
            ) -> Result<ModelOutput, AgentError> {
                *self.0.lock().unwrap() = tools.iter().map(|tool| tool.name.clone()).collect();
                Ok(ModelOutput {
                    content: "Plan ready".into(),
                    tool_calls: vec![],
                })
            }
        }
        struct CertifiedRead;
        #[async_trait]
        impl ToolExecutor for CertifiedRead {
            fn certifies_parallel_read(&self, name: &str) -> bool {
                name == "read_file"
            }
            fn definitions(&self) -> Vec<ToolDefinition> {
                vec![ToolDefinition {
                    name: "read_file".into(),
                    description: "Read".into(),
                    parameters: json!({"type":"object"}),
                }]
            }
            async fn execute(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, AgentError> {
                panic!("hooked read is not advertised in Plan")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let session = store.create(dir.path(), "native").unwrap();
        let admitted = store
            .admit_native_turn(
                &session.id,
                "turn-plan",
                "Plan",
                "catalog-model",
                TurnMode::Plan,
            )
            .unwrap();
        let request = AgentRunRequest {
            session_id: Some(session.id.clone()),
            sequence_start: admitted.sequence,
            workspace: dir.path().into(),
            history: vec![],
            prompt: "Plan".into(),
            mode: TurnMode::Plan,
        };
        let lease = store.try_acquire_native_lease(&session.id).unwrap();
        #[cfg(windows)]
        let script = "[Console]::Out.Write('{\"decision\":\"allow\"}')";
        #[cfg(unix)]
        let script = "printf '%s' '{\"decision\":\"allow\"}'";
        let mut config = command_hook_config(dir.path(), HookPhase::PreToolUse, script, 5_000);
        config.tool_name = "read_file".into();
        let hooks = Arc::new(CommandHooks::new(vec![config]).unwrap());
        let model = Arc::new(CatalogModel(Mutex::new(Vec::new())));
        let runner = AgentRunner::new(model.clone(), Arc::new(CertifiedRead))
            .with_command_hooks(hooks)
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store);
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(model.0.lock().unwrap().is_empty());
    }

    #[test]
    fn active_skills_enter_system_context_without_rewriting_user_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![ModelOutput {
                content: "Done".into(),
                tool_calls: vec![],
            }]),
            seen: Mutex::new(vec![]),
        });
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool))
            .with_system_instructions("Skill: inspect dependency changes before editing");
        let events = receive_until_terminal(&runner.start(request(dir.path().to_path_buf())));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::TurnStarted { prompt } if prompt == "Say hello"
        )));
        let seen = model.seen.lock().unwrap();
        let messages = &seen[0];
        assert_eq!(messages[0].role, MessageRole::System);
        assert!(messages[0]
            .content
            .contains("Skill: inspect dependency changes"));
        assert_eq!(messages.last().unwrap().role, MessageRole::User);
        assert_eq!(messages.last().unwrap().content, "Say hello");
    }

    #[test]
    fn runs_model_tool_model_cycle_and_preserves_history() {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "echo".into(),
                        arguments: json!({"text":"hello"}),
                    }],
                },
                ModelOutput {
                    content: "Hello!".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        });
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool));
        let handle = runner.start(request(dir.path().to_path_buf()));
        let events: Vec<_> = handle.events.into_iter().collect();
        assert_eq!(events.first().unwrap().sequence, 8);
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { steps: 2, .. }
        ));
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].last().unwrap().role, MessageRole::Tool);
        assert_eq!(seen[1].last().unwrap().content, "hello");
    }

    #[test]
    fn compacts_tool_result_for_next_model_request_but_preserves_event() {
        struct LargeTool;

        #[async_trait]
        impl ToolExecutor for LargeTool {
            fn definitions(&self) -> Vec<ToolDefinition> {
                vec![ToolDefinition {
                    name: "large_output".into(),
                    description: "Produce a large result".into(),
                    parameters: json!({"type":"object"}),
                }]
            }

            async fn execute(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, AgentError> {
                Ok(ToolOutput::success("x".repeat(4_000)))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "large-call".into(),
                        name: "large_output".into(),
                        arguments: json!({}),
                    }],
                },
                ModelOutput {
                    content: "Done".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        });
        let runner = AgentRunner::new(model.clone(), Arc::new(LargeTool)).with_context_policy(
            ContextPolicy {
                max_request_chars: 1_500,
                keep_recent_tool_results: 0,
            },
        );
        let events: Vec<_> = runner
            .start(request(dir.path().to_path_buf()))
            .events
            .into_iter()
            .collect();
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { output, .. } if output.len() == 4_000
        )));
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1][1].content, "Say hello");
        assert_eq!(seen[1][2].tool_calls[0].id, "large-call");
        assert_eq!(seen[1][3].tool_call_id.as_deref(), Some("large-call"));
        assert_eq!(
            seen[1][3].content,
            "[Earlier tool result omitted from model context]"
        );
    }

    #[test]
    fn reports_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        struct WaitModel;
        #[async_trait]
        impl ModelBackend for WaitModel {
            fn model_id(&self) -> &str {
                "wait"
            }

            async fn complete(
                &self,
                _messages: &[AgentMessage],
                _tools: &[ToolDefinition],
                cancel: CancellationToken,
            ) -> Result<ModelOutput, AgentError> {
                cancel.cancelled().await;
                Err(AgentError::Cancelled)
            }
        }
        let runner = AgentRunner::new(Arc::new(WaitModel), Arc::new(EchoTool));
        let handle = runner.start(request(dir.path().to_path_buf()));
        handle.cancel();
        let events: Vec<_> = handle.events.into_iter().collect();
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCancelled
        ));
    }

    #[test]
    fn approval_is_bound_to_the_pending_request_before_tool_execution() {
        let dir = tempfile::tempdir().unwrap();
        let model = model_with_tool_call();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model.clone(), tool.clone())
            .with_permission_policy(PermissionPolicy::new().with_tool("echo", PermissionMode::Ask));
        let handle = runner.start(request(dir.path().to_path_buf()));
        let id = wait_for_permission(&handle);
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
        assert!(!handle.resolve_permission("wrong-id", PermissionChoice::Allow));
        assert!(handle.resolve_permission(&id, PermissionChoice::Allow));
        assert!(!handle.resolve_permission(&id, PermissionChoice::Allow));
        let events = receive_until_terminal(&handle);
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::PermissionResolved { request_id, tool_call_id, resolution: PermissionResolution::Allowed }
                if request_id == &id && tool_call_id == "call_permission"
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { is_error: false, output, .. } if output == "hello"
        )));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn session_approval_survives_store_reopen_and_only_matches_exact_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("sessions");
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let policy = PermissionPolicy::new().with_tool("echo", PermissionMode::Ask);
        let session_id = {
            let store = Arc::new(SessionStore::open(&store_path).unwrap());
            let request = journaled_request(&store, dir.path());
            let session_id = request.session_id.clone().unwrap();
            let lease = store.try_acquire_native_lease(&session_id).unwrap();
            let runner = AgentRunner::new(model_with_tool_call(), tool.clone())
                .with_permission_policy(policy.clone())
                .with_native_runtime_identity("test", "https://test.example/v1")
                .with_event_journal(store.clone())
                .with_session_approvals(store.clone(), CommandIsolation::HostUnrestricted);
            let handle = runner.start_with_lease(request, lease);
            let request_id = wait_for_permission(&handle);
            assert!(handle.resolve_permission(&request_id, PermissionChoice::AllowSession));
            assert!(matches!(
                receive_until_terminal(&handle).last().unwrap().kind,
                AgentEventKind::TurnCompleted { .. }
            ));
            let decisions = store.load_permission_decisions(&session_id).unwrap();
            assert_eq!(decisions.len(), 1);
            assert_eq!(decisions[0].request_id, request_id);
            assert_eq!(decisions[0].decision, PermissionDecision::Allow);
            assert!(decisions[0].grant.is_some());
            assert_eq!(tool.executions.load(Ordering::SeqCst), 1);
            session_id
        };

        let store = Arc::new(SessionStore::open(&store_path).unwrap());
        assert_eq!(store.session_approval_grants(&session_id).unwrap().len(), 1);
        let admitted = store
            .admit_native_turn(&session_id, "turn-2", "Again", "scripted", TurnMode::Build)
            .unwrap();
        let mut turn_two_request = request(dir.path().to_path_buf());
        turn_two_request.session_id = Some(session_id.clone());
        turn_two_request.sequence_start = admitted.sequence;
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone())
            .with_permission_policy(policy.clone())
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone())
            .with_session_approvals(store.clone(), CommandIsolation::HostUnrestricted);
        let events = receive_until_terminal(&runner.start_with_lease(turn_two_request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::PermissionRequested { .. })));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 2);

        let grant = store
            .session_approval_grants(&session_id)
            .unwrap()
            .remove(0);
        assert!(store
            .revoke_session_approval_grant(&session_id, &grant.key_digest)
            .unwrap());
        let admitted = store
            .admit_native_turn(&session_id, "turn-3", "Again", "scripted", TurnMode::Build)
            .unwrap();
        let mut revoked_request = request(dir.path().to_path_buf());
        revoked_request.session_id = Some(session_id.clone());
        revoked_request.sequence_start = admitted.sequence;
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone())
            .with_permission_policy(policy.clone())
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone())
            .with_session_approvals(store.clone(), CommandIsolation::HostUnrestricted);
        let handle = runner.start_with_lease(revoked_request, lease);
        let request_id = wait_for_permission(&handle);
        assert!(handle.resolve_permission(&request_id, PermissionChoice::Deny));
        receive_until_terminal(&handle);
        assert_eq!(tool.executions.load(Ordering::SeqCst), 2);

        let admitted = store
            .admit_native_turn(&session_id, "turn-4", "Again", "scripted", TurnMode::Build)
            .unwrap();
        let mut turn_four_request = request(dir.path().to_path_buf());
        turn_four_request.session_id = Some(session_id.clone());
        turn_four_request.sequence_start = admitted.sequence;
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone())
            .with_permission_policy(policy)
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone())
            .with_session_approvals(store, CommandIsolation::WorkspaceWriteOffline);
        let handle = runner.start_with_lease(turn_four_request, lease);
        let request_id = wait_for_permission(&handle);
        assert!(handle.resolve_permission(&request_id, PermissionChoice::Deny));
        receive_until_terminal(&handle);
        assert_eq!(tool.executions.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn session_approval_fails_closed_without_a_leased_store_journal() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone())
            .with_permission_policy(PermissionPolicy::new().with_tool("echo", PermissionMode::Ask));
        let handle = runner.start(request(dir.path().to_path_buf()));
        let request_id = wait_for_permission(&handle);
        assert!(handle.resolve_permission(&request_id, PermissionChoice::AllowSession));
        let events = receive_until_terminal(&handle);
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnFailed { .. }
        ));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn queued_steers_before_and_during_model_are_answered_once() {
        struct SteeringModel {
            store: Arc<SessionStore>,
            session_id: String,
            calls: AtomicUsize,
            seen: Mutex<Vec<Vec<AgentMessage>>>,
        }

        #[async_trait]
        impl ModelBackend for SteeringModel {
            fn model_id(&self) -> &str {
                "scripted"
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
                let call = self.calls.fetch_add(1, Ordering::SeqCst);
                self.seen.lock().unwrap().push(messages.to_vec());
                if call == 0 {
                    self.store
                        .enqueue_native_steer(
                            &self.session_id,
                            "turn-1",
                            "during-model",
                            "Please include the second point",
                        )
                        .unwrap();
                }
                Ok(ModelOutput {
                    content: if call == 0 { "Initial" } else { "Updated" }.into(),
                    tool_calls: vec![],
                })
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        store
            .enqueue_native_steer(&session_id, "turn-1", "before-model", "First point")
            .unwrap();
        let model = Arc::new(SteeringModel {
            store: store.clone(),
            session_id: session_id.clone(),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        });
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool))
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone())
            .with_native_steering(store.clone());
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            &events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { response, steps: 2 } if response == "Updated"
        ));
        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
        let seen = model.seen.lock().unwrap();
        assert!(seen[0]
            .iter()
            .any(|message| message.content == "First point"));
        assert!(!seen[0]
            .iter()
            .any(|message| message.content == "Please include the second point"));
        assert_eq!(
            seen[1]
                .iter()
                .filter(|message| message.content == "First point")
                .count(),
            1
        );
        assert_eq!(
            seen[1]
                .iter()
                .filter(|message| message.content == "Please include the second point")
                .count(),
            1
        );
        let first_request = events
            .iter()
            .position(|event| {
                matches!(
                    event.kind,
                    AgentEventKind::ModelRequestStarted { step: 1, .. }
                )
            })
            .unwrap();
        let first_steer = events
            .iter()
            .position(|event| {
                matches!(
                    &event.kind,
                    AgentEventKind::TurnSteered { input_id, .. } if input_id == "before-model"
                )
            })
            .unwrap();
        let second_steer = events
            .iter()
            .position(|event| {
                matches!(
                    &event.kind,
                    AgentEventKind::TurnSteered { input_id, .. } if input_id == "during-model"
                )
            })
            .unwrap();
        let second_request = events
            .iter()
            .position(|event| {
                matches!(
                    event.kind,
                    AgentEventKind::ModelRequestStarted { step: 2, .. }
                )
            })
            .unwrap();
        assert!(first_steer < first_request && first_request < second_steer);
        assert!(second_steer < second_request);
        assert!(store.pending_native_steers(&session_id, "turn-1").is_err());
    }

    #[test]
    fn admitted_and_steered_images_reach_the_model_from_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let session = store.create(dir.path(), "native").unwrap();
        let image =
            UserImage::from_bytes(openide_core::UserImageMime::Png, b"\x89PNG\r\n\x1a\nimage")
                .unwrap();
        let admitted = store
            .admit_native_turn_with_images(
                &session.id,
                "turn-image",
                "Say hello",
                "scripted",
                TurnMode::Build,
                std::slice::from_ref(&image),
            )
            .unwrap();
        store
            .enqueue_native_steer_with_images(
                &session.id,
                "turn-image",
                "image-steer",
                "Look at this too",
                std::slice::from_ref(&image),
            )
            .unwrap();
        let mut request = request(dir.path().to_path_buf());
        request.session_id = Some(session.id.clone());
        request.sequence_start = admitted.sequence;
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![ModelOutput {
                content: "Seen".into(),
                tool_calls: vec![],
            }]),
            seen: Mutex::new(Vec::new()),
        });
        let runner = AgentRunner::new(model.clone(), Arc::new(EchoTool))
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone())
            .with_native_steering(store.clone());
        let lease = store.try_acquire_native_lease(&session.id).unwrap();
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        let seen = model.seen.lock().unwrap();
        let users = seen[0]
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .collect::<Vec<_>>();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].images, vec![image.clone()]);
        assert_eq!(users[1].images, vec![image]);
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::TurnSteeredWithImages { .. })));
    }

    #[test]
    fn completion_fence_rejects_a_steer_during_prepared_tool_shutdown() {
        struct LateSteerPreparation {
            store: Arc<SessionStore>,
            session_id: String,
            late_error: Arc<Mutex<Option<StoreError>>>,
        }

        struct LateSteerSession {
            store: Arc<SessionStore>,
            session_id: String,
            late_error: Arc<Mutex<Option<StoreError>>>,
        }

        #[async_trait]
        impl ToolPreparation for LateSteerPreparation {
            async fn prepare(
                &self,
                _workspace: &Path,
                _base_tools: Arc<dyn ToolExecutor>,
                _cancel: &CancellationToken,
            ) -> Result<Box<dyn crate::PreparedToolSession>, AgentError> {
                Ok(Box::new(LateSteerSession {
                    store: self.store.clone(),
                    session_id: self.session_id.clone(),
                    late_error: self.late_error.clone(),
                }))
            }
        }

        #[async_trait]
        impl crate::PreparedToolSession for LateSteerSession {
            fn tools(&self) -> Arc<dyn ToolExecutor> {
                Arc::new(EchoTool)
            }

            fn ask_tool_names(&self) -> &[String] {
                &[]
            }

            async fn shutdown(&self) -> Result<(), AgentError> {
                let result = self.store.enqueue_native_steer(
                    &self.session_id,
                    "turn-1",
                    "too-late",
                    "A later request",
                );
                *self.late_error.lock().unwrap() = result.err();
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let late_error = Arc::new(Mutex::new(None));
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![ModelOutput {
                content: "Complete".into(),
                tool_calls: vec![],
            }]),
            seen: Mutex::new(Vec::new()),
        });
        let runner = AgentRunner::new(model, Arc::new(EchoTool))
            .with_tool_preparation(Arc::new(LateSteerPreparation {
                store: store.clone(),
                session_id: session_id.clone(),
                late_error: late_error.clone(),
            }))
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone())
            .with_native_steering(store.clone());
        let lease = store.try_acquire_native_lease(&session_id).unwrap();
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(matches!(
            late_error.lock().unwrap().as_ref(),
            Some(StoreError::NativeTurnFinishing(_))
        ));
        assert!(store
            .native_turn_finish_fence(&session_id, "turn-1")
            .unwrap());
    }

    #[test]
    fn steer_queued_during_tool_effect_reaches_the_next_model_request() {
        struct SteeringTool {
            store: Arc<SessionStore>,
            session_id: String,
            calls: AtomicUsize,
        }

        #[async_trait]
        impl ToolExecutor for SteeringTool {
            fn definitions(&self) -> Vec<ToolDefinition> {
                EchoTool.definitions()
            }

            async fn execute(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, AgentError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.store
                    .enqueue_native_steer(
                        &self.session_id,
                        "turn-1",
                        "during-tool",
                        "Also address the next point",
                    )
                    .unwrap();
                Ok(ToolOutput::success("hello"))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&store, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let model = model_with_tool_call();
        let tools = Arc::new(SteeringTool {
            store: store.clone(),
            session_id: session_id.clone(),
            calls: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model.clone(), tools.clone())
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(store.clone())
            .with_native_steering(store);
        let lease = tools.store.try_acquire_native_lease(&session_id).unwrap();
        let handle = runner.start_with_lease(request, lease);
        let events = handle.events.iter().collect::<Vec<_>>();
        assert!(
            matches!(
                events.last().unwrap().kind,
                AgentEventKind::TurnCompleted { .. }
            ),
            "journal error: {:?}; events: {events:?}",
            handle.journal_error()
        );
        assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[1]
                .iter()
                .filter(|message| message.content == "Also address the next point")
                .count(),
            1
        );
        let tool_finished = events
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. }))
            .unwrap();
        let steered = events
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::TurnSteered { .. }))
            .unwrap();
        let second_model = events
            .iter()
            .position(|event| {
                matches!(
                    event.kind,
                    AgentEventKind::ModelRequestStarted { step: 2, .. }
                )
            })
            .unwrap();
        assert!(tool_finished < steered && steered < second_model);
    }

    #[test]
    fn denial_returns_a_tool_error_without_executing_it() {
        let dir = tempfile::tempdir().unwrap();
        let model = model_with_tool_call();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model.clone(), tool.clone())
            .with_permission_policy(PermissionPolicy::new().with_tool("echo", PermissionMode::Ask));
        let handle = runner.start(request(dir.path().to_path_buf()));
        let id = wait_for_permission(&handle);
        assert!(handle.resolve_permission(&id, PermissionChoice::Deny));
        let events = receive_until_terminal(&handle);
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::PermissionResolved {
                resolution: PermissionResolution::Denied,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { is_error: true, output, .. }
                if output.contains("Permission denied")
        )));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
        assert!(model.seen.lock().unwrap()[1]
            .last()
            .unwrap()
            .content
            .contains("Permission denied"));
    }

    #[test]
    fn deny_policy_blocks_a_tool_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone()).with_permission_policy(
            PermissionPolicy::new().with_tool("echo", PermissionMode::Deny),
        );
        let handle = runner.start(request(dir.path().to_path_buf()));
        let events = receive_until_terminal(&handle);
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::PermissionRequested { .. })));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ToolFinished { is_error: true, output, .. }
                if output.contains("Permission denied")
        )));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn argv_deny_rule_blocks_real_tool_call_and_commits_policy_before_model() {
        struct CommandTool(AtomicUsize);
        #[async_trait]
        impl ToolExecutor for CommandTool {
            fn definitions(&self) -> Vec<ToolDefinition> {
                vec![ToolDefinition {
                    name: "run_command".into(),
                    description: "Run a command".into(),
                    parameters: json!({"type":"object"}),
                }]
            }

            async fn execute(
                &self,
                _call: &ToolCall,
                _context: &ToolContext,
            ) -> Result<ToolOutput, AgentError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::success("ran"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let request = journaled_request(&sessions, dir.path());
        let session_id = request.session_id.clone().unwrap();
        let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
        let model = Arc::new(ScriptedModel {
            outputs: Mutex::new(vec![
                ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "command-1".into(),
                        name: "run_command".into(),
                        arguments: json!({"command":"git", "args":["push", "origin", "main"]}),
                    }],
                },
                ModelOutput {
                    content: "Done".into(),
                    tool_calls: vec![],
                },
            ]),
            seen: Mutex::new(vec![]),
        });
        let tools = Arc::new(CommandTool(AtomicUsize::new(0)));
        let rules = crate::ExecPolicy::new(vec![crate::ExecRule::new(
            vec![
                crate::ExecPatternToken::Literal("git".into()),
                crate::ExecPatternToken::Literal("push".into()),
            ],
            crate::ExecDecision::Deny,
            None,
        )
        .unwrap()])
        .unwrap();
        let policy = PermissionPolicy::new().with_exec_policy(rules);
        let digest = policy.digest_hex();
        let runner = AgentRunner::new(model, tools.clone())
            .with_permission_policy(policy)
            .with_native_runtime_identity("test", "https://test.example/v1")
            .with_event_journal(sessions.clone());
        let events = receive_until_terminal(&runner.start_with_lease(request, lease));
        assert_eq!(tools.0.load(Ordering::SeqCst), 0);
        let committed = events.iter().position(|event| matches!(
            &event.kind,
            AgentEventKind::ExecPolicyCommitted { version, digest: saved }
                if *version == openide_core::EXEC_POLICY_IDENTITY_VERSION && saved == &digest
        )).unwrap();
        let model_started = events
            .iter()
            .position(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. }))
            .unwrap();
        assert!(committed < model_started);
        assert!(events.iter().any(|event| matches!(
            &event.kind, AgentEventKind::ToolFinished { is_error: true, output, .. }
                if output.contains("Permission denied")
        )));
    }

    #[test]
    fn unhandled_permission_times_out_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone()).with_permission_policy(
            PermissionPolicy::new()
                .with_tool("echo", PermissionMode::Ask)
                .with_timeout(Duration::from_millis(20)),
        );
        let handle = runner.start(request(dir.path().to_path_buf()));
        let id = wait_for_permission(&handle);
        let events = receive_until_terminal(&handle);
        assert!(!handle.resolve_permission(&id, PermissionChoice::Allow));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::PermissionResolved {
                resolution: PermissionResolution::TimedOut,
                ..
            }
        )));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancellation_while_waiting_for_permission_stops_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let tool = Arc::new(CountingTool {
            executions: AtomicUsize::new(0),
        });
        let runner = AgentRunner::new(model_with_tool_call(), tool.clone())
            .with_permission_policy(PermissionPolicy::new().with_tool("echo", PermissionMode::Ask));
        let handle = runner.start(request(dir.path().to_path_buf()));
        let id = wait_for_permission(&handle);
        handle.cancel();
        let events = receive_until_terminal(&handle);
        assert!(!handle.resolve_permission(&id, PermissionChoice::Allow));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::PermissionResolved {
                resolution: PermissionResolution::Cancelled,
                ..
            }
        )));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCancelled
        ));
        assert_eq!(tool.executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn truncates_utf8_by_characters() {
        assert_eq!(truncate_chars("áéí", 2), "áé\n[tool output truncated]");
    }
}
