//! Controlled editor tools. Rendering never searches, reads files or starts a server.
use crate::{code_editor_ui::CodeEditorView, keymap::{KeymapProfile, ShortcutAction}, theme};
use openide_editor::{IndentStyle, SearchOptions, SearchResults};
use openide_language::{lsp::Diagnostic, AnalysisSnapshot};
use openide_ui::{
    ActiveHover, Button, ButtonSize, ButtonVariant, CodeLanguage, Component, ContextMenuEntry,
    ContextMenuItem, ControlSize, DropdownMenu, DropdownMenuEntry, DropdownMenuItem, Element,
    FontFamily, Icon, IconButton, PopoverPlacement, TextField, TooltipPlacement, TooltipSpec,
};
use std::{ops::Range, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorForm {
    NewFile,
    SaveAs,
    Rename,
    Goto,
    Snippet,
}
#[derive(Clone, Debug)]
pub(crate) struct LanguageResult {
    pub label: String,
    pub detail: String,
    pub value: serde_json::Value,
}
#[derive(Clone, Debug)]
pub(crate) struct EditorWorkbenchView {
    pub document_id: Option<u64>,
    pub review: Option<crate::editor_review_ui::EditorReviewView>,
    pub search_open: bool,
    pub replace_open: bool,
    pub query: String,
    pub replacement: String,
    pub search_options: SearchOptions,
    pub search: Option<SearchResults>,
    pub search_pending: bool,
    pub search_error: Option<String>,
    pub menu_open: bool,
    pub breadcrumb_open: bool,
    pub settings_open: bool,
    pub outline_open: bool,
    pub form: Option<EditorForm>,
    pub form_value: String,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub soft_wrap: bool,
    pub minimap: bool,
    pub indent: IndentStyle,
    pub auto_pairs: bool,
    pub language_override: Option<CodeLanguage>,
    pub language_menu_open: bool,
    pub server_path: String,
    pub analysis: Option<Arc<AnalysisSnapshot>>,
    pub highlight_spans: Arc<[openide_ui::CodeHighlight]>,
    pub folds: Arc<[Range<usize>]>,
    pub structure_revision: Option<(u64, u64)>,
    pub fold_candidates: Arc<[openide_ui::CodeFoldCandidate]>,
    pub sticky_scopes: Arc<[openide_ui::CodeStickyScope]>,
    pub breakpoint_revision: Option<(u64, u64)>,
    pub breakpoint_rows: Arc<[usize]>,
    pub debug_breakpoints: Arc<[openide_ui::CodeBreakpoint]>,
    pub debug_execution: Option<openide_ui::CodeExecutionLine>,
    pub conflict_disk: Option<openide_ui::EditorController>,
    pub conflict_open: bool,
    pub language_status: String,
    pub language_ready: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub diagnostic_spans: Arc<[openide_ui::CodeDiagnostic]>,
    pub diagnostic_revision: Option<(u64, u64)>,
    pub diagnostics_current: bool,
    pub results_title: String,
    pub results: Vec<LanguageResult>,
    pub result_document: Option<(u64, u64)>,
    pub hover: Option<String>,
}
impl Default for EditorWorkbenchView {
    fn default() -> Self {
        Self {
            document_id: None,
            review: None,
            search_open: false,
            replace_open: false,
            query: String::new(),
            replacement: String::new(),
            search_options: SearchOptions::default(),
            search: None,
            search_pending: false,
            search_error: None,
            menu_open: false,
            breadcrumb_open: false,
            settings_open: false,
            outline_open: false,
            form: None,
            form_value: String::new(),
            error: None,
            notice: None,
            soft_wrap: false,
            minimap: true,
            indent: IndentStyle::default(),
            auto_pairs: true,
            language_override: None,
            language_menu_open: false,
            server_path: String::new(),
            analysis: None,
            highlight_spans: Arc::from([]),
            folds: Arc::from([]),
            structure_revision: None,
            fold_candidates: Arc::from([]),
            sticky_scopes: Arc::from([]),
            breakpoint_revision: None,
            breakpoint_rows: Arc::from([]),
            debug_breakpoints: Arc::from([]),
            debug_execution: None,
            conflict_disk: None,
            conflict_open: false,
            language_status: "Lenguaje sin conectar".into(),
            language_ready: false,
            diagnostics: Vec::new(),
            diagnostic_spans: Arc::from([]),
            diagnostic_revision: None,
            diagnostics_current: false,
            results_title: String::new(),
            results: Vec::new(),
            result_document: None,
            hover: None,
        }
    }
}
pub(crate) fn text(value: impl Into<String>, muted: bool) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(if muted {
            theme::FOREGROUND_SUBTLE
        } else {
            theme::FOREGROUND
        })
}
fn action(id: &str, title: &str, tip: &str, active: bool) -> Element {
    action_with_shortcut(id, title, tip, None, active)
}
fn action_with_shortcut(
    id: &str,
    title: &str,
    tip: &str,
    shortcut: Option<&str>,
    active: bool,
) -> Element {
    let mut button = Button::new(id, title, theme::component_palette())
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .active(active)
        .tooltip(tip);
    if let Some(shortcut) = shortcut {
        button = button.tooltip_shortcut(shortcut);
    }
    theme::interactive_surface(button.render())
}
fn result_row(id: &str, title: &str, tip: &str, indent: usize) -> Element {
    let row = theme::interactive_surface(
        Element::icon_button(id, openide_ui::Icon::Code, title)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .foreground(theme::FOREGROUND)
            .padding(8.0)
            .size(None, Some(30.0)),
    );
    let row = openide_ui::Tooltip::new(row, id, tip, theme::component_palette())
        .render()
        .flex(1.0);
    Element::row(vec![
        Element::spacer().size(Some((indent.min(5) * 12) as f32), None),
        row,
    ])
}
fn field(id: &str, value: &str, placeholder: &str) -> Element {
    TextField::new(id, value, placeholder, theme::component_palette())
        .size(ControlSize::Compact)
        .render()
        .flex(1.0)
}
pub(crate) fn context_actions(ready: bool) -> Vec<ContextMenuEntry> {
    let item = |id, title, shortcut, enabled| {
        ContextMenuEntry::Item(
            ContextMenuItem::new(format!("editor_{id}"), title)
                .shortcut(shortcut)
                .enabled(enabled),
        )
    };
    // Clipboard/select-all entries come from CodeEditor. Keep this local list
    // focused on common editing and symbol navigation; the full grouped menu
    // remains one explicit click away and preserves every action ID.
    vec![
        item("undo", "Deshacer", "Ctrl+Z", true),
        item("redo", "Rehacer", "Ctrl+Shift+Z", true),
        ContextMenuEntry::Separator,
        item("comment", "Comentar / descomentar", "", true),
        ContextMenuEntry::Separator,
        item("definition", "Ir a definición", "F12", ready),
        item("rename", "Renombrar símbolo…", "F2", ready),
        ContextMenuEntry::Separator,
        item("menu", "Más acciones…", "", true),
    ]
}

