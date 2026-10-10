use std::sync::Arc;

use openide_ui::{
    Component, ControlSize, Element, Empty, Icon, Kbd, Point, ScrollArea, ScrollbarAppearance,
    Search, SegmentedControl, Separator, SkeletonGroup, Tab, Text, TextRole, Tooltip,
};

use super::ViewState;
use crate::{
    command_center::{self, ResultItem, Scope, Target},
    keymap::{KeymapProfile, ShortcutAction},
    theme,
};

const ROW_HEIGHT: f32 = 30.0;
const FOOTER_HEIGHT: f32 = 28.0;

fn inset(child: Element) -> Element {
    Element::row(vec![
        Element::spacer().size(Some(8.0), None),
        child.flex(1.0),
        Element::spacer().size(Some(8.0), None),
    ])
}

/// Placeholder rows shown while a search is still producing results.
fn search_placeholders(rows: usize) -> impl Iterator<Item = Element> {
    SkeletonGroup::rows("command_search_progress", theme::component_palette(), rows)
        .row_height(ROW_HEIGHT)
        .icon(true)
        .into_rows()
        .into_iter()
}

fn effective_scope(state: &ViewState) -> Scope {
    command_center::parse_query(&state.command_query, state.command_scope).scope
}

/// The host uses this exact mapping to reveal keyboard selection in the virtual list.
pub(crate) fn command_result_row(state: &ViewState, index: usize) -> usize {
    let mut row = index;
    if effective_scope(state) == Scope::All {
        let mut previous = None;
        for result in state.command_results.iter().take(index + 1) {
            if previous != Some(result.section) {
                row += 1;
                previous = Some(result.section);
            }
        }
    }
    row
}

/// Keep focus IDs stable, but bind each dispatched action to its current result.
pub(crate) fn command_result_action(state: &ViewState, index: usize) -> String {
    let target = state
        .command_results
        .get(index)
        .map(|result| format!("{:?}", result.target));
    let scope = format!("{:?}", effective_scope(state));
    let payload = serde_json::to_string(&(&state.workspace, &state.command_query, scope, target))
        .expect("command result identity serializes");
    format!("command_pick:{index}:{payload}")
}

fn result_icon(result: &ResultItem) -> Icon {
    match &result.target {
        Target::OpenFile(path) => openide_ui::file_icon(&path.to_string_lossy()),
        Target::OpenSession(_) | Target::OpenSessions => Icon::Chat,
        Target::NewTask => Icon::Add,
        Target::OpenFiles => Icon::Folder,
        Target::OpenSettings => Icon::Settings,
        Target::OpenAutomations => Icon::Clock,
        Target::OpenMarketplace => Icon::Grid,
        Target::OpenGitReview => Icon::GitBranch,
        Target::CompactContext => crate::provider_icons::CONTEXT_RING,
        Target::ResumeTurn => Icon::Play,
        Target::ToggleRightDock => Icon::PanelRight,
    }
}

