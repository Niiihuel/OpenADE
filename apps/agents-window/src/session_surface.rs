//! Session surface composed from the persisted/live transcript projection.
//!
//! A turn is visually anchored by its user prompt. Consecutive tool calls are
//! shown as one work group, but remain individually selectable. No task count,
//! duration, or file change is inferred from text: only transcript facts appear.

use openide_core::{OutputStream, PlanStepStatus};
use openide_ui::{
    Button, ButtonVariant, Color, Component, ControlSize, Element, FontFamily, Icon,
    IconButton as UiIconButton, ScrollArea, TextArea,
};

use crate::{
    composer_ui::{build_composer, ComposerProps},
    rich_text, theme,
    transcript::{TranscriptImage, TranscriptItem},
    view::{
        sidebar_reopen_control, AbortedToolBatchView, PermissionView, SessionPlanView,
        SessionRecoveryView, SubagentPermissionView, SubagentRunView, ToolVerificationView,
        ViewState,
    },
};

const READING_WIDTH: f32 = 780.0;
const COMPACT_BREAKPOINT: f32 = 470.0;
const HEADER_PADDING: f32 = 16.0;
const HEADER_HEIGHT: f32 = 64.0;
const HEADER_FADE_HEIGHT: f32 = 42.0;
const HEADER_ACTION_SIZE: f32 = 32.0;
const HEADER_GAP: f32 = 8.0;

fn text(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .foreground(color)
        .font_family(FontFamily::Sans)
        .font_size(size)
}

fn mono(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .foreground(color)
        .font_family(FontFamily::Monospace)
        .font_size(size)
}

