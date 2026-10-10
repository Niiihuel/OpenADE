//! A compact repository navigator with demand-loaded, virtualized diffs.
//! The repository list owns its scroll area; it never embeds patch contents.
use super::*;
use openide_ui::{
    Breadcrumb, BreadcrumbItem, CodeDiff, CodeLanguage, Component, ComponentPalette,
    SectionHeader, Separator,
};

const CHANGE_ROW_HEIGHT: f32 = crate::sidebar_chrome::NAV_ROW_HEIGHT;
const CHANGE_ACTION_SIZE: f32 = crate::sidebar_chrome::TOOL_SIZE;
const SCROLL_RAIL_WIDTH: f32 = 14.0;

#[derive(Clone)]
enum ChangeRow {
    Section {
        title: String,
        count: usize,
    },
    File {
        action: String,
        stage_action: String,
        prepared: bool,
        path: String,
        status: String,
        additions: usize,
        deletions: usize,
    },
}

// Keep the worker snapshot shared. Only the visible row creates labels/actions;
// rebuilding the pane never copies every path in a large repository.
fn indexed_change_row(review: &GitReview, mut row: usize) -> ChangeRow {
    for title in ["CAMBIOS SIN PREPARAR", "CAMBIOS PREPARADOS"] {
        if let Some((section_index, section)) = review
            .sections
            .iter()
            .enumerate()
            .find(|(_, section)| section.title == title)
        {
            if row == 0 {
                return ChangeRow::Section {
                    title: title.into(),
                    count: section.files.len(),
                };
            }
            row -= 1;
            if let Some(file) = section.files.get(row) {
                let prepared = title == "CAMBIOS PREPARADOS";
                return ChangeRow::File {
                    action: format!("review_file_open:{section_index}:{row}"),
                    stage_action: format!(
                        "review_{}:{section_index}:{row}",
                        if prepared { "unstage" } else { "stage" }
                    ),
                    prepared,
                    path: file.path.clone(),
                    status: file.status.clone(),
                    additions: file.additions,
                    deletions: file.deletions,
                };
            }
            row -= section.files.len();
        }
    }
    if row == 0 {
        return ChangeRow::Section {
            title: "SIN SEGUIMIENTO".into(),
            count: review.untracked.len(),
        };
    }
    let index = row - 1;
    ChangeRow::File {
        action: format!("review_untracked_open:{index}"),
        stage_action: format!("review_stage_untracked:{index}"),
        prepared: false,
        path: review.untracked[index].clone(),
        status: "Nuevo".into(),
        additions: 0,
        deletions: 0,
    }
}

fn change_row(row: &ChangeRow, width: f32, pending: bool, surface: Color) -> Element {
    let palette = theme::action_palette(surface, theme::FOREGROUND);
    match row {
        ChangeRow::Section { title, count } => {
            let title = match title.as_str() {
                "CAMBIOS SIN PREPARAR" => "Sin preparar",
                "CAMBIOS PREPARADOS" => "Preparados",
                "SIN SEGUIMIENTO" => "Nuevos",
                other => other,
            };
            SectionHeader::new(
                title,
                ComponentPalette {
                    text: theme::FOREGROUND_SUBTLE,
                    ..theme::component_palette()
                },
            )
            .trailing(
                UiText::new(count.to_string(), theme::component_palette())
                    .role(TextRole::Micro)
                    .muted()
                    .mono()
                    .render()
                    .centered_y()
                    .size(None, Some(CHANGE_ROW_HEIGHT - 8.0)),
            )
            .truncate(true)
            .width(width)
            .sizing(crate::ui_sizing::CHANGE_SECTION)
            .render()
        }
        ChangeRow::File {
            action,
            stage_action,
            prepared,
            path,
            status,
            additions,
            deletions,
        } => {
            let (code, color) = match status.as_str() {
                "Nuevo" => ("A", theme::DIFF_ADDED),
                "Eliminado" => ("D", theme::DIFF_REMOVED),
                "Renombrado" => ("R", theme::FOREGROUND),
                "Copiado" => ("C", theme::FOREGROUND),
                "Conflicto" => ("!", theme::DESTRUCTIVE),
                "Binario" => ("B", theme::FOREGROUND_SUBTLE),
                _ => ("M", theme::FOREGROUND_SUBTLE),
            };
            let file_path = Path::new(path);
            let filename = file_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(path);
            let parent = file_path
                .parent()
                .and_then(|parent| parent.to_str())
                .filter(|parent| !parent.is_empty());
            // Keep the filename readable even with large line counts. Counts
            // belong in the tooltip; the path gets inline room in wider panes.
            let mut file = ListRow::new(action, filename, palette)
                .icon(openide_ui::file_icon(path))
                .icon_ink(theme::ICON_PRIMARY);
            if let Some(parent) = parent.filter(|_| width >= 320.0) {
                file = file.meta_element(
                    Element::truncated_text(parent)
                        .font_family(FontFamily::Sans)
                        .font_size(11.0)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some((width * 0.28).min(160.0)), Some(CHANGE_ACTION_SIZE)),
                );
            }
            // The row's shared surface is the control's hover.
            let control = UiIconButton::new(
                stage_action,
                if *prepared { Icon::Minus } else { Icon::Add },
                theme::in_row(palette),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .radius(RadiusRole::Square)
            .enabled(!pending)
            .tooltip_with(
                TooltipSpec::new(if *prepared {
                    "Quitar de preparados"
                } else {
                    "Preparar archivo"
                })
                .placement(TooltipPlacement::Below)
                .palette(theme::component_palette()),
            )
            .render();
            let mut details = Vec::new();
            if let Some(parent) = parent {
                details.push(parent.to_owned());
            }
            details.push(if *additions + *deletions > 0 {
                format!("{status} · +{additions} −{deletions}")
            } else {
                status.clone()
            });
            // Keep staging independent from opening the diff, including when
            // the staging control is disabled by an in-flight operation.
            file.meta_element(
                Element::truncated_text(code)
                    .font_family(FontFamily::Monospace)
                    .foreground(color)
                    .font_size(11.0)
                    .size(Some(12.0), Some(CHANGE_ACTION_SIZE)),
            )
            .trailing_action(
                stage_action,
                control
                    .centered_y()
                    .size(Some(CHANGE_ACTION_SIZE), Some(CHANGE_ROW_HEIGHT)),
            )
            .tooltip_with(
                TooltipSpec::new(filename)
                    .description(details.join("\n"))
                    .placement(if surface == theme::SIDEBAR {
                        TooltipPlacement::Right
                    } else {
                        TooltipPlacement::Left
                    })
                    .palette(theme::component_palette()),
            )
            .filled()
            .sizing(crate::ui_sizing::CHANGE_ROW)
            .render()
            .size(Some(width), crate::ui_sizing::CHANGE_ROW.height)
        }
    }
}

