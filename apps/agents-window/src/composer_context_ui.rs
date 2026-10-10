//! Context occupancy first; provenance and token subcategories are opt-in.
//! Compaction keeps the existing host authority, persistence and recovery guards.

use crate::{
    composer_ui::ComposerProps, native_context_ui::NativeContextMeasurementStatus, theme,
    view::ViewState,
};
use openide_ui::{
    Button, ButtonGroup, ButtonVariant, Color, Component, ContextContent, ContextMeter, ContextRow,
    ControlSize, Element, FontFamily, Icon, IconButton, Popover, PopoverPlacement, ScrollArea,
    Separator, Tooltip, TooltipPlacement, TooltipSpec,
};
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ContextCompactionView<'a> {
    pub session_id: Option<&'a str>,
    pub busy: bool,
    pub unavailable: Option<&'static str>,
}
impl Default for ContextCompactionView<'_> {
    fn default() -> Self {
        Self {
            session_id: None,
            busy: false,
            unavailable: Some("Disponible en una conversación con historial."),
        }
    }
}

/// Presentation availability only. The host revalidates the stored session,
/// journal, workspace, current model, lease and recovery state on activation.
pub(crate) fn compaction_view(state: &ViewState) -> ContextCompactionView<'_> {
    let Some(session) = state
        .selected_session
        .as_deref()
        .and_then(|id| {
            state.sessions.iter().find(|session| {
                session.id == id
                    && session.backend == "native"
                    && session.project == state.workspace
            })
        })
        .filter(|_| state.selected_backend == "native")
    else {
        return Default::default();
    };
    let busy = state
        .conversation_activity
        .compacting_for(Path::new(&state.workspace), Some(&session.id))
        .is_some();
    let usage = &state.native_context;
    let has_history = usage.owner.as_deref() == Some(session.id.as_str())
        && Path::new(&state.workspace)
            .canonicalize()
            .ok()
            .is_some_and(|workspace| usage.workspace.as_deref() == Some(workspace.as_path()))
        && (usage.last_usage.is_some()
            || usage.reported_responses > 0
            || usage.unreported_responses > 0
            || usage.unowned_responses > 0);
    let unavailable = if busy {
        Some("Se está preparando el resumen.")
    } else if !state.pending_permissions.is_empty()
        || state.pending_subagent_permissions.iter().any(|permission| {
            permission.parent_session_id == session.id || permission.child_session_id == session.id
        })
    {
        Some("Resuelve los permisos pendientes antes de compactar.")
    } else if state.session_recovery.contains_key(&session.id) {
        Some("Recupera la conversación antes de compactar.")
    } else if state.running || state.background_run_selected {
        Some("Espera a que termine la respuesta.")
    } else if state.composer_branches.switching() {
        Some("Espera a que termine el cambio de rama.")
    } else if state.conversation_activity.compacting_session.is_some() {
        Some("Hay otra conversación compactándose.")
    } else if usage.error.is_some() {
        Some("No se pudo verificar el historial de la conversación.")
    } else if usage.measurement_status == NativeContextMeasurementStatus::AfterCheckpoint {
        Some("El contexto ya está compactado. Continúa la conversación.")
    } else if !has_history {
        Some("Todavía no hay historial para compactar.")
    } else {
        None
    };
    ContextCompactionView {
        session_id: Some(&session.id),
        busy,
        unavailable,
    }
}

fn text(value: impl Into<String>, size: f32, color: Color) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}
fn tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}
fn percentage(fraction: f32) -> String {
    if fraction > 0.0 && fraction < 0.01 {
        "<1%".into()
    } else {
        format!("{:.0}%", fraction * 100.0)
    }
}
fn usage_ink(fraction: Option<f32>) -> Color {
    match fraction {
        Some(value) if value >= 1.0 => theme::DESTRUCTIVE,
        Some(value) if value >= 0.8 => theme::CONTEXT_WARNING,
        _ => theme::FOREGROUND,
    }
}
fn progress(fraction: Option<f32>) -> Element {
    let mut segments = Vec::new();
    let used = fraction.unwrap_or(0.0).clamp(0.0, 1.0);
    if used > 0.0 {
        segments.push(Element::spacer().flex(used).background(usage_ink(fraction)));
    }
    if used < 1.0 {
        segments.push(
            Element::spacer()
                .flex(1.0 - used)
                .background(theme::TREE_GUIDE),
        );
    }
    Element::row(segments)
        .size(None, Some(4.0))
        .radius(2.0)
        .clip_children()
}