fn more_actions(ready: bool) -> Vec<DropdownMenuEntry> {
    let mut entries = Vec::new();
    for (heading, actions) in [
        (
            "Navegar",
            &[
                ("find", "Buscar", false),
                ("replace", "Reemplazar", false),
                ("outline", "Símbolos del archivo", false),
                ("goto", "Ir a línea…", false),
                ("fold", "Plegar bloque actual", false),
                ("unfold", "Desplegar todo", false),
                ("breakpoint", "Alternar breakpoint", false),
            ][..],
        ),
        (
            "Archivo",
            &[
                ("new", "Nuevo archivo…", false),
                ("save_as", "Guardar como…", false),
                ("copy_path", "Copiar ruta completa", false),
                ("copy_relative", "Copiar ruta relativa", false),
            ][..],
        ),
        (
            "Editar",
            &[
                ("undo", "Deshacer", false),
                ("redo", "Rehacer", false),
                ("duplicate", "Duplicar líneas", false),
                ("move_up", "Mover líneas arriba", false),
                ("move_down", "Mover líneas abajo", false),
                ("delete_lines", "Eliminar líneas", false),
                ("comment", "Comentar / descomentar", false),
                ("snippet", "Insertar plantilla…", false),
            ][..],
        ),
        (
            "Selección",
            &[
                ("select_next", "Siguiente coincidencia", false),
                ("select_all_matches", "Todas las coincidencias", false),
                ("cursor_above", "Añadir cursor arriba", false),
                ("cursor_below", "Añadir cursor abajo", false),
            ][..],
        ),
        (
            "Lenguaje",
            &[
                ("completion", "Completar código", true),
                ("hover", "Documentación del símbolo", true),
                ("definition", "Ir a definición", true),
                ("references", "Buscar referencias", true),
                ("rename", "Renombrar símbolo…", true),
                ("format", "Formatear documento", true),
                ("code_actions", "Acciones de código", true),
            ][..],
        ),
        (
            "Editor",
            &[
                ("wrap", "Ajustar líneas al ancho", false),
                ("minimap", "Mostrar minimapa", false),
                ("settings", "Opciones del editor", false),
            ][..],
        ),
    ] {
        entries.push(DropdownMenuEntry::Heading(heading.into()));
        for (id, label, language) in actions {
            let item = DropdownMenuItem::new(format!("editor_{id}"), *label);
            let item = if *id == "breakpoint" {
                item.shortcut("F9")
            } else {
                item
            };
            entries.push(DropdownMenuEntry::Item(if *language && !ready {
                item.disabled()
            } else {
                item
            }));
        }
    }
    entries
}
fn file_actions(view: &CodeEditorView, state: &EditorWorkbenchView, profile: KeymapProfile) -> Vec<DropdownMenuEntry> {
    let snapshot = view
        .controller
        .as_ref()
        .and_then(|controller| controller.snapshot().ok());
    let dirty = snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.is_dirty());
    let mut save = DropdownMenuItem::new(format!("code_editor_save:{}", view.tab_id), "Guardar")
        .shortcut("Ctrl+S");
    if !dirty || view.loading || view.saving {
        save = save.disabled().annotation(if view.saving {
            "Guardando…"
        } else if view.loading {
            "Abriendo…"
        } else if snapshot.is_some() {
            "Sin cambios"
        } else {
            "No disponible"
        });
    }
    let mut reload = DropdownMenuItem::new(
        format!("code_editor_reload:{}", view.tab_id),
        "Recargar desde disco",
    )
    .icon(Icon::Refresh);
    if view.loading || view.saving {
        reload = reload.disabled();
    }
    let mut entries = vec![save.into(), reload.into(), DropdownMenuEntry::Separator];
    entries.extend(more_actions(state.language_ready));
    for entry in &mut entries {
        if let DropdownMenuEntry::Item(item) = entry {
            match item.action_id.as_str() {
                "editor_outline" => item.selected = state.outline_open,
                "editor_wrap" => item.selected = state.soft_wrap,
                "editor_minimap" => item.selected = state.minimap,
                "editor_find" => item.shortcut = Some("Ctrl+F".into()),
                "editor_replace" => item.shortcut = Some("Ctrl+H".into()),
                "editor_goto" => item.shortcut = Some("Ctrl+G".into()),
                "editor_undo" => item.shortcut = Some("Ctrl+Z".into()),
                "editor_redo" => item.shortcut = Some("Ctrl+Shift+Z".into()),
                "editor_duplicate" => item.shortcut = Some(profile.shortcut(ShortcutAction::DuplicateLines).into()),
                "editor_delete_lines" => item.shortcut = Some("Ctrl+Shift+K".into()),
                "editor_select_next" => item.shortcut = Some("Ctrl+D".into()),
                "editor_select_all_matches" => item.shortcut = Some("Ctrl+Shift+L".into()),
                "editor_definition" => item.shortcut = Some("F12".into()),
                "editor_references" => item.shortcut = Some("Shift+F12".into()),
                "editor_rename" => item.shortcut = Some("F2".into()),
                "editor_completion" => item.shortcut = Some("Ctrl+Space".into()),
                _ => {}
            }
        }
    }
    entries
}