fn github_footer(status: &GithubStatus, surface: Color) -> Option<Element> {
    let palette = theme::action_palette(surface, theme::FOREGROUND);
    match status {
        GithubStatus::Ready(github) => {
            let mut rows = vec![Button::new(
                "github_repo_open",
                github.repository.full_name(),
                palette,
            )
            .icon(Icon::Github)
            .variant(ButtonVariant::Ghost)
            .sizing(crate::ui_sizing::SMALL_LABEL_BUTTON)
            .tooltip_with(
                TooltipSpec::new("Abrir repositorio en GitHub").palette(theme::component_palette()),
            )
            .render()];
            if let Some(pull) = github.pulls.first() {
                rows.push(
                    Button::new(
                        format!("github_pr_open:{}", pull.number),
                        format!("#{} {}", pull.number, shorten(&pull.title, 46)),
                        palette,
                    )
                    .variant(ButtonVariant::Ghost)
                    .sizing(crate::ui_sizing::SMALL_LABEL_BUTTON)
                    .render(),
                );
            }
            // Keep repository access issues visible without making them the
            // first thing above local changes, which work independently.
            if let Some(notice) = &github.notice {
                rows.push(
                    UiText::new(notice, theme::component_palette())
                        .role(TextRole::Micro)
                        .muted()
                        .render(),
                );
            }
            Some(Element::column(rows).gap(3.0).padding(8.0))
        }
        GithubStatus::Loading => Some(
            UiText::new("Consultando GitHub…", theme::component_palette())
                .role(TextRole::Micro)
                .muted()
                .render()
                .padding(8.0),
        ),
        GithubStatus::Unavailable(error) => Some(
            UiText::new(error, theme::component_palette())
                .role(TextRole::Micro)
                .muted()
                .render()
                .padding(8.0),
        ),
        GithubStatus::NoRemote => None,
    }
}

#[cfg(test)]
fn detail_content(detail: &ReviewDetailView, width: f32) -> Element {
    detail_view(detail, width, true)
}

