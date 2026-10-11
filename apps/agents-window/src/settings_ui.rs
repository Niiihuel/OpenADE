//! Settings workspace, with its own navigation and neutral card layout.
//!
//! This module only describes UI. The app owns settings values and handles the
//! action IDs, so navigating away and back does not reset edits.

use std::path::Path;

mod activity_charts;
mod external_agents;
#[cfg(test)]
mod agent_tests;
mod instructions_inspection;
mod mcp_inspection;
mod memory;
mod providers;
mod rules_preview;
mod skills_preview;
mod subagents;
#[cfg(test)]
pub(crate) mod visual_qa;
pub(crate) use instructions_inspection::modal as native_instructions_modal;
pub(crate) use providers::model_row_index as catalog_model_row_index;
pub(crate) use providers::connected_model_choices;
pub(crate) use skills_preview::clear_workspace_modal as native_skill_clear_workspace_modal;
pub(crate) use skills_preview::modal as native_skill_preview_modal;

use openide_core::SessionSort;
use openide_harness::{CommandIsolation, ExecDecision, HookPhase, LocalTool, SkillOrigin};
use openide_ui::{
    ActionRow, Alert, Button, ButtonGroup, ButtonVariant, Component, ControlSize, DropdownOption,
    Element, FormField, Icon, IconButton as UiIconButton, Kbd, Modal, PageColumn, PageHeader,
    PopoverPlacement, RadiusRole, ScrollArea, ScrollbarAppearance, SearchField, Select,
    SelectorSizing, Separator, Text, TextArea, TextField, TextRole,
};

use crate::{
    github_account::GithubAccountStatus,
    keymap::{KeymapProfile, ShortcutAction},
    settings_parts::{self, CapabilityStatus, FeedbackTone},
    settings_preferences::TerminalShell,
    theme,
    ui_sizing::{GROUP_FIELD, NOTE_ROW},
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
    ExternalAgents,
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
    SettingsPage::Terminal,
    SettingsPage::Shortcuts,
];
const AGENT_PAGES: &[SettingsPage] = &[
    SettingsPage::Agent,
    SettingsPage::Memory,
    SettingsPage::Subagents,
    SettingsPage::ExternalAgents,
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
    ("Preferencias", BASIC_PAGES),
    ("Agente y extensiones", AGENT_PAGES),
    ("Cuenta y datos", DATA_PAGES),
];

const SETTINGS_SCROLL_RAIL_WIDTH: f32 = 14.0;

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
            Self::ExternalAgents => "settings_page:external_agents",
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
            "settings_page:external_agents" => Some(Self::ExternalAgents),
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
            Self::ExternalAgents => "CLIs externas",
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
            Self::Appearance => Icon::Palette,
            Self::Agent => Icon::Chat,
            Self::Providers => Icon::Brain,
            Self::Browser => Icon::Globe,
            Self::ComputerUse => Icon::Laptop,
            Self::Terminal => Icon::Terminal,
            Self::WorkspaceSearch => Icon::Search,
            Self::Memory => Icon::History,
            Self::Subagents => Icon::Users,
            Self::ExternalAgents => Icon::Send,
            Self::Plugins => Icon::Puzzle,
            Self::Mcp => Icon::Server,
            Self::Skills => Icon::Book,
            Self::Commands => Icon::Code,
            Self::Automations => Icon::Clock,
            Self::Hooks => Icon::Bolt,
            Self::Data => Icon::Archive,
            Self::Usage => Icon::ChartBar,
            Self::Github => Icon::Github,
            Self::Shortcuts => Icon::Keyboard,
        }
    }

    const fn nav_title(self) -> &'static str {
        match self {
            Self::Providers => "Proveedores y modelos",
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
            Self::Providers => "Conecta tus cuentas y elige modelos para trabajar en OpenIDE.",
            Self::Browser => "Acceso del agente a sesiones de navegador.",
            Self::ComputerUse => "Control del escritorio y sus permisos.",
            Self::Terminal => "Shell y comportamiento de la terminal integrada.",
            Self::Shortcuts => "Atajos de navegación y paneles.",
            Self::WorkspaceSearch => "Búsqueda de archivos del proyecto.",
            Self::Agent => "Proveedor, herramientas y permisos del agente nativo de OpenIDE.",
            Self::Memory => "Contexto, instrucciones y continuidad de la conversación.",
            Self::Subagents => "Trabajo delegado, permisos y recuperación por conversación.",
            Self::ExternalAgents => "Herramientas de OpenIDE para Claude Code, Codex y otras CLIs.",
            Self::Plugins => "Extensiones ejecutables y capacidades instaladas.",
            Self::Mcp => "Servidores de herramientas externos.",
            Self::Skills => "Instrucciones instaladas para el agente.",
            Self::Commands => "Centro de comandos y reglas para la ejecución del agente.",
            Self::Automations => "Tareas programadas del proyecto.",
            Self::Hooks => "Acciones asociadas a eventos del agente.",
            Self::Data => "Historial local y organización de tareas.",
            Self::Usage => "Suscripciones, saldo y límites de tus proveedores conectados.",
            Self::Github => "Conexión de la cuenta y repositorio de GitHub.",
        }
    }

    const fn status(self) -> CapabilityStatus {
        match self {
            Self::Skills | Self::Automations | Self::Plugins | Self::Browser => {
                CapabilityStatus::Ready
            }
            Self::ComputerUse => CapabilityStatus::Planned,
            Self::Mcp | Self::Hooks => CapabilityStatus::Partial,
            _ => CapabilityStatus::Partial,
        }
    }
}

fn section(value: &str) -> Element {
    Text::new(value, theme::component_palette())
        .role(TextRole::Small)
        .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
        .render()
        .padding(4.0)
        .min_size(0.0, 22.0)
}

fn field(id: &str, value: &str, placeholder: &str) -> Element {
    TextField::new(id, value, placeholder, control_palette())
        .size(ControlSize::Compact)
        .render()
}

fn mono_field(id: &str, value: &str, placeholder: &str) -> TextField {
    TextField::new(id, value, placeholder, control_palette())
        .size(ControlSize::Compact)
        .monospace(true)
}

/// The two actions of a settings form: at the far edge, and in a column
/// with the primary one first when the card is narrow.
fn form_footer(cancel: Element, save: Element, width: f32) -> Element {
    ButtonGroup::new(vec![cancel, save])
        .align_end()
        .stacked(width < 350.0)
        .reverse_when_stacked(true)
        .sizing(crate::ui_sizing::FORM_FOOTER)
        .render()
}

/// A field of a settings form: its hints sit between the label and the
/// control.
fn form_field(label: &str, control: Element) -> FormField {
    FormField::new(label, control, theme::card_palette()).hint_above(true)
}

/// Settings controls need their own visible edge above the surrounding card.
/// Keep this local so quiet navigation and the rest of the native chrome retain
/// their existing treatment.
pub(crate) use crate::theme::control_palette;

/// The settings action: outlined and compact on the control palette.
pub(crate) fn outline(id: impl Into<String>, label: impl Into<String>) -> Button {
    // Page links share a navigation action with the sidebar, but each
    // control must retain its own hover, color transition and keyboard focus.
    let id = id.into();
    let link = id
        .strip_prefix("settings_page:")
        .map(|page| format!("settings_link:{page}"));
    let button = Button::new(
        link.clone().unwrap_or_else(|| id.clone()),
        label,
        control_palette(),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact);
    if link.is_some() {
        button.dispatch_as(id)
    } else {
        button
    }
}

/// Ordinary settings choices use the same short rows and bounded surface as
/// mode selectors. Selection and open state remain owned by the application.
fn settings_choice_selector(
    state: &ViewState,
    key: &str,
    label: &str,
    options: Vec<DropdownOption>,
) -> Element {
    Select::new(
        format!("settings_choice_toggle:{key}"),
        label,
        options,
        state.settings_choice_menu_open.as_deref() == Some(key),
        "settings_choice_dismiss",
        control_palette(),
    )
    .menu_palette(theme::menu_palette())
    .sizing(SelectorSizing::COMPACT)
    .render()
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
        theme::SETTINGS_FOREGROUND_SUBTLE
    };
    // The page on screen wears the solid form of its icon.
    UiIconButton::new(
        page.action_id(),
        if active {
            page.icon().filled()
        } else {
            page.icon()
        },
        theme::action_palette(fill, color),
    )
    .label(page.nav_title())
    .variant(ButtonVariant::Subtle)
    .sizing(crate::ui_sizing::SETTINGS_NAV)
    .radius(RadiusRole::Surface)
    .active(active)
    .render()
}

