//! Settings workspace, with its own navigation and neutral card layout.
//!
//! This module only describes UI. The app owns settings values and handles the
//! action IDs, so navigating away and back does not reset edits.

use std::path::Path;

use openide_core::SessionSort;
use openide_harness::{
    CommandIsolation, ExecDecision, HookPhase, HostedWebSearchMode, LocalTool, SkillOrigin,
};
use openide_ui::{
    Button, ButtonVariant, Color, Component, ControlSize, Element, FontFamily, Icon,
    IconButton as UiIconButton, Kbd, Modal, ScrollArea, Switch, TextArea,
};

use crate::{
    github_account::GithubAccountStatus,
    native_provider,
    settings_parts::{self, CapabilityStatus, FeedbackTone},
    settings_preferences::TerminalShell,
    theme,
    view::{self, McpOAuthViewPhase, SessionOrganization, ViewState},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SettingsPage {
    #[default]
    General,
    Appearance,
    Agent,
    Providers,
    Browser,
    ComputerUse,
    Terminal,
    WorkspaceSearch,
    Memory,
    Subagents,
    Plugins,
    Mcp,
    Skills,
    Commands,
    Automations,
    Hooks,
    Data,
    Usage,
    Github,
    Shortcuts,
}

const BASIC_PAGES: &[SettingsPage] = &[
    SettingsPage::General,
    SettingsPage::Appearance,
    SettingsPage::Providers,
    SettingsPage::Browser,
    SettingsPage::ComputerUse,
    SettingsPage::Terminal,
    SettingsPage::Shortcuts,
    SettingsPage::WorkspaceSearch,
];
const AGENT_PAGES: &[SettingsPage] = &[
    SettingsPage::Agent,
    SettingsPage::Memory,
    SettingsPage::Subagents,
    SettingsPage::Plugins,
    SettingsPage::Mcp,
    SettingsPage::Skills,
    SettingsPage::Commands,
    SettingsPage::Automations,
    SettingsPage::Hooks,
];
const DATA_PAGES: &[SettingsPage] = &[
    SettingsPage::Data,
    SettingsPage::Usage,
    SettingsPage::Github,
];

const SETTINGS_GROUPS: &[(&str, &[SettingsPage])] = &[
    ("BÁSICO", BASIC_PAGES),
    ("CAPACIDADES DEL AGENTE", AGENT_PAGES),
    ("DATOS Y ESTADÍSTICAS", DATA_PAGES),
];

impl SettingsPage {
    pub const fn action_id(self) -> &'static str {
        match self {
            Self::General => "settings_page:general",
            Self::Appearance => "settings_page:appearance",
            Self::Agent => "settings_page:agent",
            Self::Providers => "settings_page:providers",
            Self::Browser => "settings_page:browser",
            Self::ComputerUse => "settings_page:computer_use",
            Self::Terminal => "settings_page:terminal",
            Self::WorkspaceSearch => "settings_page:workspace_search",
            Self::Memory => "settings_page:memory",
            Self::Subagents => "settings_page:subagents",
            Self::Plugins => "settings_page:plugins",
            Self::Mcp => "settings_page:mcp",
            Self::Skills => "settings_page:skills",
            Self::Commands => "settings_page:commands",
            Self::Automations => "settings_page:automations",
            Self::Hooks => "settings_page:hooks",
            Self::Data => "settings_page:data",
            Self::Usage => "settings_page:usage",
            Self::Github => "settings_page:github",
            Self::Shortcuts => "settings_page:shortcuts",
        }
    }

    pub fn from_action_id(id: &str) -> Option<Self> {
        match id {
            "settings_page:general" => Some(Self::General),
            "settings_page:appearance" => Some(Self::Appearance),
            "settings_page:agent" => Some(Self::Agent),
            "settings_page:providers" => Some(Self::Providers),
            "settings_page:browser" => Some(Self::Browser),
            "settings_page:computer_use" => Some(Self::ComputerUse),
            "settings_page:terminal" => Some(Self::Terminal),
            "settings_page:workspace_search" => Some(Self::WorkspaceSearch),
            "settings_page:memory" => Some(Self::Memory),
            "settings_page:subagents" => Some(Self::Subagents),
            "settings_page:plugins" => Some(Self::Plugins),
            "settings_page:mcp" => Some(Self::Mcp),
            "settings_page:skills" => Some(Self::Skills),
            "settings_page:commands" => Some(Self::Commands),
            "settings_page:automations" => Some(Self::Automations),
            "settings_page:hooks" => Some(Self::Hooks),
            "settings_page:data" => Some(Self::Data),
            "settings_page:usage" => Some(Self::Usage),
            "settings_page:github" => Some(Self::Github),
            "settings_page:shortcuts" => Some(Self::Shortcuts),
            _ => None,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Apariencia",
            Self::Agent => "Agente",
            Self::Providers => "Proveedores y modelos",
            Self::Browser => "Navegador",
            Self::ComputerUse => "Control del equipo",
            Self::Terminal => "Terminal",
            Self::WorkspaceSearch => "Búsqueda de archivos",
            Self::Memory => "Memoria",
            Self::Subagents => "Subagentes",
            Self::Plugins => "Plugins",
            Self::Mcp => "MCP",
            Self::Skills => "Skills",
            Self::Commands => "Comandos",
            Self::Automations => "Automatizaciones",
            Self::Hooks => "Hooks",
            Self::Data => "Datos y sesiones",
            Self::Usage => "Uso",
            Self::Github => "GitHub",
            Self::Shortcuts => "Atajos de teclado",
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::General => Icon::Settings,
            Self::Appearance => Icon::Laptop,
            Self::Agent => Icon::Chat,
            Self::Providers => Icon::Code,
            Self::Browser => Icon::Search,
            Self::ComputerUse => Icon::Laptop,
            Self::Terminal => Icon::Terminal,
            Self::WorkspaceSearch => Icon::Folder,
            Self::Memory => Icon::History,
            Self::Subagents => Icon::Grid,
            Self::Plugins => Icon::Grid,
            Self::Mcp => Icon::GitBranch,
            Self::Skills => Icon::Grid,
            Self::Commands => Icon::Terminal,
            Self::Automations => Icon::Clock,
            Self::Hooks => Icon::GitBranch,
            Self::Data => Icon::Archive,
            Self::Usage => Icon::Clock,
            Self::Github => Icon::Github,
            Self::Shortcuts => Icon::Hash,
        }
    }

    const fn nav_title(self) -> &'static str {
        match self {
            Self::Providers => "Modelos",
            Self::ComputerUse => "Equipo",
            Self::WorkspaceSearch => "Búsqueda",
            Self::Automations => "Automatizaciones",
            Self::Data => "Datos",
            Self::Shortcuts => "Atajos",
            _ => self.title(),
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::General => "Proyecto, inicio y preferencias generales.",
            Self::Appearance => "Tema y movimiento de la interfaz nativa.",
            Self::Providers => "Catálogo, conexiones y modelos del agente Rust.",
            Self::Browser => "Acceso del agente a sesiones de navegador.",
            Self::ComputerUse => "Control del escritorio y sus permisos.",
            Self::Terminal => "Shell y comportamiento de la terminal integrada.",
            Self::Shortcuts => "Combinaciones activas para navegar y trabajar.",
            Self::WorkspaceSearch => "Búsqueda de archivos del proyecto.",
            Self::Agent => "Herramientas y permisos del agente nativo.",
            Self::Memory => "Contexto de conversación y memoria persistente.",
            Self::Subagents => "Delegación y configuración de agentes especializados.",
            Self::Plugins => "Extensiones ejecutables y capacidades instaladas.",
            Self::Mcp => "Servidores de herramientas externos.",
            Self::Skills => "Instrucciones instaladas para el agente.",
            Self::Commands => "Centro de comandos y reglas para la ejecución del agente.",
            Self::Automations => "Tareas programadas del proyecto.",
            Self::Hooks => "Acciones asociadas a eventos del agente.",
            Self::Data => "Historial local y organización de tareas.",
            Self::Usage => "Actividad, tokens y costes de ejecución.",
            Self::Github => "Conexión de la cuenta y repositorio de GitHub.",
        }
    }

    const fn status(self) -> CapabilityStatus {
        match self {
            Self::Skills | Self::Automations => CapabilityStatus::Ready,
            Self::Browser | Self::ComputerUse | Self::Subagents | Self::Plugins | Self::Usage => {
                CapabilityStatus::Planned
            }
            Self::Mcp | Self::Hooks => CapabilityStatus::Partial,
            _ => CapabilityStatus::Partial,
        }
    }
}

fn text(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}

fn subtle(value: impl Into<String>) -> Element {
    text(value, theme::FOREGROUND_SUBTLE, 12.0)
}

fn section(value: &str) -> Element {
    text(value, theme::FOREGROUND_SUBTLE, 12.0)
        .padding(8.0)
        .min_size(0.0, 28.0)
}

fn divider() -> Element {
    Element::spacer()
        .size(None, Some(1.0))
        .background(theme::BORDER)
}

fn card(children: Vec<Element>) -> Element {
    Element::panel(Element::column(children).gap(16.0))
        .padding(22.0)
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(14.0)
}

fn field(id: &str, value: &str, placeholder: &str) -> Element {
    Element::text_input(id, value, placeholder)
        .background(theme::INPUT)
        .foreground(theme::FOREGROUND)
        .font_family(FontFamily::Sans)
        .border(theme::BORDER, 1.0)
        .radius(9.0)
        .min_size(80.0, 40.0)
}

fn group(title: &str, children: Vec<Element>) -> Element {
    Element::column(vec![section(title), card(children)]).gap(4.0)
}

fn value_badge(value: impl Into<String>) -> Element {
    Element::panel(text(value, theme::FOREGROUND, 12.0))
        .padding(8.0)
        .background(theme::SELECTED)
        .radius(8.0)
}

fn action_button(id: &str, label: &str, icon: Icon) -> Element {
    Button::new(
        id,
        label,
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .icon(icon)
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Regular)
    .render()
}

fn mcp_action_button(id: &str, label: &str, icon: Icon, enabled: bool) -> Element {
    Button::new(
        id,
        label,
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .icon(icon)
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Regular)
    .enabled(enabled)
    .render()
}