pub(crate) fn user_image(image: &TranscriptImage) -> Element {
    let preview = image.raster.as_ref().map_or_else(
        || {
            Element::row(vec![
                Element::icon(Icon::Paperclip).foreground(theme::FOREGROUND_SUBTLE),
                text("Vista previa no disponible", theme::FOREGROUND_SUBTLE, 12.0),
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
        text(&image.alt, theme::FOREGROUND_SUBTLE, 11.0),
    ])
    .gap(5.0)
}

fn action(
    id: impl Into<String>,
    icon: Icon,
    label: impl Into<String>,
    background: Color,
    foreground: Color,
) -> Element {
    UiIconButton::new(id, icon, theme::action_palette(background, foreground))
        .label(label)
        .variant(if background == theme::BRAND {
            ButtonVariant::Solid
        } else {
            ButtonVariant::Subtle
        })
        .size(ControlSize::Regular)
        .render()
        .font_size(12.0)
        .padding(5.0)
        .radius(8.0)
        .min_size(0.0, 32.0)
}

fn separator() -> Element {
    Element::spacer()
        .size(None, Some(1.0))
        .background(theme::BORDER)
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

fn session_backend_label(state: &ViewState) -> Option<&'static str> {
    let backend = state
        .selected_session
        .as_deref()
        .and_then(|id| state.sessions.iter().find(|session| session.id == id))
        .map_or(state.selected_backend.as_str(), |session| {
            session.backend.as_str()
        });
    match backend {
        "native" => Some("Nativo"),
        "codex" => Some("Codex CLI"),
        _ => None,
    }
}

fn framed_row(contents: Vec<Element>, background: Color, padding: f32, radius: f32) -> Element {
    Element::panel(Element::row(contents).gap(8.0))
        .padding(padding)
        .background(background)
        .border(theme::BORDER, 1.0)
        .radius(radius)
}

fn header(state: &ViewState, width: f32, side_visible: bool, side_available: bool) -> Element {
    let sidebar_hidden = state
        .left_presentation_width
        .map_or(state.sidebar_collapsed, |width| width <= 1.0);
    let recovery_decision_pending =
        state.pending_tool_verification.is_some() || state.pending_batch_discard.is_some();
    let show_side_toggle =
        !recovery_decision_pending && !side_visible && side_available && width >= 300.0;
    let show_settings = !recovery_decision_pending && width >= 220.0;
    let mut top = Vec::new();
    if sidebar_hidden {
        top.push(sidebar_reopen_control(theme::BACKGROUND));
    }
    top.push(
        Element::truncated_text(session_name(state))
            .foreground(theme::FOREGROUND)
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .flex(1.0)
            .size(None, Some(HEADER_ACTION_SIZE)),
    );
    if width >= COMPACT_BREAKPOINT && state.selected_session.is_some() {
        if let Some(backend) = session_backend_label(state) {
            top.push(
                mono(backend, theme::FOREGROUND_SUBTLE, 10.0)
                    .padding(6.0)
                    .background(theme::PANEL)
                    .radius(6.0)
                    .size(None, Some(26.0)),
            );
        }
    }
    if !recovery_decision_pending {
        top.push(
            action(
                "new_session_header",
                Icon::Add,
                "",
                theme::BACKGROUND,
                theme::FOREGROUND_SUBTLE,
            )
            .size(Some(32.0), Some(32.0)),
        );
    }
    if show_side_toggle {
        top.push(
            action(
                "toggle_side_pane_header",
                Icon::PanelRightOpen,
                "",
                theme::BACKGROUND,
                theme::FOREGROUND_SUBTLE,
            )
            .size(Some(32.0), Some(32.0)),
        );
    }
    if show_settings {
        top.push(
            action(
                "toggle_settings_header",
                Icon::Settings,
                "",
                theme::BACKGROUND,
                theme::FOREGROUND_SUBTLE,
            )
            .size(Some(HEADER_ACTION_SIZE), Some(HEADER_ACTION_SIZE)),
        );
    }
    Element::panel(Element::row(top).gap(HEADER_GAP))
        .padding(HEADER_PADDING)
        .size(None, Some(HEADER_HEIGHT))
}

fn turn_heading(number: usize, item: &TranscriptItem) -> Element {
    let (title, icon) = match item {
        TranscriptItem::User { .. } => ("TÚ", Icon::Chat),
        TranscriptItem::Assistant { streaming, .. } => (
            if *streaming {
                "AGENTE · ESCRIBIENDO"
            } else {
                "AGENTE"
            },
            Icon::Code,
        ),
        _ => ("ACTIVIDAD", Icon::Code),
    };
    Element::row(vec![
        Element::icon(icon).foreground(theme::ICON_PRIMARY),
        text(title, theme::FOREGROUND_SUBTLE, 10.0),
        Element::spacer().flex(1.0),
        if matches!(item, TranscriptItem::User { .. }) {
            mono(format!("{number:02}"), theme::FOREGROUND_SUBTLE, 10.0)
        } else {
            Element::spacer()
        },
    ])
    .gap(7.0)
    .size(None, Some(20.0))
}

fn message(
    item: &TranscriptItem,
    item_index: usize,
    turn_number: usize,
    reading_width: f32,
) -> Element {
    match item {
        TranscriptItem::User { content, images } => {
            let mut contents = vec![
                turn_heading(turn_number, item),
                text(content, theme::FOREGROUND, 14.0),
            ];
            for image in images {
                contents.push(user_image(image));
            }
            Element::panel(Element::column(contents).gap(9.0))
                .padding(16.0)
                .background(theme::CARD)
                .border(theme::BORDER, 1.0)
                .radius(12.0)
        }
        TranscriptItem::Assistant { content, .. } => Element::panel(
            Element::column(vec![
                turn_heading(turn_number, item),
                rich_text::render_markdown_at_width(content, (reading_width - 24.0).max(96.0)),
            ])
            .gap(9.0),
        )
        .padding(12.0)
        .background(theme::BACKGROUND),
        TranscriptItem::WebCitation { title, url } => Element::clickable(
            format!("web_citation_open:{item_index}"),
            Element::row(vec![
                Element::icon(Icon::Search).foreground(theme::ICON_PRIMARY),
                Element::column(vec![
                    Element::truncated_text(title)
                        .font_family(FontFamily::Sans)
                        .font_size(12.0)
                        .foreground(theme::FOREGROUND)
                        .size(None, Some(18.0)),
                    Element::truncated_text(url)
                        .font_family(FontFamily::Monospace)
                        .font_size(10.0)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(None, Some(16.0)),
                ])
                .gap(2.0)
                .flex(1.0),
            ])
            .gap(8.0)
            .padding(9.0),
        )
        .background(theme::PANEL)
        .hover_background(theme::HOVER)
        .border(theme::BORDER, 1.0)
        .radius(8.0),
        TranscriptItem::Notice { message, is_error } => {
            let ink = if *is_error {
                theme::DESTRUCTIVE
            } else {
                theme::FOREGROUND_SUBTLE
            };
            framed_row(
                vec![
                    Element::icon(if *is_error {
                        Icon::Close
                    } else {
                        Icon::History
                    })
                    .foreground(ink),
                    text(message, ink, 12.0),
                ],
                theme::PANEL,
                10.0,
                8.0,
            )
        }
        TranscriptItem::ProcessOutput {
            stream,
            text: output,
        } => {
            let (title, ink) = match stream {
                OutputStream::Stdout => ("SALIDA DEL PROCESO", theme::FOREGROUND_SUBTLE),
                OutputStream::Stderr => ("DIAGNÓSTICOS DEL PROCESO", theme::FOREGROUND_SUBTLE),
            };
            Element::panel(
                Element::column(vec![
                    text(title, ink, 10.0),
                    mono(ellipsis(output, 700), theme::FOREGROUND_SUBTLE, 11.0),
                ])
                .gap(7.0),
            )
            .padding(12.0)
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(9.0)
        }
        TranscriptItem::Tool { .. } => unreachable!("tools are rendered in work groups"),
    }
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

fn work_counts(items: &[TranscriptItem]) -> (usize, usize, usize) {
    let mut completed = 0;
    let mut running = 0;
    let mut failed = 0;
    for item in items {
        if let TranscriptItem::Tool {
            output, is_error, ..
        } = item
        {
            if output.is_none() {
                running += 1;
            } else if *is_error {
                failed += 1;
            } else {
                completed += 1;
            }
        }
    }
    (completed, running, failed)
}

fn tool_line(
    item: &TranscriptItem,
    state: &ViewState,
    side_visible: bool,
    compact: bool,
) -> Element {
    let TranscriptItem::Tool {
        id,
        name,
        arguments,
        output,
        is_error,
    } = item
    else {
        unreachable!()
    };
    let selected = state.selected_tool_id.as_deref() == Some(id.as_str());
    let active = state.running || state.background_run_selected;
    let (status, ink) = if output.is_none() && active {
        ("EN CURSO", theme::FOREGROUND_SUBTLE)
    } else if output.is_none() {
        ("INCIERTO", theme::DESTRUCTIVE)
    } else if *is_error {
        ("ERROR", theme::DESTRUCTIVE)
    } else {
        ("LISTO", theme::FOREGROUND_SUBTLE)
    };
    let mut rows = vec![Element::row(vec![
        action(
            format!("tool:{id}"),
            Icon::Code,
            ellipsis(
                if name.is_empty() { "Herramienta" } else { name },
                if compact { 18 } else { 35 },
            ),
            if selected {
                theme::SELECTED
            } else {
                theme::CARD
            },
            if *is_error {
                theme::DESTRUCTIVE
            } else {
                theme::FOREGROUND
            },
        )
        .size(None, Some(32.0))
        .flex(1.0),
        text(status, ink, 10.0),
    ])
    .gap(8.0)];
    if selected && !side_visible {
        let args = if arguments.is_null() {
            "Sin argumentos".to_owned()
        } else {
            serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string())
        };
        rows.push(
            Element::panel(mono(ellipsis(&args, 1_200), theme::FOREGROUND_SUBTLE, 11.0))
                .padding(8.0)
                .background(theme::PANEL)
                .radius(6.0),
        );
        if let Some(output) = output {
            rows.push(
                Element::panel(mono(ellipsis(output, 2_000), ink, 11.0))
                    .padding(8.0)
                    .background(theme::PANEL)
                    .radius(6.0),
            );
        }
    }
    Element::column(rows).gap(3.0)
}

fn work_group(
    items: &[TranscriptItem],
    state: &ViewState,
    side_visible: bool,
    compact: bool,
) -> Element {
    let (completed, pending, failed) = work_counts(items);
    let active = state.running || state.background_run_selected;
    let running = if active { pending } else { 0 };
    let uncertain = if active { 0 } else { pending };
    let count = |number: usize, singular: &str, plural: &str| {
        format!("{number} {}", if number == 1 { singular } else { plural })
    };
    let summary = if compact {
        let mut summary = format!("{completed}/{} listas", completed + pending + failed);
        if failed > 0 {
            summary.push_str(&format!(" · {failed} error"));
        }
        if uncertain > 0 {
            summary.push_str(&format!(" · {}", count(uncertain, "incierta", "inciertas")));
        }
        summary
    } else {
        let mut parts = vec![count(completed, "lista", "listas")];
        if running > 0 {
            parts.push(format!("{running} en curso"));
        }
        if uncertain > 0 {
            parts.push(count(uncertain, "incierta", "inciertas"));
        }
        if failed > 0 {
            parts.push(format!("{failed} con error"));
        }
        parts.join(" · ")
    };
    let mut rows = vec![Element::row(vec![
        text("TRABAJO DEL AGENTE", theme::FOREGROUND_SUBTLE, 10.0),
        Element::spacer().flex(1.0),
        text(summary, theme::FOREGROUND_SUBTLE, 10.0),
    ])];
    rows.push(separator());
    for item in items {
        rows.push(tool_line(item, state, side_visible, compact));
    }
    Element::panel(Element::column(rows).gap(7.0))
        .padding(12.0)
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(10.0)
}

fn timeline(state: &ViewState, width: f32, side_visible: bool) -> Element {
    let reading_width = (width
        - if width < COMPACT_BREAKPOINT {
            22.0
        } else {
            56.0
        })
    .clamp(120.0, READING_WIDTH);
    // Keep first content below the scrim at scroll position zero. When the
    // transcript scrolls, earlier rows pass beneath the fade like Codex.
    let mut rows = vec![Element::spacer().size(None, Some(22.0))];
    let mut index = 0;
    let mut turn_number = 0;
    if state.transcript.is_empty() {
        rows.push(text(
            "Esta tarea aún no tiene actividad",
            theme::FOREGROUND,
            17.0,
        ));
        rows.push(text(
            "Describe qué quieres hacer en el compositor inferior.",
            theme::FOREGROUND_SUBTLE,
            12.0,
        ));
    }
    while index < state.transcript.len() {
        let item = &state.transcript[index];
        if completed_process_diagnostic(&state.transcript, index) {
            index += 1;
            continue;
        }
        if matches!(item, TranscriptItem::Tool { .. }) {
            let start = index;
            while index < state.transcript.len()
                && matches!(state.transcript[index], TranscriptItem::Tool { .. })
            {
                index += 1;
            }
            rows.push(work_group(
                &state.transcript[start..index],
                state,
                side_visible,
                width < COMPACT_BREAKPOINT,
            ));
            continue;
        }
        if matches!(item, TranscriptItem::User { .. }) {
            turn_number += 1;
            if turn_number > 1 {
                rows.push(Element::spacer().size(None, Some(8.0)));
            }
        }
        rows.push(message(item, index, turn_number, reading_width));
        index += 1;
    }
    let content = Element::column(rows)
        .gap(12.0)
        .padding(if width < COMPACT_BREAKPOINT {
            10.0
        } else {
            24.0
        })
        .size(Some(reading_width), None);
    ScrollArea::new(
        "transcript",
        Element::row(vec![
            Element::spacer().flex(1.0),
            content,
            Element::spacer().flex(1.0),
        ]),
    )
    .tint(theme::FOREGROUND_SUBTLE)
    .render()
    .flex(1.0)
    .background(theme::BACKGROUND)
    .top_fade(theme::BACKGROUND, HEADER_FADE_HEIGHT)
}

fn permission_card(permission: &PermissionView, compact: bool) -> Element {
    let argument_height = if compact { 54.0 } else { 76.0 };
    Element::panel(
        Element::column(vec![
            text("PERMISO REQUERIDO", theme::FOREGROUND_SUBTLE, 10.0),
            text(
                format!("El agente quiere usar {}", permission.tool_name),
                theme::FOREGROUND,
                13.0,
            ),
            ScrollArea::new(
                format!("permission_args:{}", permission.request_id),
                mono(&permission.arguments, theme::FOREGROUND_SUBTLE, 11.0).padding(7.0),
            )
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .size(None, Some(argument_height))
            .background(theme::PANEL)
            .radius(6.0),
            Element::row(vec![
                Element::button(
                    format!("permission_deny:{}", permission.request_id),
                    "Denegar",
                )
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::FOREGROUND)
                .background(theme::HOVER)
                .radius(7.0)
                .size(Some(90.0), Some(30.0)),
                Element::button(
                    format!("permission_allow:{}", permission.request_id),
                    "Permitir una vez",
                )
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::PRIMARY_FOREGROUND)
                .background(theme::BRAND)
                .radius(7.0)
                .size(Some(136.0), Some(30.0)),
            ])
            .gap(8.0),
            Element::button(
                format!("permission_allow_session:{}", permission.request_id),
                "Permitir en esta sesión",
            )
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::FOREGROUND)
            .background(theme::HOVER)
            .radius(7.0)
            .size(Some(196.0), Some(30.0)),
        ])
        .gap(7.0),
    )
    .padding(12.0)
    .background(theme::CARD)
    .border(theme::BORDER, 1.0)
    .radius(10.0)
}

