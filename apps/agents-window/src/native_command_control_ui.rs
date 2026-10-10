//! Presentation and preparation for explicitly confirmed process controls.
//! Rendering never calls a manager or dispatches an effect.

use crate::{
    theme,
    view::{MainPage, SideTab, ViewState},
};
use openide_core::{
    NativeCommandOperationAction, NativeCommandOperationOutcome, NativeCommandOperationPhase,
    NativeCommandOperationSnapshot, NativeCommandOperationState,
};
use openide_harness::PersistentCommandExit;
use openide_ui::{
    Button, ButtonGroup, ButtonVariant, Component, ControlSize, Element, FontFamily, Marker, Modal,
    TextArea, TextField,
};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeCommandControlScope {
    pub owner_session_id: String,
    pub command_id: String,
    pub workspace: PathBuf,
    /// UUID of the actual process-local manager, never the command ID.
    pub manager_generation: String,
}

/// A durable ledger can be inspected after its process-local manager is gone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeCommandOperationHistoryScope {
    pub owner_session_id: String,
    pub workspace: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum NativeCommandInputState {
    Open,
    Closed,
    Busy,
    #[default]
    Uncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeCommandTerminalSize {
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeCommandControlCapabilities {
    pub stdin: NativeCommandInputState,
    pub exit: Option<PersistentCommandExit>,
    pub revoked: bool,
    pub stop_requested: bool,
    pub pty: bool,
    pub terminal_size: Option<NativeCommandTerminalSize>,
    pub dimensions_uncertain: bool,
    pub resize_supported: bool,
    pub max_stdin_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NativeCommandControlRequest {
    SendInput { input: String },
    CloseInput,
    Terminate,
    Resize { columns: u16, rows: u16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeCommandControlKind {
    SendInput,
    CloseInput,
    Terminate,
    Resize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedNativeCommandControl {
    pub scope: NativeCommandControlScope,
    pub request: NativeCommandControlRequest,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum NativeCommandControlPhase {
    #[default]
    Draft,
    Confirm {
        prepared: PreparedNativeCommandControl,
    },
    Pending {
        prepared: PreparedNativeCommandControl,
        operation_id: String,
    },
    OutcomeUncertain {
        prepared: PreparedNativeCommandControl,
        operation_id: String,
        detail: String,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeCommandControlUiState {
    /// Dismissing the view never discards an in-flight operation or cancels it.
    pub open: bool,
    pub scope: Option<NativeCommandControlScope>,
    pub history_scope: Option<NativeCommandOperationHistoryScope>,
    pub observed: Option<NativeCommandControlCapabilities>,
    pub input: String,
    pub columns_draft: String,
    pub rows_draft: String,
    pub phase: NativeCommandControlPhase,
    pub error: Option<String>,
    pub operations: Vec<NativeCommandOperationSnapshot>,
    pub selected_operation_id: Option<String>,
    /// Actual actor observation. A Started receipt alone is not live evidence.
    pub live_operation_id: Option<String>,
    pub acknowledgment: String,
    /// The host accepted a cancellation request; this is not a terminal ACK.
    pub cancellation_requested: bool,
}

impl NativeCommandControlUiState {
    pub(crate) fn observed(
        scope: NativeCommandControlScope,
        capabilities: NativeCommandControlCapabilities,
    ) -> Self {
        let columns_draft = capabilities
            .terminal_size
            .map(|size| size.columns.to_string())
            .unwrap_or_default();
        let rows_draft = capabilities
            .terminal_size
            .map(|size| size.rows.to_string())
            .unwrap_or_default();
        Self {
            open: true,
            history_scope: Some(NativeCommandOperationHistoryScope {
                owner_session_id: scope.owner_session_id.clone(),
                workspace: scope.workspace.clone(),
            }),
            scope: Some(scope),
            observed: Some(capabilities),
            columns_draft,
            rows_draft,
            ..Default::default()
        }
    }

    pub(crate) fn matches_scope(&self, owner: Option<&str>, workspace: &Path) -> bool {
        let workspace = workspace.canonicalize().ok();
        let live = self
            .scope
            .as_ref()
            .map(|scope| (scope.owner_session_id.as_str(), &scope.workspace));
        let history = self
            .history_scope
            .as_ref()
            .map(|scope| (scope.owner_session_id.as_str(), &scope.workspace));
        (live.is_some() || history.is_some())
            && [live, history]
                .into_iter()
                .flatten()
                .all(|(scope_owner, scope_workspace)| {
                    owner == Some(scope_owner) && workspace.as_ref() == Some(scope_workspace)
                })
    }

    pub(crate) fn selected_operation(&self) -> Option<&NativeCommandOperationSnapshot> {
        let scope = self.history_scope.as_ref()?;
        self.operations.iter().find(|operation| {
            self.selected_operation_id.as_deref() == Some(operation.request.operation_id.as_str())
                && operation.request.session_id == scope.owner_session_id
                && operation.request.workspace == scope.workspace
        })
    }

    pub(crate) fn can_resolve_operation(&self) -> bool {
        self.selected_operation().is_some_and(|operation| {
            operation.state.requires_verification()
                && self.live_operation_id.as_deref()
                    != Some(operation.request.operation_id.as_str())
        }) && !self.acknowledgment.trim().is_empty()
    }

    pub(crate) fn prepare(&mut self, kind: NativeCommandControlKind) -> Result<(), String> {
        self.prepare_on_platform(kind, cfg!(windows))
    }

    fn prepare_on_platform(
        &mut self,
        kind: NativeCommandControlKind,
        windows: bool,
    ) -> Result<(), String> {
        if !matches!(self.phase, NativeCommandControlPhase::Draft) {
            return Err("Hay una operación preparada o pendiente; no se prepara otra.".into());
        }
        let scope = self.scope.as_ref().ok_or("Falta el ámbito del comando.")?;
        if scope.owner_session_id.is_empty()
            || scope.command_id.is_empty()
            || uuid::Uuid::parse_str(&scope.manager_generation).is_err()
        {
            return Err("La identidad del comando o del manager no es válida.".into());
        }
        let request = match kind {
            NativeCommandControlKind::SendInput => NativeCommandControlRequest::SendInput {
                input: self.input.clone(),
            },
            NativeCommandControlKind::CloseInput => NativeCommandControlRequest::CloseInput,
            NativeCommandControlKind::Terminate => NativeCommandControlRequest::Terminate,
            NativeCommandControlKind::Resize => NativeCommandControlRequest::Resize {
                columns: self
                    .columns_draft
                    .parse()
                    .map_err(|_| "Columnas inválidas: usa un entero entre 20 y 500.")?,
                rows: self
                    .rows_draft
                    .parse()
                    .map_err(|_| "Filas inválidas: usa un entero entre 5 y 200.")?,
            },
        };
        validate_request(self.observed.as_ref(), &request, windows)?;
        self.phase = NativeCommandControlPhase::Confirm {
            prepared: PreparedNativeCommandControl {
                scope: scope.clone(),
                request,
            },
        };
        self.error = None;
        Ok(())
    }

    pub(crate) fn can_confirm(&self) -> bool {
        self.can_confirm_on_platform(cfg!(windows))
    }

    fn can_confirm_on_platform(&self, windows: bool) -> bool {
        let NativeCommandControlPhase::Confirm { prepared } = &self.phase else {
            return false;
        };
        self.scope.as_ref() == Some(&prepared.scope)
            && validate_request(self.observed.as_ref(), &prepared.request, windows).is_ok()
    }
}

fn validate_request(
    observed: Option<&NativeCommandControlCapabilities>,
    request: &NativeCommandControlRequest,
    windows: bool,
) -> Result<(), String> {
    let observed = observed.ok_or("Todavía no se observó el estado del comando.")?;
    if observed.revoked {
        return Err("El manager fue revocado; no se envían controles.".into());
    }
    if observed.exit.is_some() {
        return Err("El proceso ya tiene un resultado terminal; no se envían controles.".into());
    }
    if observed.stop_requested {
        return Err("La terminación ya fue solicitada; espera su resultado.".into());
    }
    match request {
        NativeCommandControlRequest::SendInput { input } => {
            if observed.stdin != NativeCommandInputState::Open {
                return Err("La entrada no está abierta y disponible.".into());
            }
            if input.is_empty() {
                return Err("Escribe los bytes UTF-8 que quieres enviar.".into());
            }
            if input.len() > observed.max_stdin_bytes {
                return Err(format!(
                    "La entrada tiene {} bytes; el límite observado es {}.",
                    input.len(),
                    observed.max_stdin_bytes
                ));
            }
        }
        NativeCommandControlRequest::CloseInput => {
            if observed.stdin != NativeCommandInputState::Open {
                return Err("La entrada no está abierta y disponible.".into());
            }
        }
        NativeCommandControlRequest::Terminate => {}
        NativeCommandControlRequest::Resize { columns, rows } => {
            if !windows
                || !observed.pty
                || !observed.resize_supported
                || observed.terminal_size.is_none()
                || observed.dimensions_uncertain
            {
                return Err(
                    "No hay una PTY Windows con dimensiones y resize observados disponibles."
                        .into(),
                );
            }
            if !(20..=500).contains(columns) || !(5..=200).contains(rows) {
                return Err(
                    "Dimensiones fuera del contrato: columnas 20–500 y filas 5–200.".into(),
                );
            }
        }
    }
    Ok(())
}

fn request_rows(prepared: &PreparedNativeCommandControl) -> Vec<Element> {
    let scope = &prepared.scope;
    let mut rows = vec![
        Element::text(format!(
            "Conversación: {}\nComando: {}\nManager: {}\nProyecto: {}",
            scope.owner_session_id,
            scope.command_id,
            scope.manager_generation,
            scope.workspace.display()
        ))
        .font_family(FontFamily::Monospace)
        .font_size(12.0)
        .foreground(theme::FOREGROUND),
    ];
    let text = match &prepared.request {
        NativeCommandControlRequest::SendInput { input } => {
            rows.push(note(&format!("Entrada UTF-8 completa · {} bytes. No se añade ningún salto de línea. JSON hace visibles los caracteres de control.", input.len())));
            serde_json::to_string(input).expect("UTF-8 input serializes")
        }
        NativeCommandControlRequest::CloseInput => "Cerrar entrada (EOF). No se envían bytes ni se añade un salto de línea. El cierre impide enviar más entrada a este comando.".into(),
        NativeCommandControlRequest::Terminate => "Solicitar terminación de este comando. La aplicación esperará el resultado; cerrar esta vista no cancela la solicitud.".into(),
        NativeCommandControlRequest::Resize { columns, rows } => format!("Redimensionar la PTY Windows: {columns} columnas × {rows} filas."),
    };
    rows.push(
        Element::text(text)
            .font_family(FontFamily::Monospace)
            .font_size(12.0)
            .foreground(theme::FOREGROUND),
    );
    rows
}

fn note(value: &str) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(11.0)
        .foreground(theme::SETTINGS_FOREGROUND_SUBTLE)
}

fn phase_label(phase: NativeCommandOperationPhase) -> &'static str {
    match phase {
        NativeCommandOperationPhase::BeforePreHooks => "Antes de los hooks previos",
        NativeCommandOperationPhase::BeforeCommand => "Antes del efecto del comando",
        NativeCommandOperationPhase::BeforePostHooks => "Antes de los hooks posteriores",
    }
}

fn state_label(state: &NativeCommandOperationState) -> &'static str {
    match state {
        NativeCommandOperationState::Requested => {
            "Solicitud registrada; efecto todavía no autorizado"
        }
        NativeCommandOperationState::Started { .. } => "Iniciada; requiere verificación",
        NativeCommandOperationState::Finished { outcome } => match outcome {
            NativeCommandOperationOutcome::Succeeded { .. } => {
                "Finalizada con resultado confirmado"
            }
            NativeCommandOperationOutcome::Failed { .. } => "Finalizada con fallo confirmado",
            NativeCommandOperationOutcome::EffectUncertain { .. } => {
                "Efecto incierto; requiere verificación"
            }
        },
        NativeCommandOperationState::Rejected { .. } => "Rechazada antes del efecto",
        NativeCommandOperationState::Cancelled { .. } => "Cancelada antes del efecto",
        NativeCommandOperationState::Resolved { .. } => "Verificación registrada por una persona",
    }
}

fn operation_state_label(
    local: &NativeCommandControlUiState,
    operation: &NativeCommandOperationSnapshot,
) -> &'static str {
    if matches!(operation.state, NativeCommandOperationState::Started { .. })
        && local.live_operation_id.as_deref() == Some(operation.request.operation_id.as_str())
    {
        "En curso; esperando el recibo del actor"
    } else {
        state_label(&operation.state)
    }
}

fn outcome_rows(title: &str, outcome: &NativeCommandOperationOutcome) -> Vec<Element> {
    let (label, diagnostic) = match outcome {
        NativeCommandOperationOutcome::Succeeded { diagnostic } => ("Confirmado", diagnostic),
        NativeCommandOperationOutcome::Failed { diagnostic } => (
            "Fallo confirmado; no implica reversión del proceso",
            diagnostic,
        ),
        NativeCommandOperationOutcome::EffectUncertain { diagnostic } => {
            ("Efecto incierto", diagnostic)
        }
    };
    vec![
        note(&format!("{title}: {label}")),
        exact_diagnostic(diagnostic),
    ]
}

fn exact_diagnostic(value: &str) -> Element {
    Element::text(value)
        .font_family(FontFamily::Monospace)
        .font_size(12.0)
        .foreground(theme::FOREGROUND)
}

fn observed_time(milliseconds: u64) -> String {
    i64::try_from(milliseconds)
        .ok()
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S%.3f %:z")
                .to_string()
        })
        .unwrap_or_else(|| "Fecha fuera de rango".into())
}

fn history_rows(local: &NativeCommandControlUiState) -> Vec<Element> {
    let Some(scope) = &local.history_scope else {
        return Vec::new();
    };
    let mut rows = vec![
        Element::text("Operaciones manuales registradas")
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::FOREGROUND),
        note(
            "Origen: acción explícita de una persona. Estas operaciones no fabrican eventos del modelo ni reanudan procesos al abrir el historial.",
        ),
    ];
    let operations: Vec<_> = local
        .operations
        .iter()
        .filter(|operation| {
            operation.request.session_id == scope.owner_session_id
                && operation.request.workspace == scope.workspace
        })
        .collect();
    if operations.is_empty() {
        rows.push(note("No hay operaciones registradas en esta conversación."));
    }
    for operation in operations {
        rows.push(
            Element::column(vec![
                note(&format!(
                    "Comando: {}\n{}",
                    operation.request.command_id,
                    operation_state_label(local, operation)
                )),
                Button::new(
                    &format!(
                        "native_command_operation_select:{}",
                        operation.request.operation_id
                    ),
                    "Ver operación registrada",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
            ])
            .gap(6.0),
        );
    }
    let Some(operation) = local.selected_operation() else {
        return rows;
    };
    let request = &operation.request;
    rows.push(exact_diagnostic(&format!("Operación: {}\nConversación: {}\nComando: {}\nManager original: {}\nProyecto: {}\nRevisión: {}\nPolítica: {}\nHooks: {}", request.operation_id, request.session_id, request.command_id, request.manager_generation, request.workspace.display(), operation.revision, request.policy_digest, request.hooks_digest)));
    rows.push(note(&format!(
        "Solicitud registrada: {}\nÚltima actualización: {}",
        observed_time(operation.requested_at_ms),
        observed_time(operation.updated_at_ms)
    )));
    match &request.action {
        NativeCommandOperationAction::Stdin { input, close_after } => {
            rows.push(note(&format!("Entrada UTF-8 registrada · {} bytes · cerrar después: {}. JSON hace visibles los caracteres de control; no se añade ningún salto de línea.", input.len(), if *close_after { "sí" } else { "no" })));
            rows.push(exact_diagnostic(
                &serde_json::to_string(input).expect("UTF-8 serializes"),
            ));
        }
        NativeCommandOperationAction::Resize {
            columns,
            rows: terminal_rows,
        } => rows.push(note(&format!(
            "Redimensionar PTY: {columns} columnas × {terminal_rows} filas."
        ))),
        NativeCommandOperationAction::Terminate => {
            rows.push(note("Solicitud registrada: terminar el comando."))
        }
    }
    rows.push(note(&format!(
        "Estado: {}",
        operation_state_label(local, operation)
    )));
    match &operation.state {
        NativeCommandOperationState::Requested => rows.push(note(
            "La intención durable por sí sola no confirma ni autoriza repetir un efecto.",
        )),
        NativeCommandOperationState::Started { phase, diagnostic } => {
            rows.push(note(&format!(
                "Última fase registrada: {}",
                phase_label(*phase)
            )));
            rows.push(exact_diagnostic(diagnostic));
        }
        NativeCommandOperationState::Finished { outcome } => {
            rows.extend(outcome_rows("Resultado final registrado", outcome))
        }
        NativeCommandOperationState::Rejected { reason }
        | NativeCommandOperationState::Cancelled { reason } => rows.push(exact_diagnostic(reason)),
        NativeCommandOperationState::Resolved { acknowledgment } => {
            rows.push(note("Esta nota reconoce una verificación humana. No transforma un efecto incierto en éxito."));
            rows.push(exact_diagnostic(acknowledgment));
        }
    }
    if let Some(outcome) = &operation.effect_result {
        rows.extend(outcome_rows(
            "Recibo del efecto antes de los hooks posteriores",
            outcome,
        ));
    } else {
        rows.push(note("No hay recibo confirmado del efecto del comando."));
    }
    for diagnostic in operation.phase_diagnostics() {
        rows.push(note(&format!(
            "Fase · revisión {} · {}\nRegistrada: {}",
            diagnostic.revision,
            phase_label(diagnostic.phase),
            observed_time(diagnostic.at_ms)
        )));
        rows.push(exact_diagnostic(&diagnostic.diagnostic));
    }
    if operation.state.requires_verification() {
        let live =
            local.live_operation_id.as_deref() == Some(operation.request.operation_id.as_str());
        rows.push(note(if live {
            "El actor sigue en curso. Espera su recibo antes de registrar una verificación; todavía no hay un resultado terminal."
        } else {
            "Inspecciona el estado real antes de registrar tu verificación. Resolver esta advertencia no envía bytes, termina procesos ni repite la operación."
        }));
        rows.push(
            TextArea::new(
                "native_command_operation_acknowledgment",
                &local.acknowledgment,
                "Describe qué verificaste",
                theme::component_palette(),
            )
            .height(80.0)
            .enabled(!live)
            .render(),
        );
    }
    rows
}

pub(crate) fn modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    modal_on_platform(state, base, viewport_width, cfg!(windows))
}

fn modal_on_platform(
    state: &ViewState,
    base: Element,
    viewport_width: f32,
    windows: bool,
) -> Element {
    let local = &state.native_command_control;
    if !local.open
        || state.show_settings
        || state.main_page != MainPage::Workspace
        || state.selected_backend != "native"
        || !state.show_side_pane
        || state.side_tab != SideTab::Activity
        || !local.matches_scope(
            state.selected_session.as_deref(),
            Path::new(&state.workspace),
        )
    {
        return base;
    }
    let width = (viewport_width - 24.0).min(820.0);
    let body_width = (width - 54.0).max(0.0);
    let allowed = state.native_tool_permissions.run_command;
    let close = Button::new(
        "native_command_control_dismiss",
        "Cerrar vista",
        theme::component_palette(),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .render();
    let mut body = Vec::new();
    let mut footer = vec![
        close,
        Button::new(
            "native_command_control_refresh",
            "Actualizar estado",
            theme::component_palette(),
        )
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .render(),
    ];
    let mut initial_focus = "native_command_control_dismiss";
    if let Some(scope) = &local.scope {
        body.push(
            Button::new(
                &format!(
                    "native_command_history_open:{}",
                    serde_json::to_string(&(
                        &scope.owner_session_id,
                        &scope.manager_generation,
                        &scope.command_id
                    ))
                    .expect("command history identity serializes")
                ),
                "Ver salida guardada",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
        );
    }
    match &local.phase {
        NativeCommandControlPhase::Draft => {
            if let Some(scope) = &local.scope {
                body.push(
                    Element::text(format!(
                        "Conversación: {}\nComando: {}\nManager: {}\nProyecto: {}",
                        scope.owner_session_id,
                        scope.command_id,
                        scope.manager_generation,
                        scope.workspace.display()
                    ))
                    .font_family(FontFamily::Monospace)
                    .font_size(12.0)
                    .foreground(theme::FOREGROUND),
                );
                if !allowed {
                    body.push(note(
                        "El permiso de ejecutar comandos está desactivado. No se envían controles.",
                    ));
                }
                if let Some(observed) = &local.observed {
                    let stdin = match observed.stdin {
                        NativeCommandInputState::Open => "abierta",
                        NativeCommandInputState::Closed => "cerrada",
                        NativeCommandInputState::Busy => "ocupada",
                        NativeCommandInputState::Uncertain => "estado incierto",
                    };
                    body.push(note(&format!(
                        "Entrada: {stdin} · límite {} bytes · {}",
                        observed.max_stdin_bytes,
                        if observed.pty {
                            "PTY observada"
                        } else {
                            "Tuberías observadas"
                        }
                    )));
                    if observed.revoked {
                        body.push(note("El manager fue revocado."));
                    }
                    if observed.stop_requested {
                        body.push(note("La terminación ya fue solicitada."));
                    }
                    if let Some(exit) = &observed.exit {
                        let status = match exit {
                            PersistentCommandExit::Exited { code } => format!(
                                "Finalizado · código {}",
                                code.map_or_else(|| "?".into(), |code| code.to_string())
                            ),
                            PersistentCommandExit::Terminated => "Terminado".into(),
                            PersistentCommandExit::TimedOut => "Tiempo agotado".into(),
                            PersistentCommandExit::WaitFailed(error) => {
                                format!("Estado incierto al esperar proceso: {error}")
                            }
                        };
                        body.push(note(&status));
                    }
                } else {
                    body.push(note("Consultando el estado del comando. No se envían efectos durante esta consulta."));
                }
                let stdin_editable = allowed
                    && local.observed.as_ref().is_some_and(|observed| {
                        observed.stdin == NativeCommandInputState::Open
                            && !observed.revoked
                            && !observed.stop_requested
                            && observed.exit.is_none()
                    });
                body.push(note("Entrada UTF-8 exacta. Un salto de línea sólo se envía si lo escribes. EOF se confirma por separado."));
                body.push(
                    TextArea::new(
                        "native_command_control_input",
                        &local.input,
                        "Bytes UTF-8 para el proceso",
                        theme::component_palette(),
                    )
                    .height(96.0)
                    .preserve_line_endings(true)
                    .width(body_width)
                    .enabled(stdin_editable)
                    .render(),
                );
                body.push(note(&format!(
                    "Borrador: {} bytes UTF-8",
                    local.input.len()
                )));
                if stdin_editable {
                    initial_focus = "native_command_control_input";
                }
                let can_input = allowed
                    && validate_request(
                        local.observed.as_ref(),
                        &NativeCommandControlRequest::SendInput {
                            input: local.input.clone(),
                        },
                        windows,
                    )
                    .is_ok();
                let can_eof = allowed
                    && validate_request(
                        local.observed.as_ref(),
                        &NativeCommandControlRequest::CloseInput,
                        windows,
                    )
                    .is_ok();
                let can_terminate = allowed
                    && validate_request(
                        local.observed.as_ref(),
                        &NativeCommandControlRequest::Terminate,
                        windows,
                    )
                    .is_ok();
                let review_actions = vec![
                    Button::new(
                        "native_command_control_prepare_input",
                        "Revisar entrada",
                        theme::component_palette(),
                    )
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Compact)
                    .enabled(can_input)
                    .render(),
                    Button::new(
                        "native_command_control_prepare_eof",
                        "Revisar cierre de entrada (EOF)",
                        theme::component_palette(),
                    )
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Compact)
                    .enabled(can_eof)
                    .render(),
                    Button::new(
                        "native_command_control_prepare_terminate",
                        "Revisar terminación",
                        theme::component_palette(),
                    )
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Compact)
                    .enabled(can_terminate)
                    .render(),
                ];
                body.push(
                    ButtonGroup::new(review_actions)
                        .stacked(body_width < 620.0)
                        .render(),
                );
                if let Some(observed) = local.observed.as_ref().filter(|observed| {
                    windows
                        && observed.pty
                        && observed.resize_supported
                        && observed.terminal_size.is_some()
                }) {
                    let dimensions = observed.terminal_size.expect("filtered");
                    body.push(note(&format!(
                        "PTY Windows observada: {} columnas × {} filas{}",
                        dimensions.columns,
                        dimensions.rows,
                        if observed.dimensions_uncertain {
                            " · dimensiones ahora inciertas"
                        } else {
                            ""
                        }
                    )));
                    let editable = allowed
                        && !observed.revoked
                        && !observed.stop_requested
                        && observed.exit.is_none()
                        && !observed.dimensions_uncertain;
                    body.push(
                        Element::column(vec![
                            note("Columnas (20–500)"),
                            TextField::new(
                                "native_command_control_columns",
                                &local.columns_draft,
                                "Columnas",
                                theme::component_palette(),
                            )
                            .size(ControlSize::Compact)
                            .enabled(editable)
                            .render(),
                            note("Filas (5–200)"),
                            TextField::new(
                                "native_command_control_rows",
                                &local.rows_draft,
                                "Filas",
                                theme::component_palette(),
                            )
                            .size(ControlSize::Compact)
                            .enabled(editable)
                            .render(),
                        ])
                        .gap(6.0),
                    );
                    let request = local
                        .columns_draft
                        .parse()
                        .ok()
                        .zip(local.rows_draft.parse().ok())
                        .map(|(columns, rows)| NativeCommandControlRequest::Resize {
                            columns,
                            rows,
                        });
                    body.push(
                        Button::new(
                            "native_command_control_prepare_resize",
                            "Revisar dimensiones",
                            theme::component_palette(),
                        )
                        .variant(ButtonVariant::Outline)
                        .size(ControlSize::Compact)
                        .enabled(
                            allowed
                                && request.as_ref().is_some_and(|request| {
                                    validate_request(local.observed.as_ref(), request, windows)
                                        .is_ok()
                                }),
                        )
                        .render(),
                    );
                }
            } else {
                body.push(note("Historial durable. No hay un manager de procesos observado; reabrir esta vista no recupera comandos ni habilita controles."));
            }
        }
        NativeCommandControlPhase::Confirm { prepared } => {
            body.extend(request_rows(prepared));
            body.push(note("Confirmar envía una única solicitud al servicio. La identidad y las capacidades se vuelven a comprobar antes del efecto."));
            let valid = allowed && local.can_confirm_on_platform(windows);
            if !valid {
                body.push(note("El ámbito, el estado o los permisos cambiaron. Esta confirmación ya no está disponible."));
            }
            footer.push(
                Button::new(
                    "native_command_control_back",
                    "Volver",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
            );
            footer.push(
                Button::new(
                    "native_command_control_confirm",
                    "Confirmar operación",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(valid)
                .render(),
            );
            initial_focus = "native_command_control_back";
        }
        NativeCommandControlPhase::Pending {
            prepared,
            operation_id,
        } => {
            body.extend(request_rows(prepared));
            body.push(
                Marker::new(
                    format!("command-control:{operation_id}"),
                    "Esperando confirmación de la operación",
                    theme::component_palette(),
                )
                .loading(true)
                .render(),
            );
            body.push(note(&format!("Operación: {operation_id}\nNo se envía otra operación mientras ésta está pendiente. Cerrar la vista no cancela el trabajo ni confirma su resultado.")));
            if local.cancellation_requested {
                body.push(note("Cancelación solicitada; esperando el resultado confirmado. El efecto todavía puede haber ocurrido."));
            }
            footer.push(
                Button::new(
                    &format!("native_command_control_cancel:{operation_id}"),
                    "Cancelar solicitud en cola",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(
                    !local.cancellation_requested
                        && local.operations.iter().any(|operation| {
                            operation.request.operation_id == *operation_id
                                && operation.request.session_id == prepared.scope.owner_session_id
                                && operation.request.workspace == prepared.scope.workspace
                                && operation.state == NativeCommandOperationState::Requested
                        }),
                )
                .render(),
            );
        }
        NativeCommandControlPhase::OutcomeUncertain {
            prepared,
            operation_id,
            detail,
        } => {
            body.extend(request_rows(prepared));
            body.push(
                Element::text(format!(
                    "Resultado incierto\nOperación: {operation_id}\n{detail}"
                ))
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::DESTRUCTIVE),
            );
            body.push(note("El efecto pudo ocurrir. No se reenviará automáticamente ni se declara cancelado. Reabrir la aplicación no recupera este proceso."));
        }
    }
    body.extend(history_rows(local));
    if local
        .selected_operation()
        .is_some_and(|operation| operation.state.requires_verification())
    {
        footer.push(
            Button::new(
                "native_command_operation_resolve",
                "Registrar verificación",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(local.can_resolve_operation())
            .render(),
        );
    }
    if let Some(error) = &local.error {
        body.push(
            Element::text(error)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::DESTRUCTIVE),
        );
    }
    let footer = if body_width < 400.0 || (footer.len() > 3 && body_width < 620.0) {
        Element::column(footer).gap(6.0)
    } else {
        Element::row(
            std::iter::once(Element::spacer().flex(1.0))
                .chain(footer)
                .collect(),
        )
        .gap(6.0)
    };
    Modal::new(
        base,
        true,
        match local.phase {
            NativeCommandControlPhase::Draft => "Control del comando",
            NativeCommandControlPhase::Confirm { .. } => "Confirmar operación",
            NativeCommandControlPhase::Pending { .. } => "Operación pendiente",
            NativeCommandControlPhase::OutcomeUncertain { .. } => "Resultado incierto",
        },
        "Entrada, cierre y terminación con confirmación explícita.",
        "native_command_control_dismiss",
        theme::component_palette(),
    )
    .width(width)
    .height(600.0)
    .body(Element::column(body).gap(10.0).size(Some(body_width), None))
    .footer(footer)
    .initial_focus(initial_focus)
    .return_focus(
        local
            .scope
            .as_ref()
            .map(|scope| {
                format!(
                    "native_command_control_open:{}",
                    serde_json::to_string(&(&scope.owner_session_id, &scope.command_id))
                        .expect("command scope serializes")
                )
            })
            .unwrap_or_else(|| "native_command_operation_history".into()),
    )
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};

    fn state() -> (ViewState, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let mut state = crate::view::tests::empty_state();
        state.workspace = root.path().display().to_string();
        state.main_page = MainPage::Workspace;
        state.show_settings = false;
        state.selected_backend = "native".into();
        state.side_tab = SideTab::Activity;
        state.show_side_pane = true;
        state.selected_session = Some("owner-session".into());
        state.native_tool_permissions.run_command = true;
        state.native_command_control = NativeCommandControlUiState::observed(
            NativeCommandControlScope {
                owner_session_id: "owner-session".into(),
                command_id: "opaque-command:exact-id".into(),
                workspace: root.path().canonicalize().unwrap(),
                manager_generation: "4cc4c4c1-4cb2-43d2-bf3a-ab6e6f855f48".into(),
            },
            NativeCommandControlCapabilities {
                stdin: NativeCommandInputState::Open,
                max_stdin_bytes: 8192,
                ..Default::default()
            },
        );
        (state, root)
    }

    fn prepared(state: &NativeCommandControlUiState) -> PreparedNativeCommandControl {
        let NativeCommandControlPhase::Confirm { prepared } = &state.phase else {
            panic!("confirmation expected");
        };
        prepared.clone()
    }

    fn click(tree: &mut UiTree, frame: &openide_ui::Frame, id: &str) {
        let rect = frame
            .regions
            .iter()
            .find(|region| region.id == id && region.kind == HitKind::Button)
            .unwrap_or_else(|| panic!("missing {id}"))
            .rect;
        let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        assert_eq!(frame.hit_test(point), Some(id));
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked { id: id.into() }]
        );
    }

    #[test]
    fn preparation_freezes_exact_utf8_scope_and_distinct_eof_without_effects() {
        let (state, _root) = state();
        let mut local = state.native_command_control;
        let exact = "  ñ🦀\r\nuna\rsegunda\t\0línea sin salto final  ";
        local.input = exact.into();
        local.prepare(NativeCommandControlKind::SendInput).unwrap();
        let first = prepared(&local);
        assert_eq!(
            first.request,
            NativeCommandControlRequest::SendInput {
                input: exact.into()
            }
        );
        assert_eq!(local.input.as_bytes(), exact.as_bytes());
        local.input = "un borrador nuevo".into();
        assert_eq!(prepared(&local), first);
        assert!(local.can_confirm());
        assert!(local.prepare(NativeCommandControlKind::Terminate).is_err());
        local.phase = NativeCommandControlPhase::Draft;
        local.prepare(NativeCommandControlKind::CloseInput).unwrap();
        assert_eq!(
            prepared(&local).request,
            NativeCommandControlRequest::CloseInput
        );
        assert_eq!(local.input, "un borrador nuevo");
    }

    #[test]
    fn preparation_rejects_missing_stale_closed_busy_and_oversized_input() {
        let (state, _root) = state();
        let original = state.native_command_control;
        for stdin in [
            NativeCommandInputState::Closed,
            NativeCommandInputState::Busy,
            NativeCommandInputState::Uncertain,
        ] {
            for kind in [
                NativeCommandControlKind::SendInput,
                NativeCommandControlKind::CloseInput,
            ] {
                let mut local = original.clone();
                local.input = "ñ".into();
                local.observed.as_mut().unwrap().stdin = stdin;
                assert!(local.prepare(kind).is_err());
                assert_eq!(local.phase, NativeCommandControlPhase::Draft);
            }
        }
        let mut local = original.clone();
        assert!(local.prepare(NativeCommandControlKind::SendInput).is_err());
        local.input = "🦀".into();
        local.observed.as_mut().unwrap().max_stdin_bytes = 3;
        assert!(
            local
                .prepare(NativeCommandControlKind::SendInput)
                .unwrap_err()
                .contains("4 bytes")
        );
        local.observed = None;
        assert!(local.prepare(NativeCommandControlKind::Terminate).is_err());
        for status in [
            PersistentCommandExit::Exited { code: Some(0) },
            PersistentCommandExit::WaitFailed("fallo real".into()),
        ] {
            local = original.clone();
            local.observed.as_mut().unwrap().exit = Some(status);
            assert!(local.prepare(NativeCommandControlKind::Terminate).is_err());
        }
        local = original;
        local.input = "exacto".into();
        local.prepare(NativeCommandControlKind::SendInput).unwrap();
        local.scope.as_mut().unwrap().manager_generation = uuid::Uuid::new_v4().to_string();
        assert!(!local.can_confirm());
    }

    #[test]
    fn draft_editor_keeps_unicode_and_explicit_lf_without_submitting_on_enter() {
        let (state, _root) = state();
        let mut tree = UiTree::new(modal(&state, Element::button("behind", "Detrás"), 360.0));
        tree.set_reduced_motion(true);
        tree.layout(Size::new(360.0, 720.0));
        assert_eq!(tree.focused_id(), Some("native_command_control_input"));
        let exact = "  ñ🦀\nsegunda línea  ";
        assert_eq!(
            tree.handle_event(UiEvent::Text(exact.into())),
            vec![UiAction::InputChanged {
                id: "native_command_control_input".into(),
                value: exact.into()
            }]
        );
        assert_eq!(
            tree.input_value("native_command_control_input"),
            Some(exact)
        );
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::InputChanged {
                id: "native_command_control_input".into(),
                value: format!("{exact}\n")
            }]
        );
    }

    #[test]
    fn draft_control_paste_copy_cut_and_confirmation_keep_crlf_cr_and_utf8_bytes() {
        for width in [360.0, 900.0] {
            let (mut state, _root) = state();
            let exact = "  ñ🦀\r\nprimera\rsegunda\t\0";
            let mut tree = UiTree::new(modal(&state, Element::spacer(), width));
            tree.layout(Size::new(width, 720.0));
            assert!(tree.focus("native_command_control_input"));
            let actions = tree.handle_event(UiEvent::Text(exact.into()));
            assert_eq!(
                actions,
                vec![UiAction::InputChanged {
                    id: "native_command_control_input".into(),
                    value: exact.into()
                }]
            );
            state.native_command_control.input = exact.into();
            tree.set_root(modal(&state, Element::spacer(), width));
            tree.layout(Size::new(width, 720.0));
            assert_eq!(
                tree.input_value("native_command_control_input")
                    .unwrap()
                    .as_bytes(),
                exact.as_bytes()
            );
            tree.handle_event(UiEvent::Key {
                key: Key::A,
                shift: false,
                ctrl: true,
            });
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::C,
                    shift: false,
                    ctrl: true
                }),
                vec![UiAction::CopyRequested { text: exact.into() }]
            );
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::X,
                    shift: false,
                    ctrl: true
                }),
                vec![
                    UiAction::CopyRequested { text: exact.into() },
                    UiAction::InputChanged {
                        id: "native_command_control_input".into(),
                        value: String::new()
                    }
                ]
            );
            tree.handle_event(UiEvent::Text(exact.into()));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Backspace,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::InputChanged {
                    id: "native_command_control_input".into(),
                    value: exact.strip_suffix('\0').unwrap().into()
                }]
            );
            state
                .native_command_control
                .prepare(NativeCommandControlKind::SendInput)
                .unwrap();
            assert_eq!(
                prepared(&state.native_command_control).request,
                NativeCommandControlRequest::SendInput {
                    input: exact.into()
                }
            );
        }
    }

    #[test]
    fn pending_cancellation_request_remains_pending_until_a_terminal_receipt() {
        for width in [360.0, 900.0] {
            let (mut state, _root) = state();
            state.native_command_control.input = "exacta".into();
            state
                .native_command_control
                .prepare(NativeCommandControlKind::SendInput)
                .unwrap();
            let prepared = prepared(&state.native_command_control);
            let operation_id = uuid::Uuid::new_v4().to_string();
            state.native_command_control.operations = vec![serde_json::from_value(serde_json::json!({
                "request": {"operation_id":operation_id,"session_id":prepared.scope.owner_session_id,
                    "workspace":prepared.scope.workspace,"manager_generation":prepared.scope.manager_generation,
                    "command_id":prepared.scope.command_id,"action":{"type":"stdin","input":"exacta","close_after":false},
                    "policy_digest":"a".repeat(64),"hooks_digest":"b".repeat(64)},
                "revision":0,"state":{"type":"requested"},"effect_result":null,
                "requested_at_ms":1,"updated_at_ms":1,"changes":[]
            })).unwrap()];
            state.native_command_control.phase = NativeCommandControlPhase::Pending {
                prepared,
                operation_id: operation_id.clone(),
            };
            let phase = state.native_command_control.phase.clone();
            let mut tree = UiTree::new(modal(&state, Element::spacer(), width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 720.0));
            click(
                &mut tree,
                &frame,
                &format!("native_command_control_cancel:{operation_id}"),
            );
            assert_eq!(
                state.native_command_control.phase, phase,
                "Painting/actions have no ACK authority"
            );
            state.native_command_control.cancellation_requested = true;
            tree.set_root(modal(&state, Element::spacer(), width));
            tree.layout(Size::new(width, 720.0));
            assert!(!tree.focus(&format!("native_command_control_cancel:{operation_id}")));
            assert_eq!(state.native_command_control.phase, phase);
            assert!(tree.focus("native_command_control_refresh"));
            assert!(tree.focus("native_command_control_dismiss"));
        }
    }

    #[test]
    fn reopened_ledger_without_manager_keeps_exact_intention_receipts_and_manual_resolution() {
        use openide_core::{
            NativeCommandOperationRequest, NativeCommandOperationStore, SessionStore,
        };
        use std::sync::Arc;
        for width in [360.0, 900.0] {
            let (mut state, root) = state();
            let storage = root.path().join("store");
            let sessions = Arc::new(SessionStore::open(&storage).unwrap());
            let session = sessions.create(root.path(), "native").unwrap();
            let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
            let store = NativeCommandOperationStore::new(sessions.clone());
            let input = format!(
                "  {}CR\r\nCR\rTAB\tNUL\0🦀",
                "entrada exacta ñ\n".repeat(60)
            );
            let request = NativeCommandOperationRequest {
                operation_id: uuid::Uuid::new_v4().to_string(),
                session_id: session.id.clone(),
                workspace: root.path().canonicalize().unwrap(),
                manager_generation: uuid::Uuid::new_v4().to_string(),
                command_id: uuid::Uuid::new_v4().to_string(),
                action: NativeCommandOperationAction::Stdin {
                    input: input.clone(),
                    close_after: true,
                },
                policy_digest: "a".repeat(64),
                hooks_digest: "b".repeat(64),
            };
            store.enqueue_requested(request.clone()).unwrap();
            store
                .preflight_capacity(&lease, &request.operation_id, 1024 * 1024)
                .unwrap();
            store
                .begin_started(&lease, &request.operation_id, 0)
                .unwrap();
            let pre = format!(
                "{}FINAL-PRE-HOOK-ñ",
                "detalle previo sin recorte\n".repeat(80)
            );
            store
                .record_phase(
                    &lease,
                    &request.operation_id,
                    1,
                    NativeCommandOperationPhase::BeforeCommand,
                    &pre,
                )
                .unwrap();
            let effect = NativeCommandOperationOutcome::Succeeded {
                diagnostic: "Recibo de fixture: bytes y EOF observados antes del hook posterior"
                    .into(),
            };
            store
                .record_effect_result(&lease, &request.operation_id, 2, effect.clone())
                .unwrap();
            let post = format!(
                "{}FINAL-POST-HOOK-終",
                "detalle posterior sin recorte\n".repeat(80)
            );
            store
                .record_phase(
                    &lease,
                    &request.operation_id,
                    3,
                    NativeCommandOperationPhase::BeforePostHooks,
                    &post,
                )
                .unwrap();
            drop(store);
            let reopened =
                NativeCommandOperationStore::new(Arc::new(SessionStore::open(&storage).unwrap()));
            let snapshot = reopened
                .snapshot(&session.id, &request.operation_id)
                .unwrap()
                .unwrap();
            assert!(snapshot.state.requires_verification());
            state.selected_session = Some(session.id.clone());
            state.native_command_control = NativeCommandControlUiState {
                open: true,
                history_scope: Some(NativeCommandOperationHistoryScope {
                    owner_session_id: session.id.clone(),
                    workspace: request.workspace.clone(),
                }),
                operations: reopened.list(&session.id).unwrap(),
                selected_operation_id: Some(request.operation_id.clone()),
                ..Default::default()
            };
            assert!(!state.native_command_control.can_resolve_operation());
            let mut tree = UiTree::new(modal(&state, Element::button("behind", "Detrás"), width));
            tree.set_reduced_motion(true);
            let viewport = Size::new(width, 720.0);
            tree.layout(viewport);
            assert!(!tree.focus("native_command_control_input"));
            assert!(!tree.focus("native_command_control_confirm"));
            assert!(!tree.focus("native_command_operation_resolve"));
            let mut sources = std::collections::BTreeSet::new();
            let mut pre_seen = false;
            let mut post_seen = false;
            let selection = format!("native_command_operation_select:{}", request.operation_id);
            let mut selected = false;
            for _ in 0..60 {
                let frame = tree.layout(viewport);
                if !selected
                    && frame.regions.iter().any(|region| {
                        region.id == selection
                            && region.rect.y > 0.0
                            && region.rect.y + region.rect.height < viewport.height
                    })
                {
                    click(&mut tree, &frame, &selection);
                    selected = true;
                }
                for block in frame.text_layouts() {
                    sources.insert(block.source.clone());
                    pre_seen |= block
                        .runs
                        .iter()
                        .any(|run| run.text.contains("FINAL-PRE-HOOK") && run.visible.is_some());
                    post_seen |= block
                        .runs
                        .iter()
                        .any(|run| run.text.contains("FINAL-POST-HOOK") && run.visible.is_some());
                }
                let scroll = frame
                    .regions
                    .iter()
                    .find(|region| {
                        region.id == "modal_body:native_command_control_dismiss"
                            && region.kind == HitKind::Scroll
                    })
                    .unwrap()
                    .rect;
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(
                        scroll.x + scroll.width / 2.0,
                        scroll.y + scroll.height / 2.0,
                    ),
                    delta_y: -300.0,
                });
            }
            assert!(
                selected && pre_seen && post_seen,
                "All complete diagnostics must be reachable at {width}"
            );
            assert!(sources.contains(&serde_json::to_string(&input).unwrap()));
            assert!(sources.contains(&pre) && sources.contains(&post));
            assert!(
                sources
                    .iter()
                    .any(|source| source.contains(&request.manager_generation)
                        && source.contains(&request.policy_digest)
                        && source.contains(&request.hooks_digest))
            );
            state.native_command_control.acknowledgment = "   \n".into();
            assert!(!state.native_command_control.can_resolve_operation());
            state.native_command_control.acknowledgment =
                "Verifiqué externamente el efecto y los hooks; no se repite la operación".into();
            state.native_command_control.live_operation_id = Some(request.operation_id.clone());
            assert!(!state.native_command_control.can_resolve_operation());
            // Inspect the live label at the top rather than at the retained
            // scroll position of the complete diagnostics inspected above.
            tree = UiTree::new(modal(&state, Element::spacer(), width));
            let live_frame = tree.layout(viewport);
            assert!(!tree.focus("native_command_operation_resolve"));
            assert!(!tree.focus("native_command_operation_acknowledgment"));
            assert!(live_frame.text_layouts().iter().any(|block| {
                block
                    .source
                    .contains("En curso; esperando el recibo del actor")
            }));
            state.native_command_control.live_operation_id = None;
            assert!(state.native_command_control.can_resolve_operation());
            tree.set_root(modal(&state, Element::spacer(), width));
            let frame = tree.layout(viewport);
            click(&mut tree, &frame, "native_command_operation_resolve");
            assert_eq!(
                reopened
                    .snapshot(&session.id, &request.operation_id)
                    .unwrap(),
                Some(snapshot.clone()),
                "UI action alone cannot resolve durable uncertainty"
            );
            let resolved = reopened
                .resolve_started(
                    &lease,
                    &request.operation_id,
                    snapshot.revision,
                    &state.native_command_control.acknowledgment,
                )
                .unwrap();
            assert_eq!(resolved.effect_result, Some(effect));
            state.native_command_control.operations = reopened.list(&session.id).unwrap();
            assert!(!state.native_command_control.can_resolve_operation());
            assert_eq!(
                state
                    .native_command_control
                    .selected_operation()
                    .unwrap()
                    .request,
                request
            );
            tree.set_root(modal(&state, Element::spacer(), width));
            tree.layout(viewport);
            assert!(!tree.focus("native_command_operation_resolve"));
            assert!(!tree.focus("native_command_control_prepare_input"));
            assert!(
                sessions
                    .load(&session.id)
                    .unwrap()
                    .unwrap()
                    .events
                    .is_empty()
            );
            state.workspace = root.path().join("different").display().to_string();
            tree.set_root(modal(&state, Element::spacer(), width));
            tree.layout(viewport);
            assert!(!tree.focus("native_command_control_dismiss"));
        }
    }

    #[test]
    fn confirmation_shows_complete_frozen_input_bytes_scope_and_reachable_actions() {
        for width in [360.0, 520.0, 900.0] {
            let (mut state, _root) = state();
            let exact = format!(
                "  {}FINAL-ENTRADA-ñ-終わり\r\n\0",
                "Unicode completo y conservado\n".repeat(110)
            );
            state.native_command_control.input = exact.clone();
            state
                .native_command_control
                .prepare(NativeCommandControlKind::SendInput)
                .unwrap();
            let frozen = prepared(&state.native_command_control);
            let mut tree = UiTree::new(modal(&state, Element::button("behind", "Detrás"), width));
            tree.set_reduced_motion(true);
            let viewport = Size::new(width, 720.0);
            let frame = tree.layout(viewport);
            assert_eq!(tree.focused_id(), Some("native_command_control_back"));
            for id in [
                "native_command_control_confirm",
                "native_command_control_back",
                "native_command_control_dismiss",
            ] {
                let rect = frame
                    .regions
                    .iter()
                    // The modal's full-screen dismiss surface shares this
                    // action ID; validate the footer button's bounds.
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect;
                assert!(rect.x >= 0.0 && rect.x + rect.width <= width + 0.1);
                assert!(rect.y >= 0.0 && rect.y + rect.height <= viewport.height);
                assert_eq!(rect.height, 28.0);
                click(&mut tree, &frame, id);
            }
            let json = serde_json::to_string(&exact).unwrap();
            let mut sources = std::collections::BTreeSet::new();
            let mut final_seen = false;
            for _ in 0..40 {
                let frame = tree.layout(viewport);
                for block in frame.text_layouts() {
                    sources.insert(block.source.clone());
                    final_seen |= block
                        .runs
                        .iter()
                        .any(|run| run.text.contains("FINAL-ENTRADA") && run.visible.is_some());
                }
                let scroll = frame
                    .regions
                    .iter()
                    .find(|region| {
                        region.id == "modal_body:native_command_control_dismiss"
                            && region.kind == HitKind::Scroll
                    })
                    .unwrap()
                    .rect;
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(
                        scroll.x + scroll.width / 2.0,
                        scroll.y + scroll.height / 2.0,
                    ),
                    delta_y: -300.0,
                });
            }
            assert!(sources.contains(&json));
            assert!(
                sources
                    .iter()
                    .any(|source| source.contains(&format!("{} bytes", exact.len())))
            );
            assert!(
                sources
                    .iter()
                    .any(|source| source.contains(&frozen.scope.owner_session_id)
                        && source.contains(&frozen.scope.command_id)
                        && source.contains(&frozen.scope.manager_generation))
            );
            assert!(final_seen, "full input must be reachable at {width}");
            assert_eq!(
                prepared(&state.native_command_control),
                frozen,
                "painting and actions only emit requests"
            );
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "native_command_control_dismiss".into()
                }]
            );
        }
    }

    #[test]
    fn pending_and_uncertain_never_offer_resubmit_or_infer_success_or_cancellation() {
        for width in [360.0, 900.0] {
            for uncertain in [false, true] {
                let (mut state, _root) = state();
                state.native_command_control.input = "entrada parcial posible".into();
                state
                    .native_command_control
                    .prepare(NativeCommandControlKind::SendInput)
                    .unwrap();
                let frozen = prepared(&state.native_command_control);
                state.native_command_control.phase = if uncertain {
                    NativeCommandControlPhase::OutcomeUncertain {
                        prepared: frozen,
                        operation_id: "operation-opaque-id".into(),
                        detail: "Se cortó la conexión antes del ACK".into(),
                    }
                } else {
                    NativeCommandControlPhase::Pending {
                        prepared: frozen,
                        operation_id: "operation-opaque-id".into(),
                    }
                };
                let phase = state.native_command_control.phase.clone();
                let mut tree =
                    UiTree::new(modal(&state, Element::button("behind", "Detrás"), width));
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, 720.0));
                for id in [
                    "native_command_control_confirm",
                    "native_command_control_prepare_input",
                    "native_command_control_prepare_eof",
                    "native_command_control_prepare_terminate",
                    "native_command_control_prepare_resize",
                    "native_command_control_back",
                ] {
                    assert!(!tree.focus(id), "{id}");
                }
                assert!(!frame.commands.iter().any(|command| matches!(command, DrawCommand::Text { text, .. } if text == "Cancelado" || text == "Completado" || text == "Entrada enviada")));
                click(&mut tree, &frame, "native_command_control_dismiss");
                assert!(
                    state
                        .native_command_control
                        .prepare(NativeCommandControlKind::Terminate)
                        .is_err()
                );
                state.native_command_control.open = false;
                tree.set_root(modal(&state, Element::spacer(), width));
                tree.layout(Size::new(width, 720.0));
                assert!(!tree.focus("native_command_control_dismiss"));
                assert_eq!(state.native_command_control.phase, phase);
            }
        }
    }

    #[test]
    fn stale_scope_and_capabilities_disable_confirmation_and_hide_unrelated_sessions() {
        for width in [360.0, 900.0] {
            let (mut state, _root) = state();
            state
                .native_command_control
                .prepare(NativeCommandControlKind::CloseInput)
                .unwrap();
            for closed in [true, false] {
                state.native_tool_permissions.run_command = true;
                state
                    .native_command_control
                    .observed
                    .as_mut()
                    .unwrap()
                    .stdin = NativeCommandInputState::Open;
                if closed {
                    state
                        .native_command_control
                        .observed
                        .as_mut()
                        .unwrap()
                        .stdin = NativeCommandInputState::Closed;
                } else {
                    state.native_tool_permissions.run_command = false;
                }
                let mut tree = UiTree::new(modal(&state, Element::spacer(), width));
                tree.layout(Size::new(width, 720.0));
                assert!(!tree.focus("native_command_control_confirm"));
                assert!(tree.focus("native_command_control_back"));
            }
            state.selected_session = Some("another-owner".into());
            let mut tree = UiTree::new(modal(&state, Element::spacer(), width));
            tree.layout(Size::new(width, 720.0));
            assert!(!tree.focus("native_command_control_dismiss"));
            state.selected_session = Some("owner-session".into());
            state.workspace += "/other-workspace";
            let mut tree = UiTree::new(modal(&state, Element::spacer(), width));
            tree.layout(Size::new(width, 720.0));
            assert!(!tree.focus("native_command_control_dismiss"));
        }
    }

    #[test]
    fn resize_requires_windows_pty_observation_and_preserves_contract_dimensions() {
        let (mut state, _root) = state();
        for width in [360.0, 900.0] {
            let mut tree = UiTree::new(modal_on_platform(&state, Element::spacer(), width, true));
            tree.layout(Size::new(width, 720.0));
            assert!(!tree.focus("native_command_control_columns"));
            assert!(!tree.focus("native_command_control_prepare_resize"));
        }
        let observed = state.native_command_control.observed.as_mut().unwrap();
        observed.pty = true;
        observed.resize_supported = true;
        observed.terminal_size = Some(NativeCommandTerminalSize {
            columns: 100,
            rows: 40,
        });
        state.native_command_control.columns_draft = "101".into();
        state.native_command_control.rows_draft = "41".into();
        assert!(
            state
                .native_command_control
                .prepare_on_platform(NativeCommandControlKind::Resize, false)
                .is_err()
        );
        for width in [360.0, 900.0] {
            let mut tree = UiTree::new(modal_on_platform(
                &state,
                Element::button("behind", "Detrás"),
                width,
                true,
            ));
            tree.set_reduced_motion(true);
            let viewport = Size::new(width, 720.0);
            tree.layout(viewport);
            assert!(tree.focus("native_command_control_columns"));
            assert!(tree.focus("native_command_control_prepare_resize"));
            let frame = tree.layout(viewport);
            click(&mut tree, &frame, "native_command_control_prepare_resize");
        }
        state
            .native_command_control
            .prepare_on_platform(NativeCommandControlKind::Resize, true)
            .unwrap();
        assert_eq!(
            prepared(&state.native_command_control).request,
            NativeCommandControlRequest::Resize {
                columns: 101,
                rows: 41
            }
        );
        assert!(state.native_command_control.can_confirm_on_platform(true));
        state
            .native_command_control
            .observed
            .as_mut()
            .unwrap()
            .dimensions_uncertain = true;
        assert!(!state.native_command_control.can_confirm_on_platform(true));
        state.native_command_control.phase = NativeCommandControlPhase::Draft;
        state
            .native_command_control
            .observed
            .as_mut()
            .unwrap()
            .dimensions_uncertain = false;
        for (columns, rows) in [
            ("19", "40"),
            ("501", "40"),
            ("100", "4"),
            ("100", "201"),
            ("100", "-1"),
        ] {
            state.native_command_control.columns_draft = columns.into();
            state.native_command_control.rows_draft = rows.into();
            assert!(
                state
                    .native_command_control
                    .prepare_on_platform(NativeCommandControlKind::Resize, true)
                    .is_err()
            );
        }
    }
}
