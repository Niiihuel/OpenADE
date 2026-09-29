//! Shared composer for the empty task and an active conversation.
//!
//! The project context is a separate surface above the editor. The editing
//! card owns the input and actions, with stable hit targets across widths.
//! The effort control repaints while visible for its ambient particle effect.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use openide_core::TurnMode;
use openide_ui::{
    Button, ButtonVariant, Color, Component, ComponentPalette, Composer, ControlSize, DropdownMenu,
    DropdownMenuEntry, DropdownMenuItem, Element, ElementKind, FontFamily, Icon, IconButton,
    MenuMotion, PopoverPlacement, SearchField, Tooltip,
};

use crate::{
    composer_selection::{CredentialAvailability, ExecutionSupport, ModelRouteOption, RouteSource},
    provider_icons::icon_for_provider,
    theme,
    view::{ComposerAttachmentView, ProviderModelStatus, ViewState},
};

const COMPACT_CONTEXT_WIDTH: f32 = 490.0;
const EDITOR_LINE_HEIGHT: f32 = 20.0;
const CARD_RADIUS: f32 = 26.0;
const CONTROL_HEIGHT: f32 = 36.0;
const SELECTOR_HEIGHT: f32 = 36.0;
/// Keep custom controls clear of DropdownMenu's reserved scrollbar rail.
const MODEL_MENU_SCROLL_GUTTER: f32 = 16.0;
const CONTEXT_HEIGHT: f32 = 28.0;
const PILL_SURFACE: Color = Color::rgb(38, 38, 38);
const MUTED_INK: Color = Color::rgb(117, 117, 117);
const SEND_BLUE: Color = Color::rgb(37, 113, 251);
const SELECTOR_CARD: Color = theme::MENU_SURFACE;
const SELECTOR_PILL: Color = theme::INPUT;
const SELECTOR_PILL_HOVER: Color = theme::SELECTED;
const SELECTOR_INK: Color = theme::FOREGROUND;
const SELECTOR_MUTED: Color = theme::MENU_MUTED;
const SELECTOR_HAIRLINE: Color = theme::BORDER;

#[derive(Clone, Copy)]
pub(crate) struct ComposerProps<'a> {
    pub workspace: &'a str,
    pub project_action: Option<&'a str>,
    pub project_branch: Option<&'a str>,
    pub project_menu_open: bool,
    pub project_options: &'a [(String, String)],
    pub prompt: &'a str,
    pub attachments: &'a [ComposerAttachmentView],
    pub add_menu_open: bool,
    pub listening: bool,
    pub placeholder: &'a str,
    pub selected_backend: &'a str,
    pub native_provider_id: &'a str,
    pub backend_options: &'a [(String, String)],
    pub backend_menu_open: bool,
    /// Configured native model, retained for native backends that do not expose
    /// a model catalogue. CLI model choices come from the matching profile.
    pub model: &'a str,
    pub model_label: &'a str,
    pub model_options: &'a [(String, String)],
    pub model_routes: &'a [ModelRouteOption],
    pub provider_status: Option<&'a BTreeMap<String, ProviderModelStatus>>,
    pub selected_model: &'a str,
    pub model_menu_open: bool,
    pub model_list_open: bool,
    pub model_query: &'a str,
    pub advanced_open: bool,
    pub effort_label: &'a str,
    pub effort_options: &'a [(String, String)],
    pub selected_effort: &'a str,
    pub effort_menu_open: bool,
    pub effort_preview: Option<f32>,
    pub turn_mode: TurnMode,
    pub running: bool,
    pub workspace_writes_enabled: bool,
    pub status: Option<&'a str>,
    pub context_percent: Option<u8>,
    pub goal: Option<&'a str>,
    pub goal_editor_open: bool,
    pub goal_draft: &'a str,
    /// Width of the entire composer, excluding the parent dock's gutters.
    pub width: f32,
    pub home: bool,
}

impl<'a> ComposerProps<'a> {
    /// Project, backend and model data are projected once from application
    /// state. Home, session and fallback surfaces differ only in placement.
    pub(crate) fn from_state(
        state: &'a ViewState,
        width: f32,
        home: bool,
        project_action: Option<&'a str>,
        placeholder: &'a str,
        status: Option<&'a str>,
    ) -> Self {
        let model_label = state
            .composer_model_options
            .iter()
            .find(|(id, _)| id == &state.composer_model)
            .map(|(_, label)| label.as_str())
            .unwrap_or(
                if matches!(state.selected_backend.as_str(), "native" | "codex") {
                    &state.composer_model
                } else {
                    ""
                },
            );
        let effort_label = state
            .composer_effort_options
            .iter()
            .find(|(id, _)| id == &state.composer_effort)
            .map(|(_, label)| label.as_str())
            .unwrap_or("");
        let project_branch = state.project_branch.as_deref();
        Self {
            workspace: &state.workspace,
            project_action,
            project_branch,
            project_menu_open: state.composer_project_menu_open,
            project_options: &state.composer_project_options,
            prompt: &state.prompt,
            attachments: &state.composer_attachments,
            add_menu_open: state.composer_add_menu_open,
            listening: state.composer_listening,
            placeholder,
            selected_backend: &state.selected_backend,
            native_provider_id: &state.native_provider_id,
            backend_options: &state.backend_options,
            backend_menu_open: state.backend_menu_open,
            model: &state.model,
            model_label,
            model_options: &state.composer_model_options,
            model_routes: &state.composer_routes,
            provider_status: Some(&state.composer_provider_status),
            selected_model: &state.composer_model,
            model_menu_open: state.composer_model_menu_open,
            model_list_open: state.composer_model_list_open,
            model_query: &state.composer_model_query,
            advanced_open: state.composer_advanced_open,
            effort_label,
            effort_options: &state.composer_effort_options,
            selected_effort: &state.composer_effort,
            effort_menu_open: state.composer_effort_menu_open,
            effort_preview: state.composer_effort_preview,
            turn_mode: state.turn_mode,
            running: state.running || state.background_run_selected,
            workspace_writes_enabled: state.workspace_writes_enabled,
            status,
            context_percent: state.composer_context_percent,
            goal: state.composer_goal.as_deref(),
            goal_editor_open: state.composer_goal_editor_open,
            goal_draft: &state.composer_goal_draft,
            width,
            home,
        }
    }
}

fn project_name(workspace: &str) -> &str {
    Path::new(workspace)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("Elegir proyecto")
}

fn editor_height(prompt: &str, width: f32, compact: bool, home: bool) -> f32 {
    let chars_per_line = ((width - if compact { 196.0 } else { 260.0 }).max(80.0) / 7.5)
        .floor()
        .max(1.0) as usize;
    let lines = prompt
        .split('\n')
        .map(|line| line.chars().count().max(1).div_ceil(chars_per_line))
        .sum::<usize>();
    let minimum = 36.0;
    let maximum = if compact {
        128.0
    } else if home {
        160.0
    } else {
        180.0
    };
    (lines as f32 * EDITOR_LINE_HEIGHT + 14.0).clamp(minimum, maximum)
}

fn metadata_item(icon: Icon, value: &str, max_label_width: f32) -> Element {
    let label_width = metadata_label_width(value, max_label_width);
    Element::row(vec![
        Element::icon(icon)
            .foreground(MUTED_INK)
            .size(Some(16.0), Some(CONTEXT_HEIGHT)),
        Element::truncated_text(value)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .foreground(MUTED_INK)
            .size(Some(label_width), Some(CONTEXT_HEIGHT)),
    ])
    .gap(5.0)
    .size(None, Some(CONTEXT_HEIGHT))
}

fn metadata_label_width(value: &str, max_label_width: f32) -> f32 {
    (value.chars().count() as f32 * 7.2).clamp(24.0, max_label_width)
}

fn project_control_width(props: &ComposerProps<'_>, compact: bool) -> f32 {
    let desired = (project_name(props.workspace).chars().count() as f32 * 7.2 + 40.0)
        .clamp(82.0, if compact { 128.0 } else { 200.0 });
    let gap = if compact { 5.0 } else { 12.0 };
    desired.min((props.width - backend_control_width(props) - 2.0 * gap).max(0.0))
}

