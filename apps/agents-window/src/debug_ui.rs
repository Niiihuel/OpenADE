//! Native debugger presentation. Every actionable ID is scoped to the host's
//! workspace/session epoch; this module never sends DAP or starts a process.
use crate::theme;
use openide_ui::{
    AlertDialog, Button, ButtonSize, ButtonVariant, Collapse, Component, ContextMenu,
    ContextMenuEntry, ContextMenuItem, ControlSize, DropdownOption, Element, Empty, FontFamily,
    Icon, IconButton, ListRow, Notice, NoticeSizing, PanelHeader, PopoverPlacement, ScrollArea,
    SectionHeader, Select, Separator, SidebarPanel, Spinner, Text, TextRole, Tooltip,
    TooltipPlacement, TooltipSpec, TreeGuides, TreeRow,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DebugStatus {
    #[default]
    Unconfigured,
    Idle,
    Starting,
    Running,
    Stopped,
    Stopping,
    Ended,
    Failed,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct DebugControls {
    pub start: bool,
    pub resume: bool,
    pub pause: bool,
    pub step_over: bool,
    pub step_into: bool,
    pub step_out: bool,
    pub restart: bool,
    pub stop: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct DebugThreadView {
    pub id: i64,
    pub name: String,
    pub selected: bool,
    pub stopped: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct DebugFrameView {
    pub id: i64,
    pub name: String,
    pub path: Option<String>,
    /// One-based DAP line, displayed as received. No navigation is inferred here.
    pub line: Option<u64>,
    pub selected: bool,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct DebugVariableView {
    pub key: String,
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// The value differs from the one this variable had at the previous stop.
    pub changed: bool,
    /// Not a variable: a muted line under its siblings, such as a page limit.
    pub note: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct DebugOutputView {
    pub category: String,
    pub text: String,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DebugBreakpointView {
    pub path: String,
    /// One-based source line. Dirty marks remain visible but cannot navigate
    /// or be sent to an adapter until their document is saved.
    pub line: u32,
    pub dirty: bool,
    /// A disabled mark stays in the editor and is not sent to the adapter.
    pub enabled: bool,
    /// The adapter's answer for this line in the running session, if any.
    pub verified: Option<bool>,
    pub message: Option<String>,
}
/// Which sections are open. Saved with the IDE layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct DebugSections {
    pub variables: bool,
    pub stack: bool,
    pub breakpoints: bool,
}
impl Default for DebugSections {
    fn default() -> Self {
        Self {
            variables: true,
            stack: true,
            breakpoints: true,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub(crate) struct DebugView {
    /// Opaque host-owned workspace + session epoch, revalidated on every action.
    pub scope: String,
    pub status: DebugStatus,
    pub configuration: String,
    pub configurations: Arc<[String]>,
    pub selected_configuration: usize,
    pub configuration_menu: bool,
    pub adapter: Option<String>,
    /// A note or a failure in the adapter's words. Never the stop reason.
    pub message: Option<String>,
    /// Why execution paused, while it is paused.
    pub stop_reason: Option<String>,
    pub controls: DebugControls,
    pub threads: Arc<[DebugThreadView]>,
    pub frames: Arc<[DebugFrameView]>,
    pub variables: Arc<[DebugVariableView]>,
    pub output: Arc<[DebugOutputView]>,
    pub breakpoints: Arc<[DebugBreakpointView]>,
    pub sections: DebugSections,
    /// "Quitar todos" asked and not yet answered.
    pub confirm_clear_breakpoints: bool,
}

pub(crate) fn action(scope: &str, operation: &str) -> String {
    format!(
        "debug_control:{}",
        serde_json::to_string(&(scope, operation)).expect("debug action tuple")
    )
}
pub(crate) fn row_action(scope: &str, kind: &str, key: impl serde::Serialize) -> String {
    format!(
        "debug_{kind}:{}",
        serde_json::to_string(&(scope, key)).expect("debug row tuple")
    )
}

/// One slot of the inspector: a 22 px row and the 2 px under it.
const SLOT: f32 = 24.0;
const ROW: f32 = 22.0;
const START_HEIGHT: f32 = 28.0;
const RAIL: f32 = 10.0;
/// Where every row's copy starts, under the title of its section.
const COPY_INSET: f32 = 26.0;
const MAX_OUTPUT: usize = 512;
const MAX_TEXT: usize = 4096;
/// The host asks the adapter for this many frames and children at a time.
pub(crate) const PAGE: usize = 100;

fn bounded(text: &str) -> String {
    if text.len() <= MAX_TEXT {
        return text.into();
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncado)", &text[..end])
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

/// `crate::module::Type::method` reads as `Type::method` in a narrow row. A
/// name with generics or a signature is left whole rather than cut wrongly.
fn short_function(name: &str) -> &str {
    if name.contains(['<', '(', '{', ' ']) {
        return name;
    }
    let mut cuts = name.rmatch_indices("::").map(|(index, _)| index + 2);
    cuts.nth(1).map_or(name, |start| &name[start..])
}

fn location(path: Option<&str>, line: Option<u64>, short: bool) -> Option<String> {
    let path = path?;
    let path = if short { basename(path) } else { path };
    Some(match line {
        Some(line) => format!("{path}:{line}"),
        None => path.into(),
    })
}

fn session_active(status: DebugStatus) -> bool {
    matches!(
        status,
        DebugStatus::Starting | DebugStatus::Running | DebugStatus::Stopped | DebugStatus::Stopping
    )
}

fn sidebar_actions() -> openide_ui::ComponentPalette {
    theme::action_palette(theme::SIDEBAR, theme::FOREGROUND)
}

/// A tool repeated on every row: muted until the pointer reaches it.
fn row_tool(id: &str, icon: Icon, hint: &str) -> Element {
    IconButton::new(
        id,
        icon,
        theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
    )
    .variant(ButtonVariant::Ghost)
    .button_size(ButtonSize::Mini)
    .hover_ink(theme::FOREGROUND)
    .tooltip_with(TooltipSpec::new(hint).placement(TooltipPlacement::Below))
    .render()
}

/// A debug action: its id comes from the scope and its shortcut from the
/// operation, so every debug surface agrees on both.
fn surface_tool(
    state: &DebugView,
    op: &str,
    icon: Icon,
    hint: &str,
    enabled: bool,
) -> Element {
    let shortcut = match op {
        "start" | "resume" => "F5",
        "step_over" => "F10",
        "step_into" => "F11",
        "step_out" => "Shift+F11",
        "restart" => "Ctrl+Shift+F5",
        "stop" => "Shift+F5",
        _ => "",
    };
    IconButton::new(action(&state.scope, op), icon, sidebar_actions())
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .enabled(enabled && !state.scope.is_empty())
        .tooltip_with(
            TooltipSpec::new(hint)
                .shortcut(shortcut)
                .placement(TooltipPlacement::Below),
        )
        .render()
}

/// The IDE's debug sidebar: what starts or drives the session stays pinned
/// under the header, and one virtualized list holds every section, so a deep
/// stack or a large scope costs only the rows in view.
pub(crate) fn build_sidebar(state: &DebugView, width: f32, height: f32) -> Element {
    use crate::sidebar_chrome as chrome;
    let width = width.max(0.0);
    let inner = (width - chrome::INSET * 2.0).max(0.0);
    let active = session_active(state.status);
    let tool = |op, icon, hint, enabled| {
        surface_tool(state, op, icon, hint, enabled)
    };
    let header = PanelHeader::new("Depuración", theme::sidebar_palette())
        .trailing(vec![
            tool("console", Icon::Terminal, "Abrir consola de depuración", true),
            tool("reload", Icon::Refresh, "Recargar configuraciones", !active),
            tool("hide", Icon::Close, "Ocultar depuración", true),
        ])
        .width(inner)
        .sizing(crate::ui_sizing::SIDEBAR_HEADER)
        .render();
    let configured = !state.configurations.is_empty();
    // Each block carries its own space below: a closed slot leaves none.
    let block = |content: Element| {
        Element::column(vec![
            content,
            Element::spacer().size(Some(inner), Some(chrome::GAP)),
        ])
        .gap(0.0)
    };
    let rows: Arc<[Row]> = rows(state).into();
    let captured = state.clone();
    // The scrollbar's rail is kept clear from the start, so the tools at the
    // end of a row neither jump nor sit under it once the list overflows.
    let row_width = (inner - RAIL).max(0.0);
    let list = Element::uniform_list(
        format!("debug_sidebar:{}", state.scope),
        rows.len(),
        SLOT,
        move |index| row(&captured, &rows[index], row_width),
    )
    .flex(1.0);
    let body = Element::column(vec![
        Collapse::new(
            "debug_start",
            (!active && configured).then(|| block(start_row(state, inner))),
        )
        .expand()
        .render(),
        Collapse::new(
            "debug_session",
            active.then(|| block(session(state, inner))),
        )
        .expand()
        .render(),
        Collapse::new("debug_note", note(state, inner).map(block))
            .expand()
            .render(),
        list,
    ])
    .gap(0.0)
    .flex(1.0);
    SidebarPanel::new(vec![header, body], theme::sidebar_palette())
        .size(width, height.max(0.0))
        .render()
}

/// Idle: which program, a primary start and the way to its configuration.
fn start_row(state: &DebugView, width: f32) -> Element {
    let select_width = (width - START_HEIGHT * 2.0 - 8.0).max(0.0);
    let select = Select::new(
        action(&state.scope, "configurations"),
        bounded(&state.configuration),
        state
            .configurations
            .iter()
            .take(16)
            .enumerate()
            .map(|(index, name)| {
                DropdownOption::new(
                    row_action(&state.scope, "configuration", index),
                    bounded(name),
                )
                .selected(index == state.selected_configuration)
            })
            .collect(),
        state.configuration_menu,
        action(&state.scope, "dismiss_configurations"),
        theme::component_palette(),
    )
    .menu_palette(theme::menu_palette())
    .placement(PopoverPlacement::BottomStart)
    .width(select_width)
    .menu_width(select_width.max(200.0))
    .enabled(!state.scope.is_empty())
    .render();
    let start = IconButton::new(
        action(&state.scope, "start"),
        Icon::PlayFill,
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
    )
    .variant(ButtonVariant::Solid)
    .size(ControlSize::Compact)
    .enabled(state.controls.start && !state.scope.is_empty())
    .tooltip_with(
        TooltipSpec::new("Iniciar depuración")
            .description(state.adapter.as_deref().map_or_else(String::new, bounded))
            .shortcut("F5")
            .placement(TooltipPlacement::Below),
    )
    .render();
    let configure = IconButton::new(
        action(&state.scope, "configure"),
        Icon::Settings,
        sidebar_actions(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Compact)
    .enabled(!state.scope.is_empty())
    .tooltip_with(
        TooltipSpec::new("Editar configuraciones")
            .description(crate::debug_config::CONFIG_PATH)
            .placement(TooltipPlacement::Below),
    )
    .render();
    Element::row(vec![select, start, configure])
        .gap(4.0)
        .size(Some(width), Some(START_HEIGHT))
}

/// A session: the controls that follow its state over the line that names it.
fn session(state: &DebugView, width: f32) -> Element {
    let control = |op, icon, hint, enabled| {
        surface_tool(state, op, icon, hint, enabled)
    };
    let stopped = state.status == DebugStatus::Stopped;
    let controls = &state.controls;
    let strip = Element::row(vec![
        if stopped {
            control("resume", Icon::Play, "Continuar", controls.resume)
        } else {
            control(
                "pause",
                Icon::Pause,
                "Pausar",
                state.status == DebugStatus::Running && controls.pause,
            )
        },
        control(
            "step_over",
            crate::icons::STEP_OVER,
            "Paso por encima",
            stopped && controls.step_over,
        ),
        control(
            "step_into",
            crate::icons::STEP_INTO,
            "Entrar en la llamada",
            stopped && controls.step_into,
        ),
        control(
            "step_out",
            crate::icons::STEP_OUT,
            "Salir de la llamada",
            stopped && controls.step_out,
        ),
        Separator::vertical(theme::component_palette())
            .ink(theme::TREE_GUIDE)
            .length(14.0)
            .margin(3.0)
            .centered_in(ControlSize::Dense.height())
            .render(),
        control(
            "restart",
            Icon::Refresh,
            "Reiniciar",
            state.status != DebugStatus::Stopping && controls.restart,
        ),
        control(
            "stop",
            Icon::Stop,
            "Detener",
            state.status != DebugStatus::Stopping && controls.stop,
        ),
    ])
    .gap(2.0)
    .size(Some(width), Some(ControlSize::Dense.height()));

    let frame = state.frames.iter().find(|frame| frame.selected);
    let place = frame.and_then(|frame| location(frame.path.as_deref(), frame.line, true));
    let (title, ink, icon) = match state.status {
        DebugStatus::Starting => ("Iniciando…".to_owned(), theme::TOOL_RESULT, None),
        DebugStatus::Running => (
            "En ejecución".to_owned(),
            theme::DIFF_ADDED,
            Some(Icon::PlayFill),
        ),
        DebugStatus::Stopped => (
            place
                .as_ref()
                .map_or_else(|| "En pausa".to_owned(), |place| format!("En pausa en {place}")),
            theme::CODE_NUMBER,
            Some(Icon::Pause),
        ),
        _ => ("Finalizando…".to_owned(), theme::TOOL_RESULT, None),
    };
    let thread = state
        .threads
        .iter()
        .find(|thread| thread.selected)
        .filter(|_| state.threads.len() > 1);
    let subtitle = [
        Some(state.configuration.as_str()),
        thread.map(|thread| thread.name.as_str()),
        state.stop_reason.as_deref().filter(|_| stopped),
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let reveal = frame
        .filter(|frame| stopped && frame.path.is_some())
        .map(|frame| row_action(&state.scope, "reveal", frame.id));
    let leading = match icon {
        Some(icon) => Element::icon(icon)
            .foreground(ink)
            .size(Some(12.0), Some(12.0))
            .centered(),
        None => Spinner::new(format!("debug_spinner:{}", state.scope), ink)
            .size(12.0)
            .render()
            .centered(),
    };
    let line = ListRow::new(
        reveal
            .clone()
            .unwrap_or_else(|| format!("debug_session_line:{}", state.scope)),
        title,
        sidebar_actions(),
    )
    .leading(leading)
    .title_ink(ink)
    .subtitle(bounded(&subtitle))
    .enabled(reveal.is_some())
    .sizing(crate::ui_sizing::DEBUG_SESSION)
    .width(width);
    let line = match (&reveal, frame) {
        (Some(_), Some(frame)) => line.tooltip_with(
            TooltipSpec::new("Mostrar la línea en pausa")
                .description(location(frame.path.as_deref(), frame.line, false).unwrap_or_default())
                .placement(TooltipPlacement::Below),
        ),
        _ => line,
    };
    Element::column(vec![strip, line.render()]).gap(2.0)
}

/// What the adapter or the host has to say, where it can be read whole.
fn note(state: &DebugView, width: f32) -> Option<Element> {
    let message = state
        .message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty());
    let palette = theme::sidebar_palette();
    if state.status == DebugStatus::Unconfigured && state.configurations.is_empty() {
        return Some(
            Empty::new(
                "Sin configuración",
                "Elegí el programa y su adaptador en .openide/debug.json. Crearla no ejecuta nada.",
                palette,
            )
            .icon(Icon::Play)
            .action(
                action(&state.scope, "configure"),
                "Crear configuración",
                ButtonVariant::Outline,
            )
            .compact()
            .width(width)
            .render(),
        );
    }
    if state.status == DebugStatus::Failed {
        let adapter = state.adapter.as_deref().unwrap_or("el adaptador");
        let detail = message.unwrap_or("El adaptador no informó el motivo.");
        let (title, description) = if state.configurations.is_empty() {
            ("Configuración inválida".to_owned(), bounded(detail))
        } else if detail.starts_with("Cannot start debug adapter") {
            (
                format!("No se pudo iniciar «{adapter}»"),
                format!(
                    "Instalalo y comprobá que esté en el PATH, o corregí adapter.command en {}.\n{}",
                    crate::debug_config::CONFIG_PATH,
                    bounded(detail)
                ),
            )
        } else {
            ("La sesión no pudo continuar".to_owned(), bounded(detail))
        };
        let mut notice = Notice::new(title, palette)
            .icon(Icon::ErrorCircle)
            .ink(theme::DESTRUCTIVE)
            .description(description)
            .sizing(NoticeSizing::COMPACT)
            .width(width);
        if state.configurations.is_empty() {
            notice = notice.action(
                Button::new(action(&state.scope, "configure"), "Abrir debug.json", sidebar_actions())
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Compact)
                    .enabled(!state.scope.is_empty())
                    .render(),
            );
        }
        // A long adapter message scrolls in place instead of pushing the
        // sections out of the panel.
        return Some(
            ScrollArea::new(format!("debug_note:{}", state.scope), notice.render())
                .render()
                .max_height(168.0)
                .size(Some(width), None),
        );
    }
    let active = session_active(state.status);
    let (text, ink) = match (message, state.status) {
        // During a session only a failed request speaks here.
        (Some(message), _) if active => (bounded(message), theme::WARNING),
        (None, _) if active => return None,
        (Some(message), _) => (bounded(message), theme::FOREGROUND_SUBTLE),
        (None, DebugStatus::Ended) => ("Sesión finalizada".to_owned(), theme::FOREGROUND_SUBTLE),
        (None, _) => (
            match &state.adapter {
                Some(adapter) => format!("Listo · {}", bounded(adapter)),
                None => "Listo".to_owned(),
            },
            theme::FOREGROUND_SUBTLE,
        ),
    };
    Some(
        Element::row(vec![
            Element::spacer().size(Some(4.0), Some(16.0)),
            Text::new(text, palette)
                .role(TextRole::Small)
                .ink(ink)
                .truncate()
                .tooltip(format!("debug_note:{}", state.scope))
                .render()
                .size(Some((width - 8.0).max(0.0)), Some(16.0)),
        ])
        .gap(0.0),
    )
}

#[derive(Clone, Copy)]
enum Row {
    Section(&'static str),
    Hint(&'static str),
    Variable(usize),
    Thread(usize),
    Frame(usize),
    Breakpoint(usize),
}

fn rows(state: &DebugView) -> Vec<Row> {
    let mut rows = Vec::new();
    let stopped = state.status == DebugStatus::Stopped;
    if !state.configurations.is_empty() || session_active(state.status) {
        rows.push(Row::Section("variables"));
        if state.sections.variables {
            rows.extend((0..state.variables.len()).map(Row::Variable));
            if state.variables.is_empty() {
                rows.push(Row::Hint(if stopped {
                    "Cargando variables…"
                } else {
                    "Disponible al pausar"
                }));
            }
        }
        rows.push(Row::Section("stack"));
        if state.sections.stack {
            if state.threads.len() > 1 && stopped {
                for (index, thread) in state.threads.iter().enumerate() {
                    rows.push(Row::Thread(index));
                    if thread.selected {
                        rows.extend((0..state.frames.len()).map(Row::Frame));
                    }
                }
            } else {
                rows.extend((0..state.frames.len()).map(Row::Frame));
            }
            if state.frames.is_empty() {
                rows.push(Row::Hint(if stopped {
                    "Cargando llamadas…"
                } else {
                    "Disponible al pausar"
                }));
            } else if state.frames.len() >= PAGE {
                rows.push(Row::Hint("Se muestran las primeras 100 llamadas"));
            }
        }
    }
    rows.push(Row::Section("breakpoints"));
    if state.sections.breakpoints {
        rows.extend((0..state.breakpoints.len()).map(Row::Breakpoint));
        if state.breakpoints.is_empty() {
            rows.push(Row::Hint("Agregá uno con F9 en el editor"));
        }
    }
    rows
}

fn row(state: &DebugView, row: &Row, width: f32) -> Element {
    let content = match *row {
        Row::Section(key) => section(state, key, width),
        Row::Hint(text) => hint(text, COPY_INSET, width),
        Row::Variable(index) => variable(state, &state.variables[index], width),
        Row::Thread(index) => thread(state, &state.threads[index], width),
        Row::Frame(index) => frame(state, &state.frames[index], width),
        Row::Breakpoint(index) => breakpoint(state, &state.breakpoints[index], width),
    };
    Element::column(vec![content.size(Some(width), Some(ROW))])
        .gap(0.0)
        .size(Some(width), Some(SLOT))
}

fn hint(text: &str, indent: f32, width: f32) -> Element {
    Element::row(vec![
        Element::spacer().size(Some(indent), Some(ROW)),
        Text::new(text, theme::sidebar_palette())
            .role(TextRole::Small)
            .muted()
            .truncate()
            .render()
            .centered_y()
            .flex(1.0),
    ])
    .gap(0.0)
    .size(Some(width), Some(ROW))
}

fn section(state: &DebugView, key: &'static str, width: f32) -> Element {
    let (title, count, open) = match key {
        "variables" => (
            "Variables",
            state.variables.iter().filter(|row| row.depth > 0 && !row.note).count(),
            state.sections.variables,
        ),
        "stack" => (
            "Pila de llamadas",
            state.frames.len(),
            state.sections.stack,
        ),
        _ => (
            // Beside its two tools the full title needs more than a narrow
            // sidebar has; the short one says the same without being cut.
            if width < 236.0 {
                "Puntos de pausa"
            } else {
                "Puntos de interrupción"
            },
            state.breakpoints.len(),
            state.sections.breakpoints,
        ),
    };
    let mut header = SectionHeader::new(title, sidebar_actions())
        .disclosure(Some(open))
        .action(row_action(&state.scope, "section", key))
        .truncate(true)
        .sizing(crate::ui_sizing::DEBUG_SECTION);
    if count > 0 {
        header = header.detail(count.to_string());
    }
    let mut children = vec![header.render().min_size(0.0, 0.0).flex(1.0)];
    if key == "breakpoints" && !state.breakpoints.is_empty() {
        let any_enabled = state.breakpoints.iter().any(|mark| mark.enabled);
        children.push(row_tool(
            &action(&state.scope, "toggle_breakpoints"),
            Icon::CheckCircle,
            if any_enabled {
                "Desactivar todos"
            } else {
                "Activar todos"
            },
        ));
        children.push(row_tool(
            &action(&state.scope, "clear_breakpoints"),
            Icon::Trash,
            "Quitar todos…",
        ));
    }
    Element::row(children).gap(0.0).size(Some(width), Some(ROW))
}

fn variable(state: &DebugView, variable: &DebugVariableView, width: f32) -> Element {
    // The tree's gutter is 4 plus 12 a level, then the 16 px disclosure column.
    if variable.note {
        return hint(&variable.name, COPY_INSET + variable.depth as f32 * 12.0, width);
    }
    let palette = sidebar_actions();
    let id = row_action(&state.scope, "expand", &variable.key);
    let scope = variable.depth == 0;
    let family = if scope {
        FontFamily::Sans
    } else {
        FontFamily::Monospace
    };
    let (value, ink) = if variable.loading {
        ("Cargando…".to_owned(), theme::FOREGROUND_SUBTLE)
    } else if variable.error.is_some() {
        ("No disponible".to_owned(), theme::DESTRUCTIVE)
    } else if variable.changed {
        (bounded(&variable.value), theme::WARNING)
    } else {
        (bounded(&variable.value), theme::TOOL_RESULT)
    };
    let budget = (width - 34.0 - variable.depth as f32 * 12.0).max(0.0);
    let title = Element::row(vec![
        Element::truncated_text(bounded(&variable.name))
            .font_family(family)
            .font_size(if scope { 11.0 } else { 12.0 })
            .foreground(if scope {
                theme::TOOL_RESULT
            } else {
                theme::FOREGROUND
            })
            .max_width(if value.is_empty() { budget } else { budget * 0.5 })
            .centered_y(),
        Element::truncated_text(value.replace(['\r', '\n'], " "))
            .font_family(family)
            .font_size(12.0)
            .foreground(ink)
            .min_size(0.0, 0.0)
            .flex(1.0)
            .centered_y(),
    ])
    .gap(8.0)
    .size(None, Some(ROW));
    let chevron = if variable.has_children {
        Element::icon(if variable.expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        })
        .foreground(theme::TOOL_RESULT)
        .size(Some(12.0), Some(12.0))
        .centered()
    } else {
        Element::spacer().size(Some(12.0), Some(12.0))
    };
    let expandable = variable.has_children && !variable.loading;
    let target = id.clone();
    let action: Box<dyn FnOnce(Element) -> Element> = if expandable {
        Box::new(move |content| Element::clickable(target, content))
    } else {
        Box::new(|content| content)
    };
    let guides = TreeGuides {
        depth: variable.depth,
        ancestor_continuations: vec![false; variable.depth.saturating_sub(1)],
        next_sibling: false,
    };
    // Depth is read from the indent alone: guides between 22 px rows would
    // be cut by every gap.
    let row = TreeRow::new(action, chevron, title, guides, palette)
        .width(width)
        .height(ROW)
        .max_guide_depth(8)
        .guide_color(openide_ui::Color::TRANSPARENT)
        .render();
    let detail = variable.error.as_ref().map_or_else(
        || {
            format!(
                "{}{}",
                variable
                    .type_name
                    .as_ref()
                    .map_or_else(String::new, |kind| format!("{}\n", bounded(kind))),
                bounded(&variable.value)
            )
        },
        |error| bounded(error),
    );
    let row = Tooltip::new(row, &id, bounded(&variable.name), theme::component_palette())
        .description(detail)
        .placement(TooltipPlacement::Right)
        .render();
    if scope {
        return row;
    }
    let copy = |what: &str, label: &str| {
        ContextMenuEntry::Item(ContextMenuItem::new(
            row_action(&state.scope, "copy", (&variable.key, what)),
            label,
        ))
    };
    ContextMenu::new(
        format!("debug_variable_menu:{}", variable.key),
        row,
        vec![copy("value", "Copiar valor"), copy("name", "Copiar nombre")],
        theme::menu_palette(),
    )
    .render()
}

fn thread(state: &DebugView, thread: &DebugThreadView, width: f32) -> Element {
    // The selected thread is the one whose frames follow it.
    ListRow::new(
        row_action(&state.scope, "thread", thread.id),
        bounded(&thread.name),
        sidebar_actions(),
    )
    .title_ink(if thread.selected {
        theme::FOREGROUND
    } else {
        theme::TOOL_RESULT
    })
    .meta(if thread.stopped {
        "En pausa"
    } else {
        "En ejecución"
    })
    .sizing(crate::ui_sizing::DEBUG_ROW)
    .width(width)
    .render()
}

/// A row that stays marked while it is the selection.
fn selectable(selected: bool) -> openide_ui::ComponentPalette {
    let palette = sidebar_actions();
    openide_ui::ComponentPalette {
        surface: if selected {
            theme::SELECTED
        } else {
            palette.surface
        },
        hover: if selected {
            theme::SELECTED
        } else {
            palette.hover
        },
        ..palette
    }
}

fn frame(state: &DebugView, frame: &DebugFrameView, width: f32) -> Element {
    let id = row_action(&state.scope, "frame", frame.id);
    let name = bounded(&frame.name);
    // The marker of the frame being inspected; the others keep its column.
    let marker = if frame.selected {
        Element::icon(Icon::ArrowRight)
            .foreground(theme::CODE_NUMBER)
            .size(Some(12.0), Some(12.0))
            .centered()
    } else {
        Element::spacer()
    };
    let mut row = ListRow::new(&id, short_function(&name), selectable(frame.selected))
        .leading(marker)
        .sizing(crate::ui_sizing::DEBUG_ROW)
        .width(width)
        .tooltip_with(
            TooltipSpec::new(name.clone())
                .description(
                    location(frame.path.as_deref(), frame.line, false)
                        .unwrap_or_else(|| "Fuente no disponible".into()),
                )
                .placement(TooltipPlacement::Right),
        );
    match location(frame.path.as_deref(), frame.line, true) {
        Some(place) => row = row.meta(place),
        // A frame without source can be inspected but has no line to show.
        None => row = row.title_ink(theme::FOREGROUND_SUBTLE),
    }
    if frame.selected {
        row = row.filled();
    }
    row.render()
}

fn breakpoint(state: &DebugView, mark: &DebugBreakpointView, width: f32) -> Element {
    let key = (&mark.path, mark.line);
    let toggle = row_action(&state.scope, "breakpoint_toggle", key);
    let remove = row_action(&state.scope, "breakpoint_remove", key);
    let full = format!("{}:{}", mark.path, mark.line);
    let live = mark.enabled && !mark.dirty;
    let (badge, ink, why) = if mark.dirty {
        (
            "sin guardar",
            theme::FOREGROUND_SUBTLE,
            "Guardá el archivo para activar este punto de interrupción",
        )
    } else if !mark.enabled {
        ("", theme::FOREGROUND_SUBTLE, "Desactivado")
    } else if mark.verified == Some(false) {
        (
            "sin verificar",
            theme::WARNING,
            "El adaptador no aceptó esta línea",
        )
    } else {
        ("", theme::FOREGROUND, "Ir al punto de interrupción")
    };
    // limn has no checkbox: a 14 px box that holds a check while enabled.
    let box_ink = if live {
        theme::TOOL_RESULT
    } else {
        theme::FOREGROUND_SUBTLE
    };
    let check = Element::panel(if mark.enabled {
        Element::icon(Icon::Check)
            .foreground(box_ink)
            .size(Some(12.0), Some(12.0))
            .centered()
    } else {
        Element::spacer()
    })
    .border(box_ink, 1.0)
    .radius(3.0)
    .size(Some(14.0), Some(14.0))
    .centered();
    let check = Tooltip::new(
        sidebar_actions()
            .interactive_surface(Element::clickable(&toggle, check))
            .radius(4.0)
            .size(Some(COPY_INSET - 3.0), Some(ROW)),
        &toggle,
        if mark.enabled { "Desactivar" } else { "Activar" },
        theme::component_palette(),
    )
    .placement(TooltipPlacement::Below)
    .render();
    let mut row = ListRow::new(
        row_action(&state.scope, "breakpoint", key),
        format!("{}:{}", basename(&mark.path), mark.line),
        sidebar_actions(),
    )
    .title_ink(ink)
    .enabled(!mark.dirty)
    .sizing(crate::ui_sizing::DEBUG_ROW)
    .trailing_action(
        &remove,
        row_tool(&remove, Icon::Close, "Quitar punto de interrupción"),
    )
    .tooltip_with(
        TooltipSpec::new(full)
            .description(mark.message.as_deref().map_or_else(|| why.to_owned(), bounded))
            .placement(TooltipPlacement::Below),
    );
    if !badge.is_empty() {
        row = row.meta(badge);
    }
    Element::row(vec![check, row.render().min_size(0.0, 0.0).flex(1.0)])
        .gap(0.0)
        .size(Some(width), Some(ROW))
}

/// "Quitar todos" asks first: the marks of every file go at once.
pub(crate) fn clear_breakpoints_dialog(state: &DebugView, base: Element, width: f32) -> Element {
    if !state.confirm_clear_breakpoints {
        return base;
    }
    let count = state.breakpoints.len();
    AlertDialog::new(
        base,
        true,
        "¿Quitar todos los puntos de interrupción?",
        if count == 1 {
            "Se quitará 1 punto de interrupción de este proyecto.".to_owned()
        } else {
            format!("Se quitarán {count} puntos de interrupción de este proyecto.")
        },
        action(&state.scope, "clear_breakpoints_confirm"),
        action(&state.scope, "clear_breakpoints_cancel"),
        theme::component_palette(),
    )
    .width((width - 24.0).min(420.0))
    .confirm_label("Quitar todos")
    .cancel_label("Cancelar")
    .destructive(theme::DESTRUCTIVE)
    .render()
}

/// Read-only DAP output. A virtual row represents one physical output line;
/// no evaluation or terminal input is implied by this console presentation.
pub(crate) fn build_console(state: &DebugView, width: f32, height: f32) -> Element {
    const MAX_LINES: usize = 4096;
    let mut lines = std::collections::VecDeque::new();
    for output in state.output.iter().rev().take(MAX_OUTPUT) {
        for line in output.text.lines().rev() {
            if lines.len() == MAX_LINES {
                break;
            }
            lines.push_front((output.category.clone(), bounded(line)));
        }
        if lines.len() == MAX_LINES {
            break;
        }
    }
    if lines.is_empty() {
        return Element::text("La salida del adaptador aparecerá aquí al iniciar la depuración.")
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::FOREGROUND_SUBTLE)
            .padding(8.0)
            .size(Some(width), Some(height));
    }
    let lines: Arc<[(String, String)]> = lines.into_iter().collect::<Vec<_>>().into();
    let scope = state.scope.clone();
    Element::uniform_list(
        format!("debug_console:{scope}"),
        lines.len(),
        20.0,
        move |index| {
            let (category, text) = &lines[index];
            Tooltip::new(
                Element::faded_text(text, 16.0)
                    .font_family(FontFamily::Monospace)
                    .font_size(12.0)
                    .foreground(if category == "stderr" {
                        theme::DESTRUCTIVE
                    } else {
                        theme::TOOL_RESULT
                    })
                    .size(Some((width - 16.0).max(0.0)), Some(20.0)),
                format!("debug_console_line:{scope}:{index}"),
                if category.is_empty() {
                    "Salida"
                } else {
                    category.as_str()
                },
                theme::component_palette(),
            )
            .description(text)
            .render()
        },
    )
    .padding(8.0)
    .background(theme::BACKGROUND)
    .size(Some(width), Some(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    use openide_ui::{DrawCommand, HitKind, Point, Size, UiAction, UiEvent, UiTree};

    const OPERATIONS: [&str; 8] = [
        "start",
        "resume",
        "pause",
        "step_over",
        "step_into",
        "step_out",
        "restart",
        "stop",
    ];

    fn fixture() -> DebugView {
        DebugView {
            scope: "workspace:fixture/session:7".into(),
            status: DebugStatus::Stopped,
            configuration: "Python · programa de prueba".into(),
            configurations: vec![
                "Python · programa de prueba".into(),
                "Python · pruebas".into(),
            ]
            .into(),
            adapter: Some("debugpy".into()),
            controls: DebugControls {
                start: true,
                resume: true,
                pause: true,
                step_over: true,
                step_into: true,
                step_out: true,
                restart: true,
                stop: true,
            },
            frames: vec![DebugFrameView {
                id: 17,
                name: "calcular".into(),
                path: Some("src/cálculo.py".into()),
                line: Some(42),
                selected: true,
            }]
            .into(),
            variables: vec![
                DebugVariableView {
                    key: "scope:0".into(),
                    name: "Locales".into(),
                    has_children: true,
                    expanded: true,
                    ..Default::default()
                },
                DebugVariableView {
                    key: "scope:0/0".into(),
                    name: "total".into(),
                    value: "41".into(),
                    type_name: Some("int".into()),
                    depth: 1,
                    changed: true,
                    ..Default::default()
                },
                DebugVariableView {
                    key: "scope:0/1".into(),
                    name: "filas".into(),
                    value: "list(3)".into(),
                    depth: 1,
                    has_children: true,
                    ..Default::default()
                },
            ]
            .into(),
            ..Default::default()
        }
    }
    fn mark(path: &str, line: u32) -> DebugBreakpointView {
        DebugBreakpointView {
            path: path.into(),
            line,
            enabled: true,
            ..Default::default()
        }
    }
    const SIZE: Size = Size::new(280.0, 600.0);
    fn tree_at(state: &DebugView, size: Size) -> UiTree {
        let mut tree = UiTree::new(build_sidebar(state, size.width, size.height));
        tree.set_reduced_motion(true);
        tree.layout(size);
        tree
    }
    fn tree(state: &DebugView) -> UiTree {
        tree_at(state, SIZE)
    }
    fn button(tree: &UiTree, id: &str) -> Option<openide_ui::Rect> {
        tree.frame()
            .regions
            .iter()
            .find(|region| region.kind == HitKind::Button && region.id == id)
            .map(|region| region.rect)
    }
    fn click(tree: &mut UiTree, id: &str) -> Vec<UiAction> {
        let rect = button(tree, id).unwrap_or_else(|| panic!("missing {id}"));
        let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        tree.handle_event(UiEvent::PointerUp(point))
    }
    fn clicks(tree: &mut UiTree, id: String) {
        assert_eq!(click(tree, &id), vec![UiAction::Clicked { id }]);
    }
    /// What the panel says, with wrapped copy read as running text.
    fn prose(tree: &UiTree) -> String {
        copy(tree)
            .replace(" | ", " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn copy(tree: &UiTree) -> String {
        tree.frame()
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }
    fn ink_of(tree: &UiTree, wanted: &str) -> openide_ui::Color {
        tree.frame()
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, color, .. } if text == wanted => Some(*color),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text {wanted}"))
    }

    #[test]
    fn controls_follow_the_session_state_even_with_stale_capabilities() {
        for (status, expected) in [
            (DebugStatus::Idle, vec!["start"]),
            (DebugStatus::Starting, vec!["restart", "stop"]),
            (DebugStatus::Running, vec!["pause", "restart", "stop"]),
            (
                DebugStatus::Stopped,
                vec![
                    "resume",
                    "step_over",
                    "step_into",
                    "step_out",
                    "restart",
                    "stop",
                ],
            ),
            (DebugStatus::Stopping, vec![]),
            (DebugStatus::Ended, vec!["start"]),
            (DebugStatus::Failed, vec!["start"]),
        ] {
            for width in [240.0, 320.0, 480.0] {
                let mut state = fixture();
                state.status = status;
                let mut tree = tree_at(&state, Size::new(width, 600.0));
                for operation in OPERATIONS {
                    let id = action(&state.scope, operation);
                    assert_eq!(
                        button(&tree, &id).is_some(),
                        expected.contains(&operation),
                        "{status:?}/{operation}"
                    );
                    if let Some(rect) = button(&tree, &id) {
                        assert!(rect.x >= 0.0 && rect.x + rect.width <= width);
                        if operation != "start" {
                            assert_eq!((rect.width, rect.height), (24.0, 24.0));
                        }
                        clicks(&mut tree, id);
                    }
                }
                // Hiding never ends the session, and the console is one click away.
                clicks(&mut tree, action(&state.scope, "hide"));
                clicks(&mut tree, action(&state.scope, "console"));
                assert_eq!(
                    button(&tree, &action(&state.scope, "reload")).is_some(),
                    !session_active(status)
                );
            }
        }
    }

    #[test]
    fn a_capability_the_host_withholds_disables_its_control() {
        let mut state = fixture();
        state.controls.step_into = false;
        state.controls.restart = false;
        let tree = tree(&state);
        assert!(button(&tree, &action(&state.scope, "step_into")).is_none());
        assert!(button(&tree, &action(&state.scope, "restart")).is_none());
        assert!(button(&tree, &action(&state.scope, "step_over")).is_some());
    }

    #[test]
    fn idle_is_one_start_row_and_one_muted_line() {
        let mut state = fixture();
        state.status = DebugStatus::Idle;
        state.frames = Arc::from([]);
        state.variables = Arc::from([]);
        for width in [240.0, 480.0] {
            let mut tree = tree_at(&state, Size::new(width, 600.0));
            let select = button(&tree, &action(&state.scope, "configurations")).unwrap();
            let start = button(&tree, &action(&state.scope, "start")).unwrap();
            let gear = button(&tree, &action(&state.scope, "configure")).unwrap();
            assert_eq!((select.y, start.y, gear.y), (select.y, select.y, select.y));
            assert!(select.x < start.x && start.x < gear.x);
            assert_eq!((start.width, start.height), (28.0, 28.0));
            assert!(gear.x + gear.width <= width - 6.0 + 0.5);
            let copy = copy(&tree);
            assert!(copy.contains("Listo · debugpy"), "{copy}");
            assert!(!copy.contains("Iniciar ejecutará"), "{copy}");
            assert_eq!(copy.matches("Disponible al pausar").count(), 2, "{copy}");
            clicks(&mut tree, action(&state.scope, "configurations"));
            clicks(&mut tree, action(&state.scope, "configure"));
        }
        state.configuration_menu = true;
        let mut tree = tree(&state);
        clicks(&mut tree, row_action(&state.scope, "configuration", 1));
        // A session fixes its configuration: the selector leaves with the row.
        state.status = DebugStatus::Running;
        let tree = self::tree(&state);
        assert!(!tree.frame().regions.iter().any(|region| {
            region.id.starts_with("debug_configuration:")
                || region.id == action(&state.scope, "configurations")
                || region.id == action(&state.scope, "configure")
        }));
    }

    #[test]
    fn missing_configuration_has_one_explicit_setup_action_and_no_fake_execution_controls() {
        let mut state = DebugView {
            scope: "fixture:ide".into(),
            ..Default::default()
        };
        state.breakpoints = vec![mark("src/main.py", 3)].into();
        for width in [240.0, 480.0] {
            let mut tree = tree_at(&state, Size::new(width, 600.0));
            let copy = copy(&tree);
            assert!(copy.contains("Sin configuración"), "{copy}");
            assert!(copy.contains("Crear configuración"), "{copy}");
            assert!(!copy.contains("Variables") && !copy.contains("Pila de llamadas"));
            let title = if width < 250.0 {
                "Puntos de pausa"
            } else {
                "Puntos de interrupción"
            };
            assert!(copy.contains(title) && copy.contains("main.py:3"), "{copy}");
            let configure = action(&state.scope, "configure");
            assert_eq!(
                tree.frame()
                    .regions
                    .iter()
                    .filter(|region| region.kind == HitKind::Button && region.id == configure)
                    .count(),
                1
            );
            for operation in OPERATIONS {
                assert!(button(&tree, &action(&state.scope, operation)).is_none());
            }
            clicks(&mut tree, configure);
        }
    }

    #[test]
    fn every_session_state_says_what_it_is() {
        let mut state = fixture();
        state.stop_reason = Some("punto de interrupción".into());
        let wide = Size::new(480.0, 600.0);
        let mut paused = tree_at(&state, wide);
        let text = copy(&paused);
        assert!(text.contains("En pausa en cálculo.py:42"), "{text}");
        assert!(
            text.contains("Python · programa de prueba · punto de interrupción"),
            "{text}"
        );
        assert_eq!(ink_of(&paused, "En pausa en cálculo.py:42"), theme::CODE_NUMBER);
        // The paused line is the way back to the source.
        clicks(&mut paused, row_action(&state.scope, "reveal", 17));

        state.status = DebugStatus::Running;
        state.frames = Arc::from([]);
        let running = tree(&state);
        assert!(copy(&running).contains("En ejecución"));
        assert!(!copy(&running).contains("punto de interrupción |"));
        assert!(!running
            .frame()
            .regions
            .iter()
            .any(|region| region.id.starts_with("debug_reveal:")));

        state.status = DebugStatus::Starting;
        assert!(copy(&tree(&state)).contains("Iniciando…"));
        state.status = DebugStatus::Stopping;
        assert!(copy(&tree(&state)).contains("Finalizando…"));

        state.status = DebugStatus::Ended;
        state.message = Some("Programa finalizado · código 3".into());
        let ended = copy(&tree(&state));
        assert!(ended.contains("Programa finalizado · código 3"), "{ended}");
        state.message = None;
        assert!(copy(&tree(&state)).contains("Sesión finalizada"));

        // A failed request during a session is readable without ending it.
        state.status = DebugStatus::Stopped;
        state.message = Some("stepIn: el hilo no puede avanzar".into());
        let warned = tree(&state);
        assert_eq!(
            ink_of(&warned, "stepIn: el hilo no puede avanzar"),
            theme::WARNING
        );
    }

    #[test]
    fn a_failure_names_the_adapter_and_what_to_do() {
        let mut state = fixture();
        state.status = DebugStatus::Failed;
        state.frames = Arc::from([]);
        state.variables = Arc::from([]);
        state.message =
            Some("Cannot start debug adapter: No such file or directory (os error 2)".into());
        for width in [240.0, 480.0] {
            let tree = tree_at(&state, Size::new(width, 600.0));
            let text = prose(&tree);
            assert!(text.contains("No se pudo iniciar «debugpy»"), "{text}");
            assert!(text.contains("Instalalo"), "{text}");
            assert!(text.contains("os error 2"), "{text}");
            // The way to retry stays where it was.
            assert!(button(&tree, &action(&state.scope, "start")).is_some());
        }
        state.message = Some("launch: el programa no existe".into());
        let text = copy(&tree(&state)).replace(" | ", " ");
        assert!(text.contains("La sesión no pudo continuar"), "{text}");
        assert!(text.contains("launch: el programa no existe"), "{text}");

        // An unreadable debug.json is an error with one way to fix it.
        state.configurations = Arc::from([]);
        state.message = Some("Configuración de depuración: expected value".into());
        let mut tree = tree(&state);
        let text = prose(&tree);
        assert!(text.contains("Configuración inválida"), "{text}");
        assert!(text.contains("expected value"), "{text}");
        assert!(button(&tree, &action(&state.scope, "start")).is_none());
        clicks(&mut tree, action(&state.scope, "configure"));
    }

    #[test]
    fn sections_fold_and_report_their_own_action() {
        let mut state = fixture();
        state.breakpoints = vec![mark("src/main.py", 3)].into();
        let mut tree = tree(&state);
        for key in ["variables", "stack", "breakpoints"] {
            clicks(&mut tree, row_action(&state.scope, "section", key));
        }
        let open = copy(&tree);
        for shown in ["total", "calcular", "main.py:3"] {
            assert!(open.contains(shown), "{open}");
        }
        state.sections = DebugSections {
            variables: false,
            stack: false,
            breakpoints: false,
        };
        let folded = self::tree(&state);
        let text = copy(&folded);
        for hidden in ["total", "calcular", "main.py:3"] {
            assert!(!text.contains(hidden), "{text}");
        }
        // Three headers, one slot apart: a folded section leaves no gap.
        let ys: Vec<f32> = ["variables", "stack", "breakpoints"]
            .iter()
            .map(|key| {
                button(&folded, &row_action(&state.scope, "section", key))
                    .unwrap()
                    .y
            })
            .collect();
        assert_eq!((ys[1] - ys[0], ys[2] - ys[1]), (SLOT, SLOT));
    }

    #[test]
    fn variables_expand_on_request_copy_from_their_menu_and_mark_changes() {
        let mut state = fixture();
        let mut variables = state.variables.to_vec();
        variables.push(DebugVariableView {
            key: "scope:0/more".into(),
            name: "Se muestran los primeros 100".into(),
            depth: 1,
            note: true,
            ..Default::default()
        });
        state.variables = variables.into();
        let mut tree = tree(&state);
        clicks(&mut tree, row_action(&state.scope, "expand", "scope:0"));
        clicks(&mut tree, row_action(&state.scope, "expand", "scope:0/1"));
        // A scalar has nothing to expand, and a note is not a variable.
        for passive in ["scope:0/0", "scope:0/more"] {
            assert!(button(&tree, &row_action(&state.scope, "expand", passive)).is_none());
        }
        assert_eq!(ink_of(&tree, "41"), theme::WARNING);
        assert_eq!(ink_of(&tree, "list(3)"), theme::TOOL_RESULT);
        assert_eq!(ink_of(&tree, "total"), theme::FOREGROUND);
        assert!(copy(&tree).contains("Se muestran los primeros 100"));

        let text_at = |tree: &UiTree, wanted: &str| {
            tree.frame()
                .commands
                .iter()
                .rev()
                .find_map(|command| match command {
                    DrawCommand::Text { position, text, .. } if text == wanted => Some(*position),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no text {wanted}"))
        };
        let value = text_at(&tree, "41");
        tree.handle_event(UiEvent::ContextMenu {
            position: Point::new(value.x + 2.0, value.y + 4.0),
        });
        tree.layout(SIZE);
        assert!(copy(&tree).contains("Copiar nombre"));
        let item = text_at(&tree, "Copiar valor");
        let point = Point::new(item.x + 2.0, item.y + 4.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: row_action(&state.scope, "copy", ("scope:0/0", "value"))
            }]
        );
        tree.layout(SIZE);

        // While its children load, a row is not a second request.
        let mut variables = state.variables.to_vec();
        variables[2].loading = true;
        state.variables = variables.into();
        let tree = self::tree(&state);
        assert!(button(&tree, &row_action(&state.scope, "expand", "scope:0/1")).is_none());
        assert!(copy(&tree).contains("Cargando…"));
    }

    #[test]
    fn frames_stay_virtualized_mark_the_current_one_and_select_on_click() {
        let mut state = fixture();
        state.frames = (0..800)
            .map(|index| DebugFrameView {
                id: 17 + index,
                name: format!("app::módulo::Tipo::calcular_{index}"),
                path: (index != 1).then(|| "src/nested/cálculo.py".into()),
                line: Some(42 + index as u64),
                selected: index == 0,
            })
            .collect::<Vec<_>>()
            .into();
        let mut tree = tree(&state);
        assert!(
            tree.frame().commands.len() < 700,
            "DAP frames remain virtualized"
        );
        let text = copy(&tree);
        assert!(text.contains("Tipo::calcular_0") && text.contains("cálculo.py:42"));
        assert!(!text.contains("app::módulo::Tipo::calcular_0"), "{text}");
        assert_eq!(
            tree.frame()
                .commands
                .iter()
                .filter(|command| matches!(
                    command,
                    DrawCommand::Icon {
                        icon: Icon::ArrowRight,
                        ..
                    }
                ))
                .count(),
            1,
            "only the frame being inspected carries the marker"
        );
        // A frame without source is muted and still selectable.
        assert_eq!(ink_of(&tree, "Tipo::calcular_1"), theme::FOREGROUND_SUBTLE);
        clicks(&mut tree, row_action(&state.scope, "frame", 18));
        clicks(&mut tree, row_action(&state.scope, "frame", 17));
        assert!(rows(&state)
            .iter()
            .any(|row| matches!(row, Row::Hint(text) if text.contains("primeras 100"))));
    }

    #[test]
    fn several_threads_list_the_frames_under_the_selected_one() {
        let mut state = fixture();
        state.threads = vec![
            DebugThreadView {
                id: 1,
                name: "main".into(),
                selected: true,
                stopped: true,
            },
            DebugThreadView {
                id: 2,
                name: "worker".into(),
                selected: false,
                stopped: true,
            },
        ]
        .into();
        let mut tree = tree(&state);
        let y = |tree: &UiTree, id: String| button(tree, &id).unwrap().y;
        let main = y(&tree, row_action(&state.scope, "thread", 1));
        let frame = y(&tree, row_action(&state.scope, "frame", 17));
        let worker = y(&tree, row_action(&state.scope, "thread", 2));
        assert!(main < frame && frame < worker);
        assert!(copy(&tree).contains("Python · programa de prueba · main"));
        clicks(&mut tree, row_action(&state.scope, "thread", 2));
        // One thread needs no row of its own.
        state.threads = state.threads[..1].to_vec().into();
        assert!(button(&self::tree(&state), &row_action(&state.scope, "thread", 1)).is_none());
    }

    #[test]
    fn breakpoints_toggle_navigate_and_leave_one_by_one_or_all_at_once() {
        let mut state = fixture();
        state.breakpoints = vec![
            mark("src/main.py", 2),
            DebugBreakpointView {
                dirty: true,
                ..mark("src/draft.py", 4)
            },
            DebugBreakpointView {
                enabled: false,
                ..mark("src/off.py", 6)
            },
            DebugBreakpointView {
                verified: Some(false),
                message: Some("No hay código en esta línea".into()),
                ..mark("src/main.py", 9)
            },
        ]
        .into();
        for width in [240.0, 480.0] {
            let mut tree = tree_at(&state, Size::new(width, 600.0));
            for (path, line) in [("src/main.py", 2), ("src/off.py", 6), ("src/main.py", 9)] {
                for kind in ["breakpoint", "breakpoint_toggle", "breakpoint_remove"] {
                    let id = row_action(&state.scope, kind, (path, line));
                    let rect = button(&tree, &id).unwrap_or_else(|| panic!("{kind} {path}"));
                    assert!(rect.x >= 0.0 && rect.x + rect.width <= width, "{kind}");
                    clicks(&mut tree, id);
                }
            }
            // An unsaved mark cannot navigate, yet it can be switched or removed.
            let dirty = ("src/draft.py", 4);
            assert!(button(&tree, &row_action(&state.scope, "breakpoint", dirty)).is_none());
            clicks(&mut tree, row_action(&state.scope, "breakpoint_toggle", dirty));
            clicks(&mut tree, row_action(&state.scope, "breakpoint_remove", dirty));
            let text = copy(&tree);
            assert!(text.contains("sin guardar") && text.contains("sin verificar"), "{text}");
            assert_eq!(ink_of(&tree, "off.py:6"), theme::FOREGROUND_SUBTLE);
            assert_eq!(ink_of(&tree, "main.py:9"), theme::WARNING);
            clicks(&mut tree, action(&state.scope, "toggle_breakpoints"));
            clicks(&mut tree, action(&state.scope, "clear_breakpoints"));
        }
        state.breakpoints = Arc::from([]);
        let tree = tree(&state);
        assert!(copy(&tree).contains("Agregá uno con F9 en el editor"));
        for tool in ["toggle_breakpoints", "clear_breakpoints"] {
            assert!(button(&tree, &action(&state.scope, tool)).is_none());
        }
    }

    #[test]
    fn removing_every_breakpoint_asks_first() {
        let mut state = fixture();
        state.breakpoints = vec![mark("src/main.py", 2), mark("src/main.py", 9)].into();
        let size = Size::new(900.0, 600.0);
        let dialog = |state: &DebugView| {
            let mut tree = UiTree::new(clear_breakpoints_dialog(
                state,
                build_sidebar(state, 280.0, size.height),
                size.width,
            ));
            tree.set_reduced_motion(true);
            tree.layout(size);
            tree
        };
        assert!(!copy(&dialog(&state)).contains("¿Quitar todos"));
        state.confirm_clear_breakpoints = true;
        let mut tree = dialog(&state);
        let text = prose(&tree);
        assert!(text.contains("Se quitarán 2 puntos de interrupción"), "{text}");
        clicks(&mut tree, action(&state.scope, "clear_breakpoints_cancel"));
        clicks(&mut tree, action(&state.scope, "clear_breakpoints_confirm"));
    }

    #[test]
    fn a_long_function_path_reads_from_its_type() {
        assert_eq!(short_function("app::workspace::Workspace::open"), "Workspace::open");
        assert_eq!(short_function("Workspace::open"), "Workspace::open");
        assert_eq!(short_function("main"), "main");
        for whole in [
            "<T as core::fmt::Debug>::fmt",
            "core::ops::FnOnce::call_once{{closure}}",
            "ns::f(int, ns::T)",
        ] {
            assert_eq!(short_function(whole), whole);
        }
    }

    #[test]
    fn ide_debug_console_preserves_lines_and_virtualizes_read_only_output() {
        let mut state = fixture();
        state.output = (0..2000)
            .map(|index| DebugOutputView {
                category: "stdout".into(),
                text: format!("line_{index}\nsecond_{index}\n"),
            })
            .collect::<Vec<_>>()
            .into();
        let mut tree = UiTree::new(build_console(&state, 320.0, 240.0));
        tree.set_reduced_motion(true);
        tree.layout(Size::new(320.0, 240.0));
        assert!(tree.frame().commands.len() < 500);
        tree.scroll_to_end(&format!("debug_console:{}", state.scope));
        tree.layout(Size::new(320.0, 240.0));
        for expected in ["line_1999", "second_1999"] {
            assert!(tree.frame().commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == expected)
            ));
        }
        assert!(!tree
            .frame()
            .regions
            .iter()
            .any(|region| region.kind == HitKind::Button));
    }
}

#[cfg(test)]
#[path = "debug_visual_tests.rs"]
mod visual_tests;
