//! Read-only usage and provenance projected from a validated native journal.
//! Local document bytes are not provider token counts.

use openide_core::{
    system_instructions_digest, AgentEventKind, ModelRouteSnapshot, ModelTokenUsage, NativeReplay,
    StoredSession, SYSTEM_INSTRUCTIONS_VERSION,
};
use openide_harness::TokenBudgetPolicy;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeContextUiState {
    /// The category row under the pointer in the context popover; the bar
    /// brings its segment forward. Presentation only.
    pub hovered_category: Option<String>,
    pub owner: Option<String>,
    pub workspace: Option<PathBuf>,
    /// An unsuccessful lookup belongs only to this literal requested path.
    /// It is not a validated workspace and never carries token measurements.
    pub error_workspace: Option<PathBuf>,
    pub model: String,
    pub provider: String,
    pub window: Option<u64>,
    /// Who stated the window or input limit, for the popover; empty when the
    /// engine's own budget resolver did.
    pub window_source: &'static str,
    /// UTF-8 bytes that entered the context after the last measurement, or
    /// since the checkpoint or the start when nothing was measured.
    pub appended_bytes: u64,
    pub input_limit: Option<u64>,
    pub max_output: Option<u64>,
    pub output_reserve: Option<u64>,
    pub remaining_percent: Option<u8>,
    pub measurement_status: NativeContextMeasurementStatus,
    pub last_usage: Option<ModelTokenUsage>,
    pub last_usage_sequence: Option<u64>,
    pub checkpoint_sequence: Option<u64>,
    pub conversation_usage: Option<ModelTokenUsage>,
    pub reported_responses: u64,
    pub unreported_responses: u64,
    pub unowned_responses: u64,
    pub journal_last_sequence: u64,
    pub breakdown: Vec<NativeContextCategory>,
    pub sections: Vec<NativeContextSection>,
    pub error: Option<String>,
}

/// Why the latest recorded response can or cannot supply a context meter.
/// Unknown is distinct from a measured zero, and never reuses older usage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum NativeContextMeasurementStatus {
    #[default]
    NoResponse,
    Unreported,
    RouteMismatch,
    AfterCheckpoint,
    UnverifiedLimit,
    Measured,
}

/// How full the context is, by the one rule every surface reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContextOccupancy {
    pub used: u64,
    pub capacity: u64,
    /// Part of `used` is bytes / 4 rather than a provider count.
    pub estimated: bool,
}

impl ContextOccupancy {
    pub(crate) fn fraction(self) -> f32 {
        (self.used as f64 / self.capacity as f64).clamp(0.0, 1.0) as f32
    }

    /// "42%" for a provider count and "~42%" for an estimate.
    pub(crate) fn label(self) -> String {
        let fraction = self.fraction();
        if fraction > 0.0 && fraction < 0.01 {
            "<1%".into()
        } else {
            format!(
                "{}{:.0}%",
                if self.estimated { "~" } else { "" },
                fraction * 100.0
            )
        }
    }
}

/// Bytes an event adds to what the next request carries. Stream deltas count
/// only while a response is in flight; its commit replaces them.
pub(crate) fn context_bytes(kind: &AgentEventKind) -> u64 {
    use openide_core::ModelStreamEvent as Stream;
    (match kind {
        AgentEventKind::TurnAdmitted { prompt, .. }
        | AgentEventKind::TurnAdmittedWithImages { prompt, .. } => prompt.len(),
        AgentEventKind::TurnSteered { text, .. }
        | AgentEventKind::TurnSteeredWithImages { text, .. }
        | AgentEventKind::ToolResultMedia { text, .. } => text.len(),
        AgentEventKind::ToolFinished { output, .. } => output.len(),
        AgentEventKind::ModelStreamUpdate { update, .. } => match update {
            Stream::TextDelta { content } | Stream::ReasoningDelta { content, .. } => content.len(),
            Stream::ToolCallDelta { arguments_delta, .. } => arguments_delta.len(),
            _ => 0,
        },
        _ => 0,
    }) as u64
}

/// Compact observations retain their units; bytes/counts are never token shares.
#[derive(Clone, Debug, Default)]
pub(crate) struct NativeContextCategory {
    pub id: String,
    pub label: String,
    pub value: String,
    pub detail: String,
    /// The size `value` states, where it states one; the context bar
    /// estimates from this number and never from the text.
    pub bytes: Option<u64>,
}