fn project_control(props: &ComposerProps<'_>, compact: bool) -> Element {
    let name = project_name(props.workspace);
    let width = project_control_width(props, compact);
    let id = props.project_action.unwrap_or("composer_project_toggle");
    let visual = Element::row(vec![
        Element::icon(Icon::Folder)
            .foreground(MUTED_INK)
            .size(Some(16.0), Some(CONTEXT_HEIGHT)),
        Element::truncated_text(name)
            .foreground(MUTED_INK)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .flex(1.0)
            .size(None, Some(CONTEXT_HEIGHT)),
        Element::icon(Icon::ChevronDown)
            .foreground(MUTED_INK)
            .size(Some(16.0), Some(CONTEXT_HEIGHT)),
    ])
    .gap(5.0)
    .size(Some(width), Some(CONTEXT_HEIGHT));
    let trigger = if props.running {
        Element::panel(visual)
    } else {
        Element::clickable(id, visual)
    }
    .hover_background(PILL_SURFACE)
    .radius(8.0)
    .size(Some(width), Some(CONTEXT_HEIGHT));
    let mut entries = vec![DropdownMenuEntry::Heading("Local Folders".to_owned())];
    entries.extend(
        props
            .project_options
            .iter()
            .enumerate()
            .map(|(index, (path, label))| {
                DropdownMenuItem::new(format!("composer_project_select:{index}"), label)
                    .icon(Icon::Folder)
                    .selected(path == props.workspace)
                    .into()
            }),
    );
    entries.push(DropdownMenuEntry::Separator);
    entries.push(
        DropdownMenuItem::new("composer_project_browse", "Elegir otra carpeta…")
            .icon(Icon::Folder)
            .into(),
    );
    DropdownMenu::new(
        Tooltip::new(trigger, id, props.workspace, theme::component_palette()).render(),
        entries,
        props.project_menu_open && !props.running,
        "composer_project_dismiss",
        theme::menu_palette(),
    )
    .width(266.0)
    .placement(PopoverPlacement::TopStart)
    .render()
}

fn backend_label<'a>(props: &'a ComposerProps<'_>) -> &'a str {
    if props.selected_backend == "native" {
        "Agent"
    } else {
        props
            .backend_options
            .iter()
            .find(|(id, _)| id == props.selected_backend)
            .map(|(_, label)| label.as_str())
            .unwrap_or("Agent")
    }
}

fn backend_control_width(props: &ComposerProps<'_>) -> f32 {
    (backend_label(props).chars().count() as f32 * 7.2 + 40.0).clamp(70.0, 124.0)
}

fn backend_control(props: &ComposerProps<'_>) -> Element {
    let label = backend_label(props);
    let width = backend_control_width(props);
    let visual = Element::row(vec![
        Element::icon(Icon::Infinity)
            .foreground(MUTED_INK)
            .size(Some(16.0), Some(CONTEXT_HEIGHT)),
        Element::truncated_text(label)
            .foreground(MUTED_INK)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .flex(1.0)
            .size(None, Some(CONTEXT_HEIGHT)),
        Element::icon(Icon::ChevronDown)
            .foreground(MUTED_INK)
            .size(Some(16.0), Some(CONTEXT_HEIGHT)),
    ])
    .gap(5.0)
    .size(Some(width), Some(CONTEXT_HEIGHT));
    let trigger = if props.running {
        Element::panel(visual)
    } else {
        Element::clickable("backend_menu_toggle", visual)
    }
    .hover_background(PILL_SURFACE)
    .radius(8.0)
    .size(Some(width), Some(CONTEXT_HEIGHT));
    let mut entries = vec![DropdownMenuEntry::Heading("Agente".to_owned())];
    entries.extend(props.backend_options.iter().map(|(id, label)| {
        DropdownMenuItem::new(format!("backend:{id}"), label)
            .selected(id == props.selected_backend)
            .into()
    }));
    if props.selected_backend == "native" {
        entries.push(DropdownMenuEntry::Separator);
        entries.push(
            DropdownMenuItem::new(
                "write_mode",
                if props.workspace_writes_enabled {
                    "Escritura con permiso"
                } else {
                    "Solo lectura"
                },
            )
            .icon(Icon::Settings)
            .into(),
        );
    }
    DropdownMenu::new(
        trigger,
        entries,
        props.backend_menu_open && !props.running,
        "backend_menu_dismiss",
        theme::menu_palette(),
    )
    .width(230.0)
    .placement(PopoverPlacement::TopEnd)
    .render()
}

fn project_metadata(props: &ComposerProps<'_>) -> Element {
    let compact = props.width < COMPACT_CONTEXT_WIDTH;
    let gap = if compact { 5.0 } else { 12.0 };
    // Project, flexible spacer and backend are always present. Each optional
    // item consumes its measured width and one more gap in the fixed row.
    let mut occupied =
        project_control_width(props, compact) + backend_control_width(props) + 2.0 * gap;
    let show_context = props.context_percent.is_some() && occupied + gap + 62.0 <= props.width;
    if show_context {
        occupied += gap + 62.0;
    }
    let goal = props.goal.filter(|goal| {
        props.width >= 650.0
            && occupied + gap + 21.0 + metadata_label_width(goal, 120.0) <= props.width
    });
    if let Some(goal) = goal {
        occupied += gap + 21.0 + metadata_label_width(goal, 120.0);
    }
    let branch = props.project_branch.filter(|branch| {
        !compact
            && !branch.is_empty()
            && occupied + gap + 21.0 + metadata_label_width(branch, 116.0) <= props.width
    });
    if let Some(branch) = branch {
        occupied += gap + 21.0 + metadata_label_width(branch, 116.0);
    }
    let status = props.status.filter(|status| {
        props.running && !compact && !status.is_empty() && occupied + gap + 120.0 <= props.width
    });
    let mut row = Vec::new();
    if let Some(branch) = branch {
        row.push(metadata_item(Icon::GitBranch, branch, 116.0));
    }
    row.push(project_control(props, compact));
    if let Some(goal) = goal {
        row.push(
            Tooltip::new(
                Element::clickable(
                    "composer_goal_open",
                    metadata_item(Icon::Target, goal, 120.0),
                )
                .hover_background(PILL_SURFACE)
                .radius(8.0),
                "composer_goal_open",
                format!("Objetivo: {goal}"),
                theme::component_palette(),
            )
            .render(),
        );
    }
    row.push(Element::spacer().flex(1.0));
    if let Some(status) = status {
        row.push(
            Element::truncated_text(status)
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(MUTED_INK)
                .size(Some(120.0), Some(CONTEXT_HEIGHT)),
        );
    }
    row.push(backend_control(props));
    if let Some(percent) = props.context_percent.filter(|_| show_context) {
        row.push(
            Tooltip::new(
                Element::panel(
                    Element::row(vec![
                        Element::icon(Icon::ContextRing)
                            .foreground(Color::rgb(164, 164, 164))
                            .size(Some(16.0), Some(26.0)),
                        Element::text(format!("{percent}%"))
                            .font_family(FontFamily::Sans)
                            .font_size(13.0)
                            .foreground(MUTED_INK)
                            .size(None, Some(26.0)),
                    ])
                    .gap(5.0),
                )
                .background(PILL_SURFACE)
                .radius(13.0)
                .padding(5.0)
                .size(Some(62.0), Some(26.0)),
                "composer_context_percent",
                "Contexto restante",
                theme::component_palette(),
            )
            .render(),
        );
    }
    Element::row(row)
        .gap(gap)
        .size(Some(props.width), Some(CONTEXT_HEIGHT))
}

