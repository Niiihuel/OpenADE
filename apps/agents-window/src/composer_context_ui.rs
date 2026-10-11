//! Context occupancy and its categories; the full provenance is copied as text.
//! Compaction keeps the existing host authority, persistence and recovery guards.

use crate::{
    composer_ui::ComposerProps,
    native_context_ui::{NativeContextCategory, NativeContextMeasurementStatus},
    theme,
    view::ViewState,
};
use openide_ui::{
    Button, ButtonGroup, ButtonVariant, Color, Component, ContextMeter, ControlSize, Element,
    Icon, IconButton, Popover, PopoverPanel, PopoverPlacement, ScrollArea, ScrollbarAppearance, Text, TextRole,
    Tooltip, TooltipPlacement, TooltipSpec,
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
/// Height of the segmented bar and the narrowest segment it paints.
const BAR_HEIGHT: f32 = 6.0;
const MIN_SEGMENT: f32 = 2.0;
/// Whole pixels, so the rows add up to exactly the height the body is given:
/// a fraction of rounding would count as overflow and show a scrollbar.
const ROW_HEIGHT: f32 = 16.0;

/// Approximate tokens of a category that only states its size in bytes.
// shortcut: reads the "N bytes" the app itself formats into the value; give
// the category a numeric field if a value ever states its size another way.
fn estimated_tokens(value: &str) -> Option<u64> {
    let head = &value[..value.find(" bytes")?];
    let digits = head.bytes().rev().take_while(u8::is_ascii_digit).count();
    let bytes = head[head.len() - digits..].parse::<u64>().ok()?;
    Some(bytes.saturating_add(2) / 4)
}

/// One token figure per category, in order: bytes / 4 where a category states
/// bytes, and for the conversation what is left of the measured context.
/// `None` is a category with no figure, which is not a measured zero.
pub(crate) fn category_tokens(
    breakdown: &[NativeContextCategory],
    used: Option<u64>,
) -> Vec<Option<u64>> {
    let conversation = |category: &NativeContextCategory| category.id == "conversation";
    let mut tokens: Vec<_> = breakdown
        .iter()
        .map(|category| estimated_tokens(&category.value).filter(|_| !conversation(category)))
        .collect();
    if let Some((used, index)) = used.zip(breakdown.iter().position(conversation)) {
        let others = tokens.iter().flatten().fold(0u64, |sum, value| sum.saturating_add(*value));
        tokens[index] = Some(used.saturating_sub(others));
    }
    tokens
}

/// Width of each category's segment on a bar `track` wide. With a known window
/// the rest of the track is the free part; without one the segments share the
/// whole track. A category that has tokens never paints narrower than
/// `MIN_SEGMENT`, and the segments together never exceed the track.
pub(crate) fn segment_widths(tokens: &[Option<u64>], window: Option<u64>, track: f32) -> Vec<f32> {
    let sum = tokens.iter().flatten().fold(0u64, |sum, value| sum.saturating_add(*value));
    if sum == 0 || !(track > 0.0) {
        return vec![0.0; tokens.len()];
    }
    // An estimate larger than the window fills the track rather than overflow it.
    let scale = window.filter(|window| *window > 0).unwrap_or(sum).max(sum) as f64;
    let mut widths: Vec<f32> = tokens
        .iter()
        .map(|tokens| match tokens {
            Some(tokens) if *tokens > 0 => {
                ((*tokens as f64 / scale * f64::from(track)) as f32).max(MIN_SEGMENT)
            }
            _ => 0.0,
        })
        .collect();
    let excess = widths.iter().sum::<f32>() - track;
    if excess > 0.0 {
        let painted = widths.iter().filter(|width| **width > 0.0).count() as f32;
        let spare = widths.iter().sum::<f32>() - painted * MIN_SEGMENT;
        for width in widths.iter_mut().filter(|width| **width > 0.0) {
            *width = if spare >= excess {
                *width - (*width - MIN_SEGMENT) * excess / spare
            } else {
                track / painted
            };
        }
    }
    widths
}

fn segmented_bar(categories: &[NativeContextCategory], widths: &[f32]) -> Element {
    let mut pieces: Vec<_> = categories
        .iter()
        .zip(widths)
        .filter(|(_, width)| **width > 0.0)
        .map(|(category, width)| {
            Element::spacer()
                .size(Some(*width), Some(BAR_HEIGHT))
                .background(theme::context_category_ink(&category.id))
        })
        .collect();
    pieces.push(Element::spacer().flex(1.0));
    // Clipped so a rounding error in the split cannot paint past the bar.
    Element::row(pieces)
        .background(theme::TRACK)
        .size(None, Some(BAR_HEIGHT))
        .clip_children()
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
    let total = match (used, capacity, fraction) {
        (Some(used), Some(capacity), Some(fraction)) => format!(
            "{} · ~{} / {} tokens",
            percentage(fraction),
            tokens(used),
            tokens(capacity)
        ),
        (_, Some(capacity), _) => format!("Capacidad: {} tokens", tokens(capacity)),
        (Some(used), None, _) => format!("{} tokens", tokens(used)),
        _ => "Capacidad sin verificar".into(),
    };
    let categories = usage.map_or(&[][..], |usage| &usage.breakdown);
    let shares = category_tokens(categories, used);
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
        Text::new(total, theme::component_palette())
            .ink(usage_ink(fraction))
            .render()
            .size(None, Some(ROW_HEIGHT)),
        segmented_bar(
            categories,
            &segment_widths(&shares, capacity, bar_width(props.width)),
        ),
    ];
    for (category, share) in categories.iter().zip(&shares) {
        content.push(
            Element::row(vec![
                Element::spacer()
                    .size(Some(10.0), Some(10.0))
                    .background(theme::context_category_ink(&category.id))
                    .radius(2.0)
                    .centered_y(),
                Text::new(&category.label, theme::component_palette())
                    .muted()
                    .render(),
                Element::spacer().flex(1.0),
                // A category without a figure only says that nothing was measured.
                Text::new(
                    share.map_or_else(|| "–".into(), |share| format!("~{}", tokens(share))),
                    theme::component_palette(),
                )
                .render(),
            ])
            .gap(8.0)
            .size(None, Some(ROW_HEIGHT)),
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
            // An overlay keeps the width the bar's segments were sized for.
            .appearance(ScrollbarAppearance::overlay())
            .render(),
        )
        // A safety cap only: the usual seven categories fit without scrolling.
        .body_max_height(280.0)
        .footer(footer.render().size(None, Some(28.0)))
        .sizing(crate::ui_sizing::CONTEXT_POPOVER)
        .render()
}

fn popover_width(composer: f32) -> f32 {
    (composer - 16.0).clamp(96.0, 320.0)
}
/// The bar spans the popover between the insets of its surface (6) and panel.
fn bar_width(composer: f32) -> f32 {
    popover_width(composer) - 2.0 * (6.0 + crate::ui_sizing::CONTEXT_POPOVER.padding)
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
    .width(popover_width(props.width))
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

    fn categories(values: &[(&str, &str)]) -> Vec<NativeContextCategory> {
        values
            .iter()
            .map(|(id, value)| NativeContextCategory::new(*id, *id, *value, ""))
            .collect()
    }
    const USUAL: [(&str, &str); 7] = [
        ("system", "45685 bytes"),
        ("tools", "Cantidad desconocida"),
        ("project", "0 docs · 0 bytes"),
        ("skills", "4 locales · 2048 bytes"),
        ("mcp", "2 servidores · 7 herramientas observadas"),
        ("subagents", "Sin desglose"),
        ("conversation", "105325 entrada · 269 salida"),
    ];

    #[test]
    fn categories_estimate_tokens_from_bytes_and_the_conversation_takes_the_rest() {
        let breakdown = categories(&USUAL);
        // 45685 / 4 rounds to 11421; counts of servers or tools are not sizes.
        assert_eq!(
            category_tokens(&breakdown, Some(30_000)),
            [Some(11_421), None, Some(0), Some(512), None, None, Some(18_067)]
        );
        // Accumulated conversation tokens are never the current occupancy.
        assert_eq!(category_tokens(&breakdown, None)[6], None);
        // The estimates can exceed a small measurement; the rest floors at zero.
        assert_eq!(category_tokens(&breakdown, Some(100))[6], Some(0));
        assert_eq!(
            category_tokens(&categories(&[("system", "12 bytes registrados")]), None),
            [Some(3)]
        );
    }

    #[test]
    fn segments_follow_the_window_and_share_the_track_without_one() {
        let shares = [Some(10_000), None, Some(0), Some(20_000)];
        // Known window: each share of 100 000 on 300 px, the rest stays free.
        assert_eq!(
            segment_widths(&shares, Some(100_000), 300.0),
            [30.0, 0.0, 0.0, 60.0]
        );
        // Unknown window: proportional to their own sum, filling the track.
        assert_eq!(segment_widths(&shares, None, 300.0), [100.0, 0.0, 0.0, 200.0]);
    }

    #[test]
    fn segments_clamp_to_the_track_keep_two_pixels_and_vanish_without_figures() {
        // Nothing measured and all-zero paint nothing and divide by nothing.
        assert_eq!(segment_widths(&[None, None], Some(1_000), 300.0), [0.0, 0.0]);
        assert_eq!(segment_widths(&[Some(0), Some(0)], None, 300.0), [0.0, 0.0]);
        assert_eq!(segment_widths(&[], None, 300.0), [0.0; 0]);
        // A sum beyond the window fills the track and never overflows it.
        let over = segment_widths(&[Some(150_000), Some(50_000)], Some(100_000), 300.0);
        assert_eq!(over, [225.0, 75.0]);
        // A sliver is widened to 2 px and the widest segment gives the room.
        let sliver = segment_widths(&[Some(1), Some(1_000_000)], None, 300.0);
        assert_eq!(sliver[0], 2.0);
        assert!((sliver.iter().sum::<f32>() - 300.0).abs() < 0.01, "{sliver:?}");
        let small = segment_widths(&[Some(1), Some(2)], Some(1_000_000), 300.0);
        assert_eq!(small, [2.0, 2.0]);
        // A track too short for 2 px each is split evenly instead of overflowing.
        let cramped = segment_widths(&[Some(1), Some(1), Some(1)], None, 3.0);
        assert_eq!(cramped, [1.0, 1.0, 1.0]);
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
