//! Compact document status. Rendering reads snapshots; the host validates edits.
use crate::{
    theme,
    view::{SideTab, ViewState},
};
use openide_editor::IndentStyle;
use openide_ui::{
    AlertColors, AlertTone, Button, ButtonVariant, CodeLanguage, Color, Component,
    ComponentPalette, ControlSize, DropdownMenuEntry, DropdownMenuItem, Element, FontFamily, Icon,
    Popover, PopoverPlacement, ScrollArea, Separator, StatusBar, StatusBarItem,
    StatusBarItemSizing, TextField, TooltipSpec,
};

// The window reserves one more pixel above the cells, for its divider.
pub(crate) const STATUS_HEIGHT: f32 = StatusBar::HEIGHT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusMenu {
    Branch,
    Indent,
    Ending,
    Language,
    Services,
    Overflow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StatusScope {
    pub workspace: String,
    pub path: String,
    pub tab: u64,
    pub document: u64,
    pub revision: u64,
}
impl StatusScope {
    pub fn prefix(&self) -> String {
        format!(
            "ide_status:{}:{}:{}",
            self.tab, self.document, self.revision
        )
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct IdeStatusView {
    pub menu: Option<StatusMenu>,
    pub scope: Option<StatusScope>,
    pub workspace: Option<String>,
    pub service_error: Option<String>,
    pub service_language: Option<CodeLanguage>,
    pub service_connecting: bool,
    pub service_preferences_sequence: Option<u64>,
}

pub(crate) fn document_scope(state: &ViewState) -> Option<StatusScope> {
    if state.side_tab != SideTab::FilePreview
        || (crate::ide_workspace::active(state) && state.ide.extensions_open)
        || state
            .document_preview
            .as_ref()
            .is_some_and(|view| !view.is_source_visible())
    {
        return None;
    }
    let view = state.code_editor.as_ref()?;
    let snapshot = view.controller.as_ref()?.snapshot().ok()?;
    if view.loading
        || state.editor_workbench.document_id != Some(snapshot.document_id())
        || !state
            .dock_tabs
            .iter()
            .any(|tab| tab.active && tab.id == view.tab_id)
    {
        return None;
    }
    Some(StatusScope {
        workspace: state.workspace.clone(),
        path: view.path.clone(),
        tab: view.tab_id,
        document: snapshot.document_id(),
        revision: snapshot.revision(),
    })
}

struct DocumentStatus {
    scope: StatusScope,
    cursor: String,
    compact_cursor: String,
    selection: usize,
    language: &'static str,
    language_id: CodeLanguage,
    ending: &'static str,
    indent: String,
    save: &'static str,
    errors: usize,
    warnings: usize,
    information: usize,
    hints: usize,
    unclassified: usize,
    stale: bool,
}

fn language_name(language: CodeLanguage) -> &'static str {
    match language {
        CodeLanguage::Plain => "Texto",
        CodeLanguage::Rust => "Rust",
        CodeLanguage::Json => "JSON",
        CodeLanguage::JavaScript => "JavaScript",
        CodeLanguage::TypeScript => "TypeScript",
        CodeLanguage::Tsx => "TSX",
        CodeLanguage::Python => "Python",
        CodeLanguage::Toml => "TOML",
        CodeLanguage::Css => "CSS",
        CodeLanguage::Html => "HTML",
        CodeLanguage::Markdown => "Markdown",
    }
}

fn document_status(state: &ViewState) -> Option<DocumentStatus> {
    let scope = document_scope(state)?;
    let view = state.code_editor.as_ref()?;
    let snapshot = view.controller.as_ref()?.snapshot().ok()?;
    let workbench = &state.editor_workbench;
    let position = snapshot
        .position_of(snapshot.selection().head)
        .unwrap_or_default();
    let language = workbench
        .language_override
        .unwrap_or_else(|| CodeLanguage::from_path(&view.path));
    let mut errors = 0;
    let mut warnings = 0;
    let mut information = 0;
    let mut hints = 0;
    let mut unclassified = 0;
    for diagnostic in &workbench.diagnostics {
        match diagnostic.severity {
            Some(1) => errors += 1,
            Some(2) => warnings += 1,
            Some(3) => information += 1,
            Some(4) => hints += 1,
            _ => unclassified += 1,
        }
    }
    Some(DocumentStatus {
        scope,
        cursor: format!("Ln {}, Col {}", position.row + 1, position.column + 1),
        compact_cursor: format!("{}:{}", position.row + 1, position.column + 1),
        selection: snapshot.selection().range().len(),
        language: language_name(language),
        language_id: language,
        ending: match snapshot.preferred_line_ending() {
            openide_editor::LineEnding::Lf => "LF",
            openide_editor::LineEnding::CrLf => "CRLF",
            openide_editor::LineEnding::Cr => "CR",
        },
        indent: format!(
            "{}: {}",
            if matches!(workbench.indent, IndentStyle::Tabs(_)) {
                "Tabs"
            } else {
                "Espacios"
            },
            workbench.indent.width()
        ),
        save: if view.saving {
            "Guardando…"
        } else if view.conflict {
            "Conflicto en disco"
        } else if snapshot.is_dirty() {
            "Sin guardar"
        } else {
            "Guardado"
        },
        errors,
        warnings,
        information,
        hints,
        unclassified,
        stale: !workbench.diagnostics.is_empty()
            && (!workbench.diagnostics_current
                || workbench.diagnostic_revision
                    != Some((snapshot.document_id(), snapshot.revision()))),
    })
}

// Conservative reservations for short labels, instead of fixed status columns.
// Native font measurement still owns glyph layout and clipping.
fn label_width(value: &str) -> f32 {
    value
        .chars()
        .map(|c| {
            if c == ' ' {
                3.5
            } else if "ilI.,:|".contains(c) {
                3.5
            } else {
                6.5
            }
        })
        .sum::<f32>()
        + 8.0
}

// The glyph of the bell cell never took the muted ink of its row; see the
// ledger before changing it.
const GLYPH_INK: Color = Color::WHITE;

/// The color the Problems panel gives a severity.
fn tone_ink(tone: openide_ui::AlertTone) -> Color {
    openide_ui::AlertColors::for_tone(tone, theme::component_palette()).accent
}

fn cell_palette(surface: Color) -> ComponentPalette {
    theme::action_palette(surface, theme::FOREGROUND_SUBTLE)
}

// A hint above the bar, in the window's own tooltip colors.
fn hint(title: impl Into<String>) -> TooltipSpec {
    TooltipSpec::new(title).palette(theme::component_palette())
}

fn control(
    id: &str,
    action: &str,
    value: &str,
    icon: Option<Icon>,
    hint: TooltipSpec,
    width: f32,
    surface: Color,
) -> StatusBarItem {
    let item = StatusBarItem::new(id, width, cell_palette(surface));
    match icon {
        Some(icon) => item.icon(icon),
        None => item,
    }
    .label(value)
    .dispatch_as(action)
    .tooltip_with(hint)
}

fn scoped_entries(
    state: &ViewState,
    status: &DocumentStatus,
    kind: StatusMenu,
) -> Vec<DropdownMenuEntry> {
    let prefix = status.scope.prefix();
    let item = |operation: &str, value: &str| {
        DropdownMenuItem::new(format!("{prefix}:{operation}"), value)
    };
    let workbench = &state.editor_workbench;
    match kind {
        StatusMenu::Indent => {
            let mut entries = vec![
                DropdownMenuEntry::Heading("Sangría al editar".into()),
                item("indent:spaces", "Usar espacios")
                    .selected(matches!(workbench.indent, IndentStyle::Spaces(_)))
                    .into(),
                item("indent:tabs", "Usar tabulaciones")
                    .selected(matches!(workbench.indent, IndentStyle::Tabs(_)))
                    .into(),
                DropdownMenuEntry::Separator,
                DropdownMenuEntry::Heading("Ancho de tabulación".into()),
            ];
            entries.extend([2, 4, 8].into_iter().map(|width| {
                item(&format!("width:{width}"), &width.to_string())
                    .selected(workbench.indent.width() == width)
                    .into()
            }));
            entries
        }
        StatusMenu::Ending => vec![
            DropdownMenuEntry::Heading("Convertir finales de línea".into()),
            item("ending:lf", "LF · Unix / macOS")
                .selected(status.ending == "LF")
                .into(),
            item("ending:crlf", "CRLF · Windows")
                .selected(status.ending == "CRLF")
                .into(),
            item("ending:cr", "CR · Macintosh clásico")
                .selected(status.ending == "CR")
                .into(),
        ],
        StatusMenu::Language => {
            let mut entries = vec![
                item("language:auto", "Detectar por extensión")
                    .selected(workbench.language_override.is_none())
                    .into(),
                DropdownMenuEntry::Separator,
            ];
            entries.extend(
                [
                    ("plain", CodeLanguage::Plain),
                    ("markdown", CodeLanguage::Markdown),
                    ("rust", CodeLanguage::Rust),
                    ("javascript", CodeLanguage::JavaScript),
                    ("typescript", CodeLanguage::TypeScript),
                    ("tsx", CodeLanguage::Tsx),
                    ("python", CodeLanguage::Python),
                    ("json", CodeLanguage::Json),
                    ("toml", CodeLanguage::Toml),
                    ("css", CodeLanguage::Css),
                    ("html", CodeLanguage::Html),
                ]
                .into_iter()
                .map(|(key, language)| {
                    item(&format!("language:{key}"), language_name(language))
                        .selected(workbench.language_override == Some(language))
                        .into()
                }),
            );
            entries
        }
        StatusMenu::Overflow => vec![
            DropdownMenuEntry::Heading(status.save.into()),
            item("goto", &status.cursor).shortcut("Ctrl+G").into(),
            item("indent_menu", &status.indent)
                .submenu(scoped_entries(state, status, StatusMenu::Indent))
                .into(),
            item("ending_menu", status.ending)
                .submenu(scoped_entries(state, status, StatusMenu::Ending))
                .into(),
            item("language_menu", status.language)
                .submenu(scoped_entries(state, status, StatusMenu::Language))
                .into(),
            item("open:services", "Servicios de lenguaje…")
                .icon(Icon::Server)
                .into(),
            item("diagnostics", "Problemas del archivo")
                .icon(Icon::WarningTriangle)
                .into(),
            DropdownMenuEntry::Separator,
            item("encoding", "UTF-8 · Codificación del archivo")
                .disabled()
                .into(),
        ],
        StatusMenu::Branch | StatusMenu::Services => Vec::new(),
    }
}

fn document_control(
    state: &ViewState,
    status: &DocumentStatus,
    kind: StatusMenu,
    id: &str,
    value: &str,
    title: &str,
    width: f32,
    surface: Color,
) -> StatusBarItem {
    let key = match kind {
        StatusMenu::Indent => "indent",
        StatusMenu::Ending => "ending",
        StatusMenu::Language => "language",
        StatusMenu::Overflow => "overflow",
        StatusMenu::Branch | StatusMenu::Services => unreachable!(),
    };
    let prefix = status.scope.prefix();
    let open = state.ide_status.menu == Some(kind)
        && state.ide_status.scope.as_ref() == Some(&status.scope);
    control(
        id,
        &format!("{prefix}:open:{key}"),
        value,
        (kind == StatusMenu::Overflow).then_some(Icon::More),
        hint(title),
        width,
        surface,
    )
    .menu(
        scoped_entries(state, status, kind),
        open,
        format!("{prefix}:dismiss"),
        theme::menu_palette(),
    )
}

fn services_open(state: &ViewState, status: &DocumentStatus) -> bool {
    state.ide_status.menu == Some(StatusMenu::Services)
        && state.ide_status.scope.as_ref() == Some(&status.scope)
        && state.ide_status.service_language == Some(status.language_id)
}

fn services_control(
    state: &ViewState,
    status: &DocumentStatus,
    width: f32,
    viewport_width: f32,
    surface: Color,
) -> StatusBarItem {
    let prefix = status.scope.prefix();
    let id = "ide_editor_language_service";
    let item = control(
        id,
        &format!("{prefix}:open:services"),
        if width > 20.0 { "LSP" } else { "" },
        Some(Icon::Server),
        hint("Servicios de lenguaje").description(&state.editor_workbench.language_status),
        width,
        surface,
    );
    if !services_open(state, status) {
        return item;
    }
    // The form is a popover of the app's own, anchored on the rendered cell.
    StatusBarItem::new(id, width, cell_palette(surface)).hosted(services_popover(
        state,
        status,
        item.render(),
        viewport_width,
    ))
}

/// The account's tightest limit and its width; the same popover as the
/// sidebar's "Cuenta y uso" row opens from it.
fn usage_control(state: &ViewState, surface: Color) -> Option<(StatusBarItem, f32)> {
    let usage = crate::provider_usage_ui::glance(state)?;
    let id = "ide_usage";
    let width = label_width(&usage.value) + 16.0;
    let palette = cell_palette(surface);
    let palette = ComponentPalette {
        muted: usage.ink.unwrap_or(palette.muted),
        ..palette
    };
    let item = StatusBarItem::new(id, width, palette)
        .icon(usage.icon)
        .label(&usage.value)
        .dispatch_as("usage_toggle")
        .tooltip_with(hint(usage.title).description(usage.detail));
    if !state.provider_usage_open {
        return Some((item, width));
    }
    let popover = Popover::new(
        item.render(),
        crate::provider_usage_ui::popup(state),
        true,
        "usage_dismiss",
        theme::menu_palette(),
    )
    .placement(PopoverPlacement::TopEnd)
    .width(crate::provider_usage_ui::POPUP_WIDTH)
    .render();
    Some((StatusBarItem::new(id, width, palette).hosted(popover), width))
}

fn services_popover(
    state: &ViewState,
    status: &DocumentStatus,
    anchor: Element,
    viewport_width: f32,
) -> Element {
    let prefix = status.scope.prefix();
    let workbench = &state.editor_workbench;
    let popup_width = (viewport_width - 16.0).clamp(96.0, 340.0);
    // A form has its own focus scope when opened from the overflow menu.
    let dismiss = format!("{prefix}:services_dismiss");
    let input = format!("{prefix}:server_path");
    let connect = format!("{prefix}:server_connect");
    let plain = status.language_id == CodeLanguage::Plain;
    let palette = theme::menu_palette();
    let prose = |value: &str, subtle: bool| {
        Element::text(value)
            .font_family(FontFamily::Sans)
            .font_size(if subtle { 11.0 } else { 12.0 })
            .foreground(if subtle { palette.muted } else { palette.text })
    };
    let mut rows = vec![
        openide_ui::PanelHeader::new("", palette)
            .leading(
                Element::row(vec![
                    Element::icon(Icon::Server).size(Some(16.0), Some(16.0)),
                    Element::truncated_text("Servicios de lenguaje")
                        .font_family(FontFamily::Sans)
                        .font_size(12.0)
                        .foreground(palette.text)
                        .flex(1.0),
                ])
                .gap(6.0),
            )
            .dismiss(&dismiss, "")
            .sizing(openide_ui::HeaderSizing::POPOVER)
            .render()
            .foreground(palette.text),
        Element::row(vec![
            prose("Lenguaje", true).flex(1.0),
            prose(status.language, false),
        ])
        .gap(6.0),
        Element::row(vec![
            Element::icon(if workbench.language_ready && !plain {
                Icon::CheckCircle
            } else {
                Icon::InfoCircle
            })
            .foreground(if workbench.language_ready && !plain {
                AlertColors::for_tone(AlertTone::Success, palette).accent
            } else {
                palette.muted
            })
            .size(Some(14.0), Some(16.0)),
            prose(
                if plain {
                    "Texto sin servidor de lenguaje."
                } else if workbench.language_status.is_empty() {
                    "El servidor todavía no informó su estado."
                } else {
                    &workbench.language_status
                },
                false,
            )
            .flex(1.0),
        ])
        .gap(6.0),
        Separator::horizontal(palette).render(),
    ];
    if plain {
        rows.push(prose(
            "Elegí el lenguaje desde la barra de estado para conectar un servidor.",
            true,
        ));
    } else {
        rows.extend([
            prose("Ejecutable del servidor · opcional", true),
            TextField::new(
                &input,
                &workbench.server_path,
                "Detección automática",
                theme::component_palette(),
            )
            .size(ControlSize::Compact)
            .persistent_placeholder(true)
            .render(),
            prose(
                "Dejá el campo vacío para detectar el servidor automáticamente.",
                true,
            ),
        ]);
        if let Some(error) = &state.ide_status.service_error {
            rows.push(prose(error, false).foreground(theme::DESTRUCTIVE));
        }
        rows.push(
            Button::new(
                &connect,
                if state.ide_status.service_connecting {
                    "Conectando…"
                } else if workbench.language_ready {
                    "Reiniciar"
                } else {
                    "Conectar"
                },
                palette,
            )
            .icon(if workbench.language_ready {
                Icon::Refresh
            } else {
                Icon::Server
            })
            .size(ControlSize::Compact)
            .enabled(!state.ide_status.service_connecting)
            .render(),
        );
    }
    rows.push(
        Button::new(
            format!("{prefix}:diagnostics"),
            "Ver problemas del archivo",
            palette,
        )
        .icon(Icon::WarningTriangle)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .render(),
    );
    let content = ScrollArea::new(
        format!("{prefix}:services_scroll"),
        Element::column(rows).gap(8.0).padding(6.0),
    )
    .render()
    .max_height(340.0);
    let popover = Popover::new(anchor, content, true, &dismiss, palette)
        .width(popup_width)
        .placement(PopoverPlacement::TopStart)
        .render();
    if plain {
        popover.popover_initial_focus(dismiss)
    } else {
        popover.popover_initial_focus(input)
    }
}

fn branch_entries() -> Vec<DropdownMenuEntry> {
    vec![
        DropdownMenuItem::new("ide_status_branch:checkout", "Cambiar rama…")
            .icon(Icon::GitBranch)
            .into(),
        DropdownMenuItem::new("ide_status_branch:review", "Cambios del repositorio")
            .icon(Icon::Code)
            .into(),
    ]
}

fn branch_open(state: &ViewState) -> bool {
    state.ide_status.menu == Some(StatusMenu::Branch)
        && state.ide_status.workspace.as_deref() == Some(&state.workspace)
}

fn branch(state: &ViewState, width: f32) -> StatusBarItem {
    let value = state.project_branch.as_deref().unwrap_or("Git");
    // The one value the bar is read for stands out, under its host's mark
    // when the repository has one.
    let icon = if state.project_on_github { Icon::Github } else { Icon::GitBranch };
    // Its own palette: the other cells' text ink is the subtle one.
    StatusBarItem::new(
        "ide_git_review",
        width,
        theme::action_palette(theme::WINDOW, theme::FOREGROUND),
    )
    .icon(icon)
    .label(if width > 24.0 { value } else { "" })
    .strong()
    .ink(theme::FOREGROUND)
    .dispatch_as("ide_status_branch_open")
    .tooltip_with(hint(format!("{value} · Ramas y cambios del repositorio")))
    .menu(
        branch_entries(),
        branch_open(state),
        "ide_status_branch_dismiss",
        theme::menu_palette(),
    )
}

/// Full-height cells below the window's status bar divider.
pub(crate) fn footer(state: &ViewState, width: f32) -> Element {
    if width < 240.0 {
        return status_cells(state, width, true).render();
    }
    let unread = state.notifications.unread();
    // Kept clear of the window's rounded corner so the hover cell is whole.
    let corner_inset = 12.0;
    let bell_width = if unread > 0 {
        label_width(&unread.min(99).to_string()) + 28.0
    } else {
        28.0
    };
    let bell = StatusBarItem::new(
        "notifications_toggle",
        bell_width,
        cell_palette(theme::WINDOW),
    )
    .icon(crate::notifications_ui::bell_icon(&state.notifications));
    let bell = if unread > 0 {
        bell.count(unread.min(99))
    } else {
        bell
    }
    .ink(GLYPH_INK)
    .centered()
    .sizing(crate::ui_sizing::STATUS_BELL)
    .tooltip_with(hint("Notificaciones"))
    .render();
    Element::row(vec![
        status_cells(state, width - bell_width - corner_inset, true).render(),
        Element::spacer().flex(1.0),
        crate::notifications_ui::popover(&state.notifications, bell, true),
        Element::spacer().size(Some(corner_inset), None),
    ])
    .size(Some(width), Some(STATUS_HEIGHT))
}

/// The dock owns one compact status row. It shares the IDE's document cells,
/// current snapshot, scoped actions and overflow; repository state stays in
/// the workspace chrome instead of being repeated under each document.
pub(crate) fn document_footer(state: &ViewState, width: f32) -> Element {
    status_cells(state, width, false).top_hairline(true).render()
}

fn status_cells(state: &ViewState, width: f32, workspace: bool) -> StatusBar {
    let width = width.max(0.0);
    let surface = if workspace {
        theme::WINDOW
    } else {
        theme::BACKGROUND
    };
    let palette = cell_palette(surface);
    let bar = |leading, trailing| StatusBar::new(leading, trailing, palette).width(width);
    if width < 24.0 {
        return bar(Vec::new(), Vec::new());
    }
    let status = document_status(state);
    // At an unusually narrow window width, document actions take precedence
    // over the repository shortcut so both LSP and overflow remain reachable.
    let branch_width = if !workspace || (status.is_some() && width < 144.0) {
        0.0
    } else if width >= 640.0 {
        (label_width(state.project_branch.as_deref().unwrap_or("Git")) + 16.0).min(180.0)
    } else if width >= 480.0 {
        100.0
    } else {
        20.0
    };
    let mut leading = if branch_width > 0.0 {
        vec![branch(state, branch_width)]
    } else {
        Vec::new()
    };
    // The agents sidebar already carries it beside the dock's own footer.
    let usage = workspace
        .then(|| usage_control(state, surface))
        .flatten();
    let Some(status) = status else {
        return bar(leading, usage.into_iter().map(|(item, _)| item).collect());
    };
    // A hairline parts the repository from the document's own state.
    let divider_width = if branch_width > 24.0 { 1.0 + StatusBar::GAP } else { 0.0 };
    if divider_width > 0.0 {
        leading.push(StatusBarItem::divider("ide_status_divider", palette));
    }
    let prefix = status.scope.prefix();
    let diagnostic_hint = format!(
        "{} errores · {} advertencias · {} informativos · {} sugerencias · {} sin severidad{} · Mostrar problemas del archivo",
        status.errors,
        status.warnings,
        status.information,
        status.hints,
        status.unclassified,
        if status.stale {
            " · Diagnósticos desactualizados"
        } else {
            ""
        }
    );
    // A severity shows its color only while the file has it; a clean file
    // says so with one check.
    let clean = status.errors == 0 && status.warnings == 0;
    let problem_width = if clean {
        label_width("0") + 12.0
    } else {
        label_width(&status.errors.to_string()) + label_width(&status.warnings.to_string()) + 24.0
    };
    let problems = StatusBarItem::new("ide_editor_diagnostics", problem_width, palette);
    let severity = |item: StatusBarItem, icon, tone, count: usize| {
        if count > 0 { item.tinted_icon(icon, tone_ink(tone)) } else { item.icon(icon) }.count(count)
    };
    let problems = if clean {
        problems.tinted_icon(Icon::CheckCircle, tone_ink(openide_ui::AlertTone::Success)).count(0)
    } else {
        let problems =
            severity(problems, Icon::ErrorCircle, openide_ui::AlertTone::Error, status.errors);
        severity(problems, Icon::WarningTriangle, openide_ui::AlertTone::Warning, status.warnings)
    };
    leading.push(
        problems
            .sizing(StatusBarItemSizing::COUNTS)
            .dispatch_as(format!("{prefix}:diagnostics"))
            .tooltip_with(hint(diagnostic_hint)),
    );
    let service_width = if width >= 520.0 { 44.0 } else { 20.0 };
    leading.push(services_control(
        state,
        &status,
        service_width,
        width,
        surface,
    ));
    // The reservation is this bar's own rule, so the breakpoints stay put.
    let budget =
        (width - branch_width - divider_width - problem_width - service_width - 20.0).max(0.0);
    let full_cursor = if status.selection > 0 {
        format!("{} · {} sel.", status.cursor, status.selection)
    } else {
        status.cursor.clone()
    };
    let cursor = if budget < 150.0 {
        &status.compact_cursor
    } else {
        &full_cursor
    };
    let passive = |id: &str, value: &str, hint: TooltipSpec| {
        StatusBarItem::new(id, label_width(value), palette)
            .label(value)
            .passive()
            .tooltip_with(hint)
    };
    let document = |kind, id, value: &str, title: &str| {
        document_control(state, &status, kind, id, value, title, label_width(value), surface)
    };
    // Shown in this order; the priorities say which give way first.
    let mut trailing = vec![
        passive(
            "ide_editor_save_status",
            status.save,
            hint(format!("{} · Guardar archivo", status.save)).shortcut("Ctrl+S"),
        )
        .priority(1),
        control(
            "ide_editor_cursor",
            &format!("{prefix}:goto"),
            cursor,
            None,
            hint(format!("{full_cursor} · Ir a línea y columna")).shortcut("Ctrl+G"),
            label_width(cursor),
            surface,
        )
        .priority(6),
        document(
            StatusMenu::Indent,
            "ide_editor_indent",
            &status.indent,
            "Sangría y ancho de tabulación",
        )
        .priority(4),
        passive(
            "ide_editor_encoding",
            "UTF-8",
            hint("UTF-8 · Codificación validada al abrir; los archivos se guardan en UTF-8"),
        )
        .priority(2),
        document(
            StatusMenu::Ending,
            "ide_editor_ending",
            status.ending,
            "Cambiar finales de línea · Se puede deshacer",
        )
        .priority(3),
        document(
            StatusMenu::Language,
            "ide_editor_language",
            status.language,
            &format!("{} · Cambiar modo de lenguaje", status.language),
        )
        .priority(5),
    ];
    // The first to give way: it stands only where every document cell does,
    // so it never brings the overflow in or pushes one of them out.
    if let Some((item, usage_width)) = usage {
        let cells: f32 = [status.save, cursor, &status.indent, "UTF-8", status.ending, status.language]
            .iter()
            .map(|value| label_width(value) + StatusBar::GAP)
            .sum();
        if cells + usage_width + StatusBar::GAP <= budget {
            trailing.push(item.priority(0));
        }
    }
    bar(leading, trailing)
        .trailing_budget(budget)
        .overflow(document_control(
            state,
            &status,
            StatusMenu::Overflow,
            "ide_editor_status_more",
            "",
            "Estado y opciones del archivo",
            20.0,
            surface,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_editor::{Selection, TextBuffer};
    use openide_ui::{EditorController, HitKind, Point, Size, UiAction, UiEvent};

    pub(super) fn editor_state() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.ide.mode = crate::ide_workspace::WindowMode::Ide;
        state.project_branch = Some("feature/footer".into());
        let mut buffer = TextBuffer::new("árbol\r\n🦀 rust\r\n").unwrap();
        buffer
            .set_selection(Selection { anchor: 7, head: 9 })
            .unwrap();
        let controller = EditorController::new(buffer);
        state.editor_workbench.document_id = Some(controller.snapshot().unwrap().document_id());
        state.side_tab = SideTab::FilePreview;
        state.code_editor = Some(crate::code_editor_ui::CodeEditorView {
            tab_id: 7,
            path: "main.rs".into(),
            controller: Some(controller),
            loading: false,
            saving: false,
            error: None,
            notice: None,
            conflict: false,
            recovered: false,
            missing: false,
        });
        state.dock_tabs = vec![crate::view::DockTabView {
            id: 7,
            title: "main.rs".into(),
            full_path: Some("main.rs".into()),
            kind: SideTab::FilePreview,
            active: true,
            closable: true,
        }];
        state
    }

    fn root(state: &ViewState, width: f32) -> Element {
        Element::column(vec![Element::spacer().flex(1.0), footer(state, width)])
    }

    /// Codex connected, with these (label, used percent) windows measured now.
    fn with_usage(mut state: ViewState, windows: &[(&str, f64)]) -> ViewState {
        use crate::provider_usage::{ProviderUsageSnapshot, ProviderUsageStatus, UsageUnit, UsageWindow};
        let now = crate::provider_usage::now_ms();
        state.native_provider_id = "codex".into();
        state.provider_connections.insert(
            "codex".into(),
            crate::provider_connections::ConnectionSummary {
                configured: true,
                ..Default::default()
            },
        );
        state.provider_usage.insert(
            "codex".into(),
            ProviderUsageSnapshot {
                provider_id: "codex".into(),
                provider_name: "Codex".into(),
                account: None,
                status: ProviderUsageStatus::Ready,
                plan_label: Some("ChatGPT Pro".into()),
                windows: windows
                    .iter()
                    .map(|(label, used)| UsageWindow {
                        label: (*label).into(),
                        used: *used,
                        limit: Some(100.0),
                        remaining: Some(100.0 - used),
                        unit: UsageUnit::Percent,
                        reset_at_ms: Some(now + 259_260_000),
                        reset_label: None,
                    })
                    .collect(),
                balances: Vec::new(),
                fetched_at_ms: Some(now - 72_000_000),
                local: true,
                account_url: None,
                message: None,
            },
        );
        state
    }

    fn texts(frame: &openide_ui::Frame) -> String {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                openide_ui::DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    const DOCUMENT_CELLS: [&str; 4] = [
        "ide_editor_cursor",
        "ide_editor_indent",
        "ide_editor_ending",
        "ide_editor_language",
    ];

    #[test]
    fn usage_cell_shows_the_tightest_window_and_is_the_first_to_give_way() {
        let has = |frame: &openide_ui::Frame, id: &str| frame.regions.iter().any(|r| r.id == id);
        let layout = |state: &ViewState, width: f32| {
            let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(root(state, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 640.0));
            ctx.end_pass().textures_delta.clear();
            frame
        };
        // Nothing measured, nothing drawn: neither a cell nor a percentage.
        let empty = layout(&editor_state(), 1440.0);
        assert!(!has(&empty, "ide_usage") && !texts(&empty).contains('%'));
        let mut external = with_usage(editor_state(), &[]);
        assert!(!has(&layout(&external, 1440.0), "ide_usage"));
        // The dock's own footer never carries it.
        external = with_usage(editor_state(), &[("Semanal", 58.0)]);
        let dock = crate::view::ide_visual_tests::native_tree(document_footer(&external, 900.0))
            .1
            .layout(Size::new(900.0, 40.0));
        assert!(!has(&dock, "ide_usage"));

        let ink = |frame: &openide_ui::Frame, value: &str| {
            frame.commands.iter().find_map(|command| match command {
                openide_ui::DrawCommand::Text { text, color, .. } if text == value => Some(*color),
                _ => None,
            })
        };
        let state = with_usage(editor_state(), &[("5 horas", 30.0), ("Semanal", 58.0)]);
        let frame = layout(&state, 1440.0);
        let cell = frame
            .regions
            .iter()
            .find(|r| r.id == "ide_usage" && r.kind == HitKind::Button)
            .unwrap()
            .rect;
        assert_eq!((cell.height, cell.y), (STATUS_HEIGHT, 640.0 - STATUS_HEIGHT));
        assert_eq!(ink(&frame, "58%"), Some(theme::FOREGROUND_SUBTLE));
        assert!(!texts(&frame).contains("30%"));
        for (used, color) in [(85.0, theme::WARNING), (100.0, theme::DESTRUCTIVE)] {
            let state = with_usage(editor_state(), &[("Semanal", used)]);
            assert_eq!(ink(&layout(&state, 1440.0), &format!("{used:.0}%")), Some(color));
        }
        // Without a document it still stands, alone at the trailing edge.
        let mut idle = with_usage(editor_state(), &[("Semanal", 58.0)]);
        idle.dock_tabs[0].active = false;
        assert!(has(&layout(&idle, 1440.0), "ide_usage"));

        // Narrowing the bar: it leaves before any document cell or the
        // overflow changes, and the bar without it is the bar there was.
        let mut alone = 0;
        for width in (240..=1440).step_by(4) {
            let (with, without) = (layout(&state, width as f32), layout(&editor_state(), width as f32));
            for id in DOCUMENT_CELLS.into_iter().chain(["ide_editor_status_more"]) {
                assert_eq!(has(&with, id), has(&without, id), "{id} at {width}");
            }
            let whole = DOCUMENT_CELLS.iter().all(|id| has(&with, id)) && !has(&with, "ide_editor_status_more");
            assert!(whole || !has(&with, "ide_usage"), "usage outlived a document cell at {width}");
            alone += usize::from(whole && !has(&with, "ide_usage"));
        }
        assert!(alone > 0, "some width must drop only the usage cell");
    }

    #[test]
    fn usage_cell_names_its_window_and_toggles_the_account_popover() {
        let mut state = with_usage(editor_state(), &[("Semanal", 58.0)]);
        let usage = crate::provider_usage_ui::glance(&state).unwrap();
        assert_eq!(usage.title, "Codex · 58% usado · Semanal");
        assert_eq!(usage.detail, "Se renueva en 3 d 0 h · Medido por Codex · hace 20 h");
        let size = Size::new(900.0, 640.0);
        let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(root(&state, size.width));
        tree.set_reduced_motion(true);
        let frame = tree.layout(size);
        let surface = |frame: &openide_ui::Frame| {
            frame
                .regions
                .iter()
                .find(|r| r.id == "popover_surface:usage_dismiss")
                .map(|r| r.rect)
        };
        assert!(surface(&frame).is_none());
        let click = |tree: &mut openide_ui::UiTree, frame: &openide_ui::Frame| {
            let rect = frame.regions.iter().find(|r| r.id == "ide_usage").unwrap().rect;
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            tree.handle_event(UiEvent::PointerUp(point))
        };
        let toggle = vec![UiAction::Clicked { id: "usage_toggle".into() }];
        assert_eq!(click(&mut tree, &frame), toggle);
        // The host flips the flag the sidebar row shares.
        state.provider_usage_open = true;
        tree.set_root(root(&state, size.width));
        let open = tree.layout(size);
        let panel = surface(&open).expect("the account popover");
        assert!(panel.x >= 0.0 && panel.x + panel.width <= size.width);
        assert!(panel.y >= 0.0 && panel.y + panel.height <= size.height - STATUS_HEIGHT + 0.1);
        let painted = texts(&open);
        assert!(painted.contains("Cuenta y uso") && painted.contains("58% usado"), "{painted}");
        assert!(tree.focus("usage_details"));
        // Open, the cell closes it the way the sidebar row does.
        assert_eq!(click(&mut tree, &open), vec![UiAction::Clicked { id: "usage_dismiss".into() }]);
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn the_branch_stands_out_under_its_hosts_mark_and_a_clean_file_shows_one_green_check() {
        let mut state = editor_state();
        let size = Size::new(1440.0, 48.0);
        let icons = |state: &ViewState| -> Vec<(Icon, Color)> {
            openide_ui::UiTree::new(root(state, size.width))
                .layout(size)
                .commands
                .iter()
                .filter_map(|command| match command {
                    openide_ui::DrawCommand::Icon { icon, tint, .. } => Some((*icon, *tint)),
                    _ => None,
                })
                .collect()
        };
        let plain = icons(&state);
        assert!(plain.contains(&(Icon::GitBranch, theme::FOREGROUND)), "{plain:?}");
        // No problems: one check in the color of success, and no severity glyph.
        let success = tone_ink(openide_ui::AlertTone::Success);
        assert!(plain.contains(&(Icon::CheckCircle, success)), "{plain:?}");
        assert!(!plain.iter().any(|(icon, _)| *icon == Icon::ErrorCircle), "{plain:?}");

        state.project_on_github = true;
        let hosted = icons(&state);
        assert!(hosted.contains(&(Icon::Github, theme::FOREGROUND)), "{hosted:?}");
        assert!(!hosted.iter().any(|(icon, _)| *icon == Icon::GitBranch), "{hosted:?}");

        // The branch reads in the text ink, and a hairline parts it from the file's state.
        let frame = openide_ui::UiTree::new(root(&state, size.width)).layout(size);
        assert!(frame.commands.iter().any(|command| matches!(command,
            openide_ui::DrawCommand::Text { text, color, .. }
                if text == state.project_branch.as_deref().unwrap() && *color == theme::FOREGROUND)));
        assert!(frame.commands.iter().any(|command| matches!(command,
            openide_ui::DrawCommand::FillRect { rect, .. } if rect.width == 1.0 && rect.height == 11.0)));
    }

    #[test]
    #[ignore = "export the IDE footer with its usage cell at wide and narrow widths"]
    fn export_ide_footer_usage_visual_review() {
        let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/provider-usage-visual-qa");
        std::fs::create_dir_all(&output).unwrap();
        let mut state = with_usage(editor_state(), &[("5 horas", 30.0), ("Semanal", 58.0)]);
        let warning = with_usage(editor_state(), &[("Semanal", 85.0)]);
        state.provider_usage_open = true;
        let mut github = with_usage(editor_state(), &[("Semanal", 85.0)]);
        github.project_on_github = true;
        for (name, state, width, height) in [
            ("1440", &warning, 1440.0, 48.0),
            ("1440-github", &github, 1440.0, 48.0),
            ("460", &warning, 460.0, 48.0),
            ("360", &warning, 360.0, 48.0),
            ("popover-700", &state, 700.0, 360.0),
        ] {
            let root = root(state, width).background(theme::WINDOW);
            let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(root);
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, height));
            let path = output.join(format!("usage-bars-ide-footer-{name}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
                .unwrap();
            let svg = std::fs::read(&path).unwrap();
            let parsed = resvg::usvg::Tree::from_data(
                &svg,
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            ctx.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn status_uses_live_unicode_coordinates_severity_and_document_identity() {
        let mut state = editor_state();
        let status = document_status(&state).unwrap();
        assert_eq!(status.cursor, "Ln 2, Col 3");
        assert_eq!(status.selection, 2);
        assert_eq!(status.language, "Rust");
        assert_eq!(status.ending, "CRLF");
        state.editor_workbench.diagnostics = [Some(1), Some(2), Some(2), Some(3), Some(4), None]
            .into_iter()
            .map(|severity| openide_language::lsp::Diagnostic {
                range: Default::default(),
                severity,
                code: None,
                source: None,
                message: "diagnóstico".into(),
            })
            .collect();
        let status = document_status(&state).unwrap();
        assert_eq!(
            (
                status.errors,
                status.warnings,
                status.information,
                status.hints,
                status.unclassified
            ),
            (1, 2, 1, 1, 1)
        );
        assert!(status.stale);
        state.editor_workbench.diagnostics_current = true;
        state.editor_workbench.diagnostic_revision =
            Some((status.scope.document, status.scope.revision));
        assert!(!document_status(&state).unwrap().stale);
        state.code_editor.as_mut().unwrap().saving = true;
        assert_eq!(document_status(&state).unwrap().save, "Guardando…");
        state.editor_workbench.document_id = Some(0);
        assert!(document_status(&state).is_none());
        state = editor_state();
        state.dock_tabs[0].active = false;
        assert!(document_status(&state).is_none());
    }

    #[test]
    fn footer_groups_fit_without_overlap_and_dispatch_scoped_actions() {
        let state = editor_state();
        let prefix = document_scope(&state).unwrap().prefix();
        for width in [120.0, 144.0, 160.0, 240.0, 320.0, 520.0, 700.0, 1440.0] {
            let size = Size::new(width, 640.0);
            let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(root(&state, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(size);
            let mut controls = frame
                .regions
                .iter()
                .filter(|r| r.kind == HitKind::Button)
                .collect::<Vec<_>>();
            controls.sort_by(|a, b| a.rect.x.total_cmp(&b.rect.x));
            let mut end = 0.0;
            for control in controls {
                let rect = control.rect;
                assert!(
                    rect.x >= end && rect.x + rect.width <= width + 0.01,
                    "{}: {rect:?}, previous {end}",
                    control.id
                );
                assert_eq!(rect.height, STATUS_HEIGHT);
                assert_eq!(rect.y, size.height - STATUS_HEIGHT);
                end = rect.x + rect.width;
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                assert_eq!(frame.hit_test(point), Some(control.id.as_str()));
                tree.handle_event(UiEvent::PointerMove(point));
                let hovered = tree.layout(size);
                let palette = theme::action_palette(theme::WINDOW, theme::FOREGROUND_SUBTLE);
                assert!(
                    hovered.commands.iter().any(|command| matches!(command,
                        openide_ui::DrawCommand::FillRect { rect: fill, color, radius }
                            if *fill == rect && *color == palette.hover && *radius == 0.0
                    )),
                    "{} must highlight its entire rectangular cell",
                    control.id
                );
                tree.handle_event(UiEvent::PointerDown(point));
                let actions = tree.handle_event(UiEvent::PointerUp(point));
                assert!(actions
                    .iter()
                    .any(|action| matches!(action, UiAction::Clicked { id }
                    if id == "ide_status_branch_open"
                        || id == "notifications_toggle"
                        || id.starts_with(&prefix))));
            }
            if width <= 320.0 {
                assert!(frame
                    .regions
                    .iter()
                    .any(|r| r.id == "ide_editor_status_more"));
                assert!(frame
                    .regions
                    .iter()
                    .any(|r| r.id == "ide_editor_language_service"));
                assert_eq!(
                    frame.regions.iter().any(|r| r.id == "ide_git_review"),
                    width >= 144.0
                );
            }
            if width >= 700.0 {
                for id in [
                    "ide_editor_cursor",
                    "ide_editor_indent",
                    "ide_editor_ending",
                    "ide_editor_language",
                ] {
                    assert!(
                        frame.regions.iter().any(|r| r.id == id),
                        "missing {id} at {width}"
                    );
                }
                assert!(!frame
                    .regions
                    .iter()
                    .any(|r| r.id == "ide_editor_status_more"));
            }
            ctx.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn footer_menus_use_standard_upward_surface_and_keep_encoding_informative() {
        let mut state = editor_state();
        let scope = document_scope(&state).unwrap();
        let prefix = scope.prefix();
        for (kind, suffix, width) in [
            (StatusMenu::Ending, "ending:cr", 700.0),
            (StatusMenu::Indent, "indent:tabs", 700.0),
            (StatusMenu::Language, "language:markdown", 700.0),
            (StatusMenu::Overflow, "goto", 240.0),
        ] {
            state.ide_status = IdeStatusView {
                menu: Some(kind),
                scope: Some(scope.clone()),
                workspace: Some(state.workspace.clone()),
                ..Default::default()
            };
            let size = Size::new(width, 640.0);
            let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(root(&state, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(size);
            let surface = frame
                .regions
                .iter()
                .find(|r| r.id == format!("popover_surface:{prefix}:dismiss"))
                .unwrap()
                .rect;
            assert!(surface.x >= 0.0 && surface.x + surface.width <= width + 0.01);
            assert!(surface.y + surface.height <= 640.0 - STATUS_HEIGHT);
            let action = format!("{prefix}:{suffix}");
            let target = frame
                .regions
                .iter()
                .find(|r| r.id == action && r.kind == HitKind::Button)
                .unwrap()
                .rect;
            let point = Point::new(
                target.x + target.width / 2.0,
                target.y + target.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert!(tree
                .handle_event(UiEvent::PointerUp(point))
                .contains(&UiAction::Clicked { id: action }));
            assert!(!frame
                .regions
                .iter()
                .any(|r| r.kind == HitKind::Button && r.id.ends_with(":encoding")));
            ctx.end_pass().textures_delta.clear();
        }
    }

    fn dock_root(state: &ViewState, width: f32) -> Element {
        Element::column(vec![
            Element::spacer().flex(1.0),
            document_footer(state, width),
        ])
        .background(theme::BACKGROUND)
    }

    #[test]
    fn document_footer_has_one_row_and_fits_native_metrics_without_losing_actions() {
        let mut state = editor_state();
        state.ide.mode = crate::ide_workspace::WindowMode::Agents;
        state.editor_workbench.language_ready = true;
        state.editor_workbench.language_status = "rust-analyzer conectado".into();
        let scope = document_scope(&state).unwrap().prefix();
        for width in [120.0, 240.0, 320.0, 520.0, 900.0] {
            let viewport = Size::new(width, 180.0);
            let (ctx, mut tree) =
                crate::view::ide_visual_tests::native_tree(dock_root(&state, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(viewport);
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "ide_git_review"));
            for id in ["ide_editor_diagnostics", "ide_editor_language_service"] {
                assert!(
                    frame.regions.iter().any(|region| region.id == id),
                    "{id} at {width}"
                );
            }
            let mut controls = frame
                .regions
                .iter()
                .filter(|region| region.kind == HitKind::Button)
                .collect::<Vec<_>>();
            controls.sort_by(|left, right| left.rect.x.total_cmp(&right.rect.x));
            let mut end = 0.0;
            for control in controls {
                let rect = control.rect;
                assert!(
                    rect.x >= end && rect.x + rect.width <= width + 0.01,
                    "{width}: {} {rect:?} after {end}",
                    control.id
                );
                assert_eq!(rect.y, viewport.height - STATUS_HEIGHT);
                assert_eq!(rect.height, STATUS_HEIGHT);
                end = rect.x + rect.width;
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                tree.handle_event(UiEvent::PointerMove(point));
                let hovered = tree.layout(viewport);
                let hover =
                    theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE).hover;
                assert!(hovered.commands.iter().any(|command| matches!(command,
                    openide_ui::DrawCommand::FillRect { rect: fill, color, radius } if *fill == rect && *color == hover && *radius == 0.0
                )), "{} should share the full-height bar hover", control.id);
                tree.handle_event(UiEvent::PointerDown(point));
                assert!(tree.handle_event(UiEvent::PointerUp(point)).iter().any(
                    |action| matches!(action,
                        UiAction::Clicked { id } if id.starts_with(&scope)
                    )
                ));
            }
            if width <= 320.0 {
                assert!(frame
                    .regions
                    .iter()
                    .any(|region| region.id == "ide_editor_status_more"));
            }
            if width == 900.0 {
                assert!(!frame
                    .regions
                    .iter()
                    .any(|region| region.id == "ide_editor_status_more"));
                for id in [
                    "ide_editor_cursor",
                    "ide_editor_indent",
                    "ide_editor_ending",
                    "ide_editor_language",
                ] {
                    assert!(frame.regions.iter().any(|region| region.id == id));
                }
            }
            assert!(frame.commands.iter().any(|command| matches!(command,
                openide_ui::DrawCommand::FillRect { rect, color, .. } if *color == theme::BORDER && rect.height == 1.0 && rect.width == width && rect.y == viewport.height - 22.0
            )));
            ctx.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn dock_overflow_keeps_document_operations_and_language_service_scoped() {
        let mut state = editor_state();
        state.ide.mode = crate::ide_workspace::WindowMode::Agents;
        let status = document_status(&state).unwrap();
        let prefix = status.scope.prefix();
        state.ide_status = IdeStatusView {
            menu: Some(StatusMenu::Overflow),
            scope: Some(status.scope),
            workspace: Some(state.workspace.clone()),
            ..Default::default()
        };
        let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(dock_root(&state, 240.0));
        let frame = tree.layout(Size::new(240.0, 420.0));
        for suffix in ["goto", "open:services", "diagnostics"] {
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == format!("{prefix}:{suffix}")
                    && region.kind == HitKind::Button));
        }
        let surface = frame
            .regions
            .iter()
            .find(|region| region.id == format!("popover_surface:{prefix}:dismiss"))
            .unwrap()
            .rect;
        assert!(surface.x >= 0.0 && surface.x + surface.width <= 240.0);
        assert!(surface.y + surface.height <= 420.0 - STATUS_HEIGHT);
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.kind == HitKind::Button && region.id.ends_with(":encoding")));
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn inactive_ide_extensions_do_not_hide_the_agents_document_status() {
        let mut state = editor_state();
        state.ide.extensions_open = true;
        state.ide.mode = crate::ide_workspace::WindowMode::Agents;
        assert!(document_scope(&state).is_some());
        state.ide.mode = crate::ide_workspace::WindowMode::Ide;
        state.main_page = crate::view::MainPage::Workspace;
        state.show_settings = false;
        assert!(document_scope(&state).is_none());
    }

    pub(super) fn services_state() -> ViewState {
        let mut state = editor_state();
        state.ide.mode = crate::ide_workspace::WindowMode::Agents;
        state.editor_workbench.server_path = "/home/niñuel/bin/rust-analyzer".into();
        state.editor_workbench.language_status = "Rust · análisis local".into();
        state.ide_status = IdeStatusView {
            menu: Some(StatusMenu::Services),
            scope: document_scope(&state),
            workspace: Some(state.workspace.clone()),
            service_language: Some(CodeLanguage::Rust),
            ..Default::default()
        };
        state
    }

    #[test]
    fn services_form_is_a_scoped_popover_and_preserves_editing_focus_on_rebuild() {
        for width in [240.0, 340.0, 700.0] {
            let mut state = services_state();
            let prefix = document_scope(&state).unwrap().prefix();
            let input = format!("{prefix}:server_path");
            let viewport = Size::new(width, 460.0);
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(dock_root(&state, width));
            let initial = tree.layout(viewport);
            let anchor = initial
                .regions
                .iter()
                .find(|region| region.id == "ide_editor_language_service")
                .unwrap()
                .rect;
            let surface = initial
                .regions
                .iter()
                .find(|region| region.id == format!("popover_surface:{prefix}:services_dismiss"))
                .unwrap()
                .rect;
            assert!(surface.x >= 0.0 && surface.x + surface.width <= width);
            assert!(surface.y >= 0.0 && surface.y + surface.height <= anchor.y);
            assert!(surface.width <= 340.0);
            assert_eq!(tree.focused_id(), Some(input.as_str()));
            for suffix in ["server_path", "server_connect", "diagnostics"] {
                let target = initial
                    .regions
                    .iter()
                    .find(|region| region.id == format!("{prefix}:{suffix}"))
                    .unwrap()
                    .rect;
                assert!(
                    target.x >= surface.x && target.x + target.width <= surface.x + surface.width
                );
                assert!(
                    target.y >= surface.y && target.y + target.height <= surface.y + surface.height
                );
            }
            assert!(!initial.regions.iter().any(|region| matches!(
                region.id.as_str(),
                "editor_server_path" | "editor_settings"
            )));
            tree.handle_event(UiEvent::Key {
                key: openide_ui::Key::ArrowLeft,
                ctrl: false,
                shift: false,
            });
            let before = tree.input_cursor(&input).unwrap();
            let actions = tree.handle_event(UiEvent::Text("ñ".into()));
            assert!(actions
                .iter()
                .any(|action| matches!(action, UiAction::InputChanged { id, .. } if id == &input)));
            state.editor_workbench.server_path = tree.input_value(&input).unwrap().to_owned();
            let selection = tree.input_selection(&input).unwrap();
            assert_eq!(selection, (before + 1, before + 1));
            state.editor_workbench.language_status = "Rust · conectado".into();
            state.editor_workbench.language_ready = true;
            tree.set_root(dock_root(&state, width));
            let rebuilt = tree.layout(viewport);
            assert_eq!(tree.input_selection(&input), Some(selection));
            assert_eq!(
                tree.input_value(&input),
                Some(state.editor_workbench.server_path.as_str())
            );
            assert_eq!(
                rebuilt
                    .regions
                    .iter()
                    .find(|region| region.id == "ide_editor_language_service")
                    .unwrap()
                    .rect,
                anchor
            );
            tree.handle_event(UiEvent::Key {
                key: openide_ui::Key::Tab,
                ctrl: false,
                shift: false,
            });
            assert_eq!(
                tree.focused_id(),
                Some(format!("{prefix}:server_connect").as_str())
            );
            let actions = tree.handle_event(UiEvent::Key {
                key: openide_ui::Key::Escape,
                ctrl: false,
                shift: false,
            });
            assert!(actions.contains(&UiAction::Clicked {
                id: format!("{prefix}:services_dismiss")
            }));
            state.ide_status = IdeStatusView::default();
            tree.set_root(dock_root(&state, width));
            tree.layout(viewport);
            assert_eq!(tree.focused_id(), Some("ide_editor_language_service"));
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn services_report_real_state_without_connecting_plain_or_stale_languages() {
        for variant in ["ready", "connecting", "error", "plain", "changed"] {
            let mut state = services_state();
            let prefix = document_scope(&state).unwrap().prefix();
            match variant {
                "ready" => {
                    state.editor_workbench.language_ready = true;
                    state.editor_workbench.language_status = "Rust · conectado".into();
                }
                "connecting" => {
                    state.ide_status.service_connecting = true;
                    state.editor_workbench.language_status = "Conectando lenguaje…".into();
                }
                "error" => {
                    state.ide_status.service_error =
                        Some("La ruta contiene caracteres de control.".into())
                }
                "plain" => {
                    state.editor_workbench.language_override = Some(CodeLanguage::Plain);
                    state.ide_status.service_language = Some(CodeLanguage::Plain);
                }
                _ => state.editor_workbench.language_override = Some(CodeLanguage::Python),
            }
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(dock_root(&state, 320.0));
            let frame = tree.layout(Size::new(320.0, 460.0));
            let connect = frame.regions.iter().any(|region| {
                region.id == format!("{prefix}:server_connect") && region.kind == HitKind::Button
            });
            assert_eq!(connect, matches!(variant, "ready" | "error"));
            let visible = |expected: &str| {
                frame.commands.iter().any(|command| matches!(command, openide_ui::DrawCommand::Text { text, .. } if text == expected))
            };
            match variant {
                "ready" => assert!(visible("Reiniciar") && visible("Rust · conectado")),
                "connecting" => assert!(visible("Conectando…") && visible("Conectando lenguaje…")),
                "error" => assert!(frame.commands.iter().any(|command| matches!(command, openide_ui::DrawCommand::Text { text, color, .. } if text.contains("caracteres de control") && *color == theme::DESTRUCTIVE))),
                "plain" => { assert!(visible("Texto sin servidor de lenguaje.")); assert_eq!(tree.focused_id(), Some(format!("{prefix}:services_dismiss").as_str())); }
                _ => assert!(!frame.regions.iter().any(|region| region.id.starts_with("popover_surface:"))),
            }
            context.end_pass().textures_delta.clear();
        }
        let mut state = editor_state();
        state.ide_status = IdeStatusView::default();
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root(&state, 700.0));
        let frame = tree.layout(Size::new(700.0, 100.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "ide_editor_language_service"));
        context.end_pass().textures_delta.clear();
    }

    #[test]
    fn opening_services_from_overflow_starts_a_new_form_focus_scope() {
        let mut state = services_state();
        state.ide_status.menu = Some(StatusMenu::Overflow);
        let prefix = document_scope(&state).unwrap().prefix();
        let size = Size::new(240.0, 460.0);
        let (context, mut tree) =
            crate::view::ide_visual_tests::native_tree(dock_root(&state, size.width));
        let overflow = tree.layout(size);
        let open = format!("{prefix}:open:services");
        let target = overflow
            .regions
            .iter()
            .find(|region| region.id == open)
            .unwrap()
            .rect;
        let point = Point::new(
            target.x + target.width / 2.0,
            target.y + target.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert!(tree
            .handle_event(UiEvent::PointerUp(point))
            .contains(&UiAction::Clicked { id: open }));
        state.ide_status.menu = Some(StatusMenu::Services);
        tree.set_root(dock_root(&state, size.width));
        tree.layout(size);
        let input = format!("{prefix}:server_path");
        assert_eq!(tree.focused_id(), Some(input.as_str()));
        assert!(tree
            .handle_event(UiEvent::Text("x".into()))
            .iter()
            .any(|action| matches!(action, UiAction::InputChanged { id, .. } if id == &input)));
        let actions = tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::Escape,
            ctrl: false,
            shift: false,
        });
        assert!(actions.contains(&UiAction::Clicked {
            id: format!("{prefix}:services_dismiss")
        }));
        state.ide_status = IdeStatusView::default();
        tree.set_root(dock_root(&state, size.width));
        tree.layout(size);
        assert_eq!(tree.focused_id(), Some("ide_editor_language_service"));
        context.end_pass().textures_delta.clear();
    }

    #[test]
    #[ignore = "exports native scoped language-service form popovers"]
    fn export_language_services_visual_review() {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/language-services-visual-qa");
        std::fs::create_dir_all(&directory).unwrap();
        for width in [240.0, 340.0, 700.0] {
            for variant in ["local", "ready", "connecting", "error", "plain"] {
                let mut state = services_state();
                match variant {
                    "ready" => {
                        state.editor_workbench.language_ready = true;
                        state.editor_workbench.language_status = "Rust · conectado".into();
                    }
                    "connecting" => {
                        state.ide_status.service_connecting = true;
                        state.editor_workbench.language_status = "Conectando lenguaje…".into();
                    }
                    "error" => {
                        state.ide_status.service_error = Some(
                            "No se pudo iniciar el servidor. Revisá la ruta del ejecutable.".into(),
                        );
                        state.editor_workbench.language_status = "Servidor no disponible".into();
                    }
                    "plain" => {
                        state.editor_workbench.language_override = Some(CodeLanguage::Plain);
                        state.ide_status.service_language = Some(CodeLanguage::Plain);
                    }
                    _ => {}
                }
                let size = Size::new(width, 460.0);
                let (context, mut tree) =
                    crate::view::ide_visual_tests::native_tree(dock_root(&state, width));
                let frame = tree.layout(size);
                let path = directory.join(format!("services-{variant}-{width:.0}.svg"));
                crate::settings_ui::visual_qa::write_native_frame_svg(
                    &frame,
                    &path,
                    width,
                    size.height,
                )
                .unwrap();
                let svg = resvg::usvg::Tree::from_data(
                    &std::fs::read(&path).unwrap(),
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixels =
                    resvg::tiny_skia::Pixmap::new(width as u32, size.height as u32).unwrap();
                resvg::render(
                    &svg,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixels.as_mut(),
                );
                pixels.save_png(path.with_extension("png")).unwrap();
                context.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    #[ignore = "export native dock footer at narrow and wide widths"]
    fn export_document_footer_visual_review() {
        let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/document-footer-visual-qa");
        std::fs::create_dir_all(&output).unwrap();
        let mut state = editor_state();
        state.ide.mode = crate::ide_workspace::WindowMode::Agents;
        state.editor_workbench.language_ready = true;
        state.editor_workbench.language_status = "rust-analyzer conectado".into();
        for width in [120.0, 240.0, 320.0, 520.0, 900.0] {
            for open in [false, true] {
                let height = if open { 380.0 } else { 120.0 };
                state.ide_status = if open {
                    IdeStatusView {
                        menu: Some(StatusMenu::Overflow),
                        scope: document_scope(&state),
                        workspace: Some(state.workspace.clone()),
                        ..Default::default()
                    }
                } else {
                    IdeStatusView::default()
                };
                let root = Element::column(vec![
                    Element::text("fn main() {\n    println!(\"OpenIDE\");\n}")
                        .font_family(FontFamily::Monospace)
                        .font_size(14.0)
                        .foreground(theme::FOREGROUND)
                        .padding(8.0)
                        .flex(1.0),
                    document_footer(&state, width),
                ])
                .background(theme::BACKGROUND);
                let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(root);
                let frame = tree.layout(Size::new(width, height));
                let path = output.join(format!(
                    "document-footer-{width}-{}.svg",
                    if open { "menu" } else { "bar" }
                ));
                crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
                    .unwrap();
                let svg = std::fs::read(&path).unwrap();
                let tree = resvg::usvg::Tree::from_data(
                    &svg,
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixmap =
                    resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                pixmap.save_png(path.with_extension("png")).unwrap();
                ctx.end_pass().textures_delta.clear();
            }
        }
    }
}