fn secondary_label(result: &ResultItem) -> String {
    if let Target::OpenFile(path) = &result.target {
        // A root file needs no duplicate subtitle. Nested files show only the directory.
        path.parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else if result.subtitle != result.title {
        result.subtitle.clone()
    } else {
        String::new()
    }
}

fn result_row(
    index: usize,
    result: &ResultItem,
    action: &str,
    selected: bool,
    width: f32,
    profile: KeymapProfile,
) -> Element {
    let id = format!("command_result:{index}");
    let secondary = secondary_label(result);
    let shortcut =
        matches!(result.target, Target::NewTask).then(|| profile.shortcut(ShortcutAction::NewTask));
    let shortcut_width = if shortcut.is_some() { 120.0 } else { 0.0 };
    // Reserve the scrollbar gutter even before overflow, keeping labels stable.
    let text_width = (width - 20.0 - 14.0 - 16.0 - 16.0 - 8.0 - shortcut_width).max(0.0);
    let title_width = if secondary.is_empty() {
        text_width
    } else {
        (result.title.chars().count() as f32 * 7.8 + 4.0).min(text_width * 0.62)
    };
    let mut labels = vec![Text::new(&result.title, theme::component_palette())
        .role(TextRole::Label)
        .truncate()
        .render()
        .size(Some(title_width), None)];
    if !secondary.is_empty() && text_width - title_width > 16.0 {
        labels.push(Element::spacer().size(Some(8.0), None));
        labels.push(
            Text::new(secondary, theme::component_palette())
                .muted()
                .truncate()
                .render()
                .flex(1.0),
        );
    }
    let body = Element::row(vec![
        Element::icon(result_icon(result))
            .size(Some(16.0), Some(16.0))
            .centered_y()
            .size(Some(16.0), None),
        Element::row(labels)
            .size(Some(text_width), None)
            .centered_y()
            .size(None, Some(ROW_HEIGHT))
            .flex(1.0),
        if let Some(shortcut) = shortcut {
            Kbd::new(shortcut, theme::component_palette())
                .max_width(shortcut_width)
                .render()
                .centered_y()
                .size(Some(shortcut_width), None)
        } else {
            Element::spacer().size(Some(0.0), None)
        },
    ])
    .gap(8.0);
    let anchor = Element::clickable(
        &id,
        Element::row(vec![
            Element::spacer().size(Some(8.0), None),
            body.flex(1.0),
            Element::spacer().size(Some(8.0), None),
        ])
        .size(None, Some(ROW_HEIGHT)),
    )
    .dispatch_as(action)
    .background(if selected {
        theme::SELECTED
    } else {
        theme::PANEL
    })
    .hover_background(if selected {
        theme::SELECTED
    } else {
        theme::INPUT
    })
    .pressed_background(theme::SELECTED)
    .radius(6.0)
    .clip_children();
    let hint = match &result.target {
        Target::OpenFile(path) => path.to_string_lossy().into_owned(),
        _ if result.subtitle.is_empty() => result.title.clone(),
        _ => format!("{} · {}", result.title, result.subtitle),
    };
    Element::row(vec![
        Tooltip::new(anchor, id, hint, theme::component_palette())
            .render()
            .flex(1.0),
        // Keep row hover/selection ink clear of the scrollbar's pointer rail.
        Element::spacer().size(Some(2.0), None),
    ])
    .size(None, Some(ROW_HEIGHT))
}

fn footer_hint(keys: &str, label: &str) -> Element {
    Element::row(vec![
        Kbd::new(keys, theme::component_palette()).render(),
        Text::new(label, theme::component_palette())
            .role(TextRole::Small)
            .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
            .truncate()
            .render()
            .centered_y()
            .size(None, Some(20.0)),
    ])
    .gap(6.0)
    .size(None, Some(20.0))
}

pub fn build_command_center(state: &ViewState, width: f32, height: f32) -> Element {
    let scope = effective_scope(state);
    let filter_tabs = SegmentedControl::new(
        [
            ("all", "Todo", Scope::All, 48.0),
            ("commands", "Acciones", Scope::Commands, 70.0),
            ("sessions", "Tareas", Scope::Sessions, 54.0),
            ("files", "Archivos", Scope::Files, 72.0),
        ]
        .into_iter()
        .map(|(id, title, value, tab_width)| {
            Tab::new(format!("command_scope:{id}"), title, tab_width).active(scope == value)
        })
        .collect(),
        theme::component_palette(),
    )
    .render();

    let search_status = if state.command_search_loading {
        None
    } else if !state.command_search_errors.is_empty() {
        Some((
            format!(
                "{} · {}",
                if state.command_results.is_empty() {
                    "Error de búsqueda"
                } else {
                    "Resultados parciales"
                },
                state.command_search_errors.join(" · ")
            ),
            true,
        ))
    } else if state.command_search_truncated {
        Some((
            "Se muestran los primeros resultados. Precisa la búsqueda para ver más.".to_owned(),
            false,
        ))
    } else {
        None
    };
    // Header34 + filters28 + footer28 + separators2 + five gaps6 + padding20.
    let results_height =
        (height - 142.0 - if search_status.is_some() { 30.0 } else { 0.0 }).max(0.0);
    let results = if state.command_results.is_empty() {
        let body = if state.command_search_loading {
            let rows = ((results_height / ROW_HEIGHT) as usize).min(6);
            let mut placeholders: Vec<_> = search_placeholders(rows).map(inset).collect();
            placeholders.push(Element::spacer().flex(1.0));
            Element::column(placeholders).size(None, Some(results_height))
        } else {
            let (title, description, icon) = if !state.command_search_errors.is_empty() {
                (
                    "No se pudo completar la búsqueda",
                    "Revisa el origen de datos o prueba otro filtro.",
                    Icon::WarningTriangle,
                )
            } else if state.command_search_truncated {
                (
                    "Hay más coincidencias",
                    "Precisa la búsqueda para ver otros resultados.",
                    Icon::Search,
                )
            } else {
                (
                    "No hay resultados",
                    "Prueba otro término o cambia el filtro.",
                    Icon::Search,
                )
            };
            Empty::new(title, description, theme::component_palette())
                .icon(icon)
                .compact()
                .height(results_height)
                .render()
        };
        ScrollArea::new("command_results", body).render().flex(1.0)
    } else {
        let mut entries = Vec::new();
        let mut section = None;
        for (index, result) in state.command_results.iter().enumerate() {
            if scope == Scope::All && section != Some(result.section) {
                section = Some(result.section);
                let title = match result.section {
                    Scope::Commands => "Acciones",
                    Scope::Sessions => "Tareas",
                    Scope::Files => "Archivos",
                    Scope::All => "Resultados",
                };
                entries.push(inset(
                    Text::new(title, theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .truncate()
                        .render()
                        .centered_y()
                        .size(None, Some(ROW_HEIGHT)),
                ));
            }
            entries.push(result_row(
                index,
                result,
                &command_result_action(state, index),
                state.command_selected_index == index,
                width,
                state.keymap_profile,
            ));
        }
        if state.command_search_loading {
            entries.extend(search_placeholders(1).map(inset));
        }
        let entries = Arc::new(entries);
        ScrollArea::uniform_list("command_results", entries.len(), ROW_HEIGHT, move |row| {
            entries[row].clone()
        })
        .appearance(ScrollbarAppearance::reserved().buttons(false))
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .flex(1.0)
    };
    let summary = if state.command_search_loading {
        String::new()
    } else {
        match (state.command_results.len(), state.command_search_truncated) {
            (0, _) => "Sin resultados".into(),
            (1, false) => "1 resultado".into(),
            (count, true) => format!("{count}+ resultados"),
            (count, false) => format!("{count} resultados"),
        }
    };
    let placeholder = match scope {
        Scope::Files => "Buscar archivos por nombre…",
        Scope::Commands => "Buscar una acción…",
        Scope::Sessions => "Buscar tareas…",
        Scope::All => "Buscar acciones, tareas o archivos…",
    };
    let close = Tooltip::new(
        theme::interactive_surface(Element::clickable(
            "command_close",
            footer_hint("Esc", "Cerrar"),
        ))
        .padding(4.0)
        .size(None, Some(FOOTER_HEIGHT)),
        "command_close",
        "Cerrar la paleta",
        theme::component_palette(),
    )
    .render();
    let mut footer = Vec::new();
    // Keep complete key combinations at compact widths; the result count is
    // secondary to navigation and dismissal and stays in the wide layout.
    if width >= 440.0 {
        footer.push(
            Text::new(summary, theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .truncate()
                .render()
                .centered_y()
                .size(None, Some(FOOTER_HEIGHT))
                .flex(1.0),
        );
    } else {
        footer.push(Element::spacer().flex(1.0));
    }
    footer.extend([
        footer_hint("↑↓", "Navegar")
            .centered_y()
            .size(None, Some(FOOTER_HEIGHT)),
        footer_hint("Enter", "Abrir")
            .centered_y()
            .size(None, Some(FOOTER_HEIGHT)),
        close,
    ]);
    let mut content = vec![
        Search::new(
            "command_query",
            &state.command_query,
            placeholder,
            theme::component_palette(),
        )
        .size(ControlSize::Regular)
        .render()
        .persistent_placeholder(true)
        .size(None, Some(34.0)),
        filter_tabs,
    ];
    if let Some((message, error)) = search_status {
        content.push(
            Text::new(message, theme::component_palette())
                .role(TextRole::Small)
                .ink(if error {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND_SUBTLE
                })
                .truncate()
                .render()
                .centered_y()
                .size(None, Some(24.0)),
        );
    }
    content.extend([
        Separator::horizontal(theme::component_palette()).render(),
        results,
        Separator::horizontal(theme::component_palette()).render(),
        Element::row(footer)
            .gap(12.0)
            .size(None, Some(FOOTER_HEIGHT)),
    ]);
    Element::panel(Element::column(content).gap(6.0))
        .padding(10.0)
        .background(theme::PANEL)
        .border(theme::BORDER, 1.0)
        .radius(12.0)
        .shadow(
            theme::COMMAND_PALETTE_SHADOW,
            Point::new(0.0, 16.0),
            28.0,
            1.0,
        )
        .size(Some(width), Some(height))
        .clip_children()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Key, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn search_escape_keycap_closes_by_mouse_and_keyboard_without_submitting_query() {
        let state = crate::view::tests::empty_state();
        for width in [300.0, 560.0] {
            let mut tree = UiTree::new(build_command_center(&state, width, 420.0));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 420.0));
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == "command_close")
                .unwrap()
                .rect;
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "command_close".into()
                }]
            );
            assert!(tree.focus("command_close"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "command_close".into()
                }]
            );
        }
    }
}