fn choice_button(id: &str, label: &str, active: bool) -> Element {
    Button::new(
        id,
        label,
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .variant(if active {
        ButtonVariant::Raised
    } else {
        ButtonVariant::Ghost
    })
    .size(ControlSize::Regular)
    .active(active)
    .render()
}

fn isolation_choice(id: &str, label: &str, active: bool, enabled: bool) -> Element {
    Button::new(
        id,
        label,
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .variant(if active {
        ButtonVariant::Raised
    } else {
        ButtonVariant::Ghost
    })
    .size(ControlSize::Regular)
    .active(active)
    .enabled(enabled)
    .render()
}

fn choice_group(options: Vec<Element>, compact: bool) -> Element {
    if compact {
        Element::column(options).gap(6.0)
    } else {
        Element::row(options).gap(6.0)
    }
}

fn setting_row(title: &str, description: &str, trailing: Element, compact: bool) -> Element {
    let copy = Element::column(vec![
        text(title, theme::FOREGROUND, 14.0),
        subtle(description),
    ])
    .gap(6.0);
    if compact {
        Element::column(vec![copy, trailing]).gap(12.0)
    } else {
        Element::row(vec![copy.flex(1.0), trailing])
            .gap(24.0)
            .min_size(0.0, 60.0)
    }
}

fn nav_item(page: SettingsPage, active: bool) -> Element {
    let fill = if active {
        theme::SELECTED
    } else {
        theme::SIDEBAR
    };
    let color = if active {
        theme::FOREGROUND
    } else {
        theme::FOREGROUND_SUBTLE
    };
    UiIconButton::new(
        page.action_id(),
        page.icon(),
        theme::action_palette(fill, color),
    )
    .label(page.nav_title())
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Spacious)
    .active(active)
    .render()
    .font_size(theme::UI_FONT_SIZE)
    .radius(9.0)
    .size(None, Some(40.0))
}

fn project_name(workspace: &str) -> &str {
    Path::new(workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Proyecto")
}

pub fn build_settings_sidebar(state: &ViewState, width: f32) -> Element {
    let back = UiIconButton::new(
        "toggle_settings",
        Icon::ArrowLeft,
        theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
    )
    .label("Volver a OpenIDE")
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Spacious)
    .render()
    .font_size(theme::UI_FONT_SIZE)
    .radius(9.0)
    .size(None, Some(40.0));

    let mut navigation_items = Vec::new();
    for (index, (title, pages)) in SETTINGS_GROUPS.iter().enumerate() {
        if index > 0 {
            navigation_items.push(Element::spacer().size(None, Some(14.0)));
        }
        navigation_items.push(section(title));
        navigation_items.extend(
            pages
                .iter()
                .map(|page| nav_item(*page, state.settings_page == *page)),
        );
    }
    let navigation = ScrollArea::new(
        "settings_navigation",
        Element::column(navigation_items).gap(4.0).padding(4.0),
    )
    .tint(theme::FOREGROUND_SUBTLE)
    .render()
    .flex(1.0);

    Element::column(vec![
        Element::row(vec![
            Element::icon(Icon::Settings)
                .foreground(theme::FOREGROUND)
                .size(Some(16.0), Some(16.0)),
            text("Configuración", theme::FOREGROUND, 17.0),
        ])
        .gap(12.0)
        .padding(10.0)
        .size(None, Some(48.0)),
        divider(),
        Element::spacer().size(None, Some(8.0)),
        back,
        divider(),
        navigation,
        divider(),
        view::sidebar_account_footer(state, width - 16.0),
    ])
    .gap(4.0)
    .padding(8.0)
    .size(Some(width), None)
    .background(theme::SIDEBAR)
    .border(theme::BORDER, 1.0)
    .radius(12.0)
}

fn general_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let path = field("workspace", &state.workspace_draft, "Ruta del proyecto")
        .font_family(FontFamily::Monospace);
    let open =
        action_button("workspace_apply", "Abrir proyecto", Icon::Folder).size(Some(150.0), None);
    let project_control = if compact {
        Element::column(vec![path, open]).gap(10.0)
    } else {
        Element::row(vec![path.flex(1.0), open]).gap(10.0)
    };
    vec![
        group(
            "Espacio de trabajo",
            vec![
                setting_row(
                    "Proyecto activo",
                    &state.workspace,
                    value_badge(project_name(&state.workspace)),
                    compact,
                ),
                divider(),
                Element::column(vec![
                    text("Cambiar carpeta", theme::FOREGROUND, 14.0),
                    subtle("Selecciona una ruta local para trabajar en otro proyecto."),
                    project_control,
                ])
                .gap(10.0),
                divider(),
                setting_row(
                    "Reabrir el último proyecto",
                    "Al iniciar OpenIDE, vuelve a la última carpeta que abriste.",
                    Switch::new(
                        "settings_reopen_last_workspace",
                        state.settings_reopen_last_workspace,
                        theme::component_palette(),
                    )
                    .render(),
                    compact,
                ),
            ],
        ),
        group(
            "Interfaz",
            vec![
                setting_row(
                    "Apariencia",
                    "Tema oscuro y movimiento de la interfaz.",
                    action_button(
                        "settings_page:appearance",
                        "Abrir apariencia",
                        Icon::ArrowRight,
                    ),
                    compact,
                ),
                divider(),
                setting_row(
                    "Navegación",
                    "Organiza tus tareas por proyecto, grupo o actividad en Datos y sesiones.",
                    action_button("settings_page:data", "Abrir datos", Icon::ArrowRight),
                    compact,
                ),
            ],
        ),
    ]
}

fn agent_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let backend_name = state
        .backend_options
        .iter()
        .find(|(id, _)| id == &state.selected_backend)
        .map(|(_, name)| name.as_str())
        .unwrap_or("Sin agente seleccionado");
    let write_mode = Element::row(vec![
        value_badge(if state.workspace_writes_enabled {
            "Ambos activos"
        } else if state.native_tool_permissions.write_file
            || state.native_tool_permissions.run_command
        {
            "Personalizado"
        } else {
            "Sólo lectura"
        }),
        Switch::new(
            "write_mode",
            state.workspace_writes_enabled,
            theme::component_palette(),
        )
        .render(),
    ])
    .gap(12.0);
    let isolated = state.native_command_isolation == CommandIsolation::WorkspaceWriteOffline;
    vec![
        group(
            "Motor activo",
            vec![
                setting_row(
                    "Agente de la conversación",
                    "Se elige desde el composer. Cada CLI externa gestiona sus credenciales y permisos.",
                    value_badge(backend_name),
                    compact,
                ),
                divider(),
                setting_row(
                    "Modelo del motor Rust",
                    &format!("{} · {}", state.native_provider_id, state.model),
                    action_button("settings_page:providers", "Configurar modelos", Icon::ArrowRight),
                    compact,
                ),
            ],
        ),
        group(
            "Permisos del harness nativo",
            vec![
                setting_row(
                    "Escritura y comandos",
                    "Activa o desactiva escritura y comandos juntos. Cada ejecución solicita aprobación.",
                    write_mode,
                    compact,
                ),
                divider(),
                subtle("Estas preferencias se guardan para el agente nativo. Las CLI externas usan su propia política; las automatizaciones siguen en sólo lectura."),
            ],
        ),
        group(
            "Herramientas individuales",
            vec![
                tool_setting(state, LocalTool::ReadFile, "Leer archivos", "Permite leer texto dentro del proyecto.", compact),
                divider(),
                tool_setting(state, LocalTool::ListFiles, "Listar archivos", "Permite explorar carpetas dentro del proyecto.", compact),
                divider(),
                tool_setting(state, LocalTool::SearchFiles, "Buscar archivos", "Permite buscar contenido dentro del proyecto.", compact),
                divider(),
                tool_setting(state, LocalTool::WriteFile, "Escribir archivos", "Solicita aprobación para cada cambio de archivo.", compact),
                divider(),
                tool_setting(state, LocalTool::RunCommand, "Ejecutar comandos", "Solicita aprobación para cada comando.", compact),
            ],
        ),
        group(
            "Aislamiento de comandos",
            vec![
                setting_row(
                    "Modo actual",
                    "Se aplica a Ejecutar comandos del agente nativo. Cada comando sigue solicitando aprobación.",
                    value_badge(if isolated { "Proyecto, sin red" } else { "Sin aislamiento" }),
                    compact,
                ),
                divider(),
                subtle("Proyecto, sin red: permite escribir sólo dentro del proyecto y bloquea la red local y externa. Disponible en Windows."),
                subtle("Sin aislamiento: el comando usa los permisos del usuario del equipo para archivos y red."),
                choice_group(
                    vec![
                        isolation_choice(
                            "command_isolation:workspace_write_offline",
                            "Proyecto, sin red",
                            isolated,
                            cfg!(windows),
                        ),
                        isolation_choice(
                            "command_isolation:host_unrestricted",
                            "Sin aislamiento",
                            !isolated,
                            true,
                        ),
                    ],
                    compact,
                ),
            ],
        ),
        // Keep the final choices above the scroll viewport edge on short windows.
        Element::spacer().size(None, Some(64.0)),
    ]
}

fn tool_setting(
    state: &ViewState,
    tool: LocalTool,
    title: &str,
    description: &str,
    compact: bool,
) -> Element {
    setting_row(
        title,
        description,
        Switch::new(
            format!("agent_tool:{}", tool.name()),
            state.native_tool_permissions.allows(tool),
            theme::component_palette(),
        )
        .render(),
        compact,
    )
}