fn project_name(workspace: &str) -> &str {
    Path::new(workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Proyecto")
}

fn navigation_groups(state: &ViewState) -> Vec<Element> {
    let query = state.settings_nav_query.trim().to_lowercase();
    let mut items = Vec::new();
    for (title, pages) in SETTINGS_GROUPS {
        let matches: Vec<_> = pages
            .iter()
            .filter(|page| {
                query.is_empty()
                    || page.title().to_lowercase().contains(&query)
                    || page.description().to_lowercase().contains(&query)
            })
            .collect();
        if matches.is_empty() {
            continue;
        }
        if !items.is_empty() {
            items.push(Element::spacer().size(None, Some(10.0)));
        }
        items.push(section(title));
        items.extend(
            matches
                .into_iter()
                .map(|page| nav_item(*page, state.settings_page == *page)),
        );
    }
    if items.is_empty() {
        items.push(
            Text::new(
                "Sin secciones para esta búsqueda",
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
            .render()
            .padding(8.0),
        );
    }
    items
}

fn navigation_search(state: &ViewState) -> Element {
    SearchField::new(
        "settings_search",
        &state.settings_nav_query,
        "Buscar ajustes…",
        control_palette(),
    )
    .size(ControlSize::Compact)
    .render()
}

pub fn build_settings_sidebar(state: &ViewState, width: f32) -> Element {
    let back = UiIconButton::new(
        "toggle_settings",
        Icon::ArrowLeft,
        theme::action_palette(theme::SIDEBAR, theme::SETTINGS_FOREGROUND_SUBTLE),
    )
    .label("Volver a OpenIDE")
    .variant(ButtonVariant::Ghost)
    .sizing(crate::ui_sizing::SETTINGS_NAV)
    .render();
    let navigation = ScrollArea::new(
        "settings_navigation",
        Element::column(navigation_groups(state)).gap(2.0),
    )
    .appearance(ScrollbarAppearance::reserved().rail_width(SETTINGS_SCROLL_RAIL_WIDTH))
    .tint(theme::FOREGROUND_SUBTLE)
    .render()
    .flex(1.0);
    openide_ui::SidebarPanel::new(
        vec![
            back,
            Separator::horizontal(theme::component_palette()).render(),
            Element::row(vec![
                Element::icon(Icon::Settings)
                    .foreground(theme::FOREGROUND)
                    .size(Some(16.0), Some(16.0)),
                Text::new("Configuración", theme::component_palette())
                    .role(TextRole::Label)
                    .render(),
            ])
            .gap(8.0)
            .padding(8.0),
            navigation_search(state),
            navigation,
            Separator::horizontal(theme::component_palette()).render(),
            view::sidebar_account_footer(state, width - 12.0),
        ],
        theme::sidebar_palette(),
    )
    .render()
    // The panel is as tall as its window; the frame is the settings page's.
    .size(Some(width), None)
    .border(theme::BORDER, 1.0)
    .radius(RadiusRole::Card.px())
}

fn general_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let path = mono_field("workspace", &state.workspace_draft, "Ruta del proyecto").render();
    let open = outline("workspace_apply", "Abrir proyecto")
        .icon(Icon::Folder)
        .render();
    let project_control = if compact {
        Element::column(vec![path, open]).gap(8.0)
    } else {
        Element::row(vec![path.flex(1.0), open]).gap(8.0)
    };
    vec![
        settings_parts::settings_group(
            "Espacio de trabajo",
            vec![
                settings_parts::value_row(
                    "Proyecto activo",
                    &state.workspace,
                    project_name(&state.workspace),
                    compact,
                ),
                form_field("Abrir otra carpeta", project_control)
                    .hint("La carpeta define los archivos y el contexto del agente.")
                    .sizing(GROUP_FIELD)
                    .render()
                    .padding(settings_parts::ROW_PADDING),
                settings_parts::switch_row(
                    "settings_reopen_last_workspace",
                    "Reabrir el último proyecto",
                    "Continúa en la última carpeta al iniciar OpenIDE.",
                    state.settings_reopen_last_workspace,
                    true,
                    compact,
                ),
            ],
        ),
        settings_parts::settings_group(
            "Preferencias de la aplicación",
            vec![
                settings_parts::settings_row(
                    "Apariencia",
                    "Paleta y movimiento de la interfaz.",
                    outline("settings_page:appearance", "Personalizar")
                        .icon(Icon::ArrowRight)
                        .render(),
                    compact,
                ),
                settings_parts::settings_row(
                    "Organización de tareas",
                    "Agrupación, orden e historial de la barra lateral.",
                    outline("settings_page:data", "Administrar datos")
                        .icon(Icon::ArrowRight)
                        .render(),
                    compact,
                ),
                settings_parts::settings_row(
                    "Atajos de teclado",
                    "Consulta las combinaciones disponibles en OpenIDE.",
                    outline("settings_page:shortcuts", "Ver atajos")
                        .icon(Icon::ArrowRight)
                        .render(),
                    compact,
                ),
            ],
        ),
            crate::notifications_ui::settings_group(&state.notifications, compact),
        crate::tour_ui::settings_group(compact),
    ]
}

/// A passive note shares the row typography without presenting an unavailable
/// preference as an editable control.
fn page_note(title: &str, description: &str, icon: Icon) -> Element {
    ActionRow::new(title, theme::card_palette())
        .subtitle(description)
        .icon(icon)
        .icon_muted(true)
        .align_top(true)
        .wrap_title(true)
        .sizing(NOTE_ROW)
        .render()
}

fn agent_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let write_description = match (
        state.native_tool_permissions.write_file,
        state.native_tool_permissions.run_command,
    ) {
        (true, true) => "Escritura y comandos activos. Las operaciones requieren aprobación o una autorización vigente de la sesión.",
        (false, false) => "Escritura y comandos desactivados. Los permisos de lectura se configuran por separado.",
        (true, false) => "Personalizado: modificar archivos con aprobación está activo; ejecutar comandos está desactivado.",
        (false, true) => "Personalizado: ejecutar comandos con aprobación está activo; modificar archivos está desactivado.",
    };
    let isolated = state.native_command_isolation == CommandIsolation::WorkspaceWriteOffline;
    let offline_available = CommandIsolation::WorkspaceWriteOffline.is_available_on_host();
    let platform = if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Esta plataforma"
    };
    let isolation_description = if cfg!(target_os = "linux")
        && CommandIsolation::WorkspaceWriteOffline.is_supported_on_host()
        && !offline_available
    {
        "Linux: para usar Proyecto, sin red instalá Bubblewrap en una ruta ejecutable del sistema. No se detectó un ejecutable compatible. Sin aislamiento usa los permisos de tu cuenta.".to_owned()
    } else if offline_available && cfg!(target_os = "linux") {
        "Linux: Proyecto, sin red limita la escritura al proyecto y bloquea sockets de red. Requiere Bubblewrap y espacios de nombres de usuario habilitados; si no se pueden preparar, el comando se bloquea. Sin aislamiento usa los permisos de tu cuenta.".to_owned()
    } else if offline_available {
        format!("{platform}: Proyecto, sin red restringe la escritura y la red de los comandos. Sin aislamiento usa los permisos de tu cuenta.")
    } else {
        format!("{platform}: los comandos sin aislamiento tienen los permisos de tu cuenta y acceso a red. Proyecto, sin red aún no está implementado en esta plataforma.")
    };
    let offline_option = DropdownOption::new(
        "command_isolation:workspace_write_offline",
        "Proyecto, sin red",
    )
    .selected(isolated);
    let offline_option = if offline_available {
        offline_option
    } else {
        offline_option.disabled()
    };
    let isolation_selector = settings_choice_selector(
        state,
        "isolation",
        if isolated {
            "Proyecto, sin red"
        } else {
            "Sin aislamiento"
        },
        vec![
            offline_option,
            DropdownOption::new("command_isolation:host_unrestricted", "Sin aislamiento")
                .selected(!isolated),
        ],
    );
    let mut isolation_rows = vec![settings_parts::settings_row(
        "Aislamiento de comandos",
        isolation_description,
        isolation_selector,
        compact,
    )];
    if !state.native_command_isolation.is_available_on_host() {
        isolation_rows.push(settings_parts::feedback_banner(
            "El perfil guardado no está disponible aquí. Los comandos quedan bloqueados hasta que elijas un perfil compatible; no se cambia automáticamente a Sin aislamiento.",
            FeedbackTone::Error, None, compact,
        ));
    }
    isolation_rows.extend([
        settings_parts::settings_row(
            "Reglas de comandos",
            "Pide aprobación o bloquea comandos según las reglas guardadas. Una regla no habilita una herramienta desactivada.",
            outline("settings_page:commands", "Editar reglas").icon(Icon::ArrowRight).render(),
            compact,
        ),
        page_note("Aplicación de los cambios", "Se guardan para los próximos turnos nativos. Cambiar el aislamiento o desactivar comandos revoca los procesos persistentes existentes.", Icon::Check),
    ]);
    let model_description = if selected_native_route(state).is_some() {
        format!(
            "Proveedor: {}\nModelo: {}\n{}",
            state.native_provider_id,
            state.model,
            native_route_status(state)
        )
    } else if state
        .composer_routes
        .iter()
        .any(|route| route.backend == "native")
    {
        "No hay un modelo nativo seleccionado.\nElegí un modelo o revisa la conexión del proveedor."
            .into()
    } else {
        "No hay modelos disponibles.\nConecta un proveedor para configurar el agente nativo.".into()
    };
    let provider_action = if state.selected_backend == "native" {
        outline("composer_connect_provider", "Conectar proveedor")
            .icon(Icon::ArrowRight)
            .render()
    } else {
        outline("settings_page:providers", "Proveedores")
            .icon(Icon::ArrowRight)
            .render()
    };
    let mut content = vec![
        settings_parts::settings_group(
            "Agente nativo de OpenIDE",
            vec![
                settings_parts::value_row(
                    "Ejecución nativa",
                    "OpenIDE administra el contexto, las herramientas y la recuperación de la conversación, y consulta directamente la API del proveedor.",
                    "OpenIDE",
                    compact,
                ),
                settings_parts::settings_row(
                    "Proveedor y modelo",
                    model_description,
                    Element::column(vec![
                        outline("settings_engine_open", "Elegir modelo")
                            .icon(Icon::ArrowRight)
                            .enabled(!state.running && !state.background_run_selected)
                            .render(),
                        provider_action,
                    ]).gap(8.0),
                    compact,
                ),
                page_note("Alcance de estos ajustes", "Los permisos se guardan para los próximos turnos nativos. Los subagentes reciben su política al iniciar su trabajo; las automatizaciones usan su propia política guardada.", Icon::Check),
            ],
        ),
    ];
    if state.native_provider_id == "openai" {
        use openide_harness::HostedWebSearchMode as Mode;
        let options = [
            ("native_web_search:off", "Desactivada", None),
            ("native_web_search:cached", "En caché", Some(Mode::Cached)),
            ("native_web_search:indexed", "Indexada", Some(Mode::Indexed)),
            ("native_web_search:live", "En vivo", Some(Mode::Live)),
        ];
        let current = options
            .iter()
            .find(|(_, _, mode)| *mode == state.hosted_web_search_mode)
            .map_or("Desactivada", |(_, label, _)| *label);
        content.push(settings_parts::settings_group(
            "Búsqueda web",
            vec![settings_parts::settings_row(
                "Búsqueda web alojada",
                "El proveedor busca en la web durante el turno y puede contactar fuentes externas. Aplica a los próximos turnos; no se puede cambiar con un agente en curso.",
                settings_choice_selector(
                    state,
                    "native_web_search",
                    current,
                    options
                        .iter()
                        .map(|(id, label, mode)| {
                            DropdownOption::new(*id, *label)
                                .selected(*mode == state.hosted_web_search_mode)
                        })
                        .collect(),
                ),
                compact,
            )],
        ));
    }
    content.extend([
        settings_parts::settings_group(
            "Herramientas del agente nativo",
            vec![
                settings_parts::switch_row(
                    "write_mode",
                    "Escritura y comandos",
                    write_description,
                    state.native_tool_permissions.write_file
                        && state.native_tool_permissions.run_command,
                    true,
                    compact,
                ),
                tool_setting(state, LocalTool::ReadFile, "Leer archivos", "Leer contenido dentro del proyecto.", compact),
                tool_setting(state, LocalTool::ListFiles, "Explorar carpetas", "Listar archivos dentro del proyecto.", compact),
                tool_setting(state, LocalTool::SearchFiles, "Buscar contenido", "Consultar texto y rutas del proyecto.", compact),
                tool_setting(state, LocalTool::WriteFile, "Modificar archivos", "Incluye escritura y apply_patch; requiere aprobación o una autorización vigente.", compact),
                tool_setting(state, LocalTool::RunCommand, "Ejecutar comandos", "Incluye procesos persistentes y entrada a la terminal; requiere aprobación o una autorización vigente.", compact),
                page_note("Plan y Build", "Plan usa herramientas de lectura. Build permite cambios según estos permisos; activar un permiso no ejecuta ninguna herramienta.", Icon::Chat),
            ],
        ),
        settings_parts::settings_group(
            "Límites del proveedor",
            vec![settings_parts::switch_row(
                "settings_auto_resume_toggle",
                "Continuar automáticamente cuando se renueve el límite",
                "Un turno en pausa por un límite de uso retoma solo a la hora que informa el proveedor, sin reenviar tu mensaje. Nunca continúa solo si falta crédito, hay que reconectar la cuenta o faltan más de 24 horas.",
                state.pause.auto_enabled,
                true,
                compact,
            )],
        ),
        settings_parts::settings_group("Ejecución de comandos", isolation_rows),
        external_agents::review_model_group(state, compact),
        settings_parts::settings_group(
            "Chequeo al terminar",
            vec![
                settings_parts::settings_row(
                    "Comando de chequeo de este proyecto",
                    "Se ejecuta con sh en la carpeta del proyecto después de cada turno que modifique archivos o ejecute comandos. Si falla, aparece un aviso sobre el composer con la acción Corregir. Dejalo vacío y presioná Enter para desactivarlo.",
                    field("turn_check_command", &state.turn_check_command, "p. ej. cargo test")
                        .size(Some(260.0), None),
                    compact,
                ),
                page_note(
                    "No inicia turnos por su cuenta",
                    "El chequeo nunca envía un mensaje ni reemplaza lo que estés escribiendo: Corregir agrega la salida del fallo a tu borrador y vos decidís enviarlo. Se detiene a los 120 segundos.",
                    Icon::InfoCircle,
                ),
            ],
        ),
        settings_parts::settings_group("Capacidades conectadas", vec![
            settings_parts::settings_row("Subagentes", "Estado, permisos, instrucciones pendientes y recuperación de los agentes delegados.", outline("settings_page:subagents", "Ver subagentes").icon(SettingsPage::Subagents.icon()).render(), compact),
            settings_parts::settings_row("Contexto y objetivos", "Historial, compactación, instrucciones y objetivo guardado de la conversación.", outline("settings_page:memory", "Ver contexto").icon(Icon::History).render(), compact),
            settings_parts::settings_row("Herramientas e instrucciones externas", "Configura los servidores MCP, skills y hooks desde sus secciones.", outline("settings_page:mcp", "Ver MCP").icon(SettingsPage::Mcp.icon()).render(), compact),
        ]),
    ]);
    if let Some(error) = &state.native_provider_registry_error {
        content.insert(
            0,
            settings_parts::feedback_banner(error, FeedbackTone::Error, None, compact),
        );
    }
    content
}