fn add_context_control(props: &ComposerProps<'_>) -> Element {
    let trigger = Tooltip::new(
        IconButton::new(
            "composer_add_context",
            if props.add_menu_open {
                Icon::Close
            } else {
                Icon::Add
            },
            theme::action_palette(Color::rgb(64, 64, 64), Color::rgb(246, 246, 246)),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Regular)
        .enabled(!props.running)
        .render()
        .size(Some(CONTROL_HEIGHT), Some(CONTROL_HEIGHT))
        .radius(CONTROL_HEIGHT / 2.0),
        "composer_add_context",
        "Añadir al mensaje",
        theme::component_palette(),
    )
    .render();
    let entries = if props.goal_editor_open {
        let mut entries = vec![DropdownMenuEntry::Heading("Goal".to_owned())];
        entries.push(DropdownMenuEntry::Custom(
            Element::text_input(
                "composer_goal_draft",
                props.goal_draft,
                "Describe el objetivo",
            )
            .background(Color::rgb(53, 53, 53))
            .foreground(theme::FOREGROUND)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .radius(9.0)
            .size(Some(344.0), Some(38.0)),
        ));
        entries.push(DropdownMenuEntry::Custom(
            Element::row(vec![
                Button::new(
                    "composer_goal_save",
                    "Guardar",
                    theme::action_palette(SEND_BLUE, Color::rgb(255, 255, 255)),
                )
                .size(ControlSize::Compact)
                .render(),
                Button::new(
                    "composer_goal_cancel",
                    "Cancelar",
                    theme::action_palette(PILL_SURFACE, theme::FOREGROUND),
                )
                .size(ControlSize::Compact)
                .render(),
                Element::spacer().flex(1.0),
                Button::new(
                    "composer_goal_clear",
                    "Borrar",
                    theme::action_palette(PILL_SURFACE, MUTED_INK),
                )
                .size(ControlSize::Compact)
                .render(),
            ])
            .gap(5.0)
            .size(Some(344.0), Some(32.0)),
        ));
        entries
    } else {
        vec![
            DropdownMenuEntry::Heading("Add".to_owned()),
            DropdownMenuItem::new("composer_add_files", "Files and folders")
                .icon(Icon::Paperclip)
                .submenu(vec![
                    DropdownMenuItem::new("composer_add_file", "File from project")
                        .icon(Icon::Paperclip)
                        .into(),
                    DropdownMenuItem::new("composer_add_folder", "Folder from project")
                        .icon(Icon::Folder)
                        .into(),
                ])
                .into(),
            DropdownMenuItem::new("composer_goal_open", "Goal")
                .icon(Icon::Target)
                .annotation(if props.goal.is_some() {
                    "Edit current goal"
                } else {
                    "Set a goal for faster results"
                })
                .into(),
            {
                let item = DropdownMenuItem::new("turn_mode_toggle", "Plan mode")
                    .icon(Icon::Plan)
                    .annotation("Manage complex tasks")
                    .selected(props.turn_mode == TurnMode::Plan);
                (if props.selected_backend == "native" {
                    item
                } else {
                    item.disabled()
                })
                .into()
            },
            DropdownMenuEntry::Heading("Plugins".to_owned()),
            DropdownMenuItem::new("composer_plugin_documents", "Documents")
                .icon(Icon::Paperclip)
                .annotation("Create and edit documents")
                .into(),
            DropdownMenuItem::new("composer_plugin_spreadsheets", "Spreadsheets")
                .icon(Icon::Grid)
                .annotation("Generate spreadsheets")
                .into(),
            DropdownMenuItem::new("composer_plugin_presentations", "Presentations")
                .icon(Icon::Grid)
                .annotation("Create marketing assets")
                .into(),
            DropdownMenuItem::new("composer_plugin_code_blocks", "Code blocks")
                .icon(Icon::Code)
                .annotation("Write and edit existing code")
                .into(),
        ]
    };
    DropdownMenu::new(
        trigger,
        entries,
        props.add_menu_open && !props.running,
        "composer_add_dismiss",
        theme::menu_palette(),
    )
    .width(360.0)
    .placement(PopoverPlacement::TopStart)
    .motion(MenuMotion {
        open_ms: 160,
        item_ms: 100,
    })
    .max_visible_items(10)
    .render()
}

fn effort_tone(step: usize, count: usize) -> Color {
    let progress = step as f32 / count.saturating_sub(1).max(1) as f32;
    let channel = |start: u8, end: u8| {
        (f32::from(start) + (f32::from(end) - f32::from(start)) * progress).round() as u8
    };
    Color::rgb(
        channel(theme::EFFORT_BLUE.r, theme::EFFORT_ULTRA.r),
        channel(theme::EFFORT_BLUE.g, theme::EFFORT_ULTRA.g),
        channel(theme::EFFORT_BLUE.b, theme::EFFORT_ULTRA.b),
    )
}

fn preview_effort<'a>(props: &'a ComposerProps<'_>) -> Option<(usize, usize, &'a str)> {
    let levels: Vec<_> = props
        .effort_options
        .iter()
        .filter(|(id, _)| id != "default")
        .collect();
    if levels.is_empty() {
        return None;
    }
    let selected = levels
        .iter()
        .position(|(id, _)| id == props.selected_effort)
        .unwrap_or(0);
    let index = props
        .effort_preview
        .map(|fraction| {
            (fraction.clamp(0.0, 1.0) * levels.len().saturating_sub(1) as f32).round() as usize
        })
        .unwrap_or(selected)
        .min(levels.len() - 1);
    Some((index, levels.len(), levels[index].1.as_str()))
}

fn selector_palette() -> ComponentPalette {
    ComponentPalette {
        surface: SELECTOR_CARD,
        raised: SELECTOR_CARD,
        hover: SELECTOR_PILL,
        selected: SELECTOR_PILL_HOVER,
        border: SELECTOR_HAIRLINE,
        text: SELECTOR_INK,
        muted: SELECTOR_MUTED,
        shadow: theme::menu_palette().shadow,
    }
}

fn selector_row(id: &str, title: &str, value: &str, width: f32) -> Element {
    let content_width = (width - 16.0).max(0.0);
    let value_width = (content_width - 74.0 - 12.0 - 18.0).max(24.0);
    Element::clickable(
        id,
        Element::row(vec![
            Element::truncated_text(title)
                .font_family(FontFamily::Sans)
                .font_size(14.0)
                .foreground(SELECTOR_INK)
                .size(Some(74.0), Some(20.0)),
            Element::spacer().flex(1.0),
            Element::truncated_text(value)
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .foreground(SELECTOR_MUTED)
                .size(Some(value_width), Some(20.0)),
            Element::icon(Icon::ChevronRight)
                .foreground(SELECTOR_MUTED)
                .size(Some(12.0), Some(20.0)),
        ])
        .gap(6.0)
        .size(Some(content_width), Some(20.0)),
    )
    .hover_background(SELECTOR_PILL)
    .pressed_background(SELECTOR_PILL_HOVER)
    .transition_ms(0)
    .padding(8.0)
    .radius(10.0)
    .size(Some(width), Some(36.0))
}

fn selector_disabled_row(title: &str, value: &str, width: f32) -> Element {
    let content_width = (width - 16.0).max(0.0);
    Element::row(vec![
        Element::truncated_text(title)
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .foreground(SELECTOR_MUTED)
            .size(Some(74.0), Some(20.0)),
        Element::spacer().flex(1.0),
        Element::truncated_text(value)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(SELECTOR_MUTED)
            .size(Some((content_width - 90.0).max(24.0)), Some(20.0)),
    ])
    .gap(6.0)
    .padding(8.0)
    .size(Some(width), Some(36.0))
}

fn advanced_chip(width: f32) -> Element {
    Element::clickable(
        "composer_advanced_chip_toggle",
        Element::row(vec![
            Element::text("Avanzado")
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .foreground(SELECTOR_MUTED)
                .size(Some(69.0), Some(18.0)),
            Element::icon(Icon::ChevronRight)
                .foreground(SELECTOR_MUTED)
                .size(Some(12.0), Some(18.0)),
        ])
        .gap(4.0)
        .size(Some(85.0), Some(18.0)),
    )
    .hover_background(SELECTOR_PILL)
    .pressed_background(SELECTOR_PILL_HOVER)
    .transition_ms(0)
    .padding(7.0)
    .radius(10.0)
    .size(Some(width.min(100.0)), Some(32.0))
}

