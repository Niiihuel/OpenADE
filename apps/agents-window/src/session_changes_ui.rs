//! Session-owned presentation of confirmed file changes. Shared immutable
//! snapshots let virtual rows retain their data without copying diffs per frame.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use openide_ui::{
    Breadcrumb, BreadcrumbItem, Button, ButtonVariant, Component, ControlSize, Element, FontFamily,
    Icon, IconButton, ListRow, ScrollArea, Separator, Text, TextRole, Tooltip,
};

use crate::{
    git_review::{ReviewLine, ReviewLineKind},
    session_changes::{SessionChanges, SessionFileChange},
    theme,
    view::ViewState,
};

#[derive(Default)]
pub(crate) struct SessionChangesUiState {
    pub workspace: Option<PathBuf>,
    pub session_id: Option<String>,
    pub generation: u64,
    pub loading: bool,
    pub result: Option<Result<Arc<SessionChanges>, String>>,
    pub selected_file: Option<String>,
    pub page: usize,
    /// Only the last turn's changes are asked for and shown.
    pub last_turn: bool,
}

pub(crate) fn build(state: &ViewState, width: f32) -> Element {
    build_content(&state.session_changes, width)
}

fn build_content(ui: &SessionChangesUiState, width: f32) -> Element {
    let inner = (width - 24.0).max(140.0);
    let title = Element::faded_text(if ui.last_turn { "Cambios del último turno" } else { "Cambios del chat" }, 12.0)
        .font_family(FontFamily::Sans)
        .foreground(theme::FOREGROUND)
        .font_size(14.0)
        .size(None, Some(32.0));
    let mut rows = vec![openide_ui::PanelHeader::new("", theme::component_palette())
        .leading(title)
        .trailing(vec![IconButton::new(
            "session_changes_refresh",
            Icon::Refresh,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .button_size(openide_ui::ButtonSize::IconSmall)
        .enabled(!ui.loading)
        .tooltip("Actualizar cambios del chat")
        .render()])
        .width(inner)
        .sizing(crate::ui_sizing::TALL_HEADER)
        .render()];
    {
        use crate::view::ReviewOrigin;
        rows.insert(0, crate::view::origin_selector(if ui.last_turn { ReviewOrigin::Turn } else { ReviewOrigin::Chat }));
    }
    match &ui.result {
        None => rows.push(
            Text::new(
                if ui.loading {
                    "Cargando cambios…"
                } else {
                    "Inicia una conversación para ver sus cambios."
                },
                theme::component_palette(),
            )
            .muted()
            .render(),
        ),
        Some(Err(error)) => rows.push(
            Text::new(error, theme::component_palette())
                .ink(theme::DESTRUCTIVE)
                .render(),
        ),
        Some(Ok(changes)) => append_changes(&mut rows, Arc::clone(changes), ui, inner),
    }
    Element::column(rows)
        .gap(8.0)
        .padding(12.0)
        .size(Some(width), None)
        .flex(1.0)
}

fn append_changes(
    rows: &mut Vec<Element>,
    changes: Arc<SessionChanges>,
    ui: &SessionChangesUiState,
    width: f32,
) {
    use crate::session_changes::ChangesAvailability;
    if let ChangesAvailability::Unavailable { reason } = &changes.availability {
        rows.push(theme::text_px(
            "Dato no disponible",
            15.0,
            theme::FOREGROUND,
        ));
        rows.push(
            Text::new(reason, theme::component_palette())
                .muted()
                .render(),
        );
        rows.push(
            Button::new(
                "dock_open:review",
                "Ver cambios del repositorio",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .render(),
        );
        return;
    }
    rows.push(
        Element::row(vec![
            Text::new(
                format!("{} archivos", changes.totals.files),
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render()
            .flex(1.0),
            Text::new(
                format!("+{}", changes.totals.additions),
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .ink(theme::DIFF_ADDED)
            .render(),
            Text::new(
                format!("−{}", changes.totals.deletions),
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .ink(theme::DIFF_REMOVED)
            .render(),
        ])
        .gap(8.0)
        .size(Some(width), Some(22.0)),
    );
    if matches!(changes.availability, ChangesAvailability::Limited) {
        rows.push(
            Text::new(
                "Registro parcial de cambios confirmados",
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render(),
        );
    }
    // Coverage explanations are preserved without pushing files and code below
    // an unbounded banner. The full explanation is available on hover/focus.
    if !changes.coverage_notes.is_empty() {
        rows.push(
            Tooltip::new(
                Text::new(changes.coverage_notes.join(" · "), theme::component_palette())
                    .font_size(10.5)
                    .muted()
                    .truncate()
                    .render()
                    .size(Some(width), Some(18.0)),
                "session_changes_coverage",
                "Cobertura de los cambios",
                theme::component_palette(),
            )
            .description(changes.coverage_notes.join("\n"))
            .render(),
        );
    }
    rows.push(Separator::horizontal(theme::component_palette()).render());
    if let Some(selected_index) = ui.selected_file.as_deref().and_then(|path| {
        changes
            .files
            .iter()
            .position(|change| change.file.path == path)
    }) {
        let selected = &changes.files[selected_index];
        let mut path_items = vec![BreadcrumbItem::new("Archivos")
            .action("session_change_back")
            .tooltip("Volver a los archivos modificados por el chat")];
        path_items.extend(
            Path::new(&selected.file.path)
                .components()
                .map(|component| {
                    BreadcrumbItem::new(component.as_os_str().to_string_lossy())
                        .tooltip(&selected.file.path)
                }),
        );
        rows.push(
            Breadcrumb::new(
                "session_change_path",
                path_items,
                theme::component_palette(),
            )
            .width(width)
            .height(28.0)
            .render(),
        );
        rows.push(
            Element::row(vec![
                Text::new(&selected.file.status, theme::component_palette())
                    .role(TextRole::Small)
                    .muted()
                    .render()
                    .flex(1.0),
                Text::new(
                    format!("+{}", selected.file.additions),
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .ink(theme::DIFF_ADDED)
                .render(),
                Text::new(
                    format!("−{}", selected.file.deletions),
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .ink(theme::DIFF_REMOVED)
                .render(),
            ])
            .gap(6.0)
            .size(None, Some(22.0)),
        );
        if let Some(reason) = &selected.unavailable {
            rows.push(
                Text::new(reason, theme::component_palette())
                    .muted()
                    .render(),
            );
            return;
        }
        if selected.truncated {
            rows.push(
                Text::new(
                    "Vista del diff limitada por tamaño",
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .muted()
                .render(),
            );
        }
        let count = selected.file.lines.len();
        let list_id = format!(
            "session_change_diff:{}:{}",
            changes.session_id, selected.file.path
        );
        if count == 0 {
            rows.push(
                Text::new(
                    "Sin diferencias de texto disponibles.",
                    theme::component_palette(),
                )
                .muted()
                .render(),
            );
            return;
        }
        rows.push(
            ScrollArea::uniform_list(list_id, count, 20.0, move |index| {
                diff_row(
                    &changes.files[selected_index].file.lines[index],
                    width - 8.0,
                )
            })
            .render()
            .background(theme::CODE_SURFACE)
            .top_fade(theme::CODE_SURFACE, 12.0)
            .size(Some(width), None)
            .flex(1.0),
        );
        return;
    }
    if changes.files.is_empty() {
        rows.push(
            Text::new(
                "Este chat todavía no registró cambios en archivos.",
                theme::component_palette(),
            )
            .muted()
            .render(),
        );
        return;
    }
    let generation = ui.generation;
    let list_id = format!("session_changes_files:{}", changes.session_id);
    rows.push(
        ScrollArea::uniform_list(list_id, changes.files.len(), 38.0, move |index| {
            file_row(
                &changes.files[index],
                format!("session_change_file:{generation}:{index}"),
                width - 8.0,
            )
        })
        .render()
        .top_fade(theme::BACKGROUND, 12.0)
        .size(Some(width), None)
        .flex(1.0),
    );
}

/// One changed file of the chat: its path, its counts and the hint that
/// says what the row cannot.
fn file_row(change: &SessionFileChange, action: String, width: f32) -> Element {
    let file = &change.file;
    let count = |text: String, ink| {
        Text::new(text, theme::component_palette())
            .role(TextRole::Small)
            .ink(ink)
            .render()
            .size(None, Some(22.0))
    };
    let filename = Path::new(&file.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&file.path);
    Tooltip::new(
        ListRow::new(&action, &file.path, theme::component_palette())
            .icon(openide_ui::file_icon(&file.path))
            .meta_element(count(format!("+{}", file.additions), theme::DIFF_ADDED))
            .meta_element(count(format!("−{}", file.deletions), theme::DIFF_REMOVED))
            .width(width)
            .sizing(crate::ui_sizing::CHANGES_FILE)
            .render(),
        action,
        filename,
        theme::component_palette(),
    )
    .description(&file.path)
    .footer(change.unavailable.as_deref().unwrap_or(&file.status))
    .render()
}

fn diff_row(line: &ReviewLine, width: f32) -> Element {
    let (ink, background) = match line.kind {
        ReviewLineKind::Added => (theme::DIFF_ADDED, theme::DIFF_ADDED_BG),
        ReviewLineKind::Removed => (theme::DIFF_REMOVED, theme::DIFF_REMOVED_BG),
        ReviewLineKind::Hunk | ReviewLineKind::File => {
            (theme::FOREGROUND_SUBTLE, theme::DIFF_HUNK_BG)
        }
        _ => (theme::FOREGROUND, theme::CODE_SURFACE),
    };
    let number = |number: Option<usize>| {
        Element::text(number.map(|n| n.to_string()).unwrap_or_default())
            .font_family(FontFamily::Monospace)
            .font_size(10.0)
            .foreground(theme::CODE_GUTTER)
            .size(Some(32.0), Some(20.0))
    };
    Element::row(vec![
        number(line.old_number),
        number(line.new_number),
        Element::truncated_text(&line.text)
            .font_family(FontFamily::Monospace)
            .font_size(11.0)
            .foreground(ink)
            .flex(1.0)
            .size(None, Some(20.0)),
    ])
    .gap(4.0)
    .background(background)
    .size(Some(width), Some(20.0))
    .clip_children()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        git_review::ReviewFile,
        session_changes::{ChangeTotals, ChangesAvailability, SessionFileChange},
    };
    use openide_ui::{DrawCommand, Point, Size, UiEvent, UiTree};

    fn snapshot(file_count: usize, line_count: usize) -> Arc<SessionChanges> {
        Arc::new(SessionChanges {
            session_id: "chat".into(),
            workspace: PathBuf::from("/workspace"),
            baseline: None,
            availability: ChangesAvailability::Complete,
            totals: ChangeTotals {
                files: file_count,
                additions: line_count,
                deletions: 0,
                exact: true,
            },
            files: (0..file_count)
                .map(|index| SessionFileChange {
                    file: ReviewFile {
                        path: format!("src/file-{index}.rs"),
                        additions: line_count,
                        lines: (0..line_count)
                            .map(|n| ReviewLine {
                                kind: ReviewLineKind::Added,
                                text: format!("+line {n}"),
                                old_number: None,
                                new_number: Some(n + 1),
                            })
                            .collect(),
                        ..ReviewFile::default()
                    },
                    last_tool_sequence: 1,
                    unavailable: None,
                    truncated: false,
                })
                .collect(),
            coverage_notes: Vec::new(),
        })
    }

    #[test]
    fn file_list_only_builds_visible_rows_and_keeps_generation_actions() {
        let source = snapshot(128, 0);
        let ui = SessionChangesUiState {
            result: Some(Ok(Arc::clone(&source))),
            generation: 7,
            ..Default::default()
        };
        let frame = UiTree::new(build_content(&ui, 400.0)).layout(Size::new(400.0, 600.0));
        let file_actions: Vec<_> = frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("session_change_file:"))
            .collect();
        assert!(file_actions
            .iter()
            .any(|region| region.id == "session_change_file:7:0"));
        assert!(file_actions.len() < 30);
        assert!(Arc::strong_count(&source) >= 2);
    }

    #[test]
    fn selected_diff_is_virtual_and_has_no_page_navigation() {
        let ui = SessionChangesUiState {
            result: Some(Ok(snapshot(1, 2000))),
            selected_file: Some("src/file-0.rs".into()),
            ..Default::default()
        };
        let frame = UiTree::new(build_content(&ui, 400.0)).layout(Size::new(400.0, 600.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "session_change_back"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "session_change_next"));
        assert!(frame.commands.len() < 300);
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                icon: Icon::ChevronRight,
                ..
            }
        )));
    }

    #[test]
    fn file_hover_keeps_the_complete_long_path_without_an_extra_action() {
        let mut changes = snapshot(1, 1);
        let path = format!("apps/{}src/file.rs", "long-directory-name/".repeat(5));
        Arc::make_mut(&mut changes).files[0].file.path = path.clone();
        let ui = SessionChangesUiState {
            result: Some(Ok(changes)),
            generation: 9,
            ..Default::default()
        };
        let viewport = Size::new(300.0, 700.0);
        let mut tree = UiTree::new(build_content(&ui, viewport.width));
        let frame = tree.layout(viewport);
        let file = frame
            .regions
            .iter()
            .find(|region| region.id == "session_change_file:9:0")
            .unwrap();
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.contains("file_hint")));
        tree.handle_event(UiEvent::PointerMove(Point::new(
            file.rect.x + file.rect.width / 2.0,
            file.rect.y + file.rect.height / 2.0,
        )));
        let hovered = tree.layout_at(
            viewport,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        );
        let text = hovered
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(text.contains(&path), "{text}");
        assert!(hovered
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Shadow { radius, .. } if *radius == 4.0)));
    }

    #[test]
    fn coverage_and_breadcrumb_help_wrap_without_losing_their_text() {
        let mut changes = snapshot(1, 1);
        let note = "El registro incluye los cambios confirmados durante esta conversación y conserva el detalle de los archivos disponibles.";
        Arc::make_mut(&mut changes).coverage_notes.push(note.into());
        let ui = SessionChangesUiState {
            result: Some(Ok(changes)),
            selected_file: Some("src/file-0.rs".into()),
            ..Default::default()
        };
        let viewport = Size::new(280.0, 600.0);
        let mut tree = UiTree::new(build_content(&ui, viewport.width));
        let frame = tree.layout(viewport);
        let coverage = frame
            .regions
            .iter()
            .find(|region| region.id == "session_changes_coverage")
            .unwrap();
        tree.handle_event(UiEvent::PointerMove(Point::new(
            coverage.rect.x + coverage.rect.width / 2.0,
            coverage.rect.y + coverage.rect.height / 2.0,
        )));
        let hovered = tree.layout_at(
            viewport,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        );
        let visible_text = |frame: &openide_ui::Frame| {
            frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .flat_map(str::split_whitespace)
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(visible_text(&hovered).contains(note));
        tree.handle_event(UiEvent::PointerMove(Point::new(279.0, 599.0)));
        assert!(tree.focus("session_change_back"));
        let focused = tree.layout(viewport);
        assert!(visible_text(&focused).contains("Volver a los archivos modificados por el chat"));
    }
}
