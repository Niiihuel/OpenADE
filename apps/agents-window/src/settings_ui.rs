//! Settings workspace, with its own navigation and neutral card layout.
//!
//! This module only describes UI. The app owns settings values and handles the
//! action IDs, so navigating away and back does not reset edits.

use std::path::Path;

use openide_core::SessionSort;
use openide_harness::LocalTool;
use openide_ui::{
    Button, ButtonVariant, Color, Component, ControlSize, Element, FontFamily, Icon,
    IconButton as UiIconButton, ScrollArea, Switch,
};

use crate::{
    github_account::GithubAccountStatus,
    native_provider,
    settings_parts::{self, CapabilityStatus, FeedbackTone},
    settings_preferences::TerminalShell,
    theme,
    view::{self, SessionOrganization, ViewState},
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
            Self::Commands => "Acciones rápidas y comandos reutilizables.",
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
            Self::Browser
            | Self::ComputerUse
            | Self::Subagents
            | Self::Plugins
            | Self::Mcp
            | Self::Hooks
            | Self::Usage => CapabilityStatus::Planned,
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
    vec![
        group(
            "Skills del agente",
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
                "El Marketplace instala instrucciones de skills. Los plugins de proceso y servidores MCP requieren un servicio propio antes de aparecer como controles de Settings.",
            )],
        ),
    ]
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
    provider_id: &str,
    model: &crate::model_catalog::ModelInfo,
) -> bool {
    model.tool_call
        && openide_harness::native_agent_supports_tool_calls(&model.id)
        && (provider_id != "openai" || native_provider::openai_native_compatible_model(&model.id))
        && (provider_id != "deepseek"
            || native_provider::deepseek_native_compatible_model(&model.id))
        && model.status.as_deref() != Some("deprecated")
        && model.modalities.as_ref().is_some_and(|modalities| {
            modalities.input.iter().any(|kind| kind == "text")
                && modalities.output.iter().any(|kind| kind == "text")
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
    let key_name = native_provider::adapter(&state.native_provider_id)
        .map(|adapter| adapter.key_env)
        .unwrap_or("OPENIDE_API_KEY");
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
        subtle("Fuente: models.dev. Los modelos aparecen al publicarse allí; la ejecución requiere un adaptador Rust compatible."),
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
            let left_supported = native_provider::adapter(&left.id).is_some();
            let right_supported = native_provider::adapter(&right.id).is_some();
            right_selected.cmp(&left_selected).then_with(|| {
                right_supported
                    .cmp(&left_supported)
                    .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            })
        });
        let count = providers.len();
        for provider in providers.into_iter().take(18) {
            let available = native_provider::adapter(&provider.id).is_some();
            let title = format!(
                "{} · {} modelos · {}",
                provider.name,
                provider.models.len(),
                if available {
                    "Conexión nativa"
                } else {
                    "Sólo catálogo"
                }
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
        let supported = native_provider::adapter(&provider.id).is_some();
        let mut models = vec![
            subtle(format!(
                "{} · {} modelos · {}",
                provider.name,
                provider.models.len(),
                if supported {
                    "Conexión nativa disponible"
                } else {
                    "Conexión nativa no disponible"
                }
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
            let context = model
                .limit
                .as_ref()
                .and_then(|limit| limit.context)
                .map(|value| format!("{value} tokens de contexto"))
                .unwrap_or_else(|| "Contexto no informado".to_owned());
            let price = model
                .cost
                .as_ref()
                .and_then(|cost| cost.input.zip(cost.output))
                .map(|(input, output)| format!("${input:.2}/${output:.2} por millón de tokens"))
                .unwrap_or_else(|| "Precio no informado".to_owned());
            let eligible = supported && catalog_model_usable(&provider.id, model);
            let availability = if eligible {
                "Seleccionable para el agente nativo"
            } else if !supported {
                "Sin conexión nativa"
            } else if provider.id == "openai"
                && !native_provider::openai_native_compatible_model(&model.id)
            {
                "Modelo pendiente de verificar para Chat Completions"
            } else if provider.id == "deepseek"
                && !native_provider::deepseek_native_compatible_model(&model.id)
            {
                "Alias sin soporte nativo verificado"
            } else if !model.tool_call {
                "Sin herramientas para el agente"
            } else {
                "Modalidad o estado no compatible"
            };
            let updated = model.last_updated.as_deref().unwrap_or("sin fecha");
            let capabilities = format!(
                "herramientas: {} · razonamiento: {}",
                if model.tool_call { "sí" } else { "no" },
                if model.reasoning { "sí" } else { "no" }
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
                compact,
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
                            subtle(format!("Límites: {limit}")),
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
        text(keys, theme::FOREGROUND_SUBTLE, 12.0)
            .font_family(FontFamily::Monospace)
            .padding(7.0)
            .background(theme::SELECTED)
            .radius(7.0),
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
        SettingsPage::Commands => vec![
            group(
                "Acciones de OpenIDE",
                vec![setting_row(
                    "Centro de comandos",
                    "Busca acciones, tareas y archivos disponibles en la aplicación.",
                    action_button("open_command_center", "Abrir acciones", Icon::Search),
                    compact,
                )],
            ),
            settings_parts::empty_state(
                "Comandos personalizados no disponibles",
                "Todavía no hay un almacén de comandos reutilizables para el agente.",
                None,
            ),
        ],
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
        SettingsPage::Mcp => capability_page(
            "Servidores MCP no conectados",
            "OpenIDE todavía no inicia servidores MCP ni incorpora sus herramientas al agente.",
            "La configuración requiere transporte, autenticación, estado de conexión y aprobación por herramienta.",
            None,
        ),
        SettingsPage::Hooks => capability_page(
            "Hooks no disponibles",
            "No se ejecutan acciones automáticas alrededor de los eventos del agente.",
            "Se necesitan eventos definidos, límites de ejecución y permisos de confianza antes de permitir comandos automáticos.",
            None,
        ),
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
    let compact = center_width < 620.0;
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
    fn catalog_tool_flag_does_not_override_native_protocol_limits() {
        for (provider, model_id, expected) in [
            ("openai", "gpt-5.4", true),
            ("openai", "gpt-5.4-pro", false),
            ("openai", "gpt-realtime-2.1", false),
            ("openai", "gpt-6-astra", false),
            ("deepseek", "deepseek-flash", true),
            ("deepseek", "deepseek-v4-flash", false),
        ] {
            let model: crate::model_catalog::ModelInfo =
                serde_json::from_value(serde_json::json!({
                    "id": model_id,
                    "name": model_id,
                    "tool_call": true,
                    "modalities": {"input": ["text"], "output": ["text"]}
                }))
                .unwrap();
            assert_eq!(
                catalog_model_usable(provider, &model),
                expected,
                "{model_id}"
            );
        }
    }
}
