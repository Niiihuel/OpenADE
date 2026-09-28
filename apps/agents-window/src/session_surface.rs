//! Session surface composed from the persisted/live transcript projection.
//!
//! A turn is visually anchored by its user prompt. Consecutive tool calls are
//! shown as one work group, but remain individually selectable. No task count,
//! duration, or file change is inferred from text: only transcript facts appear.

use openide_core::OutputStream;
use openide_ui::{
    ButtonVariant, Color, Component, ControlSize, Element, FontFamily, Icon,
    IconButton as UiIconButton, ScrollArea,
};

use crate::{
    composer_ui::{build_composer, ComposerProps},
    rich_text, theme,
    transcript::TranscriptItem,
    view::{sidebar_reopen_control, PermissionView, ViewState},
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
    let show_side_toggle = !side_visible && side_available && width >= 300.0;
    let show_settings = width >= 220.0;
    let mut top = Vec::new();
    if sidebar_hidden {
        top.push(sidebar_reopen_control(theme::BACKGROUND));
    }
    top.extend([
        Element::truncated_text(session_name(state))
            .foreground(theme::FOREGROUND)
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .flex(1.0)
            .size(None, Some(HEADER_ACTION_SIZE)),
        action(
            "new_session_header",
            Icon::Add,
            "",
            theme::BACKGROUND,
            theme::FOREGROUND_SUBTLE,
        )
        .size(Some(32.0), Some(32.0)),
    ]);
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

fn message(item: &TranscriptItem, turn_number: usize, reading_width: f32) -> Element {
    match item {
        TranscriptItem::User { content } => Element::panel(
            Element::column(vec![
                turn_heading(turn_number, item),
                text(content, theme::FOREGROUND, 14.0),
            ])
            .gap(9.0),
        )
        .padding(16.0)
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(12.0),
        TranscriptItem::Assistant { content, .. } => Element::panel(
            Element::column(vec![
                turn_heading(turn_number, item),
                rich_text::render_markdown_at_width(content, (reading_width - 24.0).max(96.0)),
            ])
            .gap(9.0),
        )
        .padding(12.0)
        .background(theme::BACKGROUND),
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
                OutputStream::Stderr => ("ERROR DEL PROCESO", theme::DESTRUCTIVE),
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
    let (status, ink) = if output.is_none() {
        ("EN CURSO", theme::FOREGROUND_SUBTLE)
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
    let (completed, running, failed) = work_counts(items);
    let count = |number: usize, singular: &str, plural: &str| {
        format!("{number} {}", if number == 1 { singular } else { plural })
    };
    let summary = if compact {
        let mut summary = format!("{completed}/{} listas", completed + running + failed);
        if failed > 0 {
            summary.push_str(&format!(" · {failed} error"));
        }
        summary
    } else {
        let mut parts = vec![count(completed, "lista", "listas")];
        if running > 0 {
            parts.push(format!("{running} en curso"));
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
        rows.push(message(item, turn_number, reading_width));
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
        ])
        .gap(7.0),
    )
    .padding(12.0)
    .background(theme::CARD)
    .border(theme::BORDER, 1.0)
    .radius(10.0)
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
    let mut children = vec![
        header(state, center_width, side_visible, side_available),
        timeline(state, center_width, side_visible),
    ];
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
    children.push(composer(state, center_width));
    Element::column(children).flex(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{command_center::Scope, settings_ui::SettingsPage, view::SideTab};
    use openide_ui::{DrawCommand, Size, UiTree};
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
            composer_project_menu_open: false,
            composer_project_options: Vec::new(),
            endpoint: String::new(),
            model: "gpt-4.1".into(),
            native_provider_id: "custom".into(),
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
            composer_model_menu_open: false,
            composer_effort: "default".into(),
            composer_effort_options: vec![],
            composer_effort_menu_open: false,
            sessions: vec![],
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
            settings_reduced_motion: false,
            settings_reopen_last_workspace: false,
            settings_terminal_shell: crate::settings_preferences::TerminalShell::System,
            settings_terminal_shell_draft: String::new(),
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
            show_dock_overview: false,
            show_dock_add_menu: false,
            file_preview: None,
            git_review: None,
            review_detail: None,
            terminal_snapshot: None,
            terminal_input: String::new(),
            terminal_error: None,
            pending_permissions: vec![],
            selected_tool_id: None,
            show_jump_to_bottom: false,
            github_account: crate::github_account::GithubAccountStatus::Disconnected,
            github_account_menu_open: false,
        }
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
    fn compact_session_preserves_composer_controls_inside_dock() {
        let frame = UiTree::new(build_session_surface(&state(), 300.0, false, false))
            .layout(Size::new(300.0, 500.0));
        for id in [
            "new_session_header",
            "prompt",
            "run_toggle",
            "composer_model_toggle",
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
                    } if *color == theme::SELECTED && *radius == 25.0 => Some(*rect),
                    _ => None,
                })
                .expect("rounded composer card");
            assert!(card.x >= 0.0 && card.x + card.width <= width, "{width}");
            assert!((card.x - (width - card.x - card.width)).abs() <= 1.0);
            assert!(
                card.height >= 115.0 && card.height <= 230.0,
                "{width}: {card:?}"
            );
            for id in ["prompt", "run_toggle", "composer_model_toggle"] {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap_or_else(|| panic!("missing {id} at {width}"))
                    .rect;
                assert!(rect.x >= card.x, "{id} at {width}");
                assert!(
                    rect.x + rect.width <= card.x + card.width,
                    "{id} at {width}"
                );
                assert!(rect.y >= card.y, "{id} at {width}");
                assert!(
                    rect.y + rect.height <= card.y + card.height,
                    "{id} at {width}"
                );
            }
            assert!(visible_text(&frame).contains("openIDE"));
            assert!(!visible_text(&frame).contains("Listo"));
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::StrokeRect { rect, radius, .. }
                    if *rect == card && *radius == 25.0
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
                } if *color == theme::SELECTED && *radius == 25.0 => Some(*rect),
                _ => None,
            });
            let card = card.expect("rounded composer card");
            let submit = frame
                .regions
                .iter()
                .find(|region| region.id == "run_toggle")
                .expect("stop action");
            assert_eq!(submit.rect.width, 40.0);
            assert!(submit.rect.x + submit.rect.width <= card.x + card.width);
            let labels = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, position, .. } => Some((text.as_str(), position.x)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let project = labels
                .iter()
                .find(|(text, _)| text.starts_with("Proyecto"))
                .expect("project label");
            let status = labels
                .iter()
                .find(|(text, _)| text.starts_with("Ejecut"))
                .expect("status label");
            assert!(project.0.ends_with('…'));
            assert!(status.0.ends_with('…'));
            assert!(project.1 + project.0.chars().count() as f32 * 7.0 <= status.1);
            assert!(
                status.1 + status.0.chars().count() as f32 * 7.0 <= card.x + card.width,
                "{width}: status={status:?}, card={card:?}"
            );
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
        assert!(visible.contains("1 lista · 1 en curso"), "{visible}");
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
            .any(|region| region.id == "permission_deny:request-1"));
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
