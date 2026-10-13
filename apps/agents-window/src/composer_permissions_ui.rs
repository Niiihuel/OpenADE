//! Composer shortcuts into the native tool settings. The host confirms full
//! access before changing its policy; validation and journal remain mandatory.

use openide_core::TurnMode;
use openide_harness::{CommandIsolation, LocalTool, ToolPermissions};
use openide_ui::{
    Button, ButtonVariant, Component, ComponentPalette, ControlSize, Element, Icon,
    PermissionOption, PermissionSelector, SelectorSizing, Tooltip,
};

use crate::{agent_tool_settings::AgentToolSettings, theme};
use std::sync::atomic::{AtomicU64, Ordering};

static CONFIRM_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub(crate) struct FullAccessConfirmation {
    pub token: u64,
    pub settings: AgentToolSettings,
    workspace: String,
    session: Option<String>,
}

impl FullAccessConfirmation {
    pub fn new(view: &crate::view::ViewState, settings: AgentToolSettings) -> Self {
        Self {
            token: CONFIRM_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            settings,
            workspace: view.workspace.clone(),
            session: view.selected_session.clone(),
        }
    }

    pub fn matches_view(&self, view: &crate::view::ViewState) -> bool {
        view.selected_backend == "native"
            && !view.show_settings
            && view.main_page == crate::view::MainPage::Workspace
            && view.workspace == self.workspace
            && view.selected_session == self.session
    }
}

pub(crate) fn full_access_dialog(
    state: &crate::view::ViewState,
    base: Element,
    viewport_width: f32,
) -> Element {
    let Some(confirmation) = &state.composer_full_access_confirm else {
        return base;
    };
    if !confirmation.matches_view(state) {
        return base;
    }
    let description =
        full_access_description(openide_harness::linux_administrator_commands_available());
    openide_ui::AlertDialog::new(
        base,
        true,
        "¿Permitir todo?",
        description,
        format!("composer_full_access_confirm:{}", confirmation.token),
        format!("composer_full_access_cancel:{}", confirmation.token),
        theme::component_palette(),
    )
    .width((viewport_width - 24.0).min(440.0))
    .confirm_label("Permitir todo")
    .cancel_label("Cancelar")
    .render()
}

fn full_access_description(administrator_available: bool) -> &'static str {
    if cfg!(target_os = "linux") {
        if administrator_available {
            "El agente podrá editar archivos y ejecutar comandos en tu equipo sin pedir aprobación por cada operación. Los comandos como root usan autenticación de Linux. Las reglas de comandos guardadas siguen aplicándose."
        } else {
            "El agente podrá editar archivos y ejecutar comandos con los permisos de tu usuario sin pedir aprobación por cada operación. Root no está disponible: falta habilitar pkexec en el sistema. Las reglas de comandos guardadas siguen aplicándose."
        }
    } else {
        "El agente podrá editar archivos y ejecutar comandos en tu equipo sin pedir aprobación por cada operación. Las reglas de comandos guardadas siguen aplicándose."
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposerPermissionPreset {
    ReadOnly,
    Ask,
    FullAccess,
    Configured,
}

impl ComposerPermissionPreset {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Sólo lectura",
            Self::Ask => "Pedir aprobación",
            Self::FullAccess => "Permitir todo",
            Self::Configured => "Permisos",
        }
    }

    pub(crate) fn from_action_id(action: &str) -> Option<Self> {
        match action {
            "composer_permissions:read_only" => Some(Self::ReadOnly),
            "composer_permissions:ask" => Some(Self::Ask),
            "composer_permissions:full_access" => Some(Self::FullAccess),
            _ => None,
        }
    }
}

pub(crate) fn preset_for_permissions(
    permissions: ToolPermissions,
    full_access: bool,
) -> ComposerPermissionPreset {
    if full_access {
        return ComposerPermissionPreset::FullAccess;
    }
    match (permissions.write_file, permissions.run_command) {
        (false, false) => ComposerPermissionPreset::ReadOnly,
        (true, true) => ComposerPermissionPreset::Ask,
        _ => ComposerPermissionPreset::Configured,
    }
}

