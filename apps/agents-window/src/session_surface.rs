//! Session surface composed from the persisted/live transcript projection.
//!
//! A turn is visually anchored by its user prompt. Tools retain their own
//! compact disclosure through preparation, execution and review. No task count,
//! duration, or file change is inferred from text: only transcript facts appear.

use std::collections::HashSet;

use chrono::{Local, TimeZone};
use openide_core::{OutputStream, PlanStepStatus};
use openide_ui::{
    Alert, AlertTone, Button, ButtonGroup, ButtonVariant, Card, CardDensity, ChatMinimap,
    ChatMinimapMark, Collapse, Component, ControlSize, Element, FontFamily, Icon, IconButton as UiIconButton, ListRow, Marker,
    MarkerVariant, PageColumn, ScrollArea, Separator, Skeleton, Text, TextArea, TextRole, Ticker,
    ToolActivity, Tooltip, TooltipPlacement,
};

use crate::{
    appearance_preferences::ToolCallDensity,
    composer_ui::{build_composer_with_height, build_native_context_control, ComposerProps},
    message_actions::{Turn, UserEdit},
    rich_text, theme, tool_card_skeleton,
    tool_presentation::{self, ToolKind},
    transcript::{
        duration_label, ActivityStatus, ModelStreamStatus, TranscriptActivity, TranscriptImage,
        TranscriptItem,
    },
    view::{
        sidebar_reopen_control, AbortedToolBatchView, NativeAutomationSessionView, PermissionView,
        SessionPlanView, SessionRecoveryView, SubagentPermissionView, SubagentRunView,
        ToolVerificationView, ViewState,
    },
};

pub(crate) const READING_WIDTH: f32 = 780.0;
const COMPACT_BREAKPOINT: f32 = 470.0;
// The inset of `ui_sizing::PANE_HEADER`, which the header is built with.
#[cfg(test)]
const HEADER_PADDING: f32 = 4.0;
const HEADER_HEIGHT: f32 = crate::dock_tab_ui::HEADER_HEIGHT;
const HEADER_FADE_HEIGHT: f32 = 42.0;
/// Where a pinned user message rests below the header, and the height it
/// keeps there: three lines of 21 inside the card's padding.
const USER_PIN_INSET: f32 = 10.0;
/// Room the rail of turns needs at the left of the reading column.
const TURN_RAIL_ROOM: f32 = 56.0;
/// A turn is in view once its message has come this close to the top.
const TURN_IN_VIEW_LINE: f32 = 120.0;
/// As much of a message as its mark can show.
const TURN_MARK_TEXT: usize = 400;
const USER_PIN_HEIGHT: f32 = 84.0;
/// The turn fades out under the pinned message instead of being cut at its edge.
const USER_PIN_FADE: f32 = 20.0;
pub(crate) const TRANSCRIPT_BOTTOM_FADE_HEIGHT: f32 = 32.0;
const HEADER_ACTION_SIZE: f32 = crate::sidebar_chrome::TOOL_SIZE;
const HEADER_GAP: f32 = 4.0;
/// Thinking, exploration and waiting are flat text lines of this height.
const STREAM_LINE_HEIGHT: f32 = 22.0;
/// Between the rows of the transcript.
const TRANSCRIPT_GAP: f32 = 8.0;
const WAITING_LABEL: &str = "Pensando el siguiente paso";

/// Humanize known provider failures only at presentation time. The original
/// error stays intact in the transcript and journal for replay and diagnosis.
pub(crate) fn conversation_notice_message(message: &str, is_error: bool) -> &str {
    if is_error && message.contains("subscription_sharing_usage_limit_exceeded") {
        "Se alcanzó un límite de uso de ChatGPT en openADE. Revisa los límites de esta app en ChatGPT → Configuración → Uso; abre esa página desde Configuración de proveedores → Ver uso."
    } else if is_error && message.contains("subscription_sharing_usage_unavailable") {
        "ChatGPT no pudo comprobar la disponibilidad de uso para este turno. Inténtalo de nuevo más tarde."
    } else {
        message
    }
}

#[cfg(test)]
#[path = "conversation_marker_tests.rs"]
mod marker_tests;

#[cfg(test)]
#[path = "pause_visual_tests.rs"]
mod pause_visual_tests;
#[cfg(test)]
#[path = "conversation_stream_visual_tests.rs"]
mod stream_visual_tests;

#[cfg(test)]
#[path = "conversation_plan_visual_tests.rs"]
mod plan_visual_tests;

#[cfg(test)]
#[path = "tool_card_skeleton_tests.rs"]
mod tool_card_skeleton_tests;

#[cfg(test)]
#[path = "file_activity_visual_tests.rs"]
mod file_activity_visual_tests;

#[cfg(test)]
#[path = "chat_dock_visual_tests.rs"]
mod chat_dock_visual_tests;

#[cfg(test)]
#[path = "sticky_edit_rewind_visual_tests.rs"]
mod sticky_edit_rewind_visual_tests;

#[cfg(test)]
#[path = "message_actions_visual_tests.rs"]
mod message_actions_visual_tests;

#[cfg(test)]
#[path = "chat_links_visual_tests.rs"]
mod chat_links_visual_tests;

#[cfg(test)]
#[path = "chat_diagram_visual_tests.rs"]
mod chat_diagram_visual_tests;

pub(crate) fn user_image(image: &TranscriptImage) -> Element {
    let preview = image.raster.as_ref().map_or_else(
        || {
            Element::row(vec![
                Element::icon(Icon::Paperclip).foreground(theme::FOREGROUND_SUBTLE),
                Text::new("Vista previa no disponible", theme::component_palette())
                    .muted()
                    .render(),
            ])
            .gap(7.0)
        },
        |raster| {
            Element::raster_image(raster.clone())
                .size(Some(280.0), Some(180.0))
                .radius(7.0)
        },
    );
    Element::column(vec![
        preview,
        Text::new(&image.alt, theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .render(),
    ])
    .gap(5.0)
}

fn ellipsis(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let start: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{start}…")
    } else {
        start
    }
}

fn session_name(state: &ViewState) -> &str {
    state
        .selected_session_title
        .as_deref()
        .or_else(|| {
            state.selected_session.as_ref().and_then(|id| {
                state
                    .sessions
                    .iter()
                    .find(|session| &session.id == id)
                    .map(|session| session.title.as_str())
            })
        })
        .unwrap_or("Nueva tarea")
}

fn selected_native_automation(state: &ViewState) -> Option<&NativeAutomationSessionView> {
    state
        .selected_session
        .as_deref()
        .and_then(|id| {
            state
                .sessions
                .iter()
                .find(|session| session.id == id && session.backend == "native")
        })
        .and_then(|session| session.native_automation.as_ref())
}

pub(crate) fn header(
    state: &ViewState,
    width: f32,
    side_visible: bool,
    side_available: bool,
) -> Element {
    let sidebar_hidden = state
        .left_presentation_width
        .map_or(state.sidebar_collapsed, |width| width <= 1.0);
    let recovery_decision_pending =
        state.pending_tool_verification.is_some() || state.pending_batch_discard.is_some();
    let show_side_toggle =
        !recovery_decision_pending && !side_visible && side_available && width >= 300.0;
    let show_settings = !recovery_decision_pending && width >= 220.0;
    let header_action = |id: &str, icon: Icon, hint: &str| {
        Tooltip::new(
            UiIconButton::new(
                id,
                icon,
                theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Dense)
            .render(),
            id,
            hint,
            theme::component_palette(),
        )
        .placement(TooltipPlacement::Below)
        .render()
    };
    let mut top = Vec::new();
    if sidebar_hidden {
        top.push(sidebar_reopen_control(theme::BACKGROUND));
    }
    let cli = state
        .selected_backend
        .strip_prefix(crate::view::CLI_BACKEND)
        .filter(|_| crate::view::cli_task(state));
    if let Some(agent) = cli {
        top.push(Element::spacer().size(Some(4.0), None));
        top.push(
            Element::icon(crate::provider_icons::icon_for_agent_cli(agent))
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(16.0), Some(16.0))
                .centered_y()
                .size(Some(16.0), Some(HEADER_ACTION_SIZE)),
        );
    }
    let before = std::mem::take(&mut top);
    let title = Element::faded_text(session_name(state), 24.0)
        .foreground(theme::FOREGROUND)
        .font_family(FontFamily::Sans)
        .font_size(13.0)
        .shrink_to_fit()
        .size(None, Some(HEADER_ACTION_SIZE));
    let mut top = Vec::new();
    if cli.is_some() {
        top.push(header_action(
            "cli_task_restart",
            Icon::Refresh,
            "Reiniciar la CLI en una terminal nueva",
        ));
    }
    if !recovery_decision_pending && width >= 300.0 {
        top.push(crate::view::window_mode_control(state));
        top.push(crate::environment_ui::header_control(state));
    }
    if show_side_toggle {
        top.push(header_action(
            "toggle_side_pane_header",
            Icon::PanelRightOpen,
            "Mostrar panel derecho: archivos, cambios, terminal y navegador",
        ));
    }
    if show_settings {
        top.push(header_action(
            "toggle_settings_header",
            Icon::Settings,
            "Abrir configuración de openADE",
        ));
    }
    // The title sits in the middle of the header: what stands on one side
    // is matched by empty room on the other, as far as the width allows.
    let slot = HEADER_ACTION_SIZE + HEADER_GAP;
    let (left, right) = (before.len() as f32 * slot, top.len() as f32 * slot);
    let mut leading = before;
    leading.extend([
        Element::spacer().size(Some((right - left).max(0.0)), None).shrink_to_fit(),
        Element::spacer().flex(1.0),
        title,
        Element::spacer().flex(1.0),
        Element::spacer().size(Some((left - right).max(0.0)), None).shrink_to_fit(),
    ]);
    openide_ui::PanelHeader::new("", theme::component_palette())
        .leading(Element::row(leading).gap(HEADER_GAP))
        .trailing(top)
        .sizing(crate::ui_sizing::PANE_HEADER)
        .render()
}

/// A user's message. It stays at the top of the transcript while its turn
/// scrolls under it, giving up height so a long prompt cannot cover the
/// turn, and offers to stop the turn while it runs.
fn user_message(
    content: &str,
    images: &[TranscriptImage],
    item_index: usize,
    state: &ViewState,
    running: bool,
    edit: UserEdit,
    width: f32,
) -> Element {
    let mut contents = vec![rich_text::render_user_message(content, &format!(
        "user:{}:{item_index}", state.selected_session.as_deref().unwrap_or("draft")
    ))];
    for image in images {
        contents.push(user_image(image));
    }
    let mut parts = vec![Element::column(contents).gap(8.0).flex(1.0)];
    parts.extend(crate::message_actions::user_actions(state, item_index, edit));
    if running {
        parts.push(crate::composer_ui::stop_button("turn_stop", ControlSize::Dense, "Detener ejecución"));
    }
    let card = Card::new(Element::row(parts).gap(8.0), theme::card_palette()).border(false).render();
    match edit {
        UserEdit::None | UserEdit::Blocked(_) => card,
        UserEdit::Offered(user_row) => crate::message_edit::clickable_bubble(card, user_row),
        UserEdit::Open => crate::message_edit::editor(state, card, width),
    }
    // Its actions show while the pointer is over the bubble, pinned or not.
    .hover_region()
    .anchor(turn_anchor(item_index))
    .sticky_top_inset(USER_PIN_INSET)
    .sticky_top_max_height(USER_PIN_HEIGHT)
    .sticky_top_fade(USER_PIN_FADE)
}

/// What the message at transcript row `item_index` is revealed by.
pub(crate) fn turn_anchor(item_index: usize) -> String {
    format!("turn_anchor:{item_index}")
}

/// The action of a turn's mark in the rail: `turn_jump:{transcript row}`.
pub(crate) const TURN_JUMP: &str = "turn_jump:";

/// Where a jump leaves the message: its place when pinned.
pub(crate) const TURN_JUMP_MARGIN: f32 = USER_PIN_INSET;

fn user_rows(state: &ViewState) -> impl Iterator<Item = usize> + '_ {
    state
        .transcript
        .iter()
        .enumerate()
        .filter_map(|(index, item)| matches!(item, TranscriptItem::User { .. }).then_some(index))
}

/// The transcript row of the message whose turn is at the top of the view,
/// from the last layout: the last one that has reached the top.
pub(crate) fn turn_in_view(state: &ViewState, tree: &openide_ui::UiTree) -> Option<usize> {
    let mut rows = user_rows(state);
    let first = rows.next()?;
    Some(
        rows.take_while(|row| {
            tree.anchor_offset(&turn_anchor(*row))
                .is_some_and(|top| top <= TURN_IN_VIEW_LINE)
        })
        .last()
        .unwrap_or(first),
    )
}

/// The start of `text`, cut at a character.
fn head(text: &str) -> &str {
    text.char_indices().nth(TURN_MARK_TEXT).map_or(text, |(end, _)| &text[..end])
}

/// The conversation's turns as marks at the left of the reading column: a
/// mark shows how its turn opens and how the reply starts, and jumps to it.
/// It needs two turns and room beside the column, and is the Agents window's.
fn turn_rail(state: &ViewState, center_width: f32) -> Option<Element> {
    // The Agents window's conversation only: the IDE's chat is a side panel.
    if state.ide.mode != crate::ide_workspace::WindowMode::Agents
        || center_width < READING_WIDTH + 2.0 * TURN_RAIL_ROOM
    {
        return None;
    }
    let mut turns: Vec<(usize, &str, &str)> = Vec::new();
    for (index, item) in state.transcript.iter().enumerate() {
        match item {
            TranscriptItem::User { content, images } => {
                let title = match (content.trim(), images.is_empty()) {
                    ("", false) => "Imagen",
                    ("", true) => "Mensaje",
                    (text, _) => text,
                };
                turns.push((index, title, ""));
            }
            TranscriptItem::Assistant { content, .. } => {
                if let Some(turn) = turns.last_mut().filter(|turn| turn.2.is_empty()) {
                    turn.2 = content.trim();
                }
            }
            _ => {}
        }
    }
    if turns.len() < 2 {
        return None;
    }
    let marks = turns
        .into_iter()
        .map(|(index, title, preview)| {
            ChatMinimapMark::new(format!("{TURN_JUMP}{index}"), head(title)).preview(head(preview))
        })
        .collect();
    let mut rail = ChatMinimap::new("turn_rail", marks, theme::component_palette());
    if let Some(row) = state.turn_in_view {
        rail = rail.active(format!("{TURN_JUMP}{row}"));
    }
    Some(rail.render())
}

fn message(
    item: &TranscriptItem,
    item_index: usize,
    reading_width: f32,
    state: &ViewState,
) -> Element {
    match item {
        TranscriptItem::User { content, images } => {
            user_message(content, images, item_index, state, false, UserEdit::None, 0.0)
        }
        TranscriptItem::Assistant { content, streaming } => {
            let visible = state.stream_presentation.visible_bytes(item_index, content);
            let scope = format!(
                "assistant:{}:{item_index}", state.selected_session.as_deref().unwrap_or("draft")
            );
            let preview = rich_text::render_scoped_markdown_preview(
                if *streaming { &content[..visible] } else { content },
                reading_width.max(96.0),
                &scope,
                *streaming,
            );
            let mut rows = vec![rich_text::scope_web_links(preview.element, &scope)];
            if preview.limited && !*streaming && crate::conversation_source_ui::available(state) {
                // Keep actions outside the cached passive Markdown tree.
                rows.push(crate::conversation_source_ui::open_button(
                    state, item_index,
                ));
            }
            Element::column(rows).gap(8.0)
        }
        TranscriptItem::Reasoning { .. } | TranscriptItem::ToolPreview { .. } => {
            model_stream_item(item, item_index, state, reading_width)
        }
        TranscriptItem::ModelUsage { .. } => Element::spacer(),
        TranscriptItem::WebSearch { .. } => crate::search_activity_ui::hosted(item, state),
        TranscriptItem::PlanSnapshot { .. } => plan_snapshot_item(item, item_index, state),
        TranscriptItem::Hook(hook) => hook_activity_card(hook),
        TranscriptItem::Activity(activity) => activity_marker(activity),
        TranscriptItem::WebCitation { title, url } => web_citation(item_index, title, url),
        TranscriptItem::Notice { message, is_error } => Alert::new(
            if *is_error {
                "Error en la conversación"
            } else {
                "Aviso de la conversación"
            },
            conversation_notice_message(message, *is_error),
            theme::component_palette(),
        )
        .tone(if *is_error {
            AlertTone::Error
        } else {
            AlertTone::Info
        })
        .compact(true)
        .render(),
        TranscriptItem::ProcessOutput {
            stream,
            text: output,
        } => {
            let (title, ink) = match stream {
                OutputStream::Stdout => ("SALIDA DEL PROCESO", theme::FOREGROUND_SUBTLE),
                OutputStream::Stderr => ("DIAGNÓSTICOS DEL PROCESO", theme::FOREGROUND_SUBTLE),
            };
            Card::new(
                Element::column(vec![
                    Text::new(title, theme::component_palette())
                        .role(TextRole::Micro)
                        .ink(ink)
                        .render(),
                    Text::new(ellipsis(output, 700), theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .mono()
                        .render(),
                ])
                .gap(7.0),
                theme::panel_card_palette(),
            )
            .density(CardDensity::Roomy)
            .render()
            // Radius 9 is this card's own; see docs/limn-migration-ledger.md.
            .radius(9.0)
        }
        TranscriptItem::Tool { .. } => unreachable!("tools own their captured output"),
        TranscriptItem::ToolOutput { .. } | TranscriptItem::ToolMedia { .. } => Element::spacer(),
    }
}

fn detail_action(state: &ViewState, id: &str) -> String {
    format!(
        "conversation_detail:{}:{id}",
        state.selected_session.as_deref().unwrap_or("draft")
    )
}

/// One line of flat transcript text, cut with an ellipsis.
fn stream_text(text: impl Into<String>, ink: openide_ui::Color) -> Element {
    Element::truncated_text(text)
        .foreground(ink)
        .font_family(FontFamily::Sans)
        .font_size(13.0)
        .size(None, Some(STREAM_LINE_HEIGHT))
        .shrink_to_fit()
}

/// As `stream_text` with a sweeping highlight, for work in flight: muted, or
/// in the normal ink when `strong`.
fn stream_shimmer(id: String, text: &str, strong: bool) -> Element {
    let (ink, highlight) = if strong {
        (theme::FOREGROUND, theme::BRAND)
    } else {
        (
            theme::FOREGROUND_SUBTLE,
            openide_ui::thinking_highlight(theme::component_palette()),
        )
    };
    Element::shimmer_truncated_text(id, text, highlight)
        .foreground(ink)
        .font_family(FontFamily::Sans)
        .font_size(13.0)
        .size(None, Some(STREAM_LINE_HEIGHT))
        .shrink_to_fit()
}

/// A flat transcript line: no card, border or icon column. `expanded` adds
/// the disclosure chevron after the text.
fn stream_line(mut parts: Vec<Element>, action: Option<String>, expanded: Option<bool>) -> Element {
    if let Some(expanded) = expanded {
        parts.push(
            Element::icon(if expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(14.0), Some(14.0))
            .centered_y(),
        );
    }
    let row = Element::row(parts)
        .gap(5.0)
        .size(None, Some(STREAM_LINE_HEIGHT))
        .clip_children();
    match action {
        Some(action) => Element::clickable(action, row),
        None => row,
    }
}

/// Whether this reasoning chunk is the one being received right now. Prose
/// or a tool after it means the model has moved on.
fn reasoning_is_live_tail(state: &ViewState, id: &str) -> bool {
    matches!(state.transcript.iter().rev().find(|item| !matches!(item,
        TranscriptItem::ModelUsage { .. } | TranscriptItem::PlanSnapshot { .. }
    )), Some(TranscriptItem::Reasoning { id: tail_id, status: ModelStreamStatus::Receiving, content, .. })
        if tail_id == id && !content.is_empty())
}

pub(crate) fn model_stream_item(
    item: &TranscriptItem,
    item_index: usize,
    state: &ViewState,
    reading_width: f32,
) -> Element {
    if let Some(file) =
        crate::file_activity_ui::build(item, state, reading_width < COMPACT_BREAKPOINT, None, None)
    {
        return file;
    }
    if let Some(search) = crate::search_activity_ui::build(item, state) {
        return search;
    }
    match item {
        TranscriptItem::Reasoning {
            id,
            content,
            status,
            truncated,
            ..
        } => {
            let receiving = *status == ModelStreamStatus::Receiving;
            let owner = state.selected_session.as_deref().unwrap_or("draft");
            let expanded = state.conversation_details.is_expanded(id, false);
            let label = if reasoning_is_live_tail(state, id) {
                stream_shimmer(format!("reasoning:{owner}:{id}"), "Pensando…", false)
            } else {
                stream_text(
                    format!(
                        "Pensó {}",
                        duration_label(state.item_times.duration_ms(id).unwrap_or(0))
                    ),
                    theme::FOREGROUND_SUBTLE,
                )
            };
            let line = stream_line(vec![label], Some(detail_action(state, id)), Some(expanded));
            let body = expanded.then(|| {
                let visible = &content[..state.stream_presentation.visible_bytes(item_index, content)];
                let scope = format!(
                    "reasoning:{}:{item_index}", state.selected_session.as_deref().unwrap_or("draft")
                );
                let preview = rich_text::render_scoped_markdown_preview(
                    if receiving { visible } else { content },
                    reading_width.max(48.0),
                    &scope,
                    receiving,
                );
                let mut rows = vec![rich_text::scope_web_links(preview.element, &scope)];
                if *truncated {
                    rows.push(
                        Text::new("Vista de razonamiento limitada", theme::component_palette())
                            .role(TextRole::Micro)
                            .muted()
                            .render(),
                    );
                }
                ScrollArea::new(
                    format!(
                        "reasoning-body:{}:{id}",
                        state.selected_session.as_deref().unwrap_or("draft")
                    ),
                    Element::column(rows).gap(6.0),
                )
                .tint(theme::FOREGROUND_SUBTLE)
                .render()
                .max_height(160.0)
            });
            // The text opens and closes under its line instead of jumping.
            Element::column(vec![
                line,
                Collapse::new(format!("reasoning-open:{owner}:{id}"), body)
                    .expand()
                    .render(),
            ])
        }
        TranscriptItem::ToolPreview {
            id,
            call_id,
            name,
            arguments,
            input_complete,
            status,
            truncated,
            ..
        } => {
            let status_label = match status {
                ModelStreamStatus::Receiving if *input_complete => "Entrada recibida",
                ModelStreamStatus::Receiving => "Preparando",
                ModelStreamStatus::Committed => "Pendiente",
                ModelStreamStatus::Interrupted | ModelStreamStatus::Failed => "No ejecutada",
                ModelStreamStatus::Cancelled => "Cancelada",
                ModelStreamStatus::Discarded => "Descartada · sin ejecutar",
            };
            if let Some(card) = pending_call_card(item, state) {
                return card;
            }
            // Until the call's name arrives there is no title to show.
            let unnamed = name.is_none() && tool_card_skeleton::pending_call(item);
            let name = name.as_deref().unwrap_or("Herramienta");
            let disclosure = call_id.as_ref().map(|id| format!("tool:{id}"));
            let disclosure = disclosure.as_deref().unwrap_or(id);
            let expanded = state.conversation_details.is_expanded(
                disclosure,
                conversation_detail_default_open(state, disclosure),
            );
            let preview = tool_presentation::present(
                name,
                &tool_presentation::preview_arguments(arguments),
                ActivityStatus::Running,
                false,
                None,
            );
            let mut rows = Vec::new();
            if expanded {
                rows.push(
                    Text::new(
                        "Entrada provisional · todavía sin ejecutar",
                        theme::component_palette(),
                    )
                    .role(TextRole::Small)
                    .muted()
                    .render(),
                );
                if !arguments.is_empty() {
                    rows.push(
                        Text::new(arguments, theme::component_palette())
                            .role(TextRole::Small)
                            .muted()
                            .mono()
                            .render(),
                    );
                }
                if *truncated {
                    rows.push(
                        Text::new("Vista de argumentos limitada", theme::component_palette())
                            .role(TextRole::Micro)
                            .muted()
                            .render(),
                    );
                }
            }
            let row = ToolActivity::new(
                detail_action(state, disclosure),
                format!("{status_label} · {}", tool_operation(name)),
                Element::column(rows).gap(6.0),
                theme::component_palette(),
            )
            .icon(preview.icon)
            .target(preview.target)
            .command(preview.kind == ToolKind::Command)
            .running(*status == ModelStreamStatus::Receiving)
            .title_skeleton(unnamed)
            .expanded(expanded)
            .render();
            let explanation = match status {
                ModelStreamStatus::Failed => Some(
                    "El modelo falló antes de confirmar esta llamada. La herramienta no se ejecutó.",
                ),
                ModelStreamStatus::Interrupted => Some(
                    "La solicitud se interrumpió antes de confirmar esta llamada. La herramienta no se ejecutó.",
                ),
                ModelStreamStatus::Cancelled => Some(
                    "La solicitud se canceló antes de confirmar esta llamada. La herramienta no se ejecutó.",
                ),
                ModelStreamStatus::Discarded => {
                    Some("La llamada confirmada fue descartada sin ejecutar.")
                }
                _ => None,
            };
            if let Some(explanation) = explanation {
                Tooltip::new(
                    row,
                    detail_action(state, disclosure),
                    status_label,
                    theme::component_palette(),
                )
                .description(explanation)
                .placement(TooltipPlacement::Right)
                .render()
            } else {
                row
            }
        }
        _ => Element::spacer(),
    }
}

/// A call the model is still writing, drawn as the card it becomes: the
/// terminal for a command and the plan card for a plan update. Skeleton bars
/// hold the place of the text that has not arrived, in rows as tall as the
/// real ones, so the confirmed call replaces them without a change of shape.
/// Every other tool stays the flat row it will be once it runs.
fn pending_call_card(item: &TranscriptItem, state: &ViewState) -> Option<Element> {
    let TranscriptItem::ToolPreview {
        id,
        call_id,
        name: Some(name),
        arguments,
        status,
        ..
    } = item
    else {
        return None;
    };
    if !tool_card_skeleton::pending_call(item) {
        return None;
    }
    let owner = state.selected_session.as_deref().unwrap_or("draft");
    let scope = tool_card_skeleton::scope(owner, id, call_id.as_deref());
    let arguments = tool_presentation::preview_arguments(arguments);
    let receiving = *status == ModelStreamStatus::Receiving;
    if tool_presentation::kind(name) == ToolKind::Command {
        let disclosure = call_id.as_ref().map(|id| format!("tool:{id}"));
        let command = ["command", "cmd"]
            .into_iter()
            .find_map(|key| arguments.get(key).and_then(serde_json::Value::as_str))
            .filter(|value| !value.trim().is_empty());
        let prompt = match command {
            Some(command) => Element::truncated_text(format!("$ {command}").replace(['\r', '\n'], " "))
                .font_family(FontFamily::Monospace)
                .font_size(12.0)
                .foreground(theme::FOREGROUND),
            // The prompt is real text, so the row is as tall as the command
            // line that replaces the bar.
            None => Element::row(vec![
                Element::truncated_text("$")
                    .font_family(FontFamily::Monospace)
                    .font_size(12.0)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .shrink_to_fit(),
                tool_card_skeleton::bar(
                    format!("tool-skeleton:{scope}:0"),
                    0.48,
                    theme::component_palette(),
                )
                .flex(1.0),
            ])
            .gap(7.0),
        };
        let body = Element::column(vec![
            Separator::horizontal(theme::component_palette()).render(),
            Element::column(vec![prompt]).gap(4.0).padding(8.0),
        ]);
        return Some(
            ToolActivity::new(
                detail_action(state, disclosure.as_deref().unwrap_or(id)),
                command.map_or_else(String::new, terminal_command_title),
                body,
                theme::code_palette(),
            )
            .terminal(true)
            .icon(Icon::Terminal)
            .title_skeleton(command.is_none())
            .status(if receiving { "Preparando" } else { "Pendiente" })
            .running(receiving)
            .render(),
        );
    }
    if name == "update_plan" {
        return Some(pending_plan_card(owner, &scope, &arguments));
    }
    if name == crate::question_ui::TOOL {
        return Some(crate::question_ui::skeleton(&scope, &arguments));
    }
    if name == crate::mode_proposal_ui::TOOL {
        return Some(crate::mode_proposal_ui::skeleton(&scope, &arguments));
    }
    None
}

/// The plan card while its steps are still being written.
fn pending_plan_card(owner: &str, scope: &str, arguments: &serde_json::Value) -> Element {
    if let Some(snapshot) = serde_json::from_value::<openide_core::PlanSnapshot>(arguments.clone())
        .ok()
        .filter(openide_core::PlanSnapshot::is_valid)
    {
        // The whole list has arrived: the card the snapshot will draw.
        return session_plan_card(
            owner,
            &SessionPlanView {
                snapshot,
                can_build: false,
            },
            "Plan actualizado",
        );
    }
    let header = Element::row(vec![
        Element::panel(
            Element::icon(Icon::Plan)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(14.0), Some(14.0)),
        )
        .size(Some(14.0), Some(18.0))
        .center_content(),
        Text::new("Plan actualizado", theme::component_palette())
            .render()
            .centered_y()
            .flex(1.0),
        Skeleton::line(
            format!("tool-skeleton:{scope}:count"),
            tool_card_skeleton::on_card(),
            34.0,
        )
        .height(tool_card_skeleton::BAR)
        .radius(tool_card_skeleton::BAR / 2.0)
        .render()
        .centered_y(),
    ])
    .min_size(0.0, 18.0)
    .gap(8.0);
    let steps = [0.62, 0.78, 0.46]
        .into_iter()
        .enumerate()
        .map(|(index, fraction)| {
            Element::row(vec![
                Element::panel(Element::spacer())
                    .size(Some(14.0), Some(14.0))
                    .border(theme::FOREGROUND_SUBTLE, 1.0)
                    .radius(7.0),
                // The step's number is real text, so the row is as tall as
                // the step that replaces the bar.
                Text::new(format!("{}.", index + 1), theme::component_palette())
                    .muted()
                    .render(),
                tool_card_skeleton::bar(
                    format!("tool-skeleton:{scope}:{index}"),
                    fraction,
                    tool_card_skeleton::on_card(),
                )
                .flex(1.0),
            ])
            .gap(8.0)
        })
        .collect::<Vec<_>>();
    let mut content = vec![header];
    if let Some(explanation) = arguments
        .get("explanation")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        content.push(
            Text::new(explanation, theme::component_palette())
                .muted()
                .render(),
        );
    }
    content.push(Element::column(steps).gap(8.0));
    Card::new(Element::column(content).gap(7.0), theme::card_palette()).render()
}

