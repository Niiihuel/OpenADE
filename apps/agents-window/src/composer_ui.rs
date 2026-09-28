//! Shared composer for the empty task and an active conversation.
//!
//! The project context is a separate surface above the editor. The editing
//! card owns the input and actions, with stable hit targets across widths and
//! only state-driven hover/focus transitions (no idle repaint loop).

use std::path::Path;

use openide_ui::{
    Button, ButtonVariant, Color, Component, Composer, ControlSize, DropdownList, DropdownMenu,
    DropdownMenuEntry, DropdownMenuItem, DropdownOption, Element, FontFamily, Icon, IconButton,
    MenuMotion, Popover, PopoverPlacement, ScrollArea, Tooltip,
};

use crate::{
    theme,
    view::{ComposerAttachmentView, ViewState},
};

const COMPACT_CONTEXT_WIDTH: f32 = 490.0;
const EDITOR_LINE_HEIGHT: f32 = 20.0;
const CARD_RADIUS: f32 = 25.0;
const CONTROL_HEIGHT: f32 = 38.0;
const SELECTOR_HEIGHT: f32 = 40.0;
const CONTEXT_INSET: f32 = 12.0;
const CONTEXT_HEIGHT: f32 = 44.0;
const CONTEXT_RADIUS: f32 = 17.0;

#[derive(Clone, Copy)]
pub(crate) struct ComposerProps<'a> {
    pub workspace: &'a str,
    pub project_action: Option<&'a str>,
    pub project_branch: Option<&'a str>,
    pub project_menu_open: bool,
    pub project_options: &'a [(String, String)],
    /// Execution environment shown beside the project, independent of agent CLI.
    pub environment_label: &'a str,
    pub prompt: &'a str,
    pub attachments: &'a [ComposerAttachmentView],
    pub add_menu_open: bool,
    pub placeholder: &'a str,
    pub selected_backend: &'a str,
    /// Configured native model, retained for native backends that do not expose
    /// a model catalogue. CLI model choices come from the matching profile.
    pub model: &'a str,
    pub model_label: &'a str,
    pub model_options: &'a [(String, String)],
    pub selected_model: &'a str,
    pub model_menu_open: bool,
    pub effort_label: &'a str,
    pub effort_options: &'a [(String, String)],
    pub selected_effort: &'a str,
    pub effort_menu_open: bool,
    pub running: bool,
    pub workspace_writes_enabled: bool,
    pub status: Option<&'a str>,
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
            environment_label: "Local",
            prompt: &state.prompt,
            attachments: &state.composer_attachments,
            add_menu_open: state.composer_add_menu_open,
            placeholder,
            selected_backend: &state.selected_backend,
            model: &state.model,
            model_label,
            model_options: &state.composer_model_options,
            selected_model: &state.composer_model,
            model_menu_open: state.composer_model_menu_open,
            effort_label,
            effort_options: &state.composer_effort_options,
            selected_effort: &state.composer_effort,
            effort_menu_open: state.composer_effort_menu_open,
            running: state.running || state.background_run_selected,
            workspace_writes_enabled: state.workspace_writes_enabled,
            status,
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
    let chars_per_line = ((width - 58.0).max(80.0) / 7.5).floor().max(1.0) as usize;
    let lines = prompt
        .split('\n')
        .map(|line| line.chars().count().max(1).div_ceil(chars_per_line))
        .sum::<usize>();
    let minimum = if home { 56.0 } else { 64.0 };
    let maximum = if compact { 152.0 } else { 216.0 };
    (lines as f32 * EDITOR_LINE_HEIGHT + 28.0).clamp(minimum, maximum)
}

fn metadata_item(icon: Icon, value: &str, max_label_width: f32) -> Element {
    let label_width = (value.chars().count() as f32 * 7.2).clamp(28.0, max_label_width);
    Element::row(vec![
        Element::icon(icon)
            .foreground(theme::FOREGROUND)
            .size(Some(16.0), Some(28.0)),
        Element::truncated_text(value)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .foreground(theme::FOREGROUND)
            .size(Some(label_width), Some(28.0)),
    ])
    .gap(8.0)
    .size(None, Some(28.0))
}

