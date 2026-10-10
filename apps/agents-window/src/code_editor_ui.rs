//! Native document presentation. File ownership and persistence stay in the host.
use crate::{keymap::KeymapProfile, theme, view::ViewState};
use openide_ui::{
    AlertDialog, Breadcrumb, BreadcrumbItem, Button, ButtonVariant, CodeEditor, CodeLanguage,
    Component, ControlSize, EditorController, Element, FontFamily, Icon, IconButton,
    TooltipPlacement, TooltipSpec,
};
use std::path::Path;

const HEADER_HEIGHT: f32 = crate::dock_tab_ui::HEADER_HEIGHT;
const TOOL_SIZE: f32 = crate::dock_tab_ui::TOOL_SIZE;
pub(crate) const BREADCRUMB_HEIGHT: f32 = 22.0;
const BREADCRUMB_INSET: f32 = 6.0;

#[derive(Clone, Debug)]
pub(crate) struct CodeEditorView {
    pub tab_id: u64,
    pub path: String,
    pub controller: Option<EditorController>,
    pub loading: bool,
    pub saving: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub conflict: bool,
    pub recovered: bool,
    pub missing: bool,
}

pub(crate) fn view_id(tab_id: u64) -> String {
    format!("code_editor:{tab_id}")
}
fn label(value: impl Into<String>, color: openide_ui::Color) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(color)
}
fn path_items(id: &str, path: &str, dirty: bool, width: f32) -> Vec<BreadcrumbItem> {
    let file_path = Path::new(path);
    let filename = file_path.file_name().unwrap_or_default().to_string_lossy();
    let interactive = id.starts_with("code_editor_path:");
    let mut accumulated = std::path::PathBuf::new();
    let mut items = file_path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|part| match part {
            std::path::Component::Normal(part) => {
                accumulated.push(part);
                let item = BreadcrumbItem::new(part.to_string_lossy())
                    .tooltip(accumulated.to_string_lossy());
                Some(if interactive {
                    item.action(format!("editor_folder:{}", accumulated.display()))
                } else {
                    item
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut file_item =
        BreadcrumbItem::new(format!("{}{}", filename, if dirty { " •" } else { "" })).tooltip(path);
    if width >= 76.0 {
        file_item = file_item.icon(openide_ui::file_icon(path));
    }
    items.push(if interactive {
        file_item.action("editor_copy_relative")
    } else {
        file_item
    });
    items
}

pub(crate) fn path_breadcrumb(id: &str, path: &str, dirty: bool, width: f32) -> Element {
    path_breadcrumb_with_height(id, path, dirty, width, BREADCRUMB_HEIGHT)
}

pub(crate) fn path_breadcrumb_with_height(
    id: &str,
    path: &str,
    dirty: bool,
    width: f32,
    height: f32,
) -> Element {
    let items = path_items(id, path, dirty, width);
    Breadcrumb::new(id, items, theme::component_palette())
        .width(width)
        .height(height)
        .render()
}

fn editor_breadcrumb(
    view: &CodeEditorView,
    workbench: &crate::editor_workbench_ui::EditorWorkbenchView,
    dirty: bool,
    width: f32,
    height: f32,
) -> Element {
    let id = format!("code_editor_path:{}", view.tab_id);
    let mut items = path_items(&id, &view.path, dirty, width);
    if let (Some(snapshot), Some(analysis)) = (
        view.controller.as_ref().and_then(|c| c.snapshot().ok()),
        workbench.analysis.as_ref(),
    ) {
        if analysis.document_id == snapshot.document_id()
            && analysis.revision == snapshot.revision()
        {
            let caret = snapshot.selection().head;
            let mut chain = analysis
                .symbols
                .iter()
                .enumerate()
                .filter(|(_, symbol)| symbol.range.contains(&caret))
                .collect::<Vec<_>>();
            chain.sort_by_key(|(_, symbol)| {
                (symbol.range.start, std::cmp::Reverse(symbol.range.len()))
            });
            let filename = Path::new(&view.path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let root = Path::new(&view.path)
                .components()
                .next()
                .map(|p| p.as_os_str().to_string_lossy().chars().count())
                .unwrap_or(0);
            // Keep enough room for the filename, navigable root and overflow.
            // Symbols are optional; they must never displace the file identity.
            let budget = (width
                - filename.chars().count() as f32 * 10.0
                - 28.0
                - if items.len() > 1 {
                    root as f32 * 10.0 + 18.0
                } else {
                    0.0
                }
                - 36.0)
                .max(0.0);
            let total = chain
                .iter()
                .map(|(_, symbol)| symbol.name.chars().count() as f32 * 10.0 + 22.0)
                .sum::<f32>();
            if total > budget {
                chain = chain.into_iter().rev().take(1).collect();
            }
            if chain
                .iter()
                .map(|(_, symbol)| symbol.name.chars().count() as f32 * 10.0 + 22.0)
                .sum::<f32>()
                <= budget
            {
                for (index, symbol) in chain {
                    items.push(
                        BreadcrumbItem::new(&symbol.name)
                            .action(format!("editor_symbol:{index}"))
                            .tooltip(format!("{} · {}", view.path, symbol.name)),
                    );
                }
            }
        }
    }
    Breadcrumb::new(id, items, theme::component_palette())
        .width(width)
        .height(height)
        .overflow_open(workbench.breadcrumb_open)
        .render()
}

/// Editor actions share the IDE tab row. Their identity belongs to the source
/// tab, so reparsing a Markdown preview cannot interrupt a press in progress.
pub(crate) fn titlebar_actions(
    view: &CodeEditorView,
    workbench: &crate::editor_workbench_ui::EditorWorkbenchView,
    profile: KeymapProfile,
) -> Vec<Element> {
    let mut actions =
        crate::editor_workbench_ui::header_actions_with_debug(view, workbench, true, profile);
    if openide_preview::kind_for_path(Path::new(&view.path))
        == Some(openide_preview::PreviewKind::Markdown)
    {
        actions.insert(
            1,
            IconButton::new(
                format!("markdown_preview_open:{}", view.tab_id),
                Icon::PanelRightOpen,
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Abrir vista previa al costado")
                    .placement(TooltipPlacement::Above),
            )
            .render(),
        );
    }
    actions
}

pub(crate) fn build(
    view: &CodeEditorView,
    workbench: &crate::editor_workbench_ui::EditorWorkbenchView,
    width: f32,
    profile: KeymapProfile,
    vim_mode: bool,
) -> Element {
    build_with_debug(view, workbench, width, true, profile, vim_mode)
}

/// The same document may be inspected in Agents and IDE without replacing its
/// controller. Only IDE exposes debugger controls and execution decorations.
pub(crate) fn build_with_debug(
    view: &CodeEditorView,
    workbench: &crate::editor_workbench_ui::EditorWorkbenchView,
    width: f32,
    debugging: bool,
    profile: KeymapProfile,
    vim_mode: bool,
) -> Element {
    build_with_presentation(
        view,
        workbench,
        width,
        debugging,
        true,
        Vec::new(),
        profile,
        vim_mode,
    )
}

/// Both hosts own their document status outside the editor viewport: IDE in
/// its window footer and Agents in the dock footer.
pub(crate) fn build_ide(
    view: &CodeEditorView,
    workbench: &crate::editor_workbench_ui::EditorWorkbenchView,
    width: f32,
    profile: KeymapProfile,
    vim_mode: bool,
) -> Element {
    build_with_presentation(
        view,
        workbench,
        width,
        true,
        false,
        Vec::new(),
        profile,
        vim_mode,
    )
}

pub(crate) fn build_with_preview(
    view: &CodeEditorView,
    workbench: &crate::editor_workbench_ui::EditorWorkbenchView,
    width: f32,
    ide: bool,
    controls: Vec<Element>,
    profile: KeymapProfile,
    vim_mode: bool,
) -> Element {
    build_with_presentation(
        view, workbench, width, ide, !ide, controls, profile, vim_mode,
    )
}

/// Standalone fixture for the compact review strip. Production shares its row with the path.
#[cfg(test)]
pub(crate) fn review_row(
    review: &crate::editor_review_ui::EditorReviewView,
    available: f32,
) -> Element {
    Element::row(vec![
        Element::spacer().size(Some(BREADCRUMB_INSET), None),
        Element::spacer().flex(1.0),
        crate::editor_review_ui::controls(
            review,
            crate::editor_review_ui::controls_budget(available),
        ),
        Element::spacer().size(Some(BREADCRUMB_INSET), None),
    ])
    .size(
        None,
        Some(crate::editor_review_ui::height(review, available)),
    )
}

fn build_with_presentation(
    view: &CodeEditorView,
    workbench: &crate::editor_workbench_ui::EditorWorkbenchView,
    width: f32,
    debugging: bool,
    dock_header: bool,
    preview_controls: Vec<Element>,
    profile: KeymapProfile,
    vim_mode: bool,
) -> Element {
    let snapshot = view.controller.as_ref().and_then(|c| c.snapshot().ok());
    let dirty = snapshot.as_ref().is_some_and(|s| s.is_dirty());
    let review = workbench.review.as_ref().filter(|review| {
        snapshot.as_ref().is_some_and(|snapshot| {
            (snapshot.document_id(), snapshot.revision()) == (review.document_id, review.revision)
        })
    });
    // IDE keeps all actions in the tab strip, leaving a dedicated breadcrumb
    // below it. Agents retains its self-contained document header.
    let search_popover = |header| {
        crate::editor_workbench_ui::search_popover(header, view, workbench, (width - 8.0).max(0.0))
    };
    let (header, header_height) = if let Some(review) = review {
        let inset = if dock_header { 4.0 } else { BREADCRUMB_INSET };
        let row_height = if dock_header {
            TOOL_SIZE
        } else {
            crate::editor_review_ui::HEIGHT
        };
        let mut menu = if dock_header {
            crate::editor_workbench_ui::menu_entries(view, workbench, debugging, profile)
        } else {
            Vec::new()
        };
        let mut direct = Vec::new();
        if dock_header && width >= 600.0 {
            direct.extend(preview_controls);
            direct.extend(
                crate::editor_workbench_ui::header_actions_with_debug(
                    view, workbench, debugging, profile,
                )
                .into_iter()
                .take(1),
            );
            // Search already has a direct target; keep menu identities unique.
            menu.retain(|entry| {
                !matches!(entry,
                openide_ui::DropdownMenuEntry::Item(item) if item.action_id == "editor_find")
            });
        } else if dock_header && !preview_controls.is_empty() {
            // The source pane supplies only this Markdown preview shortcut.
            // Preserve its tab identity when its icon moves into overflow.
            menu.insert(
                0,
                openide_ui::DropdownMenuItem::new(
                    format!("markdown_preview_open:{}", view.tab_id),
                    "Abrir vista previa al costado",
                )
                .icon(Icon::PanelRightOpen)
                .into(),
            );
        }
        let available = (width - inset * 2.0 - direct.len() as f32 * (TOOL_SIZE + 4.0)).max(0.0);
        let budget = crate::editor_review_ui::controls_budget((available - 4.0).max(0.0));
        let review_width =
            crate::editor_review_ui::width_with_menu(review, budget, !menu.is_empty());
        let mut top = vec![
            editor_breadcrumb(
                view,
                workbench,
                dirty,
                (available - review_width - 4.0).max(0.0),
                row_height,
            )
            .flex(1.0),
            crate::editor_review_ui::controls_with_menu(
                review,
                budget,
                menu,
                (width - 16.0).max(0.0),
            )
            .size(Some(review_width), Some(row_height)),
        ];
        top.extend(direct);
        let row = Element::row(top).gap(4.0).size(None, Some(row_height));
        if dock_header {
            (
                search_popover(Element::panel(row).padding(inset)),
                row_height + inset * 2.0,
            )
        } else {
            (
                search_popover(
                    Element::row(vec![
                        Element::spacer().size(Some(inset), None),
                        row.flex(1.0),
                        Element::spacer().size(Some(inset), None),
                    ])
                    .size(None, Some(row_height)),
                ),
                row_height,
            )
        }
    } else if dock_header {
        let controls_width = preview_controls.len() as f32 * (TOOL_SIZE + 6.0);
        let pane_width = (width - 68.0 - controls_width).max(0.0);
        let mut top =
            vec![editor_breadcrumb(view, workbench, dirty, pane_width, TOOL_SIZE).flex(1.0)];
        top.extend(preview_controls);
        top.extend(crate::editor_workbench_ui::header_actions_with_debug(
            view, workbench, debugging, profile,
        ));
        let height = TOOL_SIZE + 8.0;
        (
            search_popover(
                Element::panel(Element::row(top).gap(6.0).size(None, Some(TOOL_SIZE))).padding(4.0),
            ),
            height,
        )
    } else {
        let available = (width - BREADCRUMB_INSET * 2.0).max(0.0);
        (
            search_popover(
                Element::row(vec![
                    Element::spacer().size(Some(BREADCRUMB_INSET), None),
                    editor_breadcrumb(view, workbench, dirty, available, BREADCRUMB_HEIGHT)
                        .flex(1.0),
                    Element::spacer().size(Some(BREADCRUMB_INSET), None),
                ])
                .size(None, Some(BREADCRUMB_HEIGHT)),
            ),
            BREADCRUMB_HEIGHT,
        )
    };
    let mut rows = vec![header.size(None, Some(header_height))];
    rows.extend(crate::editor_workbench_ui::tools(view, workbench, width));
    if let Some(error) = &view.error {
        rows.push(label(error, theme::DESTRUCTIVE).padding(8.0));
    }
    if let Some(notice) = &view.notice {
        rows.push(label(notice, theme::FOREGROUND_SUBTLE).padding(8.0));
    }
    if let Some(controller) = &view.controller {
        let language = workbench.language_override.unwrap_or_else(|| {
            CodeLanguage::from_extension(
                Path::new(&view.path)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default(),
            )
        });
        let mut editor = CodeEditor::new(view_id(view.tab_id), controller.clone())
            .vim_mode(vim_mode)
            .indent_style(workbench.indent)
            .auto_pairs(workbench.auto_pairs)
            .soft_wrap(workbench.soft_wrap)
            .minimap(!dock_header && workbench.minimap)
            .reveal_top_inset(crate::editor_workbench_ui::search_reveal_inset(
                workbench,
                (width - 8.0).max(0.0),
            ))
            .folds(
                if snapshot
                    .as_ref()
                    .zip(workbench.analysis.as_ref())
                    .is_some_and(|(snapshot, analysis)| {
                        (snapshot.document_id(), snapshot.revision())
                            == (analysis.document_id, analysis.revision)
                    })
                {
                    workbench.folds.clone()
                } else {
                    std::sync::Arc::from([])
                },
            )
            .context_actions(crate::editor_workbench_ui::context_actions(
                workbench.language_ready,
            ));
        if let Some((document, revision)) = workbench.structure_revision {
            editor = editor.structure(
                document,
                revision,
                workbench.fold_candidates.clone(),
                workbench.sticky_scopes.clone(),
            );
        } else if let Some(snapshot) = &snapshot {
            // Reserve the fold lane while analysis is pending. Source text,
            // wrapping and the caret must not move sideways on every edit.
            editor = editor.structure(
                snapshot.document_id(),
                snapshot.revision(),
                std::sync::Arc::<[openide_ui::CodeFoldCandidate]>::from([]),
                std::sync::Arc::<[openide_ui::CodeStickyScope]>::from([]),
            );
        }
        if let Some((document, revision)) = workbench.breakpoint_revision.filter(|_| debugging) {
            editor = editor.breakpoints(
                document,
                revision,
                format!("editor_breakpoint:{}:{document}:{revision}", view.tab_id),
                workbench.breakpoint_rows.clone(),
            );
            editor = editor.debugger(
                document,
                revision,
                workbench.debug_breakpoints.clone(),
                workbench.debug_execution,
            );
        }
        if let Some(review) = review {
            editor = editor
                .review(review.document_id, review.revision, review.hunks.clone())
                .review_actions(review.actions.clone());
        }
        if let Some(analysis) = &workbench.analysis {
            editor = editor.highlights(
                analysis.document_id,
                analysis.revision,
                workbench.highlight_spans.clone(),
            );
        }
        if let Some(search) = workbench.search.as_ref().filter(|_| workbench.search_open) {
            editor = editor.search_matches(search.revision, search.matches.clone());
        }
        if let Some((document, revision)) = workbench.diagnostic_revision {
            editor = editor.diagnostics(document, revision, workbench.diagnostic_spans.clone());
        }
        rows.push(
            editor
                .language(language)
                .status_bar(false)
                .palette(theme::component_palette())
                .render()
                .top_fade(theme::BACKGROUND, 12.0)
                .flex(1.0),
        );
    } else {
        rows.push(
            label(
                if view.loading {
                    "Abriendo archivo…"
                } else {
                    "El archivo no está disponible para editar."
                },
                theme::FOREGROUND_SUBTLE,
            )
            .padding(12.0),
        );
        rows.push(Element::spacer().flex(1.0));
    }
    Element::column(rows)
        .size(Some(width), None)
        .flex(1.0)
        .clip_children()
}

pub(crate) fn confirmation(state: &ViewState, base: Element, width: f32) -> Element {
    let base = if state.code_editor_reload_confirm.is_some() {
        AlertDialog::new(
            base,
            true,
            "¿Recargar el archivo?",
            "Se volverá a leer la versión del disco. El texto sin guardar se perderá.",
            "code_editor_reload_confirm",
            "code_editor_reload_cancel",
            theme::component_palette(),
        )
        .width((width - 24.0).min(420.0))
        .confirm_label("Descartar y recargar")
        .cancel_label("Cancelar")
        .destructive(theme::DESTRUCTIVE)
        .render()
    } else {
        base
    };
    if !state.code_editor_quit_confirm {
        return base;
    }
    AlertDialog::new(base, true, "Hay archivos sin guardar", if state.code_editor_pending_saves { "Esperando a que termine el guardado." } else { "Podés cerrar conservando los borradores para la próxima apertura, seguir editando o descartarlos." }, "code_editor_quit_keep", "code_editor_quit_cancel", theme::component_palette())
        .width((width - 24.0).min(480.0))
        .body(Button::new("code_editor_quit_discard", "Descartar los borradores y salir", theme::component_palette())
            .variant(ButtonVariant::Ghost).size(openide_ui::ControlSize::Compact).enabled(!state.code_editor_pending_saves).render().foreground(theme::DESTRUCTIVE))
        .confirm_enabled(!state.code_editor_pending_saves)
        .confirm_label(if state.code_editor_pending_saves { "Guardando…" } else { "Conservar y salir" })
        .cancel_label("Seguir editando").render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, HitKind, Icon, Key, Point, Size, UiAction, UiEvent, UiTree};
    use std::time::{Duration, Instant};

    fn header_fixture(dirty: bool) -> CodeEditorView {
        let mut buffer =
            openide_editor::TextBuffer::new("# Alertas\n\nTexto del proyecto.\n").unwrap();
        if dirty {
            buffer
                .insert("Nueva nota\n", openide_editor::EditKind::Paste, 1)
                .unwrap();
        }
        CodeEditorView {
            tab_id: 91,
            path: "docs/alerts.md".into(),
            controller: Some(EditorController::new(buffer)),
            loading: false,
            saving: false,
            error: None,
            notice: None,
            conflict: false,
            recovered: false,
            missing: false,
        }
    }

    fn native_header_tree(root: Element) -> (eframe::egui::Context, UiTree) {
        use eframe::egui;
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        let fonts = ctx.clone();
        let mut tree = UiTree::new(root);
        tree.set_reduced_motion(true);
        tree.set_text_measurer(move |text, size, family| {
            fonts.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        text.into(),
                        egui::FontId::new(
                            size,
                            match family {
                                FontFamily::Sans => egui::FontFamily::Proportional,
                                FontFamily::Monospace => egui::FontFamily::Monospace,
                            },
                        ),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
            })
        });
        (ctx, tree)
    }

    #[test]
    fn ide_editor_fills_available_workspace_after_resizing_and_hiding_panels() {
        use crate::ide_workspace::{OUTER_INSET, STATUSBAR_HEIGHT, TITLEBAR_HEIGHT, WindowMode};
        use crate::view::{DockTabView, SideTab};

        let mut state = crate::view::tests::empty_state();
        state.ide.mode = WindowMode::Ide;
        state.side_tab = SideTab::FilePreview;
        state.code_editor = Some(header_fixture(false));
        let controller = state
            .code_editor
            .as_ref()
            .unwrap()
            .controller
            .as_ref()
            .unwrap()
            .clone();
        let before = controller.snapshot().unwrap();
        state.editor_workbench.document_id = Some(before.document_id());
        state.dock_tabs = vec![DockTabView {
            id: 91,
            title: "alerts.md".into(),
            full_path: Some("docs/alerts.md".into()),
            kind: SideTab::FilePreview,
            active: true,
            closable: true,
        }];
        let (ctx, mut tree) = native_header_tree(Element::spacer());
        let mut previous = None::<openide_ui::Rect>;
        for (step, (width, height, explorer, agent, bottom)) in [
            (1440.0, 900.0, true, true, true),
            (1440.0, 900.0, true, false, true),
            (1440.0, 900.0, false, false, false),
            (2048.0, 1200.0, false, false, false),
            (800.0, 680.0, false, false, false),
        ]
        .into_iter()
        .enumerate()
        {
            state.ide.explorer_open = explorer;
            state.ide.agent_open = agent;
            state.ide.bottom_open = bottom;
            state.show_terminal_panel = false;
            tree.set_root(crate::view::build_window_view(&state, width, height));
            let frame = tree.layout(Size::new(width, height));
            let editor = frame
                .regions
                .iter()
                .find(|region| region.id == "code_editor:91")
                .unwrap()
                .rect;
            let layout = crate::view::dock_layout(&state, width);
            let terminal = crate::view::terminal_panel_height(&state, height);
            assert_eq!(
                editor.width, layout.center,
                "source length must not limit the editor's width"
            );
            assert_eq!(
                editor.y,
                OUTER_INSET
                    + TITLEBAR_HEIGHT
                    + crate::dock_tab_ui::HEADER_HEIGHT
                    + BREADCRUMB_HEIGHT
            );
            assert!(
                (editor.y + editor.height - (height - OUTER_INSET - STATUSBAR_HEIGHT - terminal))
                    .abs()
                    < 0.01,
                "the editor must extend to the next panel or the status bar: {editor:?}"
            );
            let empty_corner = Point::new(
                editor.x + editor.width - 16.0,
                editor.y + editor.height - 16.0,
            );
            assert_eq!(
                frame.hit_test(empty_corner),
                Some("code_editor:91"),
                "blank space below and to the right of short source still belongs to the editor"
            );
            if let Some(previous) = previous {
                if step <= 3 {
                    assert!(
                        editor.width > previous.width,
                        "hiding panels and enlarging the window release their full width"
                    );
                }
                if step == 2 || step == 3 {
                    assert!(editor.height > previous.height);
                }
            }
            previous = Some(editor);
            let after = controller.snapshot().unwrap();
            assert_eq!(after.document_id(), before.document_id());
            assert_eq!(after.revision(), before.revision());
            assert_eq!(after.selection(), before.selection());
            assert_eq!(
                after.line_count(),
                before.line_count(),
                "physical line endings are document content"
            );
        }
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn ide_breadcrumb_has_its_own_compact_row_without_duplicate_editor_actions() {
        for width in [240.0, 320.0, 520.0, 900.0] {
            let view = header_fixture(true);
            let workbench = crate::editor_workbench_ui::EditorWorkbenchView::default();
            let (ctx, mut tree) = native_header_tree(build_ide(
                &view,
                &workbench,
                width,
                KeymapProfile::default(),
                false,
            ));
            let frame = tree.layout(Size::new(width, 640.0));
            let editor = frame
                .regions
                .iter()
                .find(|region| region.id == "code_editor:91")
                .unwrap()
                .rect;
            assert_eq!(editor.y, BREADCRUMB_HEIGHT);
            assert_eq!(editor.width, width);
            assert_eq!(editor.y + editor.height, 640.0);
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "alerts.md •"
            )));
            assert!(!frame.regions.iter().any(|region| {
                matches!(region.id.as_str(), "editor_find" | "editor_menu")
                    || region.id.starts_with("markdown_preview_open:")
                    || region.id.starts_with("document_preview:")
            }));
            let copy = frame
                .regions
                .iter()
                .find(|region| region.id == "editor_copy_relative")
                .unwrap()
                .rect;
            assert!(copy.y >= 0.0 && copy.y + copy.height <= BREADCRUMB_HEIGHT);
            assert!(copy.x >= BREADCRUMB_INSET && copy.x + copy.width <= width - BREADCRUMB_INSET);
            ctx.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn resource_header_is_one_row_with_bounded_actions_and_no_permanent_save_button() {
        for width in [240.0, 320.0, 520.0, 900.0] {
            for dirty in [false, true] {
                let view = header_fixture(dirty);
                let state = crate::editor_workbench_ui::EditorWorkbenchView::default();
                let (ctx, mut tree) = native_header_tree(build(
                    &view,
                    &state,
                    width,
                    KeymapProfile::default(),
                    false,
                ));
                let size = Size::new(width, 640.0);
                let frame = tree.layout(size);
                let body = frame
                    .regions
                    .iter()
                    .find(|r| r.id == "code_editor:91")
                    .unwrap()
                    .rect;
                assert_eq!(
                    body.y, HEADER_HEIGHT,
                    "no second static toolbar may consume document space"
                );
                assert!(body.height > 500.0);
                assert!(!frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == "Guardar" || text == "Ajustar" || text == "Símbolos"
                )));
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == if dirty { "alerts.md •" } else { "alerts.md" }
                )));
                let mut previous_right = 0.0;
                for (id, icon) in [("editor_find", Icon::Search), ("editor_menu", Icon::More)] {
                    let rect = frame
                        .regions
                        .iter()
                        .find(|r| r.id == id && r.kind == HitKind::Button)
                        .unwrap()
                        .rect;
                    assert_eq!((rect.width, rect.height), (TOOL_SIZE, TOOL_SIZE));
                    assert_eq!(rect.y, (HEADER_HEIGHT - TOOL_SIZE) / 2.0);
                    assert!(rect.x >= previous_right && rect.x + rect.width <= width);
                    previous_right = rect.x + rect.width;
                    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                    assert_eq!(frame.hit_test(point), Some(id));
                    assert!(frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Icon { icon: actual, rect: glyph, .. } if *actual == icon
                            && (glyph.x + glyph.width / 2.0 - point.x).abs() < 0.01
                            && (glyph.y + glyph.height / 2.0 - point.y).abs() < 0.01
                    )));
                    tree.handle_event(UiEvent::PointerDown(point));
                    assert_eq!(
                        tree.handle_event(UiEvent::PointerUp(point)),
                        vec![UiAction::Clicked { id: id.into() }]
                    );
                }
                ctx.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    fn scoped_save_menu_exposes_shortcut_and_respects_clean_loading_and_saving() {
        for (dirty, loading, saving, status) in [
            (false, false, false, Some("Sin cambios")),
            (true, false, false, None),
            (true, true, false, Some("Abriendo…")),
            (true, false, true, Some("Guardando…")),
        ] {
            let mut view = header_fixture(dirty);
            view.loading = loading;
            view.saving = saving;
            let state = crate::editor_workbench_ui::EditorWorkbenchView {
                menu_open: true,
                ..Default::default()
            };
            let (ctx, mut tree) =
                native_header_tree(build(&view, &state, 320.0, KeymapProfile::default(), false));
            let size = Size::new(320.0, 640.0);
            let frame = tree.layout(size);
            let menu = frame
                .regions
                .iter()
                .find(|r| r.id == "popover_surface:editor_menu_close")
                .unwrap()
                .rect;
            assert!(menu.x >= 0.0 && menu.x + menu.width <= 320.0);
            assert!(menu.height < 400.0);
            let save = frame
                .regions
                .iter()
                .find(|r| r.id == "code_editor_save:91" && r.kind == HitKind::Button);
            assert_eq!(save.is_some(), dirty && !loading && !saving);
            if let Some(status) = status {
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == status
                )));
            } else {
                let save = save.unwrap().rect;
                let point = Point::new(save.x + save.width / 2.0, save.y + save.height / 2.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked {
                        id: "code_editor_save:91".into()
                    }]
                );
                assert!(tree.focus("code_editor_save:91"));
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Clicked {
                        id: "code_editor_save:91".into()
                    }]
                );
                let shortcut = if cfg!(target_os = "macos") {
                    &["⌃", "S"][..]
                } else {
                    &["Ctrl", "S"][..]
                };
                for &key in shortcut {
                    assert!(
                        frame.commands.iter().any(|command| matches!(command,
                            DrawCommand::Text { text, position, .. }
                                if text == key
                                    && position.x >= save.x && position.x < save.x + save.width
                                    && position.y >= save.y && position.y < save.y + save.height
                        )),
                        "missing {key} inside the save action"
                    );
                }
                let caps: Vec<_> = frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::FillRect { rect, radius, .. }
                            if *radius == 3.0
                                && rect.height == 18.0
                                && rect.y >= save.y
                                && rect.y + rect.height <= save.y + save.height =>
                        {
                            Some(rect)
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(caps.len(), 2, "Control and S remain distinct keycaps");
                assert!(
                    caps.iter()
                        .all(|cap| cap.x >= save.x && cap.x + cap.width <= save.x + save.width)
                );
            }
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .any(|r| r.id == "code_editor_reload:91" && r.kind == HitKind::Button),
                !loading && !saving
            );
            ctx.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn compact_header_preserves_error_notice_and_conflict_actions() {
        let mut view = header_fixture(true);
        view.error = Some("No se pudo guardar el archivo".into());
        view.notice = Some("Se conservaron los cambios del editor".into());
        view.conflict = true;
        let state = crate::editor_workbench_ui::EditorWorkbenchView::default();
        let (ctx, mut tree) =
            native_header_tree(build(&view, &state, 320.0, KeymapProfile::default(), false));
        let frame = tree.layout(Size::new(320.0, 640.0));
        for expected in [
            "No se pudo guardar el archivo",
            "Se conservaron los cambios del editor",
        ] {
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == expected
            )));
        }
        let compare = frame
            .regions
            .iter()
            .find(|r| r.id == "editor_conflict" && r.kind == HitKind::Button)
            .unwrap()
            .rect;
        let point = Point::new(
            compare.x + compare.width / 2.0,
            compare.y + compare.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "editor_conflict".into()
            }]
        );
        assert!(
            frame
                .regions
                .iter()
                .find(|r| r.id == "code_editor:91")
                .unwrap()
                .rect
                .height
                > 300.0
        );
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn editor_path_uses_the_shared_breadcrumb_for_the_complete_hierarchy() {
        let path = "apps/agents-window/src/commands.rs";
        let mut tree = UiTree::new(path_breadcrumb("editor_path", path, true, 900.0));
        tree.set_text_measurer(|text, _, _| text.chars().count() as f32 * 8.0);
        let frame = tree.layout(Size::new(900.0, 28.0));
        for expected in ["apps", "agents-window", "src", "commands.rs •"] {
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == expected
                )),
                "{expected}"
            );
        }
        let current = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text == "commands.rs •" => {
                    Some(position.x)
                }
                _ => None,
            })
            .unwrap();
        assert!(
            current < 300.0,
            "path should pack to its natural width: {current}"
        );
    }

    #[test]
    fn symbol_breadcrumb_requires_current_analysis_and_keeps_the_filename_at_narrow_width() {
        let source = "fn prepare() {\n    let answer = 42;\n}\n";
        let mut buffer = openide_editor::TextBuffer::new(source).unwrap();
        buffer
            .set_selection(openide_editor::Selection::caret(
                source.find("answer").unwrap(),
            ))
            .unwrap();
        let controller = EditorController::new(buffer);
        let analysis = openide_language::ParserEngine::default()
            .analyze(
                &controller.snapshot().unwrap(),
                openide_language::Language::Rust,
                &openide_language::Cancellation::default(),
            )
            .unwrap();
        let index = analysis
            .symbols
            .iter()
            .position(|symbol| symbol.name == "prepare")
            .unwrap();
        let view = CodeEditorView {
            tab_id: 81,
            path: "apps/agents-window/src/commands.rs".into(),
            controller: Some(controller.clone()),
            loading: false,
            saving: false,
            error: None,
            notice: None,
            conflict: false,
            recovered: false,
            missing: false,
        };
        let workbench = crate::editor_workbench_ui::EditorWorkbenchView {
            analysis: Some(std::sync::Arc::new(analysis)),
            ..Default::default()
        };
        let wide = UiTree::new(editor_breadcrumb(
            &view,
            &workbench,
            false,
            900.0,
            BREADCRUMB_HEIGHT,
        ))
        .layout(Size::new(900.0, 200.0));
        assert!(
            wide.regions
                .iter()
                .any(|region| region.id == format!("editor_symbol:{index}"))
        );
        assert!(
            wide.regions
                .iter()
                .any(|region| region.id == "editor_copy_relative")
        );
        let narrow = UiTree::new(editor_breadcrumb(
            &view,
            &workbench,
            false,
            140.0,
            BREADCRUMB_HEIGHT,
        ))
        .layout(Size::new(140.0, 200.0));
        assert!(
            !narrow
                .regions
                .iter()
                .any(|region| region.id.starts_with("editor_symbol:"))
        );
        assert!(
            narrow
                .regions
                .iter()
                .any(|region| region.id == "editor_copy_relative")
        );
        controller
            .shared_buffer()
            .lock()
            .unwrap()
            .insert("changed", openide_editor::EditKind::Paste, 1)
            .unwrap();
        let stale = UiTree::new(editor_breadcrumb(
            &view,
            &workbench,
            true,
            900.0,
            BREADCRUMB_HEIGHT,
        ))
        .layout(Size::new(900.0, 200.0));
        assert!(
            !stale
                .regions
                .iter()
                .any(|region| region.id.starts_with("editor_symbol:"))
        );
        assert!(
            stale
                .regions
                .iter()
                .any(|region| region.id == "editor_copy_relative")
        );
    }

    fn pending_review(view: &CodeEditorView) -> crate::editor_review_ui::EditorReviewView {
        let snapshot = view.controller.as_ref().unwrap().snapshot().unwrap();
        crate::editor_review_ui::EditorReviewView {
            token: "owner-1/document-91/review-4".into(),
            document_id: snapshot.document_id(),
            revision: snapshot.revision(),
            hunks: vec![openide_ui::CodeReviewHunk {
                new_rows: 0..1,
                old_start_row: 0,
                removed_lines: vec!["# Antes".into()].into(),
            }]
            .into(),
            actions: vec![openide_ui::CodeReviewHunkActions {
                accept_id: "editor_review:accept_hunk:owner-1/document-91/review-4:0".into(),
                reject_id: "editor_review:reject_hunk:owner-1/document-91/review-4:0".into(),
                previous_id: Some("editor_review:previous:owner-1/document-91/review-4:0".into()),
                next_id: Some("editor_review:next:owner-1/document-91/review-4:0".into()),
                enabled: true,
                reject_enabled: true,
                disabled_reason: None,
            }]
            .into(),
            additions: 1,
            deletions: 1,
            hunk_count: 1,
            current_hunk: 0,
            can_accept: true,
            can_reject: true,
            ..Default::default()
        }
    }

    #[test]
    fn review_breadcrumb_reserves_its_actions_without_overlapping_the_resource_or_editor() {
        for width in [240.0, 360.0, 460.0, 600.0, 900.0, 1200.0] {
            for (ide, file_count) in [
                (false, 0),
                (true, 0),
                (false, 1),
                (true, 1),
                (false, 3),
                (true, 3),
            ] {
                let view = header_fixture(false);
                let controller = view.controller.as_ref().unwrap();
                let before = controller.snapshot().unwrap();
                let mut review = pending_review(&view);
                review.file_count = file_count;
                review.current_file = usize::from(file_count > 1);
                let workbench = crate::editor_workbench_ui::EditorWorkbenchView {
                    review: Some(review.clone()),
                    ..Default::default()
                };
                let build = if ide { build_ide } else { build };
                let (context, mut tree) = native_header_tree(build(
                    &view,
                    &workbench,
                    width,
                    KeymapProfile::default(),
                    false,
                ));
                let viewport = Size::new(width, 440.0);
                let frame = tree.layout(viewport);
                let resource = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "editor_copy_relative")
                    .unwrap()
                    .rect;
                let editor = frame
                    .regions
                    .iter()
                    .find(|region| region.id == view_id(view.tab_id))
                    .unwrap()
                    .rect;
                if let Some(position) = frame
                    .regions
                    .iter()
                    .find(|region| {
                        region.id == crate::editor_review_ui::action(&review.token, "position")
                    })
                    .map(|region| region.rect)
                {
                    let (count_origin, count_size) = frame
                        .commands
                        .iter()
                        .find_map(|command| match command {
                            DrawCommand::Text {
                                text,
                                position,
                                font_size,
                                ..
                            } if text == "1/1" => Some((*position, *font_size)),
                            _ => None,
                        })
                        .unwrap();
                    let count_width = context.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                "1/1".into(),
                                eframe::egui::FontId::proportional(count_size),
                                eframe::egui::Color32::WHITE,
                            )
                            .size()
                            .x
                    });
                    assert!(
                        (count_origin.x + count_width / 2.0 - position.x - position.width / 2.0)
                            .abs()
                            < 0.01,
                        "block counter is centered between the navigation controls"
                    );
                }
                let file_position = crate::editor_review_ui::action(&review.token, "file_position");
                if let Some(counter) = frame
                    .regions
                    .iter()
                    .find(|region| region.id == file_position)
                    .map(|region| region.rect)
                {
                    let (origin, size) = frame
                        .commands
                        .iter()
                        .find_map(|command| match command {
                            DrawCommand::Text {
                                text,
                                position,
                                font_size,
                                ..
                            } if text == "2/3" => Some((*position, *font_size)),
                            _ => None,
                        })
                        .expect("the file count remains complete");
                    let text_width = context.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                "2/3".into(),
                                eframe::egui::FontId::proportional(size),
                                eframe::egui::Color32::WHITE,
                            )
                            .size()
                            .x
                    });
                    assert!(
                        (origin.x + text_width / 2.0 - counter.x - counter.width / 2.0).abs()
                            < 0.01
                    );
                    assert!(counter.x >= 0.0 && counter.x + counter.width <= width + 0.01);
                    assert!(counter.y >= 0.0 && counter.y + counter.height <= editor.y);
                } else if file_count <= 1 {
                    assert!(
                        !frame.regions.iter().any(|region| {
                            ["file_position", "previous_file", "next_file"].iter().any(
                                |operation| {
                                    region.id
                                        == crate::editor_review_ui::action(&review.token, operation)
                                },
                            )
                        }),
                        "file navigation is omitted unless another reviewed file is available"
                    );
                }
                let mut action_rects = Vec::new();
                let mut controls = vec![
                    ("previous", None, Some(Icon::ArrowUp)),
                    ("next", None, Some(Icon::ArrowDown)),
                    ("reject_file", Some("Deshacer"), None),
                    ("accept_file", Some("Conservar"), None),
                ];
                if file_count > 1 {
                    controls.extend([
                        ("previous_file", None, Some(Icon::ArrowLeft)),
                        ("next_file", None, Some(Icon::ArrowRight)),
                    ]);
                }
                for (operation, label, icon) in controls {
                    let id = crate::editor_review_ui::action(&review.token, operation);
                    let enabled = match operation {
                        "previous" => review.current_hunk > 0,
                        "next" => review.current_hunk + 1 < review.hunk_count,
                        "previous_file" => review.current_file > 0,
                        "next_file" => review.current_file + 1 < review.file_count,
                        _ => true,
                    };
                    let Some(region) = frame.regions.iter().find(|region| region.id == id) else {
                        assert!(label.is_none(), "file decisions always remain visible");
                        assert!(
                            frame.regions.iter().any(|region| region.id
                                == crate::editor_review_ui::action(&review.token, "menu_toggle")),
                            "hidden navigation remains reachable through the overflow menu"
                        );
                        continue;
                    };
                    assert_eq!(
                        region.kind,
                        if enabled {
                            HitKind::Button
                        } else {
                            HitKind::Hover
                        },
                        "boundary navigation retains its tooltip without an action: {operation}"
                    );
                    let rect = region.rect;
                    let overlaps = |other: openide_ui::Rect| {
                        rect.x < other.x + other.width - 0.01
                            && rect.x + rect.width > other.x + 0.01
                            && rect.y < other.y + other.height - 0.01
                            && rect.y + rect.height > other.y + 0.01
                    };
                    assert!(
                        !overlaps(resource),
                        "review must preserve the file identity: {width}/{ide} {operation} {resource:?} {rect:?}"
                    );
                    assert!(
                        action_rects.iter().all(|other| !overlaps(*other)),
                        "review actions cannot overlap on their single row"
                    );
                    assert!(
                        rect.x >= 0.0
                            && rect.x + rect.width <= width + 0.01
                            && rect.y >= 0.0
                            && rect.y + rect.height <= editor.y
                    );
                    assert_eq!(rect.height, 24.0);
                    if let Some(icon) = icon {
                        assert_eq!(rect.width, 24.0);
                        let ink = if enabled {
                            theme::FOREGROUND
                        } else {
                            theme::FOREGROUND_SUBTLE
                        };
                        assert!(frame.commands.iter().any(|command| matches!(command,
                            DrawCommand::Icon { icon: painted, rect: glyph, tint }
                                if *painted == icon && *tint == ink
                                    && (glyph.x + glyph.width / 2.0 - rect.x - rect.width / 2.0).abs() < 0.01
                                    && (glyph.y + glyph.height / 2.0 - rect.y - rect.height / 2.0).abs() < 0.01
                        )), "navigation arrow is centered and reflects availability: {operation}");
                    }
                    if let Some(label) = label {
                        assert!(
                            rect.width >= 72.0 && rect.width <= 80.0,
                            "compact text decisions must not reserve oversized fixed columns"
                        );
                        let full_label = format!("{label} archivo");
                        let (painted, origin, font_size, ink) = frame
                            .commands
                            .iter()
                            .find_map(|command| match command {
                                DrawCommand::Text {
                                    text,
                                    position,
                                    font_size,
                                    color,
                                    ..
                                } if (text == label || text == &full_label)
                                    && position.x >= rect.x
                                    && position.x < rect.x + rect.width
                                    && position.y >= rect.y
                                    && position.y < rect.y + rect.height =>
                                {
                                    Some((text, *position, *font_size, *color))
                                }
                                _ => None,
                            })
                            .unwrap_or_else(|| {
                                panic!(
                                    "the complete {label:?} label must remain visible at {width}px"
                                )
                            });
                        assert_eq!(
                            painted, label,
                            "all pane sizes retain the same concise decisions"
                        );
                        let text_width = context.fonts_mut(|fonts| {
                            fonts
                                .layout_no_wrap(
                                    painted.clone(),
                                    eframe::egui::FontId::proportional(font_size),
                                    eframe::egui::Color32::WHITE,
                                )
                                .size()
                                .x
                        });
                        assert!(
                            origin.x >= rect.x
                                && origin.x + text_width <= rect.x + rect.width + 0.01,
                            "text action fits its own hit target at {width}px: {painted}"
                        );
                        if operation == "accept_file" {
                            assert_eq!(ink, theme::BACKGROUND);
                            assert!(
                                frame.commands.iter().any(|command| matches!(command,
                                    DrawCommand::FillRect { rect: painted, color, .. }
                                        if *painted == rect && *color == theme::FOREGROUND
                                )),
                                "Conservar is the primary action"
                            );
                        } else {
                            assert_eq!(ink, theme::FOREGROUND);
                            assert!(frame.commands.iter().any(|command| matches!(command,
                                DrawCommand::StrokeRect { rect: painted, .. } if *painted == rect
                            )), "Deshacer is an outlined action");
                        }
                    }
                    action_rects.push(rect);
                    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                    tree.handle_event(UiEvent::PointerDown(point));
                    assert_eq!(
                        tree.handle_event(UiEvent::PointerUp(point)),
                        if enabled {
                            vec![UiAction::Clicked { id: id.clone() }]
                        } else {
                            vec![]
                        }
                    );
                    if !enabled {
                        assert!(
                            !tree.focus(&id),
                            "boundary navigation is not a keyboard action"
                        );
                    }
                }
                assert!(
                    resource.width >= 40.0,
                    "the filename retains space at {width}px: {resource:?}"
                );
                assert!(
                    action_rects.iter().all(|rect| {
                        (rect.y + rect.height / 2.0 - resource.y - resource.height / 2.0).abs()
                            < 0.01
                    }),
                    "the breadcrumb and every review control share one row at {width}px/{ide}"
                );
                assert_eq!(
                    editor.y,
                    if ide { 28.0 } else { HEADER_HEIGHT },
                    "review never creates a second header row"
                );
                assert!(frame.commands.iter().any(
                    |command| matches!(command, DrawCommand::Text { text, .. } if text == "# Antes")
                ));
                let after = controller.snapshot().unwrap();
                assert_eq!(
                    (after.revision(), after.selection()),
                    (before.revision(), before.selection()),
                    "review actions are requests; painting and navigation controls cannot edit the source"
                );
                assert_eq!(
                    after.text_range(0..after.char_count()).unwrap(),
                    before.text_range(0..before.char_count()).unwrap()
                );
                context.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    fn review_overflow_keeps_hidden_navigation_scoped_and_keyboard_reachable() {
        let key = |key| UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        };
        for width in [240.0, 360.0] {
            let mut review = crate::editor_review_ui::EditorReviewView {
                token: "owner-3/document-5/review-8".into(),
                hunk_count: 3,
                current_hunk: 1,
                file_count: 3,
                current_file: 1,
                can_accept: true,
                can_reject: true,
                ..Default::default()
            };
            let root = |review: &crate::editor_review_ui::EditorReviewView| {
                Element::column(vec![
                    review_row(review, width - BREADCRUMB_INSET * 2.0),
                    Element::spacer().flex(1.0),
                ])
                .size(Some(width), None)
            };
            let id = |operation| crate::editor_review_ui::action(&review.token, operation);
            let toggle = id("menu_toggle");
            let dismiss = id("menu_dismiss");
            let hidden = vec!["previous", "next", "previous_file", "next_file"];
            let (context, mut tree) = native_header_tree(root(&review));
            let viewport = Size::new(width, 360.0);
            let closed = tree.layout(viewport);
            for operation in &hidden {
                assert!(
                    !closed
                        .regions
                        .iter()
                        .any(|region| region.id == id(operation))
                );
            }
            let toggle_bounds = closed
                .regions
                .iter()
                .find(|region| region.id == toggle)
                .unwrap()
                .rect;
            for operation in ["reject_file", "accept_file"] {
                let decision = closed
                    .regions
                    .iter()
                    .find(|region| region.id == id(operation))
                    .unwrap()
                    .rect;
                assert!(decision.width >= 72.0 && decision.width <= 80.0);
                assert_eq!(decision.height, 24.0);
                assert_eq!(decision.y, toggle_bounds.y);
                assert!(decision.x >= toggle_bounds.x + toggle_bounds.width);
                assert!(decision.x + decision.width <= width);
            }
            assert!(tree.focus(&toggle));
            assert_eq!(
                tree.handle_event(key(Key::Enter)),
                vec![UiAction::Clicked { id: toggle.clone() }]
            );

            // The host owns open state; the same token must survive that rebuild.
            review.menu_open = true;
            tree.set_root(root(&review));
            let open = tree.layout(viewport);
            let surface_id = format!("popover_surface:{dismiss}");
            let surface = open
                .regions
                .iter()
                .find(|region| region.id == surface_id)
                .unwrap()
                .rect;
            assert!(surface.x >= 0.0 && surface.x + surface.width <= width);
            assert!(surface.y >= toggle_bounds.y + toggle_bounds.height);
            assert!(surface.y + surface.height <= viewport.height);
            for operation in &hidden {
                let action = id(operation);
                let regions: Vec<_> = open
                    .regions
                    .iter()
                    .filter(|region| region.id == action)
                    .collect();
                assert_eq!(
                    regions.len(),
                    1,
                    "hidden navigation has one unambiguous target: {operation}"
                );
                assert_eq!(regions[0].kind, HitKind::Button);
                let bounds = regions[0].rect;
                assert!(
                    bounds.x >= surface.x && bounds.x + bounds.width <= surface.x + surface.width
                );
                assert!(
                    bounds.y >= surface.y && bounds.y + bounds.height <= surface.y + surface.height
                );
                let point = Point::new(
                    bounds.x + bounds.width / 2.0,
                    bounds.y + bounds.height / 2.0,
                );
                assert_eq!(open.hit_test(point), Some(action.as_str()));
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id: action }]
                );
            }
            for text in ["Archivo anterior", "Archivo siguiente", "Archivo 2 de 3"] {
                assert!(
                    open.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text: painted, .. } if painted == text
                    )),
                    "menu must retain the complete {text:?} label at {width}px"
                );
            }
            assert!(tree.focus(&toggle));
            for operation in &hidden {
                assert!(tree.handle_event(key(Key::ArrowDown)).is_empty());
                let action = id(operation);
                assert_eq!(tree.focused_id(), Some(action.as_str()));
            }
            let last = id(hidden.last().unwrap());
            assert_eq!(
                tree.handle_event(key(Key::Enter)),
                vec![UiAction::Clicked { id: last.clone() }]
            );
            assert_eq!(
                tree.handle_event(key(Key::Space)),
                vec![UiAction::Clicked { id: last }]
            );
            assert_eq!(
                tree.handle_event(key(Key::Escape)),
                vec![UiAction::Clicked {
                    id: dismiss.clone()
                }]
            );
            let outside = Point::new(width - 2.0, viewport.height - 2.0);
            assert_eq!(open.hit_test(outside), Some(dismiss.as_str()));
            tree.handle_event(UiEvent::PointerDown(outside));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(outside)),
                vec![UiAction::Clicked { id: dismiss }]
            );
            review.menu_open = false;
            tree.set_root(root(&review));
            let dismissed = tree.layout(viewport);
            assert!(
                !dismissed
                    .regions
                    .iter()
                    .any(|region| region.id == surface_id)
            );
            for operation in &hidden {
                assert!(
                    !dismissed
                        .regions
                        .iter()
                        .any(|region| region.id == id(operation))
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn narrow_review_overflow_preserves_file_identity_preview_and_editor_actions() {
        for width in [240.0, 360.0, 460.0] {
            let view = header_fixture(false);
            let mut review = pending_review(&view);
            review.hunk_count = 3;
            review.current_hunk = 1;
            review.file_count = 3;
            review.current_file = 1;
            let mut workbench = crate::editor_workbench_ui::EditorWorkbenchView {
                review: Some(review.clone()),
                ..Default::default()
            };
            let root = |state: &crate::editor_workbench_ui::EditorWorkbenchView| {
                build_with_preview(
                    &view,
                    state,
                    width,
                    false,
                    vec![
                        Element::button("markdown_preview_open:91", "Vista previa")
                            .size(Some(24.0), Some(24.0)),
                    ],
                    KeymapProfile::default(),
                    false,
                )
            };
            let (context, mut tree) = native_header_tree(root(&workbench));
            let size = Size::new(width, 460.0);
            let closed = tree.layout(size);
            let editor = closed
                .regions
                .iter()
                .find(|region| region.id == view_id(view.tab_id))
                .unwrap()
                .rect;
            assert_eq!(editor.y, HEADER_HEIGHT);
            assert!(
                !closed
                    .regions
                    .iter()
                    .any(|region| region.id == "editor_menu")
            );
            assert!(
                !closed
                    .regions
                    .iter()
                    .any(|region| region.id == "editor_find")
            );
            assert!(
                !closed
                    .regions
                    .iter()
                    .any(|region| region.id == "markdown_preview_open:91")
            );
            assert!(tree.focus("editor_copy_relative"));
            let tooltip = tree.layout_at(size, Instant::now() + Duration::from_secs(1));
            let visible = tooltip
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                visible.contains("alerts.md") && visible.contains("docs/alerts.md"),
                "a narrow filename keeps its complete identity in the tooltip: {visible}"
            );
            review.menu_open = true;
            workbench.review = Some(review.clone());
            tree.set_root(root(&workbench));
            let open = tree.layout(size);
            let surface_id = format!(
                "popover_surface:{}",
                crate::editor_review_ui::action(&review.token, "menu_dismiss")
            );
            let surface = open
                .regions
                .iter()
                .find(|region| region.id == surface_id)
                .unwrap()
                .rect;
            assert!(surface.x >= 0.0 && surface.x + surface.width <= width + 0.01);
            assert!(surface.y >= HEADER_HEIGHT);
            if width == 240.0 {
                let save = open
                    .regions
                    .iter()
                    .find(|region| region.id == "code_editor_save:91")
                    .expect("the disabled save action retains its status tooltip");
                assert!(save.rect.height <= 26.01, "Guardar must remain one line");
                assert!(open.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == "Guardar"
                )));
                assert!(tree.focus("editor_replace"));
                let focused = tree.layout(size);
                let replace = focused
                    .regions
                    .iter()
                    .find(|region| region.id == "editor_replace" && region.kind == HitKind::Button)
                    .unwrap();
                assert!(
                    replace.rect.height <= 26.01,
                    "Reemplazar must remain one line"
                );
                assert!(focused.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == "Reemplazar"
                )));
                let tooltip = tree.layout_at(size, Instant::now() + Duration::from_secs(1));
                for key in [
                    if cfg!(target_os = "macos") {
                        "⌃"
                    } else {
                        "Ctrl"
                    },
                    "H",
                ] {
                    assert!(
                        tooltip.commands.iter().any(|command| matches!(command,
                            DrawCommand::Text { text, .. } if text == key
                        )),
                        "the complete shortcut remains available in the tooltip: {key}"
                    );
                }
            }
            for id in [
                "editor_find",
                "code_editor_reload:91",
                "markdown_preview_open:91",
            ] {
                assert!(tree.focus(id), "overflow retains the existing action {id}");
                let focused = tree.layout(size);
                let control = focused
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect;
                assert!(
                    control.y >= surface.y
                        && control.y + control.height <= surface.y + surface.height + 0.01
                );
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn review_overflow_skips_boundaries_and_disables_navigation_while_busy() {
        let key = |key| UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        };
        for (position, busy, can_accept) in [
            (0, false, true),
            (2, false, true),
            (1, true, true),
            (1, false, false),
        ] {
            let review = crate::editor_review_ui::EditorReviewView {
                token: "boundary-review".into(),
                hunk_count: 3,
                current_hunk: position,
                file_count: 3,
                current_file: position,
                menu_open: true,
                busy,
                can_accept,
                can_reject: can_accept,
                ..Default::default()
            };
            let (context, mut tree) = native_header_tree(Element::column(vec![
                review_row(&review, 228.0),
                Element::spacer().flex(1.0),
            ]));
            let frame = tree.layout(Size::new(240.0, 360.0));
            let mut enabled = Vec::new();
            for (operation, available) in [
                ("previous", position > 0),
                ("next", position < 2),
                ("previous_file", can_accept && position > 0),
                ("next_file", can_accept && position < 2),
            ] {
                let action = crate::editor_review_ui::action(&review.token, operation);
                let available = available && !busy;
                assert_eq!(
                    frame
                        .regions
                        .iter()
                        .any(|region| region.id == action && region.kind == HitKind::Button),
                    available
                );
                assert_eq!(
                    tree.focus(&action),
                    available,
                    "keyboard availability matches menu availability: {operation}"
                );
                if available {
                    enabled.push(action);
                }
            }
            let toggle = crate::editor_review_ui::action(&review.token, "menu_toggle");
            assert!(tree.focus(&toggle));
            for action in &enabled {
                assert!(tree.handle_event(key(Key::ArrowDown)).is_empty());
                assert_eq!(
                    tree.focused_id(),
                    Some(action.as_str()),
                    "keyboard navigation skips disabled boundaries and headings"
                );
            }
            if let Some(last) = enabled.last() {
                assert_eq!(
                    tree.handle_event(key(Key::Enter)),
                    vec![UiAction::Clicked { id: last.clone() }]
                );
            }
            assert_eq!(
                tree.handle_event(key(Key::Escape)),
                vec![UiAction::Clicked {
                    id: crate::editor_review_ui::action(&review.token, "menu_dismiss"),
                }]
            );
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn review_navigation_disables_only_the_unavailable_boundary() {
        for current in 0..3 {
            let review = crate::editor_review_ui::EditorReviewView {
                token: "navigation-boundaries".into(),
                hunk_count: 3,
                current_hunk: current,
                file_count: 3,
                current_file: current,
                can_accept: true,
                can_reject: true,
                ..Default::default()
            };
            let mut tree = UiTree::new(Element::column(vec![
                crate::editor_review_ui::controls(&review, 900.0),
                Element::spacer().flex(1.0),
            ]));
            let frame = tree.layout(Size::new(900.0, 100.0));
            for (operation, enabled) in [
                ("previous", current > 0),
                ("next", current < 2),
                ("previous_file", current > 0),
                ("next_file", current < 2),
            ] {
                let id = crate::editor_review_ui::action(&review.token, operation);
                let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert_eq!(
                    region.kind,
                    if enabled {
                        HitKind::Button
                    } else {
                        HitKind::Hover
                    }
                );
                assert_eq!((region.rect.width, region.rect.height), (24.0, 24.0));
                let point = Point::new(region.rect.x + 12.0, region.rect.y + 12.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    if enabled {
                        vec![UiAction::Clicked { id: id.clone() }]
                    } else {
                        vec![]
                    },
                    "navigation does not wrap at its boundary: {current}/{operation}"
                );
                assert_eq!(tree.focus(&id), enabled);
            }
        }
    }

    #[test]
    fn active_review_block_uses_scoped_text_buttons_without_mutating_the_controller() {
        let view = header_fixture(false);
        let before = view.controller.as_ref().unwrap().snapshot().unwrap();
        let review = pending_review(&view);
        let actions = review.actions[0].clone();
        let workbench = crate::editor_workbench_ui::EditorWorkbenchView {
            review: Some(review),
            ..Default::default()
        };
        let (context, mut tree) = native_header_tree(build_ide(
            &view,
            &workbench,
            360.0,
            KeymapProfile::default(),
            false,
        ));
        let frame = tree.layout(Size::new(360.0, 440.0));
        let editor = frame
            .regions
            .iter()
            .find(|region| region.id == view_id(view.tab_id))
            .unwrap()
            .rect;
        for (id, label) in [
            (&actions.accept_id, "Conservar"),
            (&actions.reject_id, "Deshacer"),
        ] {
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == *id && region.kind == HitKind::Button)
                .unwrap_or_else(|| panic!("missing active block action {id}"))
                .rect;
            assert_eq!(rect.width, 80.0, "inline decisions use equal widths");
            assert_eq!(rect.height, 24.0);
            assert!(
                rect.x >= editor.x
                    && rect.x + rect.width <= editor.x + editor.width
                    && rect.y >= editor.y
                    && rect.y + rect.height <= editor.y + editor.height,
                "block actions must remain inside the code viewport"
            );
            let (origin, font_size) = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text {
                        text,
                        position,
                        font_size,
                        ..
                    } if text == label
                        && position.x >= rect.x
                        && position.x < rect.x + rect.width
                        && position.y >= rect.y
                        && position.y < rect.y + rect.height =>
                    {
                        Some((*position, *font_size))
                    }
                    _ => None,
                })
                .expect("the active block action must retain its complete label");
            let text_width = context.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        label.into(),
                        eframe::egui::FontId::proportional(font_size),
                        eframe::egui::Color32::WHITE,
                    )
                    .size()
                    .x
            });
            assert!(
                origin.x + text_width <= rect.x + rect.width + 0.01,
                "the complete inline label fits the equal-width button"
            );
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked { id: id.clone() }]
            );
            let after = view.controller.as_ref().unwrap().snapshot().unwrap();
            assert_eq!(
                (after.revision(), after.selection()),
                (before.revision(), before.selection())
            );
        }
        context.end_pass().textures_delta.clear();
    }

    #[test]
    fn review_acceptance_is_unavailable_while_busy_and_stale_snapshots_have_no_review_actions() {
        let view = header_fixture(false);
        let mut workbench = crate::editor_workbench_ui::EditorWorkbenchView {
            review: Some(pending_review(&view)),
            ..Default::default()
        };
        let review = workbench.review.as_mut().unwrap();
        review.busy = true;
        review.file_count = 3;
        review.current_file = 1;
        review.reason = Some("Esperando a que termine el cambio del archivo".into());
        let id = crate::editor_review_ui::action(&review.token, "accept_file");
        let (context, mut tree) = native_header_tree(build_ide(
            &view,
            &workbench,
            900.0,
            KeymapProfile::default(),
            false,
        ));
        let frame = tree.layout(Size::new(900.0, 440.0));
        assert!(
            !frame
                .regions
                .iter()
                .any(|region| region.id == id && region.kind == HitKind::Button)
        );
        assert!(!tree.focus(&id));
        for operation in [
            "previous",
            "next",
            "previous_file",
            "next_file",
            "reject_file",
        ] {
            let id = crate::editor_review_ui::action(
                &workbench.review.as_ref().unwrap().token,
                operation,
            );
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|region| region.id == id && region.kind == HitKind::Button)
            );
            assert!(
                !tree.focus(&id),
                "busy review controls do not enter keyboard focus: {operation}"
            );
        }
        workbench.review.as_mut().unwrap().busy = false;
        view.controller
            .as_ref()
            .unwrap()
            .shared_buffer()
            .lock()
            .unwrap()
            .insert("Nueva edición", openide_editor::EditKind::Paste, 1)
            .unwrap();
        tree.set_root(build_ide(
            &view,
            &workbench,
            900.0,
            KeymapProfile::default(),
            false,
        ));
        let stale = tree.layout(Size::new(900.0, 440.0));
        assert!(
            !stale
                .regions
                .iter()
                .any(|region| region.id.starts_with("editor_review:"))
        );
        assert!(
            !stale.commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == "# Antes")
            )
        );
        let editor = stale
            .regions
            .iter()
            .find(|region| region.id == view_id(view.tab_id))
            .unwrap()
            .rect;
        assert_eq!(editor.y, BREADCRUMB_HEIGHT);
        context.end_pass().textures_delta.clear();
    }

    #[test]
    #[ignore = "native review breadcrumb and inline decorations visual artifacts"]
    fn export_editor_review_native_visual_review() {
        let directory = std::path::Path::new("/tmp/openade-editor-review");
        std::fs::create_dir_all(directory).unwrap();
        for width in [240.0, 360.0, 460.0, 600.0, 900.0, 1200.0] {
            for ide in [false, true] {
                let scenarios = if width <= 360.0 {
                    vec![
                        (3, false, ""),
                        (1, false, "-single"),
                        (3, true, "-menu"),
                        (1, true, "-single-menu"),
                    ]
                } else {
                    vec![(3, false, "")]
                };
                for (file_count, menu_open, suffix) in scenarios {
                    let view = header_fixture(false);
                    let mut review = pending_review(&view);
                    review.file_count = file_count;
                    review.current_file = usize::from(file_count > 1);
                    review.menu_open = menu_open;
                    let workbench = crate::editor_workbench_ui::EditorWorkbenchView {
                        review: Some(review),
                        ..Default::default()
                    };
                    let build = if ide { build_ide } else { build };
                    let (context, mut tree) = native_header_tree(build(
                        &view,
                        &workbench,
                        width,
                        KeymapProfile::default(),
                        false,
                    ));
                    let frame = tree.layout(Size::new(width, 400.0));
                    if menu_open
                        && !frame.regions.iter().any(|region| {
                            region
                                .id
                                .starts_with("popover_surface:editor_review:menu_dismiss:")
                        })
                    {
                        // A single-file 360px pane fits its block navigation inline.
                        context.end_pass().textures_delta.clear();
                        continue;
                    }
                    let path = directory.join(format!(
                        "review-{}-{width}{suffix}.svg",
                        if ide { "ide" } else { "agents" }
                    ));
                    crate::settings_ui::visual_qa::write_native_frame_svg(
                        &frame, &path, width, 400.0,
                    )
                    .unwrap();
                    let svg = std::fs::read(&path).unwrap();
                    let svg_tree = resvg::usvg::Tree::from_data(
                        &svg,
                        &crate::settings_ui::visual_qa::native_svg_options(),
                    )
                    .unwrap();
                    let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 400).unwrap();
                    resvg::render(
                        &svg_tree,
                        resvg::tiny_skia::Transform::identity(),
                        &mut pixmap.as_mut(),
                    );
                    pixmap.save_png(path.with_extension("png")).unwrap();
                    context.end_pass().textures_delta.clear();
                }
            }
        }
    }
}