/// A cited page in the transcript: its title over its address.
fn web_citation(index: usize, title: &str, url: &str) -> Element {
    ListRow::new(
        format!("web_citation_open:{index}"),
        title,
        theme::action_palette(theme::PANEL, theme::FOREGROUND),
    )
    .icon(Icon::Search)
    .icon_ink(theme::ICON_PRIMARY)
    .subtitle(url)
    .subtitle_mono(true)
    .filled()
    .outlined()
    .sizing(crate::ui_sizing::WEB_CITATION)
    .render()
}

pub(crate) fn activity_marker(activity: &TranscriptActivity) -> Element {
    match activity {
        TranscriptActivity::Model {
            id,
            has_text,
            status,
            ..
        } => {
            let label = match status {
                ActivityStatus::Running if *has_text => "Generando respuesta",
                ActivityStatus::Running => "Pensando",
                ActivityStatus::Completed => "Respuesta recibida",
                ActivityStatus::Failed => "Solicitud al modelo fallida",
                ActivityStatus::Cancelled => "Solicitud al modelo cancelada",
                ActivityStatus::Interrupted => "Solicitud al modelo interrumpida",
                ActivityStatus::WaitingPermission
                | ActivityStatus::PermissionInterrupted
                | ActivityStatus::Discarded => {
                    unreachable!("models never wait for tool permission")
                }
            };
            let ink = if matches!(status, ActivityStatus::Failed | ActivityStatus::Interrupted) {
                theme::DESTRUCTIVE
            } else {
                theme::FOREGROUND_SUBTLE
            };
            Marker::new(id, label, theme::component_palette())
                .ink(ink)
                .icon(if *status == ActivityStatus::Completed {
                    Icon::Check
                } else {
                    Icon::Code
                })
                .loading(*status == ActivityStatus::Running)
                .thinking(*status == ActivityStatus::Running && !has_text)
                .render()
        }
        TranscriptActivity::Checkpoint {
            id,
            covered_event_sequence,
        } => {
            let title = if id.starts_with("route-migration-") {
                "Contexto conservado para el modelo elegido"
            } else {
                "Contexto compactado"
            };
            Tooltip::new(
                Marker::new(
                    format!("checkpoint:{id}"),
                    title,
                    theme::component_palette(),
                )
                .variant(MarkerVariant::Separator)
                .icon(Icon::History)
                .render(),
                format!("checkpoint:{id}:info"),
                "El resumen está guardado. Puedes continuar la conversación.",
                theme::component_palette(),
            )
            .description(format!("Hasta el evento {covered_event_sequence}"))
            .render()
        }
    }
}

#[cfg(test)]
fn tool_marker_icon(name: &str) -> Icon {
    tool_presentation::icon(name)
}

fn tool_operation(name: &str) -> &str {
    tool_presentation::operation(name)
}

#[cfg(test)]
pub(crate) fn tool_activity_marker(
    owner: Option<&str>,
    id: &str,
    name: &str,
    activity: ActivityStatus,
    selected: bool,
    is_error: bool,
) -> Element {
    let operation = tool_operation(name);
    let status = match activity {
        ActivityStatus::Running => "en curso",
        ActivityStatus::WaitingPermission => "esperando permiso",
        ActivityStatus::PermissionInterrupted => "permiso pendiente · turno interrumpido",
        ActivityStatus::Completed => "completada",
        ActivityStatus::Failed => "error",
        ActivityStatus::Cancelled => "permiso cancelado",
        ActivityStatus::Interrupted => "resultado incierto",
        ActivityStatus::Discarded => "descartada sin ejecutar",
    };
    let ink = if is_error
        || matches!(
            activity,
            ActivityStatus::Failed | ActivityStatus::Interrupted
        ) {
        theme::DESTRUCTIVE
    } else {
        theme::FOREGROUND_SUBTLE
    };
    Marker::new(
        format!("tool-activity:{}:{id}", owner.unwrap_or("draft")),
        format!(
            "{operation} · {} · {status}",
            if name.is_empty() {
                "Nombre no registrado"
            } else {
                name
            }
        ),
        theme::action_palette(
            if selected {
                theme::SELECTED
            } else {
                theme::CARD
            },
            ink,
        ),
    )
    .ink(ink)
    .icon(tool_marker_icon(name))
    .loading(activity == ActivityStatus::Running)
    .action(format!("tool:{id}"))
    .render()
    .background(if selected {
        theme::SELECTED
    } else {
        theme::CARD
    })
}

pub(crate) fn host_activity_markers(state: &ViewState) -> Vec<Element> {
    let mut markers = Vec::new();
    if let Some(session) = state.conversation_activity.compacting_for(
        std::path::Path::new(&state.workspace),
        state.selected_session.as_deref(),
    ) {
        markers.push(
            Marker::new(
                format!("manual-compaction:{session}"),
                "Compactando contexto…",
                theme::component_palette(),
            )
            .variant(MarkerVariant::Border)
            .loading(true)
            .render(),
        );
    }
    if let Some(change) = state.conversation_activity.branch_change(
        std::path::Path::new(&state.workspace),
        state.selected_session.as_deref(),
    ) {
        markers.push(
            Marker::new(
                format!(
                    "branch-observed:{}:{}:{}",
                    state.selected_session.as_deref().unwrap_or("draft"),
                    change.previous,
                    change.current
                ),
                format!(
                    "Rama cambiada: {} → {} · observada en Git",
                    change.previous, change.current
                ),
                theme::component_palette(),
            )
            .variant(MarkerVariant::Separator)
            .icon(Icon::GitBranch)
            .render(),
        );
    }
    markers
}

/// A successful CLI turn keeps stderr in the activity pane, but its startup
/// notices should not interrupt the conversation between prompt and answer.
fn completed_process_diagnostic(items: &[TranscriptItem], index: usize) -> bool {
    if !matches!(
        items.get(index),
        Some(TranscriptItem::ProcessOutput {
            stream: OutputStream::Stderr,
            ..
        })
    ) {
        return false;
    }
    items[index + 1..]
        .iter()
        .take_while(|item| !matches!(item, TranscriptItem::User { .. }))
        .any(|item| {
            matches!(
                item,
                TranscriptItem::Assistant {
                    streaming: false,
                    ..
                }
            )
        })
}

pub(crate) fn conversation_detail_default_open(state: &ViewState, id: &str) -> bool {
    if let Some(call) = id.strip_prefix("work:") {
        return state
            .transcript
            .iter()
            .position(|item| exploration_call(item) == Some(call))
            .is_some_and(|start| {
                let (end, _) = exploration_run(&state.transcript, start);
                state.transcript[start..end]
                    .iter()
                    .any(exploration_needs_attention)
            });
    }
    if let Some(call) = id.strip_prefix("tool:") {
        return state.appearance.tool_call_density == ToolCallDensity::Detailed
            || crate::file_activity_ui::default_expanded(state, call);
    }
    if state.appearance.tool_call_density == ToolCallDensity::Detailed {
        return state.transcript.iter().any(|item| {
            matches!(item, TranscriptItem::ToolPreview { id: preview, .. }
                | TranscriptItem::WebSearch { id: preview, .. } if preview == id)
        });
    }
    if crate::file_activity_ui::default_expanded(state, id) {
        return true;
    }
    // Plan snapshots are always inline and no longer own a disclosure. Other
    // tool activity still preserves its own explicit expansion choice.
    false
}

pub(crate) fn tool_partial_output<'a>(
    state: &'a ViewState,
    index: usize,
    id: &str,
) -> Option<(&'a str, bool)> {
    match state.transcript.get(index + 1) {
        Some(TranscriptItem::ToolOutput {
            tool_call_id,
            text,
            truncated,
        }) if tool_call_id == id => Some((text, *truncated)),
        _ => None,
    }
}

pub(crate) fn tool_media<'a>(
    state: &'a ViewState,
    index: usize,
    id: &str,
) -> Option<&'a TranscriptItem> {
    let next = state.transcript.get(index + 1)?;
    match next {
        TranscriptItem::ToolMedia { tool_call_id, .. } if tool_call_id == id => Some(next),
        TranscriptItem::ToolOutput { tool_call_id, .. } if tool_call_id == id => {
            let next = state.transcript.get(index + 2)?;
            matches!(next, TranscriptItem::ToolMedia { tool_call_id, .. } if tool_call_id == id)
                .then_some(next)
        }
        _ => None,
    }
}