impl NativeContextCategory {
    pub(crate) fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value: value.into(),
            detail: detail.into(),
            bytes: None,
        }
    }

    pub(crate) fn bytes(mut self, bytes: usize) -> Self {
        self.bytes = Some(bytes as u64);
        self
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeContextSection {
    pub title: String,
    pub rows: Vec<NativeContextRow>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeContextRow {
    pub label: String,
    pub value: String,
    pub detail: String,
}

impl NativeContextRow {
    pub fn new(
        label: impl Into<String>,
        value: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            detail: detail.into(),
        }
    }
}

impl NativeContextUiState {
    pub(crate) fn failed_scope(
        owner: Option<&str>,
        workspace: &Path,
        provider: &str,
        model: &str,
        error: impl Into<String>,
    ) -> Self {
        Self {
            owner: owner.map(str::to_owned),
            error_workspace: Some(workspace.to_owned()),
            provider: provider.to_owned(),
            model: model.to_owned(),
            error: Some(error.into()),
            ..Self::default()
        }
    }

    /// Capacity of this exact route, preserving independent input limits.
    pub(crate) fn capacity(&self) -> Option<u64> {
        self.window
            .filter(|limit| *limit > 0)
            .or(self.input_limit.filter(|limit| *limit > 0))
    }

    /// The latest attributable measurement, not accumulated conversation use.
    /// With an input-only limit the output must not fill that input budget.
    pub(crate) fn used_tokens(&self) -> Option<u64> {
        if self.error.is_some() {
            return None;
        }
        self.last_usage.as_ref().map(|usage| {
            if self.window.filter(|limit| *limit > 0).is_none()
                && self.input_limit.is_some_and(|limit| limit > 0)
            {
                usage.input_tokens
            } else {
                usage.total_tokens
            }
        })
    }

    /// Keep the ring precise even when integer labels round a small use to 0%.
    pub(crate) fn used_fraction(&self) -> Option<f32> {
        let used = self.used_tokens()?;
        let capacity = self.capacity()?;
        Some((used as f64 / capacity as f64).clamp(0.0, 1.0) as f32)
    }

    /// The value the ring, its label, the popover and Settings all show.
    /// A provider count when the latest response reported one and nothing was
    /// added since; otherwise that count, or the stated sizes of the frozen
    /// instructions, plus bytes / 4 of what was appended. `None` without a
    /// known window: a ring cannot fill against nothing.
    pub(crate) fn occupancy(&self) -> Option<ContextOccupancy> {
        if self.error.is_some() {
            return None;
        }
        let capacity = self.capacity()?;
        let appended = self.appended_bytes.saturating_add(2) / 4;
        let (base, estimated) = match self.used_tokens() {
            Some(used) => (used, appended > 0),
            None => {
                let sizes: Vec<_> =
                    crate::composer_context_ui::category_tokens(&self.breakdown, None)
                        .into_iter()
                        .flatten()
                        .collect();
                // Nothing measured and nothing sized is unknown, not empty.
                if sizes.is_empty() && self.appended_bytes == 0 {
                    return None;
                }
                (sizes.into_iter().fold(0, u64::saturating_add), true)
            }
        };
        Some(ContextOccupancy {
            used: base.saturating_add(appended),
            capacity,
            estimated,
        })
    }

    /// Count a live event of this conversation until the journal is read again.
    pub(crate) fn note_event(&mut self, session_id: &str, kind: &AgentEventKind) {
        if self.owner.as_deref() == Some(session_id) {
            self.appended_bytes = self.appended_bytes.saturating_add(context_bytes(kind));
        }
    }

    pub(crate) fn measurement_label(&self) -> &'static str {
        if self.error.is_some() {
            return "Consulta no disponible";
        }
        match self.measurement_status {
            NativeContextMeasurementStatus::NoResponse => "Aún no hay una respuesta medida",
            NativeContextMeasurementStatus::Unreported => "El proveedor no informó el uso",
            NativeContextMeasurementStatus::RouteMismatch => "Sin uso verificable para esta ruta",
            NativeContextMeasurementStatus::AfterCheckpoint => {
                "Pendiente tras compactar el contexto"
            }
            NativeContextMeasurementStatus::UnverifiedLimit => "Ventana del modelo desconocida",
            NativeContextMeasurementStatus::Measured => "Último uso reportado",
        }
    }

    pub(crate) fn selected_route(
        provider: &str,
        model: &str,
        workspace: PathBuf,
        budget: Option<TokenBudgetPolicy>,
        max_output: Option<u64>,
        sections: Vec<NativeContextSection>,
    ) -> Self {
        let mut state = Self {
            provider: provider.into(),
            model: model.into(),
            workspace: Some(workspace),
            window: budget.map(|budget| budget.context_window_tokens).filter(|limit| *limit > 0),
            input_limit: budget.and_then(|budget| budget.input_limit_tokens),
            output_reserve: budget.map(|budget| budget.output_reserve_tokens),
            max_output,
            breakdown: vec![
                NativeContextCategory::new(
                    "system",
                    "Sistema",
                    "Sin copia",
                    "No hay una copia validada de instrucciones adicionales. No es un conteo del prompt base ni prueba de instrucciones vacías.",
                ),
                NativeContextCategory::new(
                    "tools",
                    "Herramientas",
                    "Sin snapshot",
                    "El catálogo del turno sólo conserva su digest; no documenta cantidad, esquemas ni tokens.",
                ),
                NativeContextCategory::new(
                    "project",
                    "Proyecto",
                    "No observado",
                    "Documentos actuales para un próximo turno; no son una atribución de tokens.",
                ),
                NativeContextCategory::new(
                    "skills",
                    "Skills",
                    "No observadas",
                    "Selección actual para un próximo turno; no sustituye la copia combinada guardada.",
                ),
                NativeContextCategory::new(
                    "mcp",
                    "MCP",
                    "No observado",
                    "Registro guardado e inspección son observaciones independientes del catálogo congelado.",
                ),
                NativeContextCategory::new(
                    "subagents",
                    "Subagentes",
                    "Sin desglose",
                    "El journal no conserva una copia separada de definiciones ni atribuye tokens a subagentes. Sus respuestas tienen sus propios journals; no se agregan al uso de esta conversación.",
                ),
                NativeContextCategory::new(
                    "conversation",
                    "Conversación",
                    "Sin uso",
                    "No hay conteos reportados y atribuibles a la ruta seleccionada. No equivale a cero tokens.",
                ),
            ],
            sections,
            ..Self::default()
        };
        state.sections.insert(0, NativeContextSection {
            title: "Ruta y límites seleccionados".into(),
            rows: vec![
                NativeContextRow::new("Proveedor e ID de modelo", format!("{provider} · {model}"), "Ruta resuelta actual. Una respuesta histórica requiere coincidencia de proveedor, modelo, endpoint y perfil; este selector no cambia su procedencia."),
                NativeContextRow::new("Ventana de contexto", state.window.map_or_else(|| "No verificada".into(), |tokens| tokens.to_string()), "Límite del modelo exacto consultado por el mismo resolutor de presupuesto del motor. No se completa una entrada parcial con límites de otro modelo u origen."),
                NativeContextRow::new("Máximo de entrada declarado", state.input_limit.map_or_else(|| "No verificado".into(), |tokens| tokens.to_string()), "Un máximo de entrada independiente no declara una ventana total. El presupuesto lo aplica sin sumarle la reserva de salida."),
                NativeContextRow::new("Máximo de salida declarado", state.max_output.map_or_else(|| "No verificado".into(), |tokens| tokens.to_string()), "Máximo declarado para la ruta exacta; no es el uso de una respuesta ni la reserva configurada."),
                NativeContextRow::new("Reserva de salida del presupuesto", state.output_reserve.map_or_else(|| "Sin presupuesto verificado".into(), |tokens| tokens.to_string()), "Espacio reservado por el motor para una próxima respuesta, incluido razonamiento. Puede ser menor que el máximo declarado si el usuario configuró un tope de salida; no se suma al consumo registrado."),
            ],
        });
        state
    }

    pub(crate) fn set_category(&mut self, category: NativeContextCategory) {
        if let Some(existing) = self
            .breakdown
            .iter_mut()
            .find(|entry| entry.id == category.id)
        {
            *existing = category;
        } else {
            self.breakdown.push(category);
        }
    }

    pub(crate) fn matches_scope(
        &self,
        session_id: Option<&str>,
        workspace: &Path,
        provider: &str,
        model: &str,
    ) -> bool {
        self.owner.as_deref() == session_id
            && self.provider == provider
            && self.model == model
            && if let Some(requested) = &self.error_workspace {
                self.error.is_some() && requested == workspace
            } else {
                self.workspace.as_deref().is_some_and(|validated| {
                    workspace.canonicalize().ok().as_deref() == Some(validated)
                })
            }
    }

    /// Replay must use the checkpoint store for this exact journal. Check
    /// observational copies again, including events preceding a checkpoint.
    pub(crate) fn apply_validated(
        mut self,
        stored: &StoredSession,
        replay: Result<NativeReplay, String>,
        selected_route: Option<&ModelRouteSnapshot>,
    ) -> Self {
        self.owner = Some(stored.session.id.clone());
        self.journal_last_sequence = stored.events.last().map_or(0, |event| event.sequence);
        let validation = (|| {
            if stored.session.backend != "native" {
                return Err("La conversación guardada no usa el motor nativo".to_owned());
            }
            let workspace = stored.session.workspace.canonicalize().map_err(|error| {
                format!("No se pudo verificar el proyecto de la conversación: {error}")
            })?;
            if self.workspace.as_ref() != Some(&workspace) {
                return Err("La conversación no pertenece al proyecto seleccionado".to_owned());
            }
            let replay = replay?;
            if replay.session_id != stored.session.id
                || replay.last_sequence != self.journal_last_sequence
            {
                return Err(
                    "La validación no corresponde a esta conversación y final del journal"
                        .to_owned(),
                );
            }
            for event in &stored.events {
                if event.session_id != stored.session.id {
                    return Err(format!(
                        "El evento {} pertenece a otra conversación",
                        event.sequence
                    ));
                }
                match &event.kind {
                    AgentEventKind::ModelRouteCommitted {
                        version,
                        provider_id,
                        model_id,
                        endpoint_digest,
                        request_profile_digest,
                    } => {
                        if !(ModelRouteSnapshot {
                            version: *version,
                            provider_id: provider_id.clone(),
                            model_id: model_id.clone(),
                            endpoint_digest: endpoint_digest.clone(),
                            request_profile_digest: request_profile_digest.clone(),
                        })
                        .is_valid()
                        {
                            return Err(format!(
                                "La procedencia de modelo del evento {} no es válida",
                                event.sequence
                            ));
                        }
                    }
                    AgentEventKind::SystemInstructionsCommitted {
                        version,
                        digest,
                        content,
                    } if *version != SYSTEM_INSTRUCTIONS_VERSION
                        || system_instructions_digest(content).as_deref()
                            != Some(digest.as_str()) =>
                    {
                        return Err(format!(
                            "La copia de instrucciones del evento {} no es válida",
                            event.sequence
                        ));
                    }
                    _ => {}
                }
            }
            Ok(())
        })();
        if let Err(error) = validation {
            self.error = Some(error);
            return self;
        }
        if selected_route.is_some_and(|route| {
            !route.is_valid() || route.provider_id != self.provider || route.model_id != self.model
        }) {
            self.error =
                Some("La ruta resuelta no coincide con el proveedor y modelo seleccionados".into());
            return self;
        }
        if let Err(error) = self.project(stored, selected_route) {
            self.last_usage = None;
            self.remaining_percent = None;
            self.conversation_usage = None;
            self.error = Some(error);
        }
        self
    }

    fn project(
        &mut self,
        stored: &StoredSession,
        selected_route: Option<&ModelRouteSnapshot>,
    ) -> Result<(), String> {
        let mut route = None;
        let mut request = None;
        let mut latest = None;
        let mut instructions = None;
        let mut catalog = None;
        let mut mcp = None;
        let mut context = None;
        let mut cache_reports = 0u64;
        let mut reasoning_reports = 0u64;
        // Bytes since the checkpoint (or the start), and since the last
        // response this route measured.
        let (mut unmeasured, mut since_measure) = (0u64, 0u64);
        let mut total = ModelTokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            reasoning_tokens: None,
            cached_input_tokens: None,
        };
        for event in &stored.events {
            // Deltas are superseded by the response that commits them.
            if !matches!(event.kind, AgentEventKind::ModelStreamUpdate { .. }) {
                let bytes = context_bytes(&event.kind);
                unmeasured = unmeasured.saturating_add(bytes);
                since_measure = since_measure.saturating_add(bytes);
            }
            match &event.kind {
                AgentEventKind::TurnAdmitted { .. }
                | AgentEventKind::TurnAdmittedWithImages { .. } => {
                    route = None;
                    request = None;
                }
                AgentEventKind::ModelRouteCommitted {
                    version,
                    provider_id,
                    model_id,
                    endpoint_digest,
                    request_profile_digest,
                } => {
                    route = Some(ModelRouteSnapshot {
                        version: *version,
                        provider_id: provider_id.clone(),
                        model_id: model_id.clone(),
                        endpoint_digest: endpoint_digest.clone(),
                        request_profile_digest: request_profile_digest.clone(),
                    })
                }
                AgentEventKind::ModelRequestStarted { step, model } => {
                    request = Some((*step, model.clone()))
                }
                AgentEventKind::ModelResponseCommitted {
                    step,
                    usage,
                    assistant,
                    ..
                } => {
                    // The reply joins the context too; a measured one is
                    // already inside the count that replaces the estimate.
                    let reply = serde_json::to_vec(assistant).map_or(0, |json| json.len()) as u64;
                    unmeasured = unmeasured.saturating_add(reply);
                    since_measure = if usage.is_some()
                        && selected_route.is_some()
                        && route.as_ref() == selected_route
                    {
                        0
                    } else {
                        since_measure.saturating_add(reply)
                    };
                    let (request_step, request_model) = request.take().ok_or_else(|| {
                        format!(
                            "La respuesta del evento {} no tiene petición de modelo",
                            event.sequence
                        )
                    })?;
                    if request_step != *step
                        || route
                            .as_ref()
                            .is_some_and(|route| route.model_id != request_model)
                    {
                        return Err(format!(
                            "La respuesta del evento {} no coincide con la petición y procedencia",
                            event.sequence
                        ));
                    }
                    let owned = selected_route.is_some() && route.as_ref() == selected_route;
                    latest = Some((
                        event.sequence,
                        request_model,
                        route.clone(),
                        usage.clone(),
                        owned,
                    ));
                    if !owned {
                        self.unowned_responses += 1;
                        continue;
                    }
                    match usage {
                        Some(usage) => {
                            self.reported_responses += 1;
                            add_usage(&mut total, usage)?;
                            cache_reports += u64::from(usage.cached_input_tokens.is_some());
                            reasoning_reports += u64::from(usage.reasoning_tokens.is_some());
                        }
                        None => self.unreported_responses += 1,
                    }
                }
                AgentEventKind::SystemInstructionsCommitted {
                    version,
                    digest,
                    content,
                } => instructions = Some((event.sequence, version, digest, content.len())),
                AgentEventKind::ToolCatalogCommitted { digest } => {
                    catalog = Some((event.sequence, digest))
                }
                AgentEventKind::McpRuntimeCommitted { version, digest } => {
                    mcp = Some((event.sequence, version, digest))
                }
                AgentEventKind::ModelContextCommitted { version, digest } => {
                    context = Some((event.sequence, version, digest))
                }
                AgentEventKind::CheckpointCommitted { .. } => {
                    self.checkpoint_sequence = Some(event.sequence);
                    (unmeasured, since_measure) = (0, 0);
                }
                AgentEventKind::TurnCompleted { .. }
                | AgentEventKind::TurnCancelled | AgentEventKind::TurnSuperseded { .. }
                | AgentEventKind::TurnFailed { .. } => {
                    route = None;
                    request = None;
                }
                _ => {}
            }
        }
        let mut rows = Vec::new();
        self.appended_bytes = unmeasured;
        if let Some((sequence, model, route, usage, owned)) = latest {
            self.last_usage_sequence = Some(sequence);
            rows.push(NativeContextRow::new(
                "Modelo de la última respuesta",
                model,
                format!("Respuesta guardada: evento {sequence}."),
            ));
            rows.push(NativeContextRow::new("Procedencia de la última respuesta", route.as_ref().map_or_else(|| "Sin marcador (journal anterior)".into(), |route| route.provider_id.clone()), route.as_ref().map_or_else(|| "No se atribuye esta respuesta a la ruta actual por coincidencia del nombre del modelo.".into(), |route| format!("Proveedor/ID exactos y fingerprints guardados. Endpoint SHA-256: {}\nPerfil: {}\n{}", route.endpoint_digest, route.request_profile_digest, if owned {"Coinciden con la ruta resuelta seleccionada."} else {"No coinciden con la ruta resuelta seleccionada."}))));
            let fresh = self
                .checkpoint_sequence
                .is_none_or(|checkpoint| sequence > checkpoint);
            self.measurement_status = if !fresh {
                NativeContextMeasurementStatus::AfterCheckpoint
            } else if !owned {
                NativeContextMeasurementStatus::RouteMismatch
            } else if usage.is_none() {
                NativeContextMeasurementStatus::Unreported
            } else if self.capacity().is_none() {
                NativeContextMeasurementStatus::UnverifiedLimit
            } else {
                NativeContextMeasurementStatus::Measured
            };
            if let Some(usage) = &usage {
                rows.extend(usage_rows(
                    usage,
                    "Uso de esta respuesta, sin sumar de nuevo cache ni razonamiento.",
                ));
                if owned && fresh {
                    self.appended_bytes = since_measure;
                    self.last_usage = Some(usage.clone());
                    self.remaining_percent =
                        self.capacity()
                            .zip(self.used_tokens())
                            .map(|(window, used)| {
                                (((u128::from(window.saturating_sub(used)) * 100)
                                    + u128::from(window) / 2)
                                    / u128::from(window))
                                .min(100) as u8
                            });
                }
            } else {
                rows.push(NativeContextRow::new(
                    "Uso de la última respuesta",
                    "No reportado",
                    "No se recupera el uso de una respuesta anterior para sustituir este dato.",
                ));
            }
            rows.push(NativeContextRow::new("Medición del contexto actual", if !fresh {"Pendiente después del checkpoint"} else if !owned {"Ruta histórica sin coincidencia verificable"} else if usage.is_none() {"Proveedor sin uso reportado"} else if self.window.is_none() && self.input_limit.is_none() {"Límite del modelo no verificado"} else {"Último uso reportado / límite actual"}, "Con ventana total se compara el total; con máximo de entrada independiente, sólo la entrada. No predice la próxima petición ni atribuye tokens a archivos locales."));
        } else {
            rows.push(NativeContextRow::new(
                "Respuesta guardada",
                "Todavía no disponible",
                "Los conteos aparecen después de una ModelResponseCommitted con uso reportado.",
            ));
        }
        self.sections.insert(
            0,
            NativeContextSection {
                title: "Última respuesta guardada".into(),
                rows,
            },
        );
        let mut sum_rows = vec![NativeContextRow::new(
            "Respuestas de esta ruta",
            format!(
                "{} con uso · {} sin uso",
                self.reported_responses, self.unreported_responses
            ),
            format!(
                "{} respuestas excluidas por procedencia ausente o ruta distinta. Coincidencia exacta de proveedor, modelo, endpoint y perfil de petición; no agrupa otras rutas por nombre parecido.",
                self.unowned_responses
            ),
        )];
        if self.reported_responses > 0 {
            sum_rows.extend(usage_rows(&total, "Suma de los conteos reportados para esta ruta. Es consumo acumulado, no ocupación de la ventana de contexto."));
            sum_rows.push(NativeContextRow::new("Cobertura de los subconteos", format!("Cache: {cache_reports}/{} · razonamiento: {reasoning_reports}/{}", self.reported_responses, self.reported_responses), "Los subconteos ausentes son desconocidos. Una suma parcial sólo incluye respuestas que reportaron ese campo; no se cuenta como tokens adicionales al total."));
            self.set_category(NativeContextCategory::new("conversation", "Conversación", format!("{} entrada · {} salida", total.input_tokens, total.output_tokens), format!("Tokens acumulados de {} respuestas registradas de esta ruta exacta; {} no reportaron uso y {} fueron excluidas por procedencia. No es ocupación actual ni facturación completa; cache y razonamiento son subconteos, sin sumar de nuevo.", self.reported_responses, self.unreported_responses, self.unowned_responses)));
            self.conversation_usage = Some(total);
        }
        self.sections.insert(
            1,
            NativeContextSection {
                title: "Uso acumulado de esta ruta en la conversación".into(),
                rows: sum_rows,
            },
        );
        let mut frozen_rows = Vec::new();
        if let Some((sequence, version, digest, bytes)) = instructions {
            self.set_category(NativeContextCategory::new("system", "Sistema", format!("{bytes} bytes"), format!("Copia validada de instrucciones adicionales combinadas: evento {sequence}, versión {version}, SHA-256 {digest}. Incluye proyecto, skills y objetivo; no incluye una atribución del prompt base ni tokens por documento.")).bytes(bytes));
            frozen_rows.push(NativeContextRow::new("Instrucciones adicionales", format!("{bytes} bytes UTF-8"), format!("Copia exacta del evento {sequence} · versión {version}\nSHA-256: {digest}\nCombina instrucciones del proyecto, skills y objetivo. El journal no atribuye tokens a cada documento; estas preferencias actuales no sustituyen la copia guardada.")));
        } else {
            frozen_rows.push(NativeContextRow::new(
                "Instrucciones adicionales",
                "Sin copia guardada",
                "Journal anterior o turno aún no preparado; no equivale a instrucciones vacías.",
            ));
        }
        if let Some((sequence, digest)) = catalog {
            self.set_category(NativeContextCategory::new("tools", "Herramientas", "Cantidad desconocida", format!("Snapshot del evento {sequence}: SHA-256 {digest}. El marcador no conserva esquemas, cantidad ni tokens; una inspección actual no demuestra su contenido.")));
            frozen_rows.push(NativeContextRow::new("Catálogo de herramientas del turno", "Digest guardado", format!("Evento {sequence}\nSHA-256: {digest}\nEl marcador no conserva los esquemas ni el número de herramientas. Un catálogo inspeccionado en Settings no demuestra este snapshot.")));
        }
        if let Some((sequence, version, digest)) = mcp {
            frozen_rows.push(NativeContextRow::new("Identidad MCP para Resume", "Marcador guardado", format!("Evento {sequence} · versión {version}\nSHA-256: {digest}\nIdentidad de recuperación; no es un conteo de tokens MCP.")));
        } else {
            frozen_rows.push(NativeContextRow::new(
                "Identidad MCP para Resume",
                "Sin marcador",
                "No demuestra ausencia de herramientas MCP ni autoriza su recuperación.",
            ));
        }
        if let Some((sequence, version, digest)) = context {
            frozen_rows.push(NativeContextRow::new(
                "Identidad del contexto del modelo",
                "Marcador guardado",
                format!("Evento {sequence} · versión {version}\nSHA-256: {digest}"),
            ));
        }
        if let Some(sequence) = self.checkpoint_sequence {
            frozen_rows.push(NativeContextRow::new("Último checkpoint", format!("Evento {sequence}"), "El historial anterior fue resumido. Su consumo sigue en los acumulados; el medidor exige una respuesta de la misma ruta posterior al checkpoint."));
        }
        self.sections.insert(
            2,
            NativeContextSection {
                title: "Snapshots guardados del motor".into(),
                rows: frozen_rows,
            },
        );
        Ok(())
    }
}

