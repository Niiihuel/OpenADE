//! UI-facing recovery worker. The native lease and prepared MCP clients stay
//! inside one Tokio thread until recovery finishes or is explicitly stopped.

use crate::native_automation::FrozenNativeAutomationAuthorityV1;
use async_trait::async_trait;
use openide_core::{
    replay_native, AgentEvent, CheckpointCompatibility, CheckpointFileStore, NativeSessionLease,
    PermissionDecision, ReplayRecovery, SessionStore, ToolCall,
};
use openide_harness::{
    update_plan_tool_definition, AgentError, CommandHooks, CommandIsolation, ModelBackend,
    NativeCommandProfile, PermissionChoice, PermissionPolicy, PersistentCommandManager,
    PreparedToolSession, ResumeDisposition, ResumeEngine, ResumeEventObserver, RunLimits,
    SubagentCoordinator, SubagentEventBridge, SubagentLimits, SubagentToolExecutor,
    SubagentToolLimits, SubagentWorker, TokenBudgetPolicy, ToolCatalog, ToolContext,
    ToolDefinition, ToolExecutor, ToolOutput, ToolPreparation,
};
use std::sync::{
    mpsc::{self, Receiver, SyncSender as Sender},
    Arc, Mutex,
};
use tokio_util::sync::CancellationToken;

pub(crate) struct ResumeWorkerRequest {
    pub session_id: String,
    /// Reconstructed from this session's immutable origin, never Settings.
    pub native_automation: Option<FrozenNativeAutomationAuthorityV1>,
    pub provider: String,
    pub endpoint: String,
    pub sessions: Arc<SessionStore>,
    pub checkpoints: Arc<CheckpointFileStore>,
    pub compatibility: CheckpointCompatibility,
    pub model: Arc<dyn ModelBackend>,
    pub base_tools: Arc<dyn ToolExecutor>,
    pub child_tools: Arc<dyn ToolExecutor>,
    pub child_command_profile: Option<NativeCommandProfile>,
    pub event_bridge: Option<Arc<SubagentEventBridge>>,
    pub tool_preparation: Option<Arc<dyn ToolPreparation>>,
    pub permission_policy: PermissionPolicy,
    pub command_isolation: CommandIsolation,
    pub command_hooks: Arc<CommandHooks>,
    pub budget_policy: Option<TokenBudgetPolicy>,
    /// What a child of this turn is identified by, when it is not the
    /// budget it runs with. None: the running budget identifies.
    pub child_identity_budget: Option<Option<TokenBudgetPolicy>>,
    pub system_instructions: String,
    /// Only for an admission that has not committed context yet. The native
    /// lease holder selects the authoritative ledger over legacy composer text.
    pub pre_context_instructions: Option<(String, Option<String>)>,
    pub limits: RunLimits,
}

#[derive(Clone, Debug)]
pub(crate) enum GoalReceiptInspection {
    Found(openide_core::NativeGoalCommitReceipt),
    NotFound,
    Failed(String),
}