fn effort_panel(props: &ComposerProps<'_>, width: f32) -> Element {
    // `default` is a preference choice, not a tier advertised by the model.
    let levels: Vec<_> = props
        .effort_options
        .iter()
        .filter(|(id, _)| id != "default")
        .collect();
    let selected = levels
        .iter()
        .position(|(id, _)| id == props.selected_effort)
        .unwrap_or(0);
    let fraction = props
        .effort_preview
        .unwrap_or_else(|| selected as f32 / levels.len().saturating_sub(1).max(1) as f32);
    let preview_index = preview_effort(props)
        .map(|(index, _, _)| index)
        .unwrap_or(0);
    let has_default = props.effort_options.iter().any(|(id, _)| id == "default");
    let heading_items = {
        let back_width = 88.0;
        let auto_width = if has_default { 46.0 } else { 0.0 };
        // The flex spacer still contributes a row gap on each side.
        let label_width =
            (width - back_width - auto_width - if has_default { 18.0 } else { 12.0 }).max(18.0);
        let mut items = vec![
            Element::clickable(
                "composer_advanced_back",
                Element::row(vec![
                    Element::icon(Icon::ArrowLeft)
                        .foreground(SELECTOR_MUTED)
                        .size(Some(14.0), Some(24.0)),
                    Element::truncated_text("Avanzado")
                        .font_family(FontFamily::Sans)
                        .font_size(14.0)
                        .foreground(SELECTOR_INK)
                        .size(Some(68.0), Some(24.0)),
                ])
                .gap(4.0)
                .size(Some(back_width), Some(24.0)),
            )
            .hover_background(SELECTOR_PILL)
            .transition_ms(0)
            .radius(7.0)
            .size(Some(back_width), Some(24.0)),
            Element::spacer().flex(1.0),
            Element::truncated_text(
                levels
                    .get(preview_index)
                    .map(|(_, label)| label.as_str())
                    .unwrap_or(""),
            )
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .foreground(effort_tone(preview_index, levels.len()))
            .size(Some(label_width), Some(24.0)),
        ];
        if has_default {
            items.push(
                Element::clickable(
                    "composer_effort:default",
                    Element::truncated_text("Auto")
                        .font_family(FontFamily::Sans)
                        .font_size(14.0)
                        .foreground(if props.selected_effort == "default" {
                            theme::EFFORT_BLUE
                        } else {
                            SELECTOR_MUTED
                        })
                        .size(Some(auto_width - 8.0), Some(24.0)),
                )
                .background(if props.selected_effort == "default" {
                    SELECTOR_PILL
                } else {
                    Color::TRANSPARENT
                })
                .hover_background(SELECTOR_PILL)
                .transition_ms(0)
                .radius(7.0)
                .padding(4.0)
                .size(Some(auto_width), Some(24.0)),
            );
        }
        items
    };
    let heading = Element::row(heading_items)
        .gap(6.0)
        .size(Some(width), Some(24.0));
    let mut children = vec![heading];
    if levels.len() >= 2 {
        children.push(
            Element::effort_slider("composer_effort_slider", fraction, levels.len())
                .effort_ultra_available(levels.last().is_some_and(|(id, _)| id.as_str() == "ultra"))
                .size(Some(width), Some(38.0)),
        );
    } else {
        children.push(
            Element::text(if levels.len() == 1 {
                "Este modelo ofrece un solo nivel de esfuerzo"
            } else {
                "Este modelo no publica niveles de esfuerzo"
            })
            .font_family(FontFamily::Sans)
            .font_size(11.0)
            .foreground(SELECTOR_MUTED)
            .size(Some(width), Some(30.0)),
        );
    }
    let height = if levels.len() >= 2 { 84.0 } else { 62.0 };
    Element::column(children)
        .gap(4.0)
        .size(Some(width), Some(height))
}

fn model_search_key(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(|character| match character {
            '\u{0300}'..='\u{036f}' => None,
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => Some('a'),
            'é' | 'è' | 'ê' | 'ë' => Some('e'),
            'í' | 'ì' | 'î' | 'ï' => Some('i'),
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => Some('o'),
            'ú' | 'ù' | 'û' | 'ü' => Some('u'),
            'ñ' => Some('n'),
            'ç' => Some('c'),
            other => Some(other),
        })
        .collect()
}

fn model_search_matches(terms: &[String], fields: &[&str]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let haystack = model_search_key(&fields.join(" "));
    terms.iter().all(|term| haystack.contains(term))
}

fn model_provider_heading(route: &ModelRouteOption) -> String {
    if route.source == RouteSource::CliLiveCatalog {
        format!("{} · En tu cuenta", route.provider_name)
    } else {
        route.provider_name.clone()
    }
}

fn model_effort_entries(props: &ComposerProps<'_>, width: f32) -> Vec<DropdownMenuEntry> {
    let inner_width = width - 12.0;
    if props.model_list_open {
        let mut search_palette = selector_palette();
        search_palette.raised = theme::INPUT;
        let terms: Vec<String> = model_search_key(props.model_query)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut entries = vec![
            DropdownMenuItem::new("composer_model_list_toggle", "Modelo")
                .icon(Icon::ArrowLeft)
                .into(),
            DropdownMenuEntry::Custom(
                SearchField::new(
                    "composer_model_search",
                    props.model_query,
                    "Buscar modelos o proveedores",
                    search_palette,
                )
                .size(ControlSize::Regular)
                .width(inner_width)
                .render(),
            ),
            DropdownMenuEntry::Heading("MODELOS DISPONIBLES".to_owned()),
            DropdownMenuItem::new("composer_routes_refresh", "Actualizar proveedores")
                .icon(Icon::Refresh)
                .into(),
        ];
        let mut visible_models = 0usize;
        if props.model_routes.is_empty() {
            let provider_id = if props.selected_backend == "native" {
                props.native_provider_id
            } else {
                props.selected_backend
            };
            for (id, title) in props.model_options {
                if !model_search_matches(&terms, &[id, title, provider_id]) {
                    continue;
                }
                entries.push(
                    DropdownMenuItem::new(format!("composer_model:{id}"), title.as_str())
                        .icon(icon_for_provider(provider_id))
                        .selected(id == props.selected_model)
                        .into(),
                );
                visible_models += 1;
            }
        } else {
            let mut previous_provider = "";
            let mut disconnected = BTreeSet::new();
            for route in props.model_routes {
                if !model_search_matches(
                    &terms,
                    &[
                        &route.provider_id,
                        &route.provider_name,
                        &route.model_id,
                        &route.model_name,
                    ],
                ) {
                    continue;
                }
                let provider_status = props
                    .provider_status
                    .and_then(|states| states.get(&route.provider_id))
                    .copied();
                if let Some(status) = provider_status {
                    if !disconnected.insert(route.provider_id.as_str()) {
                        continue;
                    }
                    if route.provider_id != previous_provider {
                        entries.push(DropdownMenuEntry::Heading(model_provider_heading(route)));
                        previous_provider = &route.provider_id;
                    }
                    let item = DropdownMenuItem::new(
                        format!("composer_route:{}", route.route_id),
                        match status {
                            ProviderModelStatus::Checking => {
                                format!("Consultando {}...", route.provider_name)
                            }
                            ProviderModelStatus::CheckFailed => {
                                format!("Revisar conexión de {}", route.provider_name)
                            }
                        },
                    )
                    .icon(icon_for_provider(&route.provider_id));
                    entries.push(match status {
                        ProviderModelStatus::Checking => item.disabled().into(),
                        ProviderModelStatus::CheckFailed => item.annotation("Error").into(),
                    });
                    visible_models += 1;
                    continue;
                }
                if route.provider_listed == Some(false) {
                    continue;
                }
                if route.credential == CredentialAvailability::MissingCredential
                    && !disconnected.insert(route.provider_id.as_str())
                {
                    continue;
                }
                if route.provider_id != previous_provider {
                    entries.push(DropdownMenuEntry::Heading(model_provider_heading(route)));
                    previous_provider = &route.provider_id;
                }
                if route.credential == CredentialAvailability::MissingCredential {
                    entries.push(
                        DropdownMenuItem::new(
                            format!("composer_route:{}", route.route_id),
                            format!("Conectar {}", route.provider_name),
                        )
                        .icon(icon_for_provider(&route.provider_id))
                        .annotation("Falta clave")
                        .into(),
                    );
                    visible_models += 1;
                    continue;
                }
                let status = match route.source {
                    RouteSource::CliLiveCatalog => "",
                    RouteSource::CliCatalog => "CLI sin verificar",
                    _ => match (route.credential, route.execution) {
                        (_, ExecutionSupport::Unsupported) => "No compatible",
                        (CredentialAvailability::NoKeyRequired, _) => "Clave opcional",
                        (CredentialAvailability::EnvPresentUnverified, _)
                            if route.provider_listed == Some(true) =>
                        {
                            "En tu cuenta"
                        }
                        (
                            CredentialAvailability::EnvPresentUnverified,
                            ExecutionSupport::Verified,
                        ) => "Clave detectada",
                        (_, ExecutionSupport::ProviderListedUnverified) => "Listado",
                        (_, ExecutionSupport::ProviderAdvertisedUnverified) => "Anunciado",
                        _ => "Sin verificar",
                    },
                };
                let item = DropdownMenuItem::new(
                    format!("composer_route:{}", route.route_id),
                    &route.model_name,
                )
                .icon(icon_for_provider(&route.provider_id))
                .selected(
                    route.backend == props.selected_backend
                        && route.model_id == props.selected_model
                        && (route.backend != "native"
                            || route.provider_id == props.native_provider_id),
                );
                let item = if status.is_empty() {
                    item
                } else {
                    item.annotation(status)
                };
                entries.push(if route.selectable() {
                    item.into()
                } else {
                    item.disabled().into()
                });
                visible_models += 1;
            }
        }
        if visible_models == 0 {
            entries.push(DropdownMenuEntry::Custom(
                Element::text(if terms.is_empty() {
                    "No hay modelos disponibles"
                } else {
                    "Sin resultados. Prueba otro nombre."
                })
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(selector_palette().muted)
                .padding(8.0)
                .size(Some(inner_width), Some(34.0)),
            ));
        }
        return entries;
    }
    if props.advanced_open {
        return vec![DropdownMenuEntry::Custom(effort_panel(props, inner_width))];
    }

    let mut entries = Vec::new();
    if !props.model_options.is_empty() || !props.model_routes.is_empty() {
        entries.push(DropdownMenuEntry::Custom(selector_row(
            "composer_model_list_toggle",
            "Modelo",
            if props.model_label.is_empty() {
                props.selected_model
            } else {
                props.model_label
            },
            inner_width,
        )));
    }
    let has_effort = props.effort_options.iter().any(|(id, _)| id != "default");
    if has_effort {
        entries.push(DropdownMenuEntry::Custom(selector_row(
            "composer_advanced_toggle",
            "Esfuerzo",
            if props.selected_effort == "default" {
                "Automático"
            } else {
                props.effort_label
            },
            inner_width,
        )));
    }
    // The reference has a Speed row. Until a selected route exposes an actual
    // speed capability, show its place in the layout without a fake action.
    entries.push(DropdownMenuEntry::Custom(selector_disabled_row(
        "Velocidad",
        "No disponible",
        inner_width,
    )));
    if has_effort {
        entries.push(DropdownMenuEntry::Separator);
        entries.push(DropdownMenuEntry::Custom(advanced_chip(inner_width)));
    }
    entries
}