fn skills_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let installed = state
        .marketplace
        .snapshot
        .items
        .iter()
        .filter(|item| item.installed_scope.is_some())
        .count();
    let enabled = state
        .marketplace
        .snapshot
        .items
        .iter()
        .filter(|item| item.installed_scope.is_some() && item.enabled)
        .count();
    let local = &state.native_skills;
    let mut content = Vec::new();
    if let Some(error) = &local.load_error {
        content.push(settings_parts::feedback_banner(
            format!("No se pudo cargar la selección de skills locales: {error}"),
            FeedbackTone::Error,
            None,
            compact,
        ));
        content.push(group(
            "Recuperar configuración",
            vec![setting_row(
                "Restablecer selección",
                "Reemplaza el archivo inválido y borra las selecciones de todos los proyectos tras confirmar.",
                action_button("native_skill_reset", "Restablecer", Icon::Trash),
                compact,
            )],
        ));
    }
    if let Some(error) = &local.catalog_error {
        content.push(settings_parts::feedback_banner(
            format!("No se pudo validar el catálogo de skills locales: {error}"),
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    if let Some(error) = &local.activation_error {
        content.push(settings_parts::feedback_banner(
            format!("La selección guardada requiere revisión: {error}"),
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    let mut selected_rows = vec![setting_row(
        "Proyecto actual",
        &local.workspace,
        value_badge(format!("{} seleccionadas", local.selected.len())),
        compact,
    )];
    if local.selected.is_empty() {
        selected_rows.push(divider());
        selected_rows.push(subtle(
            "No hay skills locales seleccionadas para este proyecto.",
        ));
    }
    for (index, selection) in local.selected.iter().enumerate() {
        selected_rows.push(divider());
        let exact = local
            .available
            .iter()
            .any(|entry| entry.selection == *selection);
        selected_rows.push(setting_row(
            &selection.name,
            if exact {
                "Selección verificada con el documento actual."
            } else {
                "El documento cambió, desapareció o el catálogo no está disponible."
            },
            mcp_action_button(
                &format!("native_skill_remove:{index}"),
                "Quitar",
                Icon::Trash,
                local.load_error.is_none() && local.catalog_error.is_none(),
            ),
            compact,
        ));
    }
    content.push(group("Selección local para el agente Rust", selected_rows));
    let mut catalog_rows = vec![setting_row(
        "Catálogo local",
        "Se vuelve a leer y verificar antes de cada turno. Ninguna skill se activa sólo por estar presente.",
        mcp_action_button(
            "native_skill_refresh",
            "Actualizar lista",
            Icon::Refresh,
            local.load_error.is_none(),
        ),
        compact,
    )];
    if local.available.is_empty() {
        catalog_rows.push(divider());
        catalog_rows.push(subtle(
            "No se encontraron skills locales en las raíces indicadas.",
        ));
    }
    for (index, entry) in local.available.iter().enumerate() {
        catalog_rows.push(divider());
        let selected = local
            .selected
            .iter()
            .find(|selection| selection.name.eq_ignore_ascii_case(&entry.name));
        let (label, status) = match selected {
            Some(selection) if *selection == entry.selection => ("Desactivar", "Seleccionada"),
            Some(_) => ("Aceptar cambios", "Cambió"),
            None => ("Seleccionar", "Disponible"),
        };
        let origin = match entry.origin {
            SkillOrigin::Project => "Proyecto",
            SkillOrigin::User => "Usuario",
        };
        catalog_rows.push(
            Element::column(vec![
                Element::row(vec![
                    text(&entry.name, theme::FOREGROUND, 13.0).flex(1.0),
                    value_badge(format!("{origin} · {status}")),
                ])
                .gap(8.0),
                subtle(&entry.description),
                subtle(entry.document.display().to_string()),
                mcp_action_button(
                    &format!("native_skill_toggle:{index}"),
                    label,
                    Icon::ArrowRight,
                    local.load_error.is_none() && local.catalog_error.is_none(),
                ),
            ])
            .gap(8.0),
        );
    }
    content.push(group("Disponibles", catalog_rows));
    content.push(group(
        "Raíces autorizadas",
        vec![
            subtle(format!("Proyecto: {}", local.project_root)),
            subtle(format!("Usuario: {}", local.user_root)),
            subtle("Los documentos y archivos referenciados no se ejecutan automáticamente. Cada selección guarda el nombre, la ruta canónica y el hash del documento."),
        ],
    ));
    content.extend([
        group(
            "Skills del Marketplace",
            vec![
                setting_row(
                    "Instaladas en OpenIDE",
                    "Se pueden activar por usuario o por proyecto desde Marketplace.",
                    value_badge(format!("{enabled} activas · {installed} instaladas")),
                    compact,
                ),
                divider(),
                setting_row(
                    "Administrar skills",
                    "Abre el catálogo local y las fuentes GitHub ya configuradas.",
                    action_button("open_marketplace", "Abrir Marketplace", Icon::ArrowRight),
                    compact,
                ),
            ],
        ),
        group(
            "Alcance actual",
            vec![subtle(
                "El Marketplace conserva sus propias skills instaladas. Los servidores MCP se configuran en su propia sección.",
            )],
        ),
    ]);
    content
}

fn automations_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let total = state.automations.items.len();
    let active = state
        .automations
        .items
        .iter()
        .filter(|item| item.enabled)
        .count();
    vec![
        group(
            "Tareas programadas",
            vec![
                setting_row(
                    "Automatizaciones de este proyecto",
                    "Crea, edita, pausa y ejecuta tareas con horarios locales.",
                    value_badge(format!("{active} activas · {total} en total")),
                    compact,
                ),
                divider(),
                setting_row(
                    "Administrar automatizaciones",
                    "Abre el editor y el historial de ejecuciones del proyecto.",
                    action_button("open_automations", "Abrir automatizaciones", Icon::ArrowRight),
                    compact,
                ),
            ],
        ),
        group(
            "Ejecución",
            vec![subtle(
                "El planificador actual revisa los horarios mientras OpenIDE está abierto. Las tareas no se ejecutan con la aplicación cerrada.",
            )],
        ),
    ]
}

fn data_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let organization = choice_group(
        vec![
            choice_button(
                "session_organize:project",
                "Proyecto",
                state.session_organization == SessionOrganization::Project,
            ),
            choice_button(
                "session_organize:group",
                "Grupo",
                state.session_organization == SessionOrganization::Group,
            ),
            choice_button(
                "session_organize:recent",
                "Recientes",
                state.session_organization == SessionOrganization::Recent,
            ),
        ],
        compact,
    );
    let sorting = choice_group(
        vec![
            choice_button(
                "session_sort:updated",
                "Actualizadas",
                state.session_sort == SessionSort::Updated,
            ),
            choice_button(
                "session_sort:created",
                "Creadas",
                state.session_sort == SessionSort::Created,
            ),
        ],
        compact,
    );
    vec![
        group(
            "Historial local",
            vec![
                setting_row(
                    "Sesiones visibles",
                    "Las tareas, eventos y borradores se guardan en el equipo.",
                    value_badge(format!("{} tareas", state.sessions.len())),
                    compact,
                ),
                divider(),
                subtle("Archivar y eliminar tareas se gestiona desde su menú en la barra lateral, con confirmación para la eliminación."),
            ],
        ),
        group(
            "Organización de la barra lateral",
            vec![
                text("Agrupar por", theme::FOREGROUND, 14.0),
                subtle("Esta preferencia se conserva al reiniciar OpenIDE."),
                organization,
                divider(),
                text("Ordenar por", theme::FOREGROUND, 14.0),
                sorting,
            ],
        ),
    ]
}

fn github_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let (status, detail, actions) = match &state.github_account {
        GithubAccountStatus::Connected(identity) => (
            format!("Conectado como {}", identity.login),
            "La cuenta verificada se usa para el perfil y las consultas GitHub del proyecto."
                .to_owned(),
            vec![action_button(
                "github_account_open",
                "Abrir perfil",
                Icon::ArrowRight,
            )],
        ),
        GithubAccountStatus::Unverified { accounts, notice } => (
            "Cuenta por verificar".to_owned(),
            notice.clone().unwrap_or_else(|| {
                "Elige una cuenta detectada para verificarla con GitHub.".to_owned()
            }),
            accounts
                .iter()
                .map(|account| {
                    action_button(
                        &format!("github_account_verify:{account}"),
                        &format!("Verificar {account}"),
                        Icon::Check,
                    )
                })
                .collect(),
        ),
        GithubAccountStatus::Checking
        | GithubAccountStatus::Connecting
        | GithubAccountStatus::Verifying(_) => (
            "Comprobando conexión".to_owned(),
            "La verificación se ejecuta fuera del hilo visual.".to_owned(),
            Vec::new(),
        ),
        GithubAccountStatus::Disconnected => (
            "Sin conexión".to_owned(),
            "La conexión abre el flujo de Git Credential Manager cuando está disponible."
                .to_owned(),
            vec![action_button(
                "github_account_connect",
                "Conectar GitHub",
                Icon::Github,
            )],
        ),
        GithubAccountStatus::Unavailable(error) => (
            "No disponible".to_owned(),
            error.clone(),
            vec![action_button(
                "github_account_connect",
                "Reintentar conexión",
                Icon::Refresh,
            )],
        ),
    };
    let mut rows = vec![setting_row(
        "Estado de GitHub",
        &detail,
        value_badge(status),
        compact,
    )];
    if !actions.is_empty() {
        rows.push(divider());
        rows.push(choice_group(actions, compact));
    }
    vec![
        group("Cuenta", rows),
        group(
            "Integración actual",
            vec![subtle("GitHub permite consultar el repositorio y los pull requests asociados al proyecto. Las cuentas de proveedores de modelos se configuran por separado en Proveedores y modelos.")],
        ),
    ]
}

pub(crate) fn catalog_model_usable(
    _provider_id: &str,
    model: &crate::model_catalog::ModelInfo,
) -> bool {
    !model.id.is_empty()
        && model.tool_call_declared != Some(false)
        && model.status.as_deref() != Some("deprecated")
        && model.modalities.as_ref().is_none_or(|modalities| {
            (modalities.input.is_empty() || modalities.input.iter().any(|kind| kind == "text"))
                && (modalities.output.is_empty()
                    || modalities.output.iter().any(|kind| kind == "text"))
        })
}

fn providers_page(state: &ViewState, compact: bool, center_width: f32) -> Vec<Element> {
    let active_name = if state.native_provider_id == "custom" {
        "Conexión personalizada".to_owned()
    } else {
        state
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.providers.get(&state.native_provider_id))
            .map(|provider| provider.name.clone())
            .or_else(|| {
                native_provider::adapter(&state.native_provider_id)
                    .map(|adapter| adapter.name.to_owned())
            })
            .unwrap_or_else(|| state.native_provider_id.clone())
    };
    let key_name = state
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.providers.get(&state.native_provider_id))
        .and_then(|provider| provider.env.first().cloned())
        .or_else(|| {
            native_provider::adapter(&state.native_provider_id).map(|adapter| adapter.key_env)
        })
        .unwrap_or_else(|| "OPENIDE_API_KEY".to_owned());
    let mut sections = vec![group(
        "Selección activa",
        vec![
            setting_row(
                "Proveedor",
                "Se usa en los próximos turnos del harness nativo.",
                value_badge(active_name),
                compact,
            ),
            divider(),
            setting_row(
                "Modelo",
                "La selección guarda el identificador exacto del proveedor.",
                value_badge(&state.model),
                compact,
            ),
            divider(),
            setting_row(
                "Credencial",
                &format!("Variable de entorno: {key_name}"),
                value_badge(if state.native_key_available {
                    "Detectada"
                } else {
                    "No detectada"
                }),
                compact,
            ),
            subtle("La presencia de la clave no comprueba la autenticación; la conexión se verifica al ejecutar el modelo."),
        ],
    )];

    if state.native_provider_id == "openai" {
        let choices = [
            ("off", "Desactivada", None),
            ("cached", "Caché", Some(HostedWebSearchMode::Cached)),
            ("indexed", "Índice", Some(HostedWebSearchMode::Indexed)),
            ("live", "Web en vivo", Some(HostedWebSearchMode::Live)),
        ];
        let buttons = choices
            .into_iter()
            .map(|(id, label, mode)| {
                Button::new(
                    format!("native_web_search:{id}"),
                    label,
                    theme::action_palette(theme::CARD, theme::FOREGROUND),
                )
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Regular)
                .enabled(!state.running)
                .active(state.hosted_web_search_mode == mode)
                .render()
            })
            .collect();
        sections.push(group(
            "Búsqueda web alojada",
            vec![
                subtle("Disponible para Responses de OpenAI. El modo elegido se fija en el turno y su historial para Resume."),
                Element::row(buttons).gap(8.0),
            ],
        ));
    }

    let custom_button = Button::new(
        "catalog_select_custom",
        "Usar conexión personalizada",
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Regular)
    .active(state.native_provider_id == "custom")
    .render();
    let mut custom = vec![
        subtle(
            "Endpoint OpenAI compatible configurado por ti. La clave se lee de OPENIDE_API_KEY.",
        ),
        custom_button,
    ];
    if state.native_provider_id == "custom" {
        custom.push(field("model", &state.model, "ID exacto del modelo"));
        custom.push(
            field("endpoint", &state.endpoint, "https://.../v1").font_family(FontFamily::Monospace),
        );
    }
    sections.push(group("Conexión personalizada", custom));

    let refresh = Button::new(
        "catalog_refresh",
        if state.catalog_refreshing {
            "Actualizando..."
        } else {
            "Actualizar catálogo"
        },
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .icon(Icon::Refresh)
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Regular)
    .enabled(!state.catalog_refreshing)
    .render();
    let mut catalog_header = vec![
        subtle("Fuente de descubrimiento: las APIs oficiales de cada proveedor, con copia local de OpenIDE. La lista de modelos no verifica por sí sola herramientas, acceso de la cuenta ni límites para el agente nativo."),
        setting_row(
            "Estado del catálogo",
            &state.catalog_status,
            refresh,
            compact,
        ),
        field(
            "catalog_provider_query",
            &state.catalog_provider_query,
            "Buscar proveedor",
        ),
    ];
    if let Some(catalog) = &state.catalog {
        let query = state.catalog_provider_query.trim().to_lowercase();
        let mut providers: Vec<_> = catalog
            .providers
            .values()
            .filter(|provider| {
                query.is_empty()
                    || provider.name.to_lowercase().contains(&query)
                    || provider.id.to_lowercase().contains(&query)
            })
            .collect();
        providers.sort_by(|left, right| {
            let left_selected = left.id == state.catalog_browse_provider;
            let right_selected = right.id == state.catalog_browse_provider;
            right_selected
                .cmp(&left_selected)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        let count = providers.len();
        for provider in providers.into_iter().take(18) {
            let title = format!(
                "{} · {} modelos · Conexión nativa",
                provider.name,
                provider.models.len(),
            );
            catalog_header.push(
                Button::new(
                    format!("catalog_browse:{}", provider.id),
                    title,
                    theme::action_palette(theme::CARD, theme::FOREGROUND),
                )
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Regular)
                .active(state.catalog_browse_provider == provider.id)
                .width(if center_width >= 800.0 {
                    210.0
                } else {
                    (center_width - 120.0).clamp(180.0, 700.0)
                })
                .render(),
            );
        }
        if count > 18 {
            catalog_header.push(subtle(format!(
                "Mostrando 18 de {count} proveedores; escribe para filtrar."
            )));
        }
    }
    let mut models_group = None;
    if let Some(provider) = state
        .catalog
        .as_ref()
        .and_then(|catalog| catalog.providers.get(&state.catalog_browse_provider))
    {
        let mut models = vec![
            subtle(format!(
                "{} · {} modelos · Conexión nativa disponible",
                provider.name,
                provider.models.len(),
            )),
            field(
                "catalog_model_query",
                &state.catalog_model_query,
                "Buscar modelo",
            ),
        ];
        let query = state.catalog_model_query.trim().to_lowercase();
        let mut candidates: Vec<_> = provider
            .models
            .values()
            .filter(|model| {
                query.is_empty()
                    || model.name.to_lowercase().contains(&query)
                    || model.id.to_lowercase().contains(&query)
            })
            .collect();
        candidates.sort_by(|left, right| {
            let left_active = state.native_provider_id == provider.id && left.id == state.model;
            let right_active = state.native_provider_id == provider.id && right.id == state.model;
            right_active.cmp(&left_active).then_with(|| {
                right
                    .last_updated
                    .cmp(&left.last_updated)
                    .then_with(|| left.name.cmp(&right.name))
            })
        });
        let count = candidates.len();
        for model in candidates.into_iter().take(24) {
            models.push(divider());
            let reviewed = native_provider::native_model_capabilities(&provider.id, &model.id);
            let context = reviewed
                .and_then(|capabilities| capabilities.context_window_tokens)
                .map(|value| format!("{value} tokens de contexto verificados"))
                .or_else(|| {
                    model
                        .limit
                        .as_ref()
                        .and_then(|limit| limit.context)
                        .map(|value| format!("catálogo: {value} tokens de contexto sin verificar"))
                })
                .unwrap_or_else(|| "Contexto sin verificar".to_owned());
            let price = model
                .cost
                .as_ref()
                .and_then(|cost| cost.input.zip(cost.output))
                .map(|(input, output)| format!("${input:.2}/${output:.2} por millón de tokens"))
                .unwrap_or_else(|| "Precio no informado".to_owned());
            let eligible = catalog_model_usable(&provider.id, model);
            let availability = if eligible && reviewed.is_some() {
                "Seleccionable para el agente nativo"
            } else if eligible {
                "Seleccionable; ejecución y límites sin verificar"
            } else if model.tool_call_declared == Some(false) {
                "El proveedor no declara herramientas para este modelo"
            } else {
                "Modalidad o estado no compatible"
            };
            let updated = model.last_updated.as_deref().unwrap_or("sin fecha");
            let source = state
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.provider_status.get(&provider.id))
                .map_or("Base local revisada", |status| {
                    if status.source == crate::model_catalog::CatalogSource::OfficialApi {
                        "API del proveedor"
                    } else {
                        "Base local revisada"
                    }
                });
            let capabilities = format!(
                "{source}: herramientas {} · razonamiento {}",
                match model.tool_call_declared {
                    Some(true) => "sí",
                    Some(false) => "no",
                    None => "sin dato",
                },
                if model.reasoning { "sí" } else { "sin dato" }
            );
            let button = Button::new(
                format!("catalog_select:{}/{}", provider.id, model.id),
                if eligible { "Usar" } else { "No disponible" },
                theme::action_palette(theme::CARD, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Regular)
            .enabled(eligible && !state.running)
            .active(state.native_provider_id == provider.id && state.model == model.id)
            .render();
            models.push(setting_row(
                &model.name,
                &format!(
                    "{} · {} · {} · actualizado {} · {} · {}",
                    model.id, context, price, updated, capabilities, availability
                ),
                button,
                compact || center_width < 720.0,
            ));
        }
        if count > 24 {
            models.push(subtle(format!(
                "Mostrando 24 de {count} modelos; escribe para filtrar."
            )));
        }
        if count == 0 {
            models.push(subtle("No hay modelos para esta búsqueda."));
        }
        models_group = Some(group("Modelos del proveedor", models));
        if state.native_provider_id == provider.id {
            if let Some(model) = provider.models.get(&state.model) {
                let modalities = model.modalities.as_ref().map_or_else(
                    || "Sin datos".to_owned(),
                    |modalities| {
                        format!(
                            "{} → {}",
                            modalities.input.join(", "),
                            modalities.output.join(", ")
                        )
                    },
                );
                let limit = model.limit.as_ref().map_or_else(
                    || "Sin datos".to_owned(),
                    |limit| {
                        format!(
                            "contexto {} · entrada {} · salida {}",
                            limit
                                .context
                                .map_or("?".to_owned(), |value| value.to_string()),
                            limit
                                .input
                                .map_or("?".to_owned(), |value| value.to_string()),
                            limit
                                .output
                                .map_or("?".to_owned(), |value| value.to_string())
                        )
                    },
                );
                sections.insert(
                    1,
                    group(
                        "Ficha del modelo activo",
                        vec![
                            text(&model.name, theme::FOREGROUND, 16.0),
                            subtle(model.description.as_deref().unwrap_or("Sin descripción")),
                            divider(),
                            subtle(format!("ID: {} / {}", provider.id, model.id)),
                            subtle(format!("Modalidades: {modalities}")),
                            subtle(format!("Límites del catálogo: {limit}")),
                            subtle(match native_provider::native_model_capabilities(
                                &provider.id,
                                &model.id,
                            ) {
                                Some(capabilities) => format!(
                                    "Límites verificados: contexto {} · salida {} · {}",
                                    capabilities.context_window_tokens.unwrap_or_default(),
                                    capabilities.max_output_tokens.unwrap_or_default(),
                                    capabilities.source_url
                                ),
                                None => "Límites nativos sin verificar; compactación automática no disponible".to_owned(),
                            }),
                            subtle(format!(
                                "Familia: {} · Estado: {}",
                                model.family.as_deref().unwrap_or("Sin datos"),
                                model.status.as_deref().unwrap_or("Activo")
                            )),
                            subtle(format!(
                                "Publicado: {} · Actualizado: {}",
                                model.release_date.as_deref().unwrap_or("Sin datos"),
                                model.last_updated.as_deref().unwrap_or("Sin datos")
                            )),
                        ],
                    ),
                );
            } else {
                sections.insert(1, group(
                    "Modelo activo fuera del catálogo",
                    vec![subtle(
                        "La selección guardada ya no aparece en la copia actual. Revísala antes de enviar otro turno.",
                    )],
                ));
            }
        }
    }
    let catalog_group = group("Catálogo de proveedores", catalog_header);
    let models_group = models_group.unwrap_or_else(|| {
        group(
            "Modelos del proveedor",
            vec![subtle(
                "Selecciona un proveedor del catálogo para ver sus modelos.",
            )],
        )
    });
    if center_width >= 800.0 {
        sections.push(
            Element::row(vec![
                catalog_group.size(Some(260.0), None),
                models_group.flex(1.0),
            ])
            .gap(16.0),
        );
    } else {
        sections.push(catalog_group);
        sections.push(models_group);
    }
    if state.catalog_refreshing {
        sections.insert(
            1,
            settings_parts::loading_state(
                "Actualizando catálogo",
                "Se comprueban los proveedores y modelos sin bloquear la interfaz.",
            ),
        );
    } else if state.catalog.is_none() {
        let status = state.catalog_status.to_lowercase();
        if status.contains("error") || status.contains("no se pudo") {
            sections.insert(
                1,
                settings_parts::error_state("Catálogo no disponible", &state.catalog_status, None),
            );
        }
    }
    sections
}