fn project_control(props: &ComposerProps<'_>, compact: bool) -> Element {
    let name = project_name(props.workspace);
    let natural_width = (name.chars().count() as f32 * 7.2 + 45.0).clamp(96.0, 230.0);
    let width = if compact {
        let reserved = if props.running { 105.0 } else { 16.0 };
        natural_width.min((props.width - reserved).max(60.0))
    } else {
        natural_width
    };
    let id = props.project_action.unwrap_or("composer_project_toggle");
    let trigger = Tooltip::new(
        Button::new(
            id,
            name,
            theme::action_palette(theme::PANEL, theme::FOREGROUND),
        )
        .icon(Icon::Folder)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .width(width)
        .active(props.project_menu_open)
        .enabled(!props.running)
        .hover_ink(theme::FOREGROUND)
        .render()
        .radius(9.0),
        id,
        props.workspace,
        theme::component_palette(),
    )
    .render();
    let mut options: Vec<_> = props
        .project_options
        .iter()
        .enumerate()
        .map(|(index, (path, label))| {
            DropdownOption::new(format!("composer_project_select:{index}"), label)
                .icon(Icon::Folder)
                .selected(path == props.workspace)
        })
        .collect();
    options
        .push(DropdownOption::new("composer_project_browse", "Elegir carpeta…").icon(Icon::Folder));
    options.push(
        DropdownOption::new("composer_project_configure", "Configurar proyecto")
            .icon(Icon::Settings),
    );
    DropdownList::new(
        trigger,
        options,
        props.project_menu_open,
        "composer_project_dismiss",
        theme::component_palette(),
    )
    .width((props.width - 20.0).clamp(180.0, 300.0))
    .placement(PopoverPlacement::BottomStart)
    .render()
}

fn project_metadata(props: &ComposerProps<'_>) -> Element {
    let compact = props.width < COMPACT_CONTEXT_WIDTH;
    let mut row = vec![project_control(props, compact)];
    if !compact && !props.environment_label.is_empty() {
        row.push(metadata_item(Icon::Laptop, props.environment_label, 72.0));
    }
    if !compact && props.width >= if props.running { 760.0 } else { 600.0 } {
        if let Some(branch) = props.project_branch.filter(|branch| !branch.is_empty()) {
            row.push(metadata_item(Icon::GitBranch, branch, 140.0));
        }
    }
    row.push(Element::spacer().flex(1.0));
    if props.running {
        let status = props
            .status
            .filter(|value| !value.is_empty())
            .unwrap_or("En curso");
        row.push(
            Element::truncated_text(status)
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(
                    Some(if compact {
                        68.0
                    } else if props.width < 720.0 {
                        100.0
                    } else {
                        160.0
                    }),
                    Some(28.0),
                ),
        );
    }
    Element::panel(Element::row(row).gap(if compact { 5.0 } else { 18.0 }))
        .background(theme::PANEL)
        .radius(CONTEXT_RADIUS)
        .padding(8.0)
        .size(
            Some((props.width - CONTEXT_INSET * 2.0).max(0.0)),
            Some(CONTEXT_HEIGHT),
        )
}

fn mode_control(props: &ComposerProps<'_>, two_rows: bool) -> Option<Element> {
    if props.selected_backend != "native" {
        return None;
    }
    let title = if props.workspace_writes_enabled {
        "Pedir permiso"
    } else {
        "Sólo lectura"
    };
    let label = if two_rows { "" } else { title };
    let width = if two_rows { 36.0 } else { 124.0 };
    Some(
        Tooltip::new(
            Button::new(
                "write_mode",
                label,
                theme::action_palette(theme::SELECTED, theme::FOREGROUND_SUBTLE),
            )
            .icon(Icon::Settings)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Regular)
            .enabled(!props.running)
            .hover_ink(theme::FOREGROUND)
            .render()
            .size(Some(width), Some(36.0))
            .radius(18.0),
            "write_mode",
            title,
            theme::component_palette(),
        )
        .render(),
    )
}