fn model_effort_group(props: &ComposerProps<'_>, compact: bool) -> Option<Element> {
    let model = if !props.model_label.is_empty() {
        props.model_label
    } else if props.selected_backend == "native" {
        props.model
    } else {
        ""
    };
    let preview = props.effort_preview.and_then(|_| preview_effort(props));
    let visible_effort = preview
        .map(|(_, _, label)| label)
        .unwrap_or(props.effort_label);
    let label = match (model.is_empty(), visible_effort.is_empty()) {
        (true, true) => return None,
        (false, true) => model.to_owned(),
        (true, false) => visible_effort.to_owned(),
        (false, false) => format!("{model} {visible_effort}"),
    };
    // Reserve a usable editor even at the width where the microphone appears.
    // The pill has 8 px padding on each side and two 8 px outer gaps.
    let other_actions = if props.width >= 380.0 {
        36.0 + 36.0 + 2.0 * 7.0
    } else {
        36.0 + 7.0
    };
    let max_width = (props.width - 16.0 - 36.0 - 16.0 - 80.0 - other_actions).clamp(0.0, 250.0);
    if max_width < 106.0 {
        return None;
    }
    let width = 250.0_f32.min(max_width);
    if props.model_options.is_empty()
        && props.model_routes.is_empty()
        && props.effort_options.is_empty()
    {
        return Some(
            Element::truncated_text(label)
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .foreground(MUTED_INK)
                .padding(8.0)
                .size(Some(width), Some(CONTROL_HEIGHT)),
        );
    }
    let open = (props.model_menu_open || props.effort_menu_open) && !props.running;
    let effort_width = if visible_effort.is_empty() {
        0.0
    } else {
        (visible_effort.chars().count() as f32 * 7.0 + 4.0).min(width * 0.38)
    };
    let effort_color = if let Some((index, count, _)) = preview {
        effort_tone(index, count)
    } else if props.selected_effort == "default" {
        SELECTOR_MUTED
    } else if let Some((index, count, _)) = preview_effort(props) {
        effort_tone(index, count)
    } else {
        SELECTOR_MUTED
    };
    let provider_id = props
        .model_routes
        .iter()
        .find(|route| {
            route.backend == props.selected_backend
                && route.model_id == props.selected_model
                && (route.backend != "native" || route.provider_id == props.native_provider_id)
        })
        .map_or_else(
            || {
                if props.selected_backend == "native" {
                    props.native_provider_id
                } else {
                    props.selected_backend
                }
            },
            |route| route.provider_id.as_str(),
        );
    let visual = Element::row(vec![
        Element::icon(icon_for_provider(provider_id))
            .foreground(SELECTOR_INK)
            .size(Some(18.0), Some(18.0)),
        Element::truncated_text(model)
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .foreground(SELECTOR_INK)
            .size(Some((width - 74.0 - effort_width).max(0.0)), Some(18.0)),
        Element::truncated_text(visible_effort)
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .foreground(effort_color)
            .size(Some(effort_width), Some(18.0)),
        Element::icon(Icon::ChevronDown)
            .foreground(SELECTOR_MUTED)
            .size(Some(12.0), Some(18.0)),
    ])
    .gap(4.0)
    .size(Some(width - 28.0), Some(18.0));
    let trigger = if props.running {
        Element::panel(visual)
    } else {
        Element::clickable("composer_model_toggle", visual)
    }
    .background(if open {
        SELECTOR_PILL_HOVER
    } else {
        SELECTOR_PILL
    })
    .hover_background(SELECTOR_PILL_HOVER)
    .pressed_background(SELECTOR_PILL_HOVER)
    .transition_ms(140)
    .padding(14.0)
    .radius(SELECTOR_HEIGHT / 2.0)
    .size(Some(width), Some(SELECTOR_HEIGHT));
    let trigger = Tooltip::new(
        trigger,
        "composer_model_toggle",
        "Elegir modelo y esfuerzo",
        theme::component_palette(),
    )
    .shortcut("Ctrl+Shift+M")
    .render();
    let menu_width = if props.model_list_open {
        (props.width - 16.0).clamp(205.0, 340.0)
    } else if compact {
        (props.width - 16.0).clamp(205.0, 250.0)
    } else {
        250.0
    };
    let max_visible_items = if compact { 6 } else { 8 };
    let mut menu_entries = model_effort_entries(props, menu_width);
    if props.model_list_open && menu_entries.len() > max_visible_items {
        menu_entries = model_effort_entries(props, menu_width - MODEL_MENU_SCROLL_GUTTER);
    }
    let mut menu = DropdownMenu::new(
        trigger,
        menu_entries,
        open,
        "composer_model_dismiss",
        selector_palette(),
    )
    .width(menu_width)
    .placement(PopoverPlacement::TopEnd)
    .motion(MenuMotion {
        open_ms: 220,
        item_ms: 0,
    })
    .close_animation_ms(150)
    .open_scale(0.96)
    .view_key(if props.model_list_open {
        "models"
    } else if props.advanced_open {
        "advanced"
    } else {
        "main"
    })
    .max_visible_items(max_visible_items)
    .render();
    if let ElementKind::Popover { floating, .. } = &mut menu.kind {
        floating.style.radius = 16.0;
    }
    Some(menu)
}

fn send_control(props: &ComposerProps<'_>) -> Element {
    let surface = if props.running {
        theme::SELECTED
    } else {
        SEND_BLUE
    };
    let mut palette = theme::action_palette(surface, Color::rgb(255, 255, 255));
    palette.hover = if props.running {
        theme::hover_for(surface)
    } else {
        Color::rgb(52, 128, 255)
    };
    palette.selected = surface;
    let trigger = IconButton::new(
        "run_toggle",
        if props.running {
            Icon::Stop
        } else {
            Icon::ArrowUp
        },
        palette,
    )
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Regular)
    .render()
    .size(Some(CONTROL_HEIGHT), Some(CONTROL_HEIGHT))
    .radius(CONTROL_HEIGHT / 2.0)
    .transition_ms(150);
    let tooltip = Tooltip::new(
        trigger,
        "run_toggle",
        if props.running { "Detener" } else { "Enviar" },
        theme::component_palette(),
    );
    if props.running {
        tooltip.render()
    } else {
        tooltip.shortcut("Ctrl+Enter").render()
    }
}

fn steer_control(props: &ComposerProps<'_>) -> Element {
    Tooltip::new(
        IconButton::new(
            "run_steer",
            Icon::ArrowUp,
            theme::action_palette(SEND_BLUE, Color::rgb(255, 255, 255)),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Regular)
        .enabled(!props.prompt.trim().is_empty() || !props.attachments.is_empty())
        .render()
        .size(Some(CONTROL_HEIGHT), Some(CONTROL_HEIGHT))
        .radius(CONTROL_HEIGHT / 2.0),
        "run_steer",
        "Añadir instrucción al turno activo",
        theme::component_palette(),
    )
    .render()
}

fn microphone_control(props: &ComposerProps<'_>) -> Element {
    Tooltip::new(
        IconButton::new(
            "composer_microphone",
            Icon::Microphone,
            theme::action_palette(
                if props.listening {
                    SEND_BLUE
                } else {
                    PILL_SURFACE
                },
                Color::rgb(240, 240, 240),
            ),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Regular)
        .render()
        .border(
            if props.listening {
                SEND_BLUE
            } else {
                Color::rgb(68, 68, 68)
            },
            1.0,
        )
        .size(Some(CONTROL_HEIGHT), Some(CONTROL_HEIGHT))
        .radius(CONTROL_HEIGHT / 2.0),
        "composer_microphone",
        if props.listening {
            "Detener dictado"
        } else {
            "Dictar mensaje"
        },
        theme::component_palette(),
    )
    .render()
}