fn shortcut_row(description: &str, keys: &str) -> Element {
    Element::row(vec![
        text(description, theme::FOREGROUND, 14.0).flex(1.0),
        Kbd::new(keys, theme::component_palette()).render(),
    ])
    .gap(16.0)
    .min_size(0.0, 40.0)
}

fn shortcuts_page() -> Vec<Element> {
    vec![
        group(
            "Navegación",
            vec![
                shortcut_row("Nueva tarea", "Ctrl+N"),
                divider(),
                shortcut_row("Buscar acciones, tareas y archivos", "Ctrl+K"),
                divider(),
                shortcut_row("Mostrar u ocultar la barra lateral", "Ctrl+B"),
                divider(),
                shortcut_row("Volver", "Alt+←"),
                divider(),
                shortcut_row("Avanzar", "Alt+→"),
            ],
        ),
        group(
            "Composer y diálogos",
            vec![
                shortcut_row("Enviar mensaje", "Ctrl+Enter"),
                divider(),
                shortcut_row("Elegir esfuerzo del modelo", "Ctrl+Shift+M"),
                divider(),
                shortcut_row("Cerrar búsqueda", "Esc"),
            ],
        ),
        group("Alcance actual", vec![subtle("Estos atajos están implementados en la ventana nativa. La edición de combinaciones todavía no está disponible.")]),
    ]
}

