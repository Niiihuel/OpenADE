//! Host projection and dispatch for the native human command actor.
use super::*;
use crate::native_command_control_ui::{
    NativeCommandControlCapabilities, NativeCommandControlKind, NativeCommandControlPhase,
    NativeCommandControlRequest, NativeCommandControlScope, NativeCommandControlUiState,
    NativeCommandInputState, NativeCommandOperationHistoryScope, NativeCommandTerminalSize,
};
use openide_core::{
    NativeCommandOperationAction, NativeCommandOperationSnapshot, NativeCommandOperationState,
    NativeCommandOperationStore,
};
use openide_harness::{
    NativeManualCommandActor, NativeManualCommandPolicy, NativeManualCommandPolicySource,
    PersistentCommandControlInspection, PersistentCommandStdinState,
};

pub(super) fn native_manual_command_policy_source(
    root: PathBuf,
) -> Arc<NativeManualCommandPolicySource> {
    Arc::new(move || {
        let settings = AgentToolSettings::load(&root)?;
        let permissions = settings
            .permission_policy()
            .with_exec_policy(ExecPolicySettings::load(&root)?.policy()?);
        Ok(NativeManualCommandPolicy {
            permissions,
            run_command_enabled: settings.permissions().run_command,
            isolation: settings.command_isolation(),
            hooks: Arc::new(NativeHookSettings::load(&root)?.registry()?),
        })
    })
}

pub(super) struct ControlObservation {
    owner: String,
    workspace: PathBuf,
    generation: Option<String>,
    command_id: Option<String>,
    control: Result<Option<PersistentCommandControlInspection>, String>,
    operations: Result<Vec<NativeCommandOperationSnapshot>, String>,
}
pub(super) type CommandLedgerResponse = (
    String,
    Result<(Vec<NativeCommandOperationSnapshot>, Option<String>), String>,
);

fn action(request: &NativeCommandControlRequest) -> NativeCommandOperationAction {
    match request {
        NativeCommandControlRequest::SendInput { input } => NativeCommandOperationAction::Stdin {
            input: input.clone(),
            close_after: false,
        },
        NativeCommandControlRequest::CloseInput => NativeCommandOperationAction::Stdin {
            input: String::new(),
            close_after: true,
        },
        NativeCommandControlRequest::Terminate => NativeCommandOperationAction::Terminate,
        NativeCommandControlRequest::Resize { columns, rows } => {
            NativeCommandOperationAction::Resize {
                columns: *columns,
                rows: *rows,
            }
        }
    }
}

fn capabilities(control: &PersistentCommandControlInspection) -> NativeCommandControlCapabilities {
    NativeCommandControlCapabilities {
        stdin: match control.stdin {
            PersistentCommandStdinState::Open => NativeCommandInputState::Open,
            PersistentCommandStdinState::Closed => NativeCommandInputState::Closed,
            PersistentCommandStdinState::Busy => NativeCommandInputState::Busy,
            PersistentCommandStdinState::Uncertain => NativeCommandInputState::Uncertain,
        },
        exit: control.exit.clone(),
        revoked: control.revoked,
        stop_requested: control.stop_requested,
        pty: control.pty,
        terminal_size: control.dimensions.map(|s| NativeCommandTerminalSize {
            columns: s.columns,
            rows: s.rows,
        }),
        dimensions_uncertain: control.dimensions_uncertain,
        resize_supported: control.resize_enabled,
        max_stdin_bytes: control.max_stdin_bytes,
    }
}

impl AgentsWindow {
    pub(super) fn open_native_command_controls(&mut self, command_id: &str) {
        self.observe_native_command_controls(Some(command_id.into()), true);
    }
    pub(super) fn native_command_actor_for(
        &mut self,
        owner: &str,
    ) -> Result<Arc<NativeManualCommandActor>, String> {
        let shared_actor = self
            .native_command_sessions
            .shared_registry()
            .map(|registry| registry.manual_actor(owner))
            .transpose()
            .map_err(|error| error.to_string())?
            .flatten();
        if let Some(actor) = shared_actor {
            self.native_command_actors
                .insert(owner.into(), actor.clone());
            return Ok(actor);
        }
        let manager = self.native_command_sessions.manager(owner);
        if let Some(actor) = self.native_command_actors.get(owner) {
            if actor.manager_generation() == manager.as_ref().map(|m| m.generation()) {
                return Ok(actor.clone());
            }
        }
        let sessions = self
            .store
            .clone()
            .ok_or("El store nativo no está disponible")?;
        let source = native_manual_command_policy_source(self.data_root.clone());
        let actor = Arc::new(
            NativeManualCommandActor::new(sessions, owner.into(), manager, source)
                .map_err(|e| e.to_string())?,
        );
        self.native_command_actors
            .insert(owner.into(), actor.clone());
        Ok(actor)
    }