fn selected_native_route(
    state: &ViewState,
) -> Option<&crate::composer_selection::ModelRouteOption> {
    state.composer_routes.iter().find(|route| {
        route.backend == "native"
            && route.provider_id == state.native_provider_id
            && route.model_id == state.model
    })
}

fn native_route_status(state: &ViewState) -> &'static str {
    use crate::composer_selection::{CredentialAvailability, ExecutionSupport};
    let Some(route) = selected_native_route(state) else {
        return "Ruta sin verificación disponible; revisa la conexión y las capacidades del modelo.";
    };
    if route.provider_listed == Some(false) || route.execution == ExecutionSupport::Unsupported {
        return "Esta ruta no está disponible para ejecutar el agente.";
    }
    match route.credential {
        CredentialAvailability::MissingCredential => "Falta la credencial del proveedor.",
        CredentialAvailability::EnvPresentUnverified => {
            "Credencial detectada; el acceso de generación aún debe verificarse con el proveedor."
        }
        CredentialAvailability::NoKeyRequired => {
            "Endpoint configurado sin clave; el acceso de generación aún debe verificarse."
        }
        CredentialAvailability::ExternalCliManaged => {
            "Esta credencial no autoriza una conexión directa al proveedor."
        }
    }
}

fn tool_setting(
    state: &ViewState,
    tool: LocalTool,
    title: &str,
    description: &str,
    compact: bool,
) -> Element {
    settings_parts::switch_row(
        format!("agent_tool:{}", tool.name()),
        title,
        description,
        state.native_tool_permissions.allows(tool),
        true,
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
    let editable = local.load_error.is_none() && local.catalog_error.is_none();
    let mut content = Vec::new();
    for (title, error) in [
        (
            "No se pudo cargar la selección local",
            local.load_error.as_ref(),
        ),
        (
            "No se pudo validar el catálogo local",
            local.catalog_error.as_ref(),
        ),
        (
            "La selección guardada requiere revisión",
            local.activation_error.as_ref(),
        ),
    ] {
        if let Some(error) = error {
            content.push(settings_parts::feedback_banner(
                format!("{title}: {error}"),
                FeedbackTone::Error,
                None,
                compact,
            ));
        }
    }
    if local.load_error.is_some() {
        content.push(settings_parts::settings_group("Recuperar configuración", vec![
            settings_parts::settings_row("Restablecer selección", "Reemplaza el archivo inválido y borra las selecciones de todos los proyectos después de confirmar.", outline("native_skill_reset", "Restablecer").icon(Icon::Trash).render(), compact),
        ]));
    }
    content.extend(external_agents::plugin_skills_group(state, compact));
    content.push(settings_parts::settings_group(
        "Marketplace",
        vec![
            settings_parts::value_row(
                "Skills instaladas",
                "Instrucciones disponibles por usuario o por proyecto.",
                format!("{enabled} activas · {installed} instaladas"),
                compact,
            ),
            settings_parts::settings_row(
                "Explorar y administrar",
                "Busca skills, agrega fuentes y administra instalaciones.",
                outline("open_marketplace", "Abrir Marketplace")
                    .icon(Icon::ArrowRight)
                    .render(),
                compact,
            ),
        ],
    ));
    let mut selected_rows = vec![settings_parts::value_row(
        "Proyecto actual",
        &local.workspace,
        format!("{} seleccionadas", local.selected.len()),
        compact,
    )];
    if local.selected.is_empty() {
        selected_rows.push(page_note("Sin selección local", "Selecciona instrucciones del catálogo de este proyecto para incluirlas en los próximos turnos del agente nativo.", SettingsPage::Skills.icon()));
    }
    selected_rows.push(settings_parts::settings_row(
        "Vaciar selección del proyecto",
        "Quita todas las selecciones locales de este proyecto después de confirmar. Conserva los documentos y las selecciones de otros proyectos; permite recuperar varias skills cambiadas o ausentes sin aceptar contenido nuevo.",
        outline("native_skill_clear_workspace", "Vaciar selección")
            .icon(Icon::Trash)
            .enabled(local.load_error.is_none() && !local.selected.is_empty())
            .render(),
        compact,
    ));
    for (index, selection) in local.selected.iter().enumerate() {
        let exact = local
            .available
            .iter()
            .any(|entry| entry.selection == *selection);
        let can_review = editable
            && local
                .available
                .iter()
                .any(|entry| entry.name.eq_ignore_ascii_case(&selection.name));
        selected_rows.push(settings_parts::settings_row(
            &selection.name,
            if exact {
                "El hash observado coincide con la selección guardada. Se verifica de nuevo al activar las instrucciones."
            } else {
                "El documento cambió, desapareció o el catálogo no está disponible."
            },
            ButtonGroup::new(vec![
                outline(format!("native_skill_preview_selected:{index}"), "Revisar")
                    .icon(Icon::History)
                    .enabled(can_review)
                    .render(),
                outline(format!("native_skill_remove:{index}"), "Quitar")
                    .icon(Icon::Close)
                    .enabled(editable)
                    .render(),
            ]).stacked(compact).render(),
            compact,
        ));
    }
    content.push(settings_parts::settings_group(
        "Selección del agente nativo",
        selected_rows,
    ));
    let mut catalog_rows = vec![settings_parts::settings_row(
        "Documentos disponibles",
        "Cada documento se verifica de nuevo antes de un turno. Estar en esta lista no lo activa.",
        outline("native_skill_refresh", "Actualizar")
            .icon(Icon::Refresh)
            .enabled(local.load_error.is_none())
            .render(),
        compact,
    )];
    if local.available.is_empty() {
        catalog_rows.push(page_note(
            "Sin documentos locales",
            "No se encontraron skills en las carpetas de proyecto y usuario.",
            Icon::Folder,
        ));
    }
    for (index, entry) in local.available.iter().enumerate() {
        let selected = local
            .selected
            .iter()
            .find(|selection| selection.name.eq_ignore_ascii_case(&entry.name));
        let (label, status) = match selected {
            Some(selection) if *selection == entry.selection => ("Desactivar", "Seleccionada"),
            Some(_) => ("Aceptar cambios", "Documento cambiado"),
            None => ("Seleccionar", "Disponible"),
        };
        let origin = match entry.origin {
            SkillOrigin::Project => "Proyecto",
            SkillOrigin::User => "Usuario",
        };
        let copy = Element::column(vec![
            Text::new(&entry.name, theme::component_palette()).render(),
            Text::new(&entry.description, theme::component_palette())
                .role(TextRole::Small)
                .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                .render(),
            Text::new(format!("{origin} · {status}"), theme::component_palette())
                .role(TextRole::Small)
                .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                .render(),
            Text::new(
                entry.document.display().to_string(),
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
            .mono()
            .render(),
        ])
        .gap(5.0);
        let action = ButtonGroup::new(vec![
            outline(format!("native_skill_preview:{index}"), "Revisar")
                .icon(Icon::History)
                .enabled(editable)
                .render(),
            outline(format!("native_skill_toggle:{index}"), label)
                .icon(Icon::Check)
                .enabled(editable)
                .render(),
        ])
        .stacked(compact)
        .render();
        catalog_rows.push(if compact {
            Element::column(vec![copy, action])
                .gap(10.0)
                .padding(settings_parts::ROW_PADDING)
        } else {
            Element::row(vec![copy.flex(1.0), action])
                .gap(12.0)
                .padding(settings_parts::ROW_PADDING)
        });
    }
    content.push(settings_parts::settings_group(
        "Catálogo local",
        catalog_rows,
    ));
    content.push(settings_parts::settings_group("Carpetas de instrucciones", vec![
        page_note("Proyecto", &local.project_root, Icon::Folder),
        page_note("Usuario", &local.user_root, Icon::Folder),
        page_note("Verificación del documento", "Revisar muestra el SKILL.md completo y su hash observado, sin activarlo. La selección conserva nombre, origen, rutas canónicas y hash; el contenido se verifica de nuevo antes de usarlo. Los archivos referenciados no se cargan automáticamente.", Icon::Check),
    ]));
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
    let mut content = vec![settings_parts::settings_group(
        "Tareas programadas",
        vec![
            settings_parts::value_row(
                "Automatizaciones del proyecto",
                "Horarios locales y ejecuciones guardadas para esta carpeta.",
                format!("{active} activas · {total} en total"),
                compact,
            ),
            settings_parts::settings_row(
                "Administrar tareas",
                "Crea, edita, pausa y ejecuta tareas. Consulta su historial.",
                outline("open_automations", "Abrir automatizaciones")
                    .icon(Icon::ArrowRight)
                    .render(),
                compact,
            ),
            settings_parts::value_row(
                "Historial de ejecución",
                "Ejecuciones registradas por el planificador del proyecto.",
                format!("{} ejecuciones", state.automations.runs.len()),
                compact,
            ),
        ],
    )];
    if let Some(error) = &state.automations.error {
        content.push(settings_parts::feedback_banner(
            error,
            FeedbackTone::Error,
            None,
            compact,
        ));
    }
    content.push(settings_parts::settings_group("Planificador", vec![
        page_note("Mientras OpenIDE esté abierto", "El planificador revisa horarios locales con la aplicación en ejecución. Las tareas no se ejecutan cuando está cerrada.", Icon::Clock),
        page_note("Política de ejecución", "Las automatizaciones del agente nativo trabajan en sólo lectura y conservan su configuración de motor y modelo.", Icon::Check),
    ]));
    content
}

fn data_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let organization = settings_choice_selector(
        state,
        "organization",
        match state.session_organization {
            SessionOrganization::Project => "Proyecto",
            SessionOrganization::Group => "Grupo",
            SessionOrganization::Recent => "Recientes",
        },
        vec![
            DropdownOption::new("session_organize:project", "Proyecto")
                .selected(state.session_organization == SessionOrganization::Project),
            DropdownOption::new("session_organize:group", "Grupo")
                .selected(state.session_organization == SessionOrganization::Group),
            DropdownOption::new("session_organize:recent", "Recientes")
                .selected(state.session_organization == SessionOrganization::Recent),
        ],
    );
    let sorting = settings_choice_selector(
        state,
        "sort",
        match state.session_sort {
            SessionSort::Updated => "Actualizadas",
            SessionSort::Created => "Creadas",
        },
        vec![
            DropdownOption::new("session_sort:updated", "Actualizadas")
                .selected(state.session_sort == SessionSort::Updated),
            DropdownOption::new("session_sort:created", "Creadas")
                .selected(state.session_sort == SessionSort::Created),
        ],
    );
    vec![
        settings_parts::settings_group("Historial local", vec![
            settings_parts::value_row("Tareas visibles", "Conversaciones, eventos y borradores guardados en este equipo.", format!("{} tareas", state.sessions.len()), compact),
            page_note("Archivar y eliminar", "Usa el menú de cada tarea en la barra lateral. Eliminar una tarea requiere confirmación.", Icon::Archive),
        ]),
        settings_parts::settings_group("Barra lateral", vec![
            settings_parts::settings_row("Agrupar tareas", "Organiza la navegación por proyecto, grupo o actividad reciente.", organization, compact),
            settings_parts::settings_row("Ordenar tareas", "La agrupación y el orden se conservan al reiniciar OpenIDE.", sorting, compact),
        ]),
    ]
}

fn github_page(state: &ViewState, compact: bool, content_width: f32) -> Vec<Element> {
    let busy = state.github_account_refreshing
        || state.github_sign_in.is_some()
        || matches!(
            state.github_account,
            GithubAccountStatus::Checking | GithubAccountStatus::Verifying(_)
        );
    let connection_action =
        |id: &str, title: &str, icon: Icon| outline(id, title).icon(icon).enabled(!busy).render();
    let account = match &state.github_account {
        GithubAccountStatus::Connected(identity) => {
            settings_parts::settings_card(vec![Element::row(vec![
                crate::github_account_ui::avatar(identity, 32.0)
                    .centered_y()
                    .size(Some(32.0), None),
                settings_parts::settings_row(
                    format!("@{}", identity.login),
                    if state.github_account_refreshing { "Actualizando conexión…" } else { "Cuenta verificada · GitHub" },
                    outline("github_account_open:settings", "Ver perfil")
                        .icon(Icon::ArrowRight)
                        .render()
                        .dispatch_as("github_account_open"),
                    compact,
                )
                .padding(0.0)
                .flex(1.0),
            ])
            .gap(10.0)
            .padding(settings_parts::ROW_PADDING)])
        }
        GithubAccountStatus::Unverified { accounts, notice } => {
            let mut rows = vec![page_note(
                "Cuentas guardadas",
                notice.as_deref().unwrap_or("Elige la cuenta que quieres usar en OpenIDE."),
                Icon::Github,
            )];
            for account in accounts {
                rows.push(settings_parts::settings_row(
                    format!("@{account}"),
                    "Pendiente de verificar",
                    connection_action(&format!("github_account_verify:{account}"), "Usar cuenta", Icon::Check),
                    compact,
                ));
            }
            settings_parts::settings_card(rows)
        }
        GithubAccountStatus::Checking | GithubAccountStatus::Verifying(_) => {
            settings_parts::loading_state("Comprobando conexión", "Verificando la identidad de la cuenta con GitHub.")
        }
        GithubAccountStatus::Disconnected => settings_parts::settings_card(vec![
            settings_parts::settings_row(
                "Conectar GitHub",
                "Consulta tus repositorios, pull requests y contribuciones con la cuenta que elijas.",
                connection_action("github_account_connect", "Conectar", Icon::Github),
                compact,
            ),
        ]),
        GithubAccountStatus::Unavailable(error) => {
            Alert::new("Conexión no disponible", error, theme::component_palette())
                .tone(FeedbackTone::Error)
                .compact(true)
                .action(connection_action("github_account_connect", "Reconectar", Icon::Github))
                .render()
        }
    };
    let mut content = vec![account];
    if let Some(notice) = &state.github_account_notice {
        content.push(settings_parts::feedback_banner(
            notice,
            FeedbackTone::Warning,
            None,
            compact,
        ));
    }
    let connected = matches!(state.github_account, GithubAccountStatus::Connected(_));
    let mut management = vec![settings_parts::settings_row(
        "Actualizar conexión",
        "Comprueba de nuevo el estado de la cuenta y sus datos.",
        connection_action("github_account_refresh", "Actualizar", Icon::Refresh),
        compact,
    )];
    if connected || matches!(state.github_account, GithubAccountStatus::Unverified { .. }) {
        management.push(settings_parts::settings_row(
            "Cambiar de cuenta",
            "Conecta otra cuenta desde el navegador.",
            connection_action("github_account_connect", "Conectar otra cuenta", Icon::Add),
            compact,
        ));
        management.push(settings_parts::settings_row(
            "Desconectar de OpenIDE",
            "Conserva tus credenciales de GitHub CLI y las demás aplicaciones.",
            outline("github_account_disconnect", "Desconectar")
                .icon(Icon::CloseCircle)
                .render(),
            compact,
        ));
    }
    content.push(settings_parts::settings_group("Conexión", management));
    if connected {
        content.push(activity_charts::github_card(state, content_width));
    }
    content.push(settings_parts::settings_group(
        "Integración del proyecto",
        vec![
            page_note(
                "Repositorio y pull requests",
                "Las consultas privadas usan la cuenta seleccionada en OpenIDE.",
                Icon::GitBranch,
            ),
            settings_parts::settings_row(
                "Proveedores de modelos",
                "Las conexiones del agente se configuran por separado.",
                outline("settings_page:providers", "Ver conexiones")
                    .icon(Icon::ArrowRight)
                    .render(),
                compact,
            ),
        ],
    ));
    content
}

pub(crate) fn catalog_model_usable(
    provider_id: &str,
    model: &crate::model_catalog::ModelInfo,
) -> bool {
    crate::provider_settings::catalog_model_usable(provider_id, model)
}

fn providers_page(state: &ViewState, compact: bool, content_width: f32) -> Vec<Element> {
    providers::page(state, compact, content_width)
}

pub fn provider_model_details_modal(
    state: &ViewState,
    base: Element,
    viewport_width: f32,
) -> Element {
    providers::details_modal(state, base, viewport_width)
}

pub fn provider_connect_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    providers::connect_modal(state, base, viewport_width)
}