fn capability_page(
    title: &str,
    current: &str,
    needed: &str,
    related: Option<(&str, &str, Icon)>,
) -> Vec<Element> {
    let related = related.map(|(id, label, icon)| action_button(id, label, icon));
    vec![
        settings_parts::empty_state(title, current, related),
        group("Para habilitar esta capacidad", vec![subtle(needed)]),
    ]
}

fn mcp_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let editable = state.mcp_load_error.is_none();
    let mut content = Vec::new();
    if let Some(error) = &state.mcp_load_error {
        content.push(settings_parts::feedback_banner(
            format!("No se pudo cargar la configuración MCP: {error}"),
            FeedbackTone::Error,
            None,
            compact,
        ));
    }

    let mut servers = vec![setting_row(
        "Servidores guardados",
        &format!("{} configurados en este equipo", state.mcp_servers.len()),
        mcp_action_button("mcp_new", "Nuevo servidor", Icon::Add, editable),
        compact,
    )];
    if state.mcp_servers.is_empty() {
        servers.push(divider());
        servers.push(subtle(
            "Todavía no hay servidores MCP configurados en este equipo.",
        ));
    }
    for server in &state.mcp_servers {
        servers.push(divider());
        let status = if server.enabled {
            "Activado"
        } else {
            "Desactivado"
        };
        let toggle_label = if server.enabled {
            "Desactivar"
        } else {
            "Activar"
        };
        let mut controls = vec![
            mcp_action_button(
                &format!("mcp_select:{}", server.id),
                "Editar",
                Icon::Settings,
                editable,
            ),
            mcp_action_button(
                &format!("mcp_toggle:{}", server.id),
                toggle_label,
                Icon::Check,
                editable,
            ),
            mcp_action_button(
                &format!("mcp_delete:{}", server.id),
                "Eliminar",
                Icon::Trash,
                editable,
            ),
        ];
        let mut server_content = vec![
            Element::row(vec![
                text(&server.id, theme::FOREGROUND, 14.0).flex(1.0),
                value_badge(status),
            ])
            .gap(10.0),
            subtle(match &server.url {
                Some(url) => format!("Streamable HTTP 2026-07-28: {url}"),
                None => format!("Programa: {}", server.program),
            }),
        ];
        if let Some(oauth) = &server.http_oauth {
            server_content.push(subtle(format!(
                "OAuth 2.1 · issuer: {} · scopes: {}",
                oauth.issuer,
                if oauth.scopes.is_empty() {
                    "los solicitados por el servidor".to_owned()
                } else {
                    oauth.scopes.join(", ")
                }
            )));
            let oauth_busy = state.mcp_oauth_status.as_ref().is_some_and(|status| {
                status.server_id == server.id
                    && matches!(
                        status.phase,
                        McpOAuthViewPhase::Discovering | McpOAuthViewPhase::AwaitingBrowser { .. }
                    )
            });
            controls.push(mcp_action_button(
                &format!("mcp_oauth_authorize:{}", server.id),
                "Autorizar",
                Icon::ArrowRight,
                editable && cfg!(windows) && !oauth_busy,
            ));
            if !cfg!(windows) {
                server_content.push(subtle("OAuth MCP requiere Credential Manager en Windows."));
            }
            if let Some(status) = state
                .mcp_oauth_status
                .as_ref()
                .filter(|status| status.server_id == server.id)
            {
                match &status.phase {
                    McpOAuthViewPhase::Discovering => {
                        server_content.push(subtle("Buscando el servidor de autorización…"));
                        controls.push(mcp_action_button(
                            "mcp_oauth_cancel", "Cancelar", Icon::Close, true,
                        ));
                    }
                    McpOAuthViewPhase::AwaitingBrowser { url } => {
                        server_content.push(subtle("Esperando autorización en el navegador. La URL es temporal:"));
                        server_content.push(
                            TextArea::new("mcp_oauth_url", url, "", theme::component_palette())
                                .width(if compact { 300.0 } else { 560.0 })
                                .height(90.0)
                                .disabled()
                                .render(),
                        );
                        controls.push(mcp_action_button(
                            "mcp_oauth_open", "Abrir autorización", Icon::ArrowRight, true,
                        ));
                        controls.push(mcp_action_button(
                            "mcp_oauth_cancel", "Cancelar", Icon::Close, true,
                        ));
                    }
                    McpOAuthViewPhase::Authorized => server_content.push(subtle(
                        "Autorización guardada en Credential Manager. El próximo turno usará esta credencial.",
                    )),
                    McpOAuthViewPhase::Failed(message) => server_content.push(
                        settings_parts::feedback_banner(message, FeedbackTone::Error, None, compact),
                    ),
                }
            }
        }
        server_content.push(if compact {
            Element::column(controls).gap(6.0)
        } else {
            Element::row(controls).gap(8.0)
        });
        servers.push(Element::column(server_content).gap(9.0));
    }
    content.push(group("Servidores MCP", servers));

    content.push(group(
        "Uso",
        vec![subtle("Los servidores activados se conectan al ejecutar el agente nativo en modo Construir. Sus herramientas solicitan aprobación antes de usarse. HTTP negocia las revisiones 2026-07-28 y 2025-11-25. OAuth requiere autorización explícita antes de usar el servidor.")],
    ));
    content
}