fn trailing_actions(props: &ComposerProps<'_>) -> Element {
    let compact = props.width < COMPACT_CONTEXT_WIDTH;
    let mut row = Vec::new();
    if props.width >= 260.0 {
        if let Some(model) = model_effort_group(props, compact) {
            row.push(model);
        }
    }
    if props.width >= 380.0 {
        row.push(microphone_control(props));
    }
    if props.running && props.selected_backend == "native" {
        row.push(steer_control(props));
    }
    row.push(send_control(props));
    Element::row(row).gap(7.0).size(None, Some(CONTROL_HEIGHT))
}

fn attachment_rows(props: &ComposerProps<'_>) -> Vec<Element> {
    let per_row = if props.width >= 650.0 {
        3
    } else if props.width >= 390.0 {
        2
    } else {
        1
    };
    let inner_width = (props.width
        - if props.width < COMPACT_CONTEXT_WIDTH {
            20.0
        } else {
            26.0
        })
    .max(80.0);
    props
        .attachments
        .chunks(per_row)
        .enumerate()
        .map(|(row_index, chunk)| {
            let item_width = ((inner_width - 6.0 * (per_row.saturating_sub(1) as f32))
                / per_row as f32)
                .min(220.0);
            let chips = chunk
                .iter()
                .enumerate()
                .map(|(column_index, attachment)| {
                    let index = row_index * per_row + column_index;
                    let id = format!("composer_attachment_remove:{index}");
                    let chip = Button::new(
                        &id,
                        &attachment.label,
                        theme::action_palette(theme::PANEL, theme::FOREGROUND),
                    )
                    .icon(if attachment.is_dir {
                        Icon::Folder
                    } else {
                        Icon::Code
                    })
                    .trailing_icon(Icon::Close)
                    .variant(ButtonVariant::Subtle)
                    .size(ControlSize::Compact)
                    .width(item_width)
                    .hover_ink(theme::FOREGROUND)
                    .render()
                    .border(theme::BORDER, 1.0)
                    .radius(8.0)
                    .size(Some(item_width), Some(30.0));
                    Tooltip::new(
                        chip,
                        &id,
                        format!("Quitar {}", attachment.path),
                        theme::component_palette(),
                    )
                    .render()
                })
                .collect();
            Element::row(chips).gap(6.0).size(None, Some(30.0))
        })
        .collect()
}