fn add_context_control(props: &ComposerProps<'_>) -> Element {
    let trigger = Tooltip::new(
        IconButton::new(
            "composer_add_context",
            Icon::Add,
            theme::action_palette(theme::SELECTED, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Regular)
        .enabled(!props.running)
        .hover_ink(theme::FOREGROUND)
        .render()
        .size(Some(CONTROL_HEIGHT), Some(CONTROL_HEIGHT))
        .radius(CONTROL_HEIGHT / 2.0),
        "composer_add_context",
        "Adjuntar archivo",
        theme::component_palette(),
    )
    .render();
    DropdownMenu::new(
        trigger,
        vec![
            DropdownMenuEntry::Heading("AÑADIR AL CHAT".into()),
            DropdownMenuItem::new("composer_add_file", "Archivo del proyecto")
                .icon(Icon::Folder)
                .into(),
            DropdownMenuItem::new("composer_add_browse", "Explorar archivos")
                .icon(Icon::Search)
                .into(),
        ],
        props.add_menu_open && !props.running,
        "composer_add_dismiss",
        theme::component_palette(),
    )
    .width(230.0)
    .placement(PopoverPlacement::TopStart)
    .motion(MenuMotion {
        open_ms: 160,
        item_ms: 100,
    })
    .render()
}

fn selector_heading(label: &str, width: f32) -> Element {
    Element::text(label)
        .font_family(FontFamily::Sans)
        .font_size(11.0)
        .foreground(theme::FOREGROUND_SUBTLE)
        .padding(8.0)
        .size(Some(width), Some(29.0))
}

fn selector_option(id: String, label: &str, selected: bool, width: f32) -> Element {
    let marker = if selected {
        Element::icon(Icon::Check).foreground(theme::FOREGROUND)
    } else {
        Element::spacer()
    }
    .size(Some(16.0), Some(18.0));
    let content = Element::row(vec![
        Element::truncated_text(label)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .foreground(theme::FOREGROUND)
            .flex(1.0)
            .size(None, Some(18.0)),
        marker,
    ])
    .gap(8.0);
    let fill = if selected {
        theme::SELECTED
    } else {
        theme::CARD
    };
    Element::clickable(id, content)
        .background(fill)
        .hover_background(theme::hover_for(fill))
        .pressed_background(theme::SELECTED)
        .padding(9.0)
        .radius(8.0)
        .size(Some(width), Some(36.0))
}

fn effort_tone(step: usize, count: usize) -> Color {
    let progress = step as f32 / count.saturating_sub(1).max(1) as f32;
    Color::rgb(
        (100.0 + 78.0 * progress) as u8,
        (100.0 + 42.0 * progress) as u8,
        (104.0 + 122.0 * progress) as u8,
    )
}

fn effort_heading(props: &ComposerProps<'_>, width: f32) -> Element {
    let selected = props
        .effort_options
        .iter()
        .position(|(id, _)| id == props.selected_effort)
        .unwrap_or(0);
    let selected_width =
        (props.effort_label.chars().count() as f32 * 7.0 + 4.0).clamp(36.0, width * 0.45);
    Element::row(vec![
        Element::text("ESFUERZO")
            .font_family(FontFamily::Sans)
            .font_size(11.0)
            .foreground(theme::FOREGROUND_SUBTLE)
            .flex(1.0)
            .size(None, Some(18.0)),
        Element::truncated_text(props.effort_label)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(effort_tone(selected, props.effort_options.len()))
            .size(Some(selected_width), Some(18.0)),
    ])
    .gap(8.0)
    .padding(5.0)
    .size(Some(width), Some(29.0))
}

fn effort_option(
    id: &str,
    label: &str,
    selected: bool,
    step: usize,
    count: usize,
    width: f32,
) -> Element {
    let tone = effort_tone(step, count);
    let mark = Element::panel(Element::spacer())
        .background(tone)
        .radius(3.0)
        .size(Some(5.0), Some(17.0));
    let content = Element::row(vec![
        mark,
        Element::truncated_text(label)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::FOREGROUND)
            .flex(1.0)
            .size(None, Some(18.0)),
        if selected {
            Element::icon(Icon::Check).foreground(theme::FOREGROUND)
        } else {
            Element::spacer()
        }
        .size(Some(16.0), Some(18.0)),
    ])
    .gap(6.0);
    let fill = if selected {
        theme::SELECTED
    } else {
        theme::CARD
    };
    let option = Element::clickable(format!("composer_effort:{id}"), content)
        .background(fill)
        .hover_background(theme::hover_for(fill))
        .pressed_background(theme::SELECTED)
        .padding(8.0)
        .radius(8.0)
        .size(Some(width), Some(36.0));
    if selected {
        option.border(tone, 1.0)
    } else {
        option
    }
}