fn hooks_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let settings = &state.native_hooks;
    let editable = settings.load_error.is_none();
    let mut content = Vec::new();
    if let Some(error) = &settings.load_error {
        content.push(settings_parts::feedback_banner(
            format!("No se pudo cargar la configuración de hooks: {error}"),
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    let mut rows = vec![setting_row(
        "Hooks de comandos guardados",
        &format!("{} configurados en este equipo", settings.hooks.len()),
        mcp_action_button("hook_new", "Nuevo hook", Icon::Add, editable),
        compact,
    )];
    if settings.hooks.is_empty() {
        rows.push(divider());
        rows.push(subtle(
            "No hay hooks configurados. El agente nativo no ejecuta comandos automáticos.",
        ));
    }
    for hook in &settings.hooks {
        rows.push(divider());
        let phase = match hook.phase {
            HookPhase::PreToolUse => "Antes de herramienta",
            HookPhase::PostToolUse => "Después de herramienta",
        };
        let controls = vec![
            mcp_action_button(
                &format!("hook_select:{}", hook.id),
                "Editar",
                Icon::Settings,
                editable,
            ),
            mcp_action_button(
                &format!("hook_delete:{}", hook.id),
                "Eliminar",
                Icon::Trash,
                editable,
            ),
        ];
        rows.push(
            Element::column(vec![
                Element::row(vec![
                    text(&hook.id, theme::FOREGROUND, 14.0).flex(1.0),
                    value_badge(phase),
                ])
                .gap(10.0),
                subtle(format!(
                    "{} · {}",
                    hook.tool_name,
                    hook.executable.display()
                )),
                if compact {
                    Element::column(controls).gap(6.0)
                } else {
                    Element::row(controls).gap(8.0)
                },
            ])
            .gap(9.0),
        );
    }
    content.push(group("Hooks del agente nativo", rows));
    content.push(group("Alcance y seguridad", vec![
        subtle("PreToolUse puede permitir o bloquear una herramienta; PostToolUse observa el resultado y no revierte un efecto. Se ejecutan comandos locales configurados aquí, sin shell y con límites de tiempo y salida."),
        subtle("Sólo se aplican a turnos interactivos del agente Rust. Las CLI externas y las automatizaciones conservan su política independiente. Cambiar un hook puede impedir reanudar un turno anterior hasta restaurar la misma configuración."),
    ]));
    content
}

pub fn hook_editor_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    if !state.show_settings
        || state.settings_page != SettingsPage::Hooks
        || !state.native_hooks.editor_open
    {
        return base;
    }
    let draft = &state.native_hooks;
    let width = (viewport_width - 24.0).min(560.0);
    let body_width = (width - 54.0).max(0.0);
    let editing = draft.selected_id.is_some();
    let mut fields = Vec::new();
    if state.settings_notice_error {
        if let Some(error) = &state.settings_notice {
            fields.push(settings_parts::feedback_banner(
                error,
                FeedbackTone::Error,
                None,
                width < 420.0,
            ));
        }
    }
    fields.extend([
        text("ID del hook", theme::FOREGROUND, 13.0),
        field("hook_draft_id", &draft.draft_id, "ej. revisar-escritura")
            .font_family(FontFamily::Monospace).size(Some(body_width), Some(40.0)),
        text("Fase", theme::FOREGROUND, 13.0),
        choice_group(vec![
            choice_button("hook_phase:pre_tool_use", "Antes de herramienta", draft.draft_phase == HookPhase::PreToolUse),
            choice_button("hook_phase:post_tool_use", "Después de herramienta", draft.draft_phase == HookPhase::PostToolUse),
        ], width < 420.0),
        text("Nombre exacto de herramienta", theme::FOREGROUND, 13.0),
        subtle("Por ejemplo: read_file, write_file o mcp__servidor__herramienta. Los hooks sobre lecturas quedan excluidos del modo Plan."),
        field("hook_draft_tool_name", &draft.draft_tool_name, "ej. write_file")
            .font_family(FontFamily::Monospace).size(Some(body_width), Some(40.0)),
        text("Ejecutable", theme::FOREGROUND, 13.0),
        subtle("Ruta absoluta a un archivo existente. Se ejecuta sin shell; el agente no puede configurar esta ruta."),
        field("hook_draft_executable", &draft.draft_executable, "C:\\tools\\hook.exe")
            .font_family(FontFamily::Monospace).size(Some(body_width), Some(40.0)),
        text("Argumentos (JSON)", theme::FOREGROUND, 13.0),
        TextArea::new("hook_draft_args", &draft.draft_args, "[\"--check\"]", theme::component_palette())
            .width(body_width).height(80.0).render(),
        text("Directorio de trabajo", theme::FOREGROUND, 13.0),
        subtle("Ruta absoluta a una carpeta existente."),
        field("hook_draft_cwd", &draft.draft_cwd, "C:\\repo")
            .font_family(FontFamily::Monospace).size(Some(body_width), Some(40.0)),
        text("Límite de tiempo (ms)", theme::FOREGROUND, 13.0),
        field("hook_draft_timeout_ms", &draft.draft_timeout_ms, "5000")
            .font_family(FontFamily::Monospace).size(Some(body_width), Some(40.0)),
        text("Límite de salida (bytes)", theme::FOREGROUND, 13.0),
        field("hook_draft_max_output_bytes", &draft.draft_max_output_bytes, "4096")
            .font_family(FontFamily::Monospace).size(Some(body_width), Some(40.0)),
        subtle("El comando recibe JSON del evento por stdin. Puede tener efectos externos; sólo configura ejecutables de confianza."),
    ]);
    let cancel = Button::new("hook_cancel", "Cancelar", theme::component_palette())
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .render();
    let save = Button::new(
        "hook_save",
        if editing {
            "Guardar cambios"
        } else {
            "Agregar hook"
        },
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
    )
    .variant(ButtonVariant::Solid)
    .size(ControlSize::Compact)
    .enabled(draft.load_error.is_none())
    .render();
    let footer = if width < 350.0 {
        Element::column(vec![save, cancel]).gap(6.0)
    } else {
        Element::row(vec![Element::spacer().flex(1.0), cancel, save]).gap(8.0)
    };
    Modal::new(
        base,
        true,
        if editing { "Editar hook" } else { "Nuevo hook" },
        "Configura un comando para el agente nativo.",
        "hook_cancel",
        theme::component_palette(),
    )
    .width(width)
    .height(620.0)
    .body(
        Element::column(fields)
            .gap(7.0)
            .size(Some(body_width), None),
    )
    .footer(footer)
    .initial_focus("hook_draft_id")
    .return_focus(
        draft
            .selected_id
            .as_ref()
            .map(|id| format!("hook_select:{id}"))
            .unwrap_or_else(|| "hook_new".to_owned()),
    )
    .render()
}

fn exec_rules_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let settings = &state.native_exec_rules;
    let editable = settings.load_error.is_none();
    let mut content = Vec::new();
    if let Some(error) = &settings.load_error {
        content.push(settings_parts::feedback_banner(
            format!("No se pudo cargar las reglas de comandos: {error}"),
            FeedbackTone::Error,
            None,
            compact,
        ));
        content.push(group(
            "Recuperar configuración",
            vec![setting_row(
                "Restablecer reglas",
                "Reemplaza el archivo inválido por una política vacía después de tu confirmación.",
                action_button("exec_rule_reset", "Restablecer reglas", Icon::Trash),
                compact,
            )],
        ));
    }
    let mut rows = vec![setting_row(
        "Reglas de prefijo guardadas",
        &format!("{} configuradas en este equipo", settings.rules.len()),
        mcp_action_button("exec_rule_new", "Nueva regla", Icon::Add, editable),
        compact,
    )];
    if settings.rules.is_empty() {
        rows.push(divider());
        rows.push(subtle(
            "No hay reglas adicionales para comandos del agente.",
        ));
    }
    for (index, rule) in settings.rules.iter().enumerate() {
        rows.push(divider());
        let decision = match rule.decision() {
            ExecDecision::Ask => "Pedir aprobación",
            ExecDecision::Deny => "Bloquear",
            ExecDecision::Allow => "Permitir",
        };
        let pattern = serde_json::to_string(rule.pattern()).unwrap_or_default();
        let summary: String = pattern.chars().take(160).collect();
        let summary = if pattern.chars().count() > 160 {
            format!("{summary}…")
        } else {
            summary
        };
        let mut body = vec![Element::row(vec![
            text(summary, theme::FOREGROUND, 13.0)
                .font_family(FontFamily::Monospace)
                .flex(1.0),
            value_badge(decision),
        ])
        .gap(8.0)];
        if let Some(justification) = rule.justification() {
            body.push(subtle(justification));
        }
        let controls = vec![
            mcp_action_button(
                &format!("exec_rule_select:{index}"),
                "Editar",
                Icon::Settings,
                editable,
            ),
            mcp_action_button(
                &format!("exec_rule_delete:{index}"),
                "Eliminar",
                Icon::Trash,
                editable,
            ),
        ];
        body.push(if compact {
            Element::column(controls).gap(6.0)
        } else {
            Element::row(controls).gap(8.0)
        });
        rows.push(Element::column(body).gap(9.0));
    }
    content.push(group("Reglas para comandos del agente", rows));
    content.push(group(
        "Cómo se aplican",
        vec![
            subtle("Cada prefijo contiene el ejecutable y cero o más argumentos exactos. Una lista dentro de un token acepta cualquiera de esas alternativas. Si varias reglas coinciden, gana la más restrictiva."),
            subtle("Estas reglas pueden pedir aprobación o bloquear. No conceden permisos por encima de la política de herramientas. Los intérpretes y wrappers de shell se tratan de forma conservadora porque pueden ocultar otros comandos."),
            subtle("Los cambios afectan los próximos turnos. Un turno previo sólo se reanuda con la misma política registrada en su journal."),
        ],
    ));
    content
}

pub fn exec_rule_editor_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    if !state.show_settings
        || state.settings_page != SettingsPage::Commands
        || !state.native_exec_rules.editor_open
    {
        return base;
    }
    let draft = &state.native_exec_rules;
    let width = (viewport_width - 24.0).min(560.0);
    let body_width = (width - 54.0).max(0.0);
    let editing = draft.selected_index.is_some();
    let mut fields = Vec::new();
    if state.settings_notice_error {
        if let Some(error) = &state.settings_notice {
            fields.push(settings_parts::feedback_banner(
                error,
                FeedbackTone::Error,
                None,
                width < 420.0,
            ));
        }
    }
    fields.extend([
        text("Prefijo de comando (JSON)", theme::FOREGROUND, 13.0),
        subtle("Ejemplo: [\"git\",[\"push\",\"reset\"]]. El primer token es el ejecutable; los siguientes son argumentos exactos."),
        TextArea::new(
            "exec_rule_draft_pattern",
            &draft.draft_pattern,
            "[\"git\",\"push\"]",
            theme::component_palette(),
        )
        .width(body_width)
        .height(100.0)
        .render(),
        text("Decisión", theme::FOREGROUND, 13.0),
        choice_group(
            vec![
                choice_button(
                    "exec_rule_decision:ask",
                    "Pedir aprobación",
                    draft.draft_decision == ExecDecision::Ask,
                ),
                choice_button(
                    "exec_rule_decision:deny",
                    "Bloquear",
                    draft.draft_decision == ExecDecision::Deny,
                ),
            ],
            width < 420.0,
        ),
        text("Motivo (opcional)", theme::FOREGROUND, 13.0),
        TextArea::new(
            "exec_rule_draft_justification",
            &draft.draft_justification,
            "Explicá por qué se aplica esta regla",
            theme::component_palette(),
        )
        .width(body_width)
        .height(75.0)
        .render(),
    ]);
    let cancel = Button::new("exec_rule_cancel", "Cancelar", theme::component_palette())
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .render();
    let save = Button::new(
        "exec_rule_save",
        if editing {
            "Guardar cambios"
        } else {
            "Agregar regla"
        },
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
    )
    .variant(ButtonVariant::Solid)
    .size(ControlSize::Compact)
    .enabled(draft.load_error.is_none())
    .render();
    let footer = if width < 350.0 {
        Element::column(vec![save, cancel]).gap(6.0)
    } else {
        Element::row(vec![Element::spacer().flex(1.0), cancel, save]).gap(8.0)
    };
    Modal::new(
        base,
        true,
        if editing {
            "Editar regla"
        } else {
            "Nueva regla"
        },
        "Configura una decisión para un prefijo de comando del agente Rust.",
        "exec_rule_cancel",
        theme::component_palette(),
    )
    .width(width)
    .height(490.0)
    .body(
        Element::column(fields)
            .gap(7.0)
            .size(Some(body_width), None),
    )
    .footer(footer)
    .initial_focus("exec_rule_draft_pattern")
    .return_focus(
        draft
            .selected_index
            .map(|index| format!("exec_rule_select:{index}"))
            .unwrap_or_else(|| "exec_rule_new".to_owned()),
    )
    .render()
}

pub fn exec_rule_reset_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    if !state.show_settings
        || state.settings_page != SettingsPage::Commands
        || !state.native_exec_rules.reset_confirm_open
        || state.native_exec_rules.load_error.is_none()
    {
        return base;
    }
    let width = (viewport_width - 24.0).min(480.0);
    let cancel = Button::new(
        "exec_rule_reset_cancel",
        "Cancelar",
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Compact)
    .render();
    let confirm = Button::new(
        "exec_rule_reset_confirm",
        "Reemplazar archivo",
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
    )
    .variant(ButtonVariant::Solid)
    .size(ControlSize::Compact)
    .render();
    Modal::new(
        base,
        true,
        "Restablecer reglas",
        "El archivo de reglas actual no se puede cargar.",
        "exec_rule_reset_cancel",
        theme::component_palette(),
    )
    .width(width)
    .body(subtle(
        "Esta acción reemplaza el archivo de reglas por una política vacía. Los próximos turnos usarán los permisos generales de herramientas.",
    ))
    .footer(Element::row(vec![
        Element::spacer().flex(1.0),
        cancel,
        confirm,
    ]))
    .initial_focus("exec_rule_reset_cancel")
    .return_focus("exec_rule_reset")
    .render()
}