impl GoalReceiptInspection {
    pub(crate) fn description(&self) -> String {
        match self {
            Self::Found(receipt) => format!(
                "Modificación guardada. Goal: {}\nOperación: {}\nRevisión guardada: {}\nRevisión actual: {}\nEl recibo no reconstruye el resultado original ni verifica semánticamente el criterio. La llamada no se repetirá; comprueba el estado antes de continuar.",
                receipt.goal_id, receipt.operation_id, receipt.committed_revision, receipt.current_revision,
            ),
            Self::NotFound => "No se encontró un recibo exacto para estos argumentos. Esto no autoriza repetir la llamada.".into(),
            Self::Failed(error) => format!("No se pudo comprobar el recibo del goal: {error}. Esto no autoriza repetir la llamada."),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ResumeUpdate {
    Events(Vec<AgentEvent>),
    NeedsPermission {
        request_id: String,
        tool_call: ToolCall,
    },
    /// An external effect may already have happened. The worker waits for an
    /// independently verified result and never repeats this tool itself.
    NeedsVerification {
        tool_call: ToolCall,
        executor_error: Option<String>,
        goal_receipt: Option<GoalReceiptInspection>,
    },
    /// Sent after all earlier Events updates and after prepared tools close.
    Finished(Result<ResumeDisposition, String>),
}

enum ResumeCommand {
    ResolvePermission {
        request_id: String,
        choice: PermissionChoice,
    },
    RecordVerifiedToolOutcome {
        tool_call_id: String,
        result: ToolOutput,
    },
}

pub(crate) struct ResumeWorkerHandle {
    pub session_id: String,
    pub updates: Receiver<ResumeUpdate>,
    command_tx: tokio::sync::mpsc::UnboundedSender<ResumeCommand>,
    cancel: CancellationToken,
}

impl ResumeWorkerHandle {
    /// Acquire the native session lease before starting a worker. A second
    /// recovery or normal run cannot be admitted while this handle is active.
    #[cfg(test)]
    pub fn start(request: ResumeWorkerRequest) -> Result<Self, String> {
        Self::start_with_command_actor(request, None)
    }

    pub fn start_with_command_actor(
        request: ResumeWorkerRequest,
        command_actor: Option<Arc<openide_harness::NativeManualCommandActor>>,
    ) -> Result<Self, String> {
        let lease = request
            .sessions
            .try_acquire_native_lease(&request.session_id)
            .map_err(|error| format!("No se pudo bloquear la sesión nativa: {error}"))?;
        let session_id = request.session_id.clone();
        // Match normal runs: retain FIFO and stop producing when the desktop
        // has a full frame of unread updates, rather than accumulating a tail.
        let (update_tx, updates) = mpsc::sync_channel(64);
        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("openide-native-resume".into())
            .spawn(move || {
                let outcome = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run_recovery_with_command_actor(
                        request,
                        lease,
                        command_rx,
                        &update_tx,
                        worker_cancel,
                        command_actor,
                    )),
                    Err(error) => {
                        drop(lease);
                        Err(format!("No se pudo iniciar Tokio: {error}"))
                    }
                };
                let _ = update_tx.send(ResumeUpdate::Finished(outcome));
            })
            .map_err(|error| format!("No se pudo iniciar la recuperación: {error}"))?;
        Ok(Self {
            session_id,
            updates,
            command_tx,
            cancel,
        })
    }

    /// Queuing succeeds only while the recovery worker is alive. Its exact
    /// durable request ID is checked again inside ResumeEngine.
    pub fn resolve_permission(&self, request_id: &str, choice: PermissionChoice) -> bool {
        self.command_tx
            .send(ResumeCommand::ResolvePermission {
                request_id: request_id.to_owned(),
                choice,
            })
            .is_ok()
    }

    /// Only call this after independently checking the external effect.
    pub fn record_verified_tool_outcome(&self, tool_call_id: &str, result: ToolOutput) -> bool {
        self.command_tx
            .send(ResumeCommand::RecordVerifiedToolOutcome {
                tool_call_id: tool_call_id.to_owned(),
                result,
            })
            .is_ok()
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl Drop for ResumeWorkerHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn run_recovery_with_command_actor(
    request: ResumeWorkerRequest,
    lease: NativeSessionLease,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<ResumeCommand>,
    updates: &Sender<ResumeUpdate>,
    cancel: CancellationToken,
    command_actor: Option<Arc<openide_harness::NativeManualCommandActor>>,
) -> Result<ResumeDisposition, String> {
    let lease = Arc::new(lease);
    let record = request
        .sessions
        .load(&request.session_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "La sesión nativa ya no existe".to_owned())?;
    let automation = FrozenNativeAutomationAuthorityV1::from_session(&record.session)?;
    if automation.as_ref() != request.native_automation.as_ref() {
        return Err(
            "El origen de la automatización no coincide con la autoridad de recuperación".into(),
        );
    }
    if let Some(frozen) = &automation {
        frozen.validate_workspace_for_execution()?;
        frozen.verify_model(request.model.as_ref())?;
        if command_actor.is_some()
            || request.tool_preparation.is_some()
            || request.event_bridge.is_some()
            || request.child_command_profile.is_some()
            || request.pre_context_instructions.is_some()
            || request.provider != frozen.provider_id()
            || request.endpoint != frozen.endpoint()
            || request.compatibility != frozen.compatibility()
            || request.budget_policy != frozen.budget()
            || request.system_instructions != frozen.instructions()
            || request.permission_policy.digest_hex() != frozen.policy().digest_hex()
            || request
                .command_hooks
                .digest_hex()
                .map_err(|error| error.to_string())?
                != CommandHooks::default()
                    .digest_hex()
                    .map_err(|error| error.to_string())?
            || ToolCatalog::from_executor(request.base_tools.as_ref())
                .map_err(|error| error.to_string())?
                .digest_hex()
                != ToolCatalog::from_executor(frozen.tools().as_ref())
                    .map_err(|error| error.to_string())?
                    .digest_hex()
        {
            return Err(
                "La recuperación de automatización exige su perfil original de sólo lectura".into(),
            );
        }
        let limits = frozen.limits();
        if request.limits.max_model_steps != limits.max_model_steps
            || request.limits.max_tool_calls != limits.max_tool_calls
            || request.limits.max_tool_output_chars != limits.max_tool_output_chars
        {
            return Err("Los límites de la automatización difieren del snapshot original".into());
        }
    }
    let event_forwarder = Arc::new(ResumeEventForwarder {
        session_id: request.session_id.clone(),
        updates: updates.clone(),
        last_reported_sequence: Mutex::new(record.events.last().map_or(0, |event| event.sequence)),
    });
    let replay = replay_native(
        &request.session_id,
        &record.events,
        Some(&request.checkpoints),
        &request.compatibility,
    )
    .map_err(|error| error.to_string())?;
    match &command_actor {
        Some(actor) => {
            if !Arc::ptr_eq(actor.sessions(), &request.sessions) {
                return Err("El actor de comandos pertenece a otro store".into());
            }
            actor
                .validate_lease(&lease)
                .map_err(|error| error.to_string())?;
        }
        None => openide_core::NativeCommandOperationStore::new(request.sessions.clone())
            .require_no_pending(&lease)
            .map_err(|error| error.to_string())?,
    }
    if replay.recovery == ReplayRecovery::Idle {
        if let Some(actor) = &command_actor {
            actor
                .process_pending(&lease, &cancel)
                .await
                .map_err(|error| error.to_string())?;
        }
        return Ok(ResumeDisposition::Complete);
    }
    if let Some(frozen) = &automation {
        if replay
            .active_system_instructions
            .as_deref()
            .is_some_and(|saved| saved != frozen.instructions())
        {
            return Err("Las instrucciones del turno difieren del origen de automatización".into());
        }
        let outcome = run_with_prepared(
            &request,
            frozen.instructions(),
            &lease,
            frozen.tools(),
            None,
            replay.active_tool_catalog_digest.as_deref(),
            None,
            &mut commands,
            updates,
            &cancel,
            event_forwarder.clone(),
            None,
        )
        .await;
        if outcome.is_err() {
            let _ =
                forward_unreported_events(&request.sessions, &request.session_id, &event_forwarder);
        }
        return outcome;
    }
    let mut system_instructions = replay
        .active_system_instructions
        .clone()
        .unwrap_or_else(|| request.system_instructions.clone());
    if replay.active_system_instructions.is_none()
        && !record
            .events
            .iter()
            .rev()
            .take_while(|e| {
                !matches!(
                    e.kind,
                    openide_core::AgentEventKind::TurnAdmitted { .. }
                        | openide_core::AgentEventKind::TurnAdmittedWithImages { .. }
                )
            })
            .any(|e| {
                matches!(
                    e.kind,
                    openide_core::AgentEventKind::ModelContextCommitted { .. }
                )
            })
    {
        let goals = openide_core::NativeGoalStore::new(request.sessions.clone())
            .list(&request.session_id)
            .map_err(|e| e.to_string())?;
        system_instructions = match &request.pre_context_instructions {
            Some((project, legacy)) => {
                crate::app::native_task_instructions(
                    project.clone(),
                    legacy.as_deref(),
                    goals.last(),
                )?
                .1
            }
            None => {
                openide_harness::native_goal_system_instructions(&system_instructions, goals.last())
                    .map_err(|e| e.to_string())?
            }
        };
    }
    let turn = replay
        .active_turn
        .as_ref()
        .ok_or_else(|| "La sesión no tiene un turno recuperable".to_owned())?;
    if turn
        .model
        .as_deref()
        .is_some_and(|model| model != request.model.model_id())
        || request.compatibility.model != request.model.model_id()
    {
        return Err("El modelo configurado difiere del turno interrumpido".into());
    }
    let workspace = record
        .session
        .workspace
        .canonicalize()
        .map_err(|error| format!("Workspace inaccesible: {error}"))?;
    if !workspace.is_dir() {
        return Err("El workspace ya no es un directorio".into());
    }

    let coordinator = Arc::new(
        SubagentCoordinator::new(request.sessions.clone(), SubagentLimits::default()).map_err(
            |error| format!("No se pudo iniciar la coordinación de subagentes: {error}"),
        )?,
    );
    let mut child_worker = SubagentWorker::new(
        coordinator.clone(),
        request.sessions.clone(),
        request.model.clone(),
        request.child_tools.clone(),
        request.provider.clone(),
        request.endpoint.clone(),
    )
    .with_permission_policy(request.permission_policy.clone())
    .with_session_approvals(request.command_isolation)
    .with_command_hooks(request.command_hooks.clone())
    .with_nested_subagents(SubagentToolLimits::default())
    .with_native_goals()
    .with_system_instructions(
        openide_harness::native_goal_base_instructions(&system_instructions)
            .unwrap_or_else(|| system_instructions.clone()),
    )
    .with_limits(request.limits)
    .with_auto_compaction(request.checkpoints.clone(), request.compatibility.clone());
    if let Some(profile) = &request.child_command_profile {
        child_worker = child_worker.with_persistent_commands(profile.clone());
    }
    if let Some(bridge) = &request.event_bridge {
        child_worker = child_worker.with_event_bridge(bridge.clone());
    }
    if let Some(identity) = request.child_identity_budget {
        child_worker = child_worker.with_identity_budget(identity);
    }
    if let Some(budget) = request.budget_policy {
        child_worker = child_worker.with_token_budget_policy(budget);
    }
    let base_tools: Arc<dyn ToolExecutor> = Arc::new(
        SubagentToolExecutor::new(
            request.base_tools.clone(),
            child_worker,
            coordinator,
            request.sessions.clone(),
            lease.clone(),
            SubagentToolLimits::default(),
        )
        .map_err(|error| format!("No se pudieron preparar subagentes para resume: {error}"))?,
    );

    // Legacy journals without a native admission have no turn identity to
    // bind goal mutations to. Preserve their original tool scope.
    let goal_tools = match turn.turn_id.as_deref() {
        Some(turn_id) => Some(Arc::new(
            openide_harness::NativeGoalToolExecutor::new_for_recovery(
                base_tools.clone(),
                request.sessions.clone(),
                lease.clone(),
                turn_id,
            )
            .map_err(|error| format!("No se pudieron preparar goals para resume: {error}"))?,
        )),
        None => None,
    };
    let base_tools: Arc<dyn ToolExecutor> = goal_tools
        .as_ref()
        .map(|tools| tools.clone() as Arc<dyn ToolExecutor>)
        .unwrap_or(base_tools);

    // Discovery, calls, and shutdown all happen on this same Tokio runtime.
    let prepared = if turn.mode == openide_core::TurnMode::Build {
        match &request.tool_preparation {
            Some(preparation) => Some(
                preparation
                    .prepare(&workspace, base_tools.clone(), &cancel)
                    .await
                    .map_err(|error| error.to_string())?,
            ),
            None => None,
        }
    } else {
        None
    };
    let outcome = run_with_prepared(
        &request,
        &system_instructions,
        &lease,
        base_tools,
        goal_tools.as_deref(),
        replay.active_tool_catalog_digest.as_deref(),
        prepared.as_deref(),
        &mut commands,
        updates,
        &cancel,
        event_forwarder.clone(),
        command_actor,
    )
    .await;
    if outcome.is_err() {
        let _ = forward_unreported_events(&request.sessions, &request.session_id, &event_forwarder);
    }
    let shutdown = match &prepared {
        Some(session) => session.shutdown().await.map_err(|error| error.to_string()),
        None => Ok(()),
    };
    match (outcome, shutdown) {
        (Ok(disposition), Ok(())) => Ok(disposition),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!("No se pudo cerrar MCP: {error}")),
        (Err(error), Err(shutdown_error)) => {
            Err(format!("{error}; no se pudo cerrar MCP: {shutdown_error}"))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_with_prepared(
    request: &ResumeWorkerRequest,
    system_instructions: &str,
    lease: &NativeSessionLease,
    base_tools: Arc<dyn ToolExecutor>,
    goal_tools: Option<&openide_harness::NativeGoalToolExecutor>,
    saved_catalog_digest: Option<&str>,
    prepared: Option<&dyn PreparedToolSession>,
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<ResumeCommand>,
    updates: &Sender<ResumeUpdate>,
    cancel: &CancellationToken,
    event_forwarder: Arc<ResumeEventForwarder>,
    command_actor: Option<Arc<openide_harness::NativeManualCommandActor>>,
) -> Result<ResumeDisposition, String> {
    let executor = prepared
        .map(|session| session.tools())
        .unwrap_or(base_tools);
    let (executor, catalog) = match &request.native_automation {
        Some(frozen) => {
            if prepared.is_some() || goal_tools.is_some() || command_actor.is_some() {
                return Err(
                    "La automatización no puede incorporar extensiones durante recuperación".into(),
                );
            }
            let catalog = frozen.catalog()?;
            if saved_catalog_digest.is_some_and(|saved| saved != catalog.digest_hex()) {
                return Err("El catálogo del turno difiere del origen de automatización".into());
            }
            (frozen.tools(), catalog)
        }
        None => select_saved_catalog(executor, saved_catalog_digest)?,
    };
    let mut policy = request.permission_policy.clone();
    if let Some(prepared) = prepared {
        for name in prepared.ask_tool_names() {
            if catalog.get(name).is_none() {
                return Err(format!(
                    "MCP exige permiso para herramienta no anunciada: {name}"
                ));
            }
            policy.require_ask_unless_denied(name);
        }
    }
    let mut engine = ResumeEngine::new(
        request.sessions.clone(),
        request.checkpoints.clone(),
        request.compatibility.clone(),
        request.model.clone(),
        executor,
        catalog,
        policy,
        request.budget_policy,
    )
    .with_limits(request.limits)
    .with_command_hooks(request.command_hooks.clone())
    .with_system_instructions(system_instructions.to_owned())
    .with_native_runtime_identity(request.provider.clone(), request.endpoint.clone())
    .with_mcp_runtime_identity_digest(
        prepared.and_then(|session| session.mcp_resume_identity_digest().map(str::to_owned)),
    )
    .with_event_observer(event_forwarder.clone());
    if request.native_automation.is_none() {
        engine = engine
            .with_session_approvals(request.sessions.clone(), request.command_isolation)
            .with_user_interaction(true);
    }
    if let Some(actor) = command_actor {
        engine = engine.with_manual_command_actor(actor);
    }
    // A single tool may need a pause, a resolution, and a later execution
    // step. Bound the whole UI recovery, not only one run_until_pause call.
    let max_actions = request
        .limits
        .max_model_steps
        .saturating_mul(3)
        .saturating_add(request.limits.max_tool_calls.saturating_mul(4))
        .saturating_add(8);
    let mut next_step = None;
    for _ in 0..max_actions {
        if cancel.is_cancelled() {
            return Err("Recuperación cancelada".into());
        }
        let step = match next_step.take() {
            Some(step) => step,
            None => engine
                .resume_step(lease, cancel.clone())
                .await
                .map_err(|error| error.to_string())?,
        };
        event_forwarder.forward_step_events(step.appended_events)?;
        match step.disposition {
            ResumeDisposition::Advanced => {}
            ResumeDisposition::NeedsPermission {
                request_id,
                tool_call,
            } => {
                let recorded = request
                    .sessions
                    .load_permission_decisions(&request.session_id)
                    .map_err(|error| format!("No se pudo leer la decisión de permiso: {error}"))?
                    .into_iter()
                    .find(|decision| decision.request_id == request_id);
                if let Some(recorded) = recorded {
                    let choice = match recorded.decision {
                        PermissionDecision::Allow if recorded.grant.is_some() => {
                            PermissionChoice::AllowSession
                        }
                        PermissionDecision::Allow => PermissionChoice::Allow,
                        PermissionDecision::Deny => PermissionChoice::Deny,
                    };
                    next_step = Some(
                        engine
                            .resolve_permission(lease, &request_id, choice)
                            .map_err(|error| error.to_string())?,
                    );
                    continue;
                }
                updates
                    .send(ResumeUpdate::NeedsPermission {
                        request_id: request_id.clone(),
                        tool_call,
                    })
                    .map_err(|_| "La UI dejó de escuchar la recuperación".to_owned())?;
                let command = wait_for_command(commands, cancel).await?;
                let ResumeCommand::ResolvePermission {
                    request_id: selected,
                    choice,
                } = command
                else {
                    return Err("La decisión no corresponde al permiso pendiente".into());
                };
                if selected != request_id {
                    return Err("El permiso pendiente cambió antes de la decisión".into());
                }
                next_step = Some(
                    engine
                        .resolve_permission(lease, &request_id, choice)
                        .map_err(|error| error.to_string())?,
                );
            }
            ResumeDisposition::ToolOutcomeUncertain {
                tool_call,
                executor_error,
            } => {
                let goal_receipt = goal_tools
                    .filter(|_| {
                        matches!(
                            tool_call.name.as_str(),
                            "record_goal_evidence" | "complete_goal"
                        )
                    })
                    .map(|tools| -> Result<GoalReceiptInspection, String> {
                        match tools.committed_mutation(
                            &tool_call,
                            &ToolContext {
                                workspace: request
                                    .sessions
                                    .native_session_for_lease(lease)
                                    .map_err(|error| error.to_string())?
                                    .workspace,
                                cancel: cancel.clone(),
                            },
                        ) {
                            Ok(Some(receipt)) => Ok(GoalReceiptInspection::Found(receipt)),
                            Ok(None) => Ok(GoalReceiptInspection::NotFound),
                            Err(error) => Ok(GoalReceiptInspection::Failed(error.to_string())),
                        }
                    })
                    .transpose()?;
                updates
                    .send(ResumeUpdate::NeedsVerification {
                        tool_call: tool_call.clone(),
                        executor_error,
                        goal_receipt,
                    })
                    .map_err(|_| "La UI dejó de escuchar la recuperación".to_owned())?;
                let command = wait_for_command(commands, cancel).await?;
                let ResumeCommand::RecordVerifiedToolOutcome {
                    tool_call_id,
                    result,
                } = command
                else {
                    return Err("La decisión no corresponde a la herramienta incierta".into());
                };
                if tool_call_id != tool_call.id {
                    return Err("La herramienta incierta cambió antes de verificarla".into());
                }
                next_step = Some(
                    engine
                        .record_verified_tool_outcome(lease, &tool_call_id, result)
                        .map_err(|error| error.to_string())?,
                );
            }
            disposition => return Ok(disposition),
        }
    }
    Err(AgentError::Limit("recovery exceeded configured step limit".into()).to_string())
}

#[derive(Clone, Copy)]
enum HistoricalSubagentCatalog {
    Current,
    WithoutSendInput,
    WithoutSubagents,
}

/// Preserve an interrupted turn's exact advertised schema when newer tool
/// revisions are installed. A saved digest selects one of the known catalog
/// generations; execution rejects fields and names removed from that view.
struct HistoricalToolCatalog {
    inner: Arc<dyn ToolExecutor>,
    subagents: HistoricalSubagentCatalog,
    legacy_start_command: bool,
    /// Before the command rule in descriptions and `timeout_ms` in
    /// `start_command` and `poll_command`.
    before_command_revision: bool,
    without_persistent_commands: bool,
    without_resize_command: bool,
    without_goals: bool,
}

#[async_trait]
impl ToolExecutor for HistoricalToolCatalog {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.inner
            .definitions()
            .into_iter()
            .filter(|definition| self.advertises(&definition.name))
            .map(|definition| {
                if self.legacy_start_command && definition.name == "start_command" {
                    PersistentCommandManager::legacy_start_command_definition()
                } else if self.before_command_revision {
                    openide_harness::definition_before_command_revision(definition)
                } else {
                    definition
                }
            })
            .collect()
    }

    fn certifies_plan_read(&self, name: &str) -> bool {
        self.advertises(name) && self.inner.certifies_plan_read(name)
    }

    fn certifies_parallel_read(&self, name: &str) -> bool {
        self.advertises(name) && self.inner.certifies_parallel_read(name)
    }

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        self.validate_call(call)?;
        self.inner.execute(call, context).await
    }

    async fn execute_streaming(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        output: openide_harness::ToolOutputSender,
    ) -> Result<ToolOutput, AgentError> {
        self.validate_call(call)?;
        self.inner.execute_streaming(call, context, output).await
    }
}

impl HistoricalToolCatalog {
    fn validate_call(&self, call: &ToolCall) -> Result<(), AgentError> {
        if !self.advertises(&call.name) {
            return Err(AgentError::InvalidRequest(
                "tool was not advertised for this turn".into(),
            ));
        }
        if self.legacy_start_command
            && call.name == "start_command"
            && call.arguments.as_object().is_some_and(|arguments| {
                arguments.contains_key("pty")
                    || arguments.contains_key("columns")
                    || arguments.contains_key("rows")
            })
        {
            return Err(AgentError::InvalidRequest(
                "PTY fields were not advertised for this turn".into(),
            ));
        }
        if (self.before_command_revision || (self.legacy_start_command && call.name == "start_command"))
            && is_persistent_command_tool(&call.name)
            && call
                .arguments
                .as_object()
                .is_some_and(|arguments| arguments.contains_key("timeout_ms"))
        {
            return Err(AgentError::InvalidRequest(
                "timeout_ms was not advertised for this turn".into(),
            ));
        }
        Ok(())
    }

    fn advertises(&self, name: &str) -> bool {
        if self.without_goals
            && matches!(name, "get_goal" | "record_goal_evidence" | "complete_goal")
        {
            return false;
        }
        if self.without_persistent_commands && is_persistent_command_tool(name) {
            return false;
        }
        if self.without_resize_command && name == "resize_command" {
            return false;
        }
        match self.subagents {
            HistoricalSubagentCatalog::Current => true,
            HistoricalSubagentCatalog::WithoutSendInput => name != "send_input",
            HistoricalSubagentCatalog::WithoutSubagents => !is_subagent_tool(name),
        }
    }
}

fn select_saved_catalog(
    current: Arc<dyn ToolExecutor>,
    saved_catalog_digest: Option<&str>,
) -> Result<(Arc<dyn ToolExecutor>, ToolCatalog), String> {
    let current_catalog = ToolCatalog::from_executor(current.as_ref())
        .map_err(|error| format!("Catálogo de herramientas no válido: {error}"))?;
    // The engine appends the built-ins that wait for the user to whichever
    // executor catalog is chosen here, so a saved digest that includes them
    // selects the same executor generation as one that does not.
    let saved = |catalog: &ToolCatalog| {
        saved_catalog_digest.is_some_and(|saved| {
            saved == catalog.digest_hex()
                || openide_harness::catalogs_with_user_interaction(catalog)
                    .iter()
                    .any(|with| saved == with.digest_hex())
        })
    };
    if saved_catalog_digest.is_none() || saved(&current_catalog) {
        return Ok((current, current_catalog));
    }
    if let Some(plan_catalog) = catalog_with_plan_tool(&current_catalog)? {
        if saved(&plan_catalog) {
            return Ok((current, plan_catalog));
        }
    }
    for without_goals in [false, true] {
        for without_persistent_commands in [false, true] {
            for legacy_start_command in [false, true] {
                if without_persistent_commands && legacy_start_command {
                    continue;
                }
                for (without_resize_command, before_command_revision) in
                    [(false, false), (false, true), (true, false), (true, true)]
                {
                    for subagents in [
                        HistoricalSubagentCatalog::WithoutSendInput,
                        HistoricalSubagentCatalog::WithoutSubagents,
                        HistoricalSubagentCatalog::Current,
                    ] {
                        if !without_goals
                            && !without_persistent_commands
                            && !legacy_start_command
                            && !without_resize_command
                            && !before_command_revision
                            && matches!(subagents, HistoricalSubagentCatalog::Current)
                        {
                            continue;
                        }
                        let candidate: Arc<dyn ToolExecutor> = Arc::new(HistoricalToolCatalog {
                            inner: current.clone(),
                            subagents,
                            legacy_start_command,
                            before_command_revision,
                            without_persistent_commands,
                            without_resize_command,
                            without_goals,
                        });
                        let catalog =
                            ToolCatalog::from_executor(candidate.as_ref()).map_err(|error| {
                                format!("Catálogo de herramientas anterior no válido: {error}")
                            })?;
                        if saved(&catalog) {
                            return Ok((candidate, catalog));
                        }
                        if let Some(plan_catalog) = catalog_with_plan_tool(&catalog)? {
                            if saved(&plan_catalog) {
                                return Ok((candidate, plan_catalog));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok((current, current_catalog))
}

fn catalog_with_plan_tool(catalog: &ToolCatalog) -> Result<Option<ToolCatalog>, String> {
    if catalog.get("update_plan").is_some() {
        return Ok(None);
    }
    ToolCatalog::new(
        catalog
            .definitions()
            .iter()
            .cloned()
            .chain(std::iter::once(update_plan_tool_definition())),
    )
    .map(Some)
    .map_err(|error| format!("Catálogo de plan no válido: {error}"))
}

fn is_subagent_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent" | "send_input" | "wait_agent" | "cancel_agent"
    )
}

fn is_persistent_command_tool(name: &str) -> bool {
    matches!(
        name,
        "start_command" | "poll_command" | "write_stdin" | "terminate_command" | "resize_command"
    )
}

async fn wait_for_command(
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<ResumeCommand>,
    cancel: &CancellationToken,
) -> Result<ResumeCommand, String> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("Recuperación cancelada".into()),
        command = commands.recv() => {
            command.ok_or_else(|| "Se cerró el canal de decisiones de recuperación".into())
        }
    }
}

/// One cursor is shared by immediate observation and the completed step. Only
/// completed engine steps may repeat an already observed prefix; live delivery
/// requires the exact next sequence and owner.
struct ResumeEventForwarder {
    session_id: String,
    updates: Sender<ResumeUpdate>,
    last_reported_sequence: Mutex<u64>,
}

impl ResumeEventForwarder {
    fn forward_step_events(&self, events: Vec<AgentEvent>) -> Result<(), String> {
        self.validate_owner_and_order(&events)?;
        let mut sequence = self
            .last_reported_sequence
            .lock()
            .map_err(|_| "Se perdió el cursor de eventos de recuperación".to_owned())?;
        let events = events
            .into_iter()
            .filter(|event| event.sequence > *sequence)
            .collect();
        forward_events(&self.updates, &mut sequence, events)
    }

    fn validate_owner_and_order(&self, events: &[AgentEvent]) -> Result<(), String> {
        if events
            .iter()
            .any(|event| event.session_id != self.session_id)
            || events
                .windows(2)
                .any(|pair| pair[0].sequence.checked_add(1) != Some(pair[1].sequence))
        {
            return Err("Los eventos no pertenecen a esta recuperación y secuencia".into());
        }
        Ok(())
    }
}

#[async_trait]
impl ResumeEventObserver for ResumeEventForwarder {
    fn on_persisted_event(&self, event: &AgentEvent) -> Result<(), AgentError> {
        self.validate_owner_and_order(std::slice::from_ref(event))
            .map_err(AgentError::Runtime)?;
        let mut sequence = self.last_reported_sequence.lock().map_err(|_| {
            AgentError::Runtime("Se perdió el cursor de eventos de recuperación".into())
        })?;
        forward_events(&self.updates, &mut sequence, vec![event.clone()])
            .map_err(AgentError::Runtime)
    }

    async fn on_persisted_stream_event(
        &self,
        event: &AgentEvent,
        cancel: &CancellationToken,
    ) -> Result<(), AgentError> {
        self.validate_owner_and_order(std::slice::from_ref(event))
            .map_err(AgentError::Runtime)?;
        let previous_sequence = {
            let sequence = self.last_reported_sequence.lock().map_err(|_| {
                AgentError::Runtime("Se perdió el cursor de eventos de recuperación".into())
            })?;
            if sequence.checked_add(1) != Some(event.sequence) {
                return Err(AgentError::Runtime(
                    "El journal cambió durante la recuperación".into(),
                ));
            }
            *sequence
        };
        let mut update = ResumeUpdate::Events(vec![event.clone()]);
        loop {
            match self.updates.try_send(update) {
                Ok(()) => {
                    // Only the recovery worker invokes this observer. Leave
                    // its cursor unchanged while waiting so cancellation can
                    // replay this one pending delivery after dropping HTTP.
                    let mut sequence = self.last_reported_sequence.lock().map_err(|_| {
                        AgentError::Runtime("Se perdió el cursor de eventos de recuperación".into())
                    })?;
                    if *sequence != previous_sequence {
                        return Err(AgentError::Runtime(
                            "El cursor cambió durante la entrega de recuperación".into(),
                        ));
                    }
                    *sequence = event.sequence;
                    return Ok(());
                }
                Err(mpsc::TrySendError::Full(pending)) => update = pending,
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(AgentError::Runtime(
                        "La UI dejó de escuchar los eventos recuperados".into(),
                    ));
                }
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AgentError::Cancelled),
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
            }
        }
    }
}

fn forward_events(
    updates: &Sender<ResumeUpdate>,
    last_reported_sequence: &mut u64,
    events: Vec<AgentEvent>,
) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }
    let mut next = *last_reported_sequence;
    for event in &events {
        next = next
            .checked_add(1)
            .ok_or_else(|| "Secuencia de eventos agotada".to_owned())?;
        if event.sequence != next {
            return Err("El journal cambió durante la recuperación".into());
        }
    }
    for event in events {
        let sequence = event.sequence;
        updates
            .send(ResumeUpdate::Events(vec![event]))
            .map_err(|_| "La UI dejó de escuchar los eventos recuperados".to_owned())?;
        *last_reported_sequence = sequence;
    }
    Ok(())
}

fn forward_unreported_events(
    sessions: &SessionStore,
    session_id: &str,
    event_forwarder: &ResumeEventForwarder,
) -> Result<(), String> {
    let record = sessions
        .load(session_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "La sesión nativa ya no existe".to_owned())?;
    event_forwarder.forward_step_events(record.events)
}

#[cfg(test)]
#[path = "resume_streaming_tests.rs"]
mod streaming_tests;

#[cfg(test)]
#[path = "resume_automation_tests.rs"]
mod automation_tests;

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    mod manual_command_gates {
        include!("resume_manual_command_tests.rs");
    }

    use super::*;
    use async_trait::async_trait;
    use openide_core::{AgentEventKind, AgentMessage, TurnMode, MODEL_CONTEXT_VERSION};
    use openide_harness::{
        native_model_context_digest, CommandHookConfig, HookPhase, LocalTools, ModelOutput,
        ToolDefinition,
    };
    use serde_json::json;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    struct WaitingModel {
        started: mpsc::Sender<()>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl ModelBackend for WaitingModel {
        fn model_id(&self) -> &str {
            "test-model"
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
            self.started.send(()).unwrap();
            let release = self.release.lock().unwrap().take().unwrap();
            release.await.unwrap();
            Ok(ModelOutput {
                content: "Recovered".into(),
                tool_calls: vec![],
            })
        }
    }

    struct TrackingPreparation {
        shutdown: Arc<AtomicBool>,
    }

    struct ImmediateModel;

    #[async_trait]
    impl ModelBackend for ImmediateModel {
        fn model_id(&self) -> &str {
            "test-model"
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
            Ok(ModelOutput {
                content: "Permission resolved".into(),
                tool_calls: vec![],
            })
        }
    }

    struct TrackingSession {
        tools: Arc<dyn ToolExecutor>,
        shutdown: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ToolPreparation for TrackingPreparation {
        async fn prepare(
            &self,
            _workspace: &Path,
            base_tools: Arc<dyn ToolExecutor>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn PreparedToolSession>, AgentError> {
            assert!(tokio::runtime::Handle::try_current().is_ok());
            Ok(Box::new(TrackingSession {
                tools: base_tools,
                shutdown: self.shutdown.clone(),
            }))
        }
    }

    #[async_trait]
    impl PreparedToolSession for TrackingSession {
        fn tools(&self) -> Arc<dyn ToolExecutor> {
            self.tools.clone()
        }

        fn ask_tool_names(&self) -> &[String] {
            &[]
        }

        async fn shutdown(&self) -> Result<(), AgentError> {
            assert!(tokio::runtime::Handle::try_current().is_ok());
            self.shutdown.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn compatibility() -> CheckpointCompatibility {
        CheckpointCompatibility {
            provider: "test".into(),
            model: "test-model".into(),
            context_format_key: "test-v1".into(),
        }
    }

    fn budget() -> TokenBudgetPolicy {
        TokenBudgetPolicy {
            input_limit_tokens: None,
            context_window_tokens: 100_000,
            output_reserve_tokens: 1_000,
            tool_schema_reserve_tokens: 100,
            auto_compact_token_limit: None,
        }
    }

    fn append(sessions: &SessionStore, session_id: &str, kind: AgentEventKind) {
        let record = sessions.load(session_id).unwrap().unwrap();
        let sequence = record.events.last().unwrap().sequence + 1;
        sessions
            .append_native_event(&AgentEvent::new(session_id, sequence, kind))
            .unwrap();
    }

    fn append_context(sessions: &SessionStore, session_id: &str, local: &LocalTools) {
        append_context_with_hooks(sessions, session_id, local, &CommandHooks::default());
    }

    fn append_context_with_hooks(
        sessions: &SessionStore,
        session_id: &str,
        local: &LocalTools,
        hooks: &CommandHooks,
    ) {
        append(
            sessions,
            session_id,
            AgentEventKind::HooksCommitted {
                version: openide_core::HOOK_CONFIG_IDENTITY_VERSION,
                digest: hooks.digest_hex().unwrap(),
            },
        );
        append(
            sessions,
            session_id,
            AgentEventKind::ExecPolicyCommitted {
                version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                digest: PermissionPolicy::new().digest_hex(),
            },
        );
        let record = sessions.load(session_id).unwrap().unwrap();
        let catalog = ToolCatalog::from_executor(local).unwrap();
        let digest = native_model_context_digest(
            "test",
            "https://test.example/v1",
            "test-model",
            TurnMode::Build,
            &record.session.workspace,
            &catalog,
            "",
            [7; 32],
        )
        .unwrap();
        append(
            sessions,
            session_id,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest,
            },
        );
    }

    #[test]
    fn model_recovery_streams_persisted_events_then_releases_lease_after_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let other = SessionStore::open(dir.path()).unwrap();
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        let local = Arc::new(LocalTools::new());
        let digest = ToolCatalog::from_executor(local.as_ref())
            .unwrap()
            .digest_hex();
        sessions
            .admit_native_turn(
                &session.id,
                "turn-1",
                "Recover this",
                "test-model",
                TurnMode::Build,
            )
            .unwrap();
        append(
            &sessions,
            &session.id,
            AgentEventKind::TurnStarted {
                prompt: "Recover this".into(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ToolCatalogCommitted { digest },
        );
        append_context(&sessions, &session.id, &local);
        append(
            &sessions,
            &session.id,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "test-model".into(),
            },
        );
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
            native_automation: None,
            session_id: session.id.clone(),
            provider: "test".into(),
            endpoint: "https://test.example/v1".into(),
            sessions: sessions.clone(),
            checkpoints,
            compatibility: compatibility(),
            model: Arc::new(WaitingModel {
                started: started_tx,
                release: Mutex::new(Some(release_rx)),
            }),
            base_tools: local.clone(),
            child_command_profile: None,
            child_tools: local,
            event_bridge: None,
            tool_preparation: Some(Arc::new(TrackingPreparation {
                shutdown: shutdown.clone(),
            })),
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: None,
            child_identity_budget: None,
            system_instructions: String::new(),
            pre_context_instructions: None,
            limits: RunLimits::default(),
        })
        .unwrap();
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(other.try_acquire_native_lease(&session.id).is_err());
        release_tx.send(()).unwrap();
        let mut events = Vec::new();
        loop {
            match handle.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
                ResumeUpdate::Events(batch) => events.extend(batch),
                ResumeUpdate::Finished(Ok(ResumeDisposition::Complete)) => break,
                update => panic!("unexpected resume update: {update:?}"),
            }
        }
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].sequence, 8);
        assert!(matches!(
            events[2].kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert!(shutdown.load(Ordering::SeqCst));
        assert!(other.try_acquire_native_lease(&session.id).is_ok());
    }

    #[test]
    fn recovery_accepts_catalog_with_subagent_coordination_tools() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path().join("sessions")).unwrap());
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path().join("sessions")).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        let local = Arc::new(LocalTools::new());
        let model: Arc<dyn ModelBackend> = Arc::new(ImmediateModel);
        let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
        let coordinator = Arc::new(
            SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap(),
        );
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            local.clone(),
            "test",
            "https://test.example/v1",
        );
        let wrapper = SubagentToolExecutor::new(
            local.clone(),
            worker,
            coordinator,
            sessions.clone(),
            lease.clone(),
            SubagentToolLimits::default(),
        )
        .unwrap();
        let catalog = ToolCatalog::from_executor(&wrapper).unwrap();
        sessions
            .admit_native_turn(
                &session.id,
                "turn-subagents",
                "Continue",
                "test-model",
                openide_core::TurnMode::Build,
            )
            .unwrap();
        append(
            &sessions,
            &session.id,
            AgentEventKind::TurnStarted {
                prompt: "Continue".into(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::HooksCommitted {
                version: openide_core::HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ExecPolicyCommitted {
                version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                digest: PermissionPolicy::new().digest_hex(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: native_model_context_digest(
                    "test",
                    "https://test.example/v1",
                    "test-model",
                    TurnMode::Build,
                    dir.path(),
                    &catalog,
                    "",
                    [7; 32],
                )
                .unwrap(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "test-model".into(),
            },
        );
        drop(wrapper);
        drop(lease);
        let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
            native_automation: None,
            session_id: session.id.clone(),
            provider: "test".into(),
            endpoint: "https://test.example/v1".into(),
            sessions: sessions.clone(),
            checkpoints,
            compatibility: compatibility(),
            model,
            base_tools: local.clone(),
            child_command_profile: None,
            child_tools: local,
            event_bridge: None,
            tool_preparation: None,
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: Some(budget()),
            child_identity_budget: None,
            system_instructions: String::new(),
            pre_context_instructions: None,
            limits: RunLimits::default(),
        })
        .unwrap();
        loop {
            match handle.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
                ResumeUpdate::Events(_) => {}
                ResumeUpdate::Finished(Ok(ResumeDisposition::Complete)) => break,
                update => panic!("unexpected recovery update: {update:?}"),
            }
        }
        assert!(matches!(
            sessions
                .load(&session.id)
                .unwrap()
                .unwrap()
                .events
                .last()
                .unwrap()
                .kind,
            AgentEventKind::TurnCompleted { .. }
        ));
    }

    #[test]
    fn cancellation_wakes_pending_permission_and_releases_lease() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let other = SessionStore::open(dir.path()).unwrap();
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        let local = Arc::new(LocalTools::new());
        let digest = ToolCatalog::from_executor(local.as_ref())
            .unwrap()
            .digest_hex();
        sessions
            .admit_native_turn(
                &session.id,
                "turn-1",
                "Read a file",
                "test-model",
                TurnMode::Build,
            )
            .unwrap();
        append(
            &sessions,
            &session.id,
            AgentEventKind::TurnStarted {
                prompt: "Read a file".into(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ToolCatalogCommitted { digest },
        );
        append_context(&sessions, &session.id, &local);
        append(
            &sessions,
            &session.id,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "test-model".into(),
            },
        );
        let call = ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"hello.txt"}),
        };
        append(
            &sessions,
            &session.id,
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::PermissionRequested {
                request_id: "request-1".into(),
                tool_call: call,
            },
        );
        let (started_tx, _started_rx) = mpsc::channel();
        let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
        let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
            native_automation: None,
            session_id: session.id.clone(),
            provider: "test".into(),
            endpoint: "https://test.example/v1".into(),
            sessions,
            checkpoints,
            compatibility: compatibility(),
            model: Arc::new(WaitingModel {
                started: started_tx,
                release: Mutex::new(Some(release_rx)),
            }),
            base_tools: local.clone(),
            child_command_profile: None,
            child_tools: local,
            event_bridge: None,
            tool_preparation: None,
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: Some(budget()),
            child_identity_budget: None,
            system_instructions: String::new(),
            pre_context_instructions: None,
            limits: RunLimits::default(),
        })
        .unwrap();
        assert!(matches!(
            handle.updates.recv_timeout(Duration::from_secs(5)).unwrap(),
            ResumeUpdate::NeedsPermission { request_id, .. } if request_id == "request-1"
        ));
        assert!(other.try_acquire_native_lease(&session.id).is_err());
        handle.cancel();
        assert!(matches!(
            handle.updates.recv_timeout(Duration::from_secs(5)).unwrap(),
            ResumeUpdate::Finished(Err(error)) if error.contains("cancelada")
        ));
        assert!(other.try_acquire_native_lease(&session.id).is_ok());
    }

    #[test]
    fn pending_permission_is_resolved_once_and_then_turn_completes() {
        for pre_recorded in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
            let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
            let session = sessions.create(dir.path(), "native").unwrap();
            let local = Arc::new(LocalTools::new());
            let executable = dir.path().join("trusted-hook.bin");
            std::fs::write(&executable, b"trusted hook").unwrap();
            let hooks = Arc::new(
                CommandHooks::new(vec![CommandHookConfig {
                    id: "review".into(),
                    phase: HookPhase::PreToolUse,
                    tool_name: "read_file".into(),
                    executable,
                    args: vec![],
                    cwd: dir.path().to_path_buf(),
                    timeout_ms: 1000,
                    max_output_bytes: 4096,
                }])
                .unwrap(),
            );
            let digest = ToolCatalog::from_executor(local.as_ref())
                .unwrap()
                .digest_hex();
            sessions
                .admit_native_turn(
                    &session.id,
                    "turn-1",
                    "Read a file",
                    "test-model",
                    TurnMode::Build,
                )
                .unwrap();
            append(
                &sessions,
                &session.id,
                AgentEventKind::TurnStarted {
                    prompt: "Read a file".into(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ToolCatalogCommitted { digest },
            );
            append_context_with_hooks(&sessions, &session.id, &local, &hooks);
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "test-model".into(),
                },
            );
            let call = ToolCall {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: json!({"path":"hello.txt"}),
            };
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant: AgentMessage::assistant("", vec![call.clone()]),
                    response_id: None,
                    usage: None,
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::PermissionRequested {
                    request_id: "request-1".into(),
                    tool_call: call,
                },
            );
            if pre_recorded {
                sessions
                    .record_permission_decision(&session.id, "request-1", PermissionDecision::Deny)
                    .unwrap();
            }
            let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
                native_automation: None,
                session_id: session.id.clone(),
                provider: "test".into(),
                endpoint: "https://test.example/v1".into(),
                sessions: sessions.clone(),
                checkpoints,
                compatibility: compatibility(),
                model: Arc::new(ImmediateModel),
                base_tools: local.clone(),
                child_command_profile: None,
                child_tools: local,
                event_bridge: None,
                tool_preparation: None,
                permission_policy: PermissionPolicy::new(),
                command_isolation: CommandIsolation::HostUnrestricted,
                command_hooks: hooks,
                budget_policy: Some(budget()),
                child_identity_budget: None,
                system_instructions: String::new(),
                pre_context_instructions: None,
                limits: RunLimits::default(),
            })
            .unwrap();
            if !pre_recorded {
                assert!(matches!(
                    handle.updates.recv_timeout(Duration::from_secs(5)).unwrap(),
                    ResumeUpdate::NeedsPermission { request_id, .. } if request_id == "request-1"
                ));
                sessions
                    .record_permission_decision(&session.id, "request-1", PermissionDecision::Deny)
                    .unwrap();
                assert!(handle.resolve_permission("request-1", PermissionChoice::Deny));
            }
            let mut delivered = Vec::new();
            loop {
                match handle.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
                    ResumeUpdate::Events(events) => delivered.extend(events),
                    ResumeUpdate::Finished(Ok(ResumeDisposition::Complete)) => break,
                    update => panic!("unexpected resume update: {update:?}"),
                }
            }
            let stored = sessions.load(&session.id).unwrap().unwrap();
            let persisted = &stored.events[9..];
            assert_eq!(delivered, persisted);
            assert_eq!(
                delivered
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::PermissionResolved { .. }))
                    .count(),
                1
            );
            assert!(matches!(
                delivered.last().map(|event| &event.kind),
                Some(AgentEventKind::TurnCompleted { .. })
            ));
        }
    }

    /// What the journal gains from the request's resolution on, without what
    /// differs between two runs of the same turn.
    fn written(kind: &AgentEventKind) -> String {
        match kind {
            AgentEventKind::PermissionResolved { request_id, tool_call_id, resolution } => {
                format!("resolved {request_id} {tool_call_id} {resolution:?}")
            }
            AgentEventKind::ToolStarted { tool_call } => format!("started {}", tool_call.name),
            AgentEventKind::ToolResultMedia { tool_call_id, images, model_images, .. } => {
                format!("media {tool_call_id} {} {model_images:?}", images.len())
            }
            AgentEventKind::ToolFinished { tool_call_id, output, is_error } => {
                format!("finished {tool_call_id} {is_error} {output}")
            }
            other => serde_json::to_value(other).unwrap()["type"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        }
    }

    #[test]
    fn a_question_answered_before_the_turn_resumes_writes_what_one_answered_through_it_writes() {
        let image = openide_core::UserImage::from_bytes(
            openide_core::UserImageMime::Png,
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x02\x80\0\0\x01\xe0",
        )
        .unwrap();
        let answer = r#"{"answered_by":"user","answers":[{"header":"Layout","selected":["B"],"attached_images":[1]}]}"#;
        // `direct`: the card came back with the conversation and its answer
        // was recorded before any worker ran. Otherwise the worker asks and
        // the answer is recorded while it waits, as "Responder y reanudar" does.
        let resumed = |direct: bool| {
            let dir = tempfile::tempdir().unwrap();
            let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
            let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
            let session = sessions.create(dir.path(), "native").unwrap();
            let local = Arc::new(LocalTools::new());
            // What a Build turn freezes: the executor's tools, the plan
            // tool and the tools that wait for the user.
            let planned =
                catalog_with_plan_tool(&ToolCatalog::from_executor(local.as_ref()).unwrap())
                    .unwrap()
                    .unwrap();
            let asked = openide_harness::catalogs_with_user_interaction(&planned).swap_remove(0);
            sessions
                .admit_native_turn(&session.id, "turn-1", "Ask me", "test-model", TurnMode::Build)
                .unwrap();
            let workspace = sessions.load(&session.id).unwrap().unwrap().session.workspace;
            let ask = ToolCall {
                id: "call-ask".into(),
                name: openide_harness::ASK_USER_TOOL.into(),
                arguments: json!({"questions": [{
                    "header": "Layout",
                    "question": "Which mockup?",
                    "options": [{"label": "A"}, {"label": "B"}],
                    "multi_select": false
                }]}),
            };
            for kind in [
                AgentEventKind::TurnStarted { prompt: "Ask me".into() },
                AgentEventKind::ToolCatalogCommitted { digest: asked.digest_hex() },
                AgentEventKind::HooksCommitted {
                    version: openide_core::HOOK_CONFIG_IDENTITY_VERSION,
                    digest: CommandHooks::default().digest_hex().unwrap(),
                },
                AgentEventKind::ExecPolicyCommitted {
                    version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                    digest: PermissionPolicy::new().digest_hex(),
                },
                AgentEventKind::ModelContextCommitted {
                    version: MODEL_CONTEXT_VERSION,
                    digest: native_model_context_digest(
                        "test",
                        "https://test.example/v1",
                        "test-model",
                        TurnMode::Build,
                        &workspace,
                        &asked,
                        "",
                        [7; 32],
                    )
                    .unwrap(),
                },
                AgentEventKind::ModelRequestStarted { step: 1, model: "test-model".into() },
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant: AgentMessage::assistant("", vec![ask.clone()]),
                    response_id: None,
                    usage: None,
                },
                AgentEventKind::PermissionRequested {
                    request_id: "request-1".into(),
                    tool_call: ask,
                },
            ] {
                append(&sessions, &session.id, kind);
            }
            let asked_at = sessions.load(&session.id).unwrap().unwrap().events.len();
            let record = || {
                sessions
                    .record_user_answer_with_images(
                        &session.id,
                        "request-1",
                        answer,
                        std::slice::from_ref(&image),
                    )
                    .unwrap();
            };
            if direct {
                record();
            }
            let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
                native_automation: None,
                session_id: session.id.clone(),
                provider: "test".into(),
                endpoint: "https://test.example/v1".into(),
                sessions: sessions.clone(),
                checkpoints,
                compatibility: compatibility(),
                model: Arc::new(ImmediateModel),
                base_tools: local.clone(),
                child_command_profile: None,
                child_tools: local,
                event_bridge: None,
                tool_preparation: None,
                permission_policy: PermissionPolicy::new(),
                command_isolation: CommandIsolation::HostUnrestricted,
                command_hooks: Arc::new(CommandHooks::default()),
                budget_policy: Some(budget()),
                child_identity_budget: None,
                system_instructions: String::new(),
                pre_context_instructions: None,
                limits: RunLimits::default(),
            })
            .unwrap();
            if !direct {
                let asked = handle.updates.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(
                    matches!(&asked, ResumeUpdate::NeedsPermission { request_id, .. } if request_id == "request-1"),
                    "{asked:?}"
                );
                record();
                assert!(handle.resolve_permission("request-1", PermissionChoice::Allow));
            }
            loop {
                // Answered beforehand, the worker never asks.
                match handle.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
                    ResumeUpdate::Events(_) => {}
                    ResumeUpdate::Finished(Ok(ResumeDisposition::Complete)) => break,
                    update => panic!("unexpected resume update: {update:?}"),
                }
            }
            let stored = sessions.load(&session.id).unwrap().unwrap();
            assert!(sessions.load_permission_decisions(&session.id).unwrap()[0]
                .answer_images
                .is_empty());
            stored.events[asked_at..]
                .iter()
                .map(|event| written(&event.kind))
                .collect::<Vec<_>>()
        };
        let through_the_turn = resumed(false);
        assert_eq!(
            through_the_turn[..4],
            [
                "resolved request-1 call-ask Allowed".to_owned(),
                "started ask_user".to_owned(),
                "media call-ask 1 [0]".to_owned(),
                format!("finished call-ask false {answer}"),
            ]
        );
        assert!(through_the_turn.last().unwrap().contains("turn_completed"), "{through_the_turn:?}");
        assert_eq!(resumed(true), through_the_turn);
    }

    #[test]
    fn native_goal_receipt_recovery_reports_exact_state_without_replaying_or_publishing_result() {
        use openide_core::{NativeGoalCriterion, NativeGoalStore};
        use openide_harness::NativeGoalToolExecutor;
        for case in ["committed", "absent", "corrupt"] {
            let root = tempfile::tempdir().unwrap();
            let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
            let session = sessions.create(root.path(), "native").unwrap();
            let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
            NativeGoalStore::new(sessions.clone())
                .create(
                    &lease,
                    "goal",
                    "Verify complete scope",
                    vec![NativeGoalCriterion {
                        id: "criterion".into(),
                        description: "Verify persisted work".into(),
                        evidence: None,
                    }],
                )
                .unwrap();
            // Completed evidence turn is a journal fixture. The goal mutation and
            // desktop recovery below execute the actual adapters and stores.
            sessions
                .admit_native_turn(&session.id, "seed", "Seed", "test-model", TurnMode::Build)
                .unwrap();
            append(
                &sessions,
                &session.id,
                AgentEventKind::TurnStarted {
                    prompt: "Seed".into(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "test-model".into(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant: AgentMessage::assistant("Evidence fixture", vec![]),
                    response_id: None,
                    usage: None,
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::TurnCompleted {
                    response: "Evidence fixture".into(),
                    steps: 1,
                },
            );
            sessions
                .admit_native_turn(
                    &session.id,
                    "mutation",
                    "Record evidence",
                    "test-model",
                    TurnMode::Build,
                )
                .unwrap();
            let local = Arc::new(LocalTools::new());
            let tools = NativeGoalToolExecutor::new(
                local.clone(),
                sessions.clone(),
                lease.clone(),
                "mutation",
            )
            .unwrap();
            let catalog = ToolCatalog::from_executor(&tools).unwrap();
            append(
                &sessions,
                &session.id,
                AgentEventKind::TurnStarted {
                    prompt: "Record evidence".into(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ToolCatalogCommitted {
                    digest: catalog.digest_hex(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::HooksCommitted {
                    version: openide_core::HOOK_CONFIG_IDENTITY_VERSION,
                    digest: CommandHooks::default().digest_hex().unwrap(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ExecPolicyCommitted {
                    version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                    digest: PermissionPolicy::new().digest_hex(),
                },
            );
            let digest = native_model_context_digest(
                "test",
                "https://test.example/v1",
                "test-model",
                TurnMode::Build,
                root.path(),
                &catalog,
                "",
                [7; 32],
            )
            .unwrap();
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelContextCommitted {
                    version: MODEL_CONTEXT_VERSION,
                    digest,
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "test-model".into(),
                },
            );
            let call = ToolCall {
                id: "evidence-call".into(),
                name: "record_goal_evidence".into(),
                arguments: json!({"goal_id":"goal", "expected_revision":0,"criterion_id":"criterion","turn_id":"seed","summary":"Fixture work checked"}),
            };
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant: AgentMessage::assistant("", vec![call.clone()]),
                    response_id: None,
                    usage: None,
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ToolStarted {
                    tool_call: call.clone(),
                },
            );
            if case != "absent" {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let output = runtime
                    .block_on(tools.execute(
                        &call,
                        &ToolContext {
                            workspace: root.path().into(),
                            cancel: CancellationToken::new(),
                        },
                    ))
                    .unwrap();
                assert!(!output.is_error, "{}", output.content);
            }
            tools.shutdown();
            drop(tools);
            drop(lease);
            let goal_path = root
                .path()
                .join(format!("{}.native-goals.json", session.id));
            if case == "corrupt" {
                std::fs::write(&goal_path, b"broken ledger").unwrap();
            }
            let before_goal = std::fs::read(&goal_path).unwrap();
            let before_events = sessions.load(&session.id).unwrap().unwrap().events;
            drop(sessions);
            let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
            let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
                native_automation: None,
                session_id: session.id.clone(),
                provider: "test".into(),
                endpoint: "https://test.example/v1".into(),
                sessions: sessions.clone(),
                checkpoints: Arc::new(CheckpointFileStore::new(root.path()).unwrap()),
                compatibility: compatibility(),
                model: Arc::new(ImmediateModel),
                base_tools: local.clone(),
                child_command_profile: None,
                child_tools: local,
                event_bridge: None,
                tool_preparation: None,
                permission_policy: PermissionPolicy::new(),
                command_isolation: CommandIsolation::HostUnrestricted,
                command_hooks: Arc::new(CommandHooks::default()),
                budget_policy: Some(budget()),
                child_identity_budget: None,
                system_instructions: String::new(),
                pre_context_instructions: None,
                limits: RunLimits::default(),
            })
            .unwrap();
            let update = handle.updates.recv_timeout(Duration::from_secs(5)).unwrap();
            let ResumeUpdate::NeedsVerification {
                goal_receipt: Some(receipt),
                tool_call,
                ..
            } = update
            else {
                panic!("missing receipt inspection for {case}: {update:?}")
            };
            assert_eq!(tool_call, call);
            match (case, &receipt) {
                ("committed", GoalReceiptInspection::Found(receipt)) => {
                    assert_eq!(receipt.goal_id, "goal");
                    assert_eq!(receipt.committed_revision, 1);
                    assert_eq!(receipt.current_revision, 1);
                }
                ("absent", GoalReceiptInspection::NotFound) => {}
                ("corrupt", GoalReceiptInspection::Failed(error)) => assert!(!error.is_empty()),
                _ => panic!("unexpected receipt for {case}: {receipt:?}"),
            }
            assert_eq!(
                sessions.load(&session.id).unwrap().unwrap().events,
                before_events
            );
            assert_eq!(std::fs::read(&goal_path).unwrap(), before_goal);
            assert!(sessions.try_acquire_native_lease(&session.id).is_err());
            if case == "committed" {
                assert!(handle.record_verified_tool_outcome(
                    &call.id,
                    ToolOutput::success("Ledger inspected manually; evidence persisted")
                ));
                loop {
                    match handle.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
                        ResumeUpdate::Events(_) => {}
                        ResumeUpdate::Finished(Ok(ResumeDisposition::Complete)) => break,
                        update => panic!("unexpected verified recovery update: {update:?}"),
                    }
                }
                let events = sessions.load(&session.id).unwrap().unwrap().events;
                assert_eq!(&events[..before_events.len()], before_events.as_slice());
                let results: Vec<_> = events
                    .iter()
                    .filter_map(|event| match &event.kind {
                        AgentEventKind::ToolFinished {
                            tool_call_id,
                            output,
                            is_error,
                        } if tool_call_id == &call.id => Some((output.as_str(), *is_error)),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    results,
                    vec![("Ledger inspected manually; evidence persisted", false)]
                );
                assert_eq!(events.iter().filter(|event| matches!(&event.kind, AgentEventKind::ToolStarted { tool_call } if tool_call.id == call.id)).count(), 1);
            } else {
                handle.cancel();
                assert!(matches!(
                    handle.updates.recv_timeout(Duration::from_secs(5)).unwrap(),
                    ResumeUpdate::Finished(Err(_))
                ));
                assert_eq!(
                    sessions.load(&session.id).unwrap().unwrap().events,
                    before_events
                );
            }
            assert_eq!(std::fs::read(&goal_path).unwrap(), before_goal);
            let _released = sessions.try_acquire_native_lease(&session.id).unwrap();
        }
    }

    #[test]
    fn uncertain_tool_waits_for_manual_result_without_reexecuting() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        let local = Arc::new(LocalTools::new());
        let digest = ToolCatalog::from_executor(local.as_ref())
            .unwrap()
            .digest_hex();
        sessions
            .admit_native_turn(
                &session.id,
                "turn-1",
                "Read a file",
                "test-model",
                TurnMode::Build,
            )
            .unwrap();
        append(
            &sessions,
            &session.id,
            AgentEventKind::TurnStarted {
                prompt: "Read a file".into(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ToolCatalogCommitted { digest },
        );
        append_context(&sessions, &session.id, &local);
        append(
            &sessions,
            &session.id,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "test-model".into(),
            },
        );
        let call = ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"hello.txt"}),
        };
        append(
            &sessions,
            &session.id,
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ToolStarted { tool_call: call },
        );
        let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
            native_automation: None,
            session_id: session.id.clone(),
            provider: "test".into(),
            endpoint: "https://test.example/v1".into(),
            sessions: sessions.clone(),
            checkpoints,
            compatibility: compatibility(),
            model: Arc::new(ImmediateModel),
            base_tools: local.clone(),
            child_command_profile: None,
            child_tools: local,
            event_bridge: None,
            tool_preparation: None,
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: Some(budget()),
            child_identity_budget: None,
            system_instructions: String::new(),
            pre_context_instructions: None,
            limits: RunLimits::default(),
        })
        .unwrap();
        assert!(matches!(
            handle.updates.recv_timeout(Duration::from_secs(5)).unwrap(),
            ResumeUpdate::NeedsVerification { tool_call, .. } if tool_call.id == "call-1"
        ));
        assert_eq!(sessions.load(&session.id).unwrap().unwrap().events.len(), 9);
        assert!(handle.record_verified_tool_outcome("call-1", ToolOutput::success("checked")));
        let mut delivered = Vec::new();
        loop {
            match handle.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
                ResumeUpdate::Events(events) => delivered.extend(events),
                ResumeUpdate::Finished(Ok(ResumeDisposition::Complete)) => break,
                update => panic!("unexpected resume update: {update:?}"),
            }
        }
        let stored = sessions.load(&session.id).unwrap().unwrap();
        assert_eq!(delivered, stored.events[9..]);
        assert_eq!(
            stored
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. }))
                .count(),
            1
        );
        assert!(matches!(
            &delivered[0].kind,
            AgentEventKind::ToolFinished { output, .. } if output == "checked"
        ));
    }

    #[test]
    fn pending_mcp_call_without_snapshot_is_rejected_after_preparation() {
        for permission_pending in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
            let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
            let session = sessions.create(dir.path(), "native").unwrap();
            let local = Arc::new(LocalTools::new());
            let digest = ToolCatalog::from_executor(local.as_ref())
                .unwrap()
                .digest_hex();
            sessions
                .admit_native_turn(
                    &session.id,
                    "turn-1",
                    "MCP call",
                    "test-model",
                    TurnMode::Build,
                )
                .unwrap();
            append(
                &sessions,
                &session.id,
                AgentEventKind::TurnStarted {
                    prompt: "MCP call".into(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ToolCatalogCommitted { digest },
            );
            append_context(&sessions, &session.id, &local);
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "test-model".into(),
                },
            );
            let call = ToolCall {
                id: "call-mcp".into(),
                name: "mcp__server__read".into(),
                arguments: json!({}),
            };
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant: AgentMessage::assistant("", vec![call.clone()]),
                    response_id: None,
                    usage: None,
                },
            );
            if permission_pending {
                append(
                    &sessions,
                    &session.id,
                    AgentEventKind::PermissionRequested {
                        request_id: "request-1".into(),
                        tool_call: call,
                    },
                );
            }
            let shutdown = Arc::new(AtomicBool::new(false));
            let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
                native_automation: None,
                session_id: session.id.clone(),
                provider: "test".into(),
                endpoint: "https://test.example/v1".into(),
                sessions,
                checkpoints,
                compatibility: compatibility(),
                model: Arc::new(ImmediateModel),
                base_tools: local.clone(),
                child_command_profile: None,
                child_tools: local,
                event_bridge: None,
                tool_preparation: Some(Arc::new(TrackingPreparation {
                    shutdown: shutdown.clone(),
                })),
                permission_policy: PermissionPolicy::new(),
                command_isolation: CommandIsolation::HostUnrestricted,
                command_hooks: Arc::new(CommandHooks::default()),
                budget_policy: Some(budget()),
                child_identity_budget: None,
                system_instructions: String::new(),
                pre_context_instructions: None,
                limits: RunLimits::default(),
            })
            .unwrap();
            assert!(matches!(
                handle.updates.recv_timeout(Duration::from_secs(5)).unwrap(),
                ResumeUpdate::Finished(Err(error)) if error.contains("MCP server identity")
            ));
            assert!(shutdown.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn resume_selects_frozen_build_catalog_with_update_plan() {
        let current: Arc<dyn ToolExecutor> = Arc::new(LocalTools::new());
        let base = ToolCatalog::from_executor(current.as_ref()).unwrap();
        let planned = catalog_with_plan_tool(&base).unwrap().unwrap();
        let digest = planned.digest_hex();
        let (_, selected) = select_saved_catalog(current, Some(&digest)).unwrap();
        assert_eq!(selected.digest_hex(), digest);
        assert!(selected.get("update_plan").is_some());
    }

    #[test]
    fn resume_selects_the_executor_generation_under_the_tools_that_wait_for_the_user() {
        let current: Arc<dyn ToolExecutor> = Arc::new(LocalTools::new());
        let base = ToolCatalog::from_executor(current.as_ref()).unwrap();
        let planned = catalog_with_plan_tool(&base).unwrap().unwrap();
        // A Plan turn froze the reads and the question tool; a Build turn
        // froze update_plan between them. The engine appends the question
        // tool itself, so the executor catalog under it is what is selected.
        for under in [&base, &planned] {
            for asked in openide_harness::catalogs_with_user_interaction(under) {
                assert!(asked.get(openide_harness::ASK_USER_TOOL).is_some());
                let (_, selected) =
                    select_saved_catalog(current.clone(), Some(&asked.digest_hex())).unwrap();
                assert_eq!(selected.digest_hex(), under.digest_hex());
            }
        }
        // A turn frozen before the question tool existed still selects itself.
        let (_, selected) =
            select_saved_catalog(current, Some(&planned.digest_hex())).unwrap();
        assert_eq!(selected.digest_hex(), planned.digest_hex());
    }

    #[tokio::test]
    async fn resume_of_a_turn_recorded_before_timeout_ms_keeps_its_schema() {
        let workspace = tempfile::tempdir().unwrap();
        let tools = |manager: PersistentCommandManager| {
            LocalTools::new()
                .with_write_access(true)
                .with_command_isolation(openide_harness::CommandIsolation::HostUnrestricted)
                .with_persistent_commands(Arc::new(manager))
        };
        let manager = || {
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                openide_harness::CommandIsolation::HostUnrestricted,
                openide_harness::PersistentCommandLimits::default(),
            )
            .unwrap()
        };
        // The catalog an interrupted turn committed before the fields existed.
        let old: Vec<_> = tools(manager().with_tools_before_command_revision())
            .definitions()
            .into_iter()
            .map(openide_harness::definition_before_command_revision)
            .collect();
        assert!(old
            .iter()
            .all(|tool| !tool.description.contains("goes in `args`")));
        let saved_digest = ToolCatalog::new(old.clone()).unwrap().digest_hex();
        let current: Arc<dyn ToolExecutor> = Arc::new(tools(manager()));
        assert_ne!(
            ToolCatalog::from_executor(current.as_ref())
                .unwrap()
                .digest_hex(),
            saved_digest
        );
        let (selected, catalog) = select_saved_catalog(current, Some(&saved_digest)).unwrap();
        assert_eq!(catalog.digest_hex(), saved_digest);
        assert_eq!(selected.definitions(), old);
        for name in ["start_command", "poll_command"] {
            let result = selected
                .execute(
                    &ToolCall {
                        id: "old-turn-wait".into(),
                        name: name.into(),
                        arguments: serde_json::json!({
                            "command":"definitely-not-a-program",
                            "session_id":"00000000-0000-4000-8000-000000000000",
                            "timeout_ms":1000
                        }),
                    },
                    &ToolContext {
                        workspace: workspace.path().to_path_buf(),
                        cancel: CancellationToken::new(),
                    },
                )
                .await;
            assert!(
                matches!(&result, Err(AgentError::InvalidRequest(message)) if message.contains("timeout_ms")),
                "{name}: {result:?}"
            );
        }
    }

    /// The host terminal is how a command runs, not something a turn records:
    /// a session interrupted before the desktop turned it on resumes under
    /// the catalog it committed, and its commands then run on a terminal.
    #[tokio::test]
    async fn resume_of_a_turn_recorded_before_the_host_terminal_keeps_its_catalog() {
        let workspace = tempfile::tempdir().unwrap();
        let tools = |manager: PersistentCommandManager| -> Arc<dyn ToolExecutor> {
            Arc::new(
                LocalTools::new()
                    .with_write_access(true)
                    .with_command_isolation(openide_harness::CommandIsolation::HostUnrestricted)
                    .with_persistent_commands(Arc::new(manager)),
            )
        };
        let manager = || {
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                openide_harness::CommandIsolation::HostUnrestricted,
                openide_harness::PersistentCommandLimits::default(),
            )
            .unwrap()
        };
        let saved = ToolCatalog::from_executor(tools(manager()).as_ref())
            .unwrap()
            .digest_hex();
        let current = tools(manager().with_host_terminal());
        let (selected, catalog) = select_saved_catalog(current.clone(), Some(&saved)).unwrap();
        assert_eq!(catalog.digest_hex(), saved);
        assert!(Arc::ptr_eq(&selected, &current));
        // An even older turn, from before `timeout_ms`, still finds its own.
        let older = ToolCatalog::new(
            tools(manager().with_tools_before_command_revision())
                .definitions()
                .into_iter()
                .map(openide_harness::definition_before_command_revision),
        )
        .unwrap()
        .digest_hex();
        let (selected, catalog) = select_saved_catalog(current, Some(&older)).unwrap();
        assert_eq!(catalog.digest_hex(), older);
        #[cfg(unix)]
        {
            let context = ToolContext {
                workspace: workspace.path().to_path_buf(),
                cancel: CancellationToken::new(),
            };
            let call = |name: &str, arguments| ToolCall {
                id: "old-turn-command".into(),
                name: name.into(),
                arguments,
            };
            let started = selected
                .execute(
                    &call(
                        "start_command",
                        serde_json::json!({"command":"/bin/sh","args":["-c","test -t 1 && echo TTY"]}),
                    ),
                    &context,
                )
                .await
                .unwrap();
            let started: serde_json::Value = serde_json::from_str(&started.content).unwrap();
            let polled = selected
                .execute(
                    &call(
                        "poll_command",
                        serde_json::json!({"session_id":started["session_id"],"wait_ms":5000}),
                    ),
                    &context,
                )
                .await
                .unwrap();
            let polled: serde_json::Value = serde_json::from_str(&polled.content).unwrap();
            assert_eq!(polled["events"][0]["stream"], "terminal", "{polled}");
            assert_eq!(polled["events"][0]["text"], "TTY\n", "{polled}");
        }
    }

    #[tokio::test]
    async fn resume_selects_legacy_persistent_command_schema_and_blocks_pty_fields() {
        let workspace = tempfile::tempdir().unwrap();
        let legacy_manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                openide_harness::CommandIsolation::HostUnrestricted,
                openide_harness::PersistentCommandLimits::default(),
            )
            .unwrap()
            .with_tools_before_command_revision(),
        );
        let legacy_tools = LocalTools::new()
            .with_write_access(true)
            .with_command_isolation(openide_harness::CommandIsolation::HostUnrestricted)
            .with_persistent_commands(legacy_manager);
        let saved_digest = ToolCatalog::new(
            legacy_tools
                .definitions()
                .into_iter()
                .map(openide_harness::definition_before_command_revision),
        )
        .unwrap()
        .digest_hex();
        let pty_manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                openide_harness::CommandIsolation::HostUnrestricted,
                openide_harness::PersistentCommandLimits::default(),
            )
            .unwrap()
            .with_pty_tools(),
        );
        let current: Arc<dyn ToolExecutor> = Arc::new(
            LocalTools::new()
                .with_write_access(true)
                .with_command_isolation(openide_harness::CommandIsolation::HostUnrestricted)
                .with_persistent_commands(pty_manager),
        );
        let (selected, catalog) = select_saved_catalog(current, Some(&saved_digest)).unwrap();
        assert_eq!(catalog.digest_hex(), saved_digest);
        assert_eq!(
            selected
                .definitions()
                .into_iter()
                .find(|definition| definition.name == "start_command")
                .unwrap(),
            PersistentCommandManager::legacy_start_command_definition()
        );
        let result = selected
            .execute(
                &ToolCall {
                    id: "old-turn-pty".into(),
                    name: "start_command".into(),
                    arguments: serde_json::json!({"command":"test","pty":true}),
                },
                &ToolContext {
                    workspace: workspace.path().to_path_buf(),
                    cancel: CancellationToken::new(),
                },
            )
            .await;
        // On hosts without PTY support, the current schema is already the
        // legacy schema and the executor rejects unknown fields directly.
        // On Windows the historical wrapper rejects them before delegation.
        match result {
            Err(AgentError::InvalidRequest(message)) => assert!(message.contains("PTY fields")),
            Err(AgentError::Tool(message)) => assert!(message.contains("unknown field")),
            other => panic!("legacy schema did not reject PTY fields: {other:?}"),
        }
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn resume_selects_pre_resize_pty_catalog_and_blocks_resize() {
        let workspace = tempfile::tempdir().unwrap();
        let previous_manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                openide_harness::CommandIsolation::HostUnrestricted,
                openide_harness::PersistentCommandLimits::default(),
            )
            .unwrap()
            .with_pty_tools(),
        );
        let previous = LocalTools::new()
            .with_write_access(true)
            .with_command_isolation(openide_harness::CommandIsolation::HostUnrestricted)
            .with_persistent_commands(previous_manager);
        let saved_digest = ToolCatalog::from_executor(&previous).unwrap().digest_hex();

        let current_manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                openide_harness::CommandIsolation::HostUnrestricted,
                openide_harness::PersistentCommandLimits::default(),
            )
            .unwrap()
            .with_pty_tools()
            .with_pty_resize_tools(),
        );
        let current: Arc<dyn ToolExecutor> = Arc::new(
            LocalTools::new()
                .with_write_access(true)
                .with_command_isolation(openide_harness::CommandIsolation::HostUnrestricted)
                .with_persistent_commands(current_manager),
        );
        let (selected, catalog) = select_saved_catalog(current, Some(&saved_digest)).unwrap();
        assert_eq!(catalog.digest_hex(), saved_digest);
        assert!(catalog.get("start_command").is_some());
        assert!(catalog.get("resize_command").is_none());
        let result = selected
            .execute(
                &ToolCall {
                    id: "old-turn-resize".into(),
                    name: "resize_command".into(),
                    arguments: serde_json::json!({
                        "session_id":"00000000-0000-4000-8000-000000000001",
                        "columns":100,
                        "rows":30
                    }),
                },
                &ToolContext {
                    workspace: workspace.path().to_path_buf(),
                    cancel: CancellationToken::new(),
                },
            )
            .await;
        assert!(matches!(result, Err(AgentError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn resume_selects_catalog_before_persistent_commands_were_available() {
        let workspace = tempfile::tempdir().unwrap();
        let old = LocalTools::new()
            .with_write_access(true)
            .with_command_isolation(openide_harness::CommandIsolation::HostUnrestricted);
        let saved_digest = ToolCatalog::from_executor(&old).unwrap().digest_hex();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                openide_harness::CommandIsolation::HostUnrestricted,
                openide_harness::PersistentCommandLimits::default(),
            )
            .unwrap(),
        );
        let current: Arc<dyn ToolExecutor> = Arc::new(old.with_persistent_commands(manager));
        let (selected, catalog) = select_saved_catalog(current, Some(&saved_digest)).unwrap();
        assert_eq!(catalog.digest_hex(), saved_digest);
        assert!(selected
            .definitions()
            .iter()
            .all(|definition| !is_persistent_command_tool(&definition.name)));
        let result = selected
            .execute(
                &ToolCall {
                    id: "old-turn-start".into(),
                    name: "start_command".into(),
                    arguments: serde_json::json!({"command":"test"}),
                },
                &ToolContext {
                    workspace: workspace.path().to_path_buf(),
                    cancel: CancellationToken::new(),
                },
            )
            .await;
        assert!(matches!(result, Err(AgentError::InvalidRequest(_))));
    }
    struct RecoveryGoalReader(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl ModelBackend for RecoveryGoalReader {
        fn model_id(&self) -> &str {
            "test-model"
        }
        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([7; 32])
        }
        async fn complete(
            &self,
            messages: &[AgentMessage],
            tools: &[ToolDefinition],
            _: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            let system = &messages.first().unwrap().content;
            assert!(system.contains("Full recovery objective"));
            assert!(system.contains("Original project instructions"));
            assert!(system.contains("\"status\":\"active\""));
            assert!(!system.contains("Changed project instructions"));
            assert!(tools.iter().any(|t| t.name == "get_goal"));
            assert!(!tools
                .iter()
                .any(|t| matches!(t.name.as_str(), "record_goal_evidence" | "complete_goal")));
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "goal-read".into(),
                        name: "get_goal".into(),
                        arguments: json!({}),
                    }],
                })
            } else {
                assert!(messages
                    .iter()
                    .any(|m| m.content.contains("Full recovery objective")));
                Ok(ModelOutput {
                    content: "Verified recovered goal scope".into(),
                    tool_calls: vec![],
                })
            }
        }
    }
    #[test]
    fn resume_installs_goal_reader_and_preserves_frozen_plan_catalog_after_store_reopen() {
        use openide_core::{NativeGoalCriterion, NativeGoalStore};
        use openide_harness::NativeGoalToolExecutor;
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
        let goals = NativeGoalStore::new(sessions.clone());
        let goal = goals
            .create(
                &lease,
                "goal",
                "Full recovery objective",
                vec![NativeGoalCriterion {
                    id: "verify".into(),
                    description: "Inspect whole scope".into(),
                    evidence: None,
                }],
            )
            .unwrap();
        sessions
            .admit_native_turn(&session.id, "plan", "Inspect", "test-model", TurnMode::Plan)
            .unwrap();
        let local = Arc::new(LocalTools::new());
        let goal_tools =
            NativeGoalToolExecutor::new(local.clone(), sessions.clone(), lease.clone(), "plan")
                .unwrap();
        let catalog = ToolCatalog::new(
            goal_tools
                .definitions()
                .into_iter()
                .filter(|d| goal_tools.certifies_plan_read(&d.name)),
        )
        .unwrap();
        append(
            &sessions,
            &session.id,
            AgentEventKind::TurnStarted {
                prompt: "Inspect".into(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::HooksCommitted {
                version: openide_core::HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ExecPolicyCommitted {
                version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                digest: PermissionPolicy::new().digest_hex(),
            },
        );
        let instructions = openide_harness::native_goal_system_instructions(
            "Original project instructions",
            Some(&goal),
        )
        .unwrap();
        append(
            &sessions,
            &session.id,
            AgentEventKind::SystemInstructionsCommitted {
                version: openide_core::SYSTEM_INSTRUCTIONS_VERSION,
                digest: openide_core::system_instructions_digest(&instructions).unwrap(),
                content: instructions.clone(),
            },
        );
        let digest = native_model_context_digest(
            "test",
            "https://test.example/v1",
            "test-model",
            TurnMode::Plan,
            root.path(),
            &catalog,
            &instructions,
            [7; 32],
        )
        .unwrap();
        append(
            &sessions,
            &session.id,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest,
            },
        );
        append(
            &sessions,
            &session.id,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "test-model".into(),
            },
        );
        let paused = goals
            .update(
                &lease,
                "goal",
                "user-pause",
                0,
                openide_core::NativeGoalMutation::PauseByUser,
            )
            .unwrap();
        drop(goal_tools);
        drop(goals);
        drop(lease);
        drop(sessions);
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let model = Arc::new(RecoveryGoalReader(std::sync::atomic::AtomicUsize::new(0)));
        let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
            native_automation: None,
            session_id: session.id.clone(),
            provider: "test".into(),
            endpoint: "https://test.example/v1".into(),
            sessions: sessions.clone(),
            checkpoints: Arc::new(CheckpointFileStore::new(root.path()).unwrap()),
            compatibility: compatibility(),
            model: model.clone(),
            base_tools: local.clone(),
            child_command_profile: None,
            child_tools: local,
            event_bridge: None,
            tool_preparation: None,
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: Some(budget()),
            child_identity_budget: None,
            system_instructions: "Changed project instructions".into(),
            pre_context_instructions: None,
            limits: RunLimits::default(),
        })
        .unwrap();
        let mut delivered = vec![];
        loop {
            match handle
                .updates
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                ResumeUpdate::Events(events) => delivered.extend(events),
                ResumeUpdate::Finished(Ok(ResumeDisposition::Complete)) => break,
                update => panic!("{update:?}"),
            }
        }
        assert!(delivered
            .iter()
            .any(|e| matches!(e.kind, AgentEventKind::ToolFinished { .. })));
        assert_eq!(model.0.load(Ordering::SeqCst), 2);
        assert_eq!(
            NativeGoalStore::new(sessions).list(&session.id).unwrap(),
            vec![paused]
        );
    }
    #[tokio::test]
    async fn frozen_catalog_before_goals_blocks_new_goal_tools() {
        use openide_harness::NativeGoalToolExecutor;
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
        sessions
            .admit_native_turn(&session.id, "build", "Build", "test-model", TurnMode::Build)
            .unwrap();
        let local = Arc::new(LocalTools::new());
        for mode in [TurnMode::Build, TurnMode::Plan] {
            let old = ToolCatalog::new(
                local
                    .definitions()
                    .into_iter()
                    .filter(|d| mode == TurnMode::Build || local.certifies_plan_read(&d.name)),
            )
            .unwrap();
            let current: Arc<dyn ToolExecutor> = Arc::new(
                NativeGoalToolExecutor::new(
                    local.clone(),
                    sessions.clone(),
                    lease.clone(),
                    "build",
                )
                .unwrap(),
            );
            let (executor, selected) =
                select_saved_catalog(current, Some(&old.digest_hex())).unwrap();
            assert_eq!(selected.digest_hex(), old.digest_hex());
            assert!(selected.get("get_goal").is_none());
            assert!(!executor.certifies_plan_read("get_goal"));
            assert!(matches!(
                executor
                    .execute(
                        &ToolCall {
                            id: "injected".into(),
                            name: "get_goal".into(),
                            arguments: json!({})
                        },
                        &ToolContext {
                            workspace: root.path().into(),
                            cancel: CancellationToken::new()
                        }
                    )
                    .await,
                Err(AgentError::InvalidRequest(_))
            ));
        }
    }
    struct PreContextGoalModel {
        has_goal: bool,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl ModelBackend for PreContextGoalModel {
        fn model_id(&self) -> &str {
            "test-model"
        }
        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([7; 32])
        }
        async fn complete(
            &self,
            messages: &[AgentMessage],
            tools: &[ToolDefinition],
            _: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            let system = &messages.first().unwrap().content;
            assert!(system.contains("Exact project instructions"));
            assert_eq!(system.contains("Old legacy objective"), !self.has_goal);
            assert_eq!(system.contains("Current complete scope"), self.has_goal);
            assert!(tools.iter().any(|t| t.name == "get_goal"));
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                content: "Context received".into(),
                tool_calls: vec![],
            })
        }
    }
    #[test]
    fn resume_before_context_commit_selects_native_goal_under_lease_and_preserves_legacy_fallback()
    {
        use openide_core::{NativeGoalCriterion, NativeGoalStore};
        use openide_harness::NativeGoalToolExecutor;
        for has_goal in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
            let session = sessions.create(root.path(), "native").unwrap();
            let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
            let goals = NativeGoalStore::new(sessions.clone());
            let original = if has_goal {
                vec![goals
                    .create(
                        &lease,
                        "goal",
                        "Current complete scope",
                        vec![NativeGoalCriterion {
                            id: "verify".into(),
                            description: "Every effect".into(),
                            evidence: None,
                        }],
                    )
                    .unwrap()]
            } else {
                vec![]
            };
            sessions
                .admit_native_turn(&session.id, "plan", "Inspect", "test-model", TurnMode::Plan)
                .unwrap();
            let local = Arc::new(LocalTools::new());
            let tools =
                NativeGoalToolExecutor::new(local.clone(), sessions.clone(), lease.clone(), "plan")
                    .unwrap();
            let catalog = ToolCatalog::new(
                tools
                    .definitions()
                    .into_iter()
                    .filter(|d| tools.certifies_plan_read(&d.name)),
            )
            .unwrap();
            append(
                &sessions,
                &session.id,
                AgentEventKind::TurnStarted {
                    prompt: "Inspect".into(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ToolCatalogCommitted {
                    digest: catalog.digest_hex(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::HooksCommitted {
                    version: openide_core::HOOK_CONFIG_IDENTITY_VERSION,
                    digest: CommandHooks::default().digest_hex().unwrap(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ExecPolicyCommitted {
                    version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                    digest: PermissionPolicy::new().digest_hex(),
                },
            );
            drop(tools);
            drop(goals);
            drop(lease);
            drop(sessions);
            let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
            let model = Arc::new(PreContextGoalModel {
                has_goal,
                calls: std::sync::atomic::AtomicUsize::new(0),
            });
            let handle = ResumeWorkerHandle::start(ResumeWorkerRequest {
                native_automation: None,
                session_id: session.id.clone(),
                provider: "test".into(),
                endpoint: "https://test.example/v1".into(),
                sessions: sessions.clone(),
                checkpoints: Arc::new(CheckpointFileStore::new(root.path()).unwrap()),
                compatibility: compatibility(),
                model: model.clone(),
                base_tools: local.clone(),
                child_command_profile: None,
                child_tools: local,
                event_bridge: None,
                tool_preparation: None,
                permission_policy: PermissionPolicy::new(),
                command_isolation: CommandIsolation::HostUnrestricted,
                command_hooks: Arc::new(CommandHooks::default()),
                budget_policy: Some(budget()),
                child_identity_budget: None,
                system_instructions: "unselected fallback".into(),
                pre_context_instructions: Some((
                    "Exact project instructions".into(),
                    Some("Old legacy objective".into()),
                )),
                limits: RunLimits::default(),
            })
            .unwrap();
            loop {
                match handle
                    .updates
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap()
                {
                    ResumeUpdate::Events(_) => {}
                    ResumeUpdate::Finished(Ok(ResumeDisposition::Complete)) => break,
                    update => panic!("{update:?}"),
                }
            }
            assert_eq!(model.calls.load(Ordering::SeqCst), 1);
            let record = sessions.load(&session.id).unwrap().unwrap();
            let content = record
                .events
                .iter()
                .find_map(|e| match &e.kind {
                    AgentEventKind::SystemInstructionsCommitted { content, .. } => Some(content),
                    _ => None,
                })
                .unwrap();
            assert!(!content.contains("unselected fallback"));
            assert_eq!(content.contains("Old legacy objective"), !has_goal);
            assert_eq!(
                NativeGoalStore::new(sessions).list(&session.id).unwrap(),
                original
            );
        }
    }
}
