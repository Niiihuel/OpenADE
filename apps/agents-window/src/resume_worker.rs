//! UI-facing recovery worker. The native lease and prepared MCP clients stay
//! inside one Tokio thread until recovery finishes or is explicitly stopped.

use async_trait::async_trait;
use openide_core::{
    replay_native, AgentEvent, CheckpointCompatibility, CheckpointFileStore, NativeSessionLease,
    PermissionDecision, ReplayRecovery, SessionStore, ToolCall,
};
use openide_harness::{
    update_plan_tool_definition, AgentError, CommandHooks, CommandIsolation, ModelBackend,
    PermissionChoice, PermissionPolicy, PersistentCommandManager, PreparedToolSession,
    ResumeDisposition, ResumeEngine, RunLimits, SubagentCoordinator, SubagentEventBridge,
    SubagentLimits, SubagentToolExecutor, SubagentToolLimits, SubagentWorker, TokenBudgetPolicy,
    ToolCatalog, ToolContext, ToolDefinition, ToolExecutor, ToolOutput, ToolPreparation,
};
use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc,
};
use tokio_util::sync::CancellationToken;

pub(crate) struct ResumeWorkerRequest {
    pub session_id: String,
    pub provider: String,
    pub endpoint: String,
    pub sessions: Arc<SessionStore>,
    pub checkpoints: Arc<CheckpointFileStore>,
    pub compatibility: CheckpointCompatibility,
    pub model: Arc<dyn ModelBackend>,
    pub base_tools: Arc<dyn ToolExecutor>,
    pub child_tools: Arc<dyn ToolExecutor>,
    pub event_bridge: Option<Arc<SubagentEventBridge>>,
    pub tool_preparation: Option<Arc<dyn ToolPreparation>>,
    pub permission_policy: PermissionPolicy,
    pub command_isolation: CommandIsolation,
    pub command_hooks: Arc<CommandHooks>,
    pub budget_policy: Option<TokenBudgetPolicy>,
    pub system_instructions: String,
    pub limits: RunLimits,
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
    pub fn start(request: ResumeWorkerRequest) -> Result<Self, String> {
        let lease = request
            .sessions
            .try_acquire_native_lease(&request.session_id)
            .map_err(|error| format!("No se pudo bloquear la sesión nativa: {error}"))?;
        let session_id = request.session_id.clone();
        let (update_tx, updates) = mpsc::channel();
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
                    Ok(runtime) => runtime.block_on(run_recovery(
                        request,
                        lease,
                        command_rx,
                        &update_tx,
                        worker_cancel,
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

async fn run_recovery(
    request: ResumeWorkerRequest,
    lease: NativeSessionLease,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<ResumeCommand>,
    updates: &Sender<ResumeUpdate>,
    cancel: CancellationToken,
) -> Result<ResumeDisposition, String> {
    let lease = Arc::new(lease);
    let record = request
        .sessions
        .load(&request.session_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "La sesión nativa ya no existe".to_owned())?;
    let mut last_reported_sequence = record.events.last().map_or(0, |event| event.sequence);
    let replay = replay_native(
        &request.session_id,
        &record.events,
        Some(&request.checkpoints),
        &request.compatibility,
    )
    .map_err(|error| error.to_string())?;
    if replay.recovery == ReplayRecovery::Idle {
        return Ok(ResumeDisposition::Complete);
    }
    let system_instructions = replay
        .active_system_instructions
        .clone()
        .unwrap_or_else(|| request.system_instructions.clone());
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
    .with_system_instructions(system_instructions.clone())
    .with_limits(request.limits)
    .with_auto_compaction(request.checkpoints.clone(), request.compatibility.clone());
    if let Some(bridge) = &request.event_bridge {
        child_worker = child_worker.with_event_bridge(bridge.clone());
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
        replay.active_tool_catalog_digest.as_deref(),
        prepared.as_deref(),
        &mut commands,
        updates,
        &cancel,
        &mut last_reported_sequence,
    )
    .await;
    if outcome.is_err() {
        let _ = forward_unreported_events(
            &request.sessions,
            &request.session_id,
            updates,
            &mut last_reported_sequence,
        );
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
    saved_catalog_digest: Option<&str>,
    prepared: Option<&dyn PreparedToolSession>,
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<ResumeCommand>,
    updates: &Sender<ResumeUpdate>,
    cancel: &CancellationToken,
    last_reported_sequence: &mut u64,
) -> Result<ResumeDisposition, String> {
    let executor = prepared
        .map(|session| session.tools())
        .unwrap_or(base_tools);
    let (executor, catalog) = select_saved_catalog(executor, saved_catalog_digest)?;
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
    let engine = ResumeEngine::new(
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
    .with_session_approvals(request.sessions.clone(), request.command_isolation)
    .with_command_hooks(request.command_hooks.clone())
    .with_system_instructions(system_instructions.to_owned())
    .with_native_runtime_identity(request.provider.clone(), request.endpoint.clone())
    .with_mcp_runtime_identity_digest(
        prepared.and_then(|session| session.mcp_resume_identity_digest().map(str::to_owned)),
    );
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
        forward_events(updates, last_reported_sequence, step.appended_events)?;
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
                updates
                    .send(ResumeUpdate::NeedsVerification {
                        tool_call: tool_call.clone(),
                        executor_error,
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
    without_persistent_commands: bool,
    without_resize_command: bool,
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
                } else {
                    definition
                }
            })
            .collect()
    }

    fn certifies_parallel_read(&self, name: &str) -> bool {
        self.advertises(name) && self.inner.certifies_parallel_read(name)
    }

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
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
        self.inner.execute(call, context).await
    }
}

impl HistoricalToolCatalog {
    fn advertises(&self, name: &str) -> bool {
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
    if saved_catalog_digest.is_none_or(|saved| saved == current_catalog.digest_hex()) {
        return Ok((current, current_catalog));
    }
    if let Some(plan_catalog) = catalog_with_plan_tool(&current_catalog)? {
        if saved_catalog_digest == Some(plan_catalog.digest_hex().as_str()) {
            return Ok((current, plan_catalog));
        }
    }
    for without_persistent_commands in [false, true] {
        for legacy_start_command in [false, true] {
            if without_persistent_commands && legacy_start_command {
                continue;
            }
            for without_resize_command in [false, true] {
                for subagents in [
                    HistoricalSubagentCatalog::WithoutSendInput,
                    HistoricalSubagentCatalog::WithoutSubagents,
                    HistoricalSubagentCatalog::Current,
                ] {
                    if !without_persistent_commands
                        && !legacy_start_command
                        && !without_resize_command
                        && matches!(subagents, HistoricalSubagentCatalog::Current)
                    {
                        continue;
                    }
                    let candidate: Arc<dyn ToolExecutor> = Arc::new(HistoricalToolCatalog {
                        inner: current.clone(),
                        subagents,
                        legacy_start_command,
                        without_persistent_commands,
                        without_resize_command,
                    });
                    let catalog =
                        ToolCatalog::from_executor(candidate.as_ref()).map_err(|error| {
                            format!("Catálogo de herramientas anterior no válido: {error}")
                        })?;
                    if saved_catalog_digest == Some(catalog.digest_hex().as_str()) {
                        return Ok((candidate, catalog));
                    }
                    if let Some(plan_catalog) = catalog_with_plan_tool(&catalog)? {
                        if saved_catalog_digest == Some(plan_catalog.digest_hex().as_str()) {
                            return Ok((candidate, plan_catalog));
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
    updates
        .send(ResumeUpdate::Events(events))
        .map_err(|_| "La UI dejó de escuchar los eventos recuperados".to_owned())?;
    *last_reported_sequence = next;
    Ok(())
}

fn forward_unreported_events(
    sessions: &SessionStore,
    session_id: &str,
    updates: &Sender<ResumeUpdate>,
    last_reported_sequence: &mut u64,
) -> Result<(), String> {
    let record = sessions
        .load(session_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "La sesión nativa ya no existe".to_owned())?;
    let events = record
        .events
        .into_iter()
        .filter(|event| event.sequence > *last_reported_sequence)
        .collect();
    forward_events(updates, last_reported_sequence, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use openide_core::{AgentEventKind, AgentMessage, TurnMode, MODEL_CONTEXT_VERSION};
    use openide_harness::{
        native_model_context_digest, CommandHookConfig, HookPhase, LocalTools, ModelOutput,
        ToolDefinition,
    };
    use serde_json::json;
    use std::path::Path;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };
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
            child_tools: local,
            event_bridge: None,
            tool_preparation: Some(Arc::new(TrackingPreparation {
                shutdown: shutdown.clone(),
            })),
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: None,
            system_instructions: String::new(),
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
            session_id: session.id.clone(),
            provider: "test".into(),
            endpoint: "https://test.example/v1".into(),
            sessions: sessions.clone(),
            checkpoints,
            compatibility: compatibility(),
            model,
            base_tools: local.clone(),
            child_tools: local,
            event_bridge: None,
            tool_preparation: None,
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: Some(budget()),
            system_instructions: String::new(),
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
            child_tools: local,
            event_bridge: None,
            tool_preparation: None,
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: Some(budget()),
            system_instructions: String::new(),
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
                session_id: session.id.clone(),
                provider: "test".into(),
                endpoint: "https://test.example/v1".into(),
                sessions: sessions.clone(),
                checkpoints,
                compatibility: compatibility(),
                model: Arc::new(ImmediateModel),
                base_tools: local.clone(),
                child_tools: local,
                event_bridge: None,
                tool_preparation: None,
                permission_policy: PermissionPolicy::new(),
                command_isolation: CommandIsolation::HostUnrestricted,
                command_hooks: hooks,
                budget_policy: Some(budget()),
                system_instructions: String::new(),
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
            session_id: session.id.clone(),
            provider: "test".into(),
            endpoint: "https://test.example/v1".into(),
            sessions: sessions.clone(),
            checkpoints,
            compatibility: compatibility(),
            model: Arc::new(ImmediateModel),
            base_tools: local.clone(),
            child_tools: local,
            event_bridge: None,
            tool_preparation: None,
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: Some(budget()),
            system_instructions: String::new(),
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
                session_id: session.id.clone(),
                provider: "test".into(),
                endpoint: "https://test.example/v1".into(),
                sessions,
                checkpoints,
                compatibility: compatibility(),
                model: Arc::new(ImmediateModel),
                base_tools: local.clone(),
                child_tools: local,
                event_bridge: None,
                tool_preparation: Some(Arc::new(TrackingPreparation {
                    shutdown: shutdown.clone(),
                })),
                permission_policy: PermissionPolicy::new(),
                command_isolation: CommandIsolation::HostUnrestricted,
                command_hooks: Arc::new(CommandHooks::default()),
                budget_policy: Some(budget()),
                system_instructions: String::new(),
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

    #[tokio::test]
    async fn resume_selects_legacy_persistent_command_schema_and_blocks_pty_fields() {
        let workspace = tempfile::tempdir().unwrap();
        let legacy_manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                openide_harness::CommandIsolation::HostUnrestricted,
                openide_harness::PersistentCommandLimits::default(),
            )
            .unwrap(),
        );
        let legacy_tools = LocalTools::new()
            .with_write_access(true)
            .with_command_isolation(openide_harness::CommandIsolation::HostUnrestricted)
            .with_persistent_commands(legacy_manager);
        let saved_digest = ToolCatalog::from_executor(&legacy_tools)
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
        assert!(matches!(result, Err(AgentError::InvalidRequest(_))));
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
}