pub fn native_skill_reset_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    if !state.show_settings
        || state.settings_page != SettingsPage::Skills
        || !state.native_skills.reset_confirm_open
        || state.native_skills.load_error.is_none()
    {
        return base;
    }
    let width = (viewport_width - 24.0).min(480.0);
    let cancel = Button::new(
        "native_skill_reset_cancel",
        "Cancelar",
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Compact)
    .render();
    let confirm = Button::new(
        "native_skill_reset_confirm",
        "Reemplazar archivo",
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
    )
    .variant(ButtonVariant::Solid)
    .size(ControlSize::Compact)
    .render();
    Modal::new(
        base,
        true,
        "Restablecer selección de skills",
        "El archivo de selección actual no se puede cargar.",
        "native_skill_reset_cancel",
        theme::component_palette(),
    )
    .width(width)
    .body(subtle(
        "Esta acción borra las selecciones de skills locales de todos los proyectos. Se pueden volver a seleccionar desde esta página.",
    ))
    .footer(Element::row(vec![
        Element::spacer().flex(1.0),
        cancel,
        confirm,
    ]))
    .initial_focus("native_skill_reset_cancel")
    .return_focus("native_skill_reset")
    .render()
}

/// Render the MCP editor above the full window, keeping the saved servers in
/// the Settings page behind it. The modal owns the scrollable form and footer.
pub fn mcp_editor_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    if !state.show_settings || state.settings_page != SettingsPage::Mcp || !state.mcp_editor_open {
        return base;
    }

    let width = (viewport_width - 24.0).min(560.0);
    // The shared modal reserves a 14 px scrollbar rail beyond its 20 px
    // horizontal padding, so form controls never sit beneath the thumb.
    let body_width = (width - 54.0).max(0.0);
    let editing = state.mcp_selected_id.is_some();
    let mut fields = Vec::new();
    if state.settings_notice_error {
        if let Some(error) = &state.settings_notice {
            fields.push(settings_parts::feedback_banner(
                error,
                FeedbackTone::Error,
                None,
                width < 420.0,
            ));
        }
    }
    fields.extend([
        text("ID del servidor", theme::FOREGROUND, 13.0),
        subtle("Minúsculas, números, guiones y guiones bajos. El ID empieza con una letra."),
        field("mcp_draft_id", &state.mcp_draft_id, "ej. filesystem")
            .font_family(FontFamily::Monospace)
            .size(Some(body_width), Some(40.0)),
    ]);
    fields.push(text("Transporte", theme::FOREGROUND, 13.0));
    fields.push(choice_group(
        vec![
            choice_button(
                "mcp_transport:stdio",
                "Proceso local (stdio)",
                !state.mcp_draft_http,
            ),
            choice_button(
                "mcp_transport:http",
                "Streamable HTTP",
                state.mcp_draft_http,
            ),
        ],
        width < 420.0,
    ));
    if state.mcp_draft_http {
        fields.extend([
            text("URL MCP", theme::FOREGROUND, 13.0),
            subtle("Endpoint HTTPS; HTTP sólo para localhost o loopback. Sin query, credenciales en URL ni redirecciones."),
            field("mcp_draft_url", &state.mcp_draft_url, "https://example.com/mcp")
                .font_family(FontFamily::Monospace)
                .size(Some(body_width), Some(40.0)),
        ]);
        fields.push(text("Autenticación", theme::FOREGROUND, 13.0));
        fields.push(choice_group(
            vec![
                choice_button(
                    "mcp_auth:bearer",
                    "Bearer por entorno o ninguna",
                    !state.mcp_draft_oauth_enabled,
                ),
                isolation_choice(
                    "mcp_auth:oauth",
                    "OAuth 2.1",
                    state.mcp_draft_oauth_enabled,
                    cfg!(windows) || state.mcp_draft_oauth_enabled,
                ),
            ],
            width < 420.0,
        ));
        if state.mcp_draft_oauth_enabled {
            fields.extend([
                subtle("Usá un client ID público registrado para este issuer. La autorización se inicia después de guardar; no escribas client secrets ni tokens."),
                text("Client ID público", theme::FOREGROUND, 13.0),
                field("mcp_draft_oauth_client_id", &state.mcp_draft_oauth_client_id, "client-id")
                    .font_family(FontFamily::Monospace)
                    .size(Some(body_width), Some(40.0)),
                text("Issuer OAuth", theme::FOREGROUND, 13.0),
                subtle("URL HTTPS exacta del servidor de autorización, sin query ni credenciales."),
                field("mcp_draft_oauth_issuer", &state.mcp_draft_oauth_issuer, "https://auth.example.com")
                    .font_family(FontFamily::Monospace)
                    .size(Some(body_width), Some(40.0)),
                text("Scopes solicitados (opcional)", theme::FOREGROUND, 13.0),
                subtle("Uno por línea. Se unen con los scopes requeridos por el servidor; no son valores secretos."),
                TextArea::new(
                    "mcp_draft_oauth_scopes",
                    &state.mcp_draft_oauth_scopes,
                    "read\nwrite",
                    theme::component_palette(),
                )
                .width(body_width)
                .height(76.0)
                .render(),
                subtle("Los tokens se guardan en Credential Manager del usuario en Windows, fuera de Settings y del journal. Para usar el servidor, guardá y elegí Autorizar."),
            ]);
        } else {
            fields.extend([
                text("Variable del bearer token (opcional)", theme::FOREGROUND, 13.0),
                subtle("Escribe sólo el nombre de la variable. El valor se lee al conectar y no se guarda. Déjalo vacío si el servidor no exige autenticación."),
                field(
                    "mcp_draft_http_bearer_token_env_name",
                    &state.mcp_draft_http_bearer_token_env_name,
                    "ej. MCP_HTTP_TOKEN",
                )
                .font_family(FontFamily::Monospace)
                .size(Some(body_width), Some(40.0)),
            ]);
        }
        fields.push(subtle("Compatible con Streamable HTTP 2026-07-28 y 2025-11-25 (con negociación de sesión). Solicitudes de entrada adicional aún no están disponibles."));
    } else {
        fields.extend([
            text("Programa", theme::FOREGROUND, 13.0),
            subtle("Nombre del ejecutable o ruta absoluta. Se inicia sin shell."),
            field("mcp_draft_program", &state.mcp_draft_program, "ej. npx")
                .font_family(FontFamily::Monospace)
                .size(Some(body_width), Some(40.0)),
            text("Argumentos (JSON)", theme::FOREGROUND, 13.0),
            subtle("Lista JSON de textos; cada elemento es un argumento literal. No escribas tokens, claves ni contraseñas aquí."),
            TextArea::new(
                "mcp_draft_args",
                &state.mcp_draft_args,
                "[\"-y\", \"@modelcontextprotocol/server-filesystem\"]",
                theme::component_palette(),
            )
            .width(body_width)
            .height(96.0)
            .render(),
            text("Directorio de trabajo", theme::FOREGROUND, 13.0),
            subtle("Ruta relativa dentro del proyecto. Se comprueba al iniciar; vacío usa la carpeta del proyecto."),
            field("mcp_draft_cwd", &state.mcp_draft_cwd, "ej. tools/mcp")
                .font_family(FontFamily::Monospace)
                .size(Some(body_width), Some(40.0)),
            text("Variables de credenciales", theme::FOREGROUND, 13.0),
            subtle("Sólo nombres, uno por línea. Sus valores se leen del entorno al iniciar el servidor y no se guardan."),
            TextArea::new(
                "mcp_draft_env_names",
                &state.mcp_draft_env_names,
                "ej. MCP_API_KEY",
                theme::component_palette(),
            )
            .width(body_width)
            .height(76.0)
            .render(),
        ]);
    }
    fields.push(subtle("Los servidores nuevos se guardan desactivados. Usa Activar en la lista cuando estén listos."));
    let cancel = Button::new("mcp_cancel", "Cancelar", theme::component_palette())
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .render();
    let save = Button::new(
        "mcp_save",
        if editing {
            "Guardar cambios"
        } else {
            "Agregar servidor"
        },
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
    )
    .variant(ButtonVariant::Solid)
    .size(ControlSize::Compact)
    .enabled(state.mcp_load_error.is_none())
    .render();
    let footer = if width < 350.0 {
        Element::column(vec![save, cancel]).gap(6.0)
    } else {
        Element::row(vec![Element::spacer().flex(1.0), cancel, save]).gap(8.0)
    };

    Modal::new(
        base,
        true,
        if editing {
            "Editar servidor MCP"
        } else {
            "Nuevo servidor MCP"
        },
        "Configura un servidor para las herramientas del agente.",
        "mcp_cancel",
        theme::component_palette(),
    )
    .width(width)
    .height(600.0)
    .body(
        Element::column(fields)
            .gap(7.0)
            .size(Some(body_width), None),
    )
    .footer(footer)
    .initial_focus("mcp_draft_id")
    .return_focus(
        state
            .mcp_selected_id
            .as_ref()
            .map(|id| format!("mcp_select:{id}"))
            .unwrap_or_else(|| "mcp_new".to_owned()),
    )
    .render()
}

fn appearance_page(state: &ViewState, compact: bool) -> Vec<Element> {
    vec![
        settings_parts::settings_group(
            "Tema",
            vec![settings_parts::settings_row(
                "Paleta de OpenIDE",
                "La paleta oscura es la opción disponible en el renderizador nativo actual.",
                value_badge("Oscuro"),
                compact,
            )],
        ),
        settings_parts::settings_group(
            "Movimiento",
            vec![settings_parts::settings_row(
                "Reducir animaciones",
                "Los cambios de panel y las transiciones visuales usan movimiento mínimo al activarlo.",
                Switch::new(
                    "settings_reduced_motion",
                    state.settings_reduced_motion,
                    theme::component_palette(),
                )
                .render(),
                compact,
            )],
        ),
    ]
}