fn subagent_permission_card(permission: &SubagentPermissionView, compact: bool) -> Element {
    Element::panel(
        Element::column(vec![
            text("PERMISO DE SUBAGENTE", theme::FOREGROUND_SUBTLE, 10.0),
            text(
                format!("El subagente quiere usar {}", permission.tool_name),
                theme::FOREGROUND,
                13.0,
            ),
            mono(
                format!("Sesión hija: {}", permission.child_session_id),
                theme::FOREGROUND_SUBTLE,
                11.0,
            ),
            ScrollArea::new(
                format!(
                    "subagent_permission_args:{}:{}",
                    permission.child_session_id, permission.request_id
                ),
                mono(&permission.arguments, theme::FOREGROUND_SUBTLE, 11.0).padding(7.0),
            )
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .size(None, Some(if compact { 54.0 } else { 76.0 }))
            .background(theme::PANEL)
            .radius(6.0),
            Element::row(vec![
                Element::button(
                    format!(
                        "subagent_permission_deny:{}:{}",
                        permission.child_session_id, permission.request_id
                    ),
                    "Denegar",
                )
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::FOREGROUND)
                .background(theme::HOVER)
                .radius(7.0)
                .size(Some(90.0), Some(30.0)),
                Element::button(
                    format!(
                        "subagent_permission_allow:{}:{}",
                        permission.child_session_id, permission.request_id
                    ),
                    "Permitir una vez",
                )
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::PRIMARY_FOREGROUND)
                .background(theme::BRAND)
                .radius(7.0)
                .size(Some(136.0), Some(30.0)),
            ])
            .gap(8.0),
            Element::button(
                format!(
                    "subagent_permission_allow_session:{}:{}",
                    permission.child_session_id, permission.request_id
                ),
                "Permitir en esta sesión",
            )
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::FOREGROUND)
            .background(theme::HOVER)
            .radius(7.0)
            .size(Some(196.0), Some(30.0)),
        ])
        .gap(7.0),
    )
    .padding(12.0)
    .background(theme::CARD)
    .border(theme::BORDER, 1.0)
    .radius(10.0)
}