pub(crate) fn header_actions_with_debug(
    view: &CodeEditorView,
    state: &EditorWorkbenchView,
    debugging: bool,
    profile: KeymapProfile,
) -> Vec<Element> {
    let actions = menu_entries(view, state, debugging, profile);
    vec![
        IconButton::new("editor_find", Icon::Search, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .active(state.search_open)
            .active_hover(ActiveHover::Palette)
            .tooltip_with(
                TooltipSpec::new("Buscar en el documento")
                    .shortcut("Ctrl+F")
                    .placement(TooltipPlacement::Below),
            )
            .render(),
        DropdownMenu::new(
            IconButton::new("editor_menu", Icon::More, theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .active(state.menu_open)
                .active_hover(ActiveHover::Palette)
                .tooltip_with(
                    TooltipSpec::new("Acciones del archivo y del editor")
                        .placement(TooltipPlacement::Below),
                )
                .render(),
            actions,
            state.menu_open,
            "editor_menu_close",
            theme::menu_palette(),
        )
        .context_menu_style()
        .placement(PopoverPlacement::BottomEnd)
        .render(),
    ]
}

pub(crate) fn menu_entries(
    view: &CodeEditorView,
    state: &EditorWorkbenchView,
    debugging: bool,
    profile: KeymapProfile,
) -> Vec<DropdownMenuEntry> {
    let mut actions = file_actions(view, state, profile);
    if !debugging {
        actions.retain(|entry| {
            !matches!(entry,
                DropdownMenuEntry::Item(item) if item.action_id == "editor_breakpoint"
            )
        });
    }
    actions
}

fn search_visible(state: &EditorWorkbenchView) -> bool {
    state.search_open
        && !state.menu_open
        && !state.breadcrumb_open
        && !state.review.as_ref().is_some_and(|review| review.menu_open)
        && state.form.is_none()
        && !state.settings_open
}

// Match the floating rows plus a little breathing room. This is used only
// when revealing a caret; opening search does not reflow or scroll the code.
pub(crate) fn search_reveal_inset(state: &EditorWorkbenchView, width: f32) -> f32 {
    // Keep the scroll range stable while another transient tool covers search.
    if !state.search_open {
        return 0.0;
    }
    48.0 + if width < 480.0 || state.replace_open {
        32.0
    } else {
        0.0
    } + if state.replace_open { 32.0 } else { 0.0 }
        + if state.search_error.is_some() {
            24.0
        } else {
            0.0
        }
}

// Like VS Code's FindWidget, this surface floats over the document. Keeping
// it non-modal lets the user edit/select code while the search stays open.
pub(crate) fn search_popover(
    anchor: Element,
    view: &CodeEditorView,
    state: &EditorWorkbenchView,
    width: f32,
) -> Element {
    // Other transient tools take priority; retain the query and highlights so
    // dismissing them restores search without disturbing the document.
    if !search_visible(state) {
        return anchor;
    }
    let width = width.clamp(0.0, 520.0);
    let options_below = width < 480.0 || state.replace_open;
    let snapshot = view.controller.as_ref().and_then(|c| c.snapshot().ok());
    let results = state.search.as_ref().filter(|results| {
        !state.search_pending
            && snapshot.as_ref().is_some_and(|snapshot| {
                results.document_id == snapshot.document_id()
                    && results.revision == snapshot.revision()
            })
    });
    let has_matches = results.is_some_and(|results| !results.matches.is_empty());
    let selected = snapshot.as_ref().map(|s| s.selection().range());
    let current = results.and_then(|r| {
        r.matches
            .iter()
            .position(|range| Some(range) == selected.as_ref())
    });
    let count = if state.query.is_empty() {
        String::new()
    } else if state.search_pending {
        "…".into()
    } else {
        results.map_or("—".into(), |r| {
            format!(
                "{} / {}{}",
                current.map_or(0, |n| n + 1),
                r.matches.len(),
                if r.truncated { "+" } else { "" }
            )
        })
    };
    let count = openide_ui::Tooltip::new(
        Element::truncated_text(count)
            .font_family(FontFamily::Sans)
            .font_size(11.0)
            .foreground(
                if !state.query.is_empty() && results.is_some() && !has_matches {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND_SUBTLE
                },
            )
            .size(Some(72.0), Some(28.0)),
        "editor_search_count",
        if state.search_pending {
            "Buscando…"
        } else {
            "Coincidencia actual / total"
        },
        theme::component_palette(),
    )
    .render();
    let search_field = |id, value: &str, placeholder| {
        TextField::new(id, value, placeholder, theme::component_palette())
            .size(ControlSize::Compact)
            .persistent_placeholder(true)
            .render()
            .flex(1.0)
    };
    let option = |id, title, tip, active| {
        theme::interactive_surface(
            Button::new(id, title, theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::IconExtraSmall)
                .width(28.0)
                .active(active)
                .tooltip(tip)
                .render(),
        )
    };
    let options = Element::row(vec![
        option(
            "editor_case",
            "Aa",
            "Distinguir mayúsculas y minúsculas",
            state.search_options.case_sensitive,
        ),
        option(
            "editor_word",
            "Ab",
            "Sólo palabras completas",
            state.search_options.whole_word,
        ),
        option(
            "editor_regex",
            ".*",
            "Expresión regular",
            state.search_options.regex,
        ),
        IconButton::new("editor_scope", Icon::Filter, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .active(state.search_options.scope.is_some())
            .active_hover(ActiveHover::Palette)
            .tooltip_with(
                TooltipSpec::new("Buscar dentro de la selección")
                    .placement(TooltipPlacement::Above),
            )
            .render(),
    ])
    .gap(2.0)
    .size(None, Some(28.0));
    let mut query = vec![
        IconButton::new(
            "editor_replace_toggle",
            if state.replace_open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            },
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .active(state.replace_open)
        .active_hover(ActiveHover::Palette)
        .tooltip_with(
            TooltipSpec::new("Mostrar u ocultar reemplazo")
                .shortcut("Ctrl+H")
                .placement(TooltipPlacement::Above),
        )
        .render(),
        search_field("editor_query", &state.query, "Buscar en el archivo"),
    ];
    if !options_below {
        query.push(options.clone());
        query.push(count.clone());
    }
    query.extend([
        IconButton::new(
            "editor_search_prev",
            Icon::ArrowUp,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .enabled(has_matches)
        .tooltip_with(
            TooltipSpec::new("Coincidencia anterior")
                .shortcut("Shift+Enter")
                .placement(TooltipPlacement::Above),
        )
        .render(),
        IconButton::new(
            "editor_search_next",
            Icon::ArrowDown,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .enabled(has_matches)
        .tooltip_with(
            TooltipSpec::new("Coincidencia siguiente")
                .shortcut("Enter")
                .placement(TooltipPlacement::Above),
        )
        .render(),
        IconButton::new(
            "editor_search_close",
            Icon::Close,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Cerrar búsqueda")
                .shortcut("Esc")
                .placement(TooltipPlacement::Above),
        )
        .render(),
    ]);
    let mut rows = vec![Element::row(query).gap(3.0).size(None, Some(28.0))];
    if state.replace_open {
        // Both fields share the same leading slot and 3-button trailing rail.
        // Search options belong after the pair, so they never split the flow.
        rows.push(
            Element::row(vec![
                Element::spacer().size(Some(24.0), None),
                search_field("editor_replacement", &state.replacement, "Reemplazar por"),
                IconButton::new(
                    "editor_replace_one",
                    crate::provider_icons::REPLACE,
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .enabled(current.is_some())
                .tooltip_with(
                    TooltipSpec::new("Reemplazar coincidencia")
                        .shortcut("Enter")
                        .placement(TooltipPlacement::Above),
                )
                .render(),
                IconButton::new(
                    "editor_replace_all",
                    crate::provider_icons::REPLACE_ALL,
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .enabled(has_matches)
                .tooltip_with(
                    TooltipSpec::new("Reemplazar todas las coincidencias")
                        .placement(TooltipPlacement::Above),
                )
                .render(),
                Element::spacer().size(Some(24.0), None),
            ])
            .gap(3.0)
            .size(None, Some(28.0)),
        );
    }
    if options_below {
        rows.push(
            Element::row(vec![
                Element::spacer().size(Some(if width >= 280.0 { 24.0 } else { 0.0 }), None),
                options,
                Element::spacer().flex(1.0),
                count,
            ])
            .gap(3.0)
            .size(None, Some(28.0)),
        );
    }
    if let Some(error) = &state.search_error {
        rows.push(
            openide_ui::Tooltip::new(
                Element::truncated_text(error.split_whitespace().collect::<Vec<_>>().join(" "))
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .foreground(theme::DESTRUCTIVE)
                    .size(None, Some(20.0)),
                "editor_search_error",
                error,
                theme::component_palette(),
            )
            .render(),
        );
    }
    let surface = Element::column(rows)
        .gap(4.0)
        .padding(8.0)
        .size(Some(width), None)
        .background(theme::MENU_SURFACE)
        .border(theme::BORDER, 1.0)
        .radius(8.0)
        .shadow(
            openide_ui::Color::rgba(0, 0, 0, 100),
            openide_ui::Point::new(0.0, 5.0),
            14.0,
            0.0,
        );
    Element::popover(
        anchor,
        surface,
        "editor_search_close",
        PopoverPlacement::BottomEnd,
    )
    .popover_non_modal()
    .popover_animation_ms(0)
}

pub(crate) fn tools(
    view: &CodeEditorView,
    state: &EditorWorkbenchView,
    width: f32,
) -> Vec<Element> {
    let mut rows = Vec::new();
    if view.conflict || view.missing || view.recovered {
        rows.push(
            Element::row(vec![
                text(
                    if view.missing {
                        "El archivo ya no está en disco"
                    } else if view.conflict {
                        "El archivo cambió en disco"
                    } else {
                        "Borrador recuperado"
                    },
                    true,
                )
                .flex(1.0),
                action(
                    if view.conflict && !view.missing {
                        "editor_conflict"
                    } else {
                        "editor_save_as"
                    },
                    if view.conflict && !view.missing {
                        "Comparar"
                    } else {
                        "Guardar como…"
                    },
                    "Conservar y revisar tus cambios",
                    false,
                ),
            ])
            .gap(6.0)
            .padding(8.0),
        );
    }
    if state.settings_open {
        rows.push(Element::horizontal_scroll(
            "editor_settings_scroll",
            Element::row(vec![
                action(
                    "editor_indent_kind",
                    if matches!(state.indent, IndentStyle::Tabs(_)) {
                        "Tabs"
                    } else {
                        "Espacios"
                    },
                    "Cambiar indentación",
                    false,
                ),
                action(
                    "editor_indent_width",
                    &format!("{}", state.indent.width()),
                    "Ancho de indentación: 2, 4 u 8",
                    false,
                ),
                action(
                    "editor_pairs",
                    "Pares",
                    "Insertar y saltar pares de delimitadores",
                    state.auto_pairs,
                ),
                action(
                    "editor_eol",
                    "LF / CRLF",
                    "Convertir finales de línea; se puede deshacer",
                    false,
                ),
                DropdownMenu::new(
                    action(
                        "editor_language",
                        &format!(
                            "{:?}",
                            state
                                .language_override
                                .unwrap_or_else(|| language_for_path(&view.path))
                        ),
                        "Cambiar lenguaje del documento",
                        state.language_menu_open,
                    ),
                    [
                        "auto",
                        "plain",
                        "rust",
                        "javascript",
                        "typescript",
                        "tsx",
                        "python",
                        "json",
                        "toml",
                        "css",
                        "html",
                        "markdown",
                    ]
                    .into_iter()
                    .map(|name| {
                        DropdownMenuEntry::Item(DropdownMenuItem::new(
                            format!("editor_language:{name}"),
                            name,
                        ))
                    })
                    .collect::<Vec<_>>(),
                    state.language_menu_open,
                    "editor_language_close",
                    theme::menu_palette(),
                )
                .context_menu_style()
                .width(200.0)
                .render(),
            ])
            .gap(2.0)
            .padding(4.0),
        ));
        rows.push(
            Element::row(vec![
                field(
                    "editor_server_path",
                    &state.server_path,
                    "Servidor de lenguaje (opcional)",
                ),
                action(
                    "editor_server_connect",
                    "Conectar",
                    "Conectar o reiniciar el servidor de lenguaje",
                    false,
                ),
            ])
            .gap(4.0)
            .padding(6.0),
        );
    }
    if let Some(form) = state.form {
        let (title, placeholder) = match form {
            EditorForm::NewFile => ("Nuevo archivo", "Ruta relativa al proyecto"),
            EditorForm::SaveAs => ("Guardar como", "Nueva ruta relativa al proyecto"),
            EditorForm::Rename => ("Renombrar símbolo", "Nombre nuevo"),
            EditorForm::Goto => ("Ir a línea", "Línea:columna"),
            EditorForm::Snippet => ("Insertar plantilla", "Texto con ${1:nombre} y $0"),
        };
        rows.push(text(title, false).padding(6.0));
        rows.push(
            Element::row(vec![
                field("editor_form_value", &state.form_value, placeholder),
                action_with_shortcut("editor_form_apply", "Aplicar", "Confirmar", Some("Enter"), false),
                openide_ui::DismissButton::new("editor_form_cancel", theme::component_palette()).render(),
            ])
            .gap(4.0)
            .padding(6.0),
        );
    }
    if let Some(error) = &state.error {
        rows.push(
            text(error, false)
                .foreground(theme::DESTRUCTIVE)
                .padding(6.0),
        );
    }
    if let Some(notice) = &state.notice {
        rows.push(text(notice, true).padding(6.0));
    }
    if state.outline_open {
        let mut symbols = Vec::new();
        if let Some(analysis) = &state.analysis {
            for (i, symbol) in analysis.symbols.iter().take(256).enumerate() {
                let depth = analysis.symbols[..i]
                    .iter()
                    .filter(|parent| {
                        parent.range.start <= symbol.range.start
                            && parent.range.end > symbol.range.end
                    })
                    .count();
                symbols.push(result_row(
                    &format!("editor_symbol:{i}"),
                    &symbol.name,
                    &format!("Ir a {}", symbol.name),
                    depth,
                ));
            }
            if analysis.truncated {
                symbols.push(text(
                    "Análisis parcial: se alcanzó el límite del documento",
                    true,
                ));
            }
        }
        if symbols.is_empty() {
            symbols.push(text("Este archivo no tiene símbolos disponibles", true).padding(8.0));
        }
        rows.push(
            Element::scroll("editor_outline_scroll", Element::column(symbols))
                .size(None, Some(160.0)),
        );
    }
    if state.conflict_open {
        rows.push(
            Element::row(vec![
                text("Versión del disco · sólo lectura", true).flex(1.0),
                action(
                    "editor_conflict_close",
                    "Cerrar",
                    "Cerrar comparación",
                    false,
                ),
            ])
            .padding(6.0),
        );
        if let Some(disk) = &state.conflict_disk {
            rows.push(
                openide_ui::CodeEditor::new("editor_conflict_disk", disk.clone())
                    .read_only(true)
                    .language(language_for_path(&view.path))
                    .palette(theme::component_palette())
                    .height(180.0)
                    .render()
                    .size(None, Some(180.0)),
            );
        }
    }
    if state.hover.is_some() || !state.results.is_empty() {
        rows.push(
            Element::row(vec![
                text(&state.results_title, false).flex(1.0),
                openide_ui::DismissButton::new("editor_results_close", theme::component_palette()).render(),
            ])
            .padding(6.0),
        );
        let mut items = Vec::new();
        if let Some(hover) = &state.hover {
            items.push(text(hover, false).padding(8.0));
        }
        for (i, result) in state.results.iter().take(80).enumerate() {
            items.push(result_row(
                &format!("editor_result:{i}"),
                &result.label,
                &result.detail,
                0,
            ));
        }
        rows.push(
            Element::scroll("editor_results_scroll", Element::column(items))
                .size(Some(width), Some(140.0)),
        );
    }
    rows
}
pub(crate) fn language_for_path(path: &str) -> CodeLanguage {
    CodeLanguage::from_extension(
        std::path::Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default(),
    )
}
/// Markers that comment out one line of `path`. Files without a known
/// language fall back on what their extension conventionally uses.
pub(crate) fn comment_markers(
    path: &str,
    language: Option<CodeLanguage>,
) -> (&'static str, &'static str) {
    language
        .unwrap_or_else(|| language_for_path(path))
        .comment_markers()
        .unwrap_or_else(|| {
            let path = std::path::Path::new(path);
            let name = |part: Option<&std::ffi::OsStr>| {
                part.and_then(|part| part.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase()
            };
            match (name(path.file_stem()).as_str(), name(path.extension()).as_str()) {
                (_, "sh" | "bash" | "zsh" | "fish" | "yaml" | "yml" | "nix" | "rb")
                | ("makefile" | "dockerfile", _) => ("#", ""),
                _ => ("//", ""),
            }
        })
}
#[cfg(test)]
mod compact_menu_tests {
    use super::*;

    #[test]
    fn comment_markers_follow_the_language_then_the_extension() {
        assert_eq!(comment_markers("src/main.rs", None), ("//", ""));
        assert_eq!(comment_markers("tool.PY", None), ("#", ""));
        assert_eq!(comment_markers("site.css", None), ("/*", "*/"));
        assert_eq!(comment_markers("index.html", None), ("<!--", "-->"));
        assert_eq!(comment_markers("README.md", None), ("<!--", "-->"));
        assert_eq!(comment_markers("flake.nix", None), ("#", ""));
        assert_eq!(comment_markers("ci.YML", None), ("#", ""));
        assert_eq!(comment_markers("main.go", None), ("//", ""));
        assert_eq!(comment_markers("build/Makefile", None), ("#", ""));
        assert_eq!(comment_markers("Dockerfile.dev", None), ("#", ""));
        assert_eq!(comment_markers("notes", Some(CodeLanguage::Toml)), ("#", ""));
        assert_eq!(comment_markers("a.rs", Some(CodeLanguage::Css)), ("/*", "*/"));
    }
    use eframe::egui;
    use openide_editor::{Selection, TextBuffer};
    use openide_ui::{
        CodeEditor, DrawCommand, EditorController, Frame, Key, Point, Size, UiAction, UiEvent,
        UiTree,
    };

    fn native_tree(root: Element) -> (egui::Context, UiTree) {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        let fonts = ctx.clone();
        let mut tree = UiTree::new(root);
        tree.set_reduced_motion(true);
        tree.set_context_menu_palette(theme::menu_palette());
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

    fn editor() -> Element {
        let mut buffer =
            TextBuffer::new("fn main() {\n    let café = 42;\n    println!(\"{café}\");\n}\n")
                .unwrap();
        buffer
            .set_selection(Selection {
                anchor: 20,
                head: 24,
            })
            .unwrap();
        CodeEditor::new("editor", EditorController::new(buffer))
            .language(CodeLanguage::Rust)
            .context_actions(context_actions(true))
            .height(500.0)
            .palette(theme::component_palette())
            .render()
    }

    #[test]
    fn compact_context_menu_keeps_primary_actions_legible_and_more_click_connected() {
        let size = Size::new(520.0, 640.0);
        let (ctx, mut tree) = native_tree(editor());
        tree.layout(size);
        tree.focus("editor");
        tree.handle_event(UiEvent::ContextMenuKeyboard);
        let frame = tree.layout(size);
        let menu = frame
            .regions
            .iter()
            .find(|region| region.id == "__context_menu:surface")
            .unwrap()
            .rect;
        assert_eq!(menu.width, 352.0);
        assert!(menu.height <= 384.0);
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.id.starts_with("__context_menu:item:"))
                .count(),
            10
        );
        assert!(!frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.contains("Marcador pendiente"))));
        for label in [
            "Cortar",
            "Copiar",
            "Pegar",
            "Seleccionar todo",
            "Deshacer",
            "Rehacer",
            "Comentar / descomentar",
            "Ir a definición",
            "Renombrar símbolo…",
            "Más acciones…",
        ] {
            assert!(
                frame.commands.iter().any(
                    |command| matches!(command, DrawCommand::Text { text, .. } if text == label)
                ),
                "missing full label: {label}"
            );
        }
        let point = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text == "Más acciones…" => {
                    Some(Point::new(position.x + 5.0, position.y + 7.0))
                }
                _ => None,
            })
            .unwrap();
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "editor_menu".into()
            }]
        );
        assert!(!tree.context_menu_open());
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn grouped_more_actions_preserve_every_command_once_and_last_item_is_reachable() {
        let entries = more_actions(true);
        let ids: std::collections::BTreeSet<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                DropdownMenuEntry::Item(item) => Some(item.action_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 33);
        assert!(entries.iter().any(|entry| matches!(entry,
            DropdownMenuEntry::Item(item)
                if item.action_id == "editor_breakpoint" && item.shortcut.as_deref() == Some("F9"))));
        assert_eq!(
            entries
                .iter()
                .filter(|entry| matches!(entry, DropdownMenuEntry::Item(_)))
                .count(),
            ids.len()
        );
        for action in [
            "undo",
            "redo",
            "find",
            "replace",
            "outline",
            "goto",
            "duplicate",
            "move_up",
            "move_down",
            "delete_lines",
            "comment",
            "select_next",
            "select_all_matches",
            "cursor_above",
            "cursor_below",
            "completion",
            "hover",
            "definition",
            "references",
            "rename",
            "format",
            "code_actions",
            "copy_path",
            "copy_relative",
            "new",
            "save_as",
            "fold",
            "unfold",
            "breakpoint",
            "snippet",
            "settings",
            "wrap",
            "minimap",
        ] {
            assert!(
                ids.contains(format!("editor_{action}").as_str()),
                "lost action {action}"
            );
        }
        let root = DropdownMenu::new(
            Element::button("more", "Más acciones"),
            entries,
            true,
            "dismiss",
            theme::menu_palette(),
        )
        .width(288.0)
        .max_visible_items(8)
        .render();
        let (ctx, mut tree) = native_tree(root);
        let size = Size::new(520.0, 640.0);
        let frame = tree.layout(size);
        let options = frame
            .regions
            .iter()
            .find(|region| region.id == "dismiss:options")
            .unwrap()
            .rect;
        assert!(options.height <= 304.0);
        let marker = frame
            .regions
            .iter()
            .find(|region| region.id == "editor_breakpoint")
            .expect("pending marker is reachable through the real More menu")
            .rect;
        let point = Point::new(
            marker.x + marker.width / 2.0,
            marker.y + marker.height / 2.0,
        );
        assert_eq!(frame.hit_test(point), Some("editor_breakpoint"));
        tree.handle_event(UiEvent::PointerDown(point));
        assert!(
            tree.handle_event(UiEvent::PointerUp(point))
                .contains(&UiAction::Clicked {
                    id: "editor_breakpoint".into(),
                })
        );
        tree.handle_event(UiEvent::ScrollDirect {
            position: Point::new(options.x + 20.0, options.y + 20.0),
            delta_y: -100_000.0,
        });
        let frame = tree.layout(size);
        let last = frame
            .regions
            .iter()
            .find(|region| region.id == "editor_settings")
            .unwrap()
            .rect;
        let point = Point::new(last.x + last.width / 2.0, last.y + last.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert!(
            tree.handle_event(UiEvent::PointerUp(point)).iter().any(
                |action| matches!(action, UiAction::Clicked { id } if id == "editor_settings")
            )
        );
        ctx.end_pass().textures_delta.clear();
    }

    fn export(frame: &Frame, size: Size, name: &str) {
        let directory = std::path::Path::new("/tmp/openade-compact-context-review");
        std::fs::create_dir_all(directory).unwrap();
        let path = directory.join(format!("{name}.svg"));
        crate::settings_ui::visual_qa::write_native_frame_svg(
            frame,
            &path,
            size.width,
            size.height,
        )
        .unwrap();
        let svg = resvg::usvg::Tree::from_str(
            &std::fs::read_to_string(&path).unwrap(),
            &crate::settings_ui::visual_qa::native_svg_options(),
        )
        .unwrap();
        let mut image =
            resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
        resvg::render(
            &svg,
            resvg::tiny_skia::Transform::identity(),
            &mut image.as_mut(),
        );
        image.save_png(path.with_extension("png")).unwrap();
    }

    #[test]
    #[ignore = "opt-in compact editor context menus with native fonts"]
    fn export_compact_editor_context_menu_review() {
        for size in [
            Size::new(520.0, 640.0),
            Size::new(1100.0, 760.0),
            Size::new(360.0, 280.0),
        ] {
            let (ctx, mut tree) = native_tree(
                Element::panel(editor())
                    .padding(16.0)
                    .background(theme::BACKGROUND),
            );
            tree.layout(size);
            tree.focus("editor");
            tree.handle_event(UiEvent::ContextMenuKeyboard);
            let frame = tree.layout(size);
            export(
                &frame,
                size,
                &format!("context-{}x{}", size.width as u32, size.height as u32),
            );
            tree.handle_event(UiEvent::Key {
                key: Key::End,
                ctrl: false,
                shift: false,
            });
            let frame = tree.layout(size);
            export(
                &frame,
                size,
                &format!("context-last-{}x{}", size.width as u32, size.height as u32),
            );
            ctx.end_pass().textures_delta.clear();
        }
        let size = Size::new(520.0, 640.0);
        let root = DropdownMenu::new(
            Element::button("more", "Más acciones"),
            more_actions(true),
            true,
            "dismiss",
            theme::menu_palette(),
        )
        .width(288.0)
        .max_visible_items(8)
        .render();
        let (ctx, mut tree) = native_tree(
            Element::panel(root)
                .padding(16.0)
                .background(theme::BACKGROUND),
        );
        export(&tree.layout(size), size, "more-actions");
        ctx.end_pass().textures_delta.clear();
    }
}