fn summary(props: &ComposerProps<'_>) -> Element {
    let usage = props.native_context;
    let fraction = usage.and_then(|usage| usage.used_fraction());
    let used = usage.and_then(|usage| usage.used_tokens());
    let capacity = usage.and_then(|usage| usage.capacity());
    let input_only = usage.is_some_and(|usage| {
        usage.window.filter(|limit| *limit > 0).is_none()
            && usage.input_limit.is_some_and(|limit| limit > 0)
    });
    let total = match (used, capacity) {
        (Some(used), Some(capacity)) => format!("{} / {} tokens", tokens(used), tokens(capacity)),
        (None, Some(capacity)) => format!("Capacidad: {} tokens", tokens(capacity)),
        (Some(used), None) => format!("{} tokens · límite sin verificar", tokens(used)),
        _ => "Capacidad sin verificar".into(),
    };
    let remaining = match (used, capacity) {
        (Some(used), Some(capacity)) => format!(
            "{} tokens disponibles",
            tokens(capacity.saturating_sub(used))
        ),
        _ => usage
            .map_or("Aún no hay una respuesta medida", |usage| {
                usage.measurement_label()
            })
            .into(),
    };
    let header = openide_ui::PanelHeader::new("", theme::menu_palette())
        .leading(text(
            if input_only {
                "Uso de entrada"
            } else {
                "Uso de contexto"
            },
            12.0,
            theme::FOREGROUND,
        ))
        // The close button's hint names its key, so it is built here.
        .trailing(vec![IconButton::new(
            "composer_context_dismiss",
            Icon::Close,
            theme::menu_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Cerrar contexto")
                .shortcut("Esc")
                .placement(TooltipPlacement::Below),
        )
        .render()])
        .sizing(openide_ui::HeaderSizing::POPOVER)
        .render();
    let mut content = vec![
        Element::row(vec![
            text(total, 12.0, theme::FOREGROUND).flex(1.0),
            text(
                fraction
                    .map(percentage)
                    .unwrap_or_else(|| "Sin medición".into()),
                12.0,
                usage_ink(fraction),
            ),
        ])
        .gap(10.0),
        progress(fraction),
        text(remaining, 11.0, theme::FOREGROUND_SUBTLE),
    ];
    if fraction.is_some() {
        content.push(text(
            if input_only {
                "Última entrada reportada"
            } else {
                "Última respuesta reportada"
            },
            11.0,
            theme::FOREGROUND_SUBTLE,
        ));
    }
    if usage.is_some_and(|usage| {
        usage.measurement_status == NativeContextMeasurementStatus::AfterCheckpoint
    }) {
        content.push(text(
            "El resumen está guardado. El uso se medirá en la próxima respuesta.",
            11.0,
            theme::FOREGROUND_SUBTLE,
        ));
    } else if fraction.is_some_and(|fraction| fraction >= 0.8) {
        content.push(text(
            "Queda poco espacio. Puedes compactar para continuar.",
            11.0,
            usage_ink(fraction),
        ));
    }
    let action = props.context_compaction;
    let mut footer = ButtonGroup::new(vec![Button::new(
        "composer_context_details_toggle",
        "Detalles",
        theme::menu_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Compact)
    .render()])
    .align_end();
    if let Some(session_id) = action.session_id {
        let id = format!("composer_context_compact:{session_id}");
        let button = Button::new(
            &id,
            if action.busy {
                "Compactando"
            } else {
                "Compactar"
            },
            theme::menu_palette(),
        )
        .icon(Icon::Collapse)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .loading(action.busy)
        .enabled(action.unavailable.is_none() && !action.busy)
        .render();
        footer = footer.leading(Tooltip::new(button, id, "Compactar conversación", theme::menu_palette())
            .description(action.unavailable.unwrap_or("Resume mensajes anteriores y conserva la conversación. El historial guardado no se elimina."))
            .render());
    }
    Element::column(vec![
        header,
        ScrollArea::new(
            "composer_context_summary",
            Element::column(content).gap(5.0),
        )
        .render()
        .max_height(180.0)
        .flex(1.0),
        Separator::horizontal(theme::component_palette()).render(),
        footer.render().size(None, Some(28.0)),
    ])
    .gap(8.0)
    .padding(10.0)
}