fn model_effort_content(props: &ComposerProps<'_>, width: f32) -> Element {
    let inner_width = width - 12.0;
    let mut sections = Vec::new();
    if !props.model_options.is_empty() {
        sections.push(selector_heading("MODELO", inner_width));
        let rows = props
            .model_options
            .iter()
            .map(|(id, title)| {
                selector_option(
                    format!("composer_model:{id}"),
                    title,
                    id == props.selected_model,
                    inner_width,
                )
            })
            .collect();
        let list = Element::column(rows).gap(2.0);
        sections.push(if props.model_options.len() > 4 {
            ScrollArea::new("composer_model_options_scroll", list)
                .tint(theme::FOREGROUND_SUBTLE)
                .render()
                .size(
                    Some(inner_width),
                    Some(if width < 260.0 { 84.0 } else { 112.0 }),
                )
        } else {
            list
        });
    }
    if !props.effort_options.is_empty() {
        if !sections.is_empty() {
            sections.push(
                Element::panel(Element::spacer())
                    .background(theme::BORDER)
                    .size(Some(inner_width), Some(1.0)),
            );
        }
        sections.push(effort_heading(props, inner_width));
        let columns = if width < 260.0 { 2 } else { 3 };
        let cell_width = (inner_width - 4.0 * (columns - 1) as f32) / columns as f32;
        let rows = props
            .effort_options
            .chunks(columns)
            .enumerate()
            .map(|(row, chunk)| {
                Element::row(
                    chunk
                        .iter()
                        .enumerate()
                        .map(|(column, (id, label))| {
                            effort_option(
                                id,
                                label,
                                id == props.selected_effort,
                                row * columns + column,
                                props.effort_options.len(),
                                cell_width,
                            )
                        })
                        .collect(),
                )
                .gap(4.0)
            })
            .collect();
        sections.push(Element::column(rows).gap(4.0));
    }
    Element::column(sections)
        .gap(5.0)
        .size(Some(inner_width), None)
}