fn usage_rows(usage: &ModelTokenUsage, detail: &str) -> Vec<NativeContextRow> {
    vec![
        NativeContextRow::new("Entrada", usage.input_tokens.to_string(), detail),
        NativeContextRow::new("Salida", usage.output_tokens.to_string(), detail),
        NativeContextRow::new("Total reportado", usage.total_tokens.to_string(), detail),
        NativeContextRow::new(
            "Entrada en cache",
            usage
                .cached_input_tokens
                .map_or_else(|| "No reportada".into(), |tokens| tokens.to_string()),
            "Subconteo reportado de la entrada. No se suma otra vez al total.",
        ),
        NativeContextRow::new(
            "Razonamiento",
            usage
                .reasoning_tokens
                .map_or_else(|| "No reportado".into(), |tokens| tokens.to_string()),
            "Subconteo reportado por el proveedor. No se suma otra vez al total.",
        ),
    ]
}

fn add_usage(total: &mut ModelTokenUsage, usage: &ModelTokenUsage) -> Result<(), String> {
    let add = |left: u64, right: u64| {
        left.checked_add(right).ok_or_else(|| {
            "El uso acumulado supera el rango representable; no se muestra una suma truncada"
                .to_owned()
        })
    };
    total.input_tokens = add(total.input_tokens, usage.input_tokens)?;
    total.output_tokens = add(total.output_tokens, usage.output_tokens)?;
    total.total_tokens = add(total.total_tokens, usage.total_tokens)?;
    if let Some(tokens) = usage.cached_input_tokens {
        total.cached_input_tokens = Some(add(total.cached_input_tokens.unwrap_or(0), tokens)?);
    }
    if let Some(tokens) = usage.reasoning_tokens {
        total.reasoning_tokens = Some(add(total.reasoning_tokens.unwrap_or(0), tokens)?);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use openide_core::{
        replay_native, AgentEvent, AgentMessage, CheckpointCompatibility, SessionStore, TurnMode,
        EXEC_POLICY_IDENTITY_VERSION, HOOK_CONFIG_IDENTITY_VERSION, MODEL_CONTEXT_VERSION,
    };

    pub(crate) fn journal_fixture(
        root: &Path,
        workspace: &Path,
        route: Option<&ModelRouteSnapshot>,
        usages: &[Option<ModelTokenUsage>],
    ) -> (SessionStore, String) {
        let store = SessionStore::open(root.join("sessions")).unwrap();
        let session = store.create(workspace, "native").unwrap();
        for (index, usage) in usages.iter().enumerate() {
            let admission = store
                .admit_native_turn(
                    &session.id,
                    &format!("usage-{index}"),
                    "Count actual recorded usage",
                    "fixture-model",
                    TurnMode::Plan,
                )
                .unwrap();
            let instructions = "Exact project instructions ñ\nSKILL provenance and goal";
            let mut kinds = vec![
                AgentEventKind::TurnStarted {
                    prompt: "Count actual recorded usage".into(),
                },
                AgentEventKind::ToolCatalogCommitted {
                    digest: "a".repeat(64),
                },
                AgentEventKind::HooksCommitted {
                    version: HOOK_CONFIG_IDENTITY_VERSION,
                    digest: "b".repeat(64),
                },
                AgentEventKind::ExecPolicyCommitted {
                    version: EXEC_POLICY_IDENTITY_VERSION,
                    digest: "c".repeat(64),
                },
                AgentEventKind::SystemInstructionsCommitted {
                    version: SYSTEM_INSTRUCTIONS_VERSION,
                    digest: system_instructions_digest(instructions).unwrap(),
                    content: instructions.into(),
                },
                AgentEventKind::ModelContextCommitted {
                    version: MODEL_CONTEXT_VERSION,
                    digest: "d".repeat(64),
                },
            ];
            if let Some(route) = route {
                kinds.push(route.clone().into_event());
            }
            kinds.extend([
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "fixture-model".into(),
                },
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant: AgentMessage::assistant("Recorded response", vec![]),
                    response_id: None,
                    usage: usage.clone(),
                },
                AgentEventKind::TurnCompleted {
                    response: "Recorded response".into(),
                    steps: 1,
                },
            ]);
            for (offset, kind) in kinds.into_iter().enumerate() {
                store
                    .append_native_event(&AgentEvent::new(
                        &session.id,
                        admission.sequence + offset as u64 + 1,
                        kind,
                    ))
                    .unwrap();
            }
        }
        (store, session.id)
    }

    fn route() -> ModelRouteSnapshot {
        ModelRouteSnapshot::new(
            "custom",
            "fixture-model",
            "http://127.0.0.1:4567/v1",
            [7; 32],
        )
        .unwrap()
    }

    fn usage(input: u64, output: u64) -> ModelTokenUsage {
        ModelTokenUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
            cached_input_tokens: Some(input / 2),
            reasoning_tokens: Some(output / 2),
        }
    }

    fn projection(stored: &StoredSession, selected: &ModelRouteSnapshot) -> NativeContextUiState {
        let budget = TokenBudgetPolicy {
            input_limit_tokens: None,
            context_window_tokens: 1_000,
            output_reserve_tokens: 100,
            tool_schema_reserve_tokens: 0,
            auto_compact_token_limit: None,
        };
        projection_with_budget(stored, selected, Some(budget))
    }

    fn projection_with_budget(
        stored: &StoredSession,
        selected: &ModelRouteSnapshot,
        budget: Option<TokenBudgetPolicy>,
    ) -> NativeContextUiState {
        let state = NativeContextUiState::selected_route(
            &selected.provider_id,
            &selected.model_id,
            stored.session.workspace.canonicalize().unwrap(),
            budget,
            Some(200),
            vec![],
        );
        let replay = replay_native(
            &stored.session.id,
            &stored.events,
            None,
            &CheckpointCompatibility {
                provider: "custom".into(),
                model: "fixture-model".into(),
                context_format_key: "chat-completions-v1".into(),
            },
        )
        .map_err(|error| error.to_string());
        state.apply_validated(stored, replay, Some(selected))
    }

    #[test]
    fn native_context_failed_scope_is_exact_and_never_a_validated_measurement() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing-project");
        let mut state = NativeContextUiState::failed_scope(
            Some("selected-session"),
            &missing,
            "selected-provider",
            "selected-model",
            "Project could not be inspected",
        );
        assert!(state.matches_scope(
            Some("selected-session"),
            &missing,
            "selected-provider",
            "selected-model"
        ));
        assert!(state.workspace.is_none());
        assert!(state.last_usage.is_none() && state.conversation_usage.is_none());
        assert!(state.used_tokens().is_none() && state.used_fraction().is_none());
        assert!(state.capacity().is_none() && state.remaining_percent.is_none());
        for (owner, path, provider, model) in [
            (
                Some("other-session"),
                missing.as_path(),
                "selected-provider",
                "selected-model",
            ),
            (
                None,
                missing.as_path(),
                "selected-provider",
                "selected-model",
            ),
            (
                Some("selected-session"),
                root.path(),
                "selected-provider",
                "selected-model",
            ),
            (
                Some("selected-session"),
                missing.as_path(),
                "other-provider",
                "selected-model",
            ),
            (
                Some("selected-session"),
                missing.as_path(),
                "selected-provider",
                "other-model",
            ),
        ] {
            assert!(!state.matches_scope(owner, path, provider, model));
        }
        // Removing the failure cannot promote its unverified request path to
        // an ordinary measurement scope, even while the directory is absent.
        state.error = None;
        assert!(!state.matches_scope(
            Some("selected-session"),
            &missing,
            "selected-provider",
            "selected-model"
        ));
        state.error_workspace = None;
        assert!(!state.matches_scope(
            Some("selected-session"),
            &missing,
            "selected-provider",
            "selected-model"
        ));
    }

    #[test]
    fn occupancy_is_exact_after_a_measured_response_and_an_estimate_otherwise() {
        let root = tempfile::tempdir().unwrap();
        let route = route();
        let prompt = "Count actual recorded usage".len() as u64;
        let project = |usages: &[Option<ModelTokenUsage>]| {
            let (store, id) =
                journal_fixture(&root.path().join(usages.len().to_string()), root.path(), Some(&route), usages);
            projection(&store.load(&id).unwrap().unwrap(), &route)
        };

        // The provider's count, with nothing added since: exact.
        let mut measured = project(&[Some(usage(400, 20))]);
        assert_eq!(measured.appended_bytes, 0);
        let exact = measured.occupancy().unwrap();
        assert_eq!((exact.used, exact.capacity, exact.estimated), (420, 1_000, false));
        assert_eq!(exact.label(), "42%");
        assert_eq!(Some(exact.fraction()), measured.used_fraction());

        // Text streamed for this conversation raises it at once, as an estimate.
        let delta = AgentEventKind::ModelStreamUpdate {
            step: 2,
            update: openide_core::ModelStreamEvent::TextDelta { content: "x".repeat(400) },
        };
        measured.note_event("another-session", &delta);
        assert_eq!(measured.appended_bytes, 0);
        let owner = measured.owner.clone().unwrap();
        measured.note_event(&owner, &delta);
        let streaming = measured.occupancy().unwrap();
        assert_eq!((streaming.used, streaming.estimated), (520, true));
        assert_eq!(streaming.label(), "~52%");

        // No count from the provider: bytes / 4 of everything in the journal.
        let unreported = project(&[None, None]);
        assert_eq!(unreported.measurement_status, NativeContextMeasurementStatus::Unreported);
        assert!(unreported.appended_bytes > 2 * prompt, "{}", unreported.appended_bytes);
        let estimate = unreported.occupancy().unwrap();
        assert!(estimate.estimated && estimate.used >= unreported.appended_bytes / 4);
        assert!(estimate.label().starts_with('~') || estimate.label() == "<1%");

        // Only what followed the last measured response is added to its count.
        let mixed = project(&[None, Some(usage(100, 0))]);
        assert_eq!((mixed.appended_bytes, mixed.occupancy().unwrap().used), (0, 100));

        // No window: no fraction, whatever was counted. Nothing known: unknown.
        let mut windowless = measured.clone();
        (windowless.window, windowless.input_limit) = (None, None);
        assert_eq!(windowless.occupancy(), None);
        let blank = NativeContextUiState { window: Some(1_000), ..Default::default() };
        assert_eq!(blank.occupancy(), None);
        let mut failed = measured;
        failed.error = Some("journal".into());
        assert_eq!(failed.occupancy(), None);
    }

    #[test]
    fn native_context_fraction_preserves_small_measured_usage_and_real_zero() {
        let root = tempfile::tempdir().unwrap();
        let route = route();
        for (input, output) in [(1, 1), (0, 0), (1_100, 0)] {
            let (store, id) = journal_fixture(
                &root.path().join("data"),
                root.path(),
                Some(&route),
                &[Some(usage(input, output))],
            );
            let stored = store.load(&id).unwrap().unwrap();
            let state = projection(&stored, &route);
            assert_eq!(
                state.measurement_status,
                NativeContextMeasurementStatus::Measured
            );
            assert_eq!(state.used_tokens(), Some(input + output));
            assert_eq!(state.capacity(), Some(1_000));
            assert_eq!(
                state.used_fraction(),
                Some(((input + output) as f32 / 1_000.0).min(1.0))
            );
            if input + output == 2 {
                assert_eq!(state.remaining_percent, Some(100));
                assert!(
                    state.used_fraction().unwrap() > 0.0,
                    "small real usage must not become an empty ring through percent rounding"
                );
            }
        }
    }

    #[test]
    fn native_context_fraction_respects_input_only_limits_and_unknown_capacity() {
        let root = tempfile::tempdir().unwrap();
        let route = route();
        let (store, id) = journal_fixture(
            &root.path().join("data"),
            root.path(),
            Some(&route),
            &[Some(usage(200, 800))],
        );
        let stored = store.load(&id).unwrap().unwrap();
        let budget = TokenBudgetPolicy {
            input_limit_tokens: Some(1_000),
            context_window_tokens: 0,
            output_reserve_tokens: 100,
            tool_schema_reserve_tokens: 0,
            auto_compact_token_limit: None,
        };
        let input = projection_with_budget(&stored, &route, Some(budget));
        assert_eq!(input.used_tokens(), Some(200));
        assert_eq!(input.used_fraction(), Some(0.2));
        assert_eq!(input.remaining_percent, Some(80));
        assert_eq!(
            input.measurement_status,
            NativeContextMeasurementStatus::Measured
        );
        let total = projection(&stored, &route);
        assert_eq!(total.used_tokens(), Some(1_000));
        assert_eq!(total.used_fraction(), Some(1.0));
        let unknown = projection_with_budget(&stored, &route, None);
        assert_eq!(unknown.used_tokens(), Some(1_000));
        assert!(unknown.capacity().is_none() && unknown.used_fraction().is_none());
        assert_eq!(
            unknown.measurement_status,
            NativeContextMeasurementStatus::UnverifiedLimit
        );
    }

    #[test]
    fn native_context_reopens_exact_usage_and_subset_sums_without_double_counting() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let route = route();
        let first = usage(100, 50);
        let second = ModelTokenUsage {
            cached_input_tokens: None,
            reasoning_tokens: None,
            ..usage(200, 80)
        };
        let (store, id) = journal_fixture(
            &data,
            root.path(),
            Some(&route),
            &[Some(first.clone()), Some(second.clone())],
        );
        let path = store.storage_root().join(format!("{id}.jsonl"));
        let before = std::fs::read(&path).unwrap();
        drop(store);
        let reopened = SessionStore::open(data.join("sessions")).unwrap();
        let stored = reopened.load(&id).unwrap().unwrap();
        let state = projection(&stored, &route);
        assert!(state.error.is_none(), "{:?}", state.error);
        assert_eq!(state.last_usage, Some(second));
        assert_eq!(state.remaining_percent, Some(72));
        assert_eq!(state.reported_responses, 2);
        assert_eq!(state.unreported_responses, 0);
        assert_eq!(state.unowned_responses, 0);
        assert_eq!(
            state.conversation_usage,
            Some(ModelTokenUsage {
                input_tokens: 300,
                output_tokens: 130,
                total_tokens: 430,
                cached_input_tokens: first.cached_input_tokens,
                reasoning_tokens: first.reasoning_tokens
            })
        );
        let details = state
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .map(|row| format!("{} {} {}", row.label, row.value, row.detail))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(details.contains("Cache: 1/2 · razonamiento: 1/2"));
        assert!(details.contains(
            &system_instructions_digest("Exact project instructions ñ\nSKILL provenance and goal")
                .unwrap()
        ));
        assert!(details.contains("No se suma otra vez al total"));
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert!(state.matches_scope(Some(&id), root.path(), "custom", "fixture-model"));
        assert!(!state.matches_scope(
            Some("other-session"),
            root.path(),
            "custom",
            "fixture-model"
        ));
    }

    #[test]
    fn native_context_missing_latest_usage_legacy_and_changed_routes_stay_unattributed() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let route = route();
        let (store, id) = journal_fixture(
            &data,
            root.path(),
            Some(&route),
            &[Some(usage(100, 50)), None],
        );
        let stored = store.load(&id).unwrap().unwrap();
        let state = projection(&stored, &route);
        assert!(state.error.is_none());
        assert!(state.last_usage.is_none() && state.remaining_percent.is_none());
        assert!(state.used_fraction().is_none() && state.used_tokens().is_none());
        assert_eq!(
            state.measurement_status,
            NativeContextMeasurementStatus::Unreported
        );
        assert_eq!(state.reported_responses, 1);
        assert_eq!(state.unreported_responses, 1);
        for changed in [
            ModelRouteSnapshot {
                provider_id: "other-provider".into(),
                ..route.clone()
            },
            ModelRouteSnapshot {
                model_id: "other-model".into(),
                ..route.clone()
            },
            ModelRouteSnapshot::new(
                "custom",
                "fixture-model",
                "http://127.0.0.1:4568/v1",
                [7; 32],
            )
            .unwrap(),
            ModelRouteSnapshot::new(
                "custom",
                "fixture-model",
                "http://127.0.0.1:4567/v1",
                [8; 32],
            )
            .unwrap(),
        ] {
            let changed = projection(&stored, &changed);
            assert!(changed.error.is_none());
            assert!(
                changed.last_usage.is_none()
                    && changed.remaining_percent.is_none()
                    && changed.conversation_usage.is_none()
            );
            assert_eq!(changed.unowned_responses, 2);
            assert!(changed.used_fraction().is_none());
            assert_eq!(
                changed.measurement_status,
                NativeContextMeasurementStatus::RouteMismatch
            );
        }
        let (legacy, legacy_id) =
            journal_fixture(&data, root.path(), None, &[Some(usage(100, 50))]);
        let legacy = projection(&legacy.load(&legacy_id).unwrap().unwrap(), &route);
        assert!(legacy.error.is_none());
        assert!(legacy.last_usage.is_none() && legacy.conversation_usage.is_none());
        assert_eq!(
            legacy.measurement_status,
            NativeContextMeasurementStatus::RouteMismatch
        );
        assert_eq!(legacy.unowned_responses, 1);
        assert!(legacy.sections[0]
            .rows
            .iter()
            .any(|row| row.value.contains("Sin marcador")));
    }

    #[test]
    fn native_context_rejects_foreign_owner_invalid_snapshot_and_sum_overflow() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let route = route();
        let (store, id) =
            journal_fixture(&data, root.path(), Some(&route), &[Some(usage(100, 50))]);
        let stored = store.load(&id).unwrap().unwrap();
        let mut foreign = stored.clone();
        foreign.events[1].session_id = "other-owner".into();
        let rejected = projection(&foreign, &route);
        assert!(rejected.error.is_some() && rejected.last_usage.is_none());
        let mut invalid = stored.clone();
        for event in &mut invalid.events {
            if let AgentEventKind::ModelRouteCommitted {
                endpoint_digest, ..
            } = &mut event.kind
            {
                *endpoint_digest = "broken".into();
            }
        }
        let rejected = projection(&invalid, &route);
        assert!(rejected.error.is_some() && rejected.conversation_usage.is_none());
        let other = tempfile::tempdir().unwrap();
        let state = NativeContextUiState::selected_route(
            "custom",
            "fixture-model",
            other.path().canonicalize().unwrap(),
            None,
            None,
            vec![],
        );
        let replay = replay_native(
            &stored.session.id,
            &stored.events,
            None,
            &CheckpointCompatibility {
                provider: String::new(),
                model: String::new(),
                context_format_key: String::new(),
            },
        )
        .map_err(|error| error.to_string());
        let rejected = state.apply_validated(&stored, replay, Some(&route));
        assert!(rejected.error.is_some() && rejected.last_usage.is_none());
        let overflow_usage = ModelTokenUsage {
            input_tokens: u64::MAX,
            output_tokens: 0,
            total_tokens: u64::MAX,
            cached_input_tokens: None,
            reasoning_tokens: None,
        };
        let (overflow, overflow_id) = journal_fixture(
            &data,
            root.path(),
            Some(&route),
            &[Some(overflow_usage.clone()), Some(overflow_usage)],
        );
        let rejected = projection(&overflow.load(&overflow_id).unwrap().unwrap(), &route);
        assert!(rejected
            .error
            .as_deref()
            .is_some_and(|error| error.contains("rango representable")));
        assert!(
            rejected.last_usage.is_none()
                && rejected.remaining_percent.is_none()
                && rejected.conversation_usage.is_none()
        );
    }
}