/// `back` adds the way out to the list of changes. The IDE keeps that list in
/// its sidebar, so there a diff is a document on its own.
fn detail_view(detail: &ReviewDetailView, width: f32, back: bool) -> Element {
    let stage = if detail.stage == DiffStage::Staged {
        "Preparado"
    } else {
        "Local"
    };
    let summary = match &detail.content {
        Some(Ok(Some(diff))) => format!(
            "{} · {} · +{} −{}{}",
            diff.file.status,
            stage,
            diff.file.additions,
            diff.file.deletions,
            diff.file
                .old_path
                .as_ref()
                .filter(|old| *old != &diff.file.path)
                .map(|old| format!(" · Desde {old}"))
                .unwrap_or_default()
        ),
        _ => stage.to_owned(),
    };
    let mut path_items: Vec<_> = Path::new(&detail.path)
        .components()
        .map(|component| {
            BreadcrumbItem::new(component.as_os_str().to_string_lossy()).tooltip(&detail.path)
        })
        .collect();
    if let Some(item) = path_items.pop() {
        path_items.push(
            item.icon(openide_ui::file_icon(&detail.path))
                .tooltip(format!("{} · {summary}", detail.path)),
        );
    }
    let tool_size = crate::dock_tab_ui::TOOL_SIZE;
    let stats = detail.content.as_ref().and_then(|content| match content {
        Ok(Some(diff)) => Some((
            format!("+{}", diff.file.additions),
            format!("−{}", diff.file.deletions),
        )),
        _ => None,
    });
    // Reserve a conservative budget, but let native font measurements hug the
    // path and metadata. Spare space belongs before the toolbar, not between
    // a filename and its change counts.
    let fixed_width =
        8.0 + if back { 56.0 + 14.0 } else { 0.0 } + tool_size * 2.0 + 4.0 * 5.0;
    let stats_width = stats.as_ref().map_or(0.0, |(added, removed)| {
        (stage.len() + added.chars().count() + removed.chars().count()) as f32 * 7.8 + 16.0
    });
    let show_stats = stats.is_some() && width - fixed_width - 17.0 - stats_width >= 96.0;
    let path_width = (width - fixed_width
        - if show_stats { stats_width + 17.0 } else { 0.0 })
        .max(0.0);
    let mut header = Vec::new();
    if back {
        // This navigates out of the diff, rather than into a path ancestor.
        // Keep it reachable when the filename consumes the narrow header.
        header.push(
            Breadcrumb::new(
                "review_detail_navigation",
                vec![BreadcrumbItem::new("Cambios")
                    .action("review_detail_back")
                    .tooltip("Volver a los cambios del repositorio")],
                theme::component_palette(),
            )
            .width(56.0)
            .height(crate::dock_tab_ui::TOOL_SIZE)
            .render(),
        );
        header.push(
            Element::icon(Icon::ChevronRight)
                .foreground(theme::FOREGROUND_SUBTLE)
                .size(Some(14.0), Some(tool_size)),
        );
    }
    header.push(
        Breadcrumb::new("review_detail_path", path_items, theme::component_palette())
            .width(path_width)
            .height(tool_size)
            .render()
            .size(None, Some(tool_size))
            .max_width(path_width),
    );
    if show_stats {
        if let Some((added, removed)) = stats {
            header.push(
                Separator::vertical(theme::component_palette())
                    .length(12.0)
                    .centered_in(tool_size)
                    .render()
                    .padding(4.0)
                    .size(Some(9.0), Some(tool_size)),
            );
            let text = |value, color, family| {
                Element::truncated_text(value)
                    .foreground(color)
                    .font_family(family)
                    .font_size(12.0)
                    .size(None, Some(tool_size))
            };
            header.push(
                Element::row(vec![
                    text(stage.to_owned(), theme::FOREGROUND_SUBTLE, FontFamily::Sans),
                    text(added, theme::DIFF_ADDED, FontFamily::Monospace),
                    text(removed, theme::DIFF_REMOVED, FontFamily::Monospace),
                ])
                .gap(8.0)
                .size(None, Some(tool_size))
                .max_width(stats_width)
                .clip_children(),
            );
        }
    }
    header.push(Element::spacer().flex(1.0));
    header.extend([
        UiIconButton::new(
            "review_detail_open_editor",
            Icon::Code,
            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Dense)
        .tooltip_with(TooltipSpec::new("Abrir en editor").placement(TooltipPlacement::Below))
        .render(),
        UiIconButton::new(
            "review_detail_retry",
            Icon::Refresh,
            theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Dense)
        .tooltip_with(TooltipSpec::new("Actualizar diff").placement(TooltipPlacement::Below))
        .render(),
    ]);
    let mut children = vec![Element::panel(
        Element::row(header)
            .gap(4.0)
            .size(None, Some(crate::dock_tab_ui::TOOL_SIZE)),
    )
    .padding(4.0)
    .size(None, Some(crate::dock_tab_ui::HEADER_HEIGHT))];
    match &detail.content {
        None => children.push(pane_loading_state("review_detail", "Cargando diff", width)),
        Some(Err(error)) => children.push(pane_empty_state(
            Icon::Code,
            "No se pudo abrir el diff",
            error,
            Some(("review_detail_retry", "Reintentar")),
        )),
        Some(Ok(None)) => children.push(pane_empty_state(
            Icon::Check,
            "Sin diferencias",
            "El archivo ya no tiene cambios en esta sección.",
            Some(("review_detail_back", "Ver cambios")),
        )),
        Some(Ok(Some(diff))) => {
            let file = &diff.file;
            if diff.editor_lines.is_empty() {
                children.push(pane_empty_state(
                    Icon::File,
                    &file.status,
                    "Este cambio no contiene diferencias de texto.",
                    None,
                ));
                children.push(Element::spacer().flex(1.0));
            } else {
                let language = CodeLanguage::from_extension(
                    Path::new(&file.path)
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or_default(),
                );
                children.push(
                    CodeDiff::new(
                        format!("git-diff:{:?}:{}", detail.stage, detail.path),
                        diff.editor_lines.clone(),
                    )
                    .language(language)
                    .palette(theme::component_palette())
                    .render()
                    .background(theme::BACKGROUND)
                    .size(None, None)
                    .min_size(0.0, 80.0)
                    .flex(1.0),
                );
            }
            if diff.truncated {
                children.push(
                    UiText::new(
                        "Vista parcial: el archivo supera el límite de tamaño.",
                        theme::component_palette(),
                    )
                    .role(TextRole::Micro)
                    .muted()
                    .render(),
                );
            }
        }
    }
    Element::column(children)
        .gap(0.0)
        .background(theme::BACKGROUND)
        .flex(1.0)
}