/// Builds one responsive composer. Visual labels come from the actual backend
/// and its supported options; unsupported controls are omitted.
pub(crate) fn build_composer(props: &ComposerProps<'_>) -> Element {
    let compact = props.width < COMPACT_CONTEXT_WIDTH;
    let mut palette = theme::component_palette();
    palette.selected = PILL_SURFACE;
    Composer::new(
        "prompt",
        props.prompt,
        props.placeholder,
        props.width,
        palette,
    )
    .editor_height(editor_height(
        props.prompt,
        props.width,
        compact,
        props.home,
    ))
    .compact(compact)
    .context(project_metadata(props))
    .context_height(CONTEXT_HEIGHT)
    .attachments(attachment_rows(props))
    .pill_controls(add_context_control(props), trailing_actions(props))
    .border_color(PILL_SURFACE)
    .shadow_color(Color::TRANSPARENT)
    .card_radius(CARD_RADIUS)
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer_selection::RouteSource;
    use openide_ui::{DrawCommand, HitKind, Point, Size, UiAction, UiEvent, UiTree};

    fn props<'a>(prompt: &'a str, width: f32) -> ComposerProps<'a> {
        ComposerProps {
            workspace: "C:/projects/openIDE",
            project_action: None,
            project_branch: Some("main"),
            project_menu_open: false,
            project_options: &[],
            prompt,
            attachments: &[],
            add_menu_open: false,
            listening: false,
            placeholder: "Ask me anything",
            selected_backend: "native",
            native_provider_id: "openai",
            backend_options: &[],
            backend_menu_open: false,
            model: "gpt-6-sol",
            model_label: "GPT-6 Sol",
            model_options: &[],
            model_routes: &[],
            provider_status: None,
            selected_model: "gpt-6-sol",
            model_menu_open: false,
            model_list_open: false,
            model_query: "",
            advanced_open: false,
            effort_label: "",
            effort_options: &[],
            selected_effort: "default",
            effort_menu_open: false,
            effort_preview: None,
            turn_mode: TurnMode::Build,
            running: false,
            workspace_writes_enabled: false,
            status: None,
            context_percent: None,
            goal: None,
            goal_editor_open: false,
            goal_draft: "",
            width,
            home: true,
        }
    }

    #[test]
    fn pill_is_above_metadata_and_contains_all_primary_controls() {
        let frame = UiTree::new(build_composer(&props("", 620.0))).layout(Size::new(620.0, 420.0));
        let rect = |id: &str| frame.regions.iter().find(|r| r.id == id).unwrap().rect;
        let editor = rect("prompt");
        let project = rect("composer_project_toggle");
        let plus = rect("composer_add_context");
        let mic = rect("composer_microphone");
        let send = rect("run_toggle");
        assert!(editor.y < project.y);
        assert!(plus.x < editor.x && editor.x < mic.x && mic.x < send.x);
        assert_eq!(plus.width, 36.0);
        assert_eq!(send.width, 36.0);
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, radius, .. }
                if *color == PILL_SURFACE && *radius == CARD_RADIUS
        )));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == SEND_BLUE
        )));
    }

    #[test]
    fn add_menu_exposes_real_actions_and_goal_editor() {
        let mut state = props("", 620.0);
        state.add_menu_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 600.0));
        for id in [
            "composer_add_files",
            "composer_goal_open",
            "turn_mode_toggle",
            "composer_plugin_documents",
            "composer_plugin_spreadsheets",
            "composer_plugin_presentations",
            "composer_plugin_code_blocks",
        ] {
            assert!(
                frame.regions.iter().any(|region| region.id == id),
                "missing {id}"
            );
        }
        state.goal_editor_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 600.0));
        for id in [
            "composer_goal_draft",
            "composer_goal_save",
            "composer_goal_clear",
        ] {
            assert!(
                frame.regions.iter().any(|region| region.id == id),
                "missing {id}"
            );
        }
    }

    #[test]
    fn project_menu_is_anchored_without_moving_pill() {
        let projects = [
            ("C:/projects/openIDE".into(), "openIDE".into()),
            ("C:/projects/other".into(), "other".into()),
        ];
        let mut state = props("", 620.0);
        state.project_options = &projects;
        let closed = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 420.0));
        let closed_prompt = closed
            .regions
            .iter()
            .find(|r| r.id == "prompt")
            .unwrap()
            .rect;
        state.project_menu_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        let open = tree.layout(Size::new(620.0, 420.0));
        assert_eq!(
            closed_prompt,
            open.regions.iter().find(|r| r.id == "prompt").unwrap().rect
        );
        let option = open
            .regions
            .iter()
            .find(|r| r.id == "composer_project_select:1")
            .unwrap()
            .rect;
        let point = Point::new(
            option.x + option.width / 2.0,
            option.y + option.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "composer_project_select:1".into()
            }]
        );
    }

    #[test]
    fn attachment_and_context_meter_reflect_supplied_state() {
        let attachments = [ComposerAttachmentView {
            path: "src/main.rs".into(),
            label: "main.rs".into(),
            is_dir: false,
        }];
        let mut state = props("", 780.0);
        state.attachments = &attachments;
        state.context_percent = Some(57);
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(780.0, 420.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_attachment_remove:0"));
        assert!(frame.commands.iter().any(|command| matches!(
            command, DrawCommand::Text { text, .. } if text == "57%"
        )));
    }

    #[test]
    fn narrow_pill_keeps_editor_and_send_inside_bounds() {
        for width in [320.0, 410.0, 780.0] {
            let frame =
                UiTree::new(build_composer(&props("hello", width))).layout(Size::new(width, 420.0));
            for id in ["composer_add_context", "prompt", "run_toggle"] {
                let rect = frame.regions.iter().find(|r| r.id == id).unwrap().rect;
                assert!(
                    rect.x >= 0.0 && rect.x + rect.width <= width + 0.1,
                    "{id} outside {width}: {rect:?}"
                );
            }
        }
    }

    #[test]
    fn native_running_composer_offers_steer_and_stop_at_narrow_width() {
        let mut state = props("Change direction", 320.0);
        state.running = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(320.0, 420.0));
        for id in ["run_steer", "run_toggle"] {
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect;
            assert!(
                rect.x >= 0.0 && rect.x + rect.width <= 320.1,
                "{id}: {rect:?}"
            );
        }
    }

    #[test]
    fn long_model_keeps_controls_inside_pill_at_compact_widths() {
        let models = [(
            "long-model".into(),
            "Un modelo con un nombre muy largo".into(),
        )];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("high".into(), "Extremadamente alto".into()),
        ];
        for width in [320.0, 410.0] {
            let mut state = props("hello", width);
            state.model_label = "Un modelo con un nombre muy largo";
            state.model_options = &models;
            state.effort_label = "Extremadamente alto";
            state.effort_options = &efforts;
            state.selected_effort = "high";
            let frame = UiTree::new(build_composer(&state)).layout(Size::new(width, 420.0));
            let pill = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect {
                        rect,
                        color,
                        radius,
                    } if *color == PILL_SURFACE && *radius == CARD_RADIUS => Some(*rect),
                    _ => None,
                })
                .unwrap();
            let controls = if width >= 380.0 {
                &[
                    "composer_add_context",
                    "prompt",
                    "composer_model_toggle",
                    "composer_microphone",
                    "run_toggle",
                ][..]
            } else {
                &[
                    "composer_add_context",
                    "prompt",
                    "composer_model_toggle",
                    "run_toggle",
                ][..]
            };
            for id in controls {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == *id)
                    .unwrap()
                    .rect;
                assert!(
                    rect.x >= pill.x - 0.1 && rect.x + rect.width <= pill.x + pill.width + 0.1,
                    "{id} outside pill at {width}: {rect:?}, pill {pill:?}"
                );
            }
            let editor = frame
                .regions
                .iter()
                .find(|region| region.id == "prompt")
                .unwrap();
            assert!(editor.rect.width >= 80.0 - 0.1);
        }
    }

    #[test]
    fn metadata_hides_optional_items_before_overflowing() {
        for width in [320.0, 410.0, 490.0, 650.0] {
            let mut state = props("hello", width);
            state.workspace = "C:/projects/a-very-long-project-directory-name";
            state.project_branch = Some("a-very-long-feature-branch-name");
            state.context_percent = Some(57);
            state.goal = Some("A very long goal for the current project");
            let frame = UiTree::new(build_composer(&state)).layout(Size::new(width, 420.0));
            for id in [
                "composer_project_toggle",
                "backend_menu_toggle",
                "composer_context_percent",
            ] {
                if let Some(region) = frame.regions.iter().find(|region| region.id == id) {
                    assert!(
                        region.rect.x >= -0.1 && region.rect.x + region.rect.width <= width + 0.1,
                        "{id} outside {width}: {:?}",
                        region.rect
                    );
                }
            }
        }
    }

    #[test]
    fn growing_editor_keeps_round_controls_on_bottom_row() {
        let draft = "first line\nsecond line\nthird line\nfourth line";
        let frame =
            UiTree::new(build_composer(&props(draft, 620.0))).layout(Size::new(620.0, 500.0));
        let rect = |id: &str| frame.regions.iter().find(|r| r.id == id).unwrap().rect;
        let editor = rect("prompt");
        let plus = rect("composer_add_context");
        let mic = rect("composer_microphone");
        let send = rect("run_toggle");
        assert!(plus.y >= editor.y + editor.height);
        for control in [plus, mic, send] {
            assert_eq!(control.width, 36.0);
            assert_eq!(control.height, 36.0);
            assert_eq!(control.y, plus.y);
        }
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, radius }
                if *color == Color::rgb(64, 64, 64)
                    && *radius == 18.0
                    && rect.width == rect.height
        )));
    }

    #[test]
    fn model_menu_has_main_advanced_and_dynamic_route_views() {
        let models = [
            ("gpt-6-sol".into(), "GPT-6 Sol".into()),
            ("gpt-6-astra".into(), "GPT-6 Astra".into()),
        ];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("medium".into(), "Medio".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        state.effort_options = &efforts;
        state.selected_effort = "medium";
        state.effort_label = "Medio";
        state.model_menu_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_effort_slider"));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_model_list_toggle"));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_advanced_toggle"));
        assert!(!frame.regions.iter().any(|r| r.id.contains("speed")));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_model:gpt-6-astra"));

        state.advanced_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_effort_slider"));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_advanced_back"));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_model_list_toggle"));

        state.advanced_open = false;
        state.model_list_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_effort_slider"));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_model:gpt-6-astra"));
    }

    #[test]
    fn model_search_filters_by_provider_and_model_and_keeps_refresh() {
        let routes = [
            ("openai", "OpenAI", "gpt-6-sol", "GPT-6 Sol"),
            ("anthropic", "Anthropic", "claude-haiku", "Claude Haiku"),
        ]
        .map(
            |(provider_id, provider_name, model_id, model_name)| ModelRouteOption {
                route_id: format!("native:{provider_id}:{model_id}"),
                backend: "native".into(),
                provider_id: provider_id.into(),
                provider_name: provider_name.into(),
                model_id: model_id.into(),
                model_name: model_name.into(),
                credential: CredentialAvailability::NoKeyRequired,
                execution: ExecutionSupport::Verified,
                source: RouteSource::AdapterReview,
                catalog_present: true,
                provider_listed: None,
                effort_options: Vec::new(),
            },
        );
        let mut state = props("", 620.0);
        state.model_routes = &routes;
        state.model_menu_open = true;
        state.model_list_open = true;
        state.model_query = "ÁNTHRÓPIC haiku";
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        for id in [
            "composer_model_search",
            "composer_routes_refresh",
            "composer_route:native:anthropic:claude-haiku",
        ] {
            assert!(frame.regions.iter().any(|region| region.id == id), "{id}");
        }
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "composer_route:native:openai:gpt-6-sol"));

        state.model_query = "sin coincidencias";
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_routes_refresh"));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.contains("Sin resultados")
        )));
    }

    #[test]
    fn selector_views_and_actions_fit_compact_viewport() {
        let models = [
            ("gpt-6-sol".into(), "GPT-6 Sol".into()),
            ("gpt-6-astra".into(), "GPT-6 Astra".into()),
        ];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let width = 320.0;
        let viewport = Size::new(width, 520.0);
        let mut state = props("", width);
        state.model_options = &models;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        let cases = [
            (false, false, "composer_advanced_toggle"),
            (true, false, "composer_effort_slider"),
            (false, true, "composer_model:gpt-6-astra"),
        ];
        for (advanced, model_list, expected_id) in cases {
            state.advanced_open = advanced;
            state.model_list_open = model_list;
            let frame = UiTree::new(build_composer(&state)).layout(viewport);
            let surface = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:composer_model_dismiss")
                .expect("selector surface")
                .rect;
            assert!(surface.x >= -0.1);
            assert!(surface.x + surface.width <= width + 0.1);
            assert!(surface.y >= -0.1);
            assert!(surface.y + surface.height <= viewport.height + 0.1);
            let trigger = frame
                .regions
                .iter()
                .find(|region| region.id == "composer_model_toggle")
                .expect("selector trigger")
                .rect;
            assert!(trigger.x >= -0.1 && trigger.x + trigger.width <= width + 0.1);
            let action = frame
                .regions
                .iter()
                .find(|region| region.id == expected_id)
                .unwrap_or_else(|| panic!("missing {expected_id} in compact selector"))
                .rect;
            assert!(action.x >= surface.x - 0.1);
            assert!(action.x + action.width <= surface.x + surface.width + 0.1);
        }
    }

    #[test]
    fn selector_main_view_emits_real_navigation_and_no_speed_action() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        // Inspect the settled card geometry; the opening animation scales
        // its corner radius from 0.96 on the first frame.
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(620.0, 520.0));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { color, radius, .. }
                if color.r == SELECTOR_CARD.r && color.g == SELECTOR_CARD.g
                    && color.b == SELECTOR_CARD.b && *radius == 16.0
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "No disponible"
        )));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.contains("speed")));
        let advanced = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_advanced_toggle")
            .expect("advanced action")
            .rect;
        let point = Point::new(
            advanced.x + advanced.width / 2.0,
            advanced.y + advanced.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "composer_advanced_toggle".into()
            }]
        );
    }

    #[test]
    fn main_selector_only_highlights_the_control_under_the_pointer() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        tree.set_reduced_motion(true);
        let viewport = Size::new(620.0, 520.0);
        let frame = tree.layout(viewport);
        let effort = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_advanced_toggle")
            .expect("effort row")
            .rect;
        let chip = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_advanced_chip_toggle")
            .expect("advanced chip")
            .rect;
        assert_ne!(effort, chip);
        let center = |rect: openide_ui::Rect| {
            Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
        };
        for (pointer, expected_id, highlighted, idle) in [
            (center(effort), "composer_advanced_toggle", effort, chip),
            (center(chip), "composer_advanced_chip_toggle", chip, effort),
        ] {
            tree.handle_event(UiEvent::PointerMove(pointer));
            assert_eq!(tree.hovered_id(), Some(expected_id));
            let frame = tree.layout(viewport);
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color, .. }
                    if *rect == highlighted && *color == SELECTOR_PILL
            )));
            assert!(!frame.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color, .. }
                    if *rect == idle && *color == SELECTOR_PILL
            )));
        }
    }

    #[test]
    fn effort_drag_previews_nearest_real_tier_without_persisting() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("medium".into(), "Medio".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        state.effort_options = &efforts;
        state.selected_effort = "low";
        state.effort_label = "Bajo";
        state.model_menu_open = true;
        state.advanced_open = true;
        state.effort_preview = Some(1.0);
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 520.0));
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "Alto"
            )),
            "pill should preview Alto"
        );
        for expected in ["Avanzado", "Auto"] {
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == expected
                )),
                "drag should keep {expected} in the advanced header"
            );
        }
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_advanced_back"));
        assert_eq!(
            state.selected_effort, "low",
            "drag preview must not persist"
        );
    }

    #[test]
    fn selector_surface_and_labels_follow_dark_theme() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        let frame = {
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(620.0, 520.0))
        };
        let surface = frame
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:composer_model_dismiss")
            .expect("selector surface")
            .rect;
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. }
                if *rect == surface && *color == theme::MENU_SURFACE
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. }
                if text == "Modelo" && *color == theme::FOREGROUND
        )));
        state.advanced_open = true;
        let advanced = {
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(620.0, 520.0))
        };
        assert!(advanced.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. }
                if text == "Avanzado" && *color == theme::FOREGROUND
        )));
        assert!(advanced.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, font_size, .. } if text == "Auto" && *font_size == 14.0
        )));
        assert!(!advanced.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.starts_with("Automát")
        )));
        let surface = advanced
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:composer_model_dismiss")
            .expect("advanced surface")
            .rect;
        let auto = advanced
            .regions
            .iter()
            .find(|region| region.id == "composer_effort:default")
            .expect("auto action")
            .rect;
        assert!(auto.x + auto.width <= surface.x + surface.width + 0.1);
        let header_text_y = |label: &str| {
            advanced.commands.iter().find_map(|command| match command {
                DrawCommand::Text { text, position, .. }
                    if text == label
                        && position.y >= surface.y
                        && position.y < surface.y + 36.0 =>
                {
                    Some(position.y)
                }
                _ => None,
            })
        };
        let advanced_y = header_text_y("Avanzado").expect("advanced label");
        let level_y = header_text_y("Bajo").expect("effort level");
        let auto_y = header_text_y("Auto").expect("auto label");
        assert!(
            (advanced_y - level_y).abs() <= 1.0,
            "advanced={advanced_y}, level={level_y}, auto={auto_y}"
        );
        assert!(
            (advanced_y - auto_y).abs() <= 1.0,
            "advanced={advanced_y}, level={level_y}, auto={auto_y}"
        );
    }

    #[test]
    fn codex_account_status_appears_once_in_provider_heading() {
        let routes = ["gpt-6-sol", "gpt-6-astra"].map(|model_id| ModelRouteOption {
            route_id: format!("codex:codex:{model_id}"),
            backend: "codex".into(),
            provider_id: "codex".into(),
            provider_name: "Codex CLI".into(),
            model_id: model_id.into(),
            model_name: model_id.into(),
            credential: CredentialAvailability::ExternalCliManaged,
            execution: ExecutionSupport::DelegatedCli,
            source: RouteSource::CliLiveCatalog,
            catalog_present: false,
            provider_listed: Some(true),
            effort_options: Vec::new(),
        });
        let mut state = props("", 620.0);
        state.model_routes = &routes;
        state.model_list_open = true;
        let entries = model_effort_entries(&state, 340.0);
        assert_eq!(
            entries
                .iter()
                .filter(|entry| matches!(entry,
                    DropdownMenuEntry::Heading(text) if text == "Codex CLI · En tu cuenta"
                ))
                .count(),
            1
        );
        assert!(entries
            .iter()
            .filter_map(|entry| match entry {
                DropdownMenuEntry::Item(item) if item.action_id.starts_with("composer_route:") => {
                    Some(item)
                }
                _ => None,
            })
            .all(|item| item.annotation.is_none()));
    }

    #[test]
    fn model_search_and_rows_clear_the_scrollbar_rail() {
        let models: Vec<(String, String)> = (0..12)
            .map(|index| (format!("model-{index}"), format!("Model {index}")))
            .collect();
        let mut state = props("", 620.0);
        state.model_options = &models;
        state.model_menu_open = true;
        state.model_list_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(620.0, 530.0));
        let scrollbar = frame
            .regions
            .iter()
            .find(|region| region.kind == HitKind::Scrollbar)
            .expect("model list scrollbar")
            .rect;
        for id in ["composer_model_search", "composer_model:model-0"] {
            let content = frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
                .rect;
            assert!(
                content.x + content.width <= scrollbar.x + 0.1,
                "{id} overlaps the scrollbar"
            );
        }
    }

    #[test]
    fn advanced_header_stays_stable_and_only_real_ultra_is_violet() {
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
            ("ultra".into(), "Ultra".into()),
        ];
        let mut state = props("", 620.0);
        state.effort_options = &efforts;
        state.selected_effort = "high";
        state.model_menu_open = true;
        state.advanced_open = true;

        let settled_frame = |state: &ComposerProps<'_>| {
            let mut tree = UiTree::new(build_composer(state));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(620.0, 520.0))
        };
        let idle = settled_frame(&state);
        assert!(idle
            .regions
            .iter()
            .any(|region| region.id == "composer_advanced_back"));
        assert!(!idle.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. }
                if text == "Ultra" && *color == theme::EFFORT_ULTRA
        )));

        assert!(idle.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Alto"
        )));
        assert!(!idle.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Más rápido" || text == "Más capaz"
        )));

        state.effort_preview = Some(1.0);
        let ultra = settled_frame(&state);
        assert!(ultra.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. }
                if text == "Ultra" && *color == theme::EFFORT_ULTRA
        )));
        assert!(!ultra.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.contains("usage limits")
        )));
    }

    #[test]
    fn selector_popover_uses_reference_motion_and_stable_view_keys() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        for (models, advanced, expected_key) in [
            (false, false, "main"),
            (true, false, "models"),
            (false, true, "advanced"),
        ] {
            state.model_list_open = models;
            state.advanced_open = advanced;
            let selector = model_effort_group(&state, false).expect("selector");
            match selector.kind {
                ElementKind::Popover {
                    animation_ms,
                    close_animation_ms,
                    open_scale,
                    view_key,
                    ..
                } => {
                    assert_eq!(animation_ms, 220);
                    assert_eq!(close_animation_ms, Some(150));
                    assert!((open_scale - 0.96).abs() < f32::EPSILON);
                    assert_eq!(view_key.as_deref(), Some(expected_key));
                }
                _ => panic!("model selector must render as a popover"),
            }
        }
    }

    #[test]
    fn disconnected_provider_shows_one_connection_action_instead_of_models() {
        let routes = ["gpt-6-sol", "gpt-6-astra"].map(|model_id| ModelRouteOption {
            route_id: format!("native:openai:{model_id}"),
            backend: "native".into(),
            provider_id: "openai".into(),
            provider_name: "OpenAI".into(),
            model_id: model_id.into(),
            model_name: model_id.into(),
            credential: CredentialAvailability::MissingCredential,
            execution: ExecutionSupport::Verified,
            source: RouteSource::AdapterReview,
            catalog_present: false,
            provider_listed: None,
            effort_options: Vec::new(),
        });
        let mut state = props("", 620.0);
        state.model_routes = &routes;
        state.model_menu_open = true;
        state.model_list_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_route:native:openai:gpt-6-sol"));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_route:native:openai:gpt-6-astra"));

        let mut provider_status = BTreeMap::new();
        state.provider_status = None;
        provider_status.insert("openai".to_owned(), ProviderModelStatus::Checking);
        state.provider_status = Some(&provider_status);
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text.contains("Consultando OpenAI")
        )));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id.starts_with("composer_route:")));

        let failed_status =
            BTreeMap::from([("openai".to_owned(), ProviderModelStatus::CheckFailed)]);
        state.provider_status = Some(&failed_status);
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_route:native:openai:gpt-6-sol"));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_route:native:openai:gpt-6-astra"));
    }
}
