//! Context occupancy and its categories; the full provenance is copied as text.
//! Compaction keeps the existing host authority, persistence and recovery guards.

use crate::{
    composer_ui::ComposerProps, native_context_ui::NativeContextMeasurementStatus, theme,
    view::ViewState,
};
use openide_ui::{
    Button, ButtonGroup, ButtonVariant, Color, Component, ComponentPalette, ContextMeter,
    ControlSize, Element, Icon, IconButton, Popover, PopoverPanel,
    PopoverPlacement, Progress, ScrollArea, Text, TextRole, Tooltip, TooltipPlacement,
    TooltipSpec,
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
    Progress::new(
        fraction.unwrap_or(0.0),
        ComponentPalette {
            text: usage_ink(fraction),
            selected: theme::TREE_GUIDE,
            ..theme::component_palette()
        },
    )
    .flat()
    .render()
}

/// Plain text of everything the journal projection recorded, for a bug report.
pub(crate) fn diagnostic(usage: &crate::native_context_ui::NativeContextUiState) -> String {
    let mut text = format!(
        "Diagnóstico del contexto\n{} · {}\nEstado: {}\n",
        usage.provider,
        usage.model,
        usage.error.as_deref().unwrap_or(usage.measurement_label())
    );
    let breakdown = usage
        .breakdown
        .iter()
        .map(|category| (&category.label, &category.value, &category.detail));
    let mut section = |title: &str, rows: &mut dyn Iterator<Item = (&String, &String, &String)>| {
        text.push_str(&format!("\n{title}\n"));
        for (label, value, detail) in rows {
            text.push_str(&format!("{label}: {value} — {detail}\n"));
        }
    };
    if !usage.breakdown.is_empty() {
        section("Instrucciones y herramientas", &mut { breakdown });
    }
    for part in &usage.sections {
        section(
            &part.title,
            &mut part.rows.iter().map(|row| (&row.label, &row.value, &row.detail)),
        );
    }
    text
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
    let mut total = match (used, capacity) {
        (Some(used), Some(capacity)) => format!("{} / {} tokens", tokens(used), tokens(capacity)),
        (None, Some(capacity)) => format!("Capacidad: {} tokens", tokens(capacity)),
        (Some(used), None) => format!("{} tokens", tokens(used)),
        _ => "Capacidad sin verificar".into(),
    };
    if let Some(fraction) = fraction {
        total.push_str(&format!(" · {}", percentage(fraction)));
    }
    let header = openide_ui::PanelHeader::new("", theme::menu_palette())
        .leading(
            Text::new(
                if input_only {
                    "Uso de entrada"
                } else {
                    "Uso de contexto"
                },
                theme::component_palette(),
            )
            .render(),
        )
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
        Text::new(total, theme::component_palette()).render(),
        progress(fraction),
    ];
    for category in usage.into_iter().flat_map(|usage| &usage.breakdown) {
        // A value without a figure only says that nothing was measured.
        let measured = category.value.chars().any(|c| c.is_ascii_digit());
        content.push(
            Element::row(vec![
                Text::new(&category.label, theme::component_palette())
                    .muted()
                    .render(),
                Element::row(vec![
                    Element::spacer().flex(1.0),
                    Text::new(
                        if measured { &category.value } else { "–" },
                        theme::component_palette(),
                    )
                    .fade(14.0)
                    .render(),
                ])
                .clip_children()
                .flex(1.0),
            ])
            .gap(12.0),
        );
    }
    // One status line at most; a normal measurement needs none.
    let failure = usage
        .and_then(|usage| usage.error.as_ref())
        .map(|error| format!("{}: {error}", "Consulta no disponible"));
    let status = if let Some(failure) = &failure {
        Some(failure.as_str())
    } else if usage.is_some_and(|usage| {
        usage.measurement_status == NativeContextMeasurementStatus::AfterCheckpoint
    }) {
        Some("El resumen está guardado. El uso se medirá en la próxima respuesta.")
    } else if fraction.is_some_and(|fraction| fraction >= 0.8) {
        Some("Queda poco espacio. Puedes compactar para continuar.")
    } else if fraction.is_none() {
        Some(usage.map_or("Aún no hay una respuesta medida", |usage| {
            usage.measurement_label()
        }))
    } else {
        None
    };
    if let Some(status) = status {
        content.push(
            Text::new(status, theme::component_palette())
                .role(TextRole::Small)
                .ink(if fraction.is_some() {
                    usage_ink(fraction)
                } else {
                    theme::FOREGROUND_SUBTLE
                })
                .render(),
        );
    }
    let action = props.context_compaction;
    let mut footer = ButtonGroup::new(vec![Button::new(
        "composer_context_copy_diagnostic",
        "Copiar diagnóstico",
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
    PopoverPanel::new(theme::component_palette())
        .header(header)
        .body(
            ScrollArea::new(
                "composer_context_summary",
                Element::column(content).gap(5.0),
            )
            .render(),
        )
        // A safety cap only: the usual seven categories fit without scrolling.
        .body_max_height(280.0)
        .footer(footer.render().size(None, Some(28.0)))
        .sizing(crate::ui_sizing::CONTEXT_POPOVER)
        .render()
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
        summary(props),
        true,
        "composer_context_dismiss",
        theme::menu_palette(),
    )
    .width((props.width - 16.0).clamp(96.0, 320.0))
    .placement(PopoverPlacement::TopEnd)
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
