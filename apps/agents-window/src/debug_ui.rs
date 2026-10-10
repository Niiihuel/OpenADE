//! Native debugger presentation. Every actionable ID is scoped to the host's
//! workspace/session epoch; this module never sends DAP or starts a process.
use crate::theme;
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, DropdownMenu, DropdownMenuItem, Element,
    FontFamily, Icon, IconButton, Notice, Separator, Tooltip, TooltipPlacement, TooltipSpec,
};
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
#[derive(Clone, Debug)]
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
}
#[derive(Clone, Debug)]
pub(crate) struct DebugOutputView {
    pub category: String,
    pub text: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DebugBreakpointView {
    pub path: String,
    /// One-based source line. Dirty marks remain visible but cannot navigate
    /// or be sent to an adapter until their document is saved.
    pub line: u32,
    pub dirty: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct DebugSections {
    pub stack: bool,
    pub variables: bool,
    pub output: bool,
    pub breakpoints: bool,
}
impl Default for DebugSections {
    fn default() -> Self {
        Self {
            stack: true,
            variables: true,
            output: false,
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
    pub message: Option<String>,
    pub controls: DebugControls,
    pub threads: Arc<[DebugThreadView]>,
    pub frames: Arc<[DebugFrameView]>,
    pub variables: Arc<[DebugVariableView]>,
    pub output: Arc<[DebugOutputView]>,
    pub breakpoints: Arc<[DebugBreakpointView]>,
    pub sections: DebugSections,
}

pub(crate) fn action(scope: &str, operation: &str) -> String {
    format!(
        "debug_control:{}",
        serde_json::to_string(&(scope, operation)).expect("debug action tuple")
    )
}
fn row_action(scope: &str, kind: &str, key: impl serde::Serialize) -> String {
    format!(
        "debug_{kind}:{}",
        serde_json::to_string(&(scope, key)).expect("debug row tuple")
    )
}

const ROW: f32 = 24.0;
const MAX_FRAMES: usize = 200;
const MAX_VARIABLES: usize = 1024;
const MAX_OUTPUT: usize = 512;
const MAX_TEXT: usize = 4096;

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
fn label(text: impl Into<String>, width: f32, color: openide_ui::Color) -> Element {
    Element::faded_text(text, 16.0)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(color)
        .size(Some(width.max(0.0)), Some(ROW))
}
fn tool(state: &DebugView, op: &str, icon: Icon, hint: &str, enabled: bool) -> Element {
    surface_tool(
        state,
        op,
        icon,
        hint,
        enabled,
        ControlSize::Regular,
        theme::PANEL,
    )
}
/// A debug action: its id comes from the scope and its shortcut from the
/// operation, so the three debug surfaces agree on both.
fn surface_tool(
    state: &DebugView,
    op: &str,
    icon: Icon,
    hint: &str,
    enabled: bool,
    size: ControlSize,
    surface: openide_ui::Color,
) -> Element {
    let shortcut = match op {
        "start" | "resume" => "F5",
        "step_over" => "F10",
        "step_into" => "F11",
        "step_out" => "Shift+F11",
        "stop" => "Shift+F5",
        _ => "",
    };
    IconButton::new(
        action(&state.scope, op),
        icon,
        theme::action_palette(surface, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Ghost)
    .size(size)
    .enabled(enabled && !state.scope.is_empty())
    .tooltip_with(
        TooltipSpec::new(hint)
            .shortcut(shortcut)
            .placement(TooltipPlacement::Above),
    )
    .render()
}
fn status_text(status: DebugStatus) -> &'static str {
    match status {
        DebugStatus::Unconfigured => "Sin configuración de depuración",
        DebugStatus::Idle => "Listo para iniciar",
        DebugStatus::Starting => "Conectando al adaptador…",
        DebugStatus::Running => "Programa en ejecución",
        DebugStatus::Stopped => "Ejecución pausada",
        DebugStatus::Stopping => "Finalizando sesión…",
        DebugStatus::Ended => "Sesión finalizada",
        DebugStatus::Failed => "La sesión no pudo continuar",
    }
}
#[derive(Clone)]
enum Row {
    Section(&'static str, &'static str, usize, bool),
    Thread(DebugThreadView),
    Frame(DebugFrameView),
    Variable(DebugVariableView),
    Output(usize, DebugOutputView),
    Breakpoint(DebugBreakpointView),
    Hint(String),
}
fn rows(state: &DebugView) -> Vec<Row> {
    let mut rows = vec![Row::Section(
        "stack",
        "Pila de llamadas",
        state.frames.len(),
        state.sections.stack,
    )];
    if state.sections.stack {
        if state.status == DebugStatus::Stopped {
            if state.threads.len() > 1 {
                rows.extend(state.threads.iter().take(64).cloned().map(Row::Thread));
            }
            rows.extend(
                state
                    .frames
                    .iter()
                    .take(MAX_FRAMES)
                    .cloned()
                    .map(Row::Frame),
            );
            if state.frames.is_empty() {
                rows.push(Row::Hint("Esperando los marcos del adaptador…".into()));
            }
            if state.frames.len() > MAX_FRAMES {
                rows.push(Row::Hint(format!(
                    "Se muestran los primeros {MAX_FRAMES} marcos"
                )));
            }
        } else {
            rows.push(Row::Hint("Disponible al pausar el programa".into()));
        }
    }
    rows.push(Row::Section(
        "variables",
        "Variables y ámbitos",
        state.variables.len(),
        state.sections.variables,
    ));
    if state.sections.variables {
        if state.status == DebugStatus::Stopped {
            rows.extend(
                state
                    .variables
                    .iter()
                    .take(MAX_VARIABLES)
                    .cloned()
                    .map(Row::Variable),
            );
            if state.variables.is_empty() {
                rows.push(Row::Hint(
                    "Selecciona un marco para inspeccionar sus ámbitos".into(),
                ));
            }
            if state.variables.len() > MAX_VARIABLES {
                rows.push(Row::Hint(format!(
                    "Se muestran {MAX_VARIABLES} filas; contrae otros ámbitos"
                )));
            }
        } else {
            rows.push(Row::Hint(
                "Los valores se consultan con la ejecución pausada".into(),
            ));
        }
    }
    rows.push(Row::Section(
        "output",
        "Salida de depuración",
        state.output.len(),
        state.sections.output,
    ));
    if state.sections.output {
        let omitted = state.output.len().saturating_sub(MAX_OUTPUT);
        if omitted > 0 {
            rows.push(Row::Hint(format!(
                "Últimas {MAX_OUTPUT} entradas de salida"
            )));
        }
        rows.extend(
            state
                .output
                .iter()
                .enumerate()
                .skip(omitted)
                .map(|(index, entry)| Row::Output(index, entry.clone())),
        );
        if state.output.is_empty() {
            rows.push(Row::Hint("El adaptador todavía no envió salida".into()));
        }
    }
    rows
}
fn row(state: &DebugView, row: &Row, width: f32) -> Element {
    row_on_surface(state, row, width, theme::PANEL)
}
fn row_on_surface(state: &DebugView, row: &Row, width: f32, surface: openide_ui::Color) -> Element {
    let palette = theme::component_palette();
    let actions = theme::action_palette(surface, theme::FOREGROUND);
    let width = (width - 16.0).max(0.0);
    match row {
        Row::Section(key, title, count, expanded) => {
            let id = row_action(&state.scope, "section", key);
            actions.interactive_surface(
                Element::clickable(
                    id,
                    Element::row(vec![
                        Element::icon(if *expanded {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        })
                        .foreground(theme::TOOL_RESULT)
                        .size(Some(12.0), Some(12.0))
                        .centered_y()
                        .size(Some(12.0), Some(ROW)),
                        label(*title, (width - 56.0).max(0.0), theme::FOREGROUND),
                        label(count.to_string(), 36.0, theme::FOREGROUND_SUBTLE),
                    ])
                    .gap(4.0),
                )
                .size(Some(width), Some(ROW)),
            )
        }
        Row::Thread(thread) => {
            let id = row_action(&state.scope, "thread", thread.id);
            let title = bounded(&thread.name);
            Tooltip::new(
                actions.interactive_surface(
                    Element::clickable(
                        &id,
                        label(
                            format!(
                                "{} · {}",
                                if thread.stopped {
                                    "Pausado"
                                } else {
                                    "En ejecución"
                                },
                                title
                            ),
                            width,
                            theme::TOOL_RESULT,
                        ),
                    )
                    .background(if thread.selected {
                        theme::SELECTED
                    } else {
                        surface
                    })
                    .size(Some(width), Some(ROW)),
                ),
                &id,
                "Seleccionar hilo",
                palette,
            )
            .description(title)
            .render()
        }
        Row::Frame(frame) => {
            let id = row_action(&state.scope, "frame", frame.id);
            let location = frame.path.as_deref().map_or_else(
                || "Fuente no disponible".into(),
                |path| match frame.line {
                    Some(line) => format!("{path}:{line}"),
                    None => path.into(),
                },
            );
            let content = Element::row(vec![
                Element::icon(Icon::Code)
                    .foreground(if frame.selected {
                        theme::FOREGROUND
                    } else {
                        theme::TOOL_RESULT
                    })
                    .size(Some(14.0), Some(14.0))
                    .centered_y()
                    .size(Some(14.0), Some(ROW)),
                label(
                    bounded(&frame.name),
                    (width - 52.0).max(0.0),
                    theme::FOREGROUND,
                ),
                label(
                    frame.line.map_or_else(String::new, |line| line.to_string()),
                    32.0,
                    theme::FOREGROUND_SUBTLE,
                ),
            ])
            .gap(3.0);
            Tooltip::new(
                actions.interactive_surface(
                    Element::clickable(&id, content)
                        .background(if frame.selected {
                            theme::SELECTED
                        } else {
                            surface
                        })
                        .size(Some(width), Some(ROW)),
                ),
                &id,
                bounded(&frame.name),
                palette,
            )
            .description(bounded(&location))
            .render()
        }
        Row::Variable(variable) => {
            let id = row_action(&state.scope, "expand", &variable.key);
            let indent = (variable.depth.min(8) as f32 * 12.0).min(width * 0.3);
            let available = (width - indent - 18.0).max(0.0);
            let name_width = (available * 0.40).max(0.0);
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
            let content = Element::row(vec![
                Element::spacer().size(Some(indent), Some(ROW)),
                if variable.has_children {
                    Element::icon(if variable.expanded {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    })
                    .foreground(theme::TOOL_RESULT)
                    .size(Some(12.0), Some(12.0))
                    .centered_y()
                    .size(Some(12.0), Some(ROW))
                } else {
                    Element::spacer().size(Some(12.0), Some(ROW))
                },
                label(bounded(&variable.name), name_width, theme::FOREGROUND),
                label(
                    if variable.loading {
                        "Cargando…".into()
                    } else if variable.error.is_some() {
                        "No disponible".into()
                    } else {
                        bounded(&variable.value)
                    },
                    (available - name_width).max(0.0),
                    if variable.error.is_some() {
                        theme::DESTRUCTIVE
                    } else {
                        theme::TOOL_RESULT
                    },
                ),
            ])
            .gap(2.0)
            .size(Some(width), Some(ROW));
            let content = if variable.has_children && !variable.loading {
                actions.interactive_surface(
                    Element::clickable(&id, content).size(Some(width), Some(ROW)),
                )
            } else {
                content
            };
            Tooltip::new(content, &id, bounded(&variable.name), palette)
                .description(detail)
                .render()
        }
        Row::Output(index, output) => {
            let text = bounded(&output.text.replace(['\r', '\n'], " "));
            Tooltip::new(
                label(
                    text,
                    width,
                    if output.category == "stderr" {
                        theme::DESTRUCTIVE
                    } else {
                        theme::TOOL_RESULT
                    },
                )
                .font_family(FontFamily::Monospace),
                format!("debug_output:{}:{index}", state.scope),
                if output.category.is_empty() {
                    "Salida"
                } else {
                    output.category.as_str()
                },
                palette,
            )
            .description(bounded(&output.text))
            .render()
        }
        Row::Breakpoint(marker) => {
            let location = format!("{}:{}", marker.path, marker.line);
            let id = row_action(&state.scope, "breakpoint", (&marker.path, marker.line));
            let ink = if marker.dirty {
                theme::FOREGROUND_SUBTLE
            } else {
                theme::FOREGROUND
            };
            let content = Element::row(vec![
                Element::spacer()
                    .background(if marker.dirty {
                        theme::FOREGROUND_SUBTLE
                    } else {
                        theme::DESTRUCTIVE
                    })
                    .radius(4.0)
                    .size(Some(8.0), Some(8.0))
                    .centered_y()
                    .size(Some(12.0), Some(ROW)),
                label(
                    format!(
                        "{location}{}",
                        if marker.dirty { " · Sin guardar" } else { "" }
                    ),
                    (width - 16.0).max(0.0),
                    ink,
                ),
            ])
            .gap(4.0)
            .size(Some(width), Some(ROW));
            let content = if marker.dirty {
                content
            } else {
                theme::action_palette(surface, ink).interactive_surface(
                    Element::clickable(&id, content).size(Some(width), Some(ROW)),
                )
            };
            Tooltip::new(content, id, location, palette)
                .description(if marker.dirty {
                    "Guarda el archivo para activar este punto de interrupción"
                } else {
                    "Ir al punto de interrupción"
                })
                .render()
        }
        Row::Hint(text) => label(text, width, theme::FOREGROUND_SUBTLE),
    }
}

fn configuration_selector(
    state: &DebugView,
    inner: f32,
    configuration: &str,
    surface: openide_ui::Color,
) -> Element {
    let can_change = !matches!(
        state.status,
        DebugStatus::Starting | DebugStatus::Running | DebugStatus::Stopped | DebugStatus::Stopping
    ) && !state.scope.is_empty()
        && !state.configurations.is_empty();
    let configuration_id = action(&state.scope, "configurations");
    let summary = Element::row(vec![
        label(
            bounded(configuration),
            (inner - 18.0).max(0.0),
            theme::TOOL_RESULT,
        ),
        Element::icon(Icon::ChevronDown)
            .foreground(theme::TOOL_RESULT)
            .size(Some(12.0), Some(12.0))
            .centered_y()
            .size(Some(12.0), Some(ROW)),
    ])
    .gap(6.0)
    .size(Some(inner), Some(ROW));
    let summary = if can_change {
        theme::action_palette(surface, theme::FOREGROUND).interactive_surface(
            Element::clickable(&configuration_id, summary).size(Some(inner), Some(ROW)),
        )
    } else {
        summary
    };
    let summary = Tooltip::new(
        summary,
        &configuration_id,
        bounded(configuration),
        theme::component_palette(),
    )
    .description(if can_change {
        state
            .adapter
            .as_deref()
            .map_or("Selecciona una configuración para iniciar".into(), bounded)
    } else if matches!(
        state.status,
        DebugStatus::Starting | DebugStatus::Running | DebugStatus::Stopped | DebugStatus::Stopping
    ) {
        "La configuración permanece fija durante esta sesión".into()
    } else {
        "Configura un adaptador para comenzar".into()
    })
    .render();
    let summary = DropdownMenu::new(
        summary,
        state
            .configurations
            .iter()
            .take(16)
            .enumerate()
            .map(|(index, name)| {
                DropdownMenuItem::new(
                    row_action(&state.scope, "configuration", index),
                    bounded(name),
                )
                .selected(index == state.selected_configuration)
                .into()
            })
            .collect(),
        state.configuration_menu && can_change,
        action(&state.scope, "dismiss_configurations"),
        theme::menu_palette(),
    )
    .context_menu_style()
    .width(inner.min(420.0))
    .max_visible_items(8)
    .render()
    .popover_return_focus(configuration_id);
    summary
}

fn configure_button(scope: &str) -> Element {
    Button::new(action(scope, "configure"), "Configurar programa", theme::component_palette())
        .icon(Icon::Settings)
        .variant(ButtonVariant::Secondary)
        .size(ControlSize::Regular)
        .enabled(!scope.is_empty())
        .render()
}

/// What the sidebar says before any program is configured.
fn setup_notice(scope: &str) -> Element {
    Notice::new(
        "Configura un programa para comenzar",
        openide_ui::ComponentPalette {
            muted: theme::SETTINGS_FOREGROUND_SUBTLE,
            ..theme::component_palette()
        },
    )
    .description("Elige un adaptador y un programa en .openide/debug.json. Configurar no ejecuta el programa.")
    .action(configure_button(scope))
    .sizing(crate::ui_sizing::DEBUG_SETUP)
    .render()
}

#[cfg(test)]
fn legacy_setup_notice(scope: &str) -> Element {
    let copy = |value: &str, size: f32, color| {
        Element::text(value)
            .font_family(FontFamily::Sans)
            .font_size(size)
            .foreground(color)
    };
    Element::column(vec![
        copy("Configura un programa para comenzar", 13.0, theme::FOREGROUND),
        copy("Elige un adaptador y un programa en .openide/debug.json. Configurar no ejecuta el programa.", 12.0, theme::SETTINGS_FOREGROUND_SUBTLE),
        configure_button(scope),
    ]).gap(10.0).padding(8.0)
}

pub(crate) fn build(state: &DebugView, width: f32, height: f32) -> Element {
    let width = width.max(0.0);
    let inner = (width - 16.0).max(0.0);
    if state.status == DebugStatus::Unconfigured && state.configurations.is_empty() {
        let header = Element::row(vec![
            label("Depuración", (inner - 68.0).max(0.0), theme::FOREGROUND),
            tool(
                state,
                "reload",
                Icon::Refresh,
                "Recargar configuraciones",
                true,
            ),
            tool(
                state,
                "hide",
                Icon::Close,
                "Ocultar panel de depuración",
                true,
            ),
        ])
        .gap(2.0)
        .size(Some(inner), Some(32.0));
        let body = setup_notice(&state.scope);
        return Element::column(vec![
            header,
            openide_ui::ScrollArea::new(format!("debug_setup:{}", state.scope), body)
                .render()
                .flex(1.0),
        ])
        .padding(8.0)
        .background(theme::PANEL)
        .size(Some(width), Some(height.max(0.0)));
    }
    let configuration = if state.configuration.is_empty() {
        "Configura un adaptador para comenzar"
    } else {
        &state.configuration
    };
    let header = Element::row(vec![
        label("Depuración", (inner - 102.0).max(0.0), theme::FOREGROUND),
        tool(
            state,
            "reload",
            Icon::Refresh,
            "Recargar configuraciones",
            !matches!(
                state.status,
                DebugStatus::Starting
                    | DebugStatus::Running
                    | DebugStatus::Stopped
                    | DebugStatus::Stopping
            ),
        ),
        tool(
            state,
            "configure",
            Icon::Settings,
            "Editar configuración de depuración",
            !matches!(
                state.status,
                DebugStatus::Starting
                    | DebugStatus::Running
                    | DebugStatus::Stopped
                    | DebugStatus::Stopping
            ),
        ),
        tool(
            state,
            "hide",
            Icon::Close,
            "Ocultar panel de depuración",
            true,
        ),
    ])
    .gap(2.0)
    .size(Some(inner), Some(32.0));
    let stopped = state.status == DebugStatus::Stopped;
    let running = state.status == DebugStatus::Running;
    let start = matches!(
        state.status,
        DebugStatus::Idle | DebugStatus::Ended | DebugStatus::Failed
    );
    let controls = Element::row(vec![
        if stopped {
            tool(
                state,
                "resume",
                Icon::Play,
                "Continuar ejecución",
                state.controls.resume,
            )
        } else {
            tool(
                state,
                "start",
                Icon::Play,
                "Iniciar depuración",
                start && state.controls.start,
            )
        },
        tool(
            state,
            "pause",
            Icon::Pause,
            "Pausar ejecución",
            running && state.controls.pause,
        ),
        tool(
            state,
            "step_over",
            crate::provider_icons::STEP_OVER,
            "Ejecutar la siguiente línea",
            stopped && state.controls.step_over,
        ),
        tool(
            state,
            "step_into",
            crate::provider_icons::STEP_INTO,
            "Entrar en la llamada",
            stopped && state.controls.step_into,
        ),
        tool(
            state,
            "step_out",
            crate::provider_icons::STEP_OUT,
            "Salir de la llamada",
            stopped && state.controls.step_out,
        ),
        tool(
            state,
            "stop",
            Icon::Stop,
            "Detener sesión de depuración",
            state.controls.stop
                && matches!(
                    state.status,
                    DebugStatus::Starting | DebugStatus::Running | DebugStatus::Stopped
                ),
        ),
    ])
    .gap(4.0)
    .size(Some(inner), Some(32.0));
    let summary = configuration_selector(state, inner, configuration, theme::PANEL);
    let message = state
        .message
        .as_deref()
        .unwrap_or_else(|| status_text(state.status));
    let status = Tooltip::new(
        label(
            bounded(message),
            inner,
            if state.status == DebugStatus::Failed {
                theme::DESTRUCTIVE
            } else {
                theme::FOREGROUND_SUBTLE
            },
        ),
        "debug_status",
        status_text(state.status),
        theme::component_palette(),
    )
    .description(bounded(message))
    .render();
    let rows: Arc<[Row]> = rows(state).into();
    let captured = state.clone();
    let list = Element::uniform_list(
        format!("debug_rows:{}", state.scope),
        rows.len(),
        ROW,
        move |index| row(&captured, &rows[index], inner),
    )
    .top_fade(theme::PANEL, 12.0)
    .flex(1.0);
    Element::column(vec![header, summary, controls, status, list])
        .padding(8.0)
        .background(theme::PANEL)
        .size(Some(width), Some(height.max(0.0)))
}

/// IDE inspector stays on the gray sidebar. Session controls belong to one
/// compact surface; the independently virtualized inspector keeps large DAP
/// responses from inflating either layout work or the configuration header.
pub(crate) fn build_sidebar(state: &DebugView, width: f32, height: f32) -> Element {
    use crate::sidebar_chrome as chrome;
    let width = width.max(0.0);
    let inner = (width - chrome::INSET * 2.0).max(0.0);
    let active = session_active(state.status);
    let control = |op, icon, hint, enabled| {
        surface_tool(
            state,
            op,
            icon,
            hint,
            enabled,
            ControlSize::Dense,
            theme::SIDEBAR,
        )
    };
    let header = openide_ui::PanelHeader::new("Depuración", theme::sidebar_palette())
        .trailing(vec![
            control("reload", Icon::Refresh, "Recargar configuraciones", !active),
            control(
                "configure",
                Icon::Settings,
                "Editar configuración de depuración",
                !active,
            ),
            control("hide", Icon::Close, "Ocultar depuración", true),
        ])
        .width(inner)
        .sizing(crate::ui_sizing::SIDEBAR_HEADER)
        .render();
    let data: Arc<[SidebarRow]> = sidebar_rows(state).into();
    let captured = state.clone();
    // Reserve the native scrollbar rail even before overflow, so section and
    // file actions never jump horizontally when more frames arrive.
    let row_width = (inner - 14.0).max(0.0);
    let list = Element::uniform_list(
        format!("debug_sidebar:{}", state.scope),
        data.len(),
        ROW,
        move |index| sidebar_row(&captured, &data[index], row_width),
    )
    .flex(1.0);
    openide_ui::SidebarPanel::new(
        vec![header, sidebar_session(state, inner), list],
        theme::sidebar_palette(),
    )
    .size(width, height.max(0.0))
    .render()
}

fn session_active(status: DebugStatus) -> bool {
    matches!(
        status,
        DebugStatus::Starting | DebugStatus::Running | DebugStatus::Stopped | DebugStatus::Stopping
    )
}

fn sidebar_caption(text: impl Into<String>, width: f32, color: openide_ui::Color) -> Element {
    Element::truncated_text(text)
        .font_family(FontFamily::Sans)
        .font_size(11.0)
        .foreground(color)
        .size(Some(width.max(0.0)), Some(16.0))
}

fn sidebar_status(status: DebugStatus) -> (&'static str, Icon, openide_ui::Color) {
    match status {
        DebugStatus::Unconfigured => ("Sin configurar", Icon::Settings, theme::TOOL_RESULT),
        DebugStatus::Idle => ("Listo para iniciar", Icon::Play, theme::TOOL_RESULT),
        DebugStatus::Starting => ("Conectando…", Icon::Clock, theme::TOOL_RESULT),
        DebugStatus::Running => ("En ejecución", Icon::Play, theme::DIFF_ADDED),
        DebugStatus::Stopped => ("En pausa", Icon::Pause, theme::CODE_NUMBER),
        DebugStatus::Stopping => ("Finalizando…", Icon::Clock, theme::TOOL_RESULT),
        DebugStatus::Ended => ("Sesión finalizada", Icon::CheckCircle, theme::TOOL_RESULT),
        DebugStatus::Failed => ("Error en la sesión", Icon::ErrorCircle, theme::DESTRUCTIVE),
    }
}

fn sidebar_session(state: &DebugView, width: f32) -> Element {
    let inner = (width - 16.0).max(0.0);
    let configured = !state.configurations.is_empty() || session_active(state.status);
    let mut children = Vec::new();
    if configured {
        children.push(sidebar_caption(
            "Configuración",
            inner,
            theme::FOREGROUND_SUBTLE,
        ));
        let configuration = if state.configuration.is_empty() {
            "Seleccionar programa"
        } else {
            &state.configuration
        };
        children.push(
            configuration_selector(state, inner, configuration, theme::PANEL)
                .background(theme::BACKGROUND)
                .radius(4.0),
        );
        let (status, icon, color) = sidebar_status(state.status);
        children.push(
            Tooltip::new(
                Element::row(vec![
                    Element::icon(icon)
                        .foreground(color)
                        .size(Some(12.0), Some(12.0))
                        .centered_y()
                        .size(Some(12.0), Some(ROW)),
                    label(status, (inner - 18.0).max(0.0), color),
                ])
                .gap(6.0)
                .size(Some(inner), Some(ROW)),
                format!("debug_sidebar_status:{}", state.scope),
                status_text(state.status),
                theme::component_palette(),
            )
            .render(),
        );
        children.push(sidebar_controls(state, inner));
    } else {
        children.push(
            Element::row(vec![
                Element::icon(Icon::Play)
                    .foreground(theme::FOREGROUND)
                    .size(Some(16.0), Some(16.0))
                    .centered_y()
                    .size(Some(16.0), Some(ROW)),
                label(
                    "Empezar a depurar",
                    (inner - 22.0).max(0.0),
                    theme::FOREGROUND,
                ),
            ])
            .gap(6.0)
            .size(Some(inner), Some(ROW)),
        );
        children.push(
            Element::text("Elegí el programa y su adaptador para comenzar.")
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::TOOL_RESULT)
                .size(Some(inner), None),
        );
        children.push(
            Button::new(
                "debug_sidebar_setup",
                "Configurar programa",
                theme::action_palette(theme::SIDEBAR, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Secondary)
            .size(ControlSize::Compact)
            .enabled(!state.scope.is_empty())
            .render()
            .dispatch_as(action(&state.scope, "configure"))
            .min_size(0.0, 0.0)
            .size(Some(inner), Some(28.0)),
        );
    }
    if let Some(message) = state
        .message
        .as_deref()
        .filter(|message| !message.trim().is_empty() && *message != status_text(state.status))
    {
        // Error details are ordinary wrapped text with their own bounded
        // viewport, never a faded single line or an unbounded header.
        children.push(
            openide_ui::ScrollArea::new(
                format!("debug_sidebar_message:{}", state.scope),
                Element::text(bounded(message))
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(if state.status == DebugStatus::Failed {
                        theme::DESTRUCTIVE
                    } else {
                        theme::TOOL_RESULT
                    }),
            )
            .render()
            .max_height(72.0)
            .size(Some(inner), None),
        );
    }
    Element::column(children)
        .gap(4.0)
        .padding(8.0)
        .background(theme::PANEL)
        .radius(6.0)
        .size(Some(width), None)
}

fn sidebar_controls(state: &DebugView, width: f32) -> Element {
    let control = |op, icon, hint, enabled| {
        surface_tool(
            state,
            op,
            icon,
            hint,
            enabled,
            ControlSize::Dense,
            theme::PANEL,
        )
    };
    let stopped = state.status == DebugStatus::Stopped;
    let running = state.status == DebugStatus::Running;
    if !session_active(state.status) {
        return Tooltip::new(
            Button::new(
                action(&state.scope, "start"),
                "Iniciar",
                theme::action_palette(theme::SIDEBAR, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Secondary)
            .icon(Icon::Play)
            .size(ControlSize::Compact)
            .enabled(state.controls.start && !state.scope.is_empty())
            .render()
            .min_size(0.0, 0.0)
            .size(Some(width), Some(28.0)),
            format!("debug_sidebar_start:{}", state.scope),
            "Iniciar depuración",
            theme::component_palette(),
        )
        .shortcut("F5")
        .render();
    }
    Element::row(vec![
        if running {
            control(
                "pause",
                Icon::Pause,
                "Pausar ejecución",
                state.controls.pause,
            )
        } else {
            control(
                "resume",
                Icon::Play,
                "Continuar",
                stopped && state.controls.resume,
            )
        },
        Separator::vertical(theme::component_palette())
            .length(16.0)
            .render(),
        control(
            "step_over",
            crate::provider_icons::STEP_OVER,
            "Siguiente línea",
            stopped && state.controls.step_over,
        ),
        control(
            "step_into",
            crate::provider_icons::STEP_INTO,
            "Entrar en la llamada",
            stopped && state.controls.step_into,
        ),
        control(
            "step_out",
            crate::provider_icons::STEP_OUT,
            "Salir de la llamada",
            stopped && state.controls.step_out,
        ),
        Element::spacer().flex(1.0),
        control(
            "stop",
            Icon::Stop,
            "Detener depuración",
            state.status != DebugStatus::Stopping && state.controls.stop,
        ),
    ])
    .gap(2.0)
    .size(Some(width), Some(ROW))
}

#[derive(Clone)]
enum SidebarRow {
    Item(Row),
    FrameLocation(DebugFrameView),
    Gap,
}

fn sidebar_rows(state: &DebugView) -> Vec<SidebarRow> {
    let mut data = Vec::new();
    if !state.configurations.is_empty() || session_active(state.status) {
        data.push(SidebarRow::Item(Row::Section(
            "stack",
            "Pila de llamadas",
            state.frames.len(),
            state.sections.stack,
        )));
        if state.sections.stack {
            if state.status == DebugStatus::Stopped {
                if state.threads.len() > 1 {
                    data.extend(
                        state
                            .threads
                            .iter()
                            .take(64)
                            .cloned()
                            .map(|thread| SidebarRow::Item(Row::Thread(thread))),
                    );
                }
                for frame in state.frames.iter().take(MAX_FRAMES) {
                    data.push(SidebarRow::Item(Row::Frame(frame.clone())));
                    data.push(SidebarRow::FrameLocation(frame.clone()));
                }
                if state.frames.is_empty() {
                    data.push(SidebarRow::Item(Row::Hint("Cargando llamadas…".into())));
                }
                if state.frames.len() > MAX_FRAMES {
                    data.push(SidebarRow::Item(Row::Hint(format!(
                        "Primeras {MAX_FRAMES} llamadas"
                    ))));
                }
            } else {
                data.push(SidebarRow::Item(Row::Hint("Disponible al pausar".into())));
            }
        }
        data.push(SidebarRow::Gap);
        data.push(SidebarRow::Item(Row::Section(
            "variables",
            "Variables",
            state.variables.len(),
            state.sections.variables,
        )));
        if state.sections.variables {
            if state.status == DebugStatus::Stopped {
                data.extend(
                    state
                        .variables
                        .iter()
                        .take(MAX_VARIABLES)
                        .cloned()
                        .map(|variable| SidebarRow::Item(Row::Variable(variable))),
                );
                if state.variables.is_empty() {
                    data.push(SidebarRow::Item(Row::Hint("Seleccioná una llamada".into())));
                }
                if state.variables.len() > MAX_VARIABLES {
                    data.push(SidebarRow::Item(Row::Hint(format!(
                        "Máximo: {MAX_VARIABLES} variables"
                    ))));
                }
            } else {
                data.push(SidebarRow::Item(Row::Hint("Disponible al pausar".into())));
            }
        }
        data.push(SidebarRow::Gap);
    }
    data.push(SidebarRow::Item(Row::Section(
        "breakpoints",
        "Puntos de interrupción",
        state.breakpoints.len(),
        state.sections.breakpoints,
    )));
    if state.sections.breakpoints {
        data.extend(
            state
                .breakpoints
                .iter()
                .cloned()
                .map(|marker| SidebarRow::Item(Row::Breakpoint(marker))),
        );
        if state.breakpoints.is_empty() {
            data.push(SidebarRow::Item(Row::Hint("Agregá uno con F9".into())));
        }
    }
    data
}

fn sidebar_row(state: &DebugView, row: &SidebarRow, width: f32) -> Element {
    let palette = theme::action_palette(theme::SIDEBAR, theme::FOREGROUND);
    match row {
        SidebarRow::Gap => Element::spacer().size(Some(width), Some(ROW)),
        SidebarRow::Item(Row::Section(key, title, count, expanded)) => {
            let id = row_action(&state.scope, "section", key);
            Tooltip::new(
                palette.interactive_surface(
                    Element::clickable(
                        &id,
                        Element::row(vec![
                            Element::icon(if *expanded {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronRight
                            })
                            .foreground(theme::TOOL_RESULT)
                            .size(Some(12.0), Some(12.0))
                            .centered_y()
                            .size(Some(12.0), Some(ROW)),
                            label(
                                if *key == "breakpoints" && width < 240.0 {
                                    "Puntos de pausa"
                                } else {
                                    title
                                },
                                (width - 46.0).max(0.0),
                                theme::FOREGROUND,
                            ),
                            label(count.to_string(), 22.0, theme::FOREGROUND_SUBTLE),
                        ])
                        .gap(4.0),
                    )
                    .background(theme::PANEL)
                    .radius(4.0)
                    .size(Some(width), Some(ROW)),
                ),
                id,
                *title,
                theme::component_palette(),
            )
            .render()
        }
        SidebarRow::Item(Row::Frame(frame)) => {
            let id = row_action(&state.scope, "frame", frame.id);
            Tooltip::new(
                palette.bar_item_surface(
                    Element::clickable(
                        &id,
                        Element::row(vec![
                            Element::icon(Icon::Code)
                                .foreground(theme::TOOL_RESULT)
                                .size(Some(14.0), Some(14.0))
                                .centered_y()
                                .size(Some(14.0), Some(ROW)),
                            label(
                                bounded(&frame.name),
                                (width - 18.0).max(0.0),
                                theme::FOREGROUND,
                            ),
                        ])
                        .gap(4.0),
                    )
                    .background(if frame.selected {
                        theme::SELECTED
                    } else {
                        theme::SIDEBAR
                    })
                    .size(Some(width), Some(ROW)),
                ),
                id,
                bounded(&frame.name),
                theme::component_palette(),
            )
            .render()
        }
        SidebarRow::FrameLocation(frame) => {
            let id = row_action(&state.scope, "frame", frame.id);
            let location = frame.path.as_deref().map_or_else(
                || "Fuente no disponible".into(),
                |path| match frame.line {
                    Some(line) => format!("{path}:{line}"),
                    None => path.into(),
                },
            );
            let short = frame.path.as_deref().map_or_else(
                || "Fuente no disponible".into(),
                |path| match frame.line {
                    Some(line) => format!("{}:{line}", basename(path)),
                    None => basename(path).into(),
                },
            );
            let location_id = format!("{id}:location");
            Tooltip::new(
                palette.bar_item_surface(
                    Element::clickable(
                        &location_id,
                        Element::row(vec![
                            Element::spacer().size(Some(18.0), Some(ROW)),
                            label(short, (width - 18.0).max(0.0), theme::FOREGROUND_SUBTLE)
                                .font_size(11.0),
                        ]),
                    )
                    .dispatch_as(id)
                    .background(if frame.selected {
                        theme::SELECTED
                    } else {
                        theme::SIDEBAR
                    })
                    .size(Some(width), Some(ROW)),
                ),
                location_id,
                bounded(&location),
                theme::component_palette(),
            )
            .render()
        }
        SidebarRow::Item(Row::Breakpoint(marker)) => {
            let id = row_action(&state.scope, "breakpoint", (&marker.path, marker.line));
            let content = Element::row(vec![
                Element::spacer()
                    .background(if marker.dirty {
                        theme::FOREGROUND_SUBTLE
                    } else {
                        theme::DESTRUCTIVE
                    })
                    .radius(4.0)
                    .size(Some(8.0), Some(8.0))
                    .centered_y()
                    .size(Some(8.0), Some(ROW)),
                label(
                    basename(&marker.path),
                    (width - 42.0).max(0.0),
                    if marker.dirty {
                        theme::FOREGROUND_SUBTLE
                    } else {
                        theme::FOREGROUND
                    },
                ),
                label(marker.line.to_string(), 26.0, theme::FOREGROUND_SUBTLE),
            ])
            .gap(4.0)
            .size(Some(width), Some(ROW));
            let content = if marker.dirty {
                content
            } else {
                palette.interactive_surface(
                    Element::clickable(&id, content).size(Some(width), Some(ROW)),
                )
            };
            Tooltip::new(
                content,
                id,
                format!("{}:{}", marker.path, marker.line),
                theme::component_palette(),
            )
            .description(if marker.dirty {
                "Guarda el archivo para activar este punto de interrupción"
            } else {
                "Ir al punto de interrupción"
            })
            .render()
        }
        SidebarRow::Item(Row::Hint(text)) => {
            label(text, width, theme::FOREGROUND_SUBTLE).font_size(11.0)
        }
        SidebarRow::Item(row) => row_on_surface(state, row, width + 16.0, theme::SIDEBAR),
    }
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
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

    #[test]
    fn setup_notice_matches_its_legacy_body() {
        for scope in ["", "proyecto"] {
            crate::state_panel_parity_tests::assert_same(
                "debug setup",
                legacy_setup_notice(scope),
                setup_notice(scope),
            );
        }
    }

    use openide_ui::{DrawCommand, HitKind, Point, Size, UiAction, UiEvent, UiTree};
    fn fixture() -> DebugView {
        DebugView {
            scope: "workspace:fixture/session:7".into(),
            status: DebugStatus::Stopped,
            configuration: "Python · programa de prueba".into(),
            adapter: Some("debugpy".into()),
            controls: DebugControls {
                start: true,
                resume: true,
                pause: true,
                step_over: true,
                step_into: true,
                step_out: true,
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
            variables: vec![DebugVariableView {
                key: "frame17:scope1".into(),
                name: "Locales".into(),
                value: String::new(),
                type_name: None,
                depth: 0,
                has_children: true,
                expanded: false,
                loading: false,
                error: None,
            }]
            .into(),
            ..Default::default()
        }
    }
    fn tree(state: &DebugView, width: f32) -> UiTree {
        let mut tree = UiTree::new(build(state, width, 360.0));
        tree.set_reduced_motion(true);
        tree.layout(Size::new(width, 360.0));
        tree
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

    #[test]
    fn ide_debug_sidebar_keeps_compact_controls_and_dirty_breakpoints_passive() {
        let mut state = fixture();
        state.configurations = vec!["Python".into()].into();
        state.breakpoints = vec![
            DebugBreakpointView {
                path: "main.py".into(),
                line: 2,
                dirty: false,
            },
            DebugBreakpointView {
                path: "draft.py".into(),
                line: 4,
                dirty: true,
            },
        ]
        .into();
        for width in [180.0, 240.0, 320.0] {
            let mut tree = UiTree::new(build_sidebar(&state, width, 600.0));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(width, 600.0));
            for operation in [
                "hide",
                "resume",
                "step_over",
                "step_into",
                "step_out",
                "stop",
            ] {
                let id = action(&state.scope, operation);
                let rect = button(&tree, &id).unwrap();
                assert_eq!((rect.width, rect.height), (24.0, 24.0));
                assert!(rect.x >= 0.0 && rect.x + rect.width <= width);
                assert_eq!(click(&mut tree, &id), vec![UiAction::Clicked { id }]);
            }
            let saved = row_action(&state.scope, "breakpoint", ("main.py", 2));
            let dirty = row_action(&state.scope, "breakpoint", ("draft.py", 4));
            assert!(button(&tree, &saved).is_some());
            assert!(button(&tree, &dirty).is_none());
            assert_eq!(
                click(&mut tree, &saved),
                vec![UiAction::Clicked { id: saved }]
            );
        }
    }

    #[test]
    fn sidebar_actions_respect_session_state_even_with_stale_capabilities() {
        for (status, expected) in [
            (DebugStatus::Idle, vec!["start"]),
            (DebugStatus::Starting, vec!["stop"]),
            (DebugStatus::Running, vec!["pause", "stop"]),
            (
                DebugStatus::Stopped,
                vec!["resume", "step_over", "step_into", "step_out", "stop"],
            ),
            (DebugStatus::Stopping, vec![]),
            (DebugStatus::Ended, vec!["start"]),
            (DebugStatus::Failed, vec!["start"]),
        ] {
            let mut state = fixture();
            state.status = status;
            state.configurations = vec![state.configuration.clone()].into();
            let mut tree = UiTree::new(build_sidebar(&state, 240.0, 600.0));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(240.0, 600.0));
            for operation in [
                "start",
                "resume",
                "pause",
                "step_over",
                "step_into",
                "step_out",
                "stop",
            ] {
                let id = action(&state.scope, operation);
                assert_eq!(
                    button(&tree, &id).is_some(),
                    expected.contains(&operation),
                    "{status:?}/{operation}"
                );
                if expected.contains(&operation) {
                    assert_eq!(click(&mut tree, &id), vec![UiAction::Clicked { id }]);
                }
            }
        }
    }

    #[test]
    fn sidebar_frame_location_dispatches_same_scoped_action_and_collapses_with_stack() {
        let mut state = fixture();
        state.configurations = vec![state.configuration.clone()].into();
        state.frames = (0..800)
            .map(|index| DebugFrameView {
                id: 17 + index,
                name: format!("calcular_{index}"),
                path: Some("src/nested/cálculo.py".into()),
                line: Some(42),
                selected: index == 0,
            })
            .collect::<Vec<_>>()
            .into();
        let size = Size::new(240.0, 480.0);
        let mut tree = UiTree::new(build_sidebar(&state, size.width, size.height));
        tree.set_reduced_motion(true);
        tree.layout(size);
        let id = row_action(&state.scope, "frame", 17);
        let location = format!("{id}:location");
        assert_eq!(
            click(&mut tree, &location),
            vec![UiAction::Clicked { id: id.clone() }]
        );
        let section = row_action(&state.scope, "section", "stack");
        assert_eq!(
            click(&mut tree, &section),
            vec![UiAction::Clicked { id: section }]
        );
        assert!(
            tree.frame().commands.len() < 700,
            "DAP frames remain virtualized"
        );
        assert_eq!(
            sidebar_rows(&state)
                .iter()
                .filter(|row| matches!(row, SidebarRow::FrameLocation(_)))
                .count(),
            MAX_FRAMES
        );
        state.sections.stack = false;
        tree.set_root(build_sidebar(&state, size.width, size.height));
        tree.layout(size);
        assert!(button(&tree, &id).is_none());
        assert!(button(&tree, &location).is_none());
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
    #[test]
    fn debugger_controls_reflect_live_state_and_never_start_during_layout() {
        for (status, visible) in [
            (
                DebugStatus::Unconfigured,
                vec!["configure", "hide", "reload"],
            ),
            (
                DebugStatus::Idle,
                vec!["configure", "hide", "reload", "start"],
            ),
            (DebugStatus::Starting, vec!["hide", "stop"]),
            (DebugStatus::Running, vec!["hide", "pause", "stop"]),
            (
                DebugStatus::Stopped,
                vec![
                    "hide",
                    "resume",
                    "step_over",
                    "step_into",
                    "step_out",
                    "stop",
                ],
            ),
            (DebugStatus::Stopping, vec!["hide"]),
        ] {
            let mut state = fixture();
            state.status = status;
            let mut tree = tree(&state, 320.0);
            for op in [
                "configure",
                "hide",
                "reload",
                "start",
                "resume",
                "pause",
                "step_over",
                "step_into",
                "step_out",
                "stop",
            ] {
                assert_eq!(
                    button(&tree, &action(&state.scope, op)).is_some(),
                    visible.contains(&op),
                    "{status:?}/{op}"
                );
            }
            assert!(tree
                .handle_event(UiEvent::PointerMove(Point::new(310.0, 350.0)))
                .is_empty());
        }
    }
    #[test]
    fn missing_configuration_has_one_explicit_setup_action_and_no_fake_execution_controls() {
        let state = DebugView {
            scope: "fixture:ide".into(),
            ..Default::default()
        };
        for width in [280.0, 520.0] {
            let mut tree = tree(&state, width);
            let frame = tree.frame();
            let copy = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                copy.contains("Configura un programa para comenzar"),
                "{copy}"
            );
            assert!(copy.contains("Configurar programa"));
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Icon { icon: Icon::Settings, rect, .. }
                    if rect.width == 16.0 && rect.height == 16.0)));
            assert!(!copy.contains("No such file") && !copy.contains("Esperando los marcos"));
            let configure = action(&state.scope, "configure");
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .filter(|region| region.kind == HitKind::Button && region.id == configure)
                    .count(),
                1
            );
            assert!(button(&tree, &action(&state.scope, "start")).is_none());
            assert_eq!(
                click(&mut tree, &configure),
                vec![UiAction::Clicked { id: configure }]
            );
        }
    }
    #[test]
    fn debugger_toolbar_targets_are_32px_and_scoped_actions_work_at_narrow_and_wide_sizes() {
        for width in [320.0, 900.0] {
            let state = fixture();
            let mut tree = tree(&state, width);
            for op in [
                "hide",
                "resume",
                "step_over",
                "step_into",
                "step_out",
                "stop",
            ] {
                let id = action(&state.scope, op);
                let rect = button(&tree, &id).unwrap();
                assert_eq!((rect.width, rect.height), (32.0, 32.0));
                assert!(rect.x >= 0.0 && rect.x + rect.width <= width);
                assert_eq!(click(&mut tree, &id), vec![UiAction::Clicked { id }]);
            }
            let frame = row_action(&state.scope, "frame", 17);
            assert_eq!(
                click(&mut tree, &frame),
                vec![UiAction::Clicked { id: frame }]
            );
        }
    }
    #[test]
    fn debugger_variables_expand_only_on_request_and_loading_or_scalar_rows_are_passive() {
        let mut state = fixture();
        let id = row_action(&state.scope, "expand", "frame17:scope1");
        let mut tree = tree(&state, 320.0);
        assert_eq!(
            click(&mut tree, &id),
            vec![UiAction::Clicked { id: id.clone() }]
        );
        for (loading, has_children) in [(true, true), (false, false)] {
            let mut variables = state.variables.to_vec();
            variables[0].loading = loading;
            variables[0].has_children = has_children;
            state.variables = variables.into();
            tree.set_root(build(&state, 320.0, 360.0));
            tree.layout(Size::new(320.0, 360.0));
            assert!(button(&tree, &id).is_none());
        }
    }
    #[test]
    fn debugger_output_is_bounded_virtualized_and_uses_unique_tooltip_ids() {
        let mut state = fixture();
        state.sections = DebugSections {
            stack: false,
            variables: false,
            output: true,
            breakpoints: true,
        };
        state.output = (0..10_000)
            .map(|index| DebugOutputView {
                category: "stdout".into(),
                text: format!("captured_line_{index:05}"),
            })
            .collect::<Vec<_>>()
            .into();
        let data = rows(&state);
        assert_eq!(
            data.iter()
                .filter(|row| matches!(row, Row::Output(..)))
                .count(),
            MAX_OUTPUT
        );
        let mut tree = tree(&state, 320.0);
        assert!(
            tree.frame().commands.len() < 1500,
            "only the viewport is built"
        );
        tree.scroll_to_end(&format!("debug_rows:{}", state.scope));
        tree.layout(Size::new(320.0, 360.0));
        assert!(tree.frame().commands.iter().any(|command| matches!(command, DrawCommand::Text {text,..} if text.contains("captured_line_09999"))));
        assert!(!tree.frame().commands.iter().any(|command| matches!(command, DrawCommand::Text {text,..} if text.contains("captured_line_00000"))));
        let mut ids = std::collections::HashSet::new();
        for entry in data.iter().filter(|row| matches!(row, Row::Output(..))) {
            let openide_ui::ElementKind::Tooltip { anchor_id, .. } = row(&state, entry, 300.0).kind
            else {
                panic!("output help missing");
            };
            assert!(ids.insert(anchor_id));
        }
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;
    use openide_ui::{HitKind, Point, Size, UiAction, UiEvent, UiTree};
    #[test]
    fn debug_configuration_menu_is_explicit_and_locked_while_a_session_is_active() {
        let mut state = DebugView {
            scope: "workspace:private/session:0".into(),
            status: DebugStatus::Idle,
            configurations: vec!["Python · principal".into(), "Python · prueba".into()].into(),
            configuration: "Python · principal".into(),
            configuration_menu: true,
            ..Default::default()
        };
        let mut tree = UiTree::new(build(&state, 320.0, 300.0));
        tree.set_reduced_motion(true);
        tree.layout(Size::new(320.0, 300.0));
        let id = row_action(&state.scope, "configuration", 1);
        let target = tree
            .frame()
            .regions
            .iter()
            .find(|region| region.kind == HitKind::Button && region.id == id)
            .unwrap()
            .rect;
        let point = Point::new(
            target.x + target.width / 2.0,
            target.y + target.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked { id }]
        );
        state.status = DebugStatus::Running;
        tree.set_root(build(&state, 320.0, 300.0));
        tree.layout(Size::new(320.0, 300.0));
        assert!(!tree
            .frame()
            .regions
            .iter()
            .any(|region| region.kind == HitKind::Button
                && region.id.starts_with("debug_configuration:")));
        assert!(!tree
            .frame()
            .regions
            .iter()
            .any(|region| region.kind == HitKind::Button
                && region.id == action(&state.scope, "configurations")));
        assert!(
            tree.frame()
                .regions
                .iter()
                .any(|region| region.kind == HitKind::Button
                    && region.id == action(&state.scope, "hide")),
            "hiding never terminates the debugger"
        );
    }
}

#[cfg(test)]
#[path = "debug_visual_tests.rs"]
mod visual_tests;
