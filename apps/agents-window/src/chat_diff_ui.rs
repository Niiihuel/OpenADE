//! Read-only editor diffs for individual, confirmed journal tool effects.
use crate::{
    git_review::ReviewLineKind, session_tool_diff::SessionToolDiff, theme, view::ViewState,
};
#[cfg(test)]
use openide_ui::Icon;
use openide_ui::{
    Button, ButtonVariant, CodeDiff, CodeDiffLine, CodeDiffLineKind, CodeLanguage, Component,
    ControlSize, Element, FontFamily,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub(crate) struct DiffFileView {
    pub path: String,
    pub old_path: Option<String>,
    pub additions: usize,
    pub deletions: usize,
    pub truncated: bool,
    pub unavailable: Option<String>,
    pub can_open: bool,
    pub lines: Arc<[CodeDiffLine]>,
}

pub(crate) struct ToolDiffEntry {
    pub files: Vec<DiffFileView>,
    pub unavailable: Option<String>,
    pub notes: Vec<String>,
    pub limited: bool,
}
impl ToolDiffEntry {
    pub(crate) fn from_result(result: Result<SessionToolDiff, String>) -> Self {
        match result {
            Err(error) => Self {
                files: Vec::new(),
                unavailable: Some(error),
                notes: Vec::new(),
                limited: true,
            },
            Ok(diff) => Self {
                files: diff
                    .files
                    .into_iter()
                    .map(|change| {
                        let lines: Arc<[CodeDiffLine]> = change
                            .file
                            .lines
                            .into_iter()
                            .filter_map(|line| {
                                let kind = match line.kind {
                                    ReviewLineKind::Added => CodeDiffLineKind::Added,
                                    ReviewLineKind::Removed => CodeDiffLineKind::Removed,
                                    ReviewLineKind::Context | ReviewLineKind::Hunk => {
                                        CodeDiffLineKind::Context
                                    }
                                    ReviewLineKind::Meta if line.text.starts_with("\\ ") => {
                                        CodeDiffLineKind::Context
                                    }
                                    ReviewLineKind::File | ReviewLineKind::Meta => return None,
                                };
                                let content = if matches!(
                                    line.kind,
                                    ReviewLineKind::Added
                                        | ReviewLineKind::Removed
                                        | ReviewLineKind::Context
                                ) {
                                    line.text.get(1..).unwrap_or_default().to_owned()
                                } else {
                                    line.text
                                };
                                Some(CodeDiffLine {
                                    kind,
                                    old_line: line.old_number,
                                    new_line: line.new_number,
                                    text: content,
                                })
                            })
                            .collect::<Vec<_>>()
                            .into();
                        DiffFileView {
                            can_open: change.file.status != "Eliminado",
                            path: change.file.path,
                            old_path: change.file.old_path,
                            additions: change.file.additions,
                            deletions: change.file.deletions,
                            truncated: change.truncated,
                            unavailable: change.unavailable,
                            lines,
                        }
                    })
                    .collect(),
                unavailable: diff.unavailable,
                notes: diff.coverage_notes,
                limited: diff.limited,
            },
        }
    }
}

#[derive(Default)]
pub(crate) struct ChatDiffUiState {
    pub workspace: Option<PathBuf>,
    pub session_id: Option<String>,
    pub open: BTreeSet<String>,
    pub entries: BTreeMap<String, ToolDiffEntry>,
    pub loading: BTreeSet<String>,
    pub limit_notice: Option<String>,
}

pub(crate) fn action(owner: &str, tool: &str, retry: bool) -> String {
    format!(
        "chat_diff_{}:{owner}:{tool}",
        if retry { "retry" } else { "toggle" }
    )
}
pub(crate) fn view_id(owner: &str, tool: &str, file: usize) -> String {
    format!("chat-diff:{owner}:{tool}:{file}")
}
pub(crate) fn file_action(owner: &str, tool: &str, file: usize) -> String {
    format!(
        "chat_diff_file:{}",
        serde_json::to_string(&(owner, tool, file)).expect("file diff scope serializes")
    )
}
fn label(value: impl Into<String>, ink: openide_ui::Color) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(ink)
}