// Top-aligned on both sides: the keycaps stay on the first line when the
// description wraps.
fn shortcut_row(description: &str, keys: &str) -> Element {
    ActionRow::new(description, theme::component_palette())
        .wrap_title(true)
        .align_top(true)
        .trailing(Kbd::new(keys, theme::component_palette()).render())
        .trailing_top(true)
        .sizing(crate::ui_sizing::SHORTCUT_ROW)
        .render()
}

fn keymap_selector(state: &ViewState) -> Element {
    let options = KeymapProfile::ALL
        .iter()
        .copied()
        .map(|profile| {
            DropdownOption::new(format!("settings_keymap:{}", profile.id()), profile.label())
                .selected(profile == state.keymap_profile)
        })
        .collect();
    Select::new(
        "settings_keymap_toggle",
        state.keymap_profile.label(),
        options,
        state.settings_keymap_menu_open,
        "settings_keymap_dismiss",
        control_palette(),
    )
    .menu_palette(theme::menu_palette())
    .sizing(SelectorSizing::COMPACT)
    .render()
}

fn shortcuts_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let mut cards = vec![settings_parts::settings_group(
        "Perfil de teclado",
        vec![settings_parts::settings_row(
            "Distribución de atajos",
            state.keymap_profile.description(),
            keymap_selector(state),
            compact,
        )],
    )];
    cards.push(settings_parts::settings_group(
        "Edición de texto",
        vec![
            settings_parts::switch_row(
                "settings_vim_toggle",
                "Modo Vim",
                "Edición modal en el editor del IDE y de agentes. Independiente del perfil de atajos.",
                state.vim_mode,
                true,
                compact,
            ),
            page_note(
                "Normal, Insertar y Visual",
                "i para escribir · Esc para volver a Normal · h j k l para moverse · v para seleccionar · u para deshacer. Las teclas de Vim tienen prioridad dentro del editor.",
                Icon::InfoCircle,
            ),
        ],
    ));
    for group in ["Navegación", "Paneles", "Conversación"] {
        let rows: Vec<_> = ShortcutAction::ALL
            .iter()
            .copied()
            .filter(|action| action.group() == group)
            .filter_map(|action| {
                let shortcut = state.keymap_profile.shortcut(action);
                (!shortcut.is_empty()).then(|| shortcut_row(action.label(), shortcut))
            })
            .collect();
        if !rows.is_empty() {
            cards.push(settings_parts::settings_group(group, rows));
        }
    }
    cards
}