fn active_subagent_card(run: &SubagentRunView, selected_session: &str) -> Element {
    let relation = if selected_session == run.child_session_id {
        "ESTA SESIÓN ES UN SUBAGENTE"
    } else {
        "SUBAGENTE ACTIVO"
    };
    Element::panel(
        Element::column(vec![
            text(relation, theme::FOREGROUND_SUBTLE, 10.0),
            text(&run.title, theme::FOREGROUND, 13.0),
            mono(
                format!("Sesión hija: {}", run.child_session_id),
                theme::FOREGROUND_SUBTLE,
                11.0,
            ),
            Element::button(
                format!("subagent_cancel:{}", run.child_session_id),
                "Cancelar subagente",
            )
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::DESTRUCTIVE)
            .background(theme::HOVER)
            .radius(7.0)
            .size(Some(170.0), Some(30.0)),
        ])
        .gap(7.0),
    )
    .padding(12.0)
    .background(theme::CARD)
    .border(theme::BORDER, 1.0)
    .radius(10.0)
}

fn recoverable_subagent_card(run: &SubagentRunView) -> Element {
    Element::panel(
        Element::column(vec![
            text("SUBAGENTE INTERRUMPIDO", theme::FOREGROUND_SUBTLE, 10.0),
            text(&run.title, theme::FOREGROUND, 13.0),
            text(
                "El journal conserva el turno. Reanudar verificará el estado antes de pedir al modelo o ejecutar una herramienta.",
                theme::FOREGROUND_SUBTLE,
                12.0,
            ),
            Element::button(
                format!("subagent_resume:{}", run.child_session_id),
                "Reanudar subagente",
            )
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::PRIMARY_FOREGROUND)
            .background(theme::BRAND)
            .radius(7.0)
            .size(Some(176.0), Some(30.0)),
        ])
        .gap(7.0),
    )
    .padding(12.0)
    .background(theme::CARD)
    .border(theme::BORDER, 1.0)
    .radius(10.0)
}

fn native_recovery_card(session_id: &str, recovery: &SessionRecoveryView) -> Element {
    let message = match recovery {
        SessionRecoveryView::Resume => "El turno quedó interrumpido y puede reanudarse.",
        SessionRecoveryView::Permission => {
            "Hay un permiso pendiente. Reanudar pedirá la decisión antes de ejecutar."
        }
        SessionRecoveryView::VerifyTool => {
            "La herramienta pudo haber producido cambios. Verifica su efecto antes de continuar."
        }
        SessionRecoveryView::ReviewBatch => {
            "Hay llamadas pendientes de un lote abortado. Revísalas antes de continuar."
        }
        SessionRecoveryView::InvalidJournal(error) => error,
    };
    let mut content = vec![
        text("RECUPERACIÓN DEL TURNO", theme::FOREGROUND_SUBTLE, 10.0),
        text(recovery.label(), theme::FOREGROUND, 13.0),
        text(message, theme::FOREGROUND_SUBTLE, 12.0),
    ];
    if recovery.can_open() {
        content.push(
            Element::button(format!("session_recover:{session_id}"), recovery.label())
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::PRIMARY_FOREGROUND)
                .background(theme::BRAND)
                .radius(7.0)
                .size(Some(196.0), Some(30.0)),
        );
    }
    Element::panel(Element::column(content).gap(7.0))
        .padding(12.0)
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(10.0)
}

fn session_plan_card(session_id: &str, plan: &SessionPlanView) -> Element {
    let steps = plan
        .snapshot
        .plan
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let marker = match step.status {
                PlanStepStatus::Pending => "○",
                PlanStepStatus::InProgress => "◉",
                PlanStepStatus::Completed => "✓",
            };
            text(
                format!("{marker} {}. {}", index + 1, step.step),
                if step.status == PlanStepStatus::InProgress {
                    theme::FOREGROUND
                } else {
                    theme::FOREGROUND_SUBTLE
                },
                12.0,
            )
        })
        .collect::<Vec<_>>();
    let list_height = (plan.snapshot.plan.len() as f32 * 28.0).min(220.0);
    let mut content = vec![
        text("PLAN", theme::FOREGROUND_SUBTLE, 10.0),
        ScrollArea::new(
            format!("session_plan_steps:{session_id}"),
            Element::column(steps).gap(5.0),
        )
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .size(None, Some(list_height)),
    ];
    if plan.can_build {
        content.push(
            Element::button(
                format!("session_build_plan:{session_id}"),
                "Construir este plan",
            )
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::PRIMARY_FOREGROUND)
            .background(theme::BRAND)
            .radius(7.0)
            .size(Some(180.0), Some(30.0)),
        );
    }
    Element::panel(Element::column(content).gap(7.0))
        .padding(12.0)
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(10.0)
}