/// The host must persist this through its existing commit pipeline, and must
/// enqueue preset actions during a running turn. FullAccess needs confirmation.
pub(crate) fn settings_for_preset(
    mut current: AgentToolSettings,
    preset: ComposerPermissionPreset,
) -> AgentToolSettings {
    current.set_full_access(false);
    let enabled = match preset {
        ComposerPermissionPreset::ReadOnly => false,
        ComposerPermissionPreset::Ask => true,
        ComposerPermissionPreset::FullAccess => {
            current.set_full_access(true);
            return current;
        }
        ComposerPermissionPreset::Configured => return current,
    };
    current.set(LocalTool::WriteFile, enabled);
    current.set(LocalTool::RunCommand, enabled);
    current
}

pub(crate) struct ComposerPermissionsProps<'a> {
    pub backend: &'a str,
    pub permissions: ToolPermissions,
    pub command_isolation: CommandIsolation,
    pub full_access: bool,
    pub turn_mode: TurnMode,
    pub open: bool,
    pub running: bool,
    /// Available composer width, for a bounded menu and compact trigger.
    pub width: f32,
}

/// Shared with the composer so its toolbar reserves the exact width of this
/// trigger instead of using independent breakpoints.
pub(crate) fn control_width(
    mode: TurnMode,
    permissions: ToolPermissions,
    full_access: bool,
    composer_width: f32,
) -> f32 {
    if composer_width < 640.0 {
        return 28.0;
    }
    let label = if mode == TurnMode::Plan {
        "Plan"
    } else {
        preset_for_permissions(permissions, full_access).label()
    };
    (label.chars().count() as f32 * 7.0 + 76.0).clamp(80.0, 180.0)
}