fn model_effort_group(props: &ComposerProps<'_>, compact: bool) -> Option<Element> {
    let model = if !props.model_label.is_empty() {
        props.model_label
    } else if props.selected_backend == "native" {
        props.model
    } else {
        ""
    };
    let label = match (model.is_empty(), props.effort_label.is_empty()) {
        (true, true) => return None,
        (false, true) => model.to_owned(),
        (true, false) => props.effort_label.to_owned(),
        (false, false) => format!("{model} {}", props.effort_label),
    };
    let max_width = if compact {
        (props.width - 20.0 - 40.0 - 5.0).max(96.0)
    } else {
        244.0
    };
    let width = (label.chars().count() as f32 * 8.0 + 50.0).clamp(96.0, max_width);
    if props.model_options.is_empty() && props.effort_options.is_empty() {
        return Some(
            Element::truncated_text(label)
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .foreground(theme::FOREGROUND)
                .padding(8.0)
                .size(Some(width), Some(CONTROL_HEIGHT)),
        );
    }
    let open = (props.model_menu_open || props.effort_menu_open) && !props.running;
    let visual = Element::row(vec![
        Element::truncated_text(label)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .foreground(theme::FOREGROUND)
            .size(Some((width - 46.0).max(0.0)), Some(16.0)),
        Element::icon(Icon::ChevronDown)
            .foreground(theme::FOREGROUND)
            .size(Some(16.0), Some(16.0)),
    ])
    .gap(6.0)
    .size(Some(width - 24.0), Some(16.0));
    let trigger = if props.running {
        Element::panel(visual)
    } else {
        Element::clickable("composer_model_toggle", visual)
    }
    .background(if open {
        theme::SELECTED
    } else {
        Color::TRANSPARENT
    })
    .hover_background(theme::hover_for(theme::SELECTED))
    .pressed_background(theme::SELECTED)
    .transition_ms(120)
    .padding(12.0)
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
    let menu_width = if compact {
        (props.width - 16.0).clamp(205.0, 304.0)
    } else {
        304.0
    };
    Some(
        Popover::new(
            trigger,
            model_effort_content(props, menu_width),
            open,
            "composer_model_dismiss",
            theme::component_palette(),
        )
        .width(menu_width)
        .placement(PopoverPlacement::TopEnd)
        .render(),
    )
}

fn send_control(props: &ComposerProps<'_>) -> Element {
    let can_send = !props.prompt.trim().is_empty() || !props.attachments.is_empty();
    let surface = if !props.running && !can_send {
        Color::rgb(70, 70, 70)
    } else if props.running {
        theme::SELECTED
    } else {
        theme::BRAND
    };
    let ink = if surface == theme::BRAND {
        theme::PRIMARY_FOREGROUND
    } else {
        theme::FOREGROUND
    };
    let mut palette = theme::action_palette(surface, ink);
    if surface == theme::BRAND {
        palette.hover = Color::rgb(238, 238, 238);
        palette.selected = Color::rgb(216, 216, 216);
    }
    let trigger = IconButton::new(
        "run_toggle",
        if props.running {
            Icon::Stop
        } else {
            Icon::Send
        },
        palette,
    )
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Regular)
    .enabled(props.running || can_send)
    .render()
    .size(Some(40.0), Some(40.0))
    .radius(20.0)
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