const PARTIAL_FILE_DIFF_NOTE: &str =
    "Cobertura limitada: algunos archivos de esta edición no tienen un diff completo";

fn confirmed_content_only(file: &DiffFileView) -> bool {
    file.unavailable.is_some()
        && file.additions == 0
        && file.deletions == 0
        && !file.lines.is_empty()
        && file.lines.iter().all(|line| {
            line.kind == CodeDiffLineKind::Context
                && line.old_line.is_none()
                && line.new_line.is_some()
        })
}

/// Historical snapshots without a baseline have readable content, but no
/// invented diff. Keep this secondary fact in the card's disclosure tooltip.
pub(crate) fn evidence_detail(entry: &ToolDiffEntry) -> Option<String> {
    (entry.files.len() == 1 && confirmed_content_only(&entry.files[0])).then(|| {
        format!(
            "Contenido guardado; no hay una versión anterior para comparar. {}",
            entry.files[0].unavailable.as_deref().unwrap_or_default()
        )
    })
}

/// Confirmed cached evidence belongs to one exact visible chat and workspace.
pub(crate) fn entry<'a>(state: &'a ViewState, tool: &str) -> Option<&'a ToolDiffEntry> {
    let ui = &state.chat_diffs;
    (ui.workspace.as_deref() == Some(Path::new(&state.workspace))
        && ui.session_id == state.selected_session)
        .then(|| ui.entries.get(tool))
        .flatten()
}

/// Inline content only: the file-activity card owns the disclosure.
pub(crate) fn body(state: &ViewState, tool: &str) -> Element {
    let Some(owner) = state.selected_session.as_deref() else {
        return Element::spacer();
    };
    let ui = &state.chat_diffs;
    let scoped = ui.workspace.as_deref() == Some(Path::new(&state.workspace))
        && ui.session_id.as_deref() == Some(owner);
    let entry = entry(state, tool);
    let loading = scoped && ui.loading.contains(tool);
    let at_capacity = scoped
        && !loading
        && entry.is_none()
        && (ui.limit_notice.as_deref() == Some(tool)
            || (ui.open.len() >= 16 && !ui.open.contains(tool)));
    let mut rows = Vec::new();
    if let Some(entry) = entry {
        let has_file_action = state.transcript.iter().any(|item| {
            matches!(item, crate::transcript::TranscriptItem::Tool { id, name, arguments, .. }
                if id == tool && matches!(crate::tool_presentation::navigation_target(name, arguments),
                    Some(crate::tool_presentation::ToolNavigationTarget::File(_))))
        });
        if let Some(error) = &entry.unavailable {
            rows.push(label(error, theme::FOREGROUND_SUBTLE));
            rows.push(
                Button::new(
                    action(owner, tool, true),
                    "Reintentar",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Compact)
                .render(),
            );
        }
        let sole_complete_snapshot = entry.unavailable.is_none()
            && entry.files.len() == 1
            && !entry.files[0].truncated
            && confirmed_content_only(&entry.files[0]);
        for note in &entry.notes {
            // The card tooltip retains this snapshot limitation. Keep unrelated
            // coverage and truncation notes visible.
            if sole_complete_snapshot && note == PARTIAL_FILE_DIFF_NOTE {
                continue;
            }
            rows.push(label(note, theme::FOREGROUND_SUBTLE));
        }
        for (index, file) in entry.files.iter().enumerate() {
            if entry.files.len() > 1 {
                rows.push(
                    Element::row(vec![
                        Element::truncated_text(
                            file.old_path
                                .as_ref()
                                .map(|old| format!("{old} → {}", file.path))
                                .unwrap_or_else(|| file.path.clone()),
                        )
                        .font_family(FontFamily::Monospace)
                        .font_size(12.0)
                        .foreground(theme::FOREGROUND)
                        .flex(1.0),
                        label(format!("+{}", file.additions), theme::DIFF_ADDED),
                        label(format!("−{}", file.deletions), theme::DIFF_REMOVED),
                    ])
                    .gap(8.0)
                    .size(None, Some(28.0)),
                );
            }
            if let Some(error) = &file.unavailable {
                if !confirmed_content_only(file) || entry.files.len() > 1 {
                    rows.push(label(error, theme::FOREGROUND_SUBTLE));
                }
            }
            if !file.lines.is_empty() {
                let language = CodeLanguage::from_extension(
                    Path::new(&file.path)
                        .extension()
                        .and_then(|x| x.to_str())
                        .unwrap_or_default(),
                );
                rows.push(
                    CodeDiff::new(view_id(owner, tool, index), file.lines.clone())
                        .single_gutter(file.lines.iter().all(|line| line.old_line.is_none()))
                        .language(language)
                        .height((file.lines.len() as f32 * 20.0 + 12.0).clamp(80.0, 280.0))
                        .palette(theme::component_palette())
                        .render(),
                );
            } else if file.unavailable.is_none() {
                rows.push(label("Sin diferencias de texto", theme::FOREGROUND_SUBTLE));
            }
            if file.truncated {
                rows.push(label(
                    "Vista parcial: se alcanzó el límite de líneas del diff.",
                    theme::FOREGROUND_SUBTLE,
                ));
            }
            if file.can_open && (!has_file_action || entry.files.len() > 1) {
                rows.push(
                    Button::new(
                        file_action(owner, tool, index),
                        "Abrir archivo",
                        theme::component_palette(),
                    )
                    .variant(ButtonVariant::Subtle)
                    .size(ControlSize::Compact)
                    .render(),
                );
            }
        }
        if entry.files.is_empty() && entry.unavailable.is_none() {
            rows.push(label(
                "La edición confirmada no cambió el contenido de texto.",
                theme::FOREGROUND_SUBTLE,
            ));
        }
    } else if !at_capacity {
        rows.push(label(
            if loading {
                "Reconstruyendo la edición confirmada…"
            } else {
                "Preparando la vista de la edición confirmada…"
            },
            theme::FOREGROUND_SUBTLE,
        ));
    }
    if at_capacity {
        rows.push(label(
            "Se alcanzó el límite de 16 vistas o cargas. Cierra una vista o espera a que termine la carga.",
            theme::FOREGROUND_SUBTLE,
        ));
    }
    // Keep the code/notes inset above and beside the body. The disclosure
    // footer supplies its own inset, so avoid a second large gap below it.
    Element::column(vec![
        Element::spacer().size(None, Some(8.0)),
        Element::row(vec![
            Element::spacer().size(Some(8.0), None),
            Element::column(rows).gap(6.0).flex(1.0),
            Element::spacer().size(Some(8.0), None),
        ]),
        Element::spacer().size(None, Some(2.0)),
    ])
}

