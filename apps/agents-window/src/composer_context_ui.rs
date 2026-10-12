//! Context occupancy and its categories; the full provenance is copied as text.
//! Compaction keeps the existing host authority, persistence and recovery guards.

use crate::{
    composer_ui::ComposerProps,
    native_context_ui::{NativeContextCategory, NativeContextMeasurementStatus},
    theme,
    view::ViewState,
};
use openide_ui::{
    Color, Component, ContextMeter, ContextShare, ContextShareAction, ContextTones,
    ContextWindow, Element, ElementKind, Icon, Popover, PopoverPanel, PopoverPlacement,
    ScrollArea, ScrollbarAppearance, Text, TextRole, Tooltip, TooltipPlacement,
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

/// A token count as the app writes it: "272K", "16.4K", "1M", "1.05M".
pub(crate) fn tokens(value: u64) -> String {
    let short = |scaled: f64, digits: usize, unit: &str| {
        let text = format!("{scaled:.digits$}");
        format!("{}{unit}", text.trim_end_matches('0').trim_end_matches('.'))
    };
    if value >= 999_950 {
        short(value as f64 / 1_000_000.0, 2, "M")
    } else if value >= 1_000 {
        short(value as f64 / 1_000.0, 1, "K")
    } else {
        value.to_string()
    }
}

/// The figure Settings repeats: the same one the ring and its popover show.
pub(crate) fn occupancy_text(usage: &crate::native_context_ui::NativeContextUiState) -> Option<String> {
    usage
        .occupancy()
        .map(|occupancy| format!("{} usado", occupancy.label()))
}
fn usage_ink(fraction: Option<f32>) -> Color {
    match fraction {
        Some(value) if value >= 1.0 => theme::DESTRUCTIVE,
        Some(value) if value >= 0.8 => theme::CONTEXT_WARNING,
        _ => theme::FOREGROUND,
    }
}
/// ID of the context card; it names its rows and actions after it.
pub(crate) const CONTEXT_CARD: &str = "composer_context";
/// The card insets its own content this far; what stands under it lines up.
const CARD_INSET: f32 = 16.0;

/// Lines an element up with the content of the card.
fn inset(element: Element) -> Element {
    let side = || Element::spacer().size(Some(CARD_INSET), None);
    Element::row(vec![side(), element.flex(1.0), side()])
}

/// `tokens` for the card, whose figures pass through fractions as they count.
fn card_tokens(value: f32) -> String {
    tokens(value.round() as u64)
}

/// The card names its own controls; make the one called `id` dispatch `action`.
fn dispatch_as(element: &mut Element, id: &str, action: &str) {
    match &mut element.kind {
        ElementKind::Clickable { id: own, .. } if *own == id => {
            element.action_alias = Some(action.into());
        }
        ElementKind::Column(children) | ElementKind::Row(children) => {
            for child in children {
                dispatch_as(child, id, action);
            }
        }
        _ => {}
    }
}

/// One token figure per category, in order: bytes / 4 where a category states
/// bytes, and for the conversation what is left of the measured context.
/// `None` is a category with no figure, which is not a measured zero.
pub(crate) fn category_tokens(
    breakdown: &[NativeContextCategory],
    used: Option<u64>,
) -> Vec<Option<u64>> {
    let conversation = |category: &NativeContextCategory| category.id == "conversation";
    let bytes = |id: &str| {
        breakdown
            .iter()
            .find(|category| category.id == id)
            .and_then(|category| category.bytes)
            .unwrap_or(0)
    };
    // The system copy is the project documents and the local skills joined
    // with the rest (`native_workspace_instructions`), so their bytes are
    // counted once, in their own categories.
    let contained = bytes("project").saturating_add(bytes("skills"));
    let mut tokens: Vec<_> = breakdown
        .iter()
        .map(|category| {
            let bytes = category.bytes.filter(|_| !conversation(category))?;
            let own = if category.id == "system" {
                bytes.saturating_sub(contained)
            } else {
                bytes
            };
            Some(own.saturating_add(2) / 4)
        })
        .collect();
    if let Some((used, index)) = used.zip(breakdown.iter().position(conversation)) {
        let others = tokens.iter().flatten().fold(0u64, |sum, value| sum.saturating_add(*value));
        tokens[index] = Some(used.saturating_sub(others));
    }
    tokens
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
    let occupancy = usage.and_then(|usage| usage.occupancy());
    let estimated = occupancy.is_some_and(|occupancy| occupancy.estimated);
    let fraction = occupancy.map(|occupancy| occupancy.fraction());
    // Without a window the last count is still worth stating.
    let used = occupancy
        .map(|occupancy| occupancy.used)
        .or_else(|| usage.and_then(|usage| usage.used_tokens()));
    let capacity = usage.and_then(|usage| usage.capacity());
    let input_only = usage.is_some_and(|usage| {
        usage.window.filter(|limit| *limit > 0).is_none()
            && usage.input_limit.is_some_and(|limit| limit > 0)
    });
    let categories = usage.map_or(&[][..], |usage| &usage.breakdown);
    let hovered = usage.and_then(|usage| usage.hovered_category.as_deref());
    let action = props.context_compaction;
    let compact = action
        .session_id
        .filter(|_| action.unavailable.is_none() && !action.busy)
        .map(|session_id| format!("composer_context_compact:{session_id}"));
    // The card offers nothing while compaction is unavailable, so it says why.
    let unavailable = action.session_id.and(action.unavailable);
    let after_checkpoint = usage.is_some_and(|usage| {
        usage.measurement_status == NativeContextMeasurementStatus::AfterCheckpoint
    });
    let mut figures = category_tokens(categories, used);
    let sum = figures.iter().flatten().fold(0u64, |sum, value| sum.saturating_add(*value));
    // The card's figure is the sum of its shares and has to be the occupancy
    // every surface shows: estimates beyond what was measured are brought
    // down to it in proportion, the running total keeping the sum exact.
    if let Some(used) = used.filter(|used| *used < sum) {
        let (mut counted, mut placed) = (0u128, 0u128);
        for figure in figures.iter_mut().flatten() {
            counted += u128::from(*figure);
            let end = counted * u128::from(used) / u128::from(sum);
            *figure = (end - placed) as u64;
            placed = end;
        }
    }
    let width = popover_width(props.width);
    // shortcut: the card cuts neither its badge nor an action's words, so both
    // are fitted here by widths read off the app's faces; drop this when the
    // component fits them itself. Under 296 the conversation's row has no room
    // for the words beside its label and figure, and the action keeps its icon.
    let words = |text: &str| if width >= 296.0 { text.to_owned() } else { String::new() };
    let shares: Vec<_> = categories
        .iter()
        .zip(figures)
        .map(|(category, tokens)| {
            let share = ContextShare {
                action: match category.id.as_str() {
                    "conversation" if action.busy => Some(ContextShareAction::Busy(words("Compactando"))),
                    "conversation" if after_checkpoint => Some(ContextShareAction::Done(words("Compactada"))),
                    "conversation" if compact.is_some() => Some(ContextShareAction::Offer(words("Compactar"))),
                    _ => None,
                },
                ..ContextShare::new(
                    &category.id,
                    &category.label,
                    tokens.unwrap_or(0),
                    theme::context_category_ink(&category.id),
                )
            };
            // A category without a figure is listed without a count, not as zero.
            if tokens.is_some() { share } else { share.unmeasured() }
        })
        .collect();
    // The card states a window only where the ring does: with an occupancy.
    let window = occupancy.map(|occupancy| occupancy.capacity);
    let in_focus = hovered.and_then(|id| shares.iter().find(|share| share.id == id));
    // The category in focus opens the status line and reads stronger there.
    let lead = in_focus.filter(|_| window.is_some()).map(|share| share.label.clone());
    let line = if let (Some(share), Some(window)) = (in_focus, window) {
        format!(
            "{} · {}% de la ventana",
            share.label,
            (share.tokens.saturating_mul(100) + window / 2) / window
        )
    } else if action.busy {
        "Compactando…".into()
    } else if let Some(reason) = unavailable {
        reason.into()
    } else if let Some(occupancy) = occupancy {
        format!("{} libres", tokens(occupancy.capacity.saturating_sub(occupancy.used)))
    } else {
        "Ventana desconocida".into()
    };
    let mut card = ContextWindow::new(
        CONTEXT_CARD,
        if input_only {
            "Uso de entrada"
        } else {
            "Uso de contexto"
        },
        shares,
        window.unwrap_or(0),
        theme::menu_palette(),
        ContextTones {
            warning: theme::CONTEXT_WARNING,
            // The card turns to danger at 95%; the app stays amber until the window is full.
            danger: usage_ink(fraction),
            success: theme::DIFF_ADDED,
        },
    )
    .caption(match window {
        Some(window) => format!("de {} tokens", tokens(window)),
        None => "tokens".into(),
    })
    .focused(hovered)
    .status(line)
    .header_icon("composer_context_copy_diagnostic", Icon::Copy, "Copiar diagnóstico")
    .footer_action("new_session_context", "Nueva tarea")
    // The popover's surface is the card: the content stands on it, with no
    // frame or card of its own, one pixel in for the surface's border.
    .flat()
    .width(width - 2.0)
    .format(card_tokens);
    if let Some(lead) = lead {
        card = card.status_lead(lead);
    }
    // The model's own name, the last segment of its ID, in the characters that
    // fit beside the title: 150 px go to the title, the insets and the badge's
    // padding, and the monospace face advances 6.7 px.
    let room = ((width - 150.0) / 6.7) as usize;
    let model = usage.and_then(|usage| usage.model.rsplit('/').next()).unwrap_or("");
    if !model.is_empty() && room > 8 {
        card = card.badge(crate::view::shorten(model, room - 1));
    }
    // The rows scroll; the status and its action stay under them.
    let (mut card, card_foot) = card.parts();
    if let Some(compact) = &compact {
        let control = format!("{CONTEXT_CARD}:action:conversation");
        dispatch_as(&mut card, &control, compact);
        card = Tooltip::new(card, control, "Compactar conversación", theme::menu_palette())
            .description("Resume mensajes anteriores y conserva la conversación. El historial guardado no se elimina.")
            .placement(TooltipPlacement::Left)
            .render();
    }
    // The card builds its rows itself, so each row's hint hangs on the whole
    // card: it states what was measured, in the app's own units.
    for category in categories {
        let mut hint = Tooltip::new(
            card,
            format!("{CONTEXT_CARD}:share:{}", category.id),
            &category.label,
            theme::component_palette(),
        )
        .placement(TooltipPlacement::Left);
        if !category.value.trim().is_empty() {
            hint = hint.description(&category.value);
        }
        if let Some(reason) = unavailable.filter(|_| category.id == "conversation") {
            hint = hint.footer(reason);
        }
        card = hint.render();
    }
    // One status line at most; a normal measurement needs none.
    let failure = usage
        .and_then(|usage| usage.error.as_ref())
        .map(|error| format!("{}: {error}", "Consulta no disponible"));
    let status = if let Some(failure) = &failure {
        Some(failure.as_str())
    } else if after_checkpoint {
        Some("El resumen está guardado. El uso se medirá en la próxima respuesta.")
    } else if fraction.is_some_and(|fraction| fraction >= 0.8) {
        Some("Queda poco espacio. Puedes compactar para continuar.")
    } else {
        None
    };
    // Why there is no figure, or why it is only an estimate, and for which model.
    let reason = match usage {
        _ if status.is_some() => None,
        // Neither the provider nor the catalog states one for this model.
        Some(usage) if usage.error.is_none() && capacity.is_none() => {
            Some(format!("Ventana no publicada: {}", usage.model))
        }
        Some(usage) if estimated => Some(
            match usage.measurement_status {
                NativeContextMeasurementStatus::Measured => "Estimado hasta la próxima respuesta medida.",
                NativeContextMeasurementStatus::Unreported => "Estimado: el proveedor no informó el uso.",
                NativeContextMeasurementStatus::RouteMismatch => "Estimado: sin uso medido con este modelo.",
                _ => "Estimado: aún no hay una respuesta medida.",
            }
            .into(),
        ),
        Some(usage) if occupancy.is_none() => Some(usage.measurement_label().into()),
        Some(_) => None,
        None => Some("Aún no hay una respuesta medida".into()),
    };
    let status = status.or(reason.as_deref());
    // Under the rows and outside their scroll area, so both show in a short window too.
    let mut foot = vec![card_foot];
    if let Some(status) = status {
        foot.extend([
            inset(
                Text::new(status, theme::component_palette())
                    .role(TextRole::Small)
                    .ink(if fraction.is_some() {
                        usage_ink(fraction)
                    } else {
                        theme::FOREGROUND_SUBTLE
                    })
                    .render(),
            ),
            Element::spacer().size(None, Some(8.0)),
        ]);
    }
    let panel = PopoverPanel::new(theme::component_palette())
        .body(
            ScrollArea::new("composer_context_summary", card)
                // An overlay keeps the width the card was sized for.
                .appearance(ScrollbarAppearance::overlay())
                .render(),
        )
        .sizing(crate::ui_sizing::CONTEXT_POPOVER);
    // The popover closes with Escape, a click outside it or its ring, so it
    // needs no button for that.
    panel.footer(Element::column(foot)).render()
}

fn popover_width(composer: f32) -> f32 {
    (composer - 16.0).clamp(96.0, 320.0)
}

pub(crate) fn control_width(props: &ComposerProps<'_>) -> f32 {
    let _ = props;
    28.0
}
pub(crate) fn control(props: &ComposerProps<'_>) -> Element {
    let occupancy = props.native_context.and_then(|usage| usage.occupancy());
    let fraction = occupancy.map(|occupancy| occupancy.fraction());
    let help = props.native_context.map_or_else(
        || "El uso se informa al recibir una respuesta del modelo.".into(),
        |usage| match occupancy {
            Some(occupancy) => format!(
                "{} de {} tokens · {}{}. Abrir contexto y compactación.",
                tokens(occupancy.used),
                tokens(occupancy.capacity),
                if occupancy.estimated { "estimación" } else { "última respuesta" },
                if usage.window_source.is_empty() {
                    String::new()
                } else {
                    format!(" · ventana según {}", usage.window_source)
                }
            ),
            None if usage.error.is_none() && usage.capacity().is_none() => {
                format!("Ventana no publicada: {}", usage.model)
            }
            None => usage.measurement_label().into(),
        },
    );
    let trigger = Tooltip::new(
        ContextMeter::new("composer_context_toggle", None, theme::component_palette())
            .fraction(fraction)
            .estimated(occupancy.is_some_and(|occupancy| occupancy.estimated))
            .usage_inks(theme::CONTEXT_WARNING, theme::CONTEXT_FULL)
            .compact(true)
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
    let mut popover = Popover::new(
        trigger,
        summary(props),
        true,
        "composer_context_dismiss",
        theme::menu_palette(),
    )
    .width(popover_width(props.width))
    .placement(PopoverPlacement::TopEnd)
    .render()
    .popover_return_focus("composer_context_toggle");
    // The card brings its own inset and its foot reaches the surface's edges.
    if let ElementKind::Popover { floating, .. } = &mut popover.kind {
        floating.style.padding = 1.0;
    }
    popover
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

    /// (id, the bytes its value states).
    fn categories(values: &[(&str, Option<usize>)]) -> Vec<NativeContextCategory> {
        values
            .iter()
            .map(|(id, bytes)| {
                // Text that names other sizes: the figure is never read from it.
                let category = NativeContextCategory::new(*id, *id, "999999 bytes · 7 docs", "");
                bytes.map_or(category.clone(), |bytes| category.bytes(bytes))
            })
            .collect()
    }
    const USUAL: [(&str, Option<usize>); 7] = [
        ("system", Some(45_685)),
        ("tools", None),
        ("project", Some(0)),
        ("skills", Some(2_048)),
        ("mcp", None),
        ("subagents", None),
        ("conversation", None),
    ];

    #[test]
    fn categories_estimate_tokens_from_bytes_and_the_conversation_takes_the_rest() {
        let breakdown = categories(&USUAL);
        // The system copy holds the skills: (45685 - 2048) / 4 rounds to 10909.
        // A category without a size has no figure, whatever its text says.
        assert_eq!(
            category_tokens(&breakdown, Some(30_000)),
            [Some(10_909), None, Some(0), Some(512), None, None, Some(18_579)]
        );
        // Accumulated conversation tokens are never the current occupancy.
        assert_eq!(category_tokens(&breakdown, None)[6], None);
        // The estimates can exceed a small measurement; the rest floors at zero.
        assert_eq!(category_tokens(&breakdown, Some(100))[6], Some(0));
        assert_eq!(category_tokens(&categories(&[("system", Some(12))]), None), [Some(3)]);
        // A size on the conversation is not its occupancy either.
        assert_eq!(category_tokens(&categories(&[("conversation", Some(400))]), None), [None]);
    }

    #[test]
    fn project_and_skills_inside_the_system_copy_are_counted_once() {
        let breakdown = categories(&[
            ("system", Some(8_192)),
            ("project", Some(4_096)),
            ("skills", Some(2_048)),
            ("conversation", None),
        ]);
        let tokens = category_tokens(&breakdown, Some(10_000));
        assert_eq!(tokens, [Some(512), Some(1_024), Some(512), Some(7_952)]);
        // Together they are the copy itself, not the copy plus its parts.
        assert_eq!(tokens[..3].iter().flatten().sum::<u64>(), 8_192 / 4);
        // Documents that grew since the copy was frozen leave no system share
        // rather than a negative one.
        let grown = categories(&[("system", Some(1_000)), ("project", Some(4_096))]);
        assert_eq!(category_tokens(&grown, None), [Some(0), Some(1_024)]);
        // Without an observed project or skills the copy is all system.
        assert_eq!(category_tokens(&categories(&[("system", Some(8_192))]), None), [Some(2_048)]);
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