    fn command_control_scope_current(&self) -> bool {
        self.view.selected_backend == "native"
            && self.view.native_command_control.matches_scope(
                self.view.selected_session.as_deref(),
                Path::new(&self.view.workspace),
            )
    }

    fn observe_native_command_controls(&mut self, command_id: Option<String>, reset: bool) {
        let Some(owner) = self.view.selected_session.clone() else {
            return;
        };
        let Some(sessions) = self.store.clone() else {
            return;
        };
        let workspace = match Path::new(&self.view.workspace).canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.view.status = error.to_string();
                return;
            }
        };
        if self.view.selected_backend != "native" {
            return;
        }
        let manager = self.native_command_sessions.manager(&owner);
        let generation = manager.as_ref().map(|m| m.generation().to_owned());
        if reset {
            self.view.native_command_control = NativeCommandControlUiState {
                open: true,
                history_scope: Some(NativeCommandOperationHistoryScope {
                    owner_session_id: owner.clone(),
                    workspace: workspace.clone(),
                }),
                ..Default::default()
            };
            self.native_command_prepared_request = None;
        }
        let (tx, rx) = mpsc::channel();
        self.native_command_control_response = Some(rx);
        std::thread::spawn(move || {
            let control = match (&manager, &command_id) {
                (Some(manager), Some(id)) => manager.control_snapshot(id).map(Some).map_err(|e| e.to_string()),
                (_, None) => Ok(None),
                (None, Some(_)) => Err("Los procesos no se restauran al reabrir; el historial de operaciones sigue disponible".into()),
            };
            let operations = NativeCommandOperationStore::new(sessions)
                .list(&owner)
                .map_err(|e| e.to_string());
            let _ = tx.send(ControlObservation {
                owner,
                workspace,
                generation,
                command_id,
                control,
                operations,
            });
        });
    }

    fn apply_command_ledger(
        &mut self,
        owner: &str,
        mut operations: Vec<NativeCommandOperationSnapshot>,
    ) {
        // Independent observation/actor replies may arrive out of order.
        // Withdrawn/finished revisions never regress to an older Requested
        // snapshot, and a late list cannot hide a just acknowledged intention.
        if self.view.selected_session.as_deref() == Some(owner)
            && self.command_control_scope_current()
        {
            for prior in &self.view.native_command_control.operations {
                if let Some(incoming) = operations
                    .iter_mut()
                    .find(|op| op.request.operation_id == prior.request.operation_id)
                {
                    if prior.revision > incoming.revision {
                        *incoming = prior.clone();
                    }
                } else {
                    operations.push(prior.clone());
                }
            }
        }
        let active_operation = self
            .native_command_actors
            .get(owner)
            .and_then(|a| a.active_operation_id());
        let pending = operations.iter().any(|op| {
            op.state == NativeCommandOperationState::Requested
                || active_operation.as_deref() == Some(op.request.operation_id.as_str())
        });
        if pending {
            self.native_command_poll_owners.insert(owner.into());
        } else {
            self.native_command_poll_owners.remove(owner);
        }
        if self.view.selected_session.as_deref() != Some(owner)
            || !self.command_control_scope_current()
        {
            return;
        }
        let state = &mut self.view.native_command_control;
        state.live_operation_id = active_operation.clone();
        let was_pending = matches!(state.phase, NativeCommandControlPhase::Pending { .. });
        let pending = match &state.phase {
            NativeCommandControlPhase::Pending {
                prepared,
                operation_id,
            }
            | NativeCommandControlPhase::OutcomeUncertain {
                prepared,
                operation_id,
                ..
            } => Some((prepared.clone(), operation_id.clone())),
            _ => None,
        };
        if let Some((prepared, operation_id)) = pending {
            if let Some(saved) = operations
                .iter()
                .find(|op| op.request.operation_id == operation_id)
            {
                if saved.state.requires_verification()
                    && active_operation.as_deref() != Some(operation_id.as_str())
                {
                    state.phase = NativeCommandControlPhase::OutcomeUncertain { prepared, operation_id: operation_id.clone(),
                        detail: "El recibo exige inspección manual. Reabrir o actualizar no reenvía la operación.".into() };
                    state.selected_operation_id = Some(operation_id);
                } else if !saved.state.is_pending() {
                    state.phase = NativeCommandControlPhase::Draft;
                    state.selected_operation_id = Some(operation_id);
                    // Input is no longer an unsent draft after an acknowledged
                    // outcome, including rejection. Its exact bytes remain in
                    // the receipt; another send requires a new confirmation.
                    state.input.clear();
                    self.native_command_prepared_request = None;
                    self.last_native_command_snapshot = None;
                }
            }
        }
        state.operations = operations;
        let needs_capabilities =
            was_pending && !matches!(state.phase, NativeCommandControlPhase::Pending { .. });
        let command = state.scope.as_ref().map(|s| s.command_id.clone());
        if needs_capabilities {
            self.observe_native_command_controls(command, false);
        }
    }

    fn launch_idle_command_actor(&mut self, owner: &str) -> bool {
        if self.native_manual_command_response.is_some() {
            return false;
        }
        // Failed actor initialization must respect the same polling interval
        // as a successful attempt instead of reading its metadata every frame.
        self.last_native_manual_command_poll = Some(Instant::now());
        let actor = match self.native_command_actor_for(owner) {
            Ok(actor) => actor,
            Err(error) => {
                let changed = self.view.status != error;
                self.view.status = error;
                return changed;
            }
        };
        let Some(sessions) = self.store.clone() else {
            return false;
        };
        let owner = owner.to_owned();
        let cancellation = self.native_manual_command_cancel.child_token();
        let (tx, rx) = mpsc::channel();
        self.native_manual_command_response = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let mut actor_error = None;
                match sessions.try_acquire_native_lease(&owner) {
                    Ok(lease) => {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(|e| e.to_string())?;
                        // An active runner/Resume owns the same actor and its
                        // Notify. A busy lease simply leaves the durable inbox
                        // for that owner; it never grants UI execution rights.
                        let outcome =
                            runtime.block_on(actor.process_pending(&lease, &cancellation));
                        actor_error = outcome.err().map(|error| error.to_string());
                    }
                    Err(openide_core::StoreError::NativeSessionBusy(_)) => {}
                    Err(error) => return Err(error.to_string()),
                }
                actor
                    .operations()
                    .list(&owner)
                    .map(|operations| (operations, actor_error))
                    .map_err(|e| e.to_string())
            })();
            let _ = tx.send((owner, result));
        });
        false
    }

    pub(super) fn poll_native_manual_commands(&mut self) -> bool {
        let mut changed = false;
        match self
            .native_command_control_response
            .as_ref()
            .map(Receiver::try_recv)
        {
            Some(Ok(observation)) => {
                self.native_command_control_response = None;
                let current_generation = self
                    .native_command_sessions
                    .manager(&observation.owner)
                    .as_ref()
                    .map(|m| m.generation().to_owned());
                if self.command_control_scope_current()
                    && self.view.selected_session.as_deref() == Some(&observation.owner)
                    && Path::new(&self.view.workspace).canonicalize().ok().as_ref()
                        == Some(&observation.workspace)
                    && current_generation == observation.generation
                {
                    match observation.control {
                        Ok(Some(control)) => {
                            let state = &mut self.view.native_command_control;
                            if state.scope.as_ref().is_none_or(|s| {
                                Some(&s.command_id) == observation.command_id.as_ref()
                            }) {
                                let observed = NativeCommandControlUiState::observed(
                                    NativeCommandControlScope {
                                        owner_session_id: observation.owner.clone(),
                                        command_id: control.id.clone(),
                                        workspace: control.workspace.clone(),
                                        manager_generation: control.generation.clone(),
                                    },
                                    capabilities(&control),
                                );
                                state.scope = observed.scope;
                                state.observed = observed.observed;
                                if state.columns_draft.is_empty() {
                                    state.columns_draft = control
                                        .dimensions
                                        .map(|s| s.columns.to_string())
                                        .unwrap_or_default();
                                }
                                if state.rows_draft.is_empty() {
                                    state.rows_draft = control
                                        .dimensions
                                        .map(|s| s.rows.to_string())
                                        .unwrap_or_default();
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(error) => self.view.native_command_control.error = Some(error),
                    }
                    match observation.operations {
                        Ok(operations) => self.apply_command_ledger(&observation.owner, operations),
                        Err(error) => self.view.native_command_control.error = Some(error),
                    }
                    changed = true;
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.native_command_control_response = None;
                self.view.native_command_control.error =
                    Some("La inspección se interrumpió".into());
                changed = true;
            }
            _ => {}
        }
        match self
            .native_manual_command_response
            .as_ref()
            .map(Receiver::try_recv)
        {
            Some(Ok((owner, result))) => {
                self.native_manual_command_response = None;
                match result {
                    Ok((operations, actor_error)) => {
                        self.apply_command_ledger(&owner, operations);
                        if self.view.selected_session.as_deref() == Some(owner.as_str()) {
                            self.view.native_command_control.error = actor_error;
                        }
                    }
                    Err(error) => {
                        if self.view.selected_session.as_deref() == Some(owner.as_str()) {
                            self.view.native_command_control.error = Some(error);
                        }
                    }
                }
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.native_manual_command_response = None;
                self.view.native_command_control.error = Some(
                    "El actor se interrumpió; inspeccioná el recibo antes de continuar".into(),
                );
                changed = true;
            }
            _ => {}
        }
        if self.native_manual_command_response.is_none()
            && self
                .last_native_manual_command_poll
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(500))
        {
            // Poll retained actors even if their view is dismissed or another
            // conversation is selected. Dismissal never cancels an intention.
            let owners = self
                .native_command_poll_owners
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            let next = owners
                .iter()
                .find(|owner| {
                    self.native_command_poll_cursor
                        .as_ref()
                        .is_none_or(|last| *owner > last)
                })
                .or_else(|| owners.first())
                .cloned();
            if let Some(owner) = next {
                self.native_command_poll_cursor = Some(owner.clone());
                changed |= self.launch_idle_command_actor(&owner);
            }
        }
        changed
    }

    pub(super) fn apply_native_command_control_action(&mut self, ui: &UiAction) -> bool {
        let id = match ui {
            UiAction::Clicked { id } | UiAction::InputChanged { id, .. } => id,
            _ => return false,
        };
        if !id.starts_with("native_command_control_")
            && !id.starts_with("native_command_operation_")
        {
            return false;
        }
        if id.starts_with("native_command_control_open:") {
            if let Ok((owner, command)) = serde_json::from_str::<(String, String)>(
                id.trim_start_matches("native_command_control_open:"),
            ) {
                if self.view.selected_session.as_deref() == Some(&owner) {
                    self.observe_native_command_controls(Some(command), true);
                }
            }
            return true;
        }
        if id == "native_command_operation_history" {
            self.observe_native_command_controls(None, true);
            return true;
        }
        if !self.command_control_scope_current() {
            return true;
        }
        if let UiAction::InputChanged { value, .. } = ui {
            match id.as_str() {
                "native_command_control_input"
                    if matches!(
                        self.view.native_command_control.phase,
                        NativeCommandControlPhase::Draft
                    ) =>
                {
                    self.view.native_command_control.input = value.clone()
                }
                "native_command_control_columns"
                    if matches!(
                        self.view.native_command_control.phase,
                        NativeCommandControlPhase::Draft
                    ) =>
                {
                    self.view.native_command_control.columns_draft = value.clone()
                }
                "native_command_control_rows"
                    if matches!(
                        self.view.native_command_control.phase,
                        NativeCommandControlPhase::Draft
                    ) =>
                {
                    self.view.native_command_control.rows_draft = value.clone()
                }
                "native_command_operation_acknowledgment" => {
                    self.view.native_command_control.acknowledgment = value.clone()
                }
                _ => {}
            }
            return true;
        }
        let result: Result<(), String> = (|| {
            match id.as_str() {
                "native_command_control_dismiss" => self.view.native_command_control.open = false,
                "native_command_control_refresh" => {
                    let command = self
                        .view
                        .native_command_control
                        .scope
                        .as_ref()
                        .map(|s| s.command_id.clone());
                    self.observe_native_command_controls(command, false);
                }
                "native_command_control_back" => {
                    if matches!(
                        self.view.native_command_control.phase,
                        NativeCommandControlPhase::Confirm { .. }
                    ) {
                        self.view.native_command_control.phase = NativeCommandControlPhase::Draft;
                        self.native_command_prepared_request = None;
                    }
                }
                "native_command_control_prepare_input"
                | "native_command_control_prepare_eof"
                | "native_command_control_prepare_terminate"
                | "native_command_control_prepare_resize" => {
                    let kind = match id.as_str() {
                        "native_command_control_prepare_input" => {
                            NativeCommandControlKind::SendInput
                        }
                        "native_command_control_prepare_eof" => {
                            NativeCommandControlKind::CloseInput
                        }
                        "native_command_control_prepare_terminate" => {
                            NativeCommandControlKind::Terminate
                        }
                        _ => NativeCommandControlKind::Resize,
                    };
                    self.view.native_command_control.prepare(kind)?;
                    let NativeCommandControlPhase::Confirm { prepared } =
                        &self.view.native_command_control.phase
                    else {
                        return Ok(());
                    };
                    let prepared = prepared.clone();
                    let actor = self.native_command_actor_for(&prepared.scope.owner_session_id)?;
                    let request = actor
                        .prepare_request(&prepared.scope.command_id, action(&prepared.request))
                        .map_err(|e| e.to_string())?;
                    if request.manager_generation != prepared.scope.manager_generation
                        || request.workspace != prepared.scope.workspace
                    {
                        return Err(
                            "El gestor cambió; actualizá la inspección antes de preparar".into(),
                        );
                    }
                    self.native_command_prepared_request = Some(request);
                }
                "native_command_control_confirm" => {
                    if !self.view.native_command_control.can_confirm() {
                        return Err("La confirmación ya no es válida".into());
                    }
                    let NativeCommandControlPhase::Confirm { prepared } =
                        &self.view.native_command_control.phase
                    else {
                        return Ok(());
                    };
                    let prepared = prepared.clone();
                    let request = self
                        .native_command_prepared_request
                        .clone()
                        .ok_or("Falta la preparación exacta de la política")?;
                    if request.session_id != prepared.scope.owner_session_id
                        || request.command_id != prepared.scope.command_id
                        || request.manager_generation != prepared.scope.manager_generation
                        || request.workspace != prepared.scope.workspace
                        || request.action != action(&prepared.request)
                    {
                        return Err("Los argumentos preparados cambiaron".into());
                    }
                    let actor = self.native_command_actor_for(&request.session_id)?;
                    let saved = actor
                        .enqueue_confirmed(request)
                        .map_err(|e| e.to_string())?;
                    self.native_command_poll_owners
                        .insert(saved.request.session_id.clone());
                    self.view.native_command_control.phase = NativeCommandControlPhase::Pending {
                        prepared,
                        operation_id: saved.request.operation_id.clone(),
                    };
                    self.view
                        .native_command_control
                        .operations
                        .push(saved.clone());
                    self.native_command_prepared_request = None;
                    self.launch_idle_command_actor(&saved.request.session_id);
                }
                "native_command_operation_resolve" => {
                    if !self.view.native_command_control.can_resolve_operation() {
                        return Err("Inspeccioná el recibo y describí tu verificación".into());
                    }
                    let operation = self
                        .view
                        .native_command_control
                        .selected_operation()
                        .cloned()
                        .ok_or("Recibo no disponible")?;
                    let sessions = self.store.clone().ok_or("Store no disponible")?;
                    let lease = sessions
                        .try_acquire_native_lease(&operation.request.session_id)
                        .map_err(|e| e.to_string())?;
                    NativeCommandOperationStore::new(sessions)
                        .resolve_started(
                            &lease,
                            &operation.request.operation_id,
                            operation.revision,
                            &self.view.native_command_control.acknowledgment,
                        )
                        .map_err(|e| e.to_string())?;
                    self.observe_native_command_controls(
                        self.view
                            .native_command_control
                            .scope
                            .as_ref()
                            .map(|s| s.command_id.clone()),
                        false,
                    );
                }
                _ if id.starts_with("native_command_operation_select:") => {
                    let selected = id.trim_start_matches("native_command_operation_select:");
                    if self
                        .view
                        .native_command_control
                        .operations
                        .iter()
                        .any(|op| op.request.operation_id == selected)
                    {
                        self.view.native_command_control.selected_operation_id =
                            Some(selected.into());
                        self.view.native_command_control.acknowledgment.clear();
                    }
                }
                _ if id.starts_with("native_command_control_cancel:") => {
                    let selected = id.trim_start_matches("native_command_control_cancel:");
                    let state = &self.view.native_command_control;
                    let operation = state
                        .operations
                        .iter()
                        .find(|op| op.request.operation_id == selected)
                        .cloned()
                        .ok_or("Recibo no disponible")?;
                    let sessions = self.store.clone().ok_or("Store no disponible")?;
                    NativeCommandOperationStore::new(sessions)
                        .cancel_queued_requested(
                            &operation.request.session_id,
                            selected,
                            operation.revision,
                            "human cancelled the queued intention before any external effect",
                        )
                        .map_err(|e| e.to_string())?;
                    self.observe_native_command_controls(
                        state.scope.as_ref().map(|s| s.command_id.clone()),
                        false,
                    );
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.view.native_command_control.error = Some(error);
            if id.contains("prepare_") {
                self.view.native_command_control.phase = NativeCommandControlPhase::Draft;
                self.native_command_prepared_request = None;
            }
        }
        true
    }
}