fn terminal_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let shell = &state.settings_terminal_shell;
    let shell_name = match shell {
        TerminalShell::System => {
            if std::env::var_os("OPENIDE_TERMINAL_SHELL").is_some() {
                "Variable de entorno"
            } else {
                "Sistema"
            }
        }
        TerminalShell::PowerShell => "PowerShell",
        TerminalShell::PowerShellCore => "PowerShell 7",
        TerminalShell::CommandPrompt => "Símbolo del sistema",
        TerminalShell::Bash => "Bash",
        TerminalShell::Zsh => "Zsh",
        TerminalShell::Fish => "Fish",
        TerminalShell::Custom(_) => "Ruta personalizada",
    };
    let mut choices = vec![choice_button(
        "settings_terminal_shell:system",
        "Sistema",
        matches!(shell, TerminalShell::System),
    )];
    if cfg!(windows) {
        choices.extend([
            choice_button(
                "settings_terminal_shell:powershell",
                "PowerShell",
                matches!(shell, TerminalShell::PowerShell),
            ),
            choice_button(
                "settings_terminal_shell:pwsh",
                "PowerShell 7",
                matches!(shell, TerminalShell::PowerShellCore),
            ),
            choice_button(
                "settings_terminal_shell:cmd",
                "CMD",
                matches!(shell, TerminalShell::CommandPrompt),
            ),
        ]);
    } else {
        choices.extend([
            choice_button(
                "settings_terminal_shell:bash",
                "Bash",
                matches!(shell, TerminalShell::Bash),
            ),
            choice_button(
                "settings_terminal_shell:zsh",
                "Zsh",
                matches!(shell, TerminalShell::Zsh),
            ),
            choice_button(
                "settings_terminal_shell:fish",
                "Fish",
                matches!(shell, TerminalShell::Fish),
            ),
        ]);
    }
    let preset_choices = Element::column(
        choices
            .chunks(2)
            .map(|pair| choice_group(pair.to_vec(), compact))
            .collect(),
    )
    .gap(8.0);
    let custom_path = field(
        "settings_terminal_shell_path",
        &state.settings_terminal_shell_draft,
        "Ruta absoluta del ejecutable",
    )
    .font_family(FontFamily::Monospace);
    let custom_button = action_button(
        "settings_terminal_shell:custom",
        "Usar esta ruta",
        Icon::Check,
    );
    let custom_control = if compact {
        Element::column(vec![custom_path, custom_button]).gap(10.0)
    } else {
        Element::row(vec![custom_path.flex(1.0), custom_button]).gap(10.0)
    };
    vec![
        group(
            "Shell integrado",
            vec![
                setting_row(
                    "Selección activa",
                    "Se aplica a las nuevas pestañas y al reiniciar una terminal existente.",
                    value_badge(shell_name),
                    compact,
                ),
                divider(),
                preset_choices,
            ],
        ),
        group(
            "Ejecutable personalizado",
            vec![
                subtle("Selecciona un ejecutable local mediante una ruta absoluta. Se comprueba antes de guardarlo."),
                custom_control,
            ],
        ),
    ]
}

fn additional_page(state: &ViewState, page: SettingsPage, compact: bool) -> Vec<Element> {
    match page {
        SettingsPage::Appearance => appearance_page(state, compact),
        SettingsPage::Terminal => terminal_page(state, compact),
        SettingsPage::WorkspaceSearch => vec![
            group(
                "Archivos del proyecto",
                vec![setting_row(
                    "Búsqueda disponible",
                    "La búsqueda local se ejecuta fuera del hilo visual.",
                    action_button("composer_add_context", "Buscar archivos", Icon::Search),
                    compact,
                )],
            ),
            group(
                "Reglas de exclusión",
                vec![subtle("El motor usa exclusiones integradas. La edición de reglas por proyecto aún no está disponible.")],
            ),
        ],
        SettingsPage::Memory => vec![
            group(
                "Contexto del agente",
                vec![setting_row(
                    "Historial de la conversación",
                    "El agente conserva los turnos recientes y recorta los más antiguos cuando alcanza su límite de contexto.",
                    value_badge("Temporal"),
                    compact,
                )],
            ),
            settings_parts::empty_state(
                "Memoria persistente no disponible",
                "OpenIDE todavía no guarda recuerdos entre conversaciones ni permite revisarlos o eliminarlos aquí.",
                None,
            ),
        ],
        SettingsPage::Commands => {
            let mut content = vec![
            group(
                "Acciones de OpenIDE",
                vec![setting_row(
                    "Centro de comandos",
                    "Busca acciones, tareas y archivos disponibles en la aplicación.",
                    action_button("open_command_center", "Abrir acciones", Icon::Search),
                    compact,
                )],
            ),
            ];
            content.extend(exec_rules_page(state, compact));
            content
        }
        SettingsPage::Browser => capability_page(
            "Navegador no conectado",
            "El agente nativo no puede abrir ni controlar sesiones de navegador desde OpenIDE.",
            "Hace falta una herramienta de navegador con sesión aislada, permisos y supervisión de sus acciones.",
            None,
        ),
        SettingsPage::ComputerUse => capability_page(
            "Control del equipo no disponible",
            "El agente no puede capturar la pantalla ni enviar entradas al escritorio.",
            "Esta función necesita permisos de sistema independientes y confirmación de las acciones del agente.",
            None,
        ),
        SettingsPage::Subagents => capability_page(
            "Subagentes no disponibles",
            "Las tareas actuales ejecutan un agente por turno; aún no hay agentes delegados configurables.",
            "Se necesita un registro de agentes, límites de concurrencia y una relación visible entre tarea principal y subtareas.",
            None,
        ),
        SettingsPage::Plugins => capability_page(
            "Plugins ejecutables no disponibles",
            "Marketplace instala skills de instrucciones; no ejecuta plugins de proceso.",
            "Antes de activar plugins se necesita aislamiento, permisos y control de su ciclo de vida.",
            Some(("settings_page:skills", "Ver skills", Icon::ArrowRight)),
        ),
        SettingsPage::Mcp => mcp_page(state, compact),
        SettingsPage::Hooks => hooks_page(state, compact),
        SettingsPage::Usage => vec![
            group(
                "Actividad local",
                vec![setting_row(
                    "Tareas visibles",
                    "Este recuento procede del historial guardado en el equipo.",
                    value_badge(format!("{} tareas", state.sessions.len())),
                    compact,
                )],
            ),
            settings_parts::empty_state(
                "Tokens y costes no disponibles",
                "El motor aún no registra consumo por turno. Los precios del catálogo no son una medición de gasto.",
                Some(action_button("settings_page:providers", "Ver modelos", Icon::ArrowRight)),
            ),
        ],
        _ => unreachable!("existing Settings page has its own renderer"),
    }
}

fn compact_navigation(state: &ViewState, page: SettingsPage, one_column: bool) -> Element {
    let mut options = vec![
        action_button("toggle_settings", "Volver a OpenIDE", Icon::ArrowLeft),
        action_button(
            "settings_nav_toggle",
            if state.settings_nav_open {
                "Ocultar secciones"
            } else {
                "Explorar secciones"
            },
            Icon::Menu,
        ),
    ];
    if state.settings_nav_open {
        options.push(divider());
        for (title, pages) in SETTINGS_GROUPS {
            options.push(section(title));
            for pair in pages.chunks(if one_column { 1 } else { 2 }) {
                options.push(choice_group(
                    pair.iter()
                        .map(|item| {
                            choice_button(item.action_id(), item.nav_title(), *item == page)
                        })
                        .collect(),
                    one_column,
                ));
            }
        }
    }
    group("Secciones", options)
}

pub fn build_settings_page(
    state: &ViewState,
    page: SettingsPage,
    center_width: f32,
    sidebar_visible: bool,
) -> Element {
    let compact = center_width < 720.0;
    let sidebar_hidden = !sidebar_visible;
    let cards = match page {
        SettingsPage::General => general_page(state, compact),
        SettingsPage::Agent => agent_page(state, compact),
        SettingsPage::Providers => providers_page(state, compact, center_width),
        SettingsPage::Skills => skills_page(state, compact),
        SettingsPage::Automations => automations_page(state, compact),
        SettingsPage::Data => data_page(state, compact),
        SettingsPage::Github => github_page(state, compact),
        SettingsPage::Shortcuts => shortcuts_page(),
        _ => additional_page(state, page, compact),
    };
    let mut body =
        vec![text("PREFERENCIAS  /  OPENIDE", theme::FOREGROUND_SUBTLE, 11.0).min_size(0.0, 24.0)];
    if sidebar_hidden {
        body.push(compact_navigation(state, page, center_width < 520.0));
    }
    body.extend([
        settings_parts::section_header(
            page.title(),
            page.description(),
            Some(settings_parts::status_badge(page.status())),
            compact,
        ),
        Element::spacer().size(None, Some(20.0)),
    ]);
    if let Some(message) = &state.settings_notice {
        body.push(settings_parts::feedback_banner(
            message,
            if state.settings_notice_error {
                FeedbackTone::Error
            } else {
                FeedbackTone::Success
            },
            None,
            compact,
        ));
    }
    body.extend(cards);
    let gutter = if center_width < 720.0 { 24.0 } else { 48.0 };
    let content_width = (center_width - gutter * 2.0).clamp(0.0, 880.0);
    let centered = Element::row(vec![
        Element::spacer().flex(1.0),
        Element::column(body)
            .gap(18.0)
            .size(Some(content_width), None),
        Element::spacer().flex(1.0),
    ])
    .padding(gutter);
    ScrollArea::new(format!("settings_page:{page:?}"), centered)
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .flex(1.0)
}

#[cfg(test)]
mod tests {
    use super::{catalog_model_usable, SettingsPage, SETTINGS_GROUPS};

    #[test]
    fn settings_routes_are_distinct_and_parseable() {
        let mut ids = std::collections::HashSet::new();
        for (_, pages) in SETTINGS_GROUPS {
            for page in *pages {
                assert_eq!(SettingsPage::from_action_id(page.action_id()), Some(*page));
                assert!(ids.insert(page.action_id()), "duplicate settings route");
            }
        }
        assert_eq!(ids.len(), 20);
        assert_eq!(SettingsPage::from_action_id("settings_page:unknown"), None);
    }

    #[test]
    fn catalog_selection_is_not_restricted_by_provider_or_model_prefix() {
        for (provider, model_id) in [
            ("openai", "new-future-model"),
            ("deepseek", "new-future-model"),
            ("groq", "some-model"),
            ("my-provider", "unseen-model"),
        ] {
            let model: crate::model_catalog::ModelInfo =
                serde_json::from_value(serde_json::json!({
                    "id": model_id,
                    "name": model_id,
                    "modalities": {"input": ["text"], "output": ["text"]}
                }))
                .unwrap();
            assert!(
                catalog_model_usable(provider, &model),
                "{provider}/{model_id}"
            );
        }
    }

    #[test]
    fn explicit_tool_or_text_exclusion_blocks_catalog_selection() {
        let model: crate::model_catalog::ModelInfo = serde_json::from_value(serde_json::json!({
            "id": "provider/model",
            "name": "Model",
            "tool_call": true,
            "tool_call_declared": true,
            "modalities": {"input": ["text"], "output": ["text"]}
        }))
        .unwrap();
        assert!(catalog_model_usable("openrouter", &model));
        let mut no_tools = model;
        no_tools.tool_call = false;
        no_tools.tool_call_declared = Some(false);
        assert!(!catalog_model_usable("openrouter", &no_tools));
        no_tools.tool_call_declared = None;
        no_tools.modalities.as_mut().unwrap().output = vec!["image".to_owned()];
        assert!(!catalog_model_usable("openrouter", &no_tools));
    }
}