/// Shared by the session layout and the workspace fallback so permissions
/// remain actionable while either related session is selected.
pub(crate) fn related_subagent_cards(state: &ViewState, compact: bool) -> Option<Element> {
    let selected = state.selected_session.as_deref()?;
    let related = |child: &str, parent: &str| selected == child || selected == parent;
    let pending: Vec<_> = state
        .pending_subagent_permissions
        .iter()
        .filter(|permission| related(&permission.child_session_id, &permission.parent_session_id))
        .collect();
    let active: Vec<_> = state
        .active_subagents
        .iter()
        .filter(|run| related(&run.child_session_id, &run.parent_session_id))
        .collect();
    let recoverable = state
        .recoverable_subagent
        .as_ref()
        .filter(|run| selected == run.child_session_id);
    if pending.is_empty() && active.is_empty() && recoverable.is_none() {
        return None;
    }
    let height = (16.0
        + pending.len() as f32 * if compact { 178.0 } else { 200.0 }
        + active.len() as f32 * 120.0
        + recoverable.map_or(0.0, |_| 150.0)
        + (pending.len() + active.len() + usize::from(recoverable.is_some()) - 1) as f32 * 8.0)
        .min(if compact { 330.0 } else { 350.0 });
    let mut cards: Vec<Element> = pending
        .into_iter()
        .map(|permission| subagent_permission_card(permission, compact))
        .collect();
    cards.extend(
        active
            .into_iter()
            .map(|run| active_subagent_card(run, selected)),
    );
    if let Some(run) = recoverable {
        cards.push(recoverable_subagent_card(run));
    }
    Some(
        ScrollArea::new(
            "related_subagents",
            Element::column(cards).gap(8.0).padding(8.0),
        )
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .size(None, Some(height))
        .background(theme::BACKGROUND),
    )
}