fn details(props: &ComposerProps<'_>) -> Element {
    let usage = props.native_context;
    let mut details = ContextContent::new(
        "Detalles del contexto",
        usage
            .map(|usage| format!("{} · {}", usage.provider, usage.model))
            .unwrap_or_else(|| "Sin medición para la ruta seleccionada.".into()),
        theme::menu_palette(),
    );
    if let Some(usage) = usage {
        if let Some(error) = &usage.error {
            details = details.section(
                "Consulta no disponible",
                vec![ContextRow::new("Estado", error)],
            );
        } else if let Some(last) = &usage.last_usage {
            let mut rows = vec![
                ContextRow::new("Entrada", tokens(last.input_tokens)),
                ContextRow::new("Salida", tokens(last.output_tokens)),
            ];
            if let Some(cached) = last.cached_input_tokens {
                rows.push(
                    ContextRow::new("Entrada en caché", tokens(cached))
                        .description("Ya incluida en Entrada; no se suma otra vez."),
                );
            }
            if let Some(reasoning) = last.reasoning_tokens {
                rows.push(
                    ContextRow::new("Razonamiento", tokens(reasoning))
                        .description("Ya incluido en Salida; no se suma otra vez."),
                );
            }
            details = details.section("Última respuesta", rows);
        }
        let mut limits = Vec::new();
        if let Some(limit) = usage.window {
            limits.push(ContextRow::new("Ventana", tokens(limit)));
        }
        if let Some(limit) = usage.input_limit {
            limits.push(ContextRow::new("Límite de entrada", tokens(limit)));
        }
        if let Some(limit) = usage.max_output {
            limits.push(ContextRow::new("Máximo de salida", tokens(limit)));
        }
        if let Some(reserve) = usage.output_reserve {
            limits.push(
                ContextRow::new("Reserva de salida", tokens(reserve))
                    .description("Capacidad reservada para responder; no es consumo medido."),
            );
        }
        if !limits.is_empty() {
            details = details.section("Capacidad del modelo · tokens", limits);
        }
        if !usage.breakdown.is_empty() {
            details = details.section(
                "Instrucciones y herramientas",
                usage
                    .breakdown
                    .iter()
                    .map(|category| {
                        ContextRow::new(&category.label, &category.value)
                            .description(&category.detail)
                    })
                    .collect(),
            );
        }
        for section in &usage.sections {
            details = details.section(
                &section.title,
                section
                    .rows
                    .iter()
                    .map(|row| ContextRow::new(&row.label, &row.value).description(&row.detail))
                    .collect(),
            );
        }
    }
    let header = Element::row(vec![
        Button::new(
            "composer_context_details_toggle",
            "Volver",
            theme::menu_palette(),
        )
        .icon(Icon::ArrowLeft)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .render(),
        Element::spacer().flex(1.0),
        IconButton::new(
            "composer_context_dismiss",
            Icon::Close,
            theme::menu_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Cerrar contexto")
                .shortcut("Esc")
                .placement(TooltipPlacement::Below),
        )
        .render(),
    ])
    .size(None, Some(28.0));
    Element::column(vec![header, ScrollArea::new("composer_context_details",
        details.footer("Caché y razonamiento ya están incluidos en los tokens reportados. Las cantidades y bytes de las fuentes se muestran por separado.").render())
        .render().max_height(340.0)
            .flex(1.0),
    ]).gap(6.0)
}