fn change_list(review: Arc<GitReview>, width: f32, pending: bool, surface: Color) -> Element {
    let count = review
        .sections
        .iter()
        .filter(|section| {
            matches!(
                section.title.as_str(),
                "CAMBIOS SIN PREPARAR" | "CAMBIOS PREPARADOS"
            )
        })
        .map(|section| section.files.len() + 1)
        .sum::<usize>()
        + review.untracked.len()
        + usize::from(!review.untracked.is_empty());
    ScrollArea::uniform_list("git_change_files", count, CHANGE_ROW_HEIGHT, move |index| {
        // A list without overflow gives its row the full viewport. Keep the
        // rail budget stable there too, and honor it through the tooltip root.
        Element::row(vec![
            change_row(
                &indexed_change_row(&review, index),
                (width - SCROLL_RAIL_WIDTH).max(0.0),
                pending,
                surface,
            ),
            Element::spacer().flex(1.0),
        ])
    })
    .tint(theme::FOREGROUND_SUBTLE)
    .render()
    .top_fade(surface, 12.0)
    .flex(1.0)
}

pub(super) fn content(state: &ViewState, width: f32) -> Element {
    let ide = crate::ide_workspace::active(state);
    if let Some(detail) = &state.review_detail {
        return detail_view(detail, width, !ide);
    }
    if ide {
        // The list of changes lives in the source control sidebar.
        return UiText::new(
            "Elige un archivo en Control de versiones para ver sus cambios.",
            theme::component_palette(),
        )
        .muted()
        .render()
        .centered();
    }
    navigator(state, width)
}