#[cfg(test)]
pub(crate) fn build(state: &ViewState, tool: &str) -> Element {
    let owner = state.selected_session.as_deref().unwrap_or("draft");
    let ui = &state.chat_diffs;
    let scoped = ui.workspace.as_deref() == Some(Path::new(&state.workspace))
        && ui.session_id.as_deref() == Some(owner);
    let opened = scoped && ui.open.contains(tool);
    let entry = scoped.then(|| ui.entries.get(tool)).flatten();
    let loading = scoped && ui.loading.contains(tool);
    let detail = if loading {
        "Cargando".to_owned()
    } else if let Some(entry) = entry {
        if entry.unavailable.is_some() {
            "No disponible".to_owned()
        } else {
            format!(
                "{} {}{}",
                entry.files.len(),
                if entry.files.len() == 1 {
                    "archivo"
                } else {
                    "archivos"
                },
                if entry.limited { " · parcial" } else { "" }
            )
        }
    } else {
        String::new()
    };
    let mut content = vec![
        openide_ui::Collapsible::new(
            action(owner, tool, false),
            "Cambios del archivo",
            if opened {
                body(state, tool)
            } else {
                Element::spacer()
            },
            theme::component_palette(),
        )
        .icon(Icon::Edit)
        .expanded(opened)
        .detail(detail)
        .render(),
    ];
    if scoped && ui.limit_notice.as_deref() == Some(tool) {
        content.push(label(
            "Se alcanzó el límite de 16 vistas o cargas. Cierra una vista o espera a que termine la carga.",
            theme::FOREGROUND_SUBTLE,
        ));
    }
    Element::column(content).gap(4.0)
}