pub(crate) fn control_width(props: &ComposerProps<'_>) -> f32 {
    if props.width < 490.0 {
        28.0
    } else {
        96.0
    }
}
pub(crate) fn control(props: &ComposerProps<'_>) -> Element {
    let fraction = props.native_context.and_then(|usage| usage.used_fraction());
    let help = props.native_context.map_or_else(
        || "El uso se informa al recibir una respuesta del modelo.".into(),
        |usage| match (usage.used_tokens(), usage.capacity()) {
            (Some(used), Some(capacity)) => format!(
                "{used} de {capacity} tokens · última respuesta. Abrir contexto y compactación."
            ),
            _ => usage.measurement_label().into(),
        },
    );
    let trigger = Tooltip::new(
        ContextMeter::new("composer_context_toggle", None, theme::component_palette())
            .fraction(fraction)
            .usage_inks(theme::CONTEXT_WARNING, theme::CONTEXT_FULL)
            .compact(props.width < 490.0)
            .render()
            .size(Some(control_width(props)), Some(28.0)),
        "composer_context_toggle",
        "Uso de contexto",
        theme::component_palette(),
    )
    .description(help)
    .render();
    if !props.context_open {
        return trigger;
    }
    Popover::new(
        trigger,
        if props.context_details_open {
            details(props)
        } else {
            summary(props)
        },
        true,
        "composer_context_dismiss",
        theme::menu_palette(),
    )
    .width((props.width - 16.0).clamp(
        96.0,
        if props.context_details_open {
            440.0
        } else {
            320.0
        },
    ))
    .placement(PopoverPlacement::TopEnd)
    .view_key(if props.context_details_open {
        "details"
    } else {
        "summary"
    })
    .render()
    .popover_return_focus("composer_context_toggle")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{PermissionView, SessionRecoveryView, SessionView, SubagentPermissionView};

    fn ready() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        let workspace = std::env::temp_dir().canonicalize().unwrap();
        state.workspace = workspace.display().to_string();
        state.selected_backend = "native".into();
        state.selected_session = Some("conversation-a".into());
        state.sessions.push(SessionView {
            id: "conversation-a".into(),
            title: "Conversación".into(),
            project: state.workspace.clone(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
            native_automation: None,
        });
        state.native_context.owner = state.selected_session.clone();
        state.native_context.workspace = Some(workspace);
        state.native_context.unreported_responses = 1;
        state
    }

    #[test]
    fn compaction_requires_the_selected_native_owner_and_verified_history() {
        let mut state = ready();
        // Usage can be unknown while a real conversation remains compactable.
        assert!(state.native_context.last_usage.is_none());
        assert!(compaction_view(&state).unavailable.is_none());
        state.native_context.owner = Some("conversation-b".into());
        assert!(compaction_view(&state).unavailable.is_some());
        state.native_context.owner = state.selected_session.clone();
        state.native_context.workspace = None;
        assert!(compaction_view(&state).unavailable.is_some());
        state.native_context.workspace = Some(Path::new("/foreign/project").into());
        assert!(compaction_view(&state).unavailable.is_some());
        state.native_context.workspace = Some(Path::new(&state.workspace).into());
        state.native_context.unreported_responses = 0;
        assert!(compaction_view(&state).unavailable.is_some());
        state.native_context.unowned_responses = 1;
        assert!(compaction_view(&state).unavailable.is_none());
        state.sessions[0].project = "/foreign/project".into();
        assert!(compaction_view(&state).session_id.is_none());
        state.sessions[0].project = state.workspace.clone();
        state.selected_backend = "cli".into();
        assert!(compaction_view(&state).session_id.is_none());
        state.selected_backend = "native".into();
        state.selected_session = None;
        assert!(compaction_view(&state).session_id.is_none());
    }

    #[test]
    fn compaction_preserves_permission_recovery_busy_and_checkpoint_boundaries() {
        let mut state = ready();
        state.pending_permissions.push(PermissionView {
            request_id: "approval".into(),
            tool_name: "run_command".into(),
            arguments: "{}".into(),
        });
        assert!(compaction_view(&state)
            .unavailable
            .unwrap()
            .contains("permisos"));
        state.pending_permissions.clear();
        state
            .pending_subagent_permissions
            .push(SubagentPermissionView {
                parent_session_id: "conversation-a".into(),
                child_session_id: "child".into(),
                request_id: "child-approval".into(),
                tool_name: "run_command".into(),
                arguments: "{}".into(),
            });
        assert!(compaction_view(&state)
            .unavailable
            .unwrap()
            .contains("permisos"));
        state.pending_subagent_permissions[0].parent_session_id = "foreign".into();
        assert!(compaction_view(&state).unavailable.is_none());
        state
            .session_recovery
            .insert("conversation-a".into(), SessionRecoveryView::VerifyTool);
        assert!(compaction_view(&state)
            .unavailable
            .unwrap()
            .contains("Recupera"));
        state.session_recovery.clear();
        state.running = true;
        assert!(compaction_view(&state).unavailable.is_some());
        state.running = false;
        state
            .conversation_activity
            .begin_compaction("conversation-a", Path::new(&state.workspace));
        assert!(compaction_view(&state).busy);
        assert!(compaction_view(&state).unavailable.is_some());
        state.conversation_activity.end_compaction();
        state.native_context.error = Some("journal inválido".into());
        assert!(compaction_view(&state)
            .unavailable
            .unwrap()
            .contains("verificar"));
        state.native_context.error = None;
        state.native_context.measurement_status = NativeContextMeasurementStatus::AfterCheckpoint;
        assert!(compaction_view(&state)
            .unavailable
            .unwrap()
            .contains("ya está compactado"));
        state.native_context.measurement_status = NativeContextMeasurementStatus::Unreported;
        assert!(compaction_view(&state).unavailable.is_none());
    }
}