fn tool_verification_card(
    verification: &ToolVerificationView,
    output: &str,
    is_error: bool,
    compact: bool,
) -> Element {
    let argument_height = if compact { 66.0 } else { 96.0 };
    let mut details = vec![
        Element::row(vec![
            Element::icon(Icon::Stop)
                .foreground(theme::DESTRUCTIVE)
                .size(Some(18.0), Some(18.0)),
            text("RESULTADO SIN VERIFICAR", theme::FOREGROUND, 13.0),
        ])
        .gap(8.0),
        text(
            "La herramienta pudo haber tenido un efecto antes de la interrupción. Comprueba el resultado fuera de OpenIDE antes de continuar.",
            theme::FOREGROUND_SUBTLE,
            12.0,
        ),
        separator(),
        text(format!("Herramienta: {}", verification.name), theme::FOREGROUND, 14.0),
        mono(format!("Llamada: {}", verification.call_id), theme::FOREGROUND_SUBTLE, 11.0),
        text("Argumentos originales", theme::FOREGROUND_SUBTLE, 11.0),
        ScrollArea::new(
            format!("tool_verification_args:{}", verification.call_id),
            mono(&verification.arguments, theme::FOREGROUND, 11.0).padding(8.0),
        )
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .size(None, Some(argument_height))
        .background(theme::PANEL)
        .radius(7.0),
    ];
    if let Some(error) = &verification.error {
        details.push(text("Error del ejecutor", theme::DESTRUCTIVE, 11.0));
        details.push(mono(error, theme::FOREGROUND, 11.0));
    }
    details.extend([
        separator(),
        text("Resultado comprobado", theme::FOREGROUND, 13.0),
        text(
            "Escribe lo que verificaste manualmente. Esta llamada no se repetirá al reanudar.",
            theme::FOREGROUND_SUBTLE,
            12.0,
        ),
        TextArea::new(
            "tool_verification_output",
            output,
            "Describe el resultado que comprobaste…",
            theme::component_palette(),
        )
        .height(if compact { 84.0 } else { 108.0 })
        .render(),
    ]);

    let classification = text(
        format!(
            "Clasificación elegida: {} (pendiente de confirmar)",
            if is_error { "Error" } else { "Éxito" }
        ),
        if is_error {
            theme::DESTRUCTIVE
        } else {
            theme::FOREGROUND
        },
        12.0,
    );
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
    .size(ControlSize::Regular)
    .render();
    details.push(if compact {
        Element::column(vec![classification, toggle]).gap(9.0)
    } else {
        Element::row(vec![classification.flex(1.0), toggle]).gap(12.0)
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
    .size(ControlSize::Regular)
    .enabled(!output.trim().is_empty())
    .render();
    let cancel = Button::new(
        "tool_verification_cancel",
        "Dejar pendiente",
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Regular)
    .render();
    details.push(if compact {
        Element::column(vec![cancel, confirm]).gap(8.0)
    } else {
        Element::row(vec![cancel, confirm]).gap(10.0)
    });
    Element::panel(Element::column(details).gap(10.0))
        .padding(if compact { 14.0 } else { 20.0 })
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(12.0)
}

fn aborted_batch_card(batch: &AbortedToolBatchView, compact: bool) -> Element {
    let count = batch.remaining_calls;
    let mut details = vec![
        Element::row(vec![
            Element::icon(Icon::Stop)
                .foreground(theme::DESTRUCTIVE)
                .size(Some(18.0), Some(18.0)),
            text("LOTE DE HERRAMIENTAS INTERRUMPIDO", theme::FOREGROUND, 13.0),
        ])
        .gap(8.0),
        text(
            format!(
                "El turno terminó con {count} {} sin ejecutar. Descartarlas permite iniciar otro turno. No revierte las {} llamadas ya completadas.",
                if count == 1 { "llamada pendiente" } else { "llamadas pendientes" },
                batch.completed_calls,
            ),
            theme::FOREGROUND_SUBTLE,
            12.0,
        ),
        separator(),
        text(
            format!("Próxima herramienta pendiente: {}", batch.next_tool_name),
            theme::FOREGROUND,
            14.0,
        ),
        mono(
            format!("Llamada: {}", batch.next_call_id),
            theme::FOREGROUND_SUBTLE,
            11.0,
        ),
        text("Argumentos originales", theme::FOREGROUND_SUBTLE, 11.0),
        ScrollArea::new(
            format!("aborted_batch_args:{}", batch.next_call_id),
            mono(&batch.next_arguments, theme::FOREGROUND, 11.0).padding(8.0),
        )
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .size(None, Some(if compact { 66.0 } else { 96.0 }))
        .background(theme::PANEL)
        .radius(7.0),
        separator(),
    ];
    let discard = Button::new(
        "aborted_batch_discard",
        "Descartar llamadas pendientes",
        theme::action_palette(theme::DESTRUCTIVE, theme::PRIMARY_FOREGROUND),
    )
    .variant(ButtonVariant::Raised)
    .size(ControlSize::Regular)
    .render();
    let defer = Button::new(
        "aborted_batch_defer",
        "Dejar pendiente",
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Regular)
    .render();
    details.push(if compact {
        Element::column(vec![defer, discard]).gap(8.0)
    } else {
        Element::row(vec![defer, discard]).gap(10.0)
    });
    Element::panel(Element::column(details).gap(10.0))
        .padding(if compact { 14.0 } else { 20.0 })
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(12.0)
}

fn composer(state: &ViewState, width: f32) -> Element {
    let compact = width < COMPACT_BREAKPOINT;
    let card_width = (width - if compact { 20.0 } else { 32.0 }).clamp(0.0, READING_WIDTH);
    let shell = build_composer(&ComposerProps::from_state(
        state,
        card_width,
        false,
        None,
        "Pide al agente que trabaje en este proyecto…",
        Some(&state.status),
    ));
    Element::row(vec![
        Element::spacer().flex(1.0),
        shell,
        Element::spacer().flex(1.0),
    ])
    .padding(10.0)
}

/// Renders an active session without representing unprojected harness state.
pub fn build_session_surface(
    state: &ViewState,
    center_width: f32,
    side_visible: bool,
    side_available: bool,
) -> Element {
    let compact = center_width < COMPACT_BREAKPOINT;
    if let Some(batch) = &state.pending_batch_discard {
        let card_width =
            (center_width - if compact { 20.0 } else { 48.0 }).clamp(0.0, READING_WIDTH);
        let recovery = Element::column(vec![
            Element::row(vec![
                Element::spacer().flex(1.0),
                aborted_batch_card(batch, compact).size(Some(card_width), None),
                Element::spacer().flex(1.0),
            ]),
            Element::spacer().size(None, Some(64.0)),
        ])
        .padding(if compact { 10.0 } else { 24.0 });
        return Element::column(vec![
            header(state, center_width, side_visible, side_available),
            ScrollArea::new("aborted_batch", recovery)
                .tint(theme::FOREGROUND_SUBTLE)
                .render()
                .flex(1.0)
                .background(theme::BACKGROUND),
        ])
        .flex(1.0);
    }
    if let Some(verification) = &state.pending_tool_verification {
        let card_width =
            (center_width - if compact { 20.0 } else { 48.0 }).clamp(0.0, READING_WIDTH);
        let recovery = Element::column(vec![
            Element::row(vec![
                Element::spacer().flex(1.0),
                tool_verification_card(
                    verification,
                    &state.tool_verification_output,
                    state.tool_verification_is_error,
                    compact,
                )
                .size(Some(card_width), None),
                Element::spacer().flex(1.0),
            ]),
            // Keep the final action above the scroll viewport's bottom edge.
            Element::spacer().size(None, Some(64.0)),
        ])
        .padding(if compact { 10.0 } else { 24.0 });
        return Element::column(vec![
            header(state, center_width, side_visible, side_available),
            ScrollArea::new("tool_verification", recovery)
                .tint(theme::FOREGROUND_SUBTLE)
                .render()
                .flex(1.0)
                .background(theme::BACKGROUND),
        ])
        .flex(1.0);
    }
    let mut children = vec![
        header(state, center_width, side_visible, side_available),
        timeline(state, center_width, side_visible),
    ];
    if let Some((session_id, plan)) = state
        .selected_session
        .as_deref()
        .and_then(|id| state.session_plans.get(id).map(|plan| (id, plan)))
    {
        children.push(session_plan_card(session_id, plan).padding(8.0));
    }
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
            if !child_card_already_visible {
                children.push(native_recovery_card(session_id, recovery).padding(8.0));
            }
        }
    }
    if state.show_jump_to_bottom {
        children.push(
            Element::row(vec![
                Element::spacer().flex(1.0),
                action(
                    "jump_to_bottom",
                    Icon::ArrowDown,
                    "Volver al final",
                    theme::CARD,
                    theme::FOREGROUND,
                )
                .border(theme::BORDER, 1.0)
                .size(Some(146.0), Some(30.0)),
            ])
            .padding(8.0)
            .size(None, Some(40.0)),
        );
    }
    for permission in state.pending_permissions.iter().take(3) {
        children.push(permission_card(permission, compact).padding(8.0));
    }
    if let Some(cards) = related_subagent_cards(state, compact) {
        children.push(cards);
    }
    children.push(composer(state, center_width));
    Element::column(children).flex(1.0)
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
    use openide_ui::{DrawCommand, Point, Size, UiEvent, UiTree};
    use serde_json::{json, Value};
    use unicode_segmentation::UnicodeSegmentation;

    fn state() -> ViewState {
        ViewState {
            workspace: "C:/repo/openIDE".into(),
            workspace_draft: "C:/repo/openIDE".into(),
            project_branch: None,
            prompt: "Siguiente paso".into(),
            composer_attachments: Vec::new(),
            composer_add_menu_open: false,
            composer_listening: false,
            composer_goal: None,
            composer_goal_editor_open: false,
            composer_goal_draft: String::new(),
            composer_project_menu_open: false,
            composer_project_options: Vec::new(),
            endpoint: String::new(),
            model: "gpt-4.1".into(),
            native_provider_id: "custom".into(),
            hosted_web_search_mode: None,
            catalog: None,
            catalog_status: String::new(),
            catalog_refreshing: false,
            catalog_browse_provider: "openai".into(),
            catalog_provider_query: String::new(),
            catalog_model_query: String::new(),
            backend_options: vec![("native".into(), "Nativo".into())],
            selected_backend: "native".into(),
            backend_menu_open: false,
            composer_model: "gpt-4.1".into(),
            composer_model_options: vec![("gpt-4.1".into(), "gpt-4.1".into())],
            composer_routes: Vec::new(),
            composer_provider_status: Default::default(),
            composer_model_menu_open: false,
            composer_model_list_open: false,
            composer_model_query: String::new(),
            composer_advanced_open: false,
            composer_effort: "default".into(),
            composer_effort_options: vec![],
            composer_effort_menu_open: false,
            composer_effort_preview: None,
            composer_effort_hovered: false,
            composer_context_percent: None,
            turn_mode: openide_core::TurnMode::Build,
            sessions: vec![],
            session_plans: Default::default(),
            session_recovery: Default::default(),
            sessions_status: None,
            selected_session: Some("s1".into()),
            selected_session_title: Some("Revisar la aplicación".into()),
            file_rows: vec![],
            files_status: String::new(),
            file_root_can_load_more: false,
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
            running: false,
            background_run_selected: false,
            status: "Listo".into(),
            settings_notice: None,
            settings_notice_error: false,
            native_key_available: false,
            workspace_writes_enabled: false,
            native_tool_permissions: openide_harness::ToolPermissions::default(),
            native_command_isolation: if cfg!(windows) {
                openide_harness::CommandIsolation::WorkspaceWriteOffline
            } else {
                openide_harness::CommandIsolation::HostUnrestricted
            },
            session_approval_grants: Vec::new(),
            native_commands: Vec::new(),
            native_commands_error: None,
            pending_native_command_termination: None,
            native_command_terminating: None,
            native_exec_rules: Default::default(),
            native_skills: Default::default(),
            native_hooks: Default::default(),
            settings_reduced_motion: false,
            settings_reopen_last_workspace: false,
            settings_terminal_shell: crate::settings_preferences::TerminalShell::System,
            settings_terminal_shell_draft: String::new(),
            mcp_servers: Vec::new(),
            mcp_selected_id: None,
            mcp_editor_open: false,
            pending_mcp_delete_id: None,
            mcp_draft_id: String::new(),
            mcp_draft_http: false,
            mcp_draft_url: String::new(),
            mcp_draft_http_bearer_token_env_name: String::new(),
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
            side_tab: SideTab::Context,
            activity_page: 0,
            review_page: 0,
            dock_tabs: vec![],
            dock_recently_closed: vec![],
            dock_overview_query: String::new(),
            show_dock_overview: false,
            show_dock_add_menu: false,
            file_preview: None,
            git_review: None,
            review_detail: None,
            terminal_snapshot: None,
            terminal_input: String::new(),
            terminal_error: None,
            pending_permissions: vec![],
            pending_subagent_permissions: vec![],
            active_subagents: vec![],
            recoverable_subagent: None,
            pending_tool_verification: None,
            pending_batch_discard: None,
            tool_verification_output: String::new(),
            tool_verification_is_error: false,
            selected_tool_id: None,
            show_jump_to_bottom: false,
            github_account: crate::github_account::GithubAccountStatus::Disconnected,
            github_account_menu_open: false,
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
            .any(|region| region.id == "new_session_header"));
    }

    #[test]
    fn uncertain_tool_result_requires_manual_text_before_confirmation() {
        let mut state = state();
        state.pending_tool_verification = Some(ToolVerificationView {
            session_id: "session-1".into(),
            call_id: "call-42".into(),
            name: "write_file".into(),
            arguments: "{\"path\":\"notes.txt\"}".into(),
            error: Some("Se perdió la respuesta del ejecutor".into()),
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
            .any(|region| region.id == "new_session_header"));
    }

    fn visible_text(frame: &openide_ui::Frame) -> String {
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
    fn session_header_labels_the_actual_backend_without_crowding_compact_layout() {
        let mut state = state();
        state.selected_backend = "codex".into();
        let wide =
            UiTree::new(header(&state, 520.0, false, true)).layout(Size::new(520.0, HEADER_HEIGHT));
        assert!(visible_text(&wide).contains("Codex CLI"));
        let compact =
            UiTree::new(header(&state, 320.0, false, true)).layout(Size::new(320.0, HEADER_HEIGHT));
        assert!(!visible_text(&compact).contains("Codex CLI"));
        state.selected_backend = "native".into();
        let native =
            UiTree::new(header(&state, 520.0, false, true)).layout(Size::new(520.0, HEADER_HEIGHT));
        assert!(visible_text(&native).contains("Nativo"));
    }

    #[test]
    fn session_header_truncates_by_painted_width_and_preserves_action_slots() {
        let mut state = state();
        state.selected_session_title = Some("W".repeat(24));
        let mut tree = UiTree::new(header(&state, 300.0, false, true));
        tree.set_text_measurer(|value, _, _| {
            value
                .chars()
                .map(|character| match character {
                    'W' => 14.0,
                    'i' => 2.0,
                    '…' => 4.0,
                    _ => 6.0,
                })
                .sum()
        });
        let wide = tree.layout(Size::new(300.0, HEADER_HEIGHT));
        let (wide_title, wide_x) = wide
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text.starts_with('W') => {
                    Some((text.as_str(), position.x))
                }
                _ => None,
            })
            .unwrap();
        assert!(wide_title.ends_with('…'), "{wide_title}");
        let new_task = wide
            .regions
            .iter()
            .find(|region| region.id == "new_session_header")
            .unwrap();
        let painted_width: f32 = wide_title
            .chars()
            .map(|character| if character == 'W' { 14.0 } else { 4.0 })
            .sum();
        assert!(wide_x + painted_width + HEADER_GAP <= new_task.rect.x);

        state.selected_session_title = Some("i".repeat(24));
        tree.set_root(header(&state, 300.0, false, true));
        let narrow = tree.layout(Size::new(300.0, HEADER_HEIGHT));
        assert!(visible_text(&narrow).contains(&"i".repeat(24)));
        let new_task_after = narrow
            .regions
            .iter()
            .find(|region| region.id == "new_session_header")
            .unwrap();
        assert_eq!(new_task.rect, new_task_after.rect);
    }

    #[test]
    fn session_header_keeps_joined_emoji_intact_when_title_is_clipped() {
        let mut state = state();
        state.selected_session_title = Some("👩‍💻".repeat(12));
        let mut tree = UiTree::new(header(&state, 300.0, false, true));
        tree.set_text_measurer(|value, _, _| {
            value
                .graphemes(true)
                .map(|grapheme| match grapheme {
                    "👩‍💻" => 20.0,
                    "…" => 4.0,
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
        let visible = title.strip_suffix('…').expect("clipped title");
        let graphemes = visible.graphemes(true).collect::<Vec<_>>();
        assert!(graphemes.len() < 12);
        assert!(graphemes.iter().all(|grapheme| *grapheme == "👩‍💻"));
    }

    #[test]
    fn session_header_actions_stay_in_fixed_bounds_at_responsive_widths() {
        for width in [220.0, 260.0, 300.0, 470.0, 800.0] {
            let frame = UiTree::new(header(&state(), width, false, true))
                .layout(Size::new(width, HEADER_HEIGHT));
            let mut actions = frame
                .regions
                .iter()
                .filter(|region| {
                    [
                        "new_session_header",
                        "toggle_side_pane_header",
                        "toggle_settings_header",
                    ]
                    .contains(&region.id.as_str())
                })
                .collect::<Vec<_>>();
            actions.sort_by(|left, right| left.rect.x.total_cmp(&right.rect.x));
            assert_eq!(actions.len(), if width >= 300.0 { 3 } else { 2 });
            for action in &actions {
                assert_eq!(action.rect.width, HEADER_ACTION_SIZE);
                assert_eq!(action.rect.height, HEADER_ACTION_SIZE);
                assert!(action.rect.x >= HEADER_PADDING);
                assert!(action.rect.x + action.rect.width <= width - HEADER_PADDING);
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
        assert!(labels.contains("Revisar la aplicación"));
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
    fn transcript_fades_under_header_without_covering_controls() {
        let mut state = state();
        state.transcript.push(TranscriptItem::User {
            content: "Primer mensaje".into(),
            images: Vec::new(),
        });
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        let fade_index = frame
            .commands
            .iter()
            .position(|command| matches!(command, DrawCommand::FillVerticalGradient { .. }))
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
        let new_task = frame
            .regions
            .iter()
            .find(|region| region.id == "new_session_header")
            .unwrap();
        assert_eq!(transcript.rect.y, HEADER_HEIGHT);
        assert!(new_task.rect.y + new_task.rect.height <= transcript.rect.y);
        assert!(frame.commands[..fade_index].iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text.contains("Primer mensaje")
        )));
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
            "new_session_header",
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
                    } if *color == openide_ui::Color::rgb(38, 38, 38) && *radius == 26.0 => {
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
                    if *rect == card && *radius == 26.0
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
                } if *color == openide_ui::Color::rgb(38, 38, 38) && *radius == 26.0 => Some(*rect),
                _ => None,
            });
            let card = card.expect("rounded composer card");
            let submit = frame
                .regions
                .iter()
                .find(|region| region.id == "run_toggle")
                .expect("stop action");
            assert_eq!(submit.rect.width, 36.0);
            assert!(submit.rect.x + submit.rect.width <= card.x + card.width);
            assert!(visible_text(&frame).contains("Proyecto"));
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
            },
            TranscriptItem::Tool {
                id: "b".into(),
                name: "search_files".into(),
                arguments: json!({"query": "main"}),
                output: None,
                is_error: false,
            },
        ];
        state.selected_tool_id = Some("a".into());
        let frame = UiTree::new(build_session_surface(&state, 520.0, false, true))
            .layout(Size::new(520.0, 700.0));
        let visible = visible_text(&frame);
        assert!(visible.contains("1 lista · 1 incierta"), "{visible}");
        assert!(visible.contains("src/main.rs"), "{visible}");
        assert!(visible.contains("fn main() {}"), "{visible}");
        assert!(frame.regions.iter().any(|region| region.id == "tool:a"));
        assert!(frame.regions.iter().any(|region| region.id == "tool:b"));
    }

    #[test]
    fn pending_permission_keeps_decision_actions() {
        let mut state = state();
        state.pending_permissions.push(PermissionView {
            request_id: "request-1".into(),
            tool_name: "write_file".into(),
            arguments: "{\"path\":\"src/main.rs\"}".into(),
        });
        let frame = UiTree::new(build_session_surface(&state, 520.0, true, true))
            .layout(Size::new(520.0, 780.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "permission_allow:request-1"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "permission_allow_session:request-1"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "permission_deny:request-1"));
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
            for id in [
                "subagent_permission_allow:child-1:request-1",
                "subagent_permission_allow_session:child-1:request-1",
                "subagent_permission_deny:child-1:request-1",
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
    fn tool_counts_separate_failed_and_pending() {
        let tools = vec![
            TranscriptItem::Tool {
                id: "one".into(),
                name: "read_file".into(),
                arguments: Value::Null,
                output: Some("ok".into()),
                is_error: false,
            },
            TranscriptItem::Tool {
                id: "two".into(),
                name: "write_file".into(),
                arguments: Value::Null,
                output: Some("denied".into()),
                is_error: true,
            },
            TranscriptItem::Tool {
                id: "three".into(),
                name: "search_files".into(),
                arguments: Value::Null,
                output: None,
                is_error: false,
            },
        ];
        assert_eq!(work_counts(&tools), (1, 1, 1));
    }
}