fn footer(props: &ComposerProps<'_>) -> Element {
    let two_rows = props.width
        < if props.selected_backend == "native" {
            440.0
        } else {
            390.0
        };
    let mut left = vec![add_context_control(props)];
    if let Some(mode) = mode_control(props, two_rows) {
        left.push(mode);
    }

    if two_rows {
        let mut right = vec![Element::spacer().flex(1.0)];
        if let Some(group) = model_effort_group(props, true) {
            right.push(group);
        }
        right.push(send_control(props));
        Element::column(vec![
            Element::row(left).gap(4.0).size(None, Some(CONTROL_HEIGHT)),
            Element::row(right).gap(4.0).size(None, Some(40.0)),
        ])
        .gap(4.0)
    } else {
        let mut row = left;
        row.push(Element::spacer().flex(1.0));
        if let Some(group) = model_effort_group(props, false) {
            row.push(group);
        }
        row.push(send_control(props));
        Element::row(row).gap(5.0).size(None, Some(40.0))
    }
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
                    .icon(Icon::Code)
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
    Composer::new(
        "prompt",
        props.prompt,
        props.placeholder,
        props.width,
        theme::component_palette(),
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
    .footer(footer(props))
    .border_color(theme::TREE_GUIDE)
    .shadow_color(Color::rgba(0, 0, 0, 48))
    .card_radius(CARD_RADIUS)
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, Key, Point, Size, UiAction, UiEvent, UiTree};

    const MODELS: &[(String, String)] = &[];

    fn props<'a>(prompt: &'a str, width: f32) -> ComposerProps<'a> {
        ComposerProps {
            workspace: "C:/projects/openIDE",
            project_action: Some("home_workspace_settings"),
            project_branch: Some("main"),
            project_menu_open: false,
            project_options: &[],
            environment_label: "Local",
            prompt,
            attachments: &[],
            add_menu_open: false,
            placeholder: "Escribe una tarea…",
            selected_backend: "codex",
            model: "",
            model_label: "GPT-6 Sol",
            model_options: MODELS,
            selected_model: "gpt-6-sol",
            model_menu_open: false,
            effort_label: "Ultra",
            effort_options: &[],
            selected_effort: "ultra",
            effort_menu_open: false,
            running: false,
            workspace_writes_enabled: false,
            status: None,
            width,
            home: true,
        }
    }

    #[test]
    fn metadata_is_outside_the_rounded_editor_card() {
        let frame = UiTree::new(build_composer(&props("", 620.0))).layout(Size::new(620.0, 350.0));
        let project = frame
            .regions
            .iter()
            .find(|region| region.id == "home_workspace_settings")
            .unwrap()
            .rect;
        let editor = frame
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .unwrap()
            .rect;
        let card = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::FillRect {
                    rect,
                    color,
                    radius,
                } if *color == theme::SELECTED && *radius == CARD_RADIUS => Some(*rect),
                _ => None,
            })
            .expect("rounded composer card");
        let context = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::FillRect {
                    rect,
                    color,
                    radius,
                } if *color == theme::PANEL && *radius == CONTEXT_RADIUS => Some(*rect),
                _ => None,
            })
            .expect("project context surface");
        assert!((context.x - card.x - CONTEXT_INSET).abs() < 0.1);
        assert!((card.x + card.width - CONTEXT_INSET - context.x - context.width).abs() < 0.1);
        assert!((context.y + context.height - card.y).abs() < 0.1);
        assert!(project.y + project.height < card.y);
        assert!(editor.y >= card.y && editor.y + editor.height <= card.y + card.height);
        assert!(
            card.height < 145.0,
            "empty composer card grew to {}",
            card.height
        );
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect {
                rect,
                color,
                radius,
            } if *color == Color::rgb(70, 70, 70)
                && *radius == 20.0
                && rect.width == 40.0
                && rect.height == 40.0
        )));
        assert!(!frame.regions.iter().any(|region| region.id == "run_toggle"));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                icon: Icon::Laptop,
                ..
            }
        )));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                icon: Icon::GitBranch,
                ..
            }
        )));
    }

    #[test]
    fn attachment_stays_compact_and_can_be_sent_without_text() {
        let attachments = [ComposerAttachmentView {
            path: "src/main.rs".into(),
            label: "main.rs".into(),
        }];
        let mut state = props("", 780.0);
        state.attachments = &attachments;
        state.add_menu_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(780.0, 400.0));
        let chip = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_attachment_remove:0")
            .expect("attachment chip");
        assert!(chip.rect.width <= 220.0);
        assert!(frame.regions.iter().any(|region| region.id == "run_toggle"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_add_file"));
    }

    #[test]
    fn project_selector_opens_without_moving_the_context_bar() {
        let projects = vec![("C:/projects/another".into(), "another".into())];
        let mut state = props("", 700.0);
        state.project_options = &projects;
        let closed = UiTree::new(build_composer(&state)).layout(Size::new(700.0, 400.0));
        let trigger = closed
            .regions
            .iter()
            .find(|region| region.id == "home_workspace_settings")
            .expect("project trigger")
            .rect;
        state.project_menu_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        let open = tree.layout(Size::new(700.0, 400.0));
        assert_eq!(
            trigger,
            open.regions
                .iter()
                .find(|region| region.id == "home_workspace_settings")
                .expect("project trigger with menu")
                .rect
        );
        assert!(open
            .regions
            .iter()
            .any(|region| region.id == "composer_project_select:0"));
        assert!(open
            .regions
            .iter()
            .any(|region| region.id == "composer_project_browse"));
        let browse = open
            .regions
            .iter()
            .find(|region| region.id == "composer_project_browse")
            .unwrap()
            .rect;
        let point = Point::new(
            browse.x + browse.width / 2.0,
            browse.y + browse.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "composer_project_browse".into()
            }]
        );
    }

    #[test]
    fn footer_uses_one_model_effort_pill_without_backend_toggle() {
        let models = vec![("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = vec![("ultra".into(), "Ultra".into())];
        let mut state = props("Tarea", 780.0);
        state.model_options = &models;
        state.effort_options = &efforts;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(780.0, 350.0));
        let rect = |id: &str| {
            frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
                .rect
        };
        let selector = rect("composer_model_toggle");
        assert!(rect("composer_add_context").x < selector.x);
        assert!(selector.x + selector.width <= rect("run_toggle").x);
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "backend_menu_toggle"
                || region.id == "composer_effort_toggle"));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "GPT-6 Sol Ultra"
        )));
    }

    #[test]
    fn narrow_footer_stacks_without_overlapping_or_clipping() {
        let models = vec![("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = vec![("ultra".into(), "Ultra".into())];
        for width in [250.0, 340.0, 410.0] {
            let mut state = props("Tarea", width);
            state.selected_backend = "native";
            state.model_options = &models;
            state.effort_options = &efforts;
            let frame = UiTree::new(build_composer(&state)).layout(Size::new(width, 360.0));
            let rect = |id: &str| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap_or_else(|| panic!("missing {id} at {width}"))
                    .rect
            };
            let left = [rect("composer_add_context"), rect("write_mode")];
            for pair in left.windows(2) {
                assert!(pair[0].x + pair[0].width <= pair[1].x);
            }
            let model = rect("composer_model_toggle");
            let send = rect("run_toggle");
            assert!(left[1].y < model.y);
            assert!(model.x + model.width <= send.x + 0.1);
            assert!(send.x + send.width <= width + 0.1);
        }
    }

    #[test]
    fn unsupported_model_or_effort_is_plain_text_not_a_fake_toggle() {
        let frame =
            UiTree::new(build_composer(&props("Tarea", 700.0))).layout(Size::new(700.0, 350.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "composer_model_toggle"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "composer_effort_toggle"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "backend_menu_toggle"));
    }

    #[test]
    fn unrelated_cli_does_not_show_the_native_model() {
        let mut state = props("Tarea", 700.0);
        state.selected_backend = "claude";
        state.model = "gpt-4.1";
        state.model_label = "";
        state.model_options = &[];
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(700.0, 350.0));
        assert!(!frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text.contains("gpt-4.1")
        )));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "composer_model_toggle"));
    }

    #[test]
    fn combined_popover_exposes_model_and_effort_actions() {
        let models = vec![
            ("gpt-6-sol".into(), "GPT-6 Sol".into()),
            ("gpt-6-astra".into(), "GPT-6 Astra".into()),
        ];
        let efforts = vec![
            ("high".into(), "High".into()),
            ("ultra".into(), "Ultra".into()),
        ];
        let mut state = props("Tarea", 700.0);
        state.model_options = &models;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        let frame = tree.layout(Size::new(700.0, 500.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_model:gpt-6-astra"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_effort:high"));
        let effort = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_effort:high")
            .unwrap()
            .rect;
        let point = Point::new(
            effort.x + effort.width / 2.0,
            effort.y + effort.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "composer_effort:high".into()
            }]
        );
        state.model_menu_open = false;
        state.effort_menu_open = true;
        let shortcut_frame = UiTree::new(build_composer(&state)).layout(Size::new(700.0, 500.0));
        assert!(shortcut_frame
            .regions
            .iter()
            .any(|region| region.id == "composer_effort:high"));
    }

    #[test]
    fn combined_pill_hover_and_chevron_share_one_centered_hitbox() {
        let models = vec![("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = vec![("ultra".into(), "Ultra".into())];
        let mut state = props("Tarea", 700.0);
        state.model_options = &models;
        state.effort_options = &efforts;
        let mut tree = UiTree::new(build_composer(&state));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(700.0, 400.0));
        let selector = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_model_toggle")
            .unwrap()
            .rect;
        let chevron = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Icon {
                    icon: Icon::ChevronDown,
                    rect,
                    ..
                } => Some(*rect),
                _ => None,
            })
            .expect("selector chevron");
        assert!(
            (chevron.y + chevron.height / 2.0 - selector.y - selector.height / 2.0).abs() < 0.1
        );
        assert!(
            chevron.x >= selector.x && chevron.x + chevron.width <= selector.x + selector.width
        );
        let point = Point::new(
            selector.x + selector.width / 2.0,
            selector.y + selector.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerMove(point));
        let hovered = tree.layout(Size::new(700.0, 400.0));
        assert!(hovered.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, .. }
                if *rect == selector && *color == theme::hover_for(theme::SELECTED)
        )));
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "composer_model_toggle".into()
            }]
        );
    }

    #[test]
    fn editor_grows_then_caps_for_long_drafts() {
        let short =
            UiTree::new(build_composer(&props("Hola", 700.0))).layout(Size::new(700.0, 350.0));
        let long = UiTree::new(build_composer(&props(
            &"texto de prueba\n".repeat(40),
            700.0,
        )))
        .layout(Size::new(700.0, 350.0));
        let height = |frame: &openide_ui::Frame| {
            frame
                .regions
                .iter()
                .find(|region| region.id == "prompt")
                .unwrap()
                .rect
                .height
        };
        assert!(height(&long) > height(&short));
        assert!(height(&long) <= 216.0);
    }

    #[test]
    fn editor_focus_keeps_the_same_hitbox() {
        let mut tree = UiTree::new(build_composer(&props("Hola", 700.0)));
        let ordinary = tree.layout(Size::new(700.0, 350.0));
        let before = ordinary
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .unwrap()
            .rect;
        assert!(tree.focus("prompt"));
        let focused = tree.layout(Size::new(700.0, 350.0));
        let after = focused
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .unwrap()
            .rect;
        assert_eq!(before, after);
    }

    #[test]
    fn context_icon_hover_is_centered_and_clicks_by_pointer_or_keyboard() {
        let mut tree = UiTree::new(build_composer(&props("Hola", 340.0)));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(340.0, 350.0));
        let hit = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_add_context")
            .unwrap()
            .rect;
        assert_eq!(hit.width, CONTROL_HEIGHT);
        assert_eq!(hit.height, CONTROL_HEIGHT);
        let center = Point::new(hit.x + hit.width / 2.0, hit.y + hit.height / 2.0);
        let glyph = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Icon {
                    rect,
                    icon: Icon::Add,
                    ..
                } => Some(*rect),
                _ => None,
            })
            .expect("context icon");
        assert!((glyph.x + glyph.width / 2.0 - center.x).abs() < 0.1);
        assert!((glyph.y + glyph.height / 2.0 - center.y).abs() < 0.1);
        tree.handle_event(UiEvent::PointerMove(center));
        let hovered = tree.layout(Size::new(340.0, 350.0));
        assert!(hovered.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, radius }
                if *rect == hit && *color == theme::hover_for(theme::SELECTED) && *radius == CONTROL_HEIGHT / 2.0
        )));
        tree.handle_event(UiEvent::PointerDown(center));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(center)),
            vec![UiAction::Clicked {
                id: "composer_add_context".into()
            }]
        );
        assert!(tree.focus("composer_add_context"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "composer_add_context".into()
            }]
        );
    }

    #[test]
    fn running_mode_exposes_stop_and_holds_permission_choice() {
        let mut state = props("Siguiente paso", 300.0);
        state.selected_backend = "native";
        let idle = UiTree::new(build_composer(&state)).layout(Size::new(300.0, 360.0));
        assert!(idle.regions.iter().any(|region| region.id == "write_mode"));
        state.running = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(300.0, 360.0));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                icon: Icon::Stop,
                ..
            }
        )));
        assert!(frame.regions.iter().any(|region| region.id == "run_toggle"));
        assert!(!frame.regions.iter().any(|region| region.id == "write_mode"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "composer_add_context"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "home_workspace_settings"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "backend_menu_toggle"));
    }
}