pub(crate) fn selector(props: ComposerPermissionsProps<'_>) -> Element {
    let native = props.backend == "native";
    let selected = preset_for_permissions(props.permissions, props.full_access);
    let planning = native && props.turn_mode == TurnMode::Plan;
    let label = if planning {
        "Plan"
    } else if native {
        selected.label()
    } else {
        "Permisos del proveedor"
    };
    let icon = if !native {
        Icon::InfoCircle
    } else if planning {
        Icon::Plan
    } else {
        match selected {
            ComposerPermissionPreset::ReadOnly => Icon::File,
            ComposerPermissionPreset::Ask => Icon::CheckCircle,
            ComposerPermissionPreset::FullAccess => Icon::WarningTriangle,
            ComposerPermissionPreset::Configured => Icon::Settings,
        }
    };
    let tooltip_description = if !native {
        "Este proveedor administra sus permisos. Los ajustes del motor de openADE no se aplican a su CLI.".to_owned()
    } else if props.running {
        format!(
            "{}El turno en curso conserva sus permisos. Tu selección se aplicará al terminar, para el próximo mensaje.",
            if planning {
                format!("Permisos: {}. ", selected.label())
            } else {
                String::new()
            }
        )
    } else if planning {
        format!("Permisos: {}. Inspeccionar y planificar con las lecturas habilitadas, sin editar archivos ni ejecutar comandos. Desmarcar Plan o usar /agent vuelve al modo de trabajo con estos permisos.", selected.label())
    } else {
        "Modo y permisos del motor de openADE para los próximos turnos. Las herramientas externas piden aprobación por separado.".to_owned()
    };
    let compact = props.width < 640.0;
    let trigger_label = if compact { "" } else { label };
    let mut anchor = Button::new(
        "composer:permissions",
        trigger_label,
        ComponentPalette {
            text: theme::FOREGROUND_SUBTLE,
            ..theme::selector_palette()
        },
    )
    .icon(icon)
    .size(ControlSize::Compact)
    .variant(ButtonVariant::Ghost)
    .hover_ink(theme::FOREGROUND)
    .enabled(native);
    // The labelled control hugs its text; only the icon-only one is a fixed
    // square. A reserved width left a visible hole beside its neighbours.
    if compact {
        anchor = anchor.width(control_width(
            props.turn_mode,
            props.permissions,
            props.full_access,
            props.width,
        ));
    } else {
        anchor = anchor.trailing_icon(Icon::ChevronDown);
    }
    // A disabled Button paints no hit target. The shared Tooltip adds only
    // a hover region, keeping its explanation available without activation.
    let anchor = anchor.render();
    let anchor = if props.open && native {
        // The options already provide contextual help. Do not paint the
        // trigger's hint over the menu it just opened.
        anchor
    } else {
        Tooltip::new(
            anchor,
            "composer:permissions",
            label,
            theme::component_palette(),
        )
        .description(tooltip_description)
        .max_width(SelectorSizing::COMPACT.tooltip_width)
        .render()
    };
    let commands_available = props.command_isolation.is_available_on_host();
    let ask_description = if commands_available {
        "Pedir confirmación antes de editar archivos o ejecutar comandos."
    } else {
        "Pedir confirmación antes de editar archivos. Los comandos están bloqueados por el aislamiento actual."
    };
    let options = vec![
        PermissionOption::new(
            if planning { "composer_mode:agent" } else { "composer_mode:plan" },
            "Plan",
            "Inspeccionar y proponer un plan con las lecturas habilitadas. Sin editar archivos ni ejecutar comandos.",
        )
        .icon(Icon::Plan)
        .enabled(!props.running)
        .selected(planning),
        PermissionOption::new(
            "composer_permissions:read_only",
            ComposerPermissionPreset::ReadOnly.label(),
            "Desactivar las herramientas locales de edición y comandos. Mantener los permisos de lectura configurados.",
        )
        .icon(Icon::File)
        .separator_before()
        .selected(selected == ComposerPermissionPreset::ReadOnly),
        PermissionOption::new(
            "composer_permissions:ask",
            ComposerPermissionPreset::Ask.label(),
            ask_description,
        )
        .icon(Icon::CheckCircle)
        .selected(selected == ComposerPermissionPreset::Ask),
        PermissionOption::new(
            "composer_permissions:full_access",
            ComposerPermissionPreset::FullAccess.label(),
            full_access_description(openide_harness::linux_administrator_commands_available()),
        )
        .icon(Icon::WarningTriangle)
        .selected(selected == ComposerPermissionPreset::FullAccess),
    ];
    PermissionSelector::new(
        anchor,
        options,
        props.open && native,
        "dismiss:composer_permissions",
        theme::selector_palette(),
    )
    .heading("Modo y permisos")
    .description(
        "Se aplican a los próximos turnos. MCP y navegador mantienen su aprobación independiente.",
    )
    .width(SelectorSizing::COMPACT.fit_width(props.width - 24.0))
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Size, UiTree};

    #[test]
    fn compact_permission_menu_has_four_options_and_no_retired_custom_action() {
        for width in [280.0, 620.0, 900.0] {
            let mut settings = AgentToolSettings::default();
            settings.set_full_access(true);
            let frame = UiTree::new(selector(ComposerPermissionsProps {
                backend: "native",
                permissions: settings.permissions(),
                command_isolation: settings.command_isolation(),
                full_access: settings.full_access(),
                turn_mode: TurnMode::Build,
                open: true,
                running: false,
                width,
            }))
            .layout(Size::new(width, 500.0));
            let rows = [
                "composer_mode:plan",
                "composer_permissions:read_only",
                "composer_permissions:ask",
                "composer_permissions:full_access",
            ]
            .map(|id| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect
            });
            for row in rows {
                assert_eq!(row.height, SelectorSizing::COMPACT.row_height);
                assert!(row.x >= 0.0 && row.x + row.width <= width + 0.1);
            }
            let gap = SelectorSizing::COMPACT.padding / 2.0;
            assert!(rows[2].y - (rows[1].y + rows[1].height) >= gap);
            assert!(rows[3].y - (rows[2].y + rows[2].height) >= gap);
            assert!(rows[3].y + rows[3].height - rows[0].y < 170.0);
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "composer_permissions:custom"));
            assert!(!frame
                .text_layouts()
                .iter()
                .any(|block| block.source == "Personalizado"));
        }
    }

    #[test]
    fn permission_icon_keeps_its_full_size_in_wide_and_compact_composers() {
        for width in [280.0, 620.0, 900.0] {
            let mut settings = AgentToolSettings::default();
            settings.set_full_access(true);
            let frame = UiTree::new(selector(ComposerPermissionsProps {
                backend: "native",
                permissions: settings.permissions(),
                command_isolation: settings.command_isolation(),
                full_access: true,
                turn_mode: TurnMode::Build,
                open: false,
                running: false,
                width,
            }))
            .layout(Size::new(width, 100.0));
            let icon = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    openide_ui::DrawCommand::Icon {
                        rect,
                        icon: Icon::WarningTriangle,
                        ..
                    } => Some(rect),
                    _ => None,
                })
                .expect("permission icon");
            assert_eq!(icon.width, 16.0, "composer width {width}");
            assert_eq!(icon.height, 16.0, "composer width {width}");
        }
    }

    #[test]
    fn running_permission_help_wraps_without_losing_the_last_sentence() {
        use openide_ui::{DrawCommand, Point, UiEvent};
        use std::time::{Duration, Instant};

        for width in [280.0, 900.0] {
            let mut settings = AgentToolSettings::default();
            settings.set_full_access(true);
            let mut tree = UiTree::new(Element::column(vec![
                Element::spacer().flex(1.0),
                selector(ComposerPermissionsProps {
                    backend: "native",
                    permissions: settings.permissions(),
                    command_isolation: settings.command_isolation(),
                    full_access: true,
                    turn_mode: TurnMode::Build,
                    open: false,
                    running: true,
                    width,
                }),
            ]));
            let viewport = Size::new(width, 240.0);
            let initial = tree.layout(viewport);
            let anchor = initial
                .regions
                .iter()
                .find(|region| region.id == "composer:permissions")
                .unwrap()
                .rect;
            tree.handle_event(UiEvent::PointerMove(Point::new(
                anchor.x + anchor.width / 2.0,
                anchor.y + anchor.height / 2.0,
            )));
            let frame = tree.layout_at(viewport, Instant::now() + Duration::from_secs(1));
            let rendered = frame
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
                rendered.contains("El turno en curso conserva sus permisos."),
                "{rendered}"
            );
            assert!(rendered.contains("para el próximo mensaje."), "{rendered}");
            assert!(rendered.contains("Permitir todo"), "{rendered}");
        }
    }

    #[test]
    fn full_access_help_distinguishes_linux_prerequisites() {
        if cfg!(target_os = "linux") {
            assert!(full_access_description(true).contains("autenticación de Linux"));
            assert!(full_access_description(false).contains("Root no está disponible"));
        }
        assert!(full_access_description(false).contains("reglas de comandos guardadas"));
    }

    #[test]
    fn read_only_and_ask_revoke_full_access_and_keep_their_existing_semantics() {
        let mut settings = AgentToolSettings::default();
        settings.set_full_access(true);
        for preset in [
            ComposerPermissionPreset::ReadOnly,
            ComposerPermissionPreset::Ask,
        ] {
            let next = settings_for_preset(settings, preset);
            assert!(!next.full_access());
            assert_eq!(
                next.mode_for(LocalTool::RunCommand),
                if preset == ComposerPermissionPreset::Ask {
                    openide_harness::PermissionMode::Ask
                } else {
                    openide_harness::PermissionMode::Deny
                },
            );
        }
        assert_eq!(
            ComposerPermissionPreset::from_action_id("composer_permissions:custom"),
            None
        );
    }
}