/// The IDE keeps the repository navigator visible while a diff opens in the
/// center dock. It must not follow the selected detail into the sidebar.
pub(super) fn navigator(state: &ViewState, width: f32) -> Element {
    let inner_width = (width - crate::sidebar_chrome::INSET * 2.0).max(0.0);
    Element::column(vec![
        openide_ui::PanelHeader::new(
            if state.git_action_pending {
                "Actualizando…"
            } else {
                "Cambios"
            },
            theme::sidebar_palette(),
        )
        .trailing(vec![UiIconButton::new(
            "review_refresh",
            Icon::Refresh,
            theme::sidebar_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(TooltipSpec::new("Actualizar cambios").placement(TooltipPlacement::Below))
        .render()])
        .width(inner_width)
        .sizing(crate::ui_sizing::SIDEBAR_HEADER)
        .render(),
        navigator_body(state, inner_width),
    ])
    .gap(crate::sidebar_chrome::GAP)
    .padding(crate::sidebar_chrome::INSET)
    .flex(1.0)
}

/// Headerless body for an IDE activity that owns its shared sidebar chrome.
pub(super) fn navigator_body(state: &ViewState, width: f32) -> Element {
    let surface = if state.ide.mode == crate::ide_workspace::WindowMode::Ide {
        theme::SIDEBAR
    } else {
        theme::BACKGROUND
    };
    let mut children = Vec::new();
    if let Some(error) = &state.git_action_error {
        children.push(
            UiText::new(error, theme::component_palette())
                .role(TextRole::Small)
                .ink(theme::DESTRUCTIVE)
                .render()
                .padding(5.0),
        );
    }
    match &state.git_review {
        None => children.push(pane_loading_state(
            "review_changes",
            "Cargando cambios",
            width,
        )),
        Some(Err(error)) => children.push(pane_empty_state(
            Icon::GitBranch,
            "No se pudieron cargar los cambios",
            error,
            Some(("review_refresh", "Reintentar")),
        )),
        Some(Ok(review)) => {
            children.push(
                Element::row(vec![
                    Element::icon(Icon::GitBranch)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(16.0), Some(16.0)),
                    Tooltip::new(
                        Element::truncated_text(&review.branch)
                            .font_family(FontFamily::Sans)
                            .font_size(12.0)
                            .foreground(theme::FOREGROUND)
                            .flex(1.0),
                        "review_branch_name",
                        &review.branch,
                        theme::component_palette(),
                    )
                    .placement(TooltipPlacement::Below)
                    .render()
                    .flex(1.0),
                ])
                .gap(6.0)
                .padding(4.0)
                .size(None, Some(28.0)),
            );
            let count = review.unique_files() + review.untracked.len();
            children.push(
                Element::row(vec![
                    UiText::new(format!("{count} archivos"), theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .render()
                        .flex(1.0),
                    UiText::new(format!("+{}", review.additions), theme::component_palette())
                        .role(TextRole::Micro)
                        .ink(theme::DIFF_ADDED)
                        .mono()
                        .render(),
                    UiText::new(format!("−{}", review.deletions), theme::component_palette())
                        .role(TextRole::Micro)
                        .ink(theme::DIFF_REMOVED)
                        .mono()
                        .render(),
                ])
                .gap(6.0)
                .padding(4.0)
                .size(None, Some(24.0)),
            );
            if let Some(tracking) = &review.tracking {
                let mut summary = vec![Tooltip::new(
                    Element::truncated_text(&tracking.upstream)
                        .font_family(FontFamily::Sans)
                        .font_size(10.0)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .flex(1.0),
                    "review_tracking_branch",
                    &tracking.upstream,
                    theme::component_palette(),
                )
                .render()
                .flex(1.0)];
                for (id, icon, count, hint) in [
                    (
                        "git_ahead",
                        Icon::ArrowUp,
                        tracking.ahead,
                        "Commits por subir",
                    ),
                    (
                        "git_behind",
                        Icon::ArrowDown,
                        tracking.behind,
                        "Commits por bajar",
                    ),
                ] {
                    summary.push(
                        Tooltip::new(
                            Element::row(vec![
                                Element::icon(icon)
                                    .foreground(theme::FOREGROUND_SUBTLE)
                                    .size(Some(12.0), Some(16.0)),
                                UiText::new(count.to_string(), theme::component_palette())
                                    .role(TextRole::Micro)
                                    .muted()
                                    .mono()
                                    .render(),
                            ])
                            .gap(3.0),
                            id,
                            hint,
                            theme::component_palette(),
                        )
                        .placement(TooltipPlacement::Below)
                        .render(),
                    );
                }
                children.push(
                    Element::row(summary)
                        .gap(6.0)
                        .padding(4.0)
                        .size(None, Some(24.0)),
                );
            }
            children.push(Separator::horizontal(theme::component_palette()).render());
            if review.files == 0 && review.untracked.is_empty() {
                children.push(pane_empty_state(
                    Icon::CheckCircle,
                    "Todo al día",
                    "No hay cambios locales en este proyecto.",
                    None,
                ));
                children.push(Element::spacer().flex(1.0));
            } else {
                children.push(change_list(
                    review.clone(),
                    width,
                    state.git_action_pending || state.environment.workflow.busy.is_some(),
                    surface,
                ));
            }
            if review.truncated {
                children.push(UiText::new("Listado parcial por tamaño. Los archivos visibles se pueden inspeccionar completos.", theme::component_palette()).role(TextRole::Micro).muted().render());
            }
            if let Some(footer) = github_footer(&review.github, surface) {
                children.push(Separator::horizontal(theme::component_palette()).render());
                children.push(footer);
            }
        }
    }
    Element::column(children).gap(4.0).flex(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, Point, Size, UiAction, UiEvent, UiTree};

    fn legacy_section_row(title: &str, count: usize, width: f32) -> Element {
            Element::row(vec![
                Element::truncated_text(title)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .flex(1.0),
                UiText::new(count.to_string(), theme::component_palette())
                    .role(TextRole::Micro)
                    .muted()
                    .mono()
                    .render()
                    .centered_y()
                    .size(None, Some(CHANGE_ROW_HEIGHT - 8.0)),
            ])
            .gap(6.0)
            .padding(4.0)
            .size(Some(width), Some(CHANGE_ROW_HEIGHT))
    }

    #[test]
    fn section_rows_match_their_legacy_body() {
        for title in ["Sin preparar", "Una sección de cambios con un nombre demasiado largo"] {
            for width in [150.0, 300.0] {
                let row = ChangeRow::Section {
                    title: title.into(),
                    count: 128,
                };
                crate::converge_pages_rows_parity_tests::assert_same_at(
                    "git section row",
                    &[160.0, 420.0],
                    legacy_section_row(title, 128, width),
                    change_row(&row, width, false, theme::SIDEBAR),
                );
            }
        }
    }

    fn text_detail() -> ReviewDetailView {
        ReviewDetailView {
            path: "src/main.rs".into(),
            stage: DiffStage::Unstaged,
            page_index: 0,
            content: Some(Ok(Some(crate::git_review::FileDiffDetail {
                stage: DiffStage::Unstaged,
                file: ReviewFile {
                    path: "src/main.rs".into(),
                    status: "Modificado".into(),
                    additions: 1,
                    deletions: 1,
                    ..Default::default()
                },
                editor_lines: vec![
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Context,
                        old_line: Some(1),
                        new_line: Some(1),
                        text: "fn main() {".into(),
                    },
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Removed,
                        old_line: Some(2),
                        new_line: None,
                        text: "    println!(\"Hello\");".into(),
                    },
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Added,
                        old_line: None,
                        new_line: Some(2),
                        text: "    println!(\"Hola, mundo\");".into(),
                    },
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Context,
                        old_line: Some(3),
                        new_line: Some(3),
                        text: "}".into(),
                    },
                ]
                .into(),
                truncated: false,
            }))),
        }
    }

    #[test]
    fn a_diff_only_offers_the_way_back_where_the_list_shares_its_tab() {
        let detail = text_detail();
        for back in [true, false] {
            let frame = UiTree::new(detail_view(&detail, 620.0, back)).layout(Size::new(620.0, 300.0));
            let shows_crumb = frame.commands.iter().any(|command| {
                matches!(command, DrawCommand::Text { text, .. } if text == "Cambios")
            });
            assert_eq!(shows_crumb, back);
        }
    }

    #[test]
    fn diff_document_uses_one_editor_header_and_the_full_document_width() {
        for width in [260.0, 360.0, 520.0, 900.0] {
            let detail = text_detail();
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                detail_content(&detail, width),
            );
            let frame = tree.layout(Size::new(width, 300.0));
            let document = frame
                .regions
                .iter()
                .find(|r| r.id.starts_with("git-diff:"))
                .unwrap();
            assert_eq!(document.rect.x, 0.0);
            assert_eq!(document.rect.y, crate::dock_tab_ui::HEADER_HEIGHT);
            assert_eq!(document.rect.width, width);
            assert_eq!(
                document.rect.height,
                300.0 - crate::dock_tab_ui::HEADER_HEIGHT
            );
            let retry = frame
                .regions
                .iter()
                .find(|r| r.id == "review_detail_retry")
                .unwrap();
            assert_eq!(retry.rect.height, crate::dock_tab_ui::TOOL_SIZE);
            assert_eq!(retry.rect.y, 4.0);
            if width >= 360.0 {
                let file = frame.regions.iter()
                    .find(|region| region.id == "review_detail_path:item:1")
                    .unwrap().rect;
                let mut metadata = Vec::new();
                for command in &frame.commands {
                    if let DrawCommand::Text { text, position, font_size, .. } = command {
                        if ["Local", "+1", "−1"].contains(&text.as_str()) {
                            assert_eq!(*font_size, 12.0);
                            assert!(position.y > 4.0 && position.y < 16.0,
                                "metadata is centered inside the 24px breadcrumb cell: {position:?}");
                            metadata.push(*position);
                        }
                    }
                }
                assert_eq!(metadata.len(), 3);
                assert!(metadata[0].x >= file.x + file.width);
                assert!(metadata[0].x - file.x - file.width < 24.0,
                    "metadata follows the filename, not the right edge of the header");
                assert!(metadata[2].x < retry.rect.x - crate::dock_tab_ui::TOOL_SIZE);
            }
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    #[ignore = "export full Git diff resource headers and documents for visual review"]
    fn export_git_diff_editor_visual_review() {
        let directory = std::env::var_os("OPENIDE_GIT_DIFF_VISUAL_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/openide-git-diff-visual-qa"));
        std::fs::create_dir_all(&directory).unwrap();
        for width in [260.0, 360.0, 520.0, 900.0] {
            let mut detail = text_detail();
            detail.path = "README.md".into();
            if let Some(Ok(Some(diff))) = detail.content.as_mut() {
                diff.file.path = detail.path.clone();
                diff.file.additions = 62;
                diff.file.deletions = 5;
            }
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                detail_content(&detail, width),
            );
            tree.set_reduced_motion(true);
            let started = std::time::Instant::now();
            let frame = loop {
                let frame = tree.layout(Size::new(width, 320.0));
                if !tree.is_animating() {
                    break frame;
                }
                assert!(started.elapsed() < std::time::Duration::from_secs(3));
                std::thread::sleep(std::time::Duration::from_millis(1));
            };
            let svg_path = directory.join(format!("git-diff-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &svg_path, width, 320.0)
                .unwrap();
            let options = crate::settings_ui::visual_qa::native_svg_options();
            let parsed =
                resvg::usvg::Tree::from_str(&std::fs::read_to_string(svg_path).unwrap(), &options)
                    .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 320).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap
                .save_png(directory.join(format!("git-diff-{width:.0}.png")))
                .unwrap();
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn ten_thousand_changes_render_only_visible_rows_and_scroll_to_the_last_file() {
        let review = GitReview {
            untracked: (0..10_000)
                .map(|index| format!("file-{index:05}.rs"))
                .collect(),
            ..Default::default()
        };
        let mut tree = UiTree::new(change_list(
            Arc::new(review),
            360.0,
            false,
            theme::BACKGROUND,
        ));
        tree.set_reduced_motion(true);
        let viewport = Size::new(360.0, 600.0);
        let first = tree.layout(viewport);
        assert!(first
            .regions
            .iter()
            .any(|region| region.id == "review_untracked_open:0"));
        assert!(first
            .regions
            .iter()
            .any(|region| region.id == "review_stage_untracked:0"));
        assert!(
            first.commands.len() < 400,
            "{} draw commands",
            first.commands.len()
        );
        assert!(!first
            .regions
            .iter()
            .any(|region| region.id == "review_untracked_open:9999"));
        tree.handle_event(UiEvent::ScrollDirect {
            position: Point::new(180.0, 300.0),
            delta_y: -1_000_000.0,
        });
        let last = tree.layout(viewport);
        assert!(last
            .regions
            .iter()
            .any(|region| region.id == "review_untracked_open:9999"));
        assert!(last.commands.len() < 400);
    }

    #[test]
    fn list_does_not_render_inline_patch_contents() {
        let review = GitReview {
            sections: vec![crate::git_review::ReviewSection {
                title: "CAMBIOS PREPARADOS".into(),
                files: vec![ReviewFile {
                    path: "src/main.rs".into(),
                    status: "Modificado".into(),
                    lines: vec![ReviewLine {
                        text: "+secret code shown only on demand".into(),
                        kind: ReviewLineKind::Added,
                        old_number: None,
                        new_number: Some(1),
                    }],
                    additions: 1,
                    ..Default::default()
                }],
                additions: 1,
                ..Default::default()
            }],
            files: 1,
            ..Default::default()
        };
        let frame = UiTree::new(change_list(
            Arc::new(review),
            360.0,
            false,
            theme::BACKGROUND,
        ))
        .layout(Size::new(360.0, 600.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "review_file_open:0:0"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "review_unstage:0:0"));
        assert!(!frame.commands.iter().any(|command| matches!(command, DrawCommand::Text { text, .. } if text.contains("secret code"))));
    }
    #[test]
    fn narrow_change_rows_prioritize_the_basename_over_paths_and_line_counts() {
        let row = ChangeRow::File {
            action: "open-test".into(),
            stage_action: "stage-test".into(),
            prepared: false,
            path: "apps/agents-window/src/dock_chrome.rs".into(),
            status: "Modificado".into(),
            additions: 12,
            deletions: 3,
        };
        let frame = UiTree::new(change_row(&row, 300.0, false, theme::BACKGROUND))
            .layout(Size::new(300.0, CHANGE_ROW_HEIGHT));
        let text: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains(&"dock_chrome.rs"), "{text:?}");
        assert!(
            !text.iter().any(|text| text.starts_with("apps/agents")),
            "{text:?}"
        );
        assert!(!text.contains(&"+12") && !text.contains(&"−3"), "{text:?}");
        assert!(frame.regions.iter().any(|region| region.id == "stage-test"));
    }

    #[test]
    fn stage_and_unstage_tooltips_follow_the_actual_button_hover() {
        for prepared in [false, true] {
            let row = ChangeRow::File {
                action: "open-test".into(),
                stage_action: "stage-test".into(),
                prepared,
                path: "src/main.rs".into(),
                status: "Modificado".into(),
                additions: 1,
                deletions: 0,
            };
            let viewport = Size::new(360.0, 160.0);
            let mut tree = UiTree::new(Element::column(vec![
                change_row(&row, 360.0, false, theme::BACKGROUND),
                Element::spacer().flex(1.0),
            ]));
            let frame = tree.layout(viewport);
            let button = frame
                .regions
                .iter()
                .find(|region| region.id == "stage-test")
                .unwrap();
            assert_eq!(button.rect.width, 24.0);
            assert_eq!(button.rect.height, 24.0);
            assert_eq!(button.rect.y, 2.0);
            let file = frame
                .regions
                .iter()
                .find(|region| region.id == "open-test")
                .unwrap();
            assert!(file.rect.x < button.rect.x);
            assert!(file.rect.x + file.rect.width <= button.rect.x);
            assert_eq!(
                frame.hit_test(Point::new(
                    button.rect.x + button.rect.width / 2.0,
                    button.rect.y + button.rect.height / 2.0,
                )),
                Some("stage-test")
            );
            tree.handle_event(UiEvent::PointerMove(Point::new(
                button.rect.x + button.rect.width / 2.0,
                button.rect.y + button.rect.height / 2.0,
            )));
            let hovered = tree.layout_at(
                viewport,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            );
            let expected = if prepared {
                "Quitar de preparados"
            } else {
                "Preparar archivo"
            };
            assert!(hovered.commands.iter().any(|command| matches!(
                command, DrawCommand::Text { text, .. } if text == expected
            )));
            assert_eq!(hovered.regions.len(), 2);
        }
    }

    #[test]
    fn git_sidebar_file_hover_is_visible_on_the_sidebar_surface() {
        let row = ChangeRow::File {
            action: "hover-file".into(),
            stage_action: "hover-stage".into(),
            prepared: false,
            path: "src/main.rs".into(),
            status: "Modificado".into(),
            additions: 1,
            deletions: 0,
        };
        let viewport = Size::new(300.0, 100.0);
        let mut tree = UiTree::new(change_row(&row, 300.0, false, theme::SIDEBAR));
        tree.set_reduced_motion(true);
        let frame = tree.layout(viewport);
        let region = frame.regions.iter().find(|r| r.id == "hover-file").unwrap();
        tree.handle_event(UiEvent::PointerMove(Point::new(
            region.rect.x + 10.0,
            region.rect.y + 10.0,
        )));
        let hovered = tree.layout(viewport);
        let expected = theme::action_palette(theme::SIDEBAR, theme::FOREGROUND).hover;
        assert_ne!(expected, theme::SIDEBAR);
        assert!(hovered
            .commands
            .iter()
            .any(|c| matches!(c,DrawCommand::FillRect {color,..} if *color==expected)));
    }

    #[test]
    fn github_footer_icon_is_centered_inside_the_repository_button() {
        let status = GithubStatus::Ready(crate::github::GithubOverview {
            repository: crate::github::GithubRepository {
                owner: "team".into(),
                name: "project".into(),
                remote_name: "origin".into(),
                head_owner: "team".into(),
            },
            branch: "main".into(),
            pulls: vec![],
            notice: None,
        });
        let mut tree = UiTree::new(github_footer(&status, theme::BACKGROUND).unwrap());
        let frame = tree.layout(Size::new(320.0, 44.0));
        let target = frame
            .regions
            .iter()
            .find(|region| region.id == "github_repo_open")
            .unwrap();
        let icon = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Icon {
                    icon: Icon::Github,
                    rect,
                    ..
                } => Some(rect),
                _ => None,
            })
            .unwrap();
        assert!(
            ((icon.y + icon.height / 2.0) - (target.rect.y + target.rect.height / 2.0)).abs()
                < 0.01
        );
        assert_eq!(
            frame.hit_test(Point::new(
                icon.x + icon.width / 2.0,
                icon.y + icon.height / 2.0
            )),
            Some("github_repo_open")
        );
        assert!(tree.focus("github_repo_open"));
        let focused = tree.layout(Size::new(320.0, 120.0));
        assert!(focused.commands.iter().any(|command| matches!(
            command, DrawCommand::Text { text, .. } if text == "Abrir repositorio en GitHub"
        )));
    }

    #[test]
    fn diff_header_breadcrumb_preserves_navigation_and_toolbar_tooltips() {
        let detail = ReviewDetailView {
            path: "apps/agents-window/src/git_panel.rs".into(),
            stage: DiffStage::Unstaged,
            page_index: 0,
            content: None,
        };
        for width in [260.0, 360.0, 520.0] {
            let viewport = Size::new(width, 300.0);
            let mut tree = UiTree::new(detail_content(&detail, width));
            let frame = tree.layout(viewport);
            for action in [
                "review_detail_back",
                "review_detail_open_editor",
                "review_detail_retry",
            ] {
                let region = frame
                    .regions
                    .iter()
                    .find(|region| region.id == action)
                    .unwrap();
                assert!(region.rect.x >= 0.0);
                assert!(region.rect.x + region.rect.width <= width + 0.01);
            }
            assert!(tree.focus("review_detail_back"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: openide_ui::Key::Enter,
                    shift: false,
                    ctrl: false,
                }),
                vec![UiAction::Clicked {
                    id: "review_detail_back".into(),
                }]
            );
            assert!(frame.commands.iter().any(|command| matches!(
                command, DrawCommand::Text { text, .. } if text == "git_panel.rs"
            )));
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Icon {
                    icon: Icon::ChevronRight,
                    ..
                }
            )));
            assert!(!frame.commands.iter().any(|command| matches!(
                command, DrawCommand::Text { text, .. } if text == "/"
            )));
            for (action, hint) in [
                ("review_detail_open_editor", "Abrir en editor"),
                ("review_detail_retry", "Actualizar diff"),
                ("review_detail_back", "Volver a los cambios del repositorio"),
            ] {
                assert!(tree.focus(action));
                let focused = tree.layout(viewport);
                // The breadcrumb's help is a wrapped description beneath
                // its title on narrow panes, so it can span text commands.
                let text = focused
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .flat_map(str::split_whitespace)
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(text.contains(hint), "{action} at {width}px: {text:?}");
                assert!(focused
                    .commands
                    .iter()
                    .any(|command| matches!(command, DrawCommand::Shadow { radius, .. } if *radius == 4.0)));
            }
        }
    }
}

#[cfg(test)]
#[path = "git_panel_visual_tests.rs"]
mod visual_tests;