fn capability_page(
    title: &str,
    current: &str,
    needed: &str,
    related: Option<(&str, &str, Icon)>,
) -> Vec<Element> {
    let related = related.map(|(id, label, icon)| outline(id, label).icon(icon).render());
    vec![
        settings_parts::empty_state(title, current, related),
        settings_parts::settings_group(
            "Disponibilidad",
            vec![page_note("Estado actual", needed, Icon::Clock)],
        ),
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

    let mut servers = vec![settings_parts::settings_row(
        "Servidores guardados",
        format!("{} configurados en este equipo", state.mcp_servers.len()),
        outline("mcp_new", "Nuevo servidor")
            .icon(Icon::Add)
            .enabled(editable)
            .render(),
        compact,
    )];
    if state.mcp_servers.is_empty() {
        servers.push(page_note(
            "Sin servidores",
            "Agrega un proceso local o un endpoint HTTP para conectar herramientas al agente nativo.",
            SettingsPage::Mcp.icon(),
        ));
    }
    for server in &state.mcp_servers {
        let status = if server.enabled {
            "Activado para turnos"
        } else {
            "Desactivado"
        };
        let toggle_label = if server.enabled {
            "Desactivar"
        } else {
            "Activar"
        };
        let inspection_busy = state.mcp_inspection.is_busy()
            && state
                .mcp_inspection
                .scope
                .as_ref()
                .is_some_and(|scope| scope.server.id == server.id);
        let inspection = outline(
            &format!("mcp_inspect:{}", server.id),
            "Inspeccionar herramientas",
        )
        .icon(SettingsPage::Mcp.icon())
        .enabled(editable && server.enabled && !state.running && !state.mcp_inspection.is_busy())
        .loading(inspection_busy)
        .render();
        let controls = vec![
            outline(format!("mcp_select:{}", server.id), "Editar")
                .icon(Icon::Settings)
                .enabled(editable)
                .render(),
            outline(format!("mcp_toggle:{}", server.id), toggle_label)
                .icon(Icon::Check)
                .enabled(editable)
                .render(),
            outline(format!("mcp_delete:{}", server.id), "Eliminar")
                .icon(Icon::Trash)
                .enabled(editable)
                .render(),
        ];
        let mut oauth_controls = Vec::new();
        let mut server_content = vec![
            Element::row(vec![
                Text::new(&server.id, theme::component_palette())
                    .render()
                    .flex(1.0),
                Text::new(status, theme::card_palette()).muted().render(),
            ])
            .gap(10.0),
            Text::new(
                match &server.url {
                    Some(url) => format!("Streamable HTTP: {url}"),
                    None => format!("Programa: {}", server.program),
                },
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
            .render(),
        ];
        if let Some(oauth) = &server.http_oauth {
            server_content.push(
                Text::new(
                    format!(
                        "OAuth 2.1 · issuer: {} · scopes: {}",
                        oauth.issuer,
                        if oauth.scopes.is_empty() {
                            "los solicitados por el servidor".to_owned()
                        } else {
                            oauth.scopes.join(", ")
                        }
                    ),
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                .render(),
            );
            let oauth_busy = state.mcp_oauth_status.as_ref().is_some_and(|status| {
                status.server_id == server.id
                    && matches!(
                        status.phase,
                        McpOAuthViewPhase::Discovering | McpOAuthViewPhase::AwaitingBrowser { .. }
                    )
            });
            oauth_controls.push(
                outline(format!("mcp_oauth_authorize:{}", server.id), "Autorizar")
                    .icon(Icon::ArrowRight)
                    .enabled(editable && cfg!(windows) && !oauth_busy)
                    .loading(oauth_busy)
                    .render(),
            );
            if !cfg!(windows) {
                server_content.push(
                    Text::new(
                        "OAuth MCP requiere Credential Manager en Windows.",
                        theme::component_palette(),
                    )
                    .role(TextRole::Small)
                    .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                    .render(),
                );
            }
            if let Some(status) = state
                .mcp_oauth_status
                .as_ref()
                .filter(|status| status.server_id == server.id)
            {
                match &status.phase {
                    McpOAuthViewPhase::Discovering => {
                        server_content.push(Text::new("Buscando el servidor de autorización", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render());
                        oauth_controls.push(
                            outline("mcp_oauth_cancel", "Cancelar")
                                .icon(Icon::Close)
                                .render(),
                        );
                    }
                    McpOAuthViewPhase::AwaitingBrowser { url } => {
                        server_content.push(Text::new("Esperando autorización en el navegador. La URL es temporal:", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render());
                        server_content.push(
                            Element::panel(
                                Text::new(url, theme::component_palette()).ink(theme::SETTINGS_FOREGROUND_SUBTLE).mono().render(),
                            )
                            .padding(10.0)
                            .background(theme::INPUT)
                            .border(theme::BORDER, 1.0)
                            .radius(6.0),
                        );
                        oauth_controls.push(
                            outline("mcp_oauth_open", "Abrir autorización")
                                .icon(Icon::ArrowRight)
                                .render(),
                        );
                        oauth_controls.push(
                            outline("mcp_oauth_cancel", "Cancelar")
                                .icon(Icon::Close)
                                .render(),
                        );
                    }
                    McpOAuthViewPhase::Authorized => server_content.push(Text::new("Autorización guardada en Credential Manager. El próximo turno usará esta credencial.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render()),
                    McpOAuthViewPhase::Failed(message) => server_content.push(
                        settings_parts::feedback_banner(message, FeedbackTone::Error, None, compact),
                    ),
                }
            }
        }
        server_content.push(inspection);
        server_content.push(ButtonGroup::new(controls)
            .sizing(crate::ui_sizing::ENTITY_ACTIONS)
            .stacked(compact)
            .render());
        if !oauth_controls.is_empty() {
            server_content.push(ButtonGroup::new(oauth_controls)
                .sizing(crate::ui_sizing::ENTITY_ACTIONS)
                .stacked(compact)
                .render());
        }
        servers.push(
            Element::column(server_content)
                .gap(10.0)
                .padding(settings_parts::ROW_PADDING),
        );
    }
    content.push(settings_parts::settings_group("Servidores MCP", servers));
    content.push(mcp_inspection::render(&state.mcp_inspection, compact));

    content.push(settings_parts::settings_group(
        "Uso",
        vec![
            page_note("Conexión al ejecutar", "Activar un servidor lo incluye en los próximos turnos del agente nativo en modo Build. La conexión, el protocolo y las herramientas se comprueban durante la ejecución.", SettingsPage::Mcp.icon()),
            page_note("Aprobación de herramientas", "Sus herramientas solicitan aprobación. Los servidores OAuth necesitan autorización explícita antes de usarse.", Icon::Check),
        ],
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
    let mut rows = vec![settings_parts::settings_row(
        "Hooks de comandos guardados",
        format!("{} configurados en este equipo", settings.hooks.len()),
        outline("hook_new", "Nuevo hook")
            .icon(Icon::Add)
            .enabled(editable)
            .render(),
        compact,
    )];
    if settings.hooks.is_empty() {
        rows.push(page_note(
            "Sin hooks",
            "Agrega acciones que se ejecuten antes o después de una herramienta del agente nativo.",
            SettingsPage::Hooks.icon(),
        ));
    }
    for hook in &settings.hooks {
        let phase = match hook.phase {
            HookPhase::PreToolUse => "Antes de herramienta",
            HookPhase::PostToolUse => "Después de herramienta",
        };
        let controls = vec![
            outline(format!("hook_select:{}", hook.id), "Editar")
                .icon(Icon::Settings)
                .enabled(editable)
                .render(),
            outline(format!("hook_delete:{}", hook.id), "Eliminar")
                .icon(Icon::Trash)
                .enabled(editable)
                .render(),
        ];
        rows.push(
            Element::column(vec![
                Element::row(vec![
                    Text::new(&hook.id, theme::component_palette())
                        .render()
                        .flex(1.0),
                    Text::new(phase, theme::card_palette()).muted().render(),
                ])
                .gap(10.0),
                Text::new(
                    format!("{} · {}", hook.tool_name, hook.executable.display()),
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                .render(),
                ButtonGroup::new(controls)
                    .sizing(crate::ui_sizing::ENTITY_ACTIONS)
                    .stacked(compact)
                    .render(),
            ])
            .gap(10.0)
            .padding(settings_parts::ROW_PADDING),
        );
    }
    content.push(settings_parts::settings_group(
        "Hooks del agente nativo",
        rows,
    ));
    content.extend(external_agents::compat_hooks_group(state, compact));
    content.push(settings_parts::settings_group("Alcance y seguridad", vec![
        page_note("Antes y después de herramientas", "PreToolUse puede permitir o bloquear. PostToolUse observa el resultado y no revierte una acción. Los comandos configurados usan límites de tiempo y salida.", SettingsPage::Hooks.icon()),
        page_note("Configuración del turno", "Se aplican a turnos interactivos del agente nativo. Reanudar un turno previo requiere restaurar la misma configuración de hooks.", Icon::History),
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
    let width = Modal::fit_width(viewport_width, 560.0);
    let body_width = crate::ui_sizing::form_width(width).max(0.0);
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
    // 40 tall is these forms' own height for an input.
    let input = |id: &str, value: &str, placeholder: &str| {
        mono_field(id, value, placeholder).width(body_width).height(40.0).render()
    };
    fields.extend([
        form_field("ID del hook", input("hook_draft_id", &draft.draft_id, "ej. revisar-escritura")).rows(),
        form_field(
            "Fase",
            ButtonGroup::new(vec![
                outline("hook_phase:pre_tool_use", "Antes de herramienta")
                    .active(draft.draft_phase == HookPhase::PreToolUse)
                    .render(),
                outline("hook_phase:post_tool_use", "Después de herramienta")
                    .active(draft.draft_phase == HookPhase::PostToolUse)
                    .render(),
            ]).stacked(width < 420.0).render(),
        )
        .rows(),
        form_field("Nombre exacto de herramienta", input("hook_draft_tool_name", &draft.draft_tool_name, "ej. write_file"))
            .hint("Por ejemplo: read_file, write_file o mcp__servidor__herramienta. Los hooks sobre lecturas quedan excluidos del modo Plan.")
            .rows(),
        form_field("Ejecutable", input("hook_draft_executable", &draft.draft_executable, "C:\\tools\\hook.exe"))
            .hint("Ruta absoluta a un archivo existente. Se ejecuta sin shell; el agente no puede configurar esta ruta.")
            .rows(),
        form_field(
            "Argumentos (JSON)",
            TextArea::new("hook_draft_args", &draft.draft_args, "[\"--check\"]", control_palette())
                .width(body_width).height(80.0).render(),
        )
        .rows(),
        form_field("Directorio de trabajo", input("hook_draft_cwd", &draft.draft_cwd, "C:\\repo"))
            .hint("Ruta absoluta a una carpeta existente.")
            .rows(),
        form_field("Límite de tiempo (ms)", input("hook_draft_timeout_ms", &draft.draft_timeout_ms, "5000")).rows(),
        form_field("Límite de salida (bytes)", input("hook_draft_max_output_bytes", &draft.draft_max_output_bytes, "4096")).rows(),
        vec![Text::new("El comando recibe JSON del evento por stdin. Puede tener efectos externos; sólo configura ejecutables de confianza.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render()],
    ].into_iter().flatten());
    let cancel = outline("hook_cancel", "Cancelar").render();
    let save = theme::primary(
        "hook_save",
        if editing {
            "Guardar cambios"
        } else {
            "Agregar hook"
        },
    )
    .size(ControlSize::Compact)
    .enabled(draft.load_error.is_none())
    .render();
    let footer = form_footer(cancel, save, width);
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
        content.push(settings_parts::settings_group(
            "Recuperar configuración",
            vec![settings_parts::settings_row(
                "Restablecer reglas",
                "Reemplaza el archivo inválido por una política vacía después de tu confirmación.",
                outline("exec_rule_reset", "Restablecer reglas")
                    .icon(Icon::Trash)
                    .render(),
                compact,
            )],
        ));
    }
    let mut rows = vec![settings_parts::settings_row(
        "Reglas de prefijo guardadas",
        format!("{} configuradas en este equipo", settings.rules.len()),
        outline("exec_rule_new", "Nueva regla")
            .icon(Icon::Add)
            .enabled(editable)
            .render(),
        compact,
    )];
    if settings.rules.is_empty() {
        rows.push(page_note("Sin reglas adicionales", "Los comandos mantienen la política de permisos del agente. Agrega prefijos para pedir aprobación o bloquearlos.", Icon::Terminal));
    }
    for (index, rule) in settings.rules.iter().enumerate() {
        let decision = match rule.decision() {
            ExecDecision::Ask => "Pedir aprobación",
            ExecDecision::Deny => "Bloquear",
            ExecDecision::Allow => "Permitir",
        };
        let pattern = serde_json::to_string(rule.pattern()).unwrap_or_default();
        let mut body = vec![Element::row(vec![
            ScrollArea::new(
                format!("exec_rule_pattern:{index}"),
                Text::new(pattern, theme::component_palette())
                    .mono()
                    .render(),
            )
            .render()
            .size(None, Some(80.0))
            .flex(1.0),
            Text::new(decision, theme::card_palette()).muted().render(),
        ])
        .gap(8.0)];
        if let Some(justification) = rule.justification() {
            body.push(
                Text::new(justification, theme::component_palette())
                    .role(TextRole::Small)
                    .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                    .render(),
            );
        }
        let controls = vec![
            outline(format!("exec_rule_select:{index}"), "Editar")
                .icon(Icon::Settings)
                .enabled(editable)
                .render(),
            outline(format!("exec_rule_delete:{index}"), "Eliminar")
                .icon(Icon::Trash)
                .enabled(editable)
                .render(),
        ];
        body.push(ButtonGroup::new(controls)
            .sizing(crate::ui_sizing::ENTITY_ACTIONS)
            .stacked(compact)
            .render());
        rows.push(
            Element::column(body)
                .gap(10.0)
                .padding(settings_parts::ROW_PADDING),
        );
    }
    content.push(settings_parts::settings_group(
        "Reglas para comandos del agente",
        rows,
    ));
    content.push(rules_preview::group(state, compact));
    content.push(settings_parts::settings_group(
        "Cómo se aplican",
        vec![
            page_note("Prefijos exactos", "Cada regla incluye el ejecutable y sus argumentos. Puedes declarar alternativas por argumento. Si varias coinciden, se aplica la más restrictiva.", Icon::Terminal),
            page_note("Aprobación y bloqueo", "Las reglas respetan los permisos de herramientas del agente. Los intérpretes de shell requieren comprobaciones adicionales.", Icon::Check),
            page_note("Próximos turnos", "Los cambios afectan las próximas ejecuciones. Reanudar un turno previo requiere la misma política que tenía al iniciarse.", Icon::History),
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
    let width = Modal::fit_width(viewport_width, 560.0);
    let body_width = crate::ui_sizing::form_width(width).max(0.0);
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
        form_field(
            "Prefijo de comando (JSON)",
            TextArea::new(
                "exec_rule_draft_pattern",
                &draft.draft_pattern,
                "[\"git\",\"push\"]",
                control_palette(),
            )
            .width(body_width)
            .height(100.0)
            .render(),
        )
        .hint("La vista previa de Settings usa las reglas guardadas. Guarda esta edición para evaluar sus cambios.")
        .hint("Ejemplo: [\"git\",[\"push\",\"reset\"]]. El primer token es el ejecutable; los siguientes son argumentos exactos.")
        .rows(),
        form_field(
            "Decisión",
            ButtonGroup::new(vec![
                outline("exec_rule_decision:ask", "Pedir aprobación")
                    .active(draft.draft_decision == ExecDecision::Ask)
                    .render(),
                outline("exec_rule_decision:deny", "Bloquear")
                    .active(draft.draft_decision == ExecDecision::Deny)
                    .render(),
            ]).stacked(width < 420.0).render(),
        )
        .rows(),
        form_field(
            "Motivo (opcional)",
            TextArea::new(
                "exec_rule_draft_justification",
                &draft.draft_justification,
                "Explicá por qué se aplica esta regla",
                control_palette(),
            )
            .width(body_width)
            .height(75.0)
            .render(),
        )
        .rows(),
    ].into_iter().flatten());
    let cancel = outline("exec_rule_cancel", "Cancelar").render();
    let save = theme::primary(
        "exec_rule_save",
        if editing {
            "Guardar cambios"
        } else {
            "Agregar regla"
        },
    )
    .size(ControlSize::Compact)
    .enabled(draft.load_error.is_none())
    .render();
    let footer = form_footer(cancel, save, width);
    Modal::new(
        base,
        true,
        if editing {
            "Editar regla"
        } else {
            "Nueva regla"
        },
        "Configura una decisión para un prefijo de comando del agente nativo.",
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
    let width = Modal::fit_width(viewport_width, 480.0);
    let cancel = outline("exec_rule_reset_cancel", "Cancelar").render();
    let confirm = theme::primary("exec_rule_reset_confirm", "Reemplazar archivo")
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
    .body(Text::new("Esta acción reemplaza el archivo de reglas por una política vacía. Los próximos turnos usarán los permisos generales de herramientas.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render())
    .footer(
        ButtonGroup::new(vec![cancel, confirm])
            .align_end()
            .sizing(crate::ui_sizing::CONFIRM_FOOTER)
            .render(),
    )
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
    let width = Modal::fit_width(viewport_width, 480.0);
    let cancel = outline("native_skill_reset_cancel", "Cancelar").render();
    let confirm = theme::primary("native_skill_reset_confirm", "Reemplazar archivo")
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
    .body(Text::new("Esta acción borra las selecciones de skills locales de todos los proyectos. Se pueden volver a seleccionar desde esta página.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render())
    .footer(
        ButtonGroup::new(vec![cancel, confirm])
            .align_end()
            .sizing(crate::ui_sizing::CONFIRM_FOOTER)
            .render(),
    )
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

    let width = Modal::fit_width(viewport_width, 560.0);
    // The shared modal reserves a 14 px scrollbar rail beyond its 20 px
    // horizontal padding, so form controls never sit beneath the thumb.
    let body_width = crate::ui_sizing::form_width(width).max(0.0);
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
    // 40 tall is these forms' own height for an input.
    let input = |id: &str, value: &str, placeholder: &str| {
        mono_field(id, value, placeholder).width(body_width).height(40.0).render()
    };
    fields.extend([
        form_field("ID del servidor", input("mcp_draft_id", &state.mcp_draft_id, "ej. filesystem"))
            .hint("Minúsculas, números, guiones y guiones bajos. El ID empieza con una letra.")
            .rows(),
        form_field(
            "Transporte",
            ButtonGroup::new(vec![
                outline("mcp_transport:stdio", "Proceso local (stdio)")
                    .active(!state.mcp_draft_http)
                    .render(),
                outline("mcp_transport:http", "Streamable HTTP")
                    .active(state.mcp_draft_http)
                    .render(),
            ])
            .stacked(width < 420.0)
            .render(),
        )
        .rows(),
    ].into_iter().flatten());
    if state.mcp_draft_http {
        fields.extend([
            form_field("URL MCP", input("mcp_draft_url", &state.mcp_draft_url, "https://example.com/mcp"))
                .hint("Endpoint HTTPS; HTTP sólo para localhost o loopback. Sin query, credenciales en URL ni redirecciones.")
                .rows(),
            form_field(
                "Autenticación",
                ButtonGroup::new(vec![
                    outline("mcp_auth:bearer", "Bearer por entorno o ninguna")
                        .active(!state.mcp_draft_oauth_enabled)
                        .render(),
                    outline("mcp_auth:oauth", "OAuth 2.1")
                        .active(state.mcp_draft_oauth_enabled)
                        .enabled(cfg!(windows) || state.mcp_draft_oauth_enabled)
                        .render(),
                ])
                .stacked(width < 420.0)
                .render(),
            )
            .rows(),
        ].into_iter().flatten());
        if state.mcp_draft_oauth_enabled {
            fields.extend([
                vec![Text::new("Usá un client ID público registrado para este issuer. La autorización se inicia después de guardar; no escribas client secrets ni tokens.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render()],
                form_field("Client ID público", input("mcp_draft_oauth_client_id", &state.mcp_draft_oauth_client_id, "client-id")).rows(),
                form_field("Issuer OAuth", input("mcp_draft_oauth_issuer", &state.mcp_draft_oauth_issuer, "https://auth.example.com"))
                    .hint("URL HTTPS exacta del servidor de autorización, sin query ni credenciales.")
                    .rows(),
                form_field(
                    "Scopes solicitados (opcional)",
                    TextArea::new(
                        "mcp_draft_oauth_scopes",
                        &state.mcp_draft_oauth_scopes,
                        "read\nwrite",
                        control_palette(),
                    )
                    .width(body_width)
                    .height(76.0)
                    .render(),
                )
                .hint("Uno por línea. Se unen con los scopes requeridos por el servidor; no son valores secretos.")
                .rows(),
                vec![Text::new("Los tokens se guardan en Credential Manager del usuario en Windows, fuera de Settings y del journal. Para usar el servidor, guardá y elegí Autorizar.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render()],
            ].into_iter().flatten());
        } else {
            fields.extend(
                form_field(
                    "Variable del bearer token (opcional)",
                    input(
                        "mcp_draft_http_bearer_token_env_name",
                        &state.mcp_draft_http_bearer_token_env_name,
                        "ej. MCP_HTTP_TOKEN",
                    ),
                )
                .hint("Escribe sólo el nombre de la variable. El valor se lee al conectar y no se guarda. Déjalo vacío si el servidor no exige autenticación.")
                .rows(),
            );
        }
        fields.push(Text::new("Streamable HTTP negocia el protocolo al conectar: 2026-07-28 o las versiones compatibles de 2025. Solicitudes de entrada adicional aún no están disponibles.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render());
    } else {
        fields.extend([
            form_field("Programa", input("mcp_draft_program", &state.mcp_draft_program, "ej. npx"))
                .hint("Nombre del ejecutable o ruta absoluta. Se inicia sin shell.")
                .rows(),
            form_field(
                "Argumentos (JSON)",
                TextArea::new(
                    "mcp_draft_args",
                    &state.mcp_draft_args,
                    "[\"-y\", \"@modelcontextprotocol/server-filesystem\"]",
                    control_palette(),
                )
                .width(body_width)
                .height(96.0)
                .render(),
            )
            .hint("Lista JSON de textos; cada elemento es un argumento literal. No escribas tokens, claves ni contraseñas aquí.")
            .rows(),
            form_field("Directorio de trabajo", input("mcp_draft_cwd", &state.mcp_draft_cwd, "ej. tools/mcp"))
                .hint("Ruta relativa dentro del proyecto. Se comprueba al iniciar; vacío usa la carpeta del proyecto.")
                .rows(),
            form_field(
                "Variables de credenciales",
                TextArea::new(
                    "mcp_draft_env_names",
                    &state.mcp_draft_env_names,
                    "ej. MCP_API_KEY",
                    control_palette(),
                )
                .width(body_width)
                .height(76.0)
                .render(),
            )
            .hint("Sólo nombres, uno por línea. Sus valores se leen del entorno al iniciar el servidor y no se guardan.")
            .rows(),
        ].into_iter().flatten());
    }
    fields.push(Text::new("Los servidores nuevos se guardan desactivados. Usa Activar en la lista cuando estén listos.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render());
    let cancel = outline("mcp_cancel", "Cancelar").render();
    let save = theme::primary(
        "mcp_save",
        if editing {
            "Guardar cambios"
        } else {
            "Agregar servidor"
        },
    )
    .size(ControlSize::Compact)
    .enabled(state.mcp_load_error.is_none())
    .render();
    let footer = form_footer(cancel, save, width);

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
            "Interfaz",
            vec![
                settings_parts::switch_row(
                    "settings_reduced_motion",
                    "Reducir movimiento",
                    "Usa transiciones mínimas al cambiar de panel o abrir superficies.",
                    state.settings_reduced_motion,
                    true,
                    compact,
                ),
            ],
        ),
    ]
}

fn terminal_page(state: &ViewState, compact: bool) -> Vec<Element> {
    let shell = &state.settings_terminal_shell;
    let shell_name = match shell {
        TerminalShell::System
            if std::env::var_os("OPENIDE_TERMINAL_SHELL")
                .is_some_and(|value| !value.is_empty()) =>
        {
            "Sistema (del entorno)"
        }
        TerminalShell::System => "Predeterminada del sistema",
        TerminalShell::PowerShell => "PowerShell",
        TerminalShell::PowerShellCore => "PowerShell 7",
        TerminalShell::CommandPrompt => "Símbolo del sistema",
        TerminalShell::Bash => "Bash",
        TerminalShell::Zsh => "Zsh",
        TerminalShell::Fish => "Fish",
        TerminalShell::Custom(_) => "Personalizado",
    };
    let mut choices = vec![DropdownOption::new(
        "settings_terminal_shell:system",
        "Predeterminada del sistema",
    )
    .selected(matches!(shell, TerminalShell::System))];
    if cfg!(windows) {
        choices.extend([
            DropdownOption::new("settings_terminal_shell:powershell", "PowerShell")
                .selected(matches!(shell, TerminalShell::PowerShell)),
            DropdownOption::new("settings_terminal_shell:pwsh", "PowerShell 7")
                .selected(matches!(shell, TerminalShell::PowerShellCore)),
            DropdownOption::new("settings_terminal_shell:cmd", "Símbolo del sistema")
                .selected(matches!(shell, TerminalShell::CommandPrompt)),
        ]);
    } else {
        choices.extend([
            DropdownOption::new("settings_terminal_shell:bash", "Bash")
                .selected(matches!(shell, TerminalShell::Bash)),
            DropdownOption::new("settings_terminal_shell:zsh", "Zsh")
                .selected(matches!(shell, TerminalShell::Zsh)),
            DropdownOption::new("settings_terminal_shell:fish", "Fish")
                .selected(matches!(shell, TerminalShell::Fish)),
        ]);
    }
    choices.push(
        DropdownOption::new("settings_terminal_shell_edit", "Personalizado…")
            .selected(matches!(shell, TerminalShell::Custom(_))),
    );
    let selector = Select::new(
        "settings_terminal_shell_toggle",
        shell_name,
        choices,
        state.settings_terminal_shell_menu_open,
        "settings_terminal_shell_dismiss",
        control_palette(),
    )
    .menu_palette(theme::menu_palette())
    .sizing(SelectorSizing::COMPACT)
    .width(238.0)
    .menu_width(254.0)
    .render();
    let mut rows = vec![settings_parts::settings_row(
        "Shell predeterminado",
        "Se usará en nuevas terminales. Las abiertas conservan su shell.",
        selector,
        compact,
    )];
    if state.settings_terminal_shell_editing || matches!(shell, TerminalShell::Custom(_)) {
        let draft = state.settings_terminal_shell_draft.trim();
        let valid_syntax = !draft.is_empty() && Path::new(draft).is_absolute();
        let custom_path = mono_field(
            "settings_terminal_shell_path",
            &state.settings_terminal_shell_draft,
            "Ruta absoluta del ejecutable",
        )
        .render();
        let custom_button = outline(
            "settings_terminal_shell:custom",
            if matches!(shell, TerminalShell::Custom(_)) {
                "Guardar"
            } else {
                "Guardar y usar"
            },
        )
        .icon(Icon::Check)
        .enabled(valid_syntax)
        .render();
        let mut actions = vec![custom_button];
        if state.settings_terminal_shell_editing {
            actions.push(
                Button::new(
                    "settings_terminal_shell_cancel",
                    "Cancelar",
                    control_palette(),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render(),
            );
        }
        let custom_control = if compact {
            Element::column(vec![custom_path, Element::row(actions).gap(8.0)]).gap(10.0)
        } else {
            let mut controls = vec![custom_path.flex(1.0)];
            controls.extend(actions);
            Element::row(controls).gap(10.0)
        };
        let mut editor = vec![form_field("Ejecutable personalizado", custom_control)
            .hint("Usa una ruta absoluta. El ejecutable se comprueba antes de guardar.")
            .sizing(GROUP_FIELD)
            .render()];
        match shell {
            TerminalShell::Custom(path) if path != Path::new(draft) => {
                editor.push(
                    Text::new(
                        format!("Ruta guardada: {}", path.display()),
                        theme::component_palette(),
                    )
                    .role(TextRole::Small)
                    .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                    .render(),
                );
            }
            TerminalShell::Custom(_) => {}
            _ => editor.push(
                Text::new(
                    format!("Se mantiene «{shell_name}» hasta guardar una ruta válida."),
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                .render(),
            ),
        }
        rows.push(
            Element::column(editor)
                .gap(8.0)
                .padding(settings_parts::ROW_PADDING),
        );
    }
    vec![settings_parts::settings_group("Terminal integrada", rows)]
}

fn additional_page(state: &ViewState, page: SettingsPage, compact: bool) -> Vec<Element> {
    match page {
        SettingsPage::Appearance => appearance_page(state, compact),
        SettingsPage::Terminal => terminal_page(state, compact),
        SettingsPage::WorkspaceSearch => vec![
            settings_parts::settings_group("Archivos del proyecto", vec![
                settings_parts::settings_row(
                    "Búsqueda de archivos y contenido",
                    "Encuentra rutas y texto de la carpeta activa desde el centro de comandos.",
                    outline("open_command_center", "Abrir búsqueda").icon(Icon::Search).render(),
                    compact,
                ),
                settings_parts::settings_row(
                    "Acceso rápido",
                    "Busca acciones, tareas y archivos desde cualquier sección.",
                    Kbd::new(state.keymap_profile.shortcut(ShortcutAction::CommandPalette), theme::component_palette()).render(),
                    compact,
                ),
            ]),
            settings_parts::settings_group("Alcance", vec![
                page_note("Exclusiones del proyecto", "La búsqueda utiliza exclusiones integradas. La edición de reglas de exclusión por proyecto todavía no está disponible.", Icon::Folder),
            ]),
        ],
        SettingsPage::Memory => memory::page(state, compact),
        SettingsPage::Commands => {
            let mut content = vec![settings_parts::settings_group("Centro de comandos", vec![
                settings_parts::settings_row("Acciones, tareas y archivos", "Busca y ejecuta acciones de la aplicación con el atajo del perfil activo.", outline("open_command_center", "Abrir acciones").icon(Icon::Search).render(), compact),
                settings_parts::settings_row(
                    "Acceso rápido",
                    "Disponible desde cualquier sección.",
                    Kbd::new(state.keymap_profile.shortcut(ShortcutAction::CommandPalette), theme::component_palette()).render(),
                    compact,
                ),
            ])];
            content.extend(exec_rules_page(state, compact));
            content
        }
        SettingsPage::Browser => {
            let mut content = capability_page(
                "Navegador integrado",
                "El agente nativo controla la pestaña Navegador que tengas abierta en el panel de la tarea.",
                "Sin una pestaña de navegador abierta e iniciada en el proyecto de la tarea, las herramientas de navegador no se ofrecen al agente.",
                Some(("settings_page:agent", "Ver permisos del agente", Icon::ArrowRight)),
            );
            content.push(settings_parts::settings_group("Enlaces", vec![
                settings_parts::settings_row(
                    "Abrir enlaces del chat en",
                    "Automático abre en el navegador integrado las direcciones de este equipo, como localhost, y el resto en el del sistema. Ctrl+clic usa el otro; el clic derecho ofrece ambos.",
                    settings_choice_selector(
                        state,
                        "chat_links",
                        state.chat_link_opening.label(),
                        state.chat_link_opening.options(),
                    ),
                    compact,
                ),
            ]));
            content
        }
        SettingsPage::ComputerUse => capability_page(
            "Control del equipo no disponible",
            "La captura de pantalla y las entradas al escritorio todavía no están disponibles para el agente nativo.",
            "El acceso a archivos y comandos se configura en Agente. Ese permiso no habilita el control del escritorio.",
            Some(("settings_page:agent", "Ver permisos del agente", Icon::ArrowRight)),
        ),
        SettingsPage::Subagents => subagents::page(state, compact),
        SettingsPage::ExternalAgents => external_agents::page(state, compact),
        SettingsPage::Plugins => crate::plugins_ui::page(state, compact),
        SettingsPage::Mcp => mcp_page(state, compact),
        SettingsPage::Hooks => hooks_page(state, compact),
        SettingsPage::Usage => vec![
            settings_parts::settings_group("Conversación activa", vec![
                settings_parts::value_row("Contexto de la conversación", "Parte de la ventana del modelo que ocupa la conversación nativa seleccionada. Con ~ es una estimación.", crate::composer_context_ui::occupancy_text(&state.native_context).unwrap_or_else(|| "Sin medición".to_owned()), compact),
            ]),
        ],
        _ => unreachable!("existing Settings page has its own renderer"),
    }
}

fn compact_navigation(state: &ViewState) -> Element {
    let mut items = vec![Element::row(vec![
        outline("toggle_settings", "Volver")
            .icon(Icon::ArrowLeft)
            .render(),
        Element::spacer().flex(1.0),
        outline(
            "settings_nav_toggle",
            if state.settings_nav_open {
                "Cerrar secciones"
            } else {
                "Secciones"
            },
        )
        .icon(Icon::Menu)
        .render(),
    ])
    .gap(8.0)];
    if state.settings_nav_open {
        items.push(navigation_search(state));
        items.extend(navigation_groups(state));
    }
    Element::column(items).gap(8.0)
}

pub fn build_settings_page(
    state: &ViewState,
    page: SettingsPage,
    center_width: f32,
    sidebar_visible: bool,
) -> Element {
    let column = PageColumn::new(
        format!("settings_page:{page:?}"),
        center_width,
        theme::component_palette(),
    )
    .gutter(12.0, 20.0, 600.0)
    .gap(12.0)
    .reserve_rail(true);
    let content_width = column.content_width();
    let compact = content_width < 640.0;
    let cards = match page {
        SettingsPage::General => general_page(state, compact),
        SettingsPage::Agent => agent_page(state, compact),
        SettingsPage::Providers => providers_page(state, compact, content_width),
        SettingsPage::Skills => skills_page(state, compact),
        SettingsPage::Automations => automations_page(state, compact),
        SettingsPage::Data => data_page(state, compact),
        SettingsPage::Github => github_page(state, compact, content_width),
        SettingsPage::Usage => {
            let mut cards = additional_page(state, page, compact);
            cards.insert(1, activity_charts::tasks_card(state, content_width));
            let mut usage = crate::provider_usage_ui::usage_page(state, content_width);
            usage.extend(cards);
            usage
        }
        SettingsPage::Shortcuts => shortcuts_page(state, compact),
        _ => additional_page(state, page, compact),
    };
    let mut body = Vec::new();
    if !sidebar_visible {
        body.push(compact_navigation(state));
    }
    let status = matches!(
        page,
        SettingsPage::Browser | SettingsPage::ComputerUse | SettingsPage::ExternalAgents
    )
        .then(|| settings_parts::status_badge(page.status()));
    let mut header = PageHeader::new(page.title(), theme::card_palette())
        .description(page.description())
        .stacked(compact);
    if let Some(status) = status {
        header = header.trailing(status);
    }
    body.push(header.render());
    body.extend(cards);
    column.children(body).render()
}

#[cfg(test)]
pub(crate) fn native_output_settings_for_test(state: &ViewState, compact: bool) -> Element {
    providers::output_settings(state, compact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, Frame, Key, Point, Size, UiAction, UiEvent, UiTree};

    fn key(key: Key) -> UiEvent {
        UiEvent::Key {
            key,
            shift: false,
            ctrl: false,
        }
    }

    fn paints_label(frame: &Frame, label: &str) -> bool {
        frame.commands.iter().any(|command| {
            matches!(command,
                DrawCommand::Text { text, .. } if text == label
            )
        })
    }

    #[test]
    fn settings_routes_have_distinct_semantic_icons_in_navigation() {
        let mut icons = std::collections::HashSet::new();
        for (_, pages) in SETTINGS_GROUPS {
            for page in *pages {
                assert!(
                    icons.insert(page.icon()),
                    "duplicate settings icon for {page:?}"
                );
                let frame = UiTree::new(nav_item(*page, false)).layout(Size::new(260.0, 28.0));
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Icon { icon, .. } if *icon == page.icon()
                )));
                let active = UiTree::new(nav_item(*page, true)).layout(Size::new(260.0, 28.0));
                assert!(active.commands.iter().any(|command| matches!(command,
                    DrawCommand::Icon { icon, .. } if *icon == page.icon().filled()
                )));
            }
        }
    }

    #[test]
    fn shortcut_settings_use_the_selected_registry_and_shared_keycaps() {
        for profile in KeymapProfile::ALL.iter().copied() {
            let mut state = crate::view::tests::empty_state();
            state.keymap_profile = profile;
            let frame = UiTree::new(Element::column(shortcuts_page(&state, false)))
                .layout(Size::new(900.0, 5000.0));
            assert!(paints_label(&frame, profile.label()));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("settings_keymap:")));
            let painted_keys = |frame: &Frame| {
                frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text {
                            text, font_size, ..
                        } if *font_size == 11.0 => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            let mut expected = Vec::new();
            for action in ShortcutAction::ALL.iter().copied() {
                let shortcut = profile.shortcut(action);
                if shortcut.is_empty() {
                    assert!(!paints_label(&frame, action.label()));
                    continue;
                }
                assert!(
                    paints_label(&frame, action.label()),
                    "missing active binding {}",
                    action.label()
                );
                let keys = UiTree::new(Kbd::new(shortcut, theme::component_palette()).render())
                    .layout(Size::new(500.0, 20.0));
                expected.extend(painted_keys(&keys));
            }
            let mut actual = painted_keys(&frame);
            // Supporting Settings copy uses the same 11px scale as keycaps.
            actual.retain(|text| expected.contains(text));
            actual.sort();
            expected.sort();
            assert_eq!(
                actual,
                expected,
                "shortcut labels must follow {}",
                profile.id()
            );
        }
    }

    #[test]
    fn keyboard_profile_selector_keeps_host_actions_and_bounded_keyboard_navigation() {
        for width in [360.0, 900.0] {
            let mut state = crate::view::tests::empty_state();
            let viewport = Size::new(width, 900.0);
            let mut tree = UiTree::new(Element::column(shortcuts_page(&state, width < 640.0)));
            let frame = tree.layout(viewport);
            let trigger = frame
                .regions
                .iter()
                .find(|region| region.id == "settings_keymap_toggle")
                .unwrap()
                .rect;
            assert!(trigger.x >= 0.0 && trigger.x + trigger.width <= width);
            let point = Point::new(trigger.x + 12.0, trigger.y + 12.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked {
                    id: "settings_keymap_toggle".into()
                }]
            );
            state.settings_keymap_menu_open = true;
            tree.set_root(Element::column(shortcuts_page(&state, width < 640.0)));
            tree.set_reduced_motion(true);
            let frame = tree.layout(viewport);
            let surface = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:settings_keymap_dismiss")
                .unwrap()
                .rect;
            assert!(surface.x >= 0.0 && surface.x + surface.width <= width);
            for profile in KeymapProfile::ALL.iter().copied() {
                assert!(frame
                    .regions
                    .iter()
                    .any(|region| region.id == format!("settings_keymap:{}", profile.id())));
                assert!(paints_label(&frame, profile.label()));
            }
            let first = format!("settings_keymap:{}", KeymapProfile::ALL[0].id());
            let last = format!(
                "settings_keymap:{}",
                KeymapProfile::ALL.last().unwrap().id()
            );
            assert!(tree.focus(&first));
            tree.handle_event(key(Key::End));
            assert_eq!(tree.focused_id(), Some(last.as_str()));
            assert_eq!(
                tree.handle_event(key(Key::Enter)),
                vec![UiAction::Clicked { id: last }]
            );
            assert_eq!(
                tree.handle_event(key(Key::Escape)),
                vec![UiAction::Clicked {
                    id: "settings_keymap_dismiss".into()
                }]
            );
        }
    }

    #[test]
    fn settings_choice_dropdowns_show_only_the_selected_data_value_when_closed() {
        let mut state = crate::view::tests::empty_state();
        state.session_organization = SessionOrganization::Group;
        state.session_sort = SessionSort::Created;
        for width in [360.0, 900.0] {
            let mut tree = UiTree::new(Element::column(data_page(&state, width < 640.0)));
            let frame = tree.layout(Size::new(width, 1000.0));
            for label in ["Grupo", "Creadas"] {
                assert!(paints_label(&frame, label));
            }
            for label in ["Proyecto", "Recientes", "Actualizadas"] {
                assert!(!paints_label(&frame, label));
            }
            assert!(!frame.regions.iter().any(|region| {
                region.id.starts_with("session_organize:") || region.id.starts_with("session_sort:")
            }));
            for key in ["organization", "sort"] {
                let id = format!("settings_choice_toggle:{key}");
                let trigger = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert_eq!(trigger.rect.width, SelectorSizing::COMPACT.min_width);
                assert!((trigger.rect.height - ControlSize::Compact.height()).abs() < 0.01);
                assert!(trigger.rect.x >= 0.0 && trigger.rect.x + trigger.rect.width <= width);
                let point = Point::new(trigger.rect.x + 8.0, trigger.rect.y + 8.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id }]
                );
            }
        }
    }

    #[test]
    fn settings_choice_dropdowns_keep_actions_keyboard_navigation_and_viewport_bounds() {
        let mut state = crate::view::tests::empty_state();
        for width in [360.0, 900.0] {
            for (choice, ids, labels) in [
                (
                    "organization",
                    vec![
                        "session_organize:project",
                        "session_organize:group",
                        "session_organize:recent",
                    ],
                    vec!["Proyecto", "Grupo", "Recientes"],
                ),
                (
                    "sort",
                    vec!["session_sort:updated", "session_sort:created"],
                    vec!["Actualizadas", "Creadas"],
                ),
            ] {
                state.settings_choice_menu_open = Some(choice.into());
                let mut tree = UiTree::new(Element::column(data_page(&state, width < 640.0)));
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, 720.0));
                let surface = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "popover_surface:settings_choice_dismiss")
                    .unwrap()
                    .rect;
                assert_eq!(surface.width, SelectorSizing::COMPACT.preferred_width);
                assert!(surface.x >= 0.0 && surface.x + surface.width <= width);
                assert!(surface.y >= 0.0 && surface.y + surface.height <= 720.0);
                for (id, label) in ids.iter().zip(&labels) {
                    let row = frame
                        .regions
                        .iter()
                        .find(|region| region.id == *id)
                        .unwrap();
                    // The standard menu's compact row.
                    assert!((row.rect.height - 26.0).abs() < 0.01);
                    assert!(paints_label(&frame, label));
                }
                let other_prefix = if choice == "organization" {
                    "session_sort:"
                } else {
                    "session_organize:"
                };
                assert!(!frame
                    .regions
                    .iter()
                    .any(|region| region.id.starts_with(other_prefix)));
                assert!(tree.focus(ids[0]));
                assert!(tree.handle_event(key(Key::ArrowDown)).is_empty());
                assert_eq!(tree.focused_id(), Some(ids[1]));
                assert_eq!(
                    tree.handle_event(key(Key::Enter)),
                    vec![UiAction::Clicked { id: ids[1].into() }]
                );
                assert_eq!(
                    tree.handle_event(key(Key::Escape)),
                    vec![UiAction::Clicked {
                        id: "settings_choice_dismiss".into(),
                    }]
                );
            }
        }
    }

    #[test]
    fn settings_choice_isolation_keeps_host_availability_and_next_turn_selection() {
        let mut state = crate::view::tests::empty_state();
        state.native_command_isolation = CommandIsolation::HostUnrestricted;
        for width in [360.0, 900.0] {
            let mut tree = UiTree::new(Element::column(agent_page(&state, width < 640.0)));
            let frame = tree.layout(Size::new(width, 10000.0));
            assert!(paints_label(&frame, "Sin aislamiento"));
            assert!(!paints_label(&frame, "Proyecto, sin red"));
            assert!(!tree.focus("command_isolation:host_unrestricted"));
            assert!(tree.focus("settings_choice_toggle:isolation"));
            for running in [false, true] {
                state.running = running;
                state.settings_choice_menu_open = Some("isolation".into());
                tree.set_root(Element::column(agent_page(&state, width < 640.0)));
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, 10000.0));
                assert!(paints_label(&frame, "Proyecto, sin red"));
                assert_eq!(
                    tree.focus("command_isolation:workspace_write_offline"),
                    CommandIsolation::WorkspaceWriteOffline.is_available_on_host()
                );
                assert!(tree.focus("command_isolation:host_unrestricted"));
                assert_eq!(
                    tree.handle_event(key(Key::Enter)),
                    vec![UiAction::Clicked {
                        id: "command_isolation:host_unrestricted".into(),
                    }]
                );
                let surface = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "popover_surface:settings_choice_dismiss")
                    .unwrap()
                    .rect;
                assert!(surface.x >= 0.0 && surface.x + surface.width <= width);
                assert_eq!(
                    state.native_command_isolation,
                    CommandIsolation::HostUnrestricted
                );
            }
            state.running = false;
            state.settings_choice_menu_open = None;
        }
    }

    #[test]
    fn settings_routes_are_distinct_and_parseable() {
        let mut ids = std::collections::HashSet::new();
        for (_, pages) in SETTINGS_GROUPS {
            for page in *pages {
                assert_eq!(SettingsPage::from_action_id(page.action_id()), Some(*page));
                assert!(ids.insert(page.action_id()), "duplicate settings route");
            }
        }
        assert_eq!(ids.len(), 19);
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