pub(crate) fn tool_media_elements(
    media: &TranscriptItem,
    compact: bool,
    owner: &str,
) -> Vec<Element> {
    let TranscriptItem::ToolMedia {
        tool_call_id,
        images,
        text: content,
        notices,
        ..
    } = media
    else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (index, image) in images.iter().enumerate() {
        rows.push(image.raster.as_ref().map_or_else(
            || {
                Text::new(&image.alt, theme::component_palette())
                    .role(TextRole::Small)
                    .muted()
                    .render()
            },
            |raster| {
                Element::clickable(
                    tool_presentation::image_action(owner, tool_call_id, index, raster.sha256()),
                    Element::raster_image(raster.clone())
                        .size(None, Some(if compact { 180.0 } else { 240.0 }))
                        .radius(6.0),
                )
            },
        ));
    }
    if !content.is_empty() {
        rows.push(Text::new(content, theme::component_palette()).render());
    }
    for notice in notices {
        rows.push(
            Text::new(notice, theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .render(),
        );
    }
    rows
}

#[cfg(test)]
pub(crate) fn tool_line(
    item: &TranscriptItem,
    state: &ViewState,
    compact: bool,
) -> Element {
    tool_line_with_output(item, state, compact, None, None)
}

pub(crate) fn tool_line_with_output(
    item: &TranscriptItem,
    state: &ViewState,
    compact: bool,
    partial: Option<(&str, bool)>,
    media: Option<&TranscriptItem>,
) -> Element {
    if let Some(file) = crate::file_activity_ui::build(item, state, compact, partial, media) {
        return file;
    }
    if let Some(search) = crate::search_activity_ui::build(item, state) {
        return search;
    }
    let TranscriptItem::Tool {
        id,
        name,
        arguments,
        output,
        is_error,
        activity,
    } = item
    else {
        unreachable!()
    };
    if openide_harness::is_user_interaction_tool(name) {
        // Interrupted and back as its card, the request is open again.
        let waiting = match activity {
            ActivityStatus::PermissionInterrupted => ActivityStatus::WaitingPermission,
            other => *other,
        };
        match (activity, output) {
            // The open request is the card below; it needs no second row.
            _ if approval_card_shown(state, name, arguments, waiting) => {
                return Element::spacer();
            }
            (ActivityStatus::Completed, Some(output)) if !*is_error => {
                return if name == crate::question_ui::TOOL {
                    crate::question_ui::answered(
                        arguments,
                        output,
                        media,
                        state.selected_session.as_deref().unwrap_or("draft"),
                    )
                } else {
                    crate::mode_proposal_ui::resolved(arguments, output)
                };
            }
            _ => {}
        }
    }
    let disclosure = format!("tool:{id}");
    let expanded = state.conversation_details.is_expanded(
        &disclosure,
        conversation_detail_default_open(state, &disclosure),
    );
    let presentation =
        tool_presentation::present(name, arguments, *activity, *is_error, output.as_deref());
    if presentation.kind == ToolKind::Command {
        return terminal_activity(item, state, expanded, compact, partial, media);
    }
    let ink = if *is_error || *activity == ActivityStatus::Failed {
        theme::DESTRUCTIVE
    } else {
        theme::FOREGROUND_SUBTLE
    };
    let palette = theme::action_palette(theme::BACKGROUND, ink);
    // Open, the row and its details are one block filled with the row's
    // hover, so the buttons inside hover one step above it.
    let on_block = theme::action_palette(palette.hover, theme::FOREGROUND);
    let mut details = Vec::new();
    if expanded {
        details.push(tool_detail_body(
            item,
            compact,
            state.selected_session.as_deref().unwrap_or("draft"),
            partial,
            media,
        ));
        if let Some(target) = tool_presentation::navigation_target(name, arguments) {
            if let Some(owner) = state.selected_session.as_deref() {
                details.push(
                    Button::new(
                        tool_presentation::navigation_action(owner, id),
                        match target {
                            tool_presentation::ToolNavigationTarget::File(_) => "Abrir archivo",
                            tool_presentation::ToolNavigationTarget::Url(_) => "Abrir enlace",
                        },
                        on_block,
                    )
                    .variant(ButtonVariant::Subtle)
                    .size(ControlSize::Compact)
                    .render(),
                );
            }
        }
        details.push(
            Button::new(
                format!("tool:{id}"),
                "Inspeccionar actividad",
                on_block,
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render(),
        );
    }
    let tooltip = if presentation.target.is_empty() {
        presentation.title.clone()
    } else {
        format!("{} · {}", presentation.title, presentation.target)
    };
    let mut row = ToolActivity::new(
        detail_action(state, &disclosure),
        &presentation.title,
        Element::column(details).gap(7.0),
        palette,
    )
    .ink(ink)
    .icon(presentation.icon)
    .target(&presentation.target)
    .command(presentation.kind == ToolKind::Command)
    .running(*activity == ActivityStatus::Running && !*is_error)
    .expanded(expanded);
    if let Some(note) = presentation
        .note
        .filter(|_| !approval_card_shown(state, name, arguments, *activity))
    {
        row = row.note(note);
    }
    Tooltip::new(
        row.render(),
        detail_action(state, &disclosure),
        tooltip,
        theme::component_palette(),
    )
    .description(name)
    .render()
}

/// Whether this waiting call's approval card is in the transcript. The card
/// already shows the whole request, so the row then keeps only its header.
/// Requests carry no call id; the same tool and arguments are the same ask.
pub(crate) fn approval_card_shown(
    state: &ViewState,
    name: &str,
    arguments: &serde_json::Value,
    activity: ActivityStatus,
) -> bool {
    if activity != ActivityStatus::WaitingPermission {
        return false;
    }
    let arguments = serde_json::to_string_pretty(arguments).unwrap_or_default();
    let selected = state.selected_session.as_deref();
    state
        .pending_permissions
        .iter()
        .take(3)
        .map(|permission| (&permission.tool_name, &permission.arguments))
        .chain(
            state
                .pending_subagent_permissions
                .iter()
                .filter(|permission| {
                    selected == Some(&permission.child_session_id)
                        || selected == Some(&permission.parent_session_id)
                })
                .map(|permission| (&permission.tool_name, &permission.arguments)),
        )
        .any(|(tool, requested)| tool == name && *requested == arguments)
}

/// A recorded command is a compact terminal, not its tool-input JSON. The
/// bounded tail stays visible; expanding reveals captured/final output and
/// the original command. Execution and background-process authority remain
/// with the existing host/runtime, never inferred from this presentation.
fn terminal_activity(
    item: &TranscriptItem,
    state: &ViewState,
    expanded: bool,
    compact: bool,
    partial: Option<(&str, bool)>,
    media: Option<&TranscriptItem>,
) -> Element {
    let TranscriptItem::Tool {
        id,
        name,
        arguments,
        output,
        is_error,
        activity,
    } = item
    else {
        return Element::spacer();
    };
    let owner = state.selected_session.as_deref().unwrap_or("draft");
    let disclosure = detail_action(state, &format!("tool:{id}"));
    let command = ["command", "cmd"]
        .into_iter()
        .find_map(|key| arguments.get(key).and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty());
    let title = command.map_or_else(|| tool_operation(name).to_owned(), terminal_command_title);
    let mut tooltip = command.unwrap_or_else(|| tool_operation(name)).to_owned();
    if let Some(cwd) = arguments.get("cwd").and_then(serde_json::Value::as_str) {
        tooltip.push_str(&format!("\n{cwd}"));
    }
    let parsed = output
        .as_deref()
        .and_then(|output| tool_presentation::terminal_result(name, output));
    let final_text = parsed
        .as_ref()
        .map(|(text, _)| text.as_str())
        .or(output.as_deref());
    let answered_below = !expanded && approval_card_shown(state, name, arguments, *activity);
    let mut content = Vec::new();
    let mut visible_copy = String::new();
    let mut limited = false;
    if let Some(command) = command.filter(|_| !answered_below) {
        let (command, clipped) = tool_presentation::clip_text(command, 12_000);
        limited |= clipped;
        let prompt = format!("$ {command}");
        content.push(if expanded {
            Text::new(prompt, theme::component_palette())
                .mono()
                .render()
        } else {
            Element::truncated_text(prompt.replace(['\r', '\n'], " "))
                .font_family(FontFamily::Monospace)
                .font_size(12.0)
                .foreground(theme::FOREGROUND)
        });
    }
    // A streamed capture and the authoritative result can differ. Only in
    // that case keep their names, so diagnostics never replace the result.
    let distinct_capture = partial.is_some_and(|(capture, _)| {
        final_text.is_some_and(|final_text| capture.trim_end() != final_text.trim_end())
    });
    let outputs = if expanded && distinct_capture {
        vec![
            (Some("Resultado final"), final_text),
            (Some("Salida capturada"), partial.map(|(text, _)| text)),
        ]
    } else {
        vec![(
            distinct_capture.then_some("Salida capturada"),
            partial.map(|(text, _)| text).or(final_text),
        )]
    };
    for (label, output) in outputs {
        let Some(output) = output.filter(|output| !output.is_empty()) else {
            continue;
        };
        if let Some(label) = label {
            content.push(
                Text::new(label, theme::component_palette())
                    .role(TextRole::Micro)
                    .muted()
                    .render(),
            );
        }
        let (preview, clipped) = tool_presentation::terminal_tail(
            output.trim_end_matches('\n'),
            if expanded { usize::MAX } else { 6 },
            if expanded { 12_000 } else { 1_400 },
        );
        limited |= clipped;
        visible_copy.push_str(&preview);
        if expanded {
            content.push(
                Text::new(preview, theme::component_palette())
                    .muted()
                    .mono()
                    .render(),
            );
        } else {
            content.push(Element::column(
                preview
                    .lines()
                    .map(|line| {
                        Element::truncated_text(line.trim_end_matches('\r'))
                            .font_family(FontFamily::Monospace)
                            .font_size(12.0)
                            .foreground(theme::FOREGROUND_SUBTLE)
                            .centered_y()
                            .size(None, Some(18.0))
                    })
                    .collect(),
            ));
        }
    }
    if let Some((_, status)) = &parsed {
        // These are process observations (including start/poll and remaining
        // output), not a redundant label for the tool's Completed lifecycle.
        content.push(
            Text::new(status, theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .render(),
        );
        visible_copy.push_str(status);
    }
    if partial.is_some_and(|(_, truncated)| truncated) {
        content.push(
            Text::new(
                "Salida limitada · parte del contenido fue omitida",
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render(),
        );
    }
    if expanded && limited {
        content.push(
            Text::new(
                "Vista limitada · contenido registrado en Actividad",
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render(),
        );
    }
    if expanded {
        if let Some(media) = media {
            content.extend(tool_media_elements(media, compact, owner));
        }
    }
    let mut body = Vec::new();
    if !content.is_empty() {
        body.push(Separator::horizontal(theme::component_palette()).render());
        let content = Element::column(content).gap(4.0);
        body.push(if expanded {
            let content = openide_ui::activity_body_inset(content);
            ScrollArea::new(
                format!(
                    "tool-result:{owner}:{id}:{}",
                    if partial.is_some() {
                        "capture"
                    } else {
                        "final"
                    }
                ),
                content,
            )
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .max_height(if compact { 240.0 } else { 320.0 })
        } else {
            content.padding(8.0)
        });
    }
    if !expanded && limited {
        body.push(
            Button::new(
                format!("{disclosure}:expand"),
                "Expandir salida",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .render()
            .dispatch_as(&disclosure)
            .center_content(),
        );
    }
    if let Some(actions) =
        crate::terminal_dock_ui::command_actions(state, "card", id, name, output.as_deref())
    {
        body.push(actions.padding(8.0));
    }
    let failed = *is_error || *activity == ActivityStatus::Failed;
    let mut terminal = ToolActivity::new(
        &disclosure,
        title,
        Element::column(body),
        theme::code_palette(),
    )
    .terminal(true)
    .tooltip(tooltip)
    .icon(Icon::Terminal)
    .expanded(expanded)
    .running(*activity == ActivityStatus::Running && !failed)
    .header_action(
        format!("tool:{id}"),
        UiIconButton::new(format!("tool:{id}"), Icon::More, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .tooltip("Inspeccionar actividad")
            .size(ControlSize::Dense)
            .render(),
    );
    if failed {
        terminal = terminal.ink(theme::DESTRUCTIVE);
    }
    let status = if failed {
        Some("Error")
    } else {
        match activity {
            ActivityStatus::WaitingPermission | ActivityStatus::PermissionInterrupted => {
                Some("Permiso pendiente")
            }
            ActivityStatus::Interrupted => Some("Sin confirmar"),
            ActivityStatus::Cancelled => Some("Cancelado"),
            ActivityStatus::Discarded => Some("Descartado"),
            _ => None,
        }
    };
    if let Some(status) = status {
        terminal = terminal.status(status);
    }
    if let Some(note) =
        tool_presentation::present(name, arguments, *activity, *is_error, output.as_deref()).note
    {
        if !answered_below && !visible_copy.contains(&note) {
            terminal = terminal.note(note);
        }
    }
    terminal.render()
}

/// Display-only first executable. Quoted paths and assignment prefixes stay
/// intact; this does not parse or authorize a shell command. The unmodified
/// command remains available in the body and the header's tooltip.
fn terminal_command_title(command: &str) -> String {
    let mut chars = command
        .trim_start_matches(|c: char| c.is_whitespace() || c == '(' || c == '{')
        .chars()
        .peekable();
    while chars.peek().is_some() {
        let mut word = String::new();
        let mut quote = None;
        while let Some(character) = chars.next() {
            if quote.is_none() && character.is_whitespace() {
                break;
            }
            if character == '\\' && quote != Some('\'') {
                if let Some(escaped) = chars.next() {
                    word.push(escaped);
                }
            } else if quote == Some(character) {
                quote = None;
            } else if quote.is_none() && matches!(character, '\'' | '"') {
                quote = Some(character);
            } else {
                word.push(character);
            }
        }
        while chars
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            chars.next();
        }
        if word.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && name.chars().enumerate().all(|(index, character)| {
                    character == '_'
                        || character.is_ascii_alphabetic()
                        || index > 0 && character.is_ascii_digit()
                })
        }) {
            continue;
        }
        if word.is_empty() || word.contains(['<', '>', ';', '|', '&']) {
            return "Terminal".into();
        }
        return tool_presentation::clip_text(word.rsplit('/').next().unwrap_or(&word), 80).0;
    }
    "Terminal".into()
}

pub(crate) fn tool_detail_body(
    item: &TranscriptItem,
    compact: bool,
    owner: &str,
    partial: Option<(&str, bool)>,
    typed_media: Option<&TranscriptItem>,
) -> Element {
    let TranscriptItem::Tool {
        id,
        name,
        arguments,
        output,
        is_error,
        activity,
        ..
    } = item
    else {
        return Element::spacer();
    };
    let (args, args_limited) = if arguments.is_null() {
        ("Sin argumentos".to_owned(), false)
    } else {
        tool_presentation::json_preview(arguments, 3_000)
    };
    let mut body = vec![
        Text::new("Entrada", theme::component_palette())
            .role(TextRole::Micro)
            .muted()
            .render(),
        Text::new(args, theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .mono()
            .render(),
    ];
    if args_limited {
        body.push(
            Text::new(
                "Entrada limitada en esta vista · completa en Actividad",
                theme::component_palette(),
            )
            .role(TextRole::Micro)
            .muted()
            .render(),
        );
    }
    let media = if typed_media.is_none() {
        output
            .as_deref()
            .and_then(|output| crate::tool_result_media::preview(name, output))
    } else {
        None
    };
    if let Some(media) = typed_media {
        body.extend(tool_media_elements(media, compact, owner));
    }
    if let Some(media) = &media {
        for image in &media.images {
            body.push(
                Element::raster_image(image.clone())
                    .size(None, Some(if compact { 180.0 } else { 240.0 }))
                    .radius(6.0),
            );
        }
        if !media.text.is_empty() {
            body.push(Text::new(&media.text, theme::component_palette()).render());
        }
        for note in &media.notices {
            body.push(
                Text::new(*note, theme::component_palette())
                    .role(TextRole::Small)
                    .muted()
                    .render(),
            );
        }
    }
    let terminal_result = output
        .as_deref()
        .and_then(|output| tool_presentation::terminal_result(name, output));
    let final_text = terminal_result
        .as_ref()
        .map(|(text, _)| text.as_str())
        .or(output.as_deref());
    let capture_title = if output.is_some() {
        "Salida capturada"
    } else if *activity == ActivityStatus::Running {
        "Salida en curso"
    } else {
        "Salida parcial · sin resultado final confirmado"
    };
    for (title, content, captured) in [
        ("Resultado final", final_text, false),
        (capture_title, partial.map(|(text, _)| text), true),
    ] {
        let Some(content) = content else {
            continue;
        };
        if !captured && (media.is_some() || typed_media.is_some()) {
            continue;
        }
        body.push(
            Text::new(title, theme::component_palette())
                .role(TextRole::Micro)
                .muted()
                .render(),
        );
        if !captured {
            if let Some((_, status)) = &terminal_result {
                body.push(
                    Text::new(status, theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .render(),
                );
            }
        } else if partial.is_some_and(|(_, truncated)| truncated) {
            body.push(
                Text::new(
                    "Salida limitada · parte del contenido fue omitida",
                    theme::component_palette(),
                )
                .role(TextRole::Micro)
                .muted()
                .render(),
            );
        }
        let (preview, limited) = if captured {
            tool_presentation::terminal_tail(content, usize::MAX, 12_000)
        } else {
            tool_presentation::clip_text(content, 12_000)
        };
        let height = ((preview
            .lines()
            .count()
            .clamp(1, if compact { 8 } else { 14 }) as f32)
            * 15.0
            + 16.0)
            .min(226.0);
        body.push(
            ScrollArea::new(
                format!(
                    "tool-result:{owner}:{id}:{}",
                    if captured { "capture" } else { "final" }
                ),
                Text::new(
                    if preview.is_empty() {
                        "Sin salida".into()
                    } else {
                        preview
                    },
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .ink(if *is_error {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND
                })
                .mono()
                .render()
                .padding(8.0),
            )
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .size(None, Some(height))
            .background(theme::CODE_SURFACE)
            .radius(6.0),
        );
        if limited {
            body.push(
                Text::new(
                    "Vista limitada · contenido registrado en Actividad",
                    theme::component_palette(),
                )
                .role(TextRole::Micro)
                .muted()
                .render(),
            );
        }
    }
    if output.is_none() && partial.is_none() {
        body.push(
            Text::new(
                match activity {
                    ActivityStatus::Running => "Esperando resultado…",
                    ActivityStatus::WaitingPermission => "Pendiente de autorización",
                    ActivityStatus::PermissionInterrupted => {
                        "Permiso pendiente · turno interrumpido"
                    }
                    ActivityStatus::Cancelled => "Operación cancelada sin resultado",
                    ActivityStatus::Discarded => "Operación descartada sin ejecutar",
                    ActivityStatus::Failed => "La operación falló sin resultado",
                    _ => "No se recibió un resultado confirmado",
                },
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render(),
        );
    }
    Element::column(body).gap(5.0)
}

/// Group only known exploration operations. Edits, commands and opaque MCP
/// calls keep their own visible records; tool names never imply safety.
pub(crate) fn exploration_call(item: &TranscriptItem) -> Option<&str> {
    let (call, name) = match item {
        TranscriptItem::Tool { id, name, .. } => (id.as_str(), name.as_str()),
        TranscriptItem::ToolPreview {
            call_id: Some(call),
            name: Some(name),
            ..
        } => (call.as_str(), name.as_str()),
        _ => return None,
    };
    // Classification must not change when output/status arrives: merging two
    // previously separate runs would replace the later group's disclosure ID.
    matches!(
        name,
        "read_file"
            | "batch_read"
            | "list_files"
            | "find_files"
            | "search_files"
            | "search_text"
            | "codebase_search"
            | "codebase_explore"
            | "get_diagnostics"
            | "web_search"
            | "web_fetch"
    )
    .then_some(call)
}

/// Find one visually consecutive run. Only invisible request bookkeeping and
/// the preceding call's own output/media may sit between exploration rows.
/// Any prose, reasoning, notice, user turn or different tool ends the group.
pub(crate) fn exploration_run(items: &[TranscriptItem], start: usize) -> (usize, usize) {
    let mut index = start;
    let mut count = 0;
    let mut previous_call = None;
    while let Some(item) = items.get(index) {
        if let Some(call) = exploration_call(item) {
            count += 1;
            previous_call = Some(call);
        } else {
            match item {
                TranscriptItem::ToolOutput { tool_call_id, .. }
                | TranscriptItem::ToolMedia { tool_call_id, .. }
                    if previous_call == Some(tool_call_id.as_str()) => {}
                TranscriptItem::ModelUsage { .. }
                | TranscriptItem::Activity(TranscriptActivity::Model {
                    status: ActivityStatus::Completed | ActivityStatus::Running,
                    ..
                }) if count > 0 => {}
                _ => break,
            }
        }
        index += 1;
    }
    (index, count)
}

pub(crate) fn exploration_needs_attention(item: &TranscriptItem) -> bool {
    match item {
        TranscriptItem::Tool {
            activity,
            is_error,
            output,
            ..
        } => {
            *is_error
                || !matches!(
                    activity,
                    ActivityStatus::Running | ActivityStatus::Completed
                )
                || (*activity == ActivityStatus::Completed && output.is_none())
        }
        TranscriptItem::ToolPreview { status, .. } => !matches!(
            status,
            ModelStreamStatus::Receiving | ModelStreamStatus::Committed
        ),
        _ => false,
    }
}

/// How one exploration call reads on a flat line: its verb while it runs,
/// its verb once done, and which count of the group's summary it adds to.
fn exploration_step(name: &str) -> (&'static str, &'static str, usize) {
    match name {
        "read_file" | "batch_read" => ("Leyendo", "Leyó", 0),
        "get_diagnostics" => ("Comprobando", "Comprobó", 0),
        "list_files" => ("Listando", "Listó", 2),
        "web_fetch" => ("Abriendo", "Abrió", 3),
        "web_search" => ("Buscando en la web", "Buscó en la web", 1),
        _ => ("Buscando", "Buscó", 1),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// What a group explored, as Cursor words it: the file's name when there is
/// one file, otherwise a count, then searches, listings and pages.
pub(crate) fn exploration_summary(items: &[TranscriptItem]) -> String {
    let mut counts = [0usize; 4];
    let mut file = None;
    for item in items {
        let (name, path) = match item {
            TranscriptItem::Tool {
                name, arguments, ..
            } if exploration_call(item).is_some() => (
                name,
                ["path", "file_path"]
                    .into_iter()
                    .find_map(|key| arguments.get(key)?.as_str()),
            ),
            TranscriptItem::ToolPreview {
                name: Some(name), ..
            } if exploration_call(item).is_some() => (name, None),
            _ => continue,
        };
        let slot = exploration_step(name).2;
        counts[slot] += 1;
        if slot == 0 {
            file = path.map(file_name);
        }
    }
    let mut parts = Vec::new();
    for (count, (one, many)) in counts.into_iter().zip([
        ("archivo", "archivos"),
        ("búsqueda", "búsquedas"),
        ("listado", "listados"),
        ("página", "páginas"),
    ]) {
        match (count, file.filter(|_| one == "archivo")) {
            (0, _) => {}
            (1, Some(file)) => parts.push(file.to_owned()),
            (1, None) => parts.push(crate::i18n::piece(format!("1 {one}"))),
            _ => parts.push(crate::i18n::piece(format!("{count} {many}"))),
        }
    }
    parts.join(", ")
}

/// Whether the call has not reached an outcome yet.
fn exploration_in_flight(item: &TranscriptItem) -> bool {
    matches!(
        item,
        TranscriptItem::Tool {
            activity: ActivityStatus::Running,
            is_error: false,
            ..
        } | TranscriptItem::ToolPreview {
            status: ModelStreamStatus::Receiving | ModelStreamStatus::Committed,
            ..
        }
    )
}

/// The verb and the target one exploration call reads as on a flat line:
/// its verb under way or done, or its whole title when it needs attention.
pub(crate) fn exploration_words<'a>(
    name: &str,
    arguments: &serde_json::Value,
    presentation: &'a tool_presentation::ToolPresentation,
    attention: bool,
    running: bool,
) -> (&'a str, std::borrow::Cow<'a, str>) {
    let (under_way, done, slot) = exploration_step(name);
    let verb = if attention {
        presentation.title.as_str()
    } else if running {
        under_way
    } else {
        done
    };
    // A file search is known by its pattern; the folder only narrows it.
    let glob = (name == "find_files")
        .then(|| arguments.get("pattern")?.as_str())
        .flatten()
        .map(|pattern| match arguments.get("path").and_then(|path| path.as_str()) {
            Some(path) if !path.is_empty() => format!("{pattern} en {path}"),
            _ => pattern.to_owned(),
        });
    let target = match glob {
        Some(glob) => glob.into(),
        None if slot == 0 => file_name(&presentation.target).into(),
        // The project's own folder has no name worth reading as "." .
        None if name == "list_files"
            && matches!(presentation.target.trim_end_matches('/'), "" | ".") =>
        {
            "la carpeta del proyecto".into()
        }
        None => presentation.target.as_str().into(),
    };
    (verb, target)
}

/// A call that ran opens its file or page, or else its recorded activity.
fn exploration_action(
    owner: Option<&str>,
    id: &str,
    name: &str,
    arguments: &serde_json::Value,
) -> String {
    match (tool_presentation::navigation_target(name, arguments), owner) {
        (Some(_), Some(owner)) => tool_presentation::navigation_action(owner, id),
        _ => format!("tool:{id}"),
    }
}

/// Room kept around a call brought into view: clear of the header's fade
/// above, and enough under a card's header to show what it holds.
pub(crate) const REVEAL_MARGIN: f32 = 96.0;

/// The control that stands for a call in the transcript, for the host to
/// bring into view: its flat line, after opening the exploration group that
/// folds it, or the header of its card.
pub(crate) fn reveal_target(state: &mut ViewState, call: &str) -> Option<String> {
    let position = state
        .transcript
        .iter()
        .position(|item| matches!(item, TranscriptItem::Tool { id, .. } if id == call))?;
    let TranscriptItem::Tool {
        name, arguments, ..
    } = &state.transcript[position]
    else {
        return None;
    };
    if state.appearance.tool_call_density != ToolCallDensity::Grouped
        || exploration_call(&state.transcript[position]).is_none()
    {
        return Some(detail_action(state, &format!("tool:{call}")));
    }
    let target = exploration_action(state.selected_session.as_deref(), call, name, arguments);
    // Walk the runs as the timeline does to find the one holding the call.
    let mut index = 0;
    while index <= position {
        let Some(first) = exploration_call(&state.transcript[index]).map(str::to_owned) else {
            index += 1;
            continue;
        };
        let (end, count) = exploration_run(&state.transcript, index);
        if position < end {
            if count >= 2 {
                state
                    .conversation_details
                    .set_expanded(&format!("work:{first}"), true);
            }
            break;
        }
        index = end;
    }
    Some(target)
}

/// Mark the call the user just jumped to from Activity, for a moment.
fn flashed(state: &ViewState, call: &str, row: Element) -> Element {
    match &state.activity.revealed {
        Some((revealed, _)) if revealed == call => Element::panel(row)
            .background(theme::SELECTED)
            .border(theme::FOREGROUND, 1.0)
            .radius(6.0),
        _ => row,
    }
}

/// One call as flat text: its verb and its target, and under them the reason
/// when it did not simply complete. `live` words it as the step under way.
fn exploration_lines(item: &TranscriptItem, state: &ViewState, live: bool) -> Vec<Element> {
    let owner = state.selected_session.as_deref();
    let preview;
    let (id, name, arguments, activity, is_error, output) = match item {
        TranscriptItem::Tool {
            id,
            name,
            arguments,
            output,
            is_error,
            activity,
        } => (id, name, arguments, *activity, *is_error, output.as_deref()),
        TranscriptItem::ToolPreview {
            call_id: Some(id),
            name: Some(name),
            arguments,
            status,
            ..
        } => {
            preview = tool_presentation::preview_arguments(arguments);
            let activity = match status {
                ModelStreamStatus::Receiving | ModelStreamStatus::Committed => {
                    ActivityStatus::Running
                }
                ModelStreamStatus::Cancelled => ActivityStatus::Cancelled,
                ModelStreamStatus::Discarded => ActivityStatus::Discarded,
                ModelStreamStatus::Interrupted | ModelStreamStatus::Failed => {
                    ActivityStatus::Interrupted
                }
            };
            (id, name, &preview, activity, false, None)
        }
        _ => return Vec::new(),
    };
    let presentation = tool_presentation::present(name, arguments, activity, is_error, output);
    let (verb, target) = exploration_words(
        name,
        arguments,
        &presentation,
        exploration_needs_attention(item),
        live || activity == ActivityStatus::Running,
    );
    if live {
        let step = if target.is_empty() {
            verb.to_owned()
        } else {
            format!("{verb} {target}")
        };
        return vec![stream_text(step, theme::FOREGROUND_SUBTLE)];
    }
    let failed = is_error || activity == ActivityStatus::Failed;
    // A call that ran opens its file or page, or else its recorded activity.
    let action = matches!(item, TranscriptItem::Tool { .. })
        .then(|| exploration_action(owner, id, name, arguments));
    // The words are plain text; what the call opens is a link, underlined
    // under the pointer, instead of a band across the transcript.
    let ink = |normal| if failed { theme::DESTRUCTIVE } else { normal };
    // The link names the file; its whole path is a hint on the link itself,
    // so it opens over the name and not over the middle of the row.
    let shortened = target.len() < presentation.target.len();
    let link = |action: &String, label: &str, color| {
        let link = openide_ui::Link::new(action, label, theme::component_palette())
            .ink(color)
            .font_size(13.0)
            .render()
            .size(None, Some(STREAM_LINE_HEIGHT))
            .shrink_to_fit();
        if shortened {
            Tooltip::new(link, action, &presentation.target, theme::component_palette())
                .render()
                .shrink_to_fit()
        } else {
            link
        }
    };
    let parts = match &action {
        // Nothing to name: the verb itself opens the recorded activity.
        Some(action) if target.is_empty() => vec![link(action, verb, ink(theme::FOREGROUND))],
        Some(action) => vec![
            stream_text(verb, ink(theme::FOREGROUND)).flex(0.0),
            link(action, target.as_ref(), ink(theme::FOREGROUND_SUBTLE)),
        ],
        None => vec![
            stream_text(verb, ink(theme::FOREGROUND)).flex(0.0),
            stream_text(target.as_ref(), ink(theme::FOREGROUND_SUBTLE)),
        ],
    };
    let line = stream_line(parts, None, None);
    let line = flashed(state, id, line);
    let mut lines = vec![line];
    if let Some(note) = presentation
        .note
        .filter(|_| !approval_card_shown(state, name, arguments, activity))
    {
        // The reason wraps: an error must be readable at any width.
        lines.push(
            Text::new(note, theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .render(),
        );
    }
    lines
}

/// Consecutive reads, searches and listings as flat text. While the turn is
/// still on them: "Explorando <resumen>" over a ticker with the step under
/// way. Once settled: "Exploró <resumen>", which opens to one line per call.
/// A call that needs attention keeps its line visible either way.
fn work_group(items: &[TranscriptItem], state: &ViewState, live_tail: bool) -> Element {
    let Some(call) = items.first().and_then(exploration_call) else {
        return Element::spacer();
    };
    let owner = state.selected_session.as_deref().unwrap_or("draft");
    let id = format!("work:{call}");
    let current = items
        .iter()
        .rev()
        .find(|item| exploration_call(item).is_some() && exploration_in_flight(item));
    let live = current.is_some() || live_tail;
    let expanded = state.conversation_details.is_expanded(
        &id,
        items.iter().any(exploration_needs_attention),
    );
    let verb = if live {
        stream_shimmer(format!("exploration:{owner}:{call}"), "Explorando", true)
    } else {
        stream_text("Exploró", theme::FOREGROUND)
    };
    let mut rows = vec![stream_line(
        vec![
            verb.flex(0.0),
            stream_text(exploration_summary(items), theme::FOREGROUND_SUBTLE),
        ],
        Some(detail_action(state, &id)),
        (!live || expanded).then_some(expanded),
    )];
    for item in items {
        let Some(line) = exploration_call(item) else {
            continue;
        };
        if exploration_needs_attention(item) {
            rows.extend(exploration_lines(item, state, false));
        } else {
            // Opens and closes with the group instead of jumping.
            let lines = expanded.then(|| Element::column(exploration_lines(item, state, false)));
            rows.push(
                Collapse::new(format!("exploration-line:{owner}:{line}"), lines)
                    .expand()
                    .render(),
            );
        }
    }
    let ticker = live.then(|| {
        // Two lines tall whatever it shows, so the transcript below holds
        // still while steps come and go.
        let (key, line) = match current {
            Some(item) => (
                exploration_call(item).unwrap_or_default(),
                exploration_lines(item, state, true).remove(0),
            ),
            None => (
                "waiting",
                stream_shimmer(format!("exploration-waiting:{owner}:{call}"), WAITING_LABEL, false),
            ),
        };
        Ticker::new(format!("exploration-step:{owner}:{call}"), key, line)
            .height(STREAM_LINE_HEIGHT * 2.0)
            .render()
    });
    // Once the group settles the ticker closes, and what follows glides up.
    rows.push(Collapse::new(format!("exploration-ticker:{owner}:{call}"), ticker).render());
    Element::column(rows)
}

/// Shared by native and CLI conversations; the action keeps the existing
/// scroll-follow policy and does not send a message or open another surface.
pub(crate) fn jump_to_bottom_control() -> Element {
    Tooltip::new(
        UiIconButton::new(
            "jump_to_bottom",
            Icon::ArrowDown,
            theme::action_palette(theme::INVERSE_SURFACE, theme::INVERSE_INK),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Compact)
        .render()
        .size(Some(28.0), Some(28.0))
        .center_content()
        .radius(14.0),
        "jump_to_bottom",
        "Volver al final",
        theme::component_palette(),
    )
    .render()
}

fn timeline(state: &ViewState, width: f32) -> Element {
    // Reserve the shared scrollbar rail even before overflow. Content and
    // cached Markdown widths must not jump when the first long turn arrives.
    let page = PageColumn::new("transcript", width, theme::component_palette())
        .max_width(READING_WIDTH)
        .min_width(120.0)
        .gutter(16.0, 24.0, COMPACT_BREAKPOINT)
        .gap(TRANSCRIPT_GAP)
        .reserve_rail(true);
    let reading_width = page.content_width();
    let _markdown_frame =
        openide_ui::Markdown::begin_frame(state.selected_session.as_deref(), reading_width.max(96.0));
    // Keep first content below the scrim at scroll position zero. When the
    // transcript scrolls, earlier rows pass beneath the fade like Codex.
    let mut rows = vec![Element::spacer().size(None, Some(22.0))];
    let mut index = 0;
    let mut turn_number = 0;
    let latest_model = state
        .transcript
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| {
            if let TranscriptItem::Activity(TranscriptActivity::Model { status, .. }) = item {
                Some((index, *status))
            } else {
                None
            }
        });
    let model_running = matches!(latest_model, Some((_, ActivityStatus::Running)));
    // The host's flags can outlive a turn, so they only count where the
    // transcript agrees that more is coming.
    let host_running = state.running || state.background_run_selected;
    let last_user = state
        .transcript
        .iter()
        .rposition(|item| matches!(item, TranscriptItem::User { .. }));
    // Both run in transcript order, so one pass pairs them.
    let mut editable = state.message_edit.turns.iter().peekable();
    let mut user_row = 0;
    // A live exploration group at the end says the turn is waiting in its
    // own ticker, so the trailing waiting line stays out.
    let mut live_group_tail = false;
    // The last turn has ended when nothing runs, streams or waits for an
    // answer; only then does it show its actions.
    let last_turn_live = host_running
        || model_running
        || !state.pending_permissions.is_empty()
        || state.transcript.iter().rev().any(
            |item| matches!(item, TranscriptItem::Assistant { streaming: true, .. }),
        );
    // The turn being collected: where its rows start in `rows`, its first
    // transcript row, and its user row with the message it can rewind to.
    let mut open_turn: Option<(usize, usize, usize, Option<&crate::message_edit::EditableTurn>)> = None;
    // One hover region per turn: the pointer anywhere over a turn shows the
    // actions under it. The user's bubble stays a child of the page, where
    // it can pin.
    let close_turn = |rows: &mut Vec<Element>, turn: Option<(usize, usize, usize, Option<_>)>, end: usize| {
        let Some((from, start, user_row, editable)) = turn else { return };
        if rows.len() == from {
            return;
        }
        let last = end == state.transcript.len();
        let mut group: Vec<Element> = rows.drain(from..).collect();
        if !(last && last_turn_live) {
            group.push(crate::message_actions::turn_actions(state, &Turn {
                start,
                items: &state.transcript[start..end],
                user_row,
                editable,
                last,
            }));
        }
        rows.push(Element::column(group).gap(TRANSCRIPT_GAP).hover_region());
    };
    let recorded_plan_calls: std::collections::HashSet<&str> = state
        .transcript
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::PlanSnapshot {
                tool_call_id: Some(id),
                ..
            } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    if state.transcript.is_empty() {
        rows.push(theme::text_px(
            "Esta tarea aún no tiene actividad",
            17.0,
            theme::FOREGROUND,
        ));
        rows.push(
            Text::new(
                "Describe qué quieres hacer en el compositor inferior.",
                theme::component_palette(),
            )
            .muted()
            .render(),
        );
    }
    while index < state.transcript.len() {
        let item = &state.transcript[index];
        let replaced_by_plan = match item {
            TranscriptItem::Tool {
                id,
                name,
                activity: ActivityStatus::Completed,
                is_error: false,
                ..
            } => name == "update_plan" && recorded_plan_calls.contains(id.as_str()),
            TranscriptItem::ToolPreview {
                call_id: Some(id),
                name: Some(name),
                status: ModelStreamStatus::Committed,
                ..
            } => name == "update_plan" && recorded_plan_calls.contains(id.as_str()),
            _ => false,
        };
        if replaced_by_plan {
            index += 1;
            continue;
        }
        if matches!(
            item,
            TranscriptItem::ToolOutput { .. } | TranscriptItem::ToolMedia { .. }
        ) {
            // Render this bounded, call-scoped partial output with its tool.
            index += 1;
            continue;
        }
        if completed_process_diagnostic(&state.transcript, index) {
            index += 1;
            continue;
        }
        if matches!(
            item,
            TranscriptItem::Activity(TranscriptActivity::Model {
                status: ActivityStatus::Completed | ActivityStatus::Running,
                ..
            }) | TranscriptItem::ModelUsage { .. }
        ) {
            index += 1;
            continue;
        }
        if matches!(
            item,
            TranscriptItem::Activity(TranscriptActivity::Model {
                status: ActivityStatus::Failed,
                ..
            })
        ) && state.transcript[index + 1..]
            .iter()
            .take_while(|item| {
                !matches!(
                    item,
                    TranscriptItem::User { .. }
                        | TranscriptItem::Activity(TranscriptActivity::Model { .. })
                )
            })
            .any(|item| matches!(item, TranscriptItem::Notice { is_error: true, .. }))
        {
            // The durable turn error already supplies the complete reason.
            // Keep the activity in the journal without repeating its title.
            index += 1;
            continue;
        }
        if state.appearance.tool_call_density == ToolCallDensity::Grouped
            && exploration_call(item).is_some()
        {
            let (end, count) = exploration_run(&state.transcript, index);
            if count >= 2 {
                live_group_tail =
                    (host_running || model_running) && end == state.transcript.len();
                rows.push(work_group(
                    &state.transcript[index..end],
                    state,
                    live_group_tail,
                ));
                index = end;
                continue;
            }
            // A lone call reads as the same flat line a group opens to.
            rows.push(Element::column(exploration_lines(item, state, false)));
            index += 1;
            continue;
        }
        if let TranscriptItem::Tool { id, .. } = item {
            let row = tool_line_with_output(
                item,
                state,
                width < COMPACT_BREAKPOINT,
                tool_partial_output(state, index, id),
                tool_media(state, index, id),
            );
            rows.push(flashed(state, id, row));
            index += 1;
            continue;
        }
        if let TranscriptItem::User { content, images } = item {
            let previous = open_turn.take();
            close_turn(&mut rows, previous, index);
            turn_number += 1;
            if turn_number > 1 {
                rows.push(Element::spacer().size(None, Some(8.0)));
            }
            let running = host_running && Some(index) == last_user;
            let turn = editable.next_if(|turn| turn.user_row == user_row);
            let edit = match (turn, &state.message_edit.editing) {
                (_, Some(editing)) if editing.user_row == user_row => UserEdit::Open,
                (Some(turn), _) => match crate::message_edit::blocked_reason(state, turn) {
                    None => UserEdit::Offered(user_row),
                    Some(reason) => UserEdit::Blocked(reason),
                },
                _ => UserEdit::None,
            };
            rows.push(user_message(content, images, index, state, running, edit, reading_width));
            // A steered message continues the turn its opening message can rewind.
            let rewinds = turn.or(previous.and_then(|(.., editable)| editable));
            open_turn = Some((rows.len(), index + 1, user_row, rewinds));
            user_row += 1;
            index += 1;
            continue;
        }
        rows.push(message(item, index, reading_width, state));
        index += 1;
    }
    close_turn(&mut rows, open_turn.take(), state.transcript.len());
    // Recovery and permissions remain current controls. Plan snapshots belong
    // to the transcript at their recorded position, never to this trailing area.
    if !state.running {
        if let Some((session_id, recovery)) = state.selected_session.as_deref().and_then(|id| {
            state
                .session_recovery
                .get(id)
                .map(|recovery| (id, recovery))
        }) {
            let child_card_already_visible = state
                .recoverable_subagent
                .as_ref()
                .is_some_and(|child| child.child_session_id == session_id);
            if let Some(pause) = state.pause.sessions.get(session_id) {
                // Paused for a known cause: the pause card, also for a child.
                let working = state
                    .subagent_sessions
                    .iter()
                    .filter(|run| {
                        run.parent_session_id == session_id
                            && run.status == openide_harness::SubagentStatus::Running
                    })
                    .count();
                rows.push(state.pause.card(
                    session_id,
                    pause,
                    working,
                    width < COMPACT_BREAKPOINT,
                    state.model_switch.proposals.get(session_id).map(|(_, model)| model.as_str()),
                ));
            } else if let Some(card) = state.model_switch.unfinished_card(session_id) {
                rows.push(card);
            } else if !child_card_already_visible
                // The open question is back as its own card, below: answering
                // it resumes the turn.
                && !(*recovery == SessionRecoveryView::Question
                    && state
                        .pending_permissions
                        .iter()
                        .any(|permission| permission.tool_name == crate::question_ui::TOOL))
            {
                let error = state
                    .session_recovery_error
                    .as_ref()
                    .filter(|(failed, _)| failed == session_id)
                    .map(|(_, error)| error.as_str());
                rows.push(native_recovery_card(session_id, recovery, error));
            }
        }
    }
    rows.extend(
        state
            .pending_permissions
            .iter()
            .take(3)
            .map(|permission| permission_card(permission, state, width < COMPACT_BREAKPOINT)),
    );
    rows.extend(related_subagent_rows(state, width < COMPACT_BREAKPOINT));
    let visible_prose = |index, item: &TranscriptItem| {
        let TranscriptItem::Assistant { content, streaming } = item else {
            return false;
        };
        let visible = if *streaming {
            state.stream_presentation.visible_bytes(index, content)
        } else {
            content.len()
        };
        !content[..visible].trim().is_empty()
    };
    // The turn goes on and nothing is in flight: say that it is thinking
    // about its next step. Whatever is in flight owns its own progress.
    let scope = match latest_model {
        Some((index, ActivityStatus::Running)) => index + 1,
        _ => 0,
    };
    let tail = state.transcript[scope..]
        .iter()
        .enumerate()
        .rev()
        .map(|(offset, item)| (scope + offset, item))
        .find(|(_, item)| {
            !matches!(
                item,
                TranscriptItem::ModelUsage { .. }
                    | TranscriptItem::PlanSnapshot { .. }
                    | TranscriptItem::ToolOutput { .. }
                    | TranscriptItem::ToolMedia { .. }
                    | TranscriptItem::Activity(TranscriptActivity::Model {
                        status: ActivityStatus::Completed,
                        ..
                    })
            )
        });
    let in_flight = match tail {
        Some((
            _,
            TranscriptItem::Reasoning {
                status: ModelStreamStatus::Receiving,
                ..
            }
            | TranscriptItem::ToolPreview {
                status: ModelStreamStatus::Receiving | ModelStreamStatus::Committed,
                ..
            }
            | TranscriptItem::WebSearch {
                status: ModelStreamStatus::Receiving,
                ..
            }
            | TranscriptItem::Tool {
                activity: ActivityStatus::Running | ActivityStatus::WaitingPermission,
                ..
            },
        )) => true,
        // Prose owns the response once it is visible, while it streams or
        // waits for its commit.
        Some((index, item @ TranscriptItem::Assistant { streaming, .. })) => {
            (*streaming || model_running) && visible_prose(index, item)
        }
        _ => false,
    };
    // A request is open; or the last one committed and the host still runs
    // the turn, which a new prompt or a finished tool confirms; or a reply
    // has begun without visible prose yet. A final answer or a failure ends it.
    let more_coming = match latest_model {
        Some((_, ActivityStatus::Running)) => true,
        None | Some((_, ActivityStatus::Completed)) => match tail {
            Some((_, TranscriptItem::User { .. } | TranscriptItem::Tool { .. })) => host_running,
            Some((
                _,
                TranscriptItem::Assistant {
                    streaming: true, ..
                },
            )) => true,
            _ => false,
        },
        _ => false,
    };
    let session = state.selected_session.as_deref().unwrap_or("draft");
    let waiting = (more_coming
        && !in_flight
        && !live_group_tail
        && state.pending_permissions.is_empty())
    .then(|| {
        Element::column(vec![
            Element::spacer().size(None, Some(TRANSCRIPT_GAP)),
            stream_line(
                vec![stream_shimmer(
                    format!("reply-stream:{session}"),
                    state.pause.retry_label().as_deref().unwrap_or(WAITING_LABEL),
                    false,
                )],
                None,
                None,
            ),
        ])
    });
    // The line closes when content arrives. Its slot hangs under the row
    // before it and carries the column's gap, so an empty slot takes no room.
    let last = rows.pop().unwrap_or_else(Element::spacer);
    rows.push(Element::column(vec![
        last,
        Collapse::new(format!("reply-waiting:{session}"), waiting).render(),
    ]));
    rows.extend(host_activity_markers(state));
    page.children(rows)
        .render()
        .background(theme::BACKGROUND)
        .top_fade(theme::BACKGROUND, HEADER_FADE_HEIGHT)
        .bottom_fade(theme::BACKGROUND, TRANSCRIPT_BOTTOM_FADE_HEIGHT)
}

fn permission_card(permission: &PermissionView, state: &ViewState, compact: bool) -> Element {
    if permission.tool_name == crate::question_ui::TOOL {
        return crate::question_ui::card(permission, state, compact);
    }
    if permission.tool_name == crate::mode_proposal_ui::TOOL {
        return crate::mode_proposal_ui::card(permission);
    }
    crate::approval_ui::inline_root(permission, state, compact)
}

fn subagent_permission_card(
    permission: &SubagentPermissionView,
    state: &ViewState,
    compact: bool,
) -> Element {
    crate::approval_ui::inline_child(permission, state, compact)
}

fn active_subagent_card(run: &SubagentRunView, selected_session: &str) -> Element {
    let relation = if selected_session == run.child_session_id {
        "ESTA SESIÓN ES UN SUBAGENTE"
    } else {
        "SUBAGENTE ACTIVO"
    };
    Card::new(
        Element::column(vec![
            Text::new(relation, theme::component_palette())
                .role(TextRole::Micro)
                .muted()
                .render(),
            Text::new(&run.title, theme::component_palette()).render(),
            Text::new(
                format!("Sesión hija: {}", run.child_session_id),
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .mono()
            .render(),
            Button::new(
                format!("subagent_cancel:{}", run.child_session_id),
                "Cancelar subagente",
                theme::action_palette(theme::HOVER, theme::DESTRUCTIVE),
            )
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Compact)
            .width(170.0)
            .render(),
        ])
        .gap(7.0),
        theme::card_palette(),
    )
    .render()
}

fn recoverable_subagent_card(run: &SubagentRunView) -> Element {
    Alert::new(
        "Subagente interrumpido",
        format!("{}\nEl journal conserva el turno. Reanudar verificará el estado antes de pedir al modelo o ejecutar una herramienta.", run.title),
        theme::component_palette(),
    )
    .tone(AlertTone::Warning)
    .action(
        theme::primary(
            format!("subagent_resume:{}", run.child_session_id),
            "Reanudar subagente",
        )
        .size(ControlSize::Compact)
        .width(176.0)
        .render(),
    )
    .compact(true)
    .render()
}

/// `error` is why the last press of its action started nothing.
fn native_recovery_card(session_id: &str, recovery: &SessionRecoveryView, error: Option<&str>) -> Element {
    let message = match recovery {
        SessionRecoveryView::StartAutomation => {
            "La sesión se registró antes de iniciar el agente. Se usará el perfil original de sólo lectura y la ruta guardada, sin ampliar permisos desde Settings."
        }
        SessionRecoveryView::Resume => "El turno quedó interrumpido y puede reanudarse.",
        SessionRecoveryView::Permission => {
            "Hay un permiso pendiente. Reanudar pedirá la decisión antes de ejecutar."
        }
        SessionRecoveryView::Question => {
            "El agente espera tu respuesta. Reanudar muestra la pregunta para que la contestes."
        }
        SessionRecoveryView::VerifyTool => {
            "La herramienta pudo haber producido cambios. Verifica su efecto antes de continuar."
        }
        SessionRecoveryView::ReviewBatch => {
            "Hay llamadas pendientes de un lote abortado. Revísalas antes de continuar."
        }
        SessionRecoveryView::InvalidJournal(error) => error,
    };
    let mut alert = Alert::new(
        "Recuperación del turno",
        match error {
            Some(error) => format!("No se pudo reanudar: {error}.\n{message}"),
            None => format!("{}\n{message}", recovery.label()),
        },
        theme::component_palette(),
    )
    .tone(
        if error.is_some() || matches!(recovery, SessionRecoveryView::InvalidJournal(_)) {
            AlertTone::Error
        } else {
            AlertTone::Warning
        },
    )
    .compact(true);
    if recovery.can_open() {
        alert = alert.action(
            theme::primary(format!("session_recover:{session_id}"), recovery.label())
                .size(ControlSize::Compact)
                .width(196.0)
                .render(),
        );
    }
    alert.render()
}

fn plan_snapshot_can_build(item: &TranscriptItem, index: usize, state: &ViewState) -> bool {
    let TranscriptItem::PlanSnapshot {
        plan,
        is_proposal: true,
        ..
    } = item
    else {
        return false;
    };
    if state.running || state.background_run_selected {
        return false;
    }
    let current_matches = state
        .selected_session
        .as_ref()
        .and_then(|id| state.session_plans.get(id))
        .is_some_and(|current| current.can_build && current.snapshot == *plan);
    if !current_matches {
        return false;
    }
    state
        .transcript
        .iter()
        .rposition(|item| matches!(item, TranscriptItem::PlanSnapshot { .. }))
        == Some(index)
}

pub(crate) fn plan_snapshot_item(
    item: &TranscriptItem,
    index: usize,
    state: &ViewState,
) -> Element {
    let TranscriptItem::PlanSnapshot {
        plan, is_proposal, ..
    } = item
    else {
        return Element::spacer();
    };
    let can_build = plan_snapshot_can_build(item, index, state);
    session_plan_card(
        state.selected_session.as_deref().unwrap_or("draft"),
        &SessionPlanView {
            snapshot: plan.clone(),
            can_build,
        },
        if *is_proposal {
            "Plan propuesto"
        } else {
            "Plan actualizado"
        },
    )
}

/// How a plan step stands: done, the one in progress, or still to do.
pub(crate) fn plan_step_marker(status: PlanStepStatus) -> Element {
    // Use vector marks: the bundled UI font does not cover every
    // Unicode circle/check glyph, and must not show missing-glyph boxes.
    match status {
        PlanStepStatus::Completed => Element::icon(Icon::CheckCircle)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(14.0), Some(14.0)),
        status => Element::panel(if status == PlanStepStatus::InProgress {
            Element::spacer()
                .size(Some(6.0), Some(6.0))
                .background(theme::BRAND)
                .radius(3.0)
                .centered_x()
                .size(None, Some(6.0))
                .centered_y()
        } else {
            Element::spacer()
        })
        .size(Some(14.0), Some(14.0))
        .center_content()
        .border(
            if status == PlanStepStatus::InProgress {
                theme::BRAND
            } else {
                theme::FOREGROUND_SUBTLE
            },
            1.0,
        )
        .radius(7.0),
    }
}

fn session_plan_card(session_id: &str, plan: &SessionPlanView, title: &str) -> Element {
    let completed = plan
        .snapshot
        .plan
        .iter()
        .filter(|step| step.status == PlanStepStatus::Completed)
        .count();
    let header = Element::row(vec![
        Element::panel(
            Element::icon(Icon::Plan)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(14.0), Some(14.0)),
        )
        .size(Some(14.0), Some(18.0))
        .center_content(),
        Text::new(title, theme::component_palette())
            .render()
            .centered_y()
            .flex(1.0),
        Text::new(
            format!("{completed} de {}", plan.snapshot.plan.len()),
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .muted()
        .render()
        .centered_y(),
    ])
    .min_size(0.0, 18.0)
    .gap(8.0);
    let steps = plan
        .snapshot
        .plan
        .iter()
        .enumerate()
        .map(|(index, step)| {
            Element::row(vec![
                plan_step_marker(step.status),
                Text::new(
                    format!("{}. {}", index + 1, step.step),
                    theme::component_palette(),
                )
                .ink(if step.status == PlanStepStatus::InProgress {
                    theme::FOREGROUND
                } else {
                    theme::FOREGROUND_SUBTLE
                })
                .render()
                .flex(1.0),
            ])
            .gap(8.0)
        })
        .collect::<Vec<_>>();
    let mut content = vec![header];
    if let Some(explanation) = plan
        .snapshot
        .explanation
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        content.push(
            Text::new(explanation, theme::component_palette())
                .muted()
                .render(),
        );
    }
    content.push(Element::column(steps).gap(8.0));
    if plan.can_build {
        content.push(
            theme::primary(
                format!("session_build_plan:{session_id}"),
                "Construir este plan",
            )
            .size(ControlSize::Compact)
            .width(180.0)
            .render(),
        );
    }
    Card::new(Element::column(content).gap(7.0), theme::card_palette()).render()
}

pub(crate) fn subagent_status_label(status: openide_harness::SubagentStatus) -> &'static str {
    match status {
        openide_harness::SubagentStatus::Ready => "Pendiente",
        openide_harness::SubagentStatus::Running => "En ejecución",
        openide_harness::SubagentStatus::Interrupted => "Interrumpido",
        openide_harness::SubagentStatus::Completed => "Completado",
        openide_harness::SubagentStatus::Failed => "Fallido",
        openide_harness::SubagentStatus::Cancelled => "Cancelado",
    }
}

/// A subagent that can still be cancelled: it runs, waits to start, or holds
/// inputs it has not processed.
pub(crate) fn subagent_cancellable(run: &crate::view::SubagentSessionView, live: bool) -> bool {
    use openide_harness::SubagentStatus::{Completed, Ready, Running};
    !run.cancellation_requested
        && (live
            || matches!(run.status, Ready | Running)
            || (run.status == Completed && run.open_inputs > 0))
}

/// Shared by the session layout and the workspace fallback so permissions
/// remain actionable while either related session is selected.
fn persisted_subagent_card(
    run: &crate::view::SubagentSessionView,
    selected: &str,
    live: bool,
    show_inputs: bool,
    edit: Option<&crate::view::SubagentInputEditView>,
    paused: Option<&crate::pause::PausedTurn>,
    usage: Option<u64>,
) -> Element {
    // Paused on a limit is its own state, never "interrupted" or "failed".
    // Its actions are offered once, on the root's pause card.
    let status = match paused {
        Some(pause) => match pause.resets_at_ms {
            Some(at) => format!("En pausa · {}", crate::pause::clock_label(at)),
            None => "En pausa".to_owned(),
        },
        None => subagent_status_label(run.status).to_owned(),
    };
    let (target, label) = if selected == run.child_session_id {
        (&run.parent_session_id, "Abrir sesión padre")
    } else if run.status == openide_harness::SubagentStatus::Interrupted && paused.is_none() {
        (&run.child_session_id, "Abrir recuperación")
    } else {
        (&run.child_session_id, "Abrir subagente")
    };
    let mut details = vec![
        Text::new(&run.title, theme::component_palette()).render(),
        Text::new(
            format!(
                "{status} · Nivel {} · {} entradas pendientes{}",
                run.depth,
                run.open_inputs,
                usage
                    .filter(|tokens| *tokens > 0)
                    .map(|tokens| format!(" · {} tokens", crate::model_switch::token_label(tokens)))
                    .unwrap_or_default()
            ),
            theme::component_palette(),
        )
        .muted()
        .render(),
    ];
    if run.cancellation_requested {
        details.push(
            Text::new("Cancelación solicitada", theme::component_palette())
                .muted()
                .render(),
        );
    }
    let mut actions = vec![Button::new(
        format!("subagent_open:{target}"),
        label,
        theme::component_palette(),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .render()];
    if subagent_cancellable(run, live) {
        actions.push(
            Button::new(
                format!("subagent_cancel:{}", run.child_session_id),
                "Cancelar",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
        );
    }
    if !run.inputs.is_empty() {
        actions.push(
            Button::new(
                format!("subagent_inputs:{}", run.child_session_id),
                if show_inputs {
                    "Ocultar entradas".to_owned()
                } else {
                    format!("Ver entradas ({})", run.inputs.len())
                },
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
        );
    }
    if (run.status == openide_harness::SubagentStatus::Ready
        || (run.status == openide_harness::SubagentStatus::Completed && run.open_inputs > 0))
        && !run.cancellation_requested
    {
        actions.push(
            Button::new(
                format!("subagent_dispatch:{}", run.child_session_id),
                if run.status == openide_harness::SubagentStatus::Ready {
                    "Iniciar subagente"
                } else {
                    "Procesar entradas"
                },
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
        );
    }
    if let Some(error) = &run.dispatch_error {
        details.push(
            Alert::new(
                if run.status == openide_harness::SubagentStatus::Ready {
                    "Inicio pendiente"
                } else {
                    "Cola pendiente"
                },
                error,
                theme::component_palette(),
            )
            .tone(AlertTone::Error)
            .compact(true)
            .render(),
        );
    }
    details.push(Element::column(actions).gap(6.0));
    if show_inputs && !run.inputs.is_empty() {
        let entries = run
            .inputs
            .iter()
            .map(|input| {
                let status = match input.status {
                    openide_harness::SubagentInputStatus::Pending => "Pendiente",
                    openide_harness::SubagentInputStatus::Active => "Activa",
                    openide_harness::SubagentInputStatus::Done => "Completada",
                    openide_harness::SubagentInputStatus::Skipped => "Omitida",
                };
                let mut content = vec![
                    Text::new(
                        format!("{status} · {:?}", input.mode),
                        theme::component_palette(),
                    )
                    .role(TextRole::Small)
                    .muted()
                    .render(),
                    Text::new(&input.id, theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .mono()
                        .render(),
                    Text::new(&input.prompt, theme::component_palette()).render(),
                ];
                if input.status == openide_harness::SubagentInputStatus::Pending
                    && !run.cancellation_requested
                {
                    content.insert(
                        2,
                        Button::new(
                            format!("subagent_input_edit:{}:{}", run.child_session_id, input.id),
                            "Editar entrada",
                            theme::component_palette(),
                        )
                        .variant(ButtonVariant::Outline)
                        .size(ControlSize::Compact)
                        .render(),
                    );
                    content.insert(
                        3,
                        Button::new(
                            format!(
                                "subagent_input_skip:{}:{}:{}",
                                run.child_session_id, input.revision, input.id
                            ),
                            "Omitir entrada",
                            theme::component_palette(),
                        )
                        .variant(ButtonVariant::Outline)
                        .size(ControlSize::Compact)
                        .render(),
                    );
                }
                Element::column(content)
                    .gap(4.0)
                    .padding(7.0)
                    .background(theme::PANEL)
                    .radius(6.0)
            })
            .collect();
        details.push(
            ScrollArea::new(
                format!("subagent_mailbox:{}", run.child_session_id),
                Element::column(entries).gap(7.0),
            )
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .size(None, Some(170.0)),
        );
    }
    if let Some(edit) = edit.filter(|edit| edit.child_session_id == run.child_session_id) {
        details.push(
            Element::column(vec![
                Text::new("Editar entrada pendiente", theme::component_palette())
                    .role(TextRole::Label)
                    .render(),
                Text::new(&edit.input_id, theme::component_palette())
                    .role(TextRole::Small)
                    .muted()
                    .mono()
                    .render(),
                TextArea::new(
                    "subagent_input_prompt",
                    &edit.prompt,
                    "Instrucción para el subagente",
                    theme::component_palette(),
                )
                .height(96.0)
                .render(),
                Element::column(vec![
                    Button::new(
                        "subagent_input_save",
                        "Guardar entrada",
                        theme::component_palette(),
                    )
                    .size(ControlSize::Compact)
                    .render(),
                    Button::new(
                        "subagent_input_cancel",
                        "Descartar edición",
                        theme::component_palette(),
                    )
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Compact)
                    .render(),
                ])
                .gap(6.0),
            ])
            .gap(7.0),
        );
    }
    Card::new(Element::column(details).gap(8.0), theme::card_palette()).render()
}

fn related_subagent_rows(state: &ViewState, compact: bool) -> Vec<Element> {
    let Some(selected) = state.selected_session.as_deref() else {
        return Vec::new();
    };
    let related = |child: &str, parent: &str| selected == child || selected == parent;
    // These sets index presence only. Render the original vectors so duplicate
    // records, card order and permission/recovery priority remain unchanged.
    // Include unrelated entries: the previous cross-lookups considered every
    // live/saved child ID when deciding whether a related card has a bridge.
    let live_ids: HashSet<&str> = state
        .active_subagents
        .iter()
        .map(|run| run.child_session_id.as_str())
        .collect();
    let saved_ids: HashSet<&str> = state
        .subagent_sessions
        .iter()
        .map(|run| run.child_session_id.as_str())
        .collect();
    let mut cards: Vec<Element> = state
        .pending_subagent_permissions
        .iter()
        .filter(|permission| related(&permission.child_session_id, &permission.parent_session_id))
        .map(|permission| subagent_permission_card(permission, state, compact))
        .collect();
    // Put explicit recovery ahead of the durable overview so its action remains
    // reachable even when a parent has many completed children.
    if let Some(run) = state
        .recoverable_subagent
        .as_ref()
        .filter(|run| selected == run.child_session_id)
        // A paused child shows the pause card instead.
        .filter(|run| !state.pause.sessions.contains_key(&run.child_session_id))
    {
        cards.push(recoverable_subagent_card(run));
    }
    cards.extend(
        state
            .subagent_sessions
            .iter()
            .filter(|run| related(&run.child_session_id, &run.parent_session_id))
            .map(|run| {
                persisted_subagent_card(
                    run,
                    selected,
                    live_ids.contains(run.child_session_id.as_str()),
                    state.expanded_subagent_inputs.as_deref()
                        == Some(run.child_session_id.as_str()),
                    state.subagent_input_edit.as_ref(),
                    state.pause.sessions.get(&run.child_session_id),
                    state.model_switch.usage.get(&run.child_session_id).copied(),
                )
            }),
    );
    cards.extend(
        state
            .active_subagents
            .iter()
            .filter(|run| {
                related(&run.child_session_id, &run.parent_session_id)
                    && !saved_ids.contains(run.child_session_id.as_str())
            })
            .map(|run| active_subagent_card(run, selected)),
    );
    cards
}

pub(crate) fn related_subagent_cards(state: &ViewState, compact: bool) -> Option<Element> {
    let cards = related_subagent_rows(state, compact);
    if cards.is_empty() {
        return None;
    }
    let height = (cards.len() as f32 * 210.0 + 16.0).min(if compact { 330.0 } else { 350.0 });
    Some(
        ScrollArea::new(
            "related_subagents",
            Element::column(cards).gap(8.0).padding(8.0),
        )
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .max_height(height)
        .background(theme::BACKGROUND),
    )
}

pub(crate) fn hook_activity_card(hook: &crate::transcript::HookActivity) -> Element {
    let phase = match hook.phase.as_str() {
        "pre_tool_use" => "Antes de la herramienta",
        "post_tool_use" => "Después de la herramienta",
        other => other,
    };
    let status = match hook.decision.as_deref() {
        Some("allow") => "Permitido · resultado confirmado",
        Some("block") => "Bloqueado · resultado confirmado",
        Some(_) => "Resultado registrado",
        None if hook.interrupted => "Interrumpido · efecto sin verificar",
        None => "En ejecución · resultado pendiente",
    };
    let mut details = vec![
        Text::new(
            format!("Hook: {}", hook.hook_id),
            theme::component_palette(),
        )
        .role(TextRole::Label)
        .render(),
        Text::new(phase, theme::component_palette())
            .muted()
            .render(),
        Text::new(
            format!("Llamada: {}", hook.tool_call_id),
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .muted()
        .mono()
        .render(),
        Text::new(status, theme::component_palette())
            .ink(
                if hook.interrupted || hook.decision.as_deref() == Some("block") {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND_SUBTLE
                },
            )
            .render(),
    ];
    for (label, value) in [
        ("Razón", &hook.reason),
        ("Salida del hook", &hook.stdout),
        ("stderr", &hook.stderr),
        ("Error del hook", &hook.error),
    ] {
        if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
            details.push(
                Text::new(label, theme::component_palette())
                    .role(TextRole::Small)
                    .muted()
                    .render(),
            );
            details.push(
                ScrollArea::new(
                    format!(
                        "hook_diagnostic:{}:{}:{}:{label}",
                        hook.tool_call_id, hook.hook_id, hook.phase
                    ),
                    Text::new(value, theme::component_palette())
                        .role(TextRole::Small)
                        .mono()
                        .render()
                        .padding(6.0),
                )
                .render()
                .size(None, Some(90.0))
                .background(theme::CARD)
                .radius(6.0),
            );
        }
    }
    if hook.interrupted {
        details.push(Text::new("Verifica también el efecto de este hook fuera de openADE. No se repetirá automáticamente.", theme::component_palette()).muted().render());
    }
    Card::new(Element::column(details).gap(6.0), theme::panel_card_palette())
        .density(CardDensity::Roomy)
        .render()
}

fn tool_verification_card(
    verification: &ToolVerificationView,
    output: &str,
    is_error: bool,
    compact: bool,
) -> Element {
    let argument_height = if compact { 66.0 } else { 96.0 };
    let mut details = vec![
        Text::new(
            format!("Herramienta: {}", verification.name),
            theme::component_palette(),
        )
        .render(),
        Text::new(
            format!("Llamada: {}", verification.call_id),
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .muted()
        .mono()
        .render(),
        Text::new("Argumentos originales", theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .render(),
        ScrollArea::new(
            format!("tool_verification_args:{}", verification.call_id),
            Text::new(&verification.arguments, theme::component_palette())
                .role(TextRole::Small)
                .mono()
                .render()
                .padding(8.0),
        )
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .size(None, Some(argument_height))
        .background(theme::PANEL)
        .radius(8.0),
    ];
    if !verification.hooks.is_empty() {
        details
            .push(Text::new("Hooks asociados a esta llamada", theme::component_palette()).render());
        details.extend(verification.hooks.iter().map(hook_activity_card));
    }
    if let Some(receipt) = &verification.goal_receipt {
        details.push(
            Text::new(
                "Recibo de modificación del goal",
                theme::component_palette(),
            )
            .render(),
        );
        details.push(
            ScrollArea::new(
                format!("tool_verification_goal_receipt:{}", verification.call_id),
                Text::new(receipt.description(), theme::component_palette())
                    .role(TextRole::Small)
                    .mono()
                    .render()
                    .padding(8.0),
            )
            .render()
            .size(None, Some(96.0)),
        );
    }
    if let Some(error) = &verification.error {
        details.push(
            Text::new("Error del ejecutor", theme::component_palette())
                .role(TextRole::Small)
                .ink(theme::DESTRUCTIVE)
                .render(),
        );
        details.push(
            Text::new(error, theme::component_palette())
                .role(TextRole::Small)
                .mono()
                .render(),
        );
    }
    details.extend([
        Separator::horizontal(theme::component_palette()).render(),
        Text::new("Resultado comprobado", theme::component_palette()).render(),
        Text::new(
            "Escribe lo que verificaste manualmente. Esta llamada no se repetirá al reanudar.",
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .muted()
        .render(),
        TextArea::new(
            "tool_verification_output",
            output,
            "Describe el resultado que comprobaste…",
            theme::component_palette(),
        )
        .height(if compact { 84.0 } else { 108.0 })
        .render(),
    ]);

    let classification = Text::new(
        format!(
            "Clasificación elegida: {} (pendiente de confirmar)",
            if is_error { "Error" } else { "Éxito" }
        ),
        theme::component_palette(),
    )
    .ink(if is_error {
        theme::DESTRUCTIVE
    } else {
        theme::FOREGROUND
    })
    .render();
    let toggle = Button::new(
        "tool_verification_toggle_error",
        if is_error {
            "Marcar como éxito"
        } else {
            "Marcar como error"
        },
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .render();
    details.push(if compact {
        Element::column(vec![classification, toggle]).gap(6.0)
    } else {
        Element::row(vec![classification.flex(1.0), toggle]).gap(8.0)
    });
    let confirm = Button::new(
        "tool_verification_confirm",
        if compact {
            "Confirmar resultado"
        } else {
            "Confirmar resultado verificado"
        },
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
    )
    .variant(ButtonVariant::Raised)
    .size(ControlSize::Compact)
    .enabled(!output.trim().is_empty())
    .render();
    let cancel = Button::new(
        "tool_verification_cancel",
        "Dejar pendiente",
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .render();
    interruption_card(
        "RESULTADO SIN VERIFICAR",
        "La herramienta pudo haber tenido un efecto antes de la interrupción. Comprueba el resultado fuera de openADE antes de continuar.".into(),
        details,
        [cancel, confirm],
        compact,
    )
}

/// What the verification and aborted-batch forms share: what stopped and
/// why it matters, the form's own rows, and the two ways out.
fn interruption_card(
    title: &str,
    reason: String,
    rows: Vec<Element>,
    actions: [Element; 2],
    compact: bool,
) -> Element {
    let mut details = vec![
        Element::row(vec![
            Element::icon(Icon::Stop)
                .foreground(theme::DESTRUCTIVE)
                .size(Some(16.0), Some(16.0)),
            Text::new(title, theme::component_palette()).render(),
        ])
        .gap(8.0),
        Text::new(reason, theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .render(),
        Separator::horizontal(theme::component_palette()).render(),
    ];
    details.extend(rows);
    details.push(
        ButtonGroup::new(actions.into())
            .sizing(crate::ui_sizing::INTERRUPTION_ACTIONS)
            .stacked(compact)
            .render(),
    );
    Card::new(Element::column(details).gap(10.0), theme::card_palette()).render()
}

fn aborted_batch_card(batch: &AbortedToolBatchView, compact: bool) -> Element {
    let count = batch.remaining_calls;
    let details = vec![
        Text::new(
            format!("Próxima herramienta pendiente: {}", batch.next_tool_name),
            theme::component_palette(),
        )
        .render(),
        Text::new(
            format!("Llamada: {}", batch.next_call_id),
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .muted()
        .mono()
        .render(),
        Text::new("Argumentos originales", theme::component_palette())
            .role(TextRole::Small)
            .muted()
            .render(),
        ScrollArea::new(
            format!("aborted_batch_args:{}", batch.next_call_id),
            Text::new(&batch.next_arguments, theme::component_palette())
                .role(TextRole::Small)
                .mono()
                .render()
                .padding(8.0),
        )
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .size(None, Some(if compact { 66.0 } else { 96.0 }))
        .background(theme::PANEL)
        .radius(8.0),
        Separator::horizontal(theme::component_palette()).render(),
    ];
    let discard = Button::new(
        "aborted_batch_discard",
        "Descartar llamadas pendientes",
        theme::action_palette(theme::DESTRUCTIVE, theme::PRIMARY_FOREGROUND),
    )
    .variant(ButtonVariant::Raised)
    .size(ControlSize::Compact)
    .render();
    let defer = Button::new(
        "aborted_batch_defer",
        "Dejar pendiente",
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .render();
    interruption_card(
        "LOTE DE HERRAMIENTAS INTERRUMPIDO",
        format!(
            "El turno terminó con {count} {} sin ejecutar. Descartarlas permite iniciar otro turno. No revierte las {} llamadas ya completadas.",
            if count == 1 {
                "llamada pendiente"
            } else {
                "llamadas pendientes"
            },
            batch.completed_calls,
        ),
        details,
        [defer, discard],
        compact,
    )
}

fn composer(state: &ViewState, width: f32) -> Element {
    composer_with_height(state, width, None)
}

fn composer_with_height(state: &ViewState, width: f32, available_height: Option<f32>) -> Element {
    let compact = width < COMPACT_BREAKPOINT;
    let card_width = (width - 32.0).clamp(0.0, READING_WIDTH);
    let shell_height = available_height.map(|height| (height - 32.0).max(0.0));
    let shell = if let Some(origin) = selected_native_automation(state) {
        automation_read_only_card(state, origin, card_width, compact)
    } else if state.message_edit.editing.is_some() {
        // The one composer is over the message being edited.
        crate::message_edit::bottom_stub(card_width)
    } else {
        build_composer_with_height(
            &ComposerProps::from_state(
                state,
                card_width,
                false,
                None,
                "Pide al agente que trabaje en este proyecto…",
                Some(&state.status),
            ),
            shell_height,
        )
    };
    let mut rows = Vec::new();
    rows.extend(crate::message_edit::undo_notice(state, card_width));
    rows.extend(queued_messages(state, card_width));
    rows.push(shell);
    Element::column(rows)
        .gap(8.0)
        .size(Some(card_width), None)
        .centered_x()
        .padding(16.0)
}

/// What was sent while the agent was working and it has not read yet. One
/// kind waits for the turn to reach a point where it can take it; the other
/// waits for the turn to end and is then sent as the next one. Each can be
/// taken out or brought back to the composer, and one for the next turn can
/// be handed to the running turn instead.
fn queued_messages(state: &ViewState, width: f32) -> Option<Element> {
    if state.queued_messages.is_empty() {
        return None;
    }
    let count = state.queued_messages.len();
    let palette = theme::component_palette();
    let tool = |id: String, icon: Icon, hint: &str| {
        UiIconButton::new(id, icon, theme::action_palette(theme::CARD, theme::FOREGROUND_SUBTLE))
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip(hint)
            .render()
    };
    let mut heading = vec![Text::new(
        if count == 1 { "1 mensaje en cola".to_owned() } else { format!("{count} mensajes en cola") },
        palette,
    )
    .role(TextRole::Small)
    .muted()
    .render()
    .flex(1.0)
    .centered_y()];
    if state.queue_paused {
        heading.push(
            Text::new("En pausa: el turno se detuvo", palette)
                .role(TextRole::Small)
                .ink(theme::DESTRUCTIVE)
                .render()
                .centered_y(),
        );
        heading.push(
            Button::new("queue_resume", "Reanudar", palette)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
        );
    }
    let mut rows = vec![Element::row(heading).gap(8.0).min_size(0.0, 22.0)];
    rows.extend(state.queued_messages.iter().map(|message| {
        let line = message.text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim();
        let mut row = vec![
            Element::icon(if message.next_turn { Icon::Clock } else { Icon::ArrowUp })
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(14.0), Some(22.0)),
            Element::truncated_text(if line.is_empty() { "Imágenes adjuntas" } else { line })
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .foreground(theme::FOREGROUND)
                .flex(1.0)
                .centered_y(),
            Text::new(
                match (message.next_turn, message.images) {
                    (true, 0) => "Próximo turno".to_owned(),
                    (true, 1) => "Próximo turno · 1 adjunto".to_owned(),
                    (true, attached) => format!("Próximo turno · {attached} adjuntos"),
                    (false, 0) => "En este turno".to_owned(),
                    (false, 1) => "En este turno · 1 imagen".to_owned(),
                    (false, images) => format!("En este turno · {images} imágenes"),
                },
                palette,
            )
            .role(TextRole::Small)
            .muted()
            .render()
            .centered_y(),
        ];
        if message.next_turn && state.running {
            row.push(tool(format!("queue_steer:{}", message.id), Icon::ArrowUp, "Redirigir: enviarlo ahora, sin detener el turno"));
        }
        // Its images are not the composer's to hold again.
        if message.next_turn || message.images == 0 {
            row.push(tool(format!("queue_edit:{}", message.id), Icon::Edit, "Editar: traerlo de vuelta al compositor"));
        }
        row.push(tool(format!("queue_remove:{}", message.id), Icon::Close, "Quitar de la cola"));
        Element::row(row).gap(6.0).min_size(0.0, 24.0)
    }));
    Some(
        Element::panel(Element::column(rows).gap(2.0))
            .padding(10.0)
            .background(theme::CARD)
            .border(theme::BORDER, 1.0)
            .radius(10.0)
            .size(Some(width), None),
    )
}

fn automation_read_only_card(
    state: &ViewState,
    origin: &NativeAutomationSessionView,
    width: f32,
    compact: bool,
) -> Element {
    let when = Local
        .timestamp_millis_opt(origin.scheduled_at_ms)
        .single()
        .map(|value| value.format("%d/%m/%Y %H:%M:%S").to_string())
        .unwrap_or_else(|| "Fecha no disponible".into());
    let details = Element::column(vec![
        Text::new(
            "Automatización nativa · sólo lectura",
            theme::component_palette(),
        )
        .render(),
        Text::new(
            "Esta sesión conserva el perfil original de la ejecución registrada.",
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .muted()
        .render(),
        Text::new(
            format!("Proveedor: {}", origin.provider_id),
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .mono()
        .render(),
        Text::new(
            format!("Modelo: {}", origin.model_id),
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .mono()
        .render(),
        Text::new(
            format!("Automatización: {}", origin.automation_id),
            theme::component_palette(),
        )
        .role(TextRole::Micro)
        .muted()
        .mono()
        .render(),
        Text::new(
            format!("Ejecución: {}", origin.run_id),
            theme::component_palette(),
        )
        .role(TextRole::Micro)
        .muted()
        .mono()
        .render(),
        Text::new(
            format!(
                "{} · {when}",
                if origin.manual {
                    "Inicio manual"
                } else {
                    "Programada"
                }
            ),
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .muted()
        .render(),
        Text::new(
            "Para enviar otra instrucción, crea una tarea nativa nueva.",
            theme::component_palette(),
        )
        .role(TextRole::Small)
        .muted()
        .render(),
    ])
    .gap(5.0)
    .padding(4.0);
    let mut rows = vec![ScrollArea::new(
        format!("automation_session_origin:{}", origin.run_id),
        details,
    )
    .tint(theme::FOREGROUND_SUBTLE)
    .render()
    .size(None, Some(if compact { 148.0 } else { 134.0 }))];
    if !state.status.trim().is_empty() {
        rows.push(
            Text::new(&state.status, theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .render(),
        );
    }
    if let Some(error) = &state.composer_error {
        rows.push(
            Alert::new("Error de automatización", error, theme::component_palette())
                .tone(AlertTone::Error)
                .compact(true)
                .render(),
        );
    }
    if state.running || state.background_run_selected {
        rows.push(
            Button::new(
                "run_toggle",
                "Detener ejecución",
                theme::action_palette(theme::CARD, theme::DESTRUCTIVE),
            )
            .icon(Icon::Stop)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
        );
    }
    rows.push(Element::row(vec![
        Element::spacer().flex(1.0),
        build_native_context_control(state, (width - 20.0).max(0.0)),
    ]));
    rows.push(
        Button::new(
            "new_session_header",
            "Nueva tarea nativa",
            theme::action_palette(theme::CARD, theme::FOREGROUND),
        )
        .icon(Icon::Add)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .render(),
    );
    Card::new(Element::column(rows).gap(7.0), theme::card_palette())
        .width(width)
        .render()
}

/// Renders an active session without representing unprojected harness state.
pub fn build_session_surface(
    state: &ViewState,
    center_width: f32,
    side_visible: bool,
    side_available: bool,
) -> Element {
    build_session_surface_layout(
        state,
        center_width,
        side_visible,
        side_available,
        true,
        None,
    )
}

pub(crate) fn build_session_surface_sized(
    state: &ViewState,
    center_width: f32,
    available_height: f32,
    side_visible: bool,
    side_available: bool,
) -> Element {
    build_session_surface_layout(
        state,
        center_width,
        side_visible,
        side_available,
        true,
        Some(available_height),
    )
}

pub(crate) fn build_session_body(
    state: &ViewState,
    center_width: f32,
    side_visible: bool,
    side_available: bool,
) -> Element {
    build_session_surface_layout(
        state,
        center_width,
        side_visible,
        side_available,
        false,
        None,
    )
}

pub(crate) fn build_session_body_sized(
    state: &ViewState,
    center_width: f32,
    available_height: f32,
    side_visible: bool,
    side_available: bool,
) -> Element {
    // Environment places this header outside the body, but the supplied
    // height includes it, just as it does for the ordinary session surface.
    build_session_surface_layout(
        state,
        center_width,
        side_visible,
        side_available,
        false,
        Some(available_height),
    )
}

/// A decision that replaces the transcript until it is taken: one card in
/// the reading column.
fn recovery_page(scroll_id: &str, card: Element, center_width: f32) -> Element {
    PageColumn::new(scroll_id, center_width, theme::component_palette())
        .max_width(READING_WIDTH)
        .gutter(10.0, 24.0, COMPACT_BREAKPOINT)
        .gap(0.0)
        .children(vec![
            card,
            // Keep the final action above the scroll viewport's bottom edge.
            Element::spacer().size(None, Some(64.0)),
        ])
        .render()
        .background(theme::BACKGROUND)
}

fn build_session_surface_layout(
    state: &ViewState,
    center_width: f32,
    side_visible: bool,
    side_available: bool,
    include_header: bool,
    available_height: Option<f32>,
) -> Element {
    let with_header = |mut children: Vec<Element>| {
        if include_header {
            children.insert(0, header(state, center_width, side_visible, side_available));
        }
        Element::column(children).flex(1.0)
    };
    let compact = center_width < COMPACT_BREAKPOINT;
    if let Some(batch) = &state.pending_batch_discard {
        let card = aborted_batch_card(batch, compact);
        return with_header(vec![recovery_page("aborted_batch", card, center_width)]).flex(1.0);
    }
    if let Some(verification) = &state.pending_tool_verification {
        let card = tool_verification_card(
            verification,
            &state.tool_verification_output,
            state.tool_verification_is_error,
            compact,
        );
        return with_header(vec![recovery_page("tool_verification", card, center_width)])
            .flex(1.0);
    }
    let chat = timeline(state, center_width);
    let chat = match turn_rail(state, center_width) {
        // Over the gutter, at mid height; the transcript stays under it.
        Some(rail) => Element::overlay(
            chat,
            Element::row(vec![
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    rail,
                    Element::spacer().flex(1.0),
                ]),
                Element::spacer().flex(1.0),
            ])
            .padding(12.0),
        )
        .clip_children()
        .flex(1.0),
        None => chat,
    };
    let chat = if state.show_jump_to_bottom {
        // A floating chat control must not shrink the scroll viewport or move
        // the composer when it appears after the user scrolls upward.
        Element::overlay(
            chat,
            Element::column(vec![
                Element::spacer().flex(1.0),
                jump_to_bottom_control()
                    .centered_x()
                    .padding(8.0)
                    .size(None, Some(52.0)),
            ]),
        )
        .clip_children()
        .flex(1.0)
    } else {
        chat
    };
    // Reserve a readable tail for the normal composer when the editor expands
    // or the window shrinks. A short window with Environment stacked or extra
    // notices can exhaust even fixed chrome; retain one usable 36px editor
    // line, including its existing 9px vertical insets.
    let composer_height = available_height.map(|height| (height - HEADER_HEIGHT - 100.0).max(0.0));
    crate::message_edit::confirm_dialog(
        with_header(vec![
            chat,
            composer_with_height(state, center_width, composer_height),
        ])
        .flex(1.0),
        state,
        center_width,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        command_center::Scope, settings_ui::SettingsPage, transcript::TranscriptProjection,
        view::SideTab,
    };
    use image::ImageEncoder as _;
    use openide_core::{AgentEvent, AgentEventKind, TurnMode, UserImage, UserImageMime};
    use openide_ui::{DrawCommand, HitKind, Point, Size, UiEvent, UiTree};

    #[test]
    #[ignore = "export the session cards and a chat code block for manual visual review"]
    fn export_session_cards_visual_review() {
        let directory = std::path::PathBuf::from(format!(
            "{}/../../target/openade-session-cards-review",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let plan = SessionPlanView {
            snapshot: openide_core::PlanSnapshot {
                explanation: Some("Separar la presentación del dominio sin cambiar píxeles.".into()),
                plan: [
                    ("Inspeccionar el proyecto", PlanStepStatus::Completed),
                    ("Validar el plan en la conversación", PlanStepStatus::InProgress),
                    ("Implementar los cambios acordados", PlanStepStatus::Pending),
                ]
                .into_iter()
                .map(|(step, status)| openide_core::PlanStep {
                    step: step.into(),
                    status,
                })
                .collect(),
            },
            can_build: true,
        };
        let hook = crate::transcript::HookActivity {
            tool_call_id: "call-1".into(),
            hook_id: "post-check".into(),
            phase: "post_tool_use".into(),
            decision: Some("block".into()),
            interrupted: false,
            reason: Some("El formateador modificó archivos fuera del alcance.".into()),
            stdout: None,
            stderr: Some("warning: 2 files reformatted\nerror: scope violation".into()),
            error: None,
        };
        let verification = ToolVerificationView {
            goal_receipt: None,
            session_id: "session-1".into(),
            call_id: "call-7".into(),
            name: "write_file".into(),
            arguments: "{\n  \"path\": \"notes.txt\",\n  \"content\": \"hola\"\n}".into(),
            error: Some("El proceso terminó antes de confirmar la escritura.".into()),
            hooks: Vec::new(),
        };
        let batch = AbortedToolBatchView {
            session_id: "session-1".into(),
            next_call_id: "call-9".into(),
            next_tool_name: "write_file".into(),
            next_arguments: "{\"path\":\"notes.txt\"}".into(),
            completed_calls: 1,
            remaining_calls: 2,
        };
        for width in [360.0, 900.0] {
            let compact = width < COMPACT_BREAKPOINT;
            let size = Size::new(width, 1760.0);
            let mut tree = UiTree::new(
                Element::column(vec![
                    session_plan_card("session-1", &plan, "Plan propuesto"),
                    hook_activity_card(&hook),
                    tool_verification_card(&verification, "", false, compact),
                    aborted_batch_card(&batch, compact),
                    rich_text::render_markdown_at_width(
                        "Un bloque de código:\n\n```rust\nfn main() {\n    println!(\"hola\");\n}\n```\n",
                        width - 32.0,
                    ),
                    Element::spacer().flex(1.0),
                ])
                .gap(12.0)
                .padding(16.0)
                .background(theme::BACKGROUND),
            );
            tree.set_reduced_motion(true);
            let frame = tree.layout(size);
            crate::settings_ui::visual_qa::write_native_frame_svg(
                &frame,
                &directory.join(format!("session-cards-{width:.0}.svg")),
                size.width,
                size.height,
            )
            .unwrap();
        }
    }

    #[test]
    #[ignore]
    fn export_web_citation_visual_review() {
        let directory = std::path::PathBuf::from(format!(
            "{}/../../target/openade-session-cards-review",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let size = Size::new(360.0, 96.0);
        let mut tree = UiTree::new(
            Element::column(vec![
                web_citation(0, "Documentación de limn", "https://example.com/limn/list-row"),
                Element::spacer().flex(1.0),
            ])
            .padding(16.0)
            .background(theme::BACKGROUND),
        );
        tree.set_reduced_motion(true);
        for state in ["idle", "hover"] {
            if state == "hover" {
                tree.handle_event(openide_ui::UiEvent::PointerMove(openide_ui::Point::new(
                    100.0, 40.0,
                )));
            }
            let frame = tree.layout(size);
            crate::settings_ui::visual_qa::write_native_frame_svg(
                &frame,
                &directory.join(format!("web-citation-{state}.svg")),
                size.width,
                size.height,
            )
            .unwrap();
        }
    }

    use serde_json::json;
    use unicode_segmentation::UnicodeSegmentation;

    pub(super) fn state() -> ViewState {
        ViewState {
            appearance: Default::default(),
            appearance_font_query: Default::default(),
            custom_theme_ui: Default::default(),
            tour: Default::default(),
            pause: Default::default(),
            model_switch: Default::default(),
            keymap_profile: crate::keymap::KeymapProfile::default(),
            vim_mode: false,
            settings_keymap_menu_open: false,
            ide: crate::ide_workspace::IdeWorkspaceState::default(),
            ide_search: crate::ide_search::IdeSearchState::default(),
            document_preview: None,
            markdown_preview: None,
            ide_status: Default::default(),
            workspace: "C:/repo/openIDE".into(),
            workspace_draft: "C:/repo/openIDE".into(),
            project_branch: None,
            project_on_github: false,
            composer_branches: Default::default(),
            github_contributions: crate::github_contributions::GithubContributionsStatus::Idle,
            prompt: "Siguiente paso".into(),
            composer_attachments: Vec::new(),
            composer_attachment_visuals: Default::default(),
            composer_attachment_loading: false,
            composer_image_viewer: None,
            message_edit: Default::default(),
            message_actions: Default::default(),
            composer_add_menu_open: false,
            composer_dictation: openide_ui::DictationState::Idle,
            composer_transcript: String::new(),
            composer_transcript_at: 0,
            composer_goal: None,
            composer_goal_editor_open: false,
            composer_goal_draft: String::new(),
            native_goal: Default::default(),
            native_instructions: Default::default(),
            native_context: Default::default(),
            composer_permissions_open: false,
            composer_full_access_confirm: None,
            composer_suggestions: Default::default(),
            composer_project_menu_open: false,
            environment: Default::default(),
            project_actions: Default::default(),
            composer_project_options: Vec::new(),
            endpoint: String::new(),
            model: "gpt-4.1".into(),
            native_provider_id: "custom".into(),
            native_output_limit_draft: String::new(),
            native_output_limit_field: String::new(),
            native_output_limit_responses: false,
            native_output_limit_messages: false,
            native_output_limit_load_error: None,
            hosted_web_search_mode: None,
            image_model: crate::image_runtime::IMAGE_MODELS[0].id.into(),
            image_quality: crate::image_runtime::DEFAULT_QUALITY.into(),
            catalog: None,
            catalog_status: String::new(),
            catalog_refreshing: false,
            catalog_browse_provider: "openai".into(),
            catalog_provider_query: String::new(),
            catalog_model_query: String::new(),
            catalog_models_filter: Default::default(),
            catalog_settings_tab: Default::default(),
            catalog_expanded_model: None,
            catalog_provider_menu_open: false,
            native_provider_definitions: Default::default(),
            native_provider_registry_error: None,
            user_providers: Vec::new(),
            provider_draft: None,
            pending_provider_delete_id: None,
            cli_profiles: Vec::new(),
            cli_profile_draft: None,
            pending_cli_profile_delete_id: None,
            provider_connections: Default::default(),
            provider_usage: Default::default(),
            claude_limits: Default::default(),
            provider_usage_open: false,
            provider_usage_refreshing: false,
            provider_connect: None,
            backend_options: vec![("native".into(), "Nativo".into())],
            selected_backend: "native".into(),
            backend_menu_open: false,
            composer_model: "gpt-4.1".into(),
            composer_model_options: vec![("gpt-4.1".into(), "gpt-4.1".into())],
            composer_routes: Vec::new(),
            composer_model_metadata: Default::default(),
            composer_provider_status: Default::default(),
            composer_model_menu_open: false,
            composer_model_list_open: false,
            composer_model_query: String::new(),
            composer_collapsed_model_providers: Default::default(),
            composer_search_collapsed_model_providers: Default::default(),
            composer_advanced_open: false,
            composer_effort: "default".into(),
            composer_effort_options: vec![],
            composer_effort_menu_open: false,
            composer_effort_preview: None,
            composer_effort_hovered: false,
            composer_context_percent: None,
            composer_context_open: false,
            composer_error: None,
            turn_mode: openide_core::TurnMode::Build,
            sessions: vec![],
            session_plans: Default::default(),
            session_recovery: Default::default(),
            session_recovery_error: None,
            sessions_status: None,
            selected_session: Some("s1".into()),
            selected_session_title: Some("Revisar la aplicación".into()),
            file_rows: vec![],
            files_status: String::new(),
            file_root_can_load_more: false,
            file_draft: None,
            file_delete: None,
            file_clipboard: None,
            show_archived: false,
            session_organization: crate::view::SessionOrganization::Project,
            session_sort: openide_core::SessionSort::Updated,
            session_sort_menu_open: false,
            collapsed_projects: std::collections::BTreeSet::new(),
            session_groups: crate::session_groups::SessionGroups::default(),
            group_create_open: false,
            group_create_draft: String::new(),
            group_menu_id: None,
            group_rename_id: None,
            group_rename_draft: String::new(),
            pending_delete_group_id: None,
            session_menu_id: None,
            renaming_session_id: None,
            rename_draft: String::new(),
            pending_archive_session_id: None,
            pending_delete_session_id: None,
            transcript: vec![],
            item_times: Default::default(),
            running: false,
            background_run_selected: false,
            status: "Listo".into(),
            settings_notice: None,
            settings_notice_error: false,
            native_key_available: false,
            workspace_writes_enabled: false,
            native_tool_permissions: openide_harness::ToolPermissions::default(),
            native_full_access: false,
            native_command_isolation: if cfg!(windows) {
                openide_harness::CommandIsolation::WorkspaceWriteOffline
            } else {
                openide_harness::CommandIsolation::HostUnrestricted
            },
            external_agents: Default::default(),
            external_approval: None,
            compat_hooks: Vec::new(),
            plugin_skills: Vec::new(),
            agent_clis: Vec::new(),
            turn_check_command: String::new(),
            turn_check_failure: None,
            review_model: Default::default(),
            compat_hooks_error: None,
            session_approval_grants: Vec::new(),
            native_commands: Vec::new(),
            native_commands_error: None,
            native_command_control: Default::default(),
            native_command_output: Default::default(),
            native_command_history: Default::default(),
            pending_native_command_termination: None,
            native_command_terminating: None,
            native_exec_rules: Default::default(),
            native_skills: Default::default(),
            plugins: Default::default(),
            native_hooks: Default::default(),
            settings_reduced_motion: false,
            settings_reopen_last_workspace: false,
            settings_terminal_shell: crate::settings_preferences::TerminalShell::System,
            settings_terminal_shell_draft: String::new(),
            settings_terminal_shell_menu_open: false,
            settings_choice_menu_open: None,
            chat_link_opening: Default::default(),
            preview_opening: Default::default(),
            chat_link_menu: None,
            settings_terminal_shell_editing: false,
            mcp_servers: Vec::new(),
            mcp_settings_scope: Default::default(),
            mcp_effective_summary: String::new(),
            mcp_shadowed_user_ids: Vec::new(),
            mcp_selected_id: None,
            mcp_editor_open: false,
            pending_mcp_delete_id: None,
            mcp_draft_id: String::new(),
            mcp_draft_http: false,
            mcp_draft_url: String::new(),
            mcp_draft_http_bearer_token_env_name: String::new(),
            mcp_draft_http_env_headers: String::new(),
            mcp_draft_oauth_enabled: false,
            mcp_draft_oauth_client_id: String::new(),
            mcp_draft_oauth_issuer: String::new(),
            mcp_draft_oauth_scopes: String::new(),
            mcp_oauth_status: None,
            mcp_draft_program: String::new(),
            mcp_draft_args: "[]".into(),
            mcp_draft_cwd: String::new(),
            mcp_draft_env_names: String::new(),
            mcp_load_error: None,
            mcp_inspection: Default::default(),
            mcp_presentation: Default::default(),
            conversation_activity: Default::default(),
            conversation_details: Default::default(),
            stream_presentation: Default::default(),
            show_settings: false,
            main_page: crate::view::MainPage::Workspace,
            automations: crate::automation_view::AutomationViewState::new(
                "C:/repo/openIDE",
                vec![],
                0,
            ),
            marketplace: crate::marketplace_view::MarketplaceViewState::default(),
            settings_page: SettingsPage::General,
            settings_nav_open: false,
            settings_nav_query: String::new(),
            can_go_back: false,
            can_go_forward: false,
            show_command_center: false,
            command_query: String::new(),
            command_scope: Scope::All,
            command_results: vec![],
            command_selected_index: 0,
            command_search_loading: false,
            command_search_errors: Vec::new(),
            command_search_truncated: false,
            show_side_pane: true,
            sidebar_collapsed: false,
            left_dock_width: None,
            right_dock_width: None,
            left_presentation_width: None,
            right_presentation_width: None,
            bottom_presentation_height: None,
            environment_presentation_progress: None,
            side_tab: SideTab::Context,
            graph_options: Default::default(),
            graph_status: Default::default(),
            activity_page: 0,
            activity: Default::default(),
            review_page: 0,
            dock_tabs: vec![],
            dock_recently_closed: vec![],
            dock_overview_query: String::new(),
            show_dock_add_menu: false,
            notifications: Default::default(),
            show_dock_tabs_menu: false,
            file_preview: None,
            code_editor: None,
            editor_workbench: Default::default(),
            debug: Default::default(),
            debug_open: false,
            code_snapshot: None,
            code_editor_quit_confirm: false,
            code_editor_pending_saves: false,
            chat_diffs: Default::default(),
            code_editor_reload_confirm: None,
            git_review: None,
            data_folder: String::new(),
            git_action_pending: false,
            git_action_error: None,
            git_panel: Default::default(),
            session_changes: Default::default(),
            browser: Default::default(),
            browser_refs: std::sync::Arc::from(""),
            agent_page_url: String::new(),
            queued_messages: Vec::new(),
            queue_paused: false,
            browser_sites: Vec::new(),
            browser_automation_enabled: true,
            browser_sites_error: None,
            browser_site_draft_access: Default::default(),
            browser_sites_reset_confirm: false,
            subagent_settings: Default::default(),
            subagent_settings_error: None,
            subagent_settings_reset_pending: false,
            subagent_personas: Vec::new(),
            browser_site_draft: String::new(),
            review_detail: None,
            terminal_tabs: Vec::new(),
            terminal_menu_open: false,
            terminal_controls: Default::default(),
            terminal_maximized: false,
            dock_maximized: false,
            dock_covering: false,
            show_terminal_panel: false,
            terminal_panel_height: 240.0,
            terminal_snapshot: None,
            terminal_input: String::new(),
            terminal_error: None,
            pending_permissions: vec![],
            question_drafts: Default::default(),
            composer_pending: None,
            pending_subagent_permissions: vec![],
            active_subagents: vec![],
            subagent_sessions: vec![],
            expanded_subagent_inputs: None,
            subagent_input_edit: None,
            recoverable_subagent: None,
            pending_tool_verification: None,
            pending_batch_discard: None,
            tool_verification_output: String::new(),
            tool_verification_is_error: false,
            selected_tool_id: None,
            show_jump_to_bottom: false,
            turn_in_view: None,
            link_copied: None,
            github_account: crate::github_account::GithubAccountStatus::Disconnected,
            github_account_menu_open: false,
            github_account_refreshing: false,
            github_account_notice: None,
            github_sign_in: None,
        }
    }

    fn recovery_action_reachable(
        state: &ViewState,
        width: f32,
        height: f32,
        scroll_id: &str,
        id: &str,
    ) -> bool {
        let mut tree = UiTree::new(build_session_surface(state, width, false, false));
        for _ in 0..30 {
            let frame = tree.layout(Size::new(width, height));
            if frame.regions.iter().any(|region| region.id == id) {
                return true;
            }
            tree.handle_event(UiEvent::Scroll {
                position: Point::new(width - 4.0, height * 0.5),
                delta_y: -140.0,
            });
        }
        tree.scroll_to_end(scroll_id);
        tree.layout(Size::new(width, height))
            .regions
            .iter()
            .any(|region| region.id == id)
    }

    fn verification_action_reachable(state: &ViewState, width: f32, height: f32, id: &str) -> bool {
        recovery_action_reachable(state, width, height, "tool_verification", id)
    }

    #[test]
    fn hook_effect_details_and_verification_actions_remain_visible_on_small_surfaces() {
        let hook = crate::transcript::HookActivity {
            tool_call_id: "call-1".into(),
            hook_id: "post-check".into(),
            phase: "post_tool_use".into(),
            decision: None,
            interrupted: true,
            reason: None,
            stdout: None,
            stderr: None,
            error: None,
        };
        let frame = UiTree::new(hook_activity_card(&hook)).layout(Size::new(360.0, 350.0));
        let content = visible_text(&frame);
        for expected in [
            "post-check",
            "Después de la herramienta",
            "call-1",
            "efecto sin verificar",
            "No se repetirá automáticamente",
        ] {
            assert!(content.contains(expected), "{content}");
        }
        let mut diagnosed = hook.clone();
        diagnosed.reason = Some("Razón histórica".into());
        diagnosed.stdout = Some("Salida JSON registrada".into());
        diagnosed.stderr = Some("Aviso stderr".into());
        diagnosed.error = Some("TimedOut".into());
        let frame = UiTree::new(hook_activity_card(&diagnosed)).layout(Size::new(360.0, 1000.0));
        let content = visible_text(&frame);
        for expected in [
            "Razón histórica",
            "Salida JSON registrada",
            "Aviso stderr",
            "TimedOut",
        ] {
            assert!(content.contains(expected), "{content}");
        }
        let mut state = state();
        state.pending_tool_verification = Some(ToolVerificationView {
            goal_receipt: None,
            session_id: "s1".into(),
            call_id: "call-1".into(),
            name: "write_file".into(),
            arguments: "{}".into(),
            error: None,
            hooks: vec![diagnosed],
        });
        state.tool_verification_output = "Verifiqué los efectos de herramienta y hook".into();
        for (width, height) in [(360.0, 320.0), (760.0, 720.0)] {
            for action in [
                "tool_verification_confirm",
                "tool_verification_cancel",
                "tool_verification_output",
            ] {
                assert!(
                    verification_action_reachable(&state, width, height, action),
                    "{action} at {width}px"
                );
            }
        }
    }

    #[test]
    fn aborted_batch_discard_and_defer_are_reachable_on_small_surfaces() {
        let mut state = state();
        state.pending_batch_discard = Some(AbortedToolBatchView {
            session_id: "session-1".into(),
            next_call_id: "call-9".into(),
            next_tool_name: "write_file".into(),
            next_arguments: "{\"path\":\"notes.txt\"}".into(),
            completed_calls: 1,
            remaining_calls: 2,
        });
        for (width, height) in [(360.0, 320.0), (760.0, 720.0)] {
            for action in ["aborted_batch_defer", "aborted_batch_discard"] {
                assert!(
                    recovery_action_reachable(&state, width, height, "aborted_batch", action),
                    "{action} is unreachable at {width}×{height}"
                );
            }
        }
        let frame = UiTree::new(build_session_surface(&state, 760.0, false, false))
            .layout(Size::new(760.0, 720.0));
        assert!(!frame.regions.iter().any(|region| region.id == "prompt"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "toggle_settings_header"));
    }

    #[test]
    fn native_goal_receipt_details_preserve_manual_verification_controls() {
        use crate::resume_worker::GoalReceiptInspection;
        let cases = [
            GoalReceiptInspection::Found(openide_core::NativeGoalCommitReceipt {
                goal_id: "goal-full".into(),
                operation_id: "operation-exact".into(),
                committed_revision: 1,
                current_revision: 3,
            }),
            GoalReceiptInspection::NotFound,
            GoalReceiptInspection::Failed("ledger corrupto".into()),
        ];
        for receipt in cases {
            let view = ToolVerificationView {
                goal_receipt: Some(receipt.clone()),
                session_id: "session-1".into(),
                call_id: "goal-call".into(),
                name: "record_goal_evidence".into(),
                arguments: "{}".into(),
                error: None,
                hooks: vec![],
            };
            let frame = UiTree::new(tool_verification_card(&view, "", false, false))
                .layout(Size::new(900.0, 1400.0));
            let text = visible_text(&frame);
            assert!(text.contains("Recibo de modificación del goal"));
            let expected = receipt
                .description()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                normalized.contains(&expected),
                "missing receipt content: {normalized}"
            );
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "tool_verification_goal_receipt:goal-call"));
            let mut state = state();
            state.pending_tool_verification = Some(view);
            for (width, height) in [(420.0, 360.0), (900.0, 800.0)] {
                assert!(verification_action_reachable(
                    &state,
                    width,
                    height,
                    "tool_verification_output"
                ));
                assert!(verification_action_reachable(
                    &state,
                    width,
                    height,
                    "tool_verification_cancel"
                ));
                assert!(!verification_action_reachable(
                    &state,
                    width,
                    height,
                    "tool_verification_confirm"
                ));
                state.tool_verification_output = "Estado verificado manualmente".into();
                assert!(verification_action_reachable(
                    &state,
                    width,
                    height,
                    "tool_verification_confirm"
                ));
                state.tool_verification_output.clear();
            }
        }
    }

    #[test]
    fn uncertain_tool_result_requires_manual_text_before_confirmation() {
        let mut state = state();
        state.pending_tool_verification = Some(ToolVerificationView {
            goal_receipt: None,
            session_id: "session-1".into(),
            call_id: "call-42".into(),
            name: "write_file".into(),
            arguments: "{\"path\":\"notes.txt\"}".into(),
            error: Some("Se perdió la respuesta del ejecutor".into()),
            hooks: vec![],
        });
        for (width, height) in [(360.0, 320.0), (760.0, 720.0)] {
            assert!(verification_action_reachable(
                &state,
                width,
                height,
                "tool_verification_output"
            ));
            assert!(verification_action_reachable(
                &state,
                width,
                height,
                "tool_verification_toggle_error"
            ));
            assert!(verification_action_reachable(
                &state,
                width,
                height,
                "tool_verification_cancel"
            ));
            assert!(!verification_action_reachable(
                &state,
                width,
                height,
                "tool_verification_confirm"
            ));
            state.tool_verification_output = "  \n  ".into();
            assert!(!verification_action_reachable(
                &state,
                width,
                height,
                "tool_verification_confirm"
            ));
            state.tool_verification_output = "Archivo comprobado en disco".into();
            assert!(
                verification_action_reachable(&state, width, height, "tool_verification_confirm"),
                "confirm is unreachable at {width}×{height}"
            );
            state.tool_verification_output.clear();
        }
        let frame = UiTree::new(build_session_surface(&state, 760.0, false, false))
            .layout(Size::new(760.0, 720.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text.contains("write_file")
        )));
        assert!(!frame.regions.iter().any(|region| region.id == "prompt"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "toggle_settings_header"));
    }

    pub(super) fn visible_text(frame: &openide_ui::Frame) -> String {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn long_assistant_source_pages_are_readable_and_actions_stay_inside_modal() {
        let mut state = state();
        state.selected_session = Some("source-session".into());
        state.running = false;
        state.background_run_selected = false;
        let content = format!("PRIMERA_PAGINA\n{}ULTIMA_PAGINA", "línea🦀\n".repeat(3_500));
        state.transcript = vec![TranscriptItem::Assistant {
            content: content.clone(),
            streaming: false,
        }];
        state.conversation_details.sync(Some("source-session"));
        let open_id = "conversation_source:source-session:0:0:open";
        let first = UiTree::new(message(&state.transcript[0], 0, 360.0, &state))
            .layout(Size::new(360.0, 500.0));
        assert!(visible_text(&first).contains("Respuesta extensa"));
        let mut preview_tree = UiTree::new(
            ScrollArea::new(
                "source-preview",
                message(&state.transcript[0], 0, 360.0, &state),
            )
            .render(),
        );
        preview_tree.layout(Size::new(360.0, 500.0));
        preview_tree.scroll_to_end("source-preview");
        click_visible_control(&mut preview_tree, Size::new(360.0, 500.0), open_id);
        // Navigation and close stay outside the modal body's scroll.
        assert!(crate::conversation_source_ui::apply_action(
            &mut state, open_id
        ));
        for viewport in [Size::new(360.0, 320.0), Size::new(900.0, 700.0)] {
            let modal = crate::conversation_source_ui::modal(
                &state,
                Element::button("source-base", "Chat"),
                viewport.width,
            );
            let mut tree = UiTree::new(modal);
            let frame = tree.layout(viewport);
            assert!(visible_text(&frame).contains("Respuesta completa"));
            let close_id = "conversation_source:source-session:0:0:close";
            // The first page's Previous is passive, rather than a clickable
            // region. Its visible label remains part of the fixed footer.
            assert!(visible_text(&frame).contains("Anterior"));
            for suffix in ["next", "close:close"] {
                let id = format!("conversation_source:source-session:0:0:{suffix}");
                let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert!(region.rect.x >= 0.0 && region.rect.y >= 0.0, "{id}");
                assert!(region.rect.x + region.rect.width <= viewport.width, "{id}");
                assert!(
                    region.rect.y + region.rect.height <= viewport.height,
                    "{id}"
                );
            }
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: openide_ui::Key::Escape,
                    shift: false,
                    ctrl: false,
                }),
                vec![openide_ui::UiAction::Clicked {
                    id: close_id.into()
                }],
            );
        }
        let next_id = "conversation_source:source-session:0:0:next";
        assert!(crate::conversation_source_ui::apply_action(
            &mut state, next_id
        ));
        let (index, start) = state.conversation_details.source_view().unwrap();
        assert_eq!(index, 0);
        assert!(start > 0 && content.is_char_boundary(start));
        // A queued action from a previous page is ignored.
        assert!(!crate::conversation_source_ui::apply_action(
            &mut state, next_id
        ));
        let page = openide_ui::Markdown::source_page(&content, start);
        assert!(page.text.ends_with("ULTIMA_PAGINA"));
        assert!(crate::conversation_source_ui::apply_action(
            &mut state,
            &format!("conversation_source:source-session:0:{start}:previous"),
        ));
        assert_eq!(state.conversation_details.source_view(), Some((0, 0)));
        assert!(crate::conversation_source_ui::apply_action(
            &mut state,
            "conversation_source:source-session:0:0:close",
        ));
        assert_eq!(state.conversation_details.source_view(), None);
        assert_eq!(
            state.transcript[0],
            TranscriptItem::Assistant {
                content,
                streaming: false
            }
        );
    }

    #[test]
    fn source_view_is_scoped_to_session_and_waits_for_live_projection_to_settle() {
        let mut state = state();
        state.selected_session = Some("source-owner".into());
        state.running = false;
        state.background_run_selected = false;
        state.transcript = vec![TranscriptItem::Assistant {
            content: "Fuente íntegra 🦀".into(),
            streaming: false,
        }];
        let open = "conversation_source:source-owner:0:0:open";
        assert!(crate::conversation_source_ui::apply_action(
            &mut state, open
        ));
        state.running = true;
        assert!(!crate::conversation_source_ui::apply_action(
            &mut state, open
        ));
        let base = crate::conversation_source_ui::modal(
            &state,
            Element::button("source-base", "Chat"),
            600.0,
        );
        let frame = UiTree::new(base).layout(Size::new(600.0, 400.0));
        assert!(!visible_text(&frame).contains("Respuesta completa"));
        state.running = false;
        state.selected_session = Some("other-owner".into());
        state
            .conversation_details
            .sync(state.selected_session.as_deref());
        assert_eq!(state.conversation_details.source_view(), None);
        assert!(!crate::conversation_source_ui::apply_action(
            &mut state, open
        ));
    }

    fn automation_state() -> ViewState {
        let mut state = state();
        state.selected_session = Some("auto-recorded".into());
        state.sessions = vec![crate::view::SessionView {
            id: "auto-recorded".into(),
            title: "Resumen registrado".into(),
            project: state.workspace.clone(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
            native_automation: Some(NativeAutomationSessionView {
                automation_id: "987784fa-996e-4f54-83a6-a5c60b652e44".into(),
                run_id: "987784fa-996e-4f54-83a6-a5c60b652e44:2000000".into(),
                provider_id: "provider-original".into(),
                model_id: "exact-model-original".into(),
                manual: false,
                scheduled_at_ms: 2_000_000,
            }),
        }];
        state.native_provider_id = "settings-provider-later".into();
        state.composer_model = "settings-model-later".into();
        state.model = "settings-model-later".into();
        state.status = "Registrada; aún no se inició".into();
        state
    }

    fn click_visible_control(tree: &mut UiTree, viewport: Size, id: &str) {
        let frame = tree.layout(viewport);
        let control = frame
            .regions
            .iter()
            .rev()
            .find(|region| region.id == id && region.kind == HitKind::Button)
            .unwrap_or_else(|| panic!("missing {id}"));
        assert!(control.rect.x >= 0.0 && control.rect.y >= 0.0);
        assert!(control.rect.x + control.rect.width <= viewport.width);
        assert!(control.rect.y + control.rect.height <= viewport.height);
        let point = Point::new(
            control.rect.x + control.rect.width / 2.0,
            control.rect.y + control.rect.height / 2.0,
        );
        assert_eq!(frame.hit_test(point), Some(id));
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![openide_ui::UiAction::Clicked { id: id.into() }]
        );
    }

    #[test]
    fn automation_origin_and_start_recovery_remain_scoped_read_only_and_clickable_at_small_and_large_widths(
    ) {
        for width in [360.0, 900.0] {
            let mut state = automation_state();
            state
                .session_recovery
                .insert("auto-recorded".into(), SessionRecoveryView::StartAutomation);
            let viewport = Size::new(width, 760.0);
            let mut tree = UiTree::new(build_session_surface(&state, width, false, false));
            let frame = tree.layout(viewport);
            let top = visible_text(&frame);
            for value in [
                "sólo lectura",
                "provider-original",
                "exact-model-original",
                "Registrada; aún no se inició",
                "Iniciar ejecución registrada",
            ] {
                assert!(top.contains(value), "missing {value} at {width}: {top}");
            }
            assert!(
                !top.contains("settings-provider-later") && !top.contains("settings-model-later")
            );
            for id in [
                "prompt",
                "run_toggle",
                "run_steer",
                "composer_add_context",
                "composer_model_toggle",
            ] {
                assert!(
                    !frame.regions.iter().any(|region| region.id == id),
                    "interactive {id} at {width}"
                );
            }
            for region in &frame.regions {
                assert!(
                    region.rect.x >= -0.5,
                    "{} starts outside at {width}",
                    region.id
                );
                assert!(
                    region.rect.x + region.rect.width <= width + 0.5,
                    "{} ends outside at {width}",
                    region.id
                );
            }
            click_visible_control(&mut tree, viewport, "session_recover:auto-recorded");
            click_visible_control(&mut tree, viewport, "new_session_header");
            let origin = selected_native_automation(&state).unwrap();
            tree.scroll_to_end(&format!("automation_session_origin:{}", origin.run_id));
            let bottom = visible_text(&tree.layout(viewport));
            let all = format!("{top} {bottom}")
                .split_whitespace()
                .collect::<String>();
            assert!(all.contains(&origin.automation_id));
            assert!(all.contains(&origin.run_id));
            assert!(bottom.contains("Programada"));
        }
    }

    #[test]
    fn automation_context_control_preserves_original_route_usage_and_pointer_actions() {
        use crate::native_context_ui::{
            NativeContextCategory, NativeContextMeasurementStatus, NativeContextRow,
            NativeContextSection, NativeContextUiState,
        };
        let project = tempfile::tempdir().unwrap();
        let workspace = project.path().canonicalize().unwrap().display().to_string();
        for width in [360.0, 900.0] {
            let mut state = automation_state();
            state.workspace = workspace.clone();
            state.sessions[0].project = workspace.clone();
            state.status = "Automatización terminada".into();
            state.native_context = NativeContextUiState {
                owner: state.selected_session.clone(),
                workspace: Some(std::path::PathBuf::from(&state.workspace)),
                provider: "provider-original".into(),
                model: "exact-model-original".into(),
                window: Some(2_000),
                remaining_percent: Some(85),
                measurement_status: NativeContextMeasurementStatus::Measured,
                last_usage: Some(openide_core::ModelTokenUsage {
                    input_tokens: 240,
                    output_tokens: 60,
                    total_tokens: 300,
                    cached_input_tokens: Some(40),
                    reasoning_tokens: Some(20),
                }),
                last_usage_sequence: Some(12),
                journal_last_sequence: 13,
                reported_responses: 1,
                breakdown: vec![
                    NativeContextCategory::new(
                        "tools",
                        "Herramientas",
                        "4 definiciones",
                        "Catálogo original de sólo lectura; no representa tokens.",
                    ),
                    // The card's figure is the sum of its rows: as in the real
                    // projection, the conversation takes what was measured.
                    NativeContextCategory::new("conversation", "Conversación", "240 entrada · 60 salida", ""),
                ],
                sections: vec![NativeContextSection {
                    title: "Ruta y límites registrados".into(),
                    rows: vec![NativeContextRow::new(
                        "Proveedor e ID de modelo",
                        "provider-original · exact-model-original",
                        "La ruta registrada no cambia con Settings.",
                    )],
                }],
                ..Default::default()
            };
            let viewport = Size::new(width, 760.0);
            let mut tree = UiTree::new(build_session_surface(&state, width, false, false));
            tree.set_reduced_motion(true);
            let initial = tree.layout(viewport);
            let trigger = initial
                .regions
                .iter()
                .find(|region| region.id == "composer_context_toggle" && region.kind == HitKind::Button)
                .unwrap();
            assert!(initial.commands.iter().any(|command| matches!(command,
                DrawCommand::ProgressRing { rect, fraction: Some(fraction), .. }
                    if (*fraction - 0.15).abs() < 0.001
                        && rect.x >= trigger.rect.x
                        && rect.y >= trigger.rect.y
                        && rect.x + rect.width <= trigger.rect.x + trigger.rect.width
                        && rect.y + rect.height <= trigger.rect.y + trigger.rect.height)));
            // The narrow trigger retains its measured ring without a label.
            assert!(!visible_text(&initial).contains("15%"));
            click_visible_control(&mut tree, viewport, "composer_context_toggle");
            state.composer_context_open = true;
            tree.set_root(build_session_surface(&state, width, false, false));
            let summary = tree.layout(viewport);
            for value in ["300 de 2K tokens", "15%", "exact-model-original", "Herramientas"] {
                assert!(
                    visible_text(&summary).contains(value),
                    "missing {value} at {width}"
                );
            }
            assert!(!summary.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if matches!(text.as_str(), "Entrada" | "Salida"))));
            assert!(!visible_text(&summary).contains("settings-provider-later"));
            assert!(!visible_text(&summary).contains("settings-model-later"));
            let area = summary
                .regions
                .iter()
                .find(|region| region.id == "composer_context_summary")
                .unwrap()
                .rect;
            assert!(area.width <= 344.0 && area.height <= viewport.height - 16.0);
            assert!(
                area.x >= 0.0
                    && area.y >= 0.0
                    && area.x + area.width <= width
                    && area.y + area.height <= viewport.height
            );
            let copied = crate::composer_context_ui::diagnostic(&state.native_context);
            assert!(copied.contains(
                "Proveedor e ID de modelo: provider-original · exact-model-original — La ruta"
            ));
            assert!(copied.contains("Herramientas: 4 definiciones — Catálogo original"));
            click_visible_control(&mut tree, viewport, "composer_context_dismiss");
            state.composer_context_open = false;
            tree.set_root(build_session_surface(&state, width, false, false));
            let closed = tree.layout(viewport);
            assert!(!closed
                .regions
                .iter()
                .any(|region| region.id == "composer_context_summary"));
            assert!(!closed.regions.iter().any(|region| matches!(
                region.id.as_str(),
                "prompt" | "run_toggle" | "run_steer" | "composer_model_toggle"
            )));

            for changed in 0..4 {
                let mut stale = automation_state();
                stale.workspace = workspace.clone();
                stale.sessions[0].project = workspace.clone();
                stale.native_context = state.native_context.clone();
                match changed {
                    0 => stale.native_context.owner = Some("another-owner".into()),
                    1 => stale.native_context.workspace = Some("another-workspace".into()),
                    2 => stale.native_context.provider = "settings-provider-later".into(),
                    _ => stale.native_context.model = "settings-model-later".into(),
                }
                stale.composer_context_open = true;
                let frame = UiTree::new(build_session_surface(&stale, width, false, false))
                    .layout(viewport);
                let text = visible_text(&frame);
                assert!(
                    text.contains("Aún no hay una respuesta medida"),
                    "{changed}: {text}"
                );
                assert!(!text.contains("15%") && !text.contains("300 / 2K tokens"));
            }
        }
    }

    #[test]
    fn automation_running_card_preserves_stop_status_and_errors_without_interactive_sending() {
        for width in [360.0, 900.0] {
            let mut state = automation_state();
            state.background_run_selected = true;
            state.status = "Automatización en ejecución".into();
            state.composer_error = Some("Credencial de la ruta guardada no disponible".into());
            state.session_recovery.insert(
                "auto-recorded".into(),
                SessionRecoveryView::InvalidJournal("Snapshot no verificable".into()),
            );
            let viewport = Size::new(width, 760.0);
            let mut tree = UiTree::new(build_session_surface(&state, width, false, false));
            let frame = tree.layout(viewport);
            let visible = visible_text(&frame)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(visible.contains("Automatización en ejecución"));
            assert!(
                visible.contains("Credencial de la ruta guardada no disponible"),
                "{visible}"
            );
            assert!(visible.contains("Snapshot no verificable"));
            assert!(!frame.regions.iter().any(|region| matches!(
                region.id.as_str(),
                "prompt" | "run_steer" | "session_recover:auto-recorded"
            )));
            click_visible_control(&mut tree, viewport, "run_toggle");
            assert!(visible.contains("Detener ejecución"));
        }
    }

    #[test]
    fn selecting_another_native_session_restores_its_composer_without_reusing_automation_origin() {
        let mut state = automation_state();
        state.sessions.push(crate::view::SessionView {
            id: "interactive".into(),
            title: "Tarea nueva".into(),
            project: state.workspace.clone(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
            native_automation: None,
        });
        state.selected_session = Some("interactive".into());
        let frame = UiTree::new(build_session_surface(&state, 900.0, false, false))
            .layout(Size::new(900.0, 760.0));
        assert!(frame.regions.iter().any(|region| region.id == "prompt"));
        assert!(!visible_text(&frame).contains("provider-original"));
        assert!(!visible_text(&frame).contains("987784fa-996e-4f54-83a6-a5c60b652e44"));
    }

    #[test]
    fn image_admission_renders_a_labelled_preview_in_conversation() {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[255, 10, 20, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        let image = UserImage::from_bytes(UserImageMime::Png, &png).unwrap();
        let expected_base64 = image.data_base64.clone();
        let events = vec![
            AgentEvent::new(
                "s1",
                1,
                AgentEventKind::TurnAdmittedWithImages {
                    turn_id: "turn-1".into(),
                    prompt: "Describe".into(),
                    model: "model".into(),
                    mode: TurnMode::Plan,
                    images: vec![image],
                },
            ),
            AgentEvent::new(
                "s1",
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Describe".into(),
                },
            ),
        ];
        let mut state = state();
        state.transcript = TranscriptProjection::from_events(&events).items().to_vec();
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::RasterImage { image, rect, .. }
                    if image.alt() == "Imagen adjunta 1 de 1"
                        && image.data_base64() == expected_base64
                        && rect.width <= 280.0 && rect.height <= 180.0
            )),
            "raster rectangles: {:?}; visible: {}",
            frame
                .commands
                .iter()
                .filter_map(|command| {
                    if let DrawCommand::RasterImage { rect, .. } = command {
                        Some(rect)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>(),
            visible_text(&frame)
        );
        assert!(visible_text(&frame).contains("Imagen adjunta 1 de 1"));
    }

    #[test]
    fn session_header_preserves_title_without_engine_badges_at_responsive_widths() {
        let mut state = state();
        for backend in ["codex", "native"] {
            state.selected_backend = backend.into();
            for width in [320.0, 520.0] {
                let frame = UiTree::new(header(&state, width, false, true))
                    .layout(Size::new(width, HEADER_HEIGHT));
                let title = visible_text(&frame);
                let painted_title = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Text { text, .. } if text.starts_with("Revisar") => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .expect("the session title remains visible beside the IDE control");
                assert_eq!(
                    painted_title, "Revisar la aplicación",
                    "compact chrome leaves the short session title intact at {width}px"
                );
                assert!(!title.contains("Codex CLI"));
                assert!(!title.contains("Nativo"));
                assert!(!frame
                    .regions
                    .iter()
                    .any(|region| region.id == "new_session_header"));
            }
        }
    }

    fn header_title_bands(
        frame: &openide_ui::Frame,
        title: &str,
    ) -> Vec<(Point, u8, openide_ui::Rect)> {
        let mut clips = Vec::new();
        let mut bands = Vec::new();
        for command in &frame.commands {
            match command {
                DrawCommand::PushClip(rect) => clips.push(*rect),
                DrawCommand::PopClip => {
                    clips.pop();
                }
                DrawCommand::Text {
                    text,
                    position,
                    color,
                    ..
                } if text == title => {
                    bands.push((
                        *position,
                        color.a,
                        *clips.last().expect("title is clipped to its own slot"),
                    ));
                }
                _ => {}
            }
        }
        bands
    }

    #[test]
    fn session_header_fades_by_painted_width_and_preserves_action_slots() {
        let mut state = state();
        state.selected_session_title = Some("W".repeat(24));
        let mut tree = UiTree::new(header(&state, 300.0, false, true));
        tree.set_text_measurer(|value, _, _| {
            value
                .chars()
                .map(|character| match character {
                    'W' => 14.0,
                    'i' => 2.0,
                    _ => 6.0,
                })
                .sum()
        });
        let wide = tree.layout(Size::new(300.0, HEADER_HEIGHT));
        let bands = header_title_bands(&wide, state.selected_session_title.as_deref().unwrap());
        assert!(bands.len() > 1, "overflow fades the original shaped line");
        assert_eq!(bands[0].1, theme::FOREGROUND.a);
        assert!(bands.last().unwrap().1 < 16);
        assert!(bands.windows(2).all(|pair| pair[0].1 >= pair[1].1));
        assert!(!wide.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.starts_with('W') && text.contains('…')
        )));
        let first_action = wide
            .regions
            .iter()
            .find(|region| region.id == "environment_open")
            .unwrap();
        assert!(
            bands
                .iter()
                .all(|(_, _, clip)| clip.x + clip.width + HEADER_GAP <= first_action.rect.x)
        );

        state.selected_session_title = Some("i".repeat(24));
        tree.set_root(header(&state, 300.0, false, true));
        let narrow = tree.layout(Size::new(300.0, HEADER_HEIGHT));
        assert!(visible_text(&narrow).contains(&"i".repeat(24)));
        let fitting = header_title_bands(&narrow, state.selected_session_title.as_deref().unwrap());
        assert_eq!(fitting.len(), 1, "fitting titles retain one opaque pass");
        assert_eq!(fitting[0].1, theme::FOREGROUND.a);
        let first_action_after = narrow
            .regions
            .iter()
            .find(|region| region.id == "environment_open")
            .unwrap();
        assert_eq!(first_action.rect, first_action_after.rect);
    }

    #[test]
    fn session_header_fades_joined_emoji_without_rewriting_the_shaped_line() {
        let mut state = state();
        state.selected_session_title = Some("👩‍💻".repeat(12));
        let mut tree = UiTree::new(header(&state, 300.0, false, true));
        tree.set_text_measurer(|value, _, _| {
            value
                .graphemes(true)
                .map(|grapheme| match grapheme {
                    "👩‍💻" => 20.0,
                    _ => 6.0,
                })
                .sum()
        });
        let frame = tree.layout(Size::new(300.0, HEADER_HEIGHT));
        let title = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, .. } if text.starts_with("👩‍💻") => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(title, state.selected_session_title.as_deref().unwrap());
        let graphemes = title.graphemes(true).collect::<Vec<_>>();
        assert_eq!(graphemes.len(), 12);
        assert!(graphemes.iter().all(|grapheme| *grapheme == "👩‍💻"));
        let bands = header_title_bands(&frame, title);
        assert!(bands.len() > 1);
        assert!(
            bands.windows(2).all(|pair| pair[0].0 == pair[1].0),
            "all alpha bands share the original Unicode shaping origin"
        );
    }

    #[test]
    fn native_session_title_fades_only_overflow_and_never_paints_over_header_actions() {
        let context = eframe::egui::Context::default();
        crate::fonts::install(&context);
        context.begin_pass(eframe::egui::RawInput::default());
        let mut state = state();
        let full_title = "Revisar los cambios del proyecto y la integración del editor. ".repeat(8);
        for width in [220.0, 300.0, 360.0, 520.0, 800.0] {
            for sidebar_hidden in [false, true] {
                state.sidebar_collapsed = sidebar_hidden;
                let mut previous_actions = None;
                for (title, overflowing) in [(full_title.as_str(), true), ("Revisar", false)] {
                    state.selected_session_title = Some(title.into());
                    let mut tree = UiTree::new(header(&state, width, false, true));
                    let fonts = context.clone();
                    tree.set_text_measurer(move |text, size, family| {
                        fonts.fonts_mut(|fonts| {
                            fonts
                                .layout_no_wrap(
                                    text.into(),
                                    eframe::egui::FontId::new(
                                        size,
                                        match family {
                                            FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => {
                                                eframe::egui::FontFamily::Proportional
                                            }
                                            FontFamily::Monospace => {
                                                eframe::egui::FontFamily::Monospace
                                            }
                                        },
                                    ),
                                    eframe::egui::Color32::WHITE,
                                )
                                .size()
                                .x
                        })
                    });
                    let frame = tree.layout(Size::new(width, HEADER_HEIGHT));
                    let bands = header_title_bands(&frame, title);
                    assert!(!bands.is_empty(), "title remains visible at {width}px");
                    assert_eq!(
                        bands
                            .iter()
                            .any(|(_, alpha, _)| *alpha < theme::FOREGROUND.a),
                        overflowing,
                        "native painted width decides whether to fade at {width}px"
                    );
                    assert!(frame.commands.iter().all(|command| !matches!(command,
                        DrawCommand::Text { text, .. } if text.starts_with("Revisar") && text != title
                    )), "the header never substitutes ellipsis or slices its title");
                    let actions: Vec<_> = frame
                        .regions
                        .iter()
                        .filter(|region| region.kind == HitKind::Button)
                        .map(|region| (region.id.clone(), region.rect))
                        .collect();
                    for (_, bounds) in &actions {
                        assert!(
                            bands.iter().all(|(_, _, clip)| {
                                clip.x + clip.width <= bounds.x || clip.x >= bounds.x + bounds.width
                            }),
                            "title fade cannot overlap the action hitbox at {width}px"
                        );
                    }
                    if let Some(previous) = &previous_actions {
                        assert_eq!(
                            &actions, previous,
                            "changing a title must not move header actions"
                        );
                    }
                    previous_actions = Some(actions);
                }
            }
        }
        context.end_pass().textures_delta.clear();
    }

    #[test]
    fn session_header_actions_stay_in_fixed_bounds_at_responsive_widths() {
        for width in [220.0, 260.0, 300.0, 470.0, 800.0] {
            let mut tree = UiTree::new(header(&state(), width, false, true));
            tree.set_reduced_motion(true);
            let viewport = Size::new(width, HEADER_HEIGHT);
            let frame = tree.layout(viewport);
            let mut actions = frame
                .regions
                .iter()
                .filter(|region| {
                    [
                        "environment_open",
                        "toggle_side_pane_header",
                        "toggle_settings_header",
                    ]
                    .contains(&region.id.as_str())
                })
                .collect::<Vec<_>>();
            actions.sort_by(|left, right| left.rect.x.total_cmp(&right.rect.x));
            assert_eq!(actions.len(), if width >= 300.0 { 3 } else { 1 });
            for action in &actions {
                assert_eq!(action.rect.width, HEADER_ACTION_SIZE);
                assert_eq!(action.rect.height, HEADER_ACTION_SIZE);
                assert!(action.rect.x >= HEADER_PADDING);
                assert!(action.rect.x + action.rect.width <= width - HEADER_PADDING);
                assert_eq!(action.rect.y, HEADER_PADDING);
                assert_eq!(
                    action.rect.y + action.rect.height,
                    HEADER_HEIGHT - HEADER_PADDING
                );
                let (glyph, ink) = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Icon { rect, tint, .. }
                            if rect.x >= action.rect.x
                                && rect.x + rect.width <= action.rect.x + action.rect.width
                                && rect.y >= action.rect.y
                                && rect.y + rect.height <= action.rect.y + action.rect.height =>
                        {
                            Some((*rect, *tint))
                        }
                        _ => None,
                    })
                    .expect("every header action retains its native icon");
                if action.id != "environment_open" {
                    assert_eq!(ink, theme::FOREGROUND, "available action: {}", action.id);
                }
                assert_eq!((glyph.width, glyph.height), (16.0, 16.0));
                assert_eq!(glyph.x, action.rect.x + 4.0);
                assert_eq!(glyph.y, action.rect.y + 4.0);
                let point = Point::new(action.rect.x + 12.0, action.rect.y + 12.0);
                tree.handle_event(UiEvent::PointerMove(point));
                let hovered = tree.layout(viewport);
                assert_eq!(
                    hovered
                        .regions
                        .iter()
                        .find(|region| region.id == action.id)
                        .unwrap()
                        .rect,
                    action.rect
                );
                tree.handle_event(UiEvent::PointerDown(point));
                assert!(tree.handle_event(UiEvent::PointerUp(point)).contains(
                    &openide_ui::UiAction::Clicked {
                        id: action.id.clone(),
                    }
                ));
            }
            assert!(actions.windows(2).all(|pair| {
                pair[0].rect.x + pair[0].rect.width + HEADER_GAP <= pair[1].rect.x
            }));
        }
    }

    #[test]
    fn session_header_is_one_flat_row_without_duplicate_project_metadata() {
        let mut state = state();
        state.workspace = format!("C:/repo/Proyecto{}", "x".repeat(90));
        state.backend_options[0].1 = format!("Agente {}", "Y".repeat(50));
        state.running = true;
        let mut tree = UiTree::new(header(&state, 300.0, false, true));
        tree.set_text_measurer(|value, _, _| value.chars().count() as f32 * 6.0);
        let frame = tree.layout(Size::new(300.0, HEADER_HEIGHT));
        let labels = visible_text(&frame);
        let painted_title = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, .. } if text.starts_with("Revisar") => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .expect("the flat header retains the session title beside the IDE control");
        assert_eq!(painted_title, "Revisar la aplicación");
        assert!(!labels.contains("Proyecto"));
        assert!(!labels.contains("Agente"));
        assert!(!labels.contains("EN CURSO"));
        assert!(frame.commands.iter().all(|command| !matches!(
            command,
            DrawCommand::StrokeRect { rect, .. }
                if rect.width >= 290.0 && rect.height >= 30.0
        )));
    }

    #[test]
    #[ignore = "export the conversation with its rail of turns, at rest and with the pointer on a mark"]
    fn export_turn_rail_visual_review() {
        let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/openade-turn-rail-review");
        std::fs::create_dir_all(&output).unwrap();
        let mut state = state();
        state.transcript.extend((0..9).flat_map(|index| {
            [
                TranscriptItem::User {
                    content: format!("Pregunta {index}: ¿por qué salta el contenido al pasar el puntero?"),
                    images: Vec::new(),
                },
                TranscriptItem::Assistant {
                    content: format!(
                        "Respuesta {index}. El fallo está en el cálculo del margen: la fila reserva \
                         su alto aunque no se pinte, así que el mensaje no cambia de tamaño."
                    ),
                    streaming: false,
                },
            ]
        }));
        state.turn_in_view = Some(6);
        let (width, height) = (1100.0, 640.0);
        for hovered in [false, true] {
            let root = build_session_surface(&state, width, false, true).background(theme::BACKGROUND);
            let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(root);
            tree.set_reduced_motion(true);
            let mut frame = tree.layout(Size::new(width, height));
            if hovered {
                let mark = frame
                    .regions
                    .iter()
                    .find(|region| region.id == format!("{TURN_JUMP}8"))
                    .unwrap()
                    .rect;
                tree.handle_event(UiEvent::PointerMove(Point::new(mark.x + 4.0, mark.y + 4.0)));
                frame = tree.layout(Size::new(width, height));
            }
            let name = if hovered { "hover" } else { "rest" };
            let path = output.join(format!("turn-rail-{name}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height).unwrap();
            let svg = std::fs::read(&path).unwrap();
            let parsed = resvg::usvg::Tree::from_data(
                &svg,
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(&parsed, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
            pixmap.save_png(path.with_extension("png")).unwrap();
            ctx.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn the_turn_rail_stands_beside_the_column_and_a_mark_leads_to_its_turn() {
        let mut state = state();
        state.transcript.extend((0..20).flat_map(|index| {
            [
                TranscriptItem::User {
                    content: format!("Pregunta {index}"),
                    images: Vec::new(),
                },
                TranscriptItem::Assistant {
                    content: format!("Respuesta {index}"),
                    streaming: false,
                },
            ]
        }));
        let marks = |frame: &openide_ui::Frame| -> Vec<openide_ui::HitRegion> {
            frame
                .regions
                .iter()
                .filter(|region| region.id.starts_with(TURN_JUMP))
                .cloned()
                .collect()
        };
        // With no room beside the reading column there is no rail.
        let narrow = UiTree::new(build_session_surface(&state, 800.0, false, true))
            .layout(Size::new(800.0, 700.0));
        assert!(marks(&narrow).is_empty());

        let size = Size::new(1000.0, 700.0);
        let mut tree = UiTree::new(build_session_surface(&state, 1000.0, false, true));
        tree.set_reduced_motion(true);
        let frame = tree.layout(size);
        let rail = marks(&frame);
        assert_eq!(rail.len(), 20);
        let column_left = (size.width - READING_WIDTH) / 2.0;
        assert!(rail.iter().all(|mark| mark.rect.x + mark.rect.width <= column_left));
        assert_eq!(turn_in_view(&state, &tree), Some(0));

        // The thirteenth turn's message is transcript row 24.
        let point = Point::new(rail[12].rect.x + 4.0, rail[12].rect.y + rail[12].rect.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            [openide_ui::UiAction::Clicked { id: format!("{TURN_JUMP}24") }]
        );
        tree.reveal_at_start(&turn_anchor(24), TURN_JUMP_MARGIN);
        tree.layout(size);
        let top = tree.anchor_offset(&turn_anchor(24)).expect("the message is laid out");
        assert!((top - USER_PIN_INSET).abs() < 0.5, "{top}");
        assert_eq!(turn_in_view(&state, &tree), Some(24));

        // The IDE's chat has no rail, whatever the room.
        state.ide.mode = crate::ide_workspace::WindowMode::Ide;
        let ide = UiTree::new(build_session_surface(&state, 1000.0, false, true)).layout(size);
        assert!(marks(&ide).is_empty());
    }

    #[test]
    fn transcript_fades_under_header_without_covering_controls() {
        let mut state = state();
        state
            .transcript
            .extend((0..20).map(|index| TranscriptItem::User {
                content: format!("Mensaje {index}"),
                images: Vec::new(),
            }));
        let mut tree = UiTree::new(build_session_surface(&state, 520.0, false, true));
        tree.set_reduced_motion(true);
        let size = Size::new(520.0, 700.0);
        let initial = tree.layout(size);
        assert!(
            !initial
                .commands
                .iter()
                .any(|command| matches!(command, DrawCommand::FillVerticalGradient { top, bottom, .. } if top.a > bottom.a)),
            "the first message stays fully legible before scrolling"
        );
        let transcript = initial
            .regions
            .iter()
            .find(|region| region.id == "transcript")
            .unwrap()
            .rect;
        tree.handle_event(UiEvent::ScrollDirect {
            position: Point::new(transcript.x + transcript.width / 2.0, transcript.y + 40.0),
            delta_y: -120.0,
        });
        let frame = tree.layout(size);
        let fade_index = frame
            .commands
            .iter()
            .position(|command| matches!(command, DrawCommand::FillVerticalGradient { top, bottom, .. } if top.a > bottom.a))
            .expect("fade de transcript");
        let DrawCommand::FillVerticalGradient { rect, top, bottom } = frame.commands[fade_index]
        else {
            unreachable!()
        };
        assert_eq!(rect.y, HEADER_HEIGHT);
        assert_eq!(rect.height, HEADER_FADE_HEIGHT);
        assert_eq!(top.a, 255);
        assert_eq!(bottom.a, 0);
        let transcript = frame
            .regions
            .iter()
            .find(|region| region.id == "transcript")
            .unwrap();
        let settings = frame
            .regions
            .iter()
            .find(|region| region.id == "toggle_settings_header")
            .unwrap();
        assert_eq!(transcript.rect.y, HEADER_HEIGHT);
        assert!(settings.rect.y + settings.rect.height <= transcript.rect.y);
        assert!(frame.commands[..fade_index].iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text.contains("Mensaje")
        )));
    }

    #[test]
    fn transcript_bottom_fade_stays_above_the_composer_and_clears_at_the_last_message() {
        for width in [360.0, 900.0] {
            for show_header in [false, true] {
                let mut state = state();
                state.prompt = "Un borrador que debe seguir editable".into();
                state
                    .transcript
                    .extend((0..30).map(|index| TranscriptItem::User {
                        content: format!("Mensaje de prueba {index}"),
                        images: Vec::new(),
                    }));
                let root = if show_header {
                    build_session_surface(&state, width, false, true)
                } else {
                    build_session_body(&state, width, false, false)
                };
                let mut tree = UiTree::new(root);
                tree.set_reduced_motion(true);
                let size = Size::new(width, 700.0);
                let frame = tree.layout(size);
                let transcript = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "transcript")
                    .unwrap()
                    .rect;
                let prompt = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "prompt")
                    .unwrap()
                    .rect;
                let (fade_index, fade) = frame
                    .commands
                    .iter()
                    .enumerate()
                    .find_map(|(index, command)| match command {
                        DrawCommand::FillVerticalGradient { rect, top, bottom }
                            if top.a == 0 && bottom.a == 255 =>
                        {
                            Some((index, *rect))
                        }
                        _ => None,
                    })
                    .expect("overflowing conversation has a bottom scrim");
                assert_eq!(fade.height, TRANSCRIPT_BOTTOM_FADE_HEIGHT);
                assert!((fade.y + fade.height - transcript.y - transcript.height).abs() < 0.01);
                assert!(fade.intersection(prompt).is_none());
                assert!(
                    frame.commands[..fade_index]
                        .iter()
                        .any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text.contains("Mensaje de prueba")))
                );
                for region in frame.regions.iter().filter(|region| {
                    region.id == "prompt"
                        || region.id.starts_with("composer_")
                        || region.id == "run_toggle"
                }) {
                    assert!(
                        fade.intersection(region.rect).is_none(),
                        "{} overlaps the transcript scrim",
                        region.id
                    );
                }
                tree.handle_event(UiEvent::ScrollDirect {
                    position: Point::new(transcript.x + 40.0, transcript.y + 40.0),
                    delta_y: -100_000.0,
                });
                let end = tree.layout(size);
                assert!(!end.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillVerticalGradient { top, bottom, .. } if top.a == 0 && bottom.a == 255)));
                assert!(visible_text(&end).contains("Mensaje de prueba 29"));
                assert_eq!(
                    end.regions
                        .iter()
                        .find(|region| region.id == "prompt")
                        .unwrap()
                        .rect,
                    prompt
                );
                let point = Point::new(prompt.x + 8.0, prompt.y + 8.0);
                assert_eq!(end.hit_test(point), Some("prompt"));
                tree.handle_event(UiEvent::PointerDown(point));
                tree.handle_event(UiEvent::PointerUp(point));
                assert_eq!(tree.focused_id(), Some("prompt"));
                assert!(tree.handle_event(UiEvent::Text("!".into())).iter().any(|action| matches!(action,
                    openide_ui::UiAction::InputChanged { id, value } if id == "prompt" && value.contains('!'))));
            }
        }
    }

    #[test]
    fn a_resume_that_started_nothing_says_why_on_its_card() {
        let texts = |error| {
            let frame = UiTree::new(native_recovery_card("s1", &SessionRecoveryView::Resume, error))
                .layout(Size::new(600.0, 300.0));
            frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(!texts(None).contains("No se pudo reanudar"));
        let failed = texts(Some("Falta la clave API del proveedor nativo"));
        assert!(failed.contains("No se pudo reanudar: Falta la clave API"), "{failed}");
        assert!(failed.contains("Reanudar turno"), "the action stays: {failed}");
    }

    #[test]
    fn recovery_alert_keeps_its_action_reachable_on_narrow_surfaces() {
        for width in [260.0, 520.0] {
            let mut tree =
                UiTree::new(native_recovery_card("s1", &SessionRecoveryView::VerifyTool, None));
            let frame = tree.layout(Size::new(width, 360.0));
            let control = frame
                .regions
                .iter()
                .find(|region| region.id == "session_recover:s1")
                .expect("recovery action");
            assert!(control.rect.x >= 0.0 && control.rect.y >= 0.0);
            assert!(control.rect.x + control.rect.width <= width);
            assert!(control.rect.y + control.rect.height <= 360.0);
            let point = Point::new(control.rect.x + 8.0, control.rect.y + 8.0);
            assert_eq!(frame.hit_test(point), Some("session_recover:s1"));
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![openide_ui::UiAction::Clicked {
                    id: "session_recover:s1".into()
                }]
            );
        }
    }

    #[test]
    fn root_recovery_card_routes_to_resume_and_invalid_journal_is_read_only() {
        let mut state = state();
        state
            .session_recovery
            .insert("s1".into(), SessionRecoveryView::VerifyTool);
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "session_recover:s1"));

        state.session_recovery.insert(
            "s1".into(),
            SessionRecoveryView::InvalidJournal("secuencia inválida".into()),
        );
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "session_recover:s1"));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text.contains("secuencia inválida")
        )));
    }

    #[test]
    fn completed_plan_has_explicit_build_handoff() {
        let mut state = state();
        state.session_plans.insert(
            "s1".into(),
            SessionPlanView {
                snapshot: openide_core::PlanSnapshot::from_plan_response(
                    "1. Inspect the project\n2. Implement the change",
                ),
                can_build: true,
            },
        );
        state.transcript.push(TranscriptItem::PlanSnapshot {
            id: "plan:s1:1".into(),
            sequence: Some(1),
            plan: state.session_plans["s1"].snapshot.clone(),
            is_proposal: true,
            tool_call_id: None,
        });
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "session_build_plan:s1"));
        state.session_plans.get_mut("s1").unwrap().can_build = false;
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "session_build_plan:s1"));
    }

    #[test]
    fn compact_session_preserves_composer_controls_inside_dock() {
        let frame = UiTree::new(build_session_surface(&state(), 300.0, false, false))
            .layout(Size::new(300.0, 500.0));
        for id in [
            "toggle_settings_header",
            "prompt",
            "run_toggle",
            "composer_add_context",
        ] {
            let region = frame.regions.iter().find(|region| region.id == id).unwrap();
            assert!(region.rect.x >= 0.0, "{id}");
            assert!(region.rect.x + region.rect.width <= 300.0, "{id}");
            assert!(region.rect.y + region.rect.height <= 500.0, "{id}");
        }
    }

    #[test]
    fn active_composer_separates_project_context_and_keeps_controls_inside() {
        for width in [220.0, 300.0, 520.0, 900.0] {
            let frame = UiTree::new(composer(&state(), width)).layout(Size::new(width, 260.0));
            let card = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect {
                        rect,
                        color,
                        radius,
                    } if *color == openide_ui::Color::rgb(38, 38, 38) && *radius == 10.0 => {
                        Some(*rect)
                    }
                    _ => None,
                })
                .expect("rounded composer card");
            assert!(card.x >= 0.0 && card.x + card.width <= width, "{width}");
            assert!((card.x - (width - card.x - card.width)).abs() <= 1.0);
            assert!(
                card.height >= 52.0 && card.height <= 230.0,
                "{width}: {card:?}"
            );
            let ids = if width < 320.0 {
                vec!["prompt", "run_toggle", "composer_add_context"]
            } else {
                vec![
                    "prompt",
                    "run_toggle",
                    "composer_model_toggle",
                    "composer_add_context",
                ]
            };
            for id in ids {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap_or_else(|| panic!("missing {id} at {width}"))
                    .rect;
                assert!(rect.x >= card.x, "{id} at {width}: {rect:?} vs {card:?}");
                assert!(
                    rect.x + rect.width <= card.x + card.width,
                    "{id} at {width}: {rect:?} vs {card:?}"
                );
                assert!(rect.y >= card.y, "{id} at {width}");
                assert!(
                    rect.y + rect.height <= card.y + card.height,
                    "{id} at {width}"
                );
            }
            assert!(!visible_text(&frame).contains("Listo"));
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::StrokeRect { rect, radius, .. }
                    if *rect == card && *radius == 10.0
            )));
        }
    }

    #[test]
    fn active_composer_clips_long_project_and_status_without_covering_submit() {
        let mut state = state();
        state.workspace = format!("C:/repo/{}", "Proyecto muy largo ".repeat(12));
        state.status = "Ejecutando una tarea con una descripción de estado muy extensa".into();
        state.running = true;
        for width in [300.0, 520.0] {
            let mut tree = UiTree::new(composer(&state, width));
            tree.set_text_measurer(|value, _, _| value.chars().count() as f32 * 7.0);
            let frame = tree.layout(Size::new(width, 260.0));
            let card = frame.commands.iter().find_map(|command| match command {
                DrawCommand::FillRect {
                    rect,
                    color,
                    radius,
                } if *color == openide_ui::Color::rgb(38, 38, 38) && *radius == 10.0 => Some(*rect),
                _ => None,
            });
            let card = card.expect("rounded composer card");
            let submit = frame
                .regions
                .iter()
                .find(|region| region.id == "run_toggle")
                .expect("stop action");
            assert_eq!(submit.rect.width, 28.0);
            assert!(submit.rect.x + submit.rect.width <= card.x + card.width);
            let project = frame.regions.iter()
                .find(|region| region.id == "composer_project_toggle")
                .expect("the clipped project retains its full-path hover target");
            assert!(project.rect.width >= 72.0);
            assert!(project.rect.x >= card.x && project.rect.x + project.rect.width <= card.x + card.width);
            assert!(project.rect.y + project.rect.height <= card.y);
            assert_eq!(frame.hit_test(openide_ui::Point::new(
                project.rect.x + project.rect.width / 2.0,
                project.rect.y + project.rect.height / 2.0,
            )), Some("composer_project_toggle"));
        }
    }

    #[test]
    fn window_height_limits_editor_growth_and_retains_a_readable_transcript() {
        {
            for (width, height, environment_open) in [
                (960.0, 320.0, false),
                (960.0, 540.0, false),
                (560.0, 540.0, true),
            ] {
                let mut state = state();
                state.selected_session = Some("height-budget".into());
                state.sidebar_collapsed = true;
                state.show_side_pane = false;
                state.environment.open = environment_open;
                state.prompt = "Instrucción larga que debe conservarse completa.\n".repeat(40);
                state.transcript = vec![TranscriptItem::Assistant {
                    content: format!(
                        "{}VISIBLE_HEIGHT_BUDGET_TAIL",
                        "Respuesta retenida.\n\n".repeat(40)
                    ),
                    streaming: true,
                }];
                let viewport = Size::new(width, height);
                let mut tree = UiTree::new(crate::view::build_window_view(&state, width, height));
                tree.set_reduced_motion(true);
                tree.layout(viewport);
                tree.scroll_to_end("transcript");
                let frame = tree.layout(viewport);
                let region = |id: &str| {
                    frame
                        .regions
                        .iter()
                        .find(|region| region.id == id)
                        .unwrap()
                        .rect
                };
                let transcript = region("transcript");
                let editor = region("prompt");
                // At the minimum 320px window the one-line editor needs an
                // extra pixel beyond the target budget, leaving 99px of chat.
                assert!(
                    transcript.height >= 98.9,
                    "{width}x{height}, environment={environment_open}: {transcript:?}"
                );
                let editor_limit = 180.0;
                assert!(
                    editor.height >= 36.0 && editor.height <= editor_limit,
                    "{editor:?}"
                );
                assert!(
                    transcript.y + transcript.height <= editor.y,
                    "the growing editor must not overlap the readable transcript"
                );
                assert!(region("run_toggle").y >= editor.y + editor.height);
                let metadata = region("composer_context_toggle");
                assert!(metadata.y + metadata.height <= height - 4.0);
                assert!(visible_text(&frame).contains("VISIBLE_HEIGHT_BUDGET_TAIL"));
                assert!(tree.is_scroll_at_end("transcript", 0.1));
            }
        }
    }

    #[test]
    fn active_composer_grows_for_multiline_drafts_and_caps_editor_height() {
        for width in [300.0, 800.0] {
            let mut state = state();
            let short = UiTree::new(composer(&state, width)).layout(Size::new(width, 400.0));
            let short_height = short
                .regions
                .iter()
                .find(|region| region.id == "prompt")
                .unwrap()
                .rect
                .height;
            state.prompt = "una línea\n".repeat(5);
            let multiline = UiTree::new(composer(&state, width)).layout(Size::new(width, 400.0));
            let multiline_height = multiline
                .regions
                .iter()
                .find(|region| region.id == "prompt")
                .unwrap()
                .rect
                .height;
            assert!(multiline_height > short_height, "{width}");
            state.prompt = "texto de prueba\n".repeat(40);
            let long = UiTree::new(composer(&state, width)).layout(Size::new(width, 400.0));
            let capped_height = long
                .regions
                .iter()
                .find(|region| region.id == "prompt")
                .unwrap()
                .rect
                .height;
            assert!(capped_height >= multiline_height);
            assert!(
                capped_height
                    <= if width < COMPACT_BREAKPOINT {
                        152.0
                    } else {
                        216.0
                    }
            );
        }
    }

    #[test]
    fn work_summary_uses_only_actual_tool_results() {
        let mut state = state();
        state.transcript = vec![
            TranscriptItem::User {
                content: "Revisa src/main.rs".into(),
                images: Vec::new(),
            },
            TranscriptItem::Tool {
                id: "a".into(),
                name: "read_file".into(),
                arguments: json!({"path": "src/main.rs"}),
                output: Some("fn main() {}".into()),
                is_error: false,
                activity: ActivityStatus::Completed,
            },
            TranscriptItem::Tool {
                id: "b".into(),
                name: "search_files".into(),
                arguments: json!({"query": "main"}),
                output: None,
                is_error: false,
                activity: ActivityStatus::Interrupted,
            },
        ];
        state.selected_tool_id = Some("a".into());
        let mut tree = UiTree::new(build_session_surface(&state, 520.0, false, true));
        let frame = tree.layout(Size::new(520.0, 1000.0));
        let visible = visible_text(&frame);
        // The uncertain search keeps the group open and says why.
        assert!(visible.contains("Exploró"), "{visible}");
        assert!(visible.contains("main.rs, 1 búsqueda"), "{visible}");
        assert!(visible.contains("Resultado incierto"), "{visible}");
        assert!(visible.contains("Leyó"), "{visible}");
        assert!(!visible.contains("fn main() {}"), "{visible}");
        let open_file = tool_presentation::navigation_action(
            state.selected_session.as_deref().unwrap_or("draft"),
            "a",
        );
        assert!(frame.regions.iter().any(|region| region.id == open_file));
        assert!(frame.regions.iter().any(|region| region.id == "tool:b"));
        let read = frame
            .regions
            .iter()
            .find(|region| region.kind == openide_ui::HitKind::Button && region.id == open_file)
            .unwrap()
            .rect;
        let now = std::time::Instant::now();
        tree.handle_event_at(
            openide_ui::UiEvent::PointerMove(openide_ui::Point::new(
                read.x + read.width / 2.0,
                read.y + read.height / 2.0,
            )),
            now,
        );
        let hinted = tree.layout_at(
            Size::new(520.0, 1000.0),
            now + std::time::Duration::from_secs(1),
        );
        assert!(
            visible_text(&hinted).contains("src/main.rs"),
            "read tooltip preserves the full path"
        );
    }

    #[test]
    fn pending_permission_is_answered_in_the_transcript_without_a_composer_banner() {
        let mut state = state();
        state.pending_permissions.push(PermissionView {
            request_id: "request-1".into(),
            tool_name: "write_file".into(),
            arguments: "{\"path\":\"src/main.rs\"}".into(),
        });
        let frame = UiTree::new(build_session_surface(&state, 520.0, true, true))
            .layout(Size::new(520.0, 780.0));
        let decisions: Vec<_> = frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("approval_decide:"))
            .collect();
        assert_eq!(decisions.len(), 3, "one card, three answers, no duplicate");
        let transcript = frame
            .regions
            .iter()
            .find(|region| region.id == "transcript")
            .unwrap()
            .rect;
        for decision in decisions {
            assert!(decision.rect.x >= transcript.x);
            assert!(decision.rect.x + decision.rect.width <= transcript.x + transcript.width);
        }
    }

    // Frozen pre-index implementation for full render/interaction equivalence.
    fn legacy_related_subagent_cards(state: &ViewState, compact: bool) -> Option<Element> {
        let selected = state.selected_session.as_deref()?;
        let related = |child: &str, parent: &str| selected == child || selected == parent;
        let mut cards: Vec<Element> = state
            .pending_subagent_permissions
            .iter()
            .filter(|permission| {
                related(&permission.child_session_id, &permission.parent_session_id)
            })
            .map(|permission| subagent_permission_card(permission, state, compact))
            .collect();
        // Put explicit recovery ahead of the durable overview so its action remains
        // reachable even when a parent has many completed children.
        if let Some(run) = state
            .recoverable_subagent
            .as_ref()
            .filter(|run| selected == run.child_session_id)
        {
            cards.push(recoverable_subagent_card(run));
        }
        cards.extend(
            state
                .subagent_sessions
                .iter()
                .filter(|run| related(&run.child_session_id, &run.parent_session_id))
                .map(|run| {
                    persisted_subagent_card(
                        run,
                        selected,
                        state
                            .active_subagents
                            .iter()
                            .any(|live| live.child_session_id == run.child_session_id),
                        state.expanded_subagent_inputs.as_deref()
                            == Some(run.child_session_id.as_str()),
                        state.subagent_input_edit.as_ref(),
                        None,
                        None,
                    )
                }),
        );
        cards.extend(
            state
                .active_subagents
                .iter()
                .filter(|run| {
                    related(&run.child_session_id, &run.parent_session_id)
                        && !state
                            .subagent_sessions
                            .iter()
                            .any(|saved| saved.child_session_id == run.child_session_id)
                })
                .map(|run| active_subagent_card(run, selected)),
        );
        if cards.is_empty() {
            return None;
        }
        let height = (cards.len() as f32 * 210.0 + 16.0).min(if compact { 330.0 } else { 350.0 });
        Some(
            ScrollArea::new(
                "related_subagents",
                Element::column(cards).gap(8.0).padding(8.0),
            )
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .max_height(height)
            .background(theme::BACKGROUND),
        )
    }

    #[test]
    fn related_subagent_indexes_preserve_full_frames_priority_and_duplicate_ids() {
        use crate::view::SubagentSessionView;
        use openide_harness::SubagentStatus;

        let saved = |child: &str, parent: &str, title: &str| SubagentSessionView {
            child_session_id: child.into(),
            parent_session_id: parent.into(),
            title: title.into(),
            status: SubagentStatus::Running,
            cancellation_requested: false,
            open_inputs: 0,
            inputs: vec![],
            dispatch_error: None,
            depth: 1,
            final_response: None,
        };
        let live = |child: &str, parent: &str, title: &str| SubagentRunView {
            child_session_id: child.into(),
            parent_session_id: parent.into(),
            title: title.into(),
        };
        let mut fixture = state();
        fixture.pending_subagent_permissions = vec![
            SubagentPermissionView {
                child_session_id: "child-1".into(),
                parent_session_id: "parent-1".into(),
                request_id: "priority-request".into(),
                tool_name: "write_file".into(),
                arguments: "{\"path\":\"src/main.rs\"}".into(),
            },
            SubagentPermissionView {
                child_session_id: "unrelated-child".into(),
                parent_session_id: "other-parent".into(),
                request_id: "unrelated-request".into(),
                tool_name: "run_command".into(),
                arguments: "{}".into(),
            },
        ];
        fixture.recoverable_subagent =
            Some(live("child-1", "parent-1", "Recovery before saved cards"));
        fixture.subagent_sessions = vec![
            saved("child-1", "parent-1", "Saved first record"),
            saved("child-1", "parent-1", "Saved duplicate record"),
            saved("child-2", "parent-1", "Bridge matched by global live ID"),
            saved(
                "shadow",
                "other-parent",
                "Unrelated saved record suppresses live duplicate",
            ),
        ];
        fixture.active_subagents = vec![
            live(
                "child-1",
                "parent-1",
                "Live counterpart suppressed by saved ID",
            ),
            live(
                "child-1",
                "parent-1",
                "Duplicate live counterpart suppressed",
            ),
            live(
                "child-2",
                "other-parent",
                "Unrelated live record still supplies bridge",
            ),
            live("shadow", "parent-1", "Suppressed by unrelated saved record"),
            live("live-only", "parent-1", "First unsaved live record"),
            live("live-only", "parent-1", "Second unsaved live record"),
            live("unrelated-live", "other-parent", "Unrelated live record"),
        ];
        // Failed saved records only offer cancellation when a live ID bridge
        // exists, even if that live record has an unrelated parent.
        fixture.subagent_sessions[2].status = SubagentStatus::Failed;

        for selected in [
            None,
            Some("parent-1"),
            Some("child-1"),
            Some("child-2"),
            Some("not-present"),
        ] {
            fixture.selected_session = selected.map(str::to_owned);
            for settings in [false, true] {
                fixture.show_settings = settings;
                for width in [360.0, 900.0] {
                    let current = related_subagent_cards(&fixture, width < 500.0);
                    let legacy = legacy_related_subagent_cards(&fixture, width < 500.0);
                    assert_eq!(current.is_some(), legacy.is_some());
                    let (Some(current), Some(legacy)) = (current, legacy) else {
                        continue;
                    };
                    let mut current = UiTree::new(current);
                    let mut legacy = UiTree::new(legacy);
                    current.set_reduced_motion(true);
                    legacy.set_reduced_motion(true);
                    let mut seen_actions = HashSet::new();
                    let mut seen_text = String::new();
                    for scroll in 0..24 {
                        let frame = current.layout(Size::new(width, 600.0));
                        assert_eq!(
                            frame,
                            legacy.layout(Size::new(width, 600.0)),
                            "selected={selected:?}, width={width}, settings={settings}, scroll={scroll}"
                        );
                        seen_actions.extend(frame.regions.iter().map(|region| region.id.clone()));
                        seen_text.push_str(&visible_text(&frame));
                        if scroll == 0 && selected == Some("child-1") {
                            let permission = frame.commands.iter().position(|command| matches!(command, DrawCommand::Text { text, .. } if text.contains("Escribir archivo"))).unwrap();
                            let recovery = frame.commands.iter().position(|command| matches!(command, DrawCommand::Text { text, .. } if text.contains("Recovery before saved cards")));
                            if let Some(recovery) = recovery {
                                assert!(permission < recovery);
                            }
                        }
                        let event = UiEvent::Scroll {
                            position: Point::new(width / 2.0, 160.0),
                            delta_y: -180.0,
                        };
                        assert_eq!(
                            current.handle_event(event.clone()),
                            legacy.handle_event(event)
                        );
                    }
                    if selected == Some("child-1") {
                        let review = crate::approval_ui::ApprovalTarget {
                            owner_session_id: "child-1".into(),
                            workspace: fixture.workspace.clone(),
                            child_session_id: Some("child-1".into()),
                            request_id: "priority-request".into(),
                        }
                        .decision_action("allow_once");
                        for id in [
                            review.as_str(),
                            "subagent_resume:child-1",
                        ] {
                            assert!(
                                seen_actions.contains(id),
                                "{id} remains reachable at {width}"
                            );
                        }
                    }
                    if selected == Some("parent-1") {
                        for title in [
                            "Saved first record",
                            "Saved duplicate record",
                            "First unsaved live record",
                            "Second unsaved live record",
                        ] {
                            assert!(
                                seen_text.contains(title),
                                "duplicate vectors still render: {title}"
                            );
                        }
                        assert!(!seen_text.contains("Suppressed by unrelated saved record"));
                        assert!(
                            seen_actions.contains("subagent_cancel:child-2"),
                            "global live ID preserves the saved bridge"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn related_subagent_cards_keep_permission_and_cancel_actions() {
        let mut state = state();
        state
            .pending_subagent_permissions
            .push(SubagentPermissionView {
                child_session_id: "child-1".into(),
                parent_session_id: "s1".into(),
                request_id: "request-1".into(),
                tool_name: "write_file".into(),
                arguments: "{\"path\":\"src/main.rs\"}".into(),
            });
        state.active_subagents.push(SubagentRunView {
            child_session_id: "child-1".into(),
            parent_session_id: "s1".into(),
            title: "Actualizar pruebas".into(),
        });
        state.active_subagents.push(SubagentRunView {
            child_session_id: "other-child".into(),
            parent_session_id: "other-parent".into(),
            title: "Otra tarea".into(),
        });
        for selected in ["s1", "child-1"] {
            state.selected_session = Some(selected.into());
            let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
                .layout(Size::new(520.0, 780.0));
            let review = crate::approval_ui::ApprovalTarget {
                owner_session_id: selected.into(),
                workspace: state.workspace.clone(),
                child_session_id: Some("child-1".into()),
                request_id: "request-1".into(),
            }
            .decision_action("allow_once");
            for id in [
                review.as_str(),
                "subagent_cancel:child-1",
            ] {
                assert!(
                    frame.regions.iter().any(|region| region.id == id),
                    "selected {selected} missing {id}"
                );
            }
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "subagent_cancel:other-child"));
        }
    }

    #[test]
    fn persisted_subagent_states_and_mailboxes_are_visible_without_a_worker() {
        use openide_harness::SubagentStatus;
        for (status, label) in [
            (SubagentStatus::Ready, "Pendiente"),
            (SubagentStatus::Running, "En ejecución"),
            (SubagentStatus::Interrupted, "Interrumpido"),
            (SubagentStatus::Completed, "Completado"),
            (SubagentStatus::Failed, "Fallido"),
            (SubagentStatus::Cancelled, "Cancelado"),
        ] {
            let mut state = state();
            state
                .subagent_sessions
                .push(crate::view::SubagentSessionView {
                    child_session_id: "child-1".into(),
                    parent_session_id: "s1".into(),
                    title: "Tarea persistida".into(),
                    status,
                    cancellation_requested: true,
                    open_inputs: 2,
                    inputs: vec![],
                    dispatch_error: None,
                    depth: 1,
                    final_response: None,
                });
            for selected in ["s1", "child-1"] {
                state.selected_session = Some(selected.into());
                for width in [360.0, 900.0] {
                    let frame = UiTree::new(related_subagent_cards(&state, width < 500.0).unwrap())
                        .layout(Size::new(width, 600.0));
                    let content = visible_text(&frame);
                    assert!(content.contains(label), "{content}");
                    assert!(content.contains("2 entradas pendientes"), "{content}");
                    assert!(content.contains("Cancelación solicitada"), "{content}");
                    let target = if selected == "s1" { "child-1" } else { "s1" };
                    assert!(frame
                        .regions
                        .iter()
                        .any(|r| r.id == format!("subagent_open:{target}")));
                    assert!(!frame
                        .regions
                        .iter()
                        .any(|r| r.id.starts_with("subagent_cancel:")));
                    assert!(!frame
                        .regions
                        .iter()
                        .any(|r| r.id.starts_with("subagent_resume:")));
                }
            }
            state.selected_session = Some("unrelated".into());
            assert!(related_subagent_cards(&state, false).is_none());
        }
    }

    #[test]
    fn persisted_cancellation_controls_follow_lifecycle_without_live_bridge() {
        use openide_harness::SubagentStatus;
        for (status, inputs, offered) in [
            (SubagentStatus::Ready, 0, true),
            (SubagentStatus::Running, 0, true),
            (SubagentStatus::Interrupted, 1, false),
            (SubagentStatus::Completed, 0, false),
            (SubagentStatus::Completed, 1, true),
            (SubagentStatus::Failed, 1, false),
            (SubagentStatus::Cancelled, 1, false),
        ] {
            let mut state = state();
            state
                .subagent_sessions
                .push(crate::view::SubagentSessionView {
                    child_session_id: "child-1".into(),
                    parent_session_id: "s1".into(),
                    title: "Subagente persistido".into(),
                    status,
                    cancellation_requested: false,
                    open_inputs: inputs,
                    inputs: vec![],
                    dispatch_error: None,
                    depth: 1,
                    final_response: None,
                });
            for width in [360.0, 900.0] {
                let frame = UiTree::new(related_subagent_cards(&state, width < 500.0).unwrap())
                    .layout(Size::new(width, 600.0));
                assert_eq!(
                    frame
                        .regions
                        .iter()
                        .any(|r| r.id == "subagent_cancel:child-1"),
                    offered,
                    "{status:?}, {width}"
                );
            }
        }
    }

    #[test]
    fn persisted_mailbox_details_are_complete_and_scrollable() {
        use openide_harness::{SubagentInputSnapshot, SubagentInputStatus, SubagentStatus};
        let mut state = state();
        state.expanded_subagent_inputs = Some("child-1".into());
        state
            .subagent_sessions
            .push(crate::view::SubagentSessionView {
                child_session_id: "child-1".into(),
                parent_session_id: "s1".into(),
                title: "Mailbox".into(),
                status: SubagentStatus::Interrupted,
                cancellation_requested: false,
                open_inputs: 2,
                depth: 1,
                final_response: None,
                dispatch_error: None,
                inputs: [
                    SubagentInputStatus::Pending,
                    SubagentInputStatus::Active,
                    SubagentInputStatus::Done,
                    SubagentInputStatus::Skipped,
                ]
                .into_iter()
                .enumerate()
                .map(|(index, status)| SubagentInputSnapshot {
                    id: format!("input-{index}"),
                    mode: openide_core::TurnMode::Build,
                    status,
                    revision: 0,
                    prompt: format!("Instrucción exacta {index}\n{}", "contexto\n".repeat(100)),
                })
                .collect(),
            });
        for selected in ["s1", "child-1"] {
            state.selected_session = Some(selected.into());
            for width in [360.0, 900.0] {
                let mut tree = UiTree::new(persisted_subagent_card(
                    &state.subagent_sessions[0],
                    selected,
                    false,
                    true,
                    None,
                    None,
                    None,
                ));
                let start = std::time::Instant::now();
                let mut now = start;
                let mut frame = tree.layout_at(Size::new(width, 600.0), now);
                let viewport = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "subagent_mailbox:child-1")
                    .unwrap()
                    .rect;
                let position = openide_ui::Point::new(viewport.x + 10.0, viewport.y + 10.0);
                let mut content = visible_text(&frame);
                assert!(!tree.is_scroll_at_end("subagent_mailbox:child-1", 0.0));
                let mut steps = 0;
                while !tree.is_scroll_at_end("subagent_mailbox:child-1", 0.0) {
                    tree.handle_event_at(
                        openide_ui::UiEvent::Scroll {
                            position,
                            delta_y: -100.0,
                        },
                        now,
                    );
                    steps += 1;
                    now = start + std::time::Duration::from_secs_f64(steps as f64 / 120.0);
                    frame = tree.layout_at(Size::new(width, 600.0), now);
                    content.push_str(&visible_text(&frame));
                    assert!(steps < 256, "mailbox scroll must reach the end");
                }
                // is_scroll_at_end reflects the requested wheel target. Let
                // the visible offset finish easing before inspecting its tail.
                for _ in 0..36 {
                    steps += 1;
                    now = start + std::time::Duration::from_secs_f64(steps as f64 / 120.0);
                    frame = tree.layout_at(Size::new(width, 600.0), now);
                    content.push_str(&visible_text(&frame));
                }
                for expected in [
                    "Pendiente",
                    "Activa",
                    "Completada",
                    "Omitida",
                    "Instrucción exacta 0",
                    "input-3",
                    "Ocultar entradas",
                ] {
                    assert!(
                        content.contains(expected),
                        "{expected} at {width}: {content}"
                    );
                }
                assert!(tree.scroll_offset("subagent_mailbox:child-1") > 0.0);
                assert!(tree.is_scroll_at_end("subagent_mailbox:child-1", 1.0));
                assert!(frame
                    .regions
                    .iter()
                    .any(|region| region.id == "subagent_inputs:child-1"));
            }
        }
    }

    #[test]
    fn mailbox_edit_form_is_reachable_on_small_parent_and_child_surfaces() {
        let mut state = state();
        state
            .subagent_sessions
            .push(crate::view::SubagentSessionView {
                child_session_id: "child-1".into(),
                parent_session_id: "s1".into(),
                title: "Mailbox".into(),
                status: openide_harness::SubagentStatus::Completed,
                cancellation_requested: false,
                open_inputs: 1,
                inputs: vec![],
                dispatch_error: None,
                depth: 1,
                final_response: None,
            });
        state.subagent_input_edit = Some(crate::view::SubagentInputEditView {
            child_session_id: "child-1".into(),
            input_id: "input-1".into(),
            expected_revision: 0,
            prompt: "Borrador\ncon contexto".into(),
        });
        for selected in ["s1", "child-1"] {
            state.selected_session = Some(selected.into());
            for width in [360.0, 900.0] {
                let mut tree = UiTree::new(related_subagent_cards(&state, width < 500.0).unwrap());
                tree.layout(Size::new(width, 600.0));
                for id in [
                    "subagent_input_prompt",
                    "subagent_input_save",
                    "subagent_input_cancel",
                ] {
                    assert!(tree.focus(id));
                    let frame = tree.layout(Size::new(width, 600.0));
                    let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                    assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width);
                    assert!(region.rect.height > 0.0);
                }
            }
        }
    }

    #[test]
    fn mailbox_skip_control_is_reachable_only_for_pending_entries() {
        let mut state = state();
        state.expanded_subagent_inputs = Some("child-1".into());
        state
            .subagent_sessions
            .push(crate::view::SubagentSessionView {
                child_session_id: "child-1".into(),
                parent_session_id: "s1".into(),
                title: "Mailbox".into(),
                status: openide_harness::SubagentStatus::Completed,
                cancellation_requested: false,
                open_inputs: 2,
                depth: 1,
                final_response: None,
                dispatch_error: None,
                inputs: vec![
                    openide_harness::SubagentInputSnapshot {
                        id: "pending:1".into(),
                        prompt: "Pending text".into(),
                        mode: TurnMode::Build,
                        status: openide_harness::SubagentInputStatus::Pending,
                        revision: 7,
                    },
                    openide_harness::SubagentInputSnapshot {
                        id: "active".into(),
                        prompt: "Active text".into(),
                        mode: TurnMode::Build,
                        status: openide_harness::SubagentInputStatus::Active,
                        revision: 0,
                    },
                ],
            });
        for selected in ["s1", "child-1"] {
            state.selected_session = Some(selected.into());
            for width in [360.0, 900.0] {
                let mut tree = UiTree::new(related_subagent_cards(&state, width < 500.0).unwrap());
                tree.layout(Size::new(width, 600.0));
                let id = "subagent_input_skip:child-1:7:pending:1";
                assert!(tree.focus(id));
                let frame = tree.layout(Size::new(width, 600.0));
                let region = frame.regions.iter().find(|r| r.id == id).unwrap();
                assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width);
                assert!(!frame
                    .regions
                    .iter()
                    .any(|r| r.id == "subagent_input_skip:child-1:0:active"));
            }
        }
    }

    #[test]
    fn mailbox_dispatch_diagnostic_and_retry_remain_visible_without_worker() {
        let mut state = state();
        state
            .subagent_sessions
            .push(crate::view::SubagentSessionView {
                child_session_id: "child-1".into(),
                parent_session_id: "s1".into(),
                title: "Mailbox".into(),
                status: openide_harness::SubagentStatus::Completed,
                cancellation_requested: false,
                open_inputs: 1,
                inputs: vec![],
                depth: 1,
                final_response: None,
                dispatch_error: Some("Selecciona el modelo original del subagente".into()),
            });
        for selected in ["s1", "child-1"] {
            state.selected_session = Some(selected.into());
            for width in [360.0, 900.0] {
                let mut tree = UiTree::new(related_subagent_cards(&state, width < 500.0).unwrap());
                tree.layout(Size::new(width, 600.0));
                assert!(tree.focus("subagent_dispatch:child-1"));
                let frame = tree.layout(Size::new(width, 600.0));
                assert!(frame
                    .regions
                    .iter()
                    .any(|region| region.id == "subagent_dispatch:child-1"));
                assert!(visible_text(&frame).contains("modelo original"));
            }
        }
    }

    #[test]
    fn interrupted_child_offers_explicit_resume_action() {
        let mut state = state();
        state.selected_session = Some("child-1".into());
        state.recoverable_subagent = Some(SubagentRunView {
            child_session_id: "child-1".into(),
            parent_session_id: "parent-1".into(),
            title: "Revisar proyecto".into(),
        });
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 780.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "subagent_resume:child-1"));
        state.selected_session = Some("other".into());
        let other = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 780.0));
        assert!(!other
            .regions
            .iter()
            .any(|region| region.id == "subagent_resume:child-1"));
    }

    #[test]
    fn assistant_markdown_uses_reading_column() {
        let mut state = state();
        state.transcript.push(TranscriptItem::Assistant {
            content: "## Resultado\n\n- Compila\n- Pruebas pasan".into(),
            streaming: false,
        });
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        let visible = visible_text(&frame);
        let normalized = visible.split_whitespace().collect::<Vec<_>>().join(" ");
        for text in ["Resultado", "Compila", "Pruebas pasan"] {
            assert!(normalized.contains(text), "{visible}");
        }
    }

    #[test]
    fn web_citation_is_clickable_in_transcript() {
        let mut state = state();
        state.transcript.push(TranscriptItem::WebCitation {
            title: "Fuente primaria".into(),
            url: "https://example.com/source".into(),
        });
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "web_citation_open:0"));
    }

    #[test]
    fn successful_cli_diagnostics_stay_in_activity_instead_of_conversation() {
        let mut state = state();
        state.transcript = vec![
            TranscriptItem::User {
                content: "hola".into(),
                images: Vec::new(),
            },
            TranscriptItem::ProcessOutput {
                stream: OutputStream::Stderr,
                text: "WARN shell snapshot unavailable".into(),
            },
            TranscriptItem::Assistant {
                content: "¡Hola!".into(),
                streaming: false,
            },
        ];
        assert!(completed_process_diagnostic(&state.transcript, 1));
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        let visible = visible_text(&frame);
        assert!(visible.contains("¡Hola!"));
        assert!(!visible.contains("shell snapshot"));

        state.transcript[2] = TranscriptItem::Notice {
            message: "Codex CLI exited with code 1".into(),
            is_error: true,
        };
        assert!(!completed_process_diagnostic(&state.transcript, 1));
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        let visible = visible_text(&frame);
        assert!(visible.contains("shell snapshot"));
        assert!(visible.contains("DIAGNÓSTICOS DEL PROCESO"));
    }

    #[test]
    fn exploration_summary_names_one_file_and_counts_the_rest() {
        let call = |name: &str, path: &str| TranscriptItem::Tool {
            id: path.into(),
            name: name.into(),
            arguments: json!({"path": path, "query": "q"}),
            output: None,
            is_error: false,
            activity: ActivityStatus::Running,
        };
        let mut items = vec![call("read_file", "web/package.json")];
        assert_eq!(exploration_summary(&items), "package.json");
        items.push(call("search_text", "web"));
        items.push(call("find_files", "web"));
        assert_eq!(exploration_summary(&items), "package.json, 2 búsquedas");
        items.push(call("read_file", "README.md"));
        items.push(call("list_files", "src"));
        items.push(call("web_fetch", "https://example.com"));
        // An edit is not exploration and is never counted.
        items.push(call("write_file", "src/a.rs"));
        assert_eq!(
            exploration_summary(&items),
            "2 archivos, 2 búsquedas, 1 listado, 1 página"
        );
    }

    #[test]
    fn tool_density_groups_only_in_grouped_mode_and_keeps_individual_disclosures() {
        use crate::appearance_preferences::ToolCallDensity;
        let mut state = state();
        state.transcript = ["one", "two"]
            .into_iter()
            .map(|id| TranscriptItem::Tool {
                id: id.into(),
                name: "read_file".into(),
                arguments: json!({"path": format!("{id}.rs")}),
                output: Some(format!("DENSITY_RESULT_{id}")),
                is_error: false,
                activity: ActivityStatus::Completed,
            })
            .collect();
        for density in ToolCallDensity::ALL {
            state.appearance.tool_call_density = density;
            let frame = UiTree::new(timeline(&state, 900.0)).layout(Size::new(900.0, 1200.0));
            let text = visible_text(&frame);
            assert_eq!(
                text.contains("Exploró"),
                density == ToolCallDensity::Grouped
            );
            for call in ["one", "two"] {
                assert_eq!(
                    frame
                        .regions
                        .iter()
                        .any(|region| region.id == detail_action(&state, &format!("tool:{call}"))),
                    density != ToolCallDensity::Grouped
                );
                assert_eq!(
                    text.contains(&format!("DENSITY_RESULT_{call}")),
                    density == ToolCallDensity::Detailed
                );
            }
        }
    }

    #[test]
    fn detailed_tools_default_open_but_explicit_collapse_wins() {
        use crate::appearance_preferences::ToolCallDensity;
        let mut state = state();
        state.appearance.tool_call_density = ToolCallDensity::Detailed;
        let item = TranscriptItem::Tool {
            id: "opaque".into(),
            name: "mcp__example__inspect".into(),
            arguments: json!({"key":"value"}),
            output: Some("DENSITY_OPAQUE_RESULT".into()),
            is_error: false,
            activity: ActivityStatus::Completed,
        };
        assert!(conversation_detail_default_open(&state, "tool:opaque"));
        let frame = UiTree::new(tool_line(&item, &state, false)).layout(Size::new(900.0, 900.0));
        assert!(visible_text(&frame).contains("DENSITY_OPAQUE_RESULT"));
        state
            .conversation_details
            .set_expanded("tool:opaque", false);
        let frame = UiTree::new(tool_line(&item, &state, false)).layout(Size::new(900.0, 900.0));
        assert!(!visible_text(&frame).contains("DENSITY_OPAQUE_RESULT"));
    }

    #[test]
    fn tool_density_keeps_errors_and_permission_status_visible() {
        use crate::appearance_preferences::ToolCallDensity;
        let mut state = state();
        for density in ToolCallDensity::ALL {
            state.appearance.tool_call_density = density;
            for (activity, output, expected) in [
                (
                    ActivityStatus::Failed,
                    Some("DENSITY_FAILURE".into()),
                    "DENSITY_FAILURE",
                ),
                (ActivityStatus::WaitingPermission, None, "autorización"),
            ] {
                state.transcript = vec![TranscriptItem::Tool {
                    id: "attention".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":"missing.rs"}),
                    output,
                    is_error: activity == ActivityStatus::Failed,
                    activity,
                }];
                let frame = UiTree::new(timeline(&state, 900.0)).layout(Size::new(900.0, 900.0));
                assert!(
                    visible_text(&frame).contains(expected),
                    "{density:?} {activity:?}"
                );
            }
        }
    }

    #[test]
    fn identical_assistant_messages_keep_independent_code_viewports() {
        let mut state = state();
        state.transcript = vec![
            TranscriptItem::Assistant {
                content: format!("```rust\nlet value = \"{}\";\n```", "same_value".repeat(30)),
                streaming: false,
            };
            2
        ];
        let root = Element::column(state.transcript.iter().enumerate()
            .map(|(index, item)| message(item, index, 360.0, &state)).collect());
        let frame = UiTree::new(root).layout(Size::new(360.0, 1200.0));
        let ids: Vec<_> = frame.regions.iter().filter(|region| region.kind == HitKind::Scroll)
            .map(|region| region.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
    }
}
