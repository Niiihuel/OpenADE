//! Shared composer for the empty task and an active conversation.
//!
//! The project context is a separate surface above the editor. The editing
//! card owns the input and actions, with stable hit targets across widths.
//! The effort control repaints while visible for its ambient particle effect.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use openide_core::TurnMode;
use openide_ui::{
    Attachment, AttachmentOrientation, Button, ButtonVariant, Color, Component, Composer,
    ControlSize, DropdownMenu, DropdownMenuEntry, DropdownMenuItem, DropdownMenuSection, Element,
    FontFamily, Icon, IconButton, MenuHeader, MenuItem, MenuMotion, PopoverPlacement,
    SelectorSizing, Skeleton, Tooltip, TooltipItem, TooltipPlacement,
};

use crate::{
    composer_selection::{CredentialAvailability, ExecutionSupport, ModelRouteOption, RouteSource},
    environment_ui::{EnvironmentMenu, EnvironmentUiState},
    keymap::{KeymapProfile, ShortcutAction},
    provider_icons::icon_for_provider,
    theme,
    view::{ComposerAttachmentView, ProviderModelStatus, ViewState},
};

pub(crate) use crate::environment_selectors_ui::{
    branch_selector, branch_selector_inline, project_selector, project_selector_inline,
};

const COMPACT_CONTEXT_WIDTH: f32 = 490.0;
const CARD_RADIUS: f32 = 10.0;
const CONTROL_HEIGHT: f32 = 28.0;
const SELECTOR_HEIGHT: f32 = 28.0;
const CONTEXT_HEIGHT: f32 = 28.0;
const CONTEXT_STRIP_HEIGHT: f32 = Composer::HEADER_STRIP_HEIGHT;
// Read only by the tests.
#[cfg(test)]
const CONTEXT_STRIP_INSET: f32 = 8.0;
#[cfg(test)]
const CONTEXT_STRIP_PADDING: f32 = 4.0;
use crate::theme::{MUTED_INK, PILL_SURFACE, SEND_BLUE};

#[derive(Clone, Copy)]
pub(crate) struct ComposerProps<'a> {
    pub keymap_profile: KeymapProfile,
    pub workspace: &'a str,
    /// The IDE already presents project and branch in its own chrome.
    pub embedded_in_ide: bool,
    pub environment: Option<&'a EnvironmentUiState>,
    pub project_action: Option<&'a str>,
    pub project_branch: Option<&'a str>,
    pub branch_picker: Option<&'a crate::git_branches::BranchPickerView>,
    pub branch_menu_open: bool,
    pub project_menu_open: bool,
    pub project_query: &'a str,
    pub project_options: &'a [(String, String)],
    pub prompt: &'a str,
    pub attachments: &'a [ComposerAttachmentView],
    pub attachment_visuals:
        Option<&'a BTreeMap<String, crate::composer_attachments::AttachmentVisual>>,
    pub attachment_loading: bool,
    pub add_menu_open: bool,
    pub listening: bool,
    pub placeholder: &'a str,
    pub selected_backend: &'a str,
    pub native_provider_id: &'a str,
    pub model_label: &'a str,
    pub model_options: &'a [(String, String)],
    pub model_routes: &'a [ModelRouteOption],
    pub model_metadata: Option<&'a BTreeMap<String, crate::model_metadata::ModelMetadataView>>,
    pub provider_status: Option<&'a BTreeMap<String, ProviderModelStatus>>,
    pub selected_model: &'a str,
    pub model_menu_open: bool,
    pub model_list_open: bool,
    pub model_query: &'a str,
    pub collapsed_model_providers: Option<&'a BTreeSet<String>>,
    pub search_collapsed_model_providers: Option<&'a BTreeSet<String>>,
    pub advanced_open: bool,
    pub effort_label: &'a str,
    pub effort_options: &'a [(String, String)],
    pub selected_effort: &'a str,
    pub effort_menu_open: bool,
    pub effort_preview: Option<f32>,
    pub turn_mode: TurnMode,
    pub full_access: bool,
    pub permission_controls: Option<(
        openide_harness::ToolPermissions,
        openide_harness::CommandIsolation,
        bool,
    )>,
    pub suggestions: Option<&'a crate::composer_suggestions::ComposerSuggestionsState>,
    pub running: bool,
    pub pending: Option<&'a crate::composer_pending::PendingComposerSelection>,
    pub error: Option<&'a str>,
    /// Output of the project's check when it failed after the last turn.
    pub check_failure: Option<&'a str>,
    pub native_context: Option<&'a crate::native_context_ui::NativeContextUiState>,
    pub context_compaction: crate::composer_context_ui::ContextCompactionView<'a>,
    pub context_open: bool,
    pub goal: Option<&'a str>,
    pub goal_editor_open: bool,
    pub goal_draft: &'a str,
    pub(crate) native_goal: Option<&'a crate::native_goal_ui::NativeGoalUiState>,
    /// Width of the entire composer, excluding the parent dock's gutters.
    pub width: f32,
    pub home: bool,
    pub expanded: bool,
}

impl<'a> ComposerProps<'a> {
    /// Project, backend and model data are projected once from application
    /// state. Home, session and fallback surfaces differ only in placement.
    pub(crate) fn from_state(
        state: &'a ViewState,
        width: f32,
        home: bool,
        project_action: Option<&'a str>,
        placeholder: &'a str,
        _status: Option<&'a str>,
    ) -> Self {
        let pending = state.composer_pending.as_ref().filter(|pending| {
            pending.scope.workspace == state.workspace
                && pending.scope.session == state.selected_session
        });
        let pending_model = pending.and_then(|pending| pending.model.as_ref());
        let selected_model = pending_model.map_or(state.composer_model.as_str(), |model| {
            model.route.model_id.as_str()
        });
        let selected_effort = pending_model.map_or(state.composer_effort.as_str(), |model| {
            model.effort.as_str()
        });
        let effort_options = pending_model
            .map_or(state.composer_effort_options.as_slice(), |model| {
                model.route.effort_options.as_slice()
            });
        let pending_permissions = pending.and_then(|pending| pending.permissions);
        let model_label = pending_model
            .map(|model| model.route.model_name.as_str())
            .unwrap_or_else(|| {
                state
                    .composer_model_options
                    .iter()
                    .find(|(id, _)| id == &state.composer_model)
                    .map(|(_, label)| label.as_str())
                    .unwrap_or(
                        if matches!(state.selected_backend.as_str(), "native" | "codex") {
                            &state.composer_model
                        } else {
                            ""
                        },
                    )
            });
        let effort_label = effort_options
            .iter()
            .find(|(id, _)| id == selected_effort)
            .map(|(_, label)| label.as_str())
            .unwrap_or("");
        let project_branch = state.project_branch.as_deref();
        Self {
            keymap_profile: state.keymap_profile,
            workspace: &state.workspace,
            embedded_in_ide: state.ide.is_active(),
            environment: Some(&state.environment),
            project_action,
            project_branch,
            branch_picker: Some(&state.composer_branches),
            branch_menu_open: state.composer_branches.open
                && state.environment.menu == EnvironmentMenu::None,
            project_menu_open: state.composer_project_menu_open
                && state.environment.menu == EnvironmentMenu::None,
            project_query: &state.environment.project_query,
            project_options: &state.composer_project_options,
            prompt: &state.prompt,
            attachments: &state.composer_attachments,
            attachment_visuals: Some(&state.composer_attachment_visuals),
            attachment_loading: state.composer_attachment_loading,
            add_menu_open: state.composer_add_menu_open,
            listening: state.composer_listening,
            placeholder,
            selected_backend: &state.selected_backend,
            native_provider_id: pending_model.map_or(state.native_provider_id.as_str(), |model| {
                model.route.provider_id.as_str()
            }),
            model_label,
            model_options: &state.composer_model_options,
            model_routes: &state.composer_routes,
            model_metadata: Some(&state.composer_model_metadata),
            provider_status: Some(&state.composer_provider_status),
            selected_model,
            model_menu_open: state.composer_model_menu_open,
            model_list_open: state.composer_model_list_open,
            model_query: &state.composer_model_query,
            collapsed_model_providers: Some(&state.composer_collapsed_model_providers),
            search_collapsed_model_providers: Some(
                &state.composer_search_collapsed_model_providers,
            ),
            advanced_open: state.composer_advanced_open,
            effort_label,
            effort_options,
            selected_effort,
            effort_menu_open: state.composer_effort_menu_open,
            effort_preview: state.composer_effort_preview,
            turn_mode: state.turn_mode,
            full_access: pending_permissions
                .map_or(state.native_full_access, |settings| settings.full_access()),
            permission_controls: Some((
                pending_permissions.map_or(state.native_tool_permissions, |settings| {
                    settings.permissions()
                }),
                pending_permissions.map_or(state.native_command_isolation, |settings| {
                    settings.command_isolation()
                }),
                state.composer_permissions_open,
            )),
            suggestions: Some(&state.composer_suggestions),
            running: state.running || state.background_run_selected,
            pending,
            error: state.composer_error.as_deref(),
            check_failure: state.turn_check_failure.as_deref(),
            native_context: state
                .native_context
                .matches_scope(
                    state.selected_session.as_deref(),
                    std::path::Path::new(&state.workspace),
                    &state.native_provider_id,
                    &state.model,
                )
                .then_some(&state.native_context),
            context_compaction: crate::composer_context_ui::compaction_view(state),
            context_open: state.selected_backend == "native" && state.composer_context_open,
            goal: state
                .native_goal
                .goal
                .as_ref()
                .map(|goal| goal.objective.as_str())
                .or(state.composer_goal.as_deref()),
            goal_editor_open: state.composer_goal_editor_open,
            goal_draft: &state.composer_goal_draft,
            native_goal: Some(&state.native_goal),
            width,
            home,
            expanded: state.composer_expanded,
        }
    }
}

fn project_name(workspace: &str) -> &str {
    Path::new(workspace)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("Elegir proyecto")
}

fn metadata_label_width(value: &str, max_label_width: f32) -> f32 {
    (value.chars().count() as f32 * 7.2).clamp(24.0, max_label_width)
}

/// Metadata actions use the same interaction surface as the permissions
/// trigger, including press feedback and the short hover transition.
fn metadata_trigger(trigger: Element) -> Element {
    theme::interactive_surface(trigger)
}

/// A metadata action in the strip under the composer: muted ink on the strip
/// itself, live or not, with the hover and press of every other control.
fn metadata_button(id: &str, label: &str, icon: Icon, width: f32) -> Button {
    let mut palette = theme::action_palette(Color::TRANSPARENT, MUTED_INK);
    palette.muted = MUTED_INK;
    Button::new(id, label, palette)
        .icon(icon)
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Compact)
        .gap(5.0)
        .width(width)
}

fn project_control_width(props: &ComposerProps<'_>, available: f32) -> f32 {
    let compact = props.width < COMPACT_CONTEXT_WIDTH;
    let desired = (project_name(props.workspace).chars().count() as f32 * 8.5 + 68.0)
        .clamp(112.0, if compact { 180.0 } else { 260.0 });
    desired.min(available.max(0.0))
}

fn project_control(props: &ComposerProps<'_>, width: f32) -> Element {
    let name = project_name(props.workspace);
    let id = props.project_action.unwrap_or("composer_project_toggle");
    let visual = Element::row(vec![
        Element::icon(Icon::Folder)
            .foreground(MUTED_INK)
            .size(Some(16.0), Some(16.0)),
        Element::truncated_text(name)
            .foreground(MUTED_INK)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .flex(1.0)
            .size(None, Some(16.0)),
        Element::icon(Icon::ChevronDown)
            .foreground(MUTED_INK)
            .size(Some(16.0), Some(16.0)),
    ])
    .gap(5.0)
    .size(Some((width - 12.0).max(0.0)), Some(16.0));
    let switching = props.branch_picker.is_some_and(|picker| picker.switching());
    let trigger = metadata_trigger(if props.running || switching {
        Element::panel(visual)
    } else {
        Element::clickable(id, visual)
    })
    .padding(6.0)
    .size(Some(width), Some(CONTEXT_HEIGHT));
    project_selector(
        props,
        Tooltip::new(trigger, id, props.workspace, theme::component_palette()).render(),
        PopoverPlacement::TopStart,
        id,
    )
}

fn branch_control(props: &ComposerProps<'_>, width: f32) -> Element {
    let picker = props.branch_picker;
    let label = props.project_branch.unwrap_or("Ramas");
    // Under 72 px only the icon fits.
    let trigger = if width >= 72.0 {
        metadata_button("composer_branch_toggle", label, Icon::GitBranch, width)
            .trailing_icon(Icon::ChevronDown)
    } else {
        metadata_button("composer_branch_toggle", "", Icon::GitBranch, width)
    }
    .enabled(!picker.is_some_and(|picker| picker.switching()))
    .render();
    let trigger = Tooltip::new(
        trigger,
        "composer_branch_toggle",
        format!("Rama: {label}"),
        theme::component_palette(),
    )
    .render();
    branch_selector(
        props,
        trigger,
        PopoverPlacement::TopStart,
        "composer_branch_toggle",
    )
}

/// Reuse the native Context panel on a recorded session without rendering an
/// interactive composer. Automation usage belongs to its recorded route, not
/// the provider/model currently selected in Settings.
pub(crate) fn build_native_context_control(state: &ViewState, width: f32) -> Element {
    let selected = state.selected_session.as_deref().and_then(|id| {
        state
            .sessions
            .iter()
            .find(|session| session.id == id && session.backend == "native")
    });
    let Some(session) = selected.filter(|_| state.selected_backend == "native") else {
        return Element::spacer();
    };
    let mut props = ComposerProps::from_state(state, width, false, None, "", None);
    if let Some(origin) = &session.native_automation {
        let usage = &state.native_context;
        props.native_context = (session.project == state.workspace
            && usage.matches_scope(
                Some(&session.id),
                Path::new(&session.project),
                &origin.provider_id,
                &origin.model_id,
            ))
        .then_some(usage);
    } else {
        props.native_context = props
            .native_context
            .filter(|usage| usage.owner.as_deref() == Some(session.id.as_str()));
    }
    crate::composer_context_ui::control(&props)
}

fn project_metadata(props: &ComposerProps<'_>) -> Element {
    const GAP: f32 = 4.0;
    const ENVIRONMENT_WIDTH: f32 = 88.0;
    let context_width = crate::composer_context_ui::control_width(props);
    let left_width = (props.width - context_width - 6.0).max(0.0);
    let environment_width = props
        .environment
        .map_or(0.0, |_| ENVIRONMENT_WIDTH.min(left_width));
    let has_branch =
        !props.embedded_in_ide && (props.project_branch.is_some() || props.branch_picker.is_some());
    // Each selector owns a fixed hit target. The project gets the first text
    // budget; the branch gracefully becomes an icon before controls overlap.
    let reserved = environment_width
        + if props.environment.is_some() {
            GAP
        } else {
            0.0
        }
        + if has_branch {
            CONTEXT_HEIGHT + GAP
        } else {
            0.0
        };
    let project_width = if props.embedded_in_ide {
        0.0
    } else {
        project_control_width(props, left_width - reserved)
    };
    let mut row = Vec::new();
    let mut occupied = 0.0;
    if !props.embedded_in_ide {
        row.push(project_control(props, project_width));
        occupied += project_width;
    }
    if let Some(environment) = props.environment {
        let locked = props.running || props.branch_picker.is_some_and(|picker| picker.switching());
        row.push(crate::environment_selectors_ui::local_selector(
            environment,
            props.workspace,
            locked,
            environment_width,
        ));
        occupied += environment_width + if occupied > 0.0 { GAP } else { 0.0 };
    }
    if has_branch {
        let available = (left_width - occupied - GAP).max(0.0);
        let desired = (props.project_branch.unwrap_or("Ramas").chars().count() as f32 * 8.5 + 44.0)
            .clamp(96.0, 300.0);
        if available >= CONTEXT_HEIGHT {
            let width = if available >= 72.0 {
                desired.min(available)
            } else {
                CONTEXT_HEIGHT
            };
            row.push(branch_control(props, width));
            occupied += GAP + width;
        }
    }
    if let Some(goal) = props.goal.filter(|goal| {
        !props.embedded_in_ide
            && props.width >= 650.0
            && occupied + GAP + 37.0 + metadata_label_width(goal, 120.0) <= left_width
    }) {
        row.push(
            Tooltip::new(
                // 33: the padding on both sides, the icon and its gap.
                metadata_button(
                    "composer_goal_open",
                    goal,
                    Icon::Target,
                    metadata_label_width(goal, 120.0) + 33.0,
                )
                .render(),
                "composer_goal_open",
                format!("Objetivo: {goal}"),
                theme::component_palette(),
            )
            .render(),
        );
    }
    Element::row(vec![
        Element::row(row)
            .gap(GAP)
            .size(Some(left_width), Some(CONTEXT_HEIGHT)),
        crate::composer_context_ui::control(props),
    ])
    .gap(6.0)
    .size(Some(props.width), Some(CONTEXT_HEIGHT))
}

fn add_context_control(props: &ComposerProps<'_>) -> Element {
    let trigger = Tooltip::new(
        IconButton::new(
            "composer_add_context",
            if props.add_menu_open {
                Icon::Close
            } else {
                Icon::Add
            },
            theme::action_palette(
                if props.home {
                    PILL_SURFACE
                } else {
                    Color::rgb(64, 64, 64)
                },
                Color::rgb(246, 246, 246),
            ),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Compact)
        .enabled(!props.running)
        .render()
        .size(Some(CONTROL_HEIGHT), Some(CONTROL_HEIGHT)),
        "composer_add_context",
        "Añadir al mensaje",
        theme::component_palette(),
    )
    .render();
    let entries = if props.goal_editor_open {
        let content_width = (props.width - 32.0).clamp(200.0, 336.0);
        let mut entries = vec![DropdownMenuEntry::Heading("Objetivo".to_owned())];
        entries.push(DropdownMenuEntry::custom(
            Element::text_input(
                "composer_goal_draft",
                props.goal_draft,
                "Describe el objetivo",
            )
            .background(Color::rgb(53, 53, 53))
            .foreground(theme::FOREGROUND)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .radius(4.0)
            .size(Some(content_width), Some(ControlSize::Compact.height())),
        ));
        entries.push(DropdownMenuEntry::custom(
            Element::row(vec![
                Button::new(
                    "composer_goal_save",
                    "Guardar",
                    theme::action_palette(SEND_BLUE, Color::rgb(255, 255, 255)),
                )
                .size(ControlSize::Compact)
                .render(),
                Button::new(
                    "composer_goal_cancel",
                    "Cancelar",
                    theme::action_palette(PILL_SURFACE, theme::FOREGROUND),
                )
                .size(ControlSize::Compact)
                .render(),
                Element::spacer().flex(1.0),
                Button::new(
                    "composer_goal_clear",
                    "Borrar",
                    theme::action_palette(PILL_SURFACE, MUTED_INK),
                )
                .size(ControlSize::Compact)
                .render(),
            ])
            .gap(5.0)
            .size(Some(content_width), Some(ControlSize::Compact.height())),
        ));
        if let Some(native) = props.native_goal.filter(|state| state.session_id.is_some()) {
            entries.push(DropdownMenuEntry::Heading(
                "Objetivo de esta sesión".into(),
            ));
            if let Some(goal) = &native.goal {
                let status = match goal.status {
                    openide_core::NativeGoalStatus::Active => "Activo",
                    openide_core::NativeGoalStatus::Paused => "Pausado",
                    openide_core::NativeGoalStatus::Complete => "Completado",
                };
                entries.push(DropdownMenuEntry::custom(
                    Element::text(format!(
                        "{status} · revisión {} · {} criterios",
                        goal.revision,
                        goal.criteria.len()
                    ))
                    .font_size(12.0)
                    .foreground(theme::FOREGROUND),
                ));
                for criterion in &goal.criteria {
                    let evidence = criterion
                        .evidence
                        .as_ref()
                        .map_or("Pendiente".to_owned(), |e| {
                            format!("Evidencia del turno {}: {}", e.turn_id, e.summary)
                        });
                    entries.push(DropdownMenuEntry::custom(
                        Element::text(format!("{}\n{evidence}", criterion.description))
                            .font_size(12.0)
                            .foreground(theme::FOREGROUND),
                    ));
                }
                if let Some(id) = native.control_id() {
                    entries.push(DropdownMenuEntry::custom(
                        Button::new(
                            id,
                            if goal.status == openide_core::NativeGoalStatus::Active {
                                "Pausar objetivo"
                            } else {
                                "Reanudar objetivo"
                            },
                            theme::action_palette(PILL_SURFACE, theme::FOREGROUND),
                        )
                        .enabled(!props.running)
                        .size(ControlSize::Compact)
                        .render(),
                    ));
                }
            }
            if native
                .goal
                .as_ref()
                .is_none_or(|g| g.status == openide_core::NativeGoalStatus::Complete)
            {
                entries.push(DropdownMenuEntry::custom(
                    Element::text_area(
                        "native_goal_criteria",
                        &native.criteria_draft,
                        "Criterios de aceptación, uno por línea",
                    )
                    .font_size(13.0)
                    .background(PILL_SURFACE)
                    .foreground(theme::FOREGROUND)
                    .size(Some(content_width), Some(96.0)),
                ));
                entries.push(DropdownMenuEntry::custom(
                    Button::new(
                        "native_goal_activate",
                        "Activar objetivo",
                        theme::action_palette(SEND_BLUE, Color::rgb(255, 255, 255)),
                    )
                    .enabled(!props.running)
                    .size(ControlSize::Compact)
                    .render(),
                ));
            }
            if let Some(error) = &native.error {
                entries.push(DropdownMenuEntry::custom(
                    Element::text(error)
                        .font_size(12.0)
                        .foreground(theme::FOREGROUND),
                ));
            }
        }
        if let Some(native) = props.native_goal {
            entries.push(DropdownMenuEntry::Heading("Recuperación de objetivos".into()));
            entries.push(DropdownMenuEntry::custom(
                openide_ui::ScrollArea::new(
                    "native_goal_recovery_details",
                    Element::text(native.recovery_description())
                        .font_size(12.0)
                        .foreground(theme::FOREGROUND)
                        .padding(6.0),
                )
                .render()
                .size(Some(content_width), Some(120.0)),
            ));
            entries.push(DropdownMenuEntry::custom(
                Button::new(
                    "native_goal_maintenance_retry",
                    "Revisar y limpiar temporales",
                    theme::action_palette(PILL_SURFACE, theme::FOREGROUND),
                )
                .enabled(!props.running)
                .size(ControlSize::Compact)
                .render(),
            ));
        }
        entries
    } else {
        vec![
            DropdownMenuEntry::Heading("Añadir al mensaje".to_owned()),
            DropdownMenuItem::new("composer_add_files", "Archivos y carpetas")
                .icon(Icon::Paperclip)
                .submenu(vec![
                    DropdownMenuItem::new("composer_add_file", "Archivo del proyecto")
                        .icon(Icon::Paperclip)
                        .into(),
                    DropdownMenuItem::new("composer_add_folder", "Carpeta del proyecto")
                        .icon(Icon::Folder)
                        .into(),
                    DropdownMenuItem::new("composer_add_paste", "Pegar imágenes o archivos")
                        .icon(Icon::Paperclip)
                        .shortcut("Ctrl+V")
                        .into(),
                ])
                .into(),
            DropdownMenuItem::new("composer_goal_open", "Objetivo")
                .icon(Icon::Target)
                .annotation(if props.goal.is_some() {
                    "Editar"
                } else {
                    "Definir"
                })
                .into(),
            {
                let item = DropdownMenuItem::new("turn_mode_toggle", "Modo Plan")
                    .icon(Icon::Plan)
                    .annotation("Sin editar")
                    .selected(props.turn_mode == TurnMode::Plan);
                (if props.selected_backend == "native" {
                    item
                } else {
                    item.disabled()
                })
                .into()
            },
            DropdownMenuEntry::Heading("Skills y plantillas".to_owned()),
            DropdownMenuItem::new("composer_plugin_documents", "Documentos")
                .icon(Icon::File)
                .into(),
            DropdownMenuItem::new("composer_plugin_spreadsheets", "Hojas de cálculo")
                .icon(Icon::ChartBar)
                .into(),
            DropdownMenuItem::new("composer_plugin_presentations", "Presentaciones")
                .icon(Icon::Image)
                .into(),
            DropdownMenuItem::new("composer_plugin_code_blocks", "Código")
                .icon(Icon::Code)
                .into(),
            DropdownMenuEntry::Separator,
            DropdownMenuItem::new("plugins_open", "Administrar plugins")
                .icon(Icon::Puzzle)
                .into(),
        ]
    };
    DropdownMenu::new(
        trigger,
        entries,
        props.add_menu_open && !props.running,
        "composer_add_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .width(360.0)
    .placement(PopoverPlacement::TopStart)
    .motion(MenuMotion {
        open_ms: 160,
        item_ms: 100,
    })
    .max_visible_items(10)
    .render()
}

fn effort_tone(step: usize, count: usize) -> Color {
    let progress = step as f32 / count.saturating_sub(1).max(1) as f32;
    let channel = |start: u8, end: u8| {
        (f32::from(start) + (f32::from(end) - f32::from(start)) * progress).round() as u8
    };
    Color::rgb(
        channel(theme::EFFORT_BLUE.r, theme::EFFORT_ULTRA.r),
        channel(theme::EFFORT_BLUE.g, theme::EFFORT_ULTRA.g),
        channel(theme::EFFORT_BLUE.b, theme::EFFORT_ULTRA.b),
    )
}

fn preview_effort<'a>(props: &'a ComposerProps<'_>) -> Option<(usize, usize, &'a str)> {
    let levels: Vec<_> = props
        .effort_options
        .iter()
        .filter(|(id, _)| id != "default")
        .collect();
    if levels.is_empty() {
        return None;
    }
    let selected = levels
        .iter()
        .position(|(id, _)| id == props.selected_effort)
        .unwrap_or(0);
    let index = props
        .effort_preview
        .map(|fraction| {
            (fraction.clamp(0.0, 1.0) * levels.len().saturating_sub(1) as f32).round() as usize
        })
        .unwrap_or(selected)
        .min(levels.len() - 1);
    Some((index, levels.len(), levels[index].1.as_str()))
}

/// A row of the model menu's first view: what it sets, its current value and
/// a chevron into the view that changes it.
fn selector_row(id: &str, title: &str, value: &str, icon: Icon, width: f32) -> DropdownMenuEntry {
    DropdownMenuEntry::custom(
        MenuItem::new(id, title, theme::selector_palette())
            .icon(icon)
            .description(value)
            .chevron()
            .width(width)
            .render()
            // limn's two-line row answers at once; these two keep their fade.
            .transition_ms(openide_ui::tokens().motion.hover_ms),
    )
}

fn effort_panel(props: &ComposerProps<'_>, width: f32) -> Element {
    let sizing = SelectorSizing::COMPACT;
    // `default` is a preference choice, not a tier advertised by the model.
    let levels: Vec<_> = props
        .effort_options
        .iter()
        .filter(|(id, _)| id != "default")
        .collect();
    let selected = levels
        .iter()
        .position(|(id, _)| id == props.selected_effort)
        .unwrap_or(0);
    let fraction = props
        .effort_preview
        .unwrap_or_else(|| selected as f32 / levels.len().saturating_sub(1).max(1) as f32);
    let preview_index = preview_effort(props)
        .map(|(index, _, _)| index)
        .unwrap_or(0);
    let has_default = props.effort_options.iter().any(|(id, _)| id == "default");
    let heading_items = {
        let back_width = 104.0;
        let auto_width = if has_default { 46.0 } else { 0.0 };
        // Center the current tier in the space between the two actions.
        // A flexible label keeps both margins equal as its text changes.
        let mut items = vec![
            theme::selector_palette()
                .interactive_surface(Element::clickable(
                    "composer_advanced_back",
                    Element::row(vec![
                        Element::icon(Icon::ArrowLeft)
                            .foreground(theme::MENU_MUTED)
                            .size(Some(sizing.icon_size), Some(sizing.icon_size)),
                        Element::truncated_text("Esfuerzo")
                            .font_family(FontFamily::Sans)
                            .font_size(sizing.label_size)
                            .foreground(theme::FOREGROUND)
                            .size(
                                Some(
                                    back_width
                                        - sizing.icon_size
                                        - sizing.gap
                                        - 2.0 * sizing.padding,
                                ),
                                Some(sizing.icon_size),
                            ),
                    ])
                    .gap(sizing.gap)
                    .size(
                        Some(back_width - 2.0 * sizing.padding),
                        Some(sizing.icon_size),
                    ),
                ))
                .padding(sizing.padding)
                .size(Some(back_width), Some(sizing.header_height)),
            Element::truncated_text(
                levels
                    .get(preview_index)
                    .map(|(_, label)| label.as_str())
                    .unwrap_or(""),
            )
            .font_family(FontFamily::Sans)
            .font_size(sizing.label_size)
            .foreground(effort_tone(preview_index, levels.len()))
            .center_content()
            .flex(1.0)
            .size(None, Some(sizing.header_height)),
        ];
        if has_default {
            items.push(
                theme::selector_palette()
                    .interactive_surface(Element::clickable(
                        "composer_effort:default",
                        Element::truncated_text("Auto")
                            .font_family(FontFamily::Sans)
                            .font_size(sizing.label_size)
                            .foreground(if props.selected_effort == "default" {
                                theme::EFFORT_BLUE
                            } else {
                                theme::MENU_MUTED
                            })
                            .center_content()
                            .size(
                                Some(auto_width - sizing.padding * 2.0),
                                Some(sizing.header_height - sizing.padding * 2.0),
                            ),
                    ))
                    .background(if props.selected_effort == "default" {
                        theme::INPUT
                    } else {
                        Color::TRANSPARENT
                    })
                    .padding(sizing.padding)
                    .size(Some(auto_width), Some(sizing.header_height)),
            );
        }
        items
    };
    let heading = Element::row(heading_items)
        .gap(sizing.gap)
        .size(Some(width), Some(sizing.header_height));
    let mut children = vec![heading];
    if levels.len() >= 2 {
        children.push(
            Element::effort_slider("composer_effort_slider", fraction, levels.len())
                .effort_ultra_available(levels.last().is_some_and(|(id, _)| id.as_str() == "ultra"))
                .size(Some(width), Some(38.0)),
        );
    } else {
        children.push(
            Element::text(if levels.len() == 1 {
                "Este modelo ofrece un solo nivel de esfuerzo"
            } else {
                "Este modelo no publica niveles de esfuerzo"
            })
            .font_family(FontFamily::Sans)
            .font_size(sizing.detail_size)
            .foreground(theme::MENU_MUTED)
            .size(Some(width), Some(sizing.row_height)),
        );
    }
    let height = sizing.header_height
        + sizing.gap
        + if levels.len() >= 2 {
            38.0
        } else {
            sizing.row_height
        };
    Element::column(children)
        .gap(sizing.gap)
        .size(Some(width), Some(height))
}

fn model_search_key(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(|character| match character {
            '\u{0300}'..='\u{036f}' => None,
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => Some('a'),
            'é' | 'è' | 'ê' | 'ë' => Some('e'),
            'í' | 'ì' | 'î' | 'ï' => Some('i'),
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => Some('o'),
            'ú' | 'ù' | 'û' | 'ü' => Some('u'),
            'ñ' => Some('n'),
            'ç' => Some('c'),
            other => Some(other),
        })
        .collect()
}

fn model_search_matches(terms: &[String], fields: &[&str]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let haystack = model_search_key(&fields.join(" "));
    terms.iter().all(|term| haystack.contains(term))
}

fn model_provider_heading(route: &ModelRouteOption) -> String {
    if route.source == RouteSource::CliLiveCatalog {
        format!("{} · En tu cuenta", route.provider_name)
    } else {
        route.provider_name.clone()
    }
}

/// One model in the catalog: its name over its identifier, with what is
/// known of it at the end of the second line: capability glyphs and the
/// context size, or why the model cannot be chosen.
struct ModelPickerRow<'a> {
    action_id: &'a str,
    title: &'a str,
    detail: &'a str,
    status: &'a str,
    glyphs: Vec<Icon>,
    icon: Icon,
    width: f32,
    selected: bool,
    enabled: bool,
}

impl ModelPickerRow<'_> {
    fn render(self) -> Element {
        let palette = theme::selector_palette();
        let sizing = SelectorSizing::CATALOG;
        let body_height = sizing.row_height;
        let row_padding = (sizing.detail_row_height - body_height) / 2.0;
        let text_width =
            (self.width - row_padding * 2.0 - sizing.icon_size * 2.0 - sizing.gap * 2.0).max(0.0);
        const GLYPH: f32 = 12.0;
        let status_width = if self.status.is_empty() {
            0.0
        } else {
            (self.status.chars().count() as f32 * 5.8 + 10.0).min(text_width * 0.46)
        };
        let mut detail = vec![
            Element::truncated_text(self.detail)
                .font_family(FontFamily::Monospace)
                .font_size(sizing.detail_size)
                .foreground(palette.muted)
                .flex(1.0)
                .size(None, Some(body_height / 2.0)),
        ];
        for glyph in self.glyphs {
            detail.push(
                Element::icon(glyph)
                    .foreground(palette.muted)
                    .size(Some(GLYPH), Some(body_height / 2.0)),
            );
        }
        if status_width > 0.0 {
            detail.push(
                Element::truncated_text(self.status)
                    .font_family(FontFamily::Sans)
                    .font_size(sizing.detail_size)
                    .foreground(palette.muted)
                    .size(Some(status_width), Some(body_height / 2.0)),
            );
        }
        let visual = Element::row(vec![
            Element::icon(self.icon)
                .foreground(if self.enabled {
                    palette.text
                } else {
                    palette.muted
                })
                .size(Some(sizing.icon_size), Some(body_height)),
            Element::column(vec![
                Element::truncated_text(self.title)
                    .font_family(FontFamily::Sans)
                    .font_size(sizing.label_size)
                    .foreground(if self.enabled {
                        palette.text
                    } else {
                        palette.muted
                    })
                    .size(Some(text_width), Some(body_height / 2.0)),
                Element::row(detail)
                    .gap(sizing.gap)
                    .size(Some(text_width), Some(body_height / 2.0)),
            ])
            .size(Some(text_width), Some(body_height)),
            if self.selected {
                Element::icon(Icon::Check).foreground(palette.text)
            } else {
                Element::spacer()
            }
            .size(Some(sizing.icon_size), Some(body_height)),
        ])
        .gap(sizing.gap)
        .clip_children();
        let row = if self.enabled {
            Element::clickable(self.action_id, visual)
                .action_menu_item()
                .transition_ms(0)
        } else {
            Element::panel(visual)
        };
        palette
            .interactive_surface(row)
            .transition_ms(0)
            .background(Color::TRANSPARENT)
            .padding(row_padding)
            .size(Some(self.width), Some(sizing.detail_row_height))
    }
}

fn model_provider_section(
    provider_id: &str,
    label: &str,
    count: usize,
    collapsed: bool,
    width: f32,
) -> DropdownMenuEntry {
    DropdownMenuSection::new(
        format!("composer_model_provider_toggle:{provider_id}"),
        label,
    )
    .icon(icon_for_provider(provider_id))
    .count(count)
    .collapsed(collapsed)
    .width(width)
    .into()
}

fn model_route_details(route: &ModelRouteOption) -> (&'static str, &'static str) {
    let contract = match route.execution {
        ExecutionSupport::Verified => {
            "Contrato del adaptador revisado para este ID; generación con la cuenta sin comprobar."
        }
        ExecutionSupport::ProviderAdvertisedUnverified => {
            "Herramientas declaradas por el proveedor; generación con la cuenta sin comprobar."
        }
        ExecutionSupport::ProviderListedUnverified => {
            "ID en el listado del proveedor; capacidad de herramientas desconocida."
        }
        ExecutionSupport::UnverifiedUserChoice => {
            "Ruta configurada explícitamente; capacidad de herramientas desconocida."
        }
        ExecutionSupport::Unsupported => "El modelo no es compatible con el agente.",
        ExecutionSupport::DelegatedCli => "Codex CLI comprueba el modelo y la cuenta al ejecutar.",
    };
    let credential = match route.credential {
        CredentialAvailability::EnvPresentUnverified => {
            if route.provider_id == "codex" {
                "Cuenta autorizada con ChatGPT; el uso depende de tu plan."
            } else {
                "Credencial disponible; acceso a generación sin comprobar."
            }
        }
        CredentialAvailability::MissingCredential => "Falta la credencial del proveedor.",
        CredentialAvailability::NoKeyRequired => "Esta conexión permite una clave opcional.",
        CredentialAvailability::ExternalCliManaged => "Autenticación administrada por Codex CLI.",
    };
    (contract, credential)
}

fn model_list_fixed_header(props: &ComposerProps<'_>, width: f32) -> Element {
    let mut palette = theme::selector_palette();
    palette.raised = theme::INPUT;
    MenuHeader::new(palette)
        .back("composer_model_list_toggle", "Modelo")
        .trailing(
            IconButton::new(
                "composer_routes_refresh",
                Icon::Refresh,
                theme::selector_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .tooltip("Actualizar listado de proveedores")
            .tooltip_placement(TooltipPlacement::Below)
            .render(),
        )
        .search(
            "composer_model_search",
            props.model_query,
            "Buscar modelos o proveedores",
        )
        .width(width)
        .render()
}

fn model_menu_view_key(props: &ComposerProps<'_>) -> String {
    if !props.model_list_open {
        return if props.advanced_open {
            "advanced"
        } else {
            "main"
        }
        .into();
    }
    let collapsed = if props.model_query.trim().is_empty() {
        props.collapsed_model_providers
    } else {
        props.search_collapsed_model_providers
    };
    let mut key = "models".to_owned();
    if let Some(providers) = collapsed {
        // Reuse the submenu transition for collapse/expand. The filter text
        // itself is excluded so typing does not restart this animation.
        for provider in providers {
            key.push_str(&format!("|{}:{provider}", provider.len()));
        }
    }
    key
}

fn model_token_price(price: f64) -> String {
    let value = if price > 0.0 && price < 0.000_001 {
        price.to_string()
    } else {
        format!("{price:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    };
    format!("${value}")
}

/// "2026-09-04" as "4 sep 2026" and "2026-06" as "jun 2026"; anything else as given.
fn model_short_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = [
        "ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic",
    ];
    let mut parts = iso.split('-').map(|part| part.parse::<usize>().ok());
    let (Some(Some(year)), Some(Some(month @ 1..=12))) = (parts.next(), parts.next()) else {
        return iso.to_owned();
    };
    match parts.next() {
        Some(Some(day)) => format!("{day} {} {year}", MONTHS[month - 1]),
        _ => format!("{} {year}", MONTHS[month - 1]),
    }
}

/// One line saying where the card's facts come from. The source of most
/// fields is named alone; any other source names the fields it supplied, so a
/// catalog figure is never read as the provider's own.
fn model_source_line(metadata: &crate::model_metadata::ModelMetadataView) -> Option<String> {
    use crate::model_metadata::MetadataSource;
    let mut groups: Vec<(MetadataSource, Vec<&str>)> = Vec::new();
    for source in [
        MetadataSource::ProviderApi,
        MetadataSource::ProviderAccount,
        MetadataSource::Reviewed,
        MetadataSource::Catalog,
        MetadataSource::CodexCli,
    ] {
        let fields: Vec<_> = metadata
            .field_sources
            .iter()
            .filter(|(_, from)| **from == source)
            .map(|(field, _)| field.label())
            .collect();
        if !fields.is_empty() {
            groups.push((source, fields));
        }
    }
    // Stable: among equals the earlier, more authoritative source leads.
    groups.sort_by_key(|(_, fields)| std::cmp::Reverse(fields.len()));
    let name = |source: MetadataSource| match source {
        MetadataSource::Catalog => format!(
            "{} ({})",
            source.label(),
            model_short_date(crate::model_metadata::catalog_date())
        ),
        _ => source.label().to_owned(),
    };
    let mut parts = groups.iter().enumerate().map(|(index, (source, fields))| {
        if index == 0 {
            name(*source)
        } else {
            format!("{}: {}", name(*source), fields.join(", "))
        }
    });
    let mut line = format!("Fuente: {}", parts.next()?);
    for part in parts {
        line.push_str(" · ");
        line.push_str(&part);
    }
    if metadata.refresh_failed {
        line.push_str(" · copia local; falló la actualización");
    }
    Some(line)
}

/// What the list row says at its trailing edge: the context size and the
/// capabilities known to be present.
fn model_row_info(
    route: &ModelRouteOption,
    props: &ComposerProps<'_>,
) -> (String, Vec<Icon>) {
    let metadata = props
        .model_metadata
        .and_then(|models| models.get(&route.route_id));
    let glyphs = metadata.map_or_else(Vec::new, |metadata| {
        [
            (metadata.tool_call, Icon::Bolt),
            (metadata.image_input, Icon::Image),
            (metadata.reasoning, Icon::Brain),
        ]
        .into_iter()
        .filter_map(|(known, icon)| (known == Some(true)).then_some(icon))
        .collect()
    });
    let window = metadata.and_then(|metadata| metadata.context_tokens.or(metadata.max_input_tokens));
    let text = if route.execution == ExecutionSupport::Unsupported {
        "No compatible".into()
    } else if let Some(window) = window {
        crate::composer_context_ui::tokens(window)
    } else if route.source == RouteSource::CliCatalog {
        "Respaldo CLI".into()
    } else {
        String::new()
    };
    (text, glyphs)
}

/// The model's info card. Only known facts appear; an unknown one is left out.
fn model_route_tooltip(
    anchor: Element,
    tooltip_id: String,
    route: &ModelRouteOption,
    props: &ComposerProps<'_>,
) -> Element {
    let fallback = crate::model_metadata::ModelMetadataView::default();
    let metadata = props
        .model_metadata
        .and_then(|models| models.get(&route.route_id))
        .unwrap_or(&fallback);
    let mut tooltip = Tooltip::new(anchor, tooltip_id, &route.model_name, theme::selector_palette())
        .placement(TooltipPlacement::Left)
        .max_width(SelectorSizing::CATALOG.tooltip_width)
        .footer(format!(
            "{} · {}",
            route.provider_name,
            metadata
                .resolved_model_id
                .as_deref()
                .unwrap_or(&route.model_id)
        ));
    if let Some(description) = &metadata.description {
        let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut preview: String = description.chars().take(120).collect();
        if description.chars().count() > 120 {
            preview.push('…');
        }
        tooltip = tooltip.description(preview);
    }
    let (mut can, mut cannot) = (Vec::new(), Vec::new());
    for (known, label, icon) in [
        (metadata.tool_call, "Herramientas", Icon::Bolt),
        (metadata.image_input, "Visión", Icon::Image),
        (metadata.reasoning, "Razonamiento", Icon::Brain),
        (metadata.structured_output, "Salida estructurada", Icon::Code),
    ] {
        match known {
            Some(true) => can.push(TooltipItem::new(label).icon(icon)),
            Some(false) => cannot.push(TooltipItem::new(label)),
            None => {}
        }
    }
    for (kind, label, icon) in [
        ("pdf", "PDF", Icon::File),
        ("audio", "Audio", Icon::Audio),
        ("video", "Video", Icon::Video),
    ] {
        if metadata.input_modalities.iter().any(|input| input == kind) {
            can.push(TooltipItem::new(label).icon(icon));
        }
    }
    let fact = |value: Option<String>| value.map(TooltipItem::new);
    let limit = |value: Option<u64>| fact(value.map(crate::composer_context_ui::tokens));
    tooltip = tooltip
        .separator()
        .icon_row("Admite", can)
        .icon_row("No admite", cannot)
        .icon_row("Contexto", limit(metadata.context_tokens))
        .icon_row("Salida máx.", limit(metadata.max_output_tokens))
        .icon_row(
            "Entrada máx.",
            limit(
                metadata
                    .max_input_tokens
                    .filter(|input| Some(*input) != metadata.context_tokens),
            ),
        )
        .icon_row(
            "Conocimiento",
            fact(metadata.knowledge.as_deref().map(model_short_date)),
        )
        .icon_row(
            "Lanzamiento",
            fact(metadata.release_date.as_deref().map(model_short_date)),
        )
        .icon_row(
            "Esfuerzos",
            route
                .effort_options
                .iter()
                .filter(|(id, _)| id != "default")
                .map(|(_, label)| TooltipItem::new(label)),
        );
    let price = |value: Option<f64>| value.filter(|price| price.is_finite() && *price >= 0.0);
    if metadata.included_in_plan {
        tooltip = tooltip.separator().description("Incluido en tu plan");
    } else if let Some((input, output)) = metadata
        .cost
        .as_ref()
        .and_then(|cost| price(cost.input).zip(price(cost.output)))
    {
        let mut prices = format!(
            "Precio de referencia · USD / 1M tokens\nEntrada {} · Salida {}",
            model_token_price(input),
            model_token_price(output)
        );
        if let Some(read) = metadata.cost.as_ref().and_then(|cost| price(cost.cache_read)) {
            prices.push_str(&format!(" · Caché lectura {}", model_token_price(read)));
        }
        tooltip = tooltip.separator().description(prices);
    }
    tooltip = tooltip.separator();
    tooltip = tooltip.footer(model_source_line(metadata).unwrap_or_else(|| {
        "Ni el proveedor ni el catálogo describen este modelo.".into()
    }));
    // A sentence only where it explains why the model cannot be used.
    let (contract, credential) = model_route_details(route);
    if route.execution == ExecutionSupport::Unsupported {
        tooltip = tooltip.footer(contract);
    }
    if route.credential == CredentialAvailability::MissingCredential {
        tooltip = tooltip.footer(credential);
    }
    tooltip
        .render()
        .size(None, Some(SelectorSizing::CATALOG.detail_row_height))
}

fn native_model_configured(route: &ModelRouteOption) -> bool {
    route.backend == "native"
        && matches!(
            route.credential,
            CredentialAvailability::EnvPresentUnverified | CredentialAvailability::NoKeyRequired
        )
        && route.provider_listed != Some(false)
}

fn native_model_available(props: &ComposerProps<'_>, route: &ModelRouteOption) -> bool {
    native_model_configured(route)
        && !props
            .provider_status
            .is_some_and(|states| states.contains_key(&route.provider_id))
}

/// Background account discovery can temporarily disable selection and send.
/// Keep the exact configured choice visible while its known contract remains
/// present; a verification state must not erase its model or effort label.
fn selected_native_model_configured(props: &ComposerProps<'_>) -> bool {
    props.model_routes.iter().any(|route| {
        native_model_configured(route)
            && route.selectable()
            && route.provider_id == props.native_provider_id
            && route.model_id == props.selected_model
    })
}

fn selected_native_model_available(props: &ComposerProps<'_>) -> bool {
    props.model_routes.iter().any(|route| {
        native_model_available(props, route)
            && route.selectable()
            && route.provider_id == props.native_provider_id
            && route.model_id == props.selected_model
    })
}

fn composer_model_display<'a>(props: &'a ComposerProps<'_>) -> &'a str {
    if props.selected_backend == "native" && !selected_native_model_configured(props) {
        return if props
            .model_routes
            .iter()
            .any(|route| native_model_available(props, route))
        {
            "Elegir modelo"
        } else {
            "No hay modelos disponibles"
        };
    }
    if props.model_label.is_empty() {
        props.selected_model
    } else {
        props.model_label
    }
}

fn model_effort_entries(props: &ComposerProps<'_>, width: f32) -> Vec<DropdownMenuEntry> {
    // A contextual surface has a 4 px inset. Reserve the catalog rail even
    // when final placement, rather than item count, makes the list scroll.
    let inner_width = (width - 8.0 - if props.model_list_open { 16.0 } else { 0.0 }).max(0.0);
    let sizing = if props.model_list_open {
        SelectorSizing::CATALOG
    } else {
        SelectorSizing::COMPACT
    };
    if props.model_list_open {
        let terms: Vec<String> = model_search_key(props.model_query)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let collapsed_providers = if props.model_query.trim().is_empty() {
            props.collapsed_model_providers
        } else {
            props.search_collapsed_model_providers
        };
        let mut entries = Vec::new();
        let mut visible_models = 0usize;
        if props.model_routes.is_empty() && props.selected_backend != "native" {
            let provider_id = if props.selected_backend == "native" {
                props.native_provider_id
            } else {
                props.selected_backend
            };
            let models: Vec<_> = props
                .model_options
                .iter()
                .filter(|(id, title)| {
                    id != "openide:configure-model"
                        && model_search_matches(&terms, &[id, title, provider_id])
                })
                .collect();
            let collapsed =
                collapsed_providers.is_some_and(|providers| providers.contains(provider_id));
            visible_models += models.len();
            if !models.is_empty() {
                entries.push(model_provider_section(
                    provider_id,
                    if provider_id == "codex" {
                        "Codex CLI"
                    } else {
                        provider_id
                    },
                    models.len(),
                    collapsed,
                    inner_width,
                ));
            }
            for (id, title) in models.into_iter().filter(|_| !collapsed) {
                let action_id = format!("composer_model:{id}");
                entries.push(DropdownMenuEntry::custom(
                    Tooltip::new(
                        ModelPickerRow {
                            action_id: &action_id,
                            title,
                            detail: id,
                            status: "",
                            glyphs: Vec::new(),
                            icon: icon_for_provider(provider_id),
                            width: inner_width,
                            selected: id == props.selected_model,
                            enabled: true,
                        }
                        .render(),
                        &action_id,
                        format!("{provider_id} · {id}"),
                        theme::selector_palette(),
                    )
                    .placement(TooltipPlacement::Left)
                    .max_width(sizing.tooltip_width)
                    .render()
                    .size(Some(inner_width), Some(sizing.detail_row_height)),
                ));
            }
        } else {
            let mut groups: Vec<Vec<&ModelRouteOption>> = Vec::new();
            for route in props.model_routes {
                if props.selected_backend == "native" && !native_model_available(props, route) {
                    continue;
                }
                if route.credential == CredentialAvailability::MissingCredential
                    || route.provider_listed == Some(false)
                    || props
                        .provider_status
                        .is_some_and(|states| states.contains_key(&route.provider_id))
                {
                    continue;
                }
                if !model_search_matches(
                    &terms,
                    &[
                        &route.provider_id,
                        &route.provider_name,
                        &route.model_id,
                        &route.model_name,
                    ],
                ) {
                    continue;
                }
                if let Some(group) = groups
                    .iter_mut()
                    .find(|group| group[0].provider_id == route.provider_id)
                {
                    group.push(route);
                } else {
                    groups.push(vec![route]);
                }
            }
            for routes in groups {
                let provider = routes[0];
                let collapsed = collapsed_providers
                    .is_some_and(|providers| providers.contains(&provider.provider_id));
                if !entries.is_empty() {
                    entries.push(DropdownMenuEntry::Separator);
                }
                entries.push(model_provider_section(
                    &provider.provider_id,
                    &model_provider_heading(provider),
                    routes.len(),
                    collapsed,
                    inner_width,
                ));
                visible_models += routes.len();
                if collapsed {
                    continue;
                }
                for route in routes {
                    let action_id = format!("composer_route:{}", route.route_id);
                    let selectable = route.selectable();
                    let tooltip_id = if selectable {
                        action_id.clone()
                    } else {
                        format!("{action_id}:description")
                    };
                    let (status, glyphs) = model_row_info(route, props);
                    entries.push(DropdownMenuEntry::custom(model_route_tooltip(
                        ModelPickerRow {
                            action_id: &action_id,
                            title: &route.model_name,
                            detail: &route.model_id,
                            status: &status,
                            glyphs,
                            icon: icon_for_provider(&route.provider_id),
                            width: inner_width,
                            selected: route.backend == props.selected_backend
                                && route.model_id == props.selected_model
                                && (route.backend != "native"
                                    || route.provider_id == props.native_provider_id),
                            enabled: selectable,
                        }
                        .render(),
                        tooltip_id,
                        route,
                        props,
                    )));
                }
            }
        }
        if visible_models == 0 {
            entries.push(DropdownMenuEntry::custom(
                Element::text(if terms.is_empty() {
                    "No hay modelos disponibles"
                } else {
                    "Sin resultados. Prueba otro nombre."
                })
                .font_family(FontFamily::Sans)
                .font_size(sizing.label_size)
                .foreground(theme::selector_palette().muted)
                .padding(sizing.padding)
                .size(Some(inner_width), None),
            ));
        }
        return entries;
    }
    let has_effort = (props.selected_backend != "native"
        || selected_native_model_configured(props))
        && props.effort_options.iter().any(|(id, _)| id != "default");
    if props.advanced_open && has_effort {
        return vec![DropdownMenuEntry::custom(effort_panel(props, inner_width))];
    }

    let mut entries = vec![DropdownMenuEntry::Heading("Para el próximo mensaje".into())];
    if props.selected_backend == "native"
        || !props.model_options.is_empty()
        || !props.model_routes.is_empty()
    {
        let display = composer_model_display(props);
        entries.push(selector_row(
            "composer_model_list_toggle",
            "Modelo",
            display,
            icon_for_provider(if props.selected_backend == "native" {
                props.native_provider_id
            } else {
                props.selected_backend
            }),
            inner_width,
        ));
    }
    if has_effort {
        entries.push(DropdownMenuEntry::Separator);
        entries.push(selector_row(
            "composer_advanced_toggle",
            "Esfuerzo",
            if props.selected_effort == "default" {
                "Automático"
            } else {
                props.effort_label
            },
            Icon::Brain,
            inner_width,
        ));
    }
    entries
}

fn model_effort_group_width(props: &ComposerProps<'_>) -> Option<f32> {
    let has_model = props.selected_backend == "native" || !props.model_label.is_empty();
    let visible_effort = props
        .effort_preview
        .and_then(|_| preview_effort(props))
        .map(|(_, _, label)| label)
        .unwrap_or(props.effort_label);
    if !has_model && visible_effort.is_empty() {
        return None;
    }
    // Send and stop occupy the same slot; queue submission uses Ctrl+Enter.
    let controls = 1 + usize::from(props.width >= 380.0);
    let other_actions = controls as f32 * (CONTROL_HEIGHT + 4.0);
    let policy_width = props
        .permission_controls
        .map_or(0.0, |(permissions, _, _)| {
            crate::composer_permissions_ui::control_width(
                props.turn_mode,
                permissions,
                props.full_access,
                props.width,
            ) + 4.0
        });
    // Keep the text labels together while reserving the remaining toolbar.
    let desired = ((composer_model_display(props).chars().count() + visible_effort.chars().count())
        as f32
        * 8.0
        + 40.0)
        .clamp(106.0, 250.0);
    let width = (props.width - 16.0 - CONTROL_HEIGHT - 16.0 - other_actions - policy_width)
        .clamp(0.0, desired);
    (width >= 106.0).then_some(width)
}

fn model_effort_group(props: &ComposerProps<'_>, compact: bool) -> Option<Element> {
    let model = match composer_model_display(props) {
        "No hay modelos disponibles" if compact => "Sin modelos",
        display => display,
    };
    let model_unavailable =
        props.selected_backend == "native" && !selected_native_model_configured(props);
    let preview = props.effort_preview.and_then(|_| preview_effort(props));
    let visible_effort = if model_unavailable {
        ""
    } else {
        preview
            .map(|(_, _, label)| label)
            .unwrap_or(props.effort_label)
    };
    let label = match (model.is_empty(), visible_effort.is_empty()) {
        (true, true) => return None,
        (false, true) => model.to_owned(),
        (true, false) => visible_effort.to_owned(),
        (false, false) => format!("{model} {visible_effort}"),
    };
    let width = model_effort_group_width(props)?;
    if props.selected_backend != "native"
        && props.model_options.is_empty()
        && props.model_routes.is_empty()
        && props.effort_options.is_empty()
    {
        return Some(
            Element::truncated_text(label)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(MUTED_INK)
                .padding(6.0)
                .size(Some(width), Some(CONTROL_HEIGHT)),
        );
    }
    let open = props.model_menu_open || props.effort_menu_open;
    let effort_width = if visible_effort.is_empty() {
        0.0
    } else {
        // Keep a conservative width for proportional Sans text and accents. Reserving only
        // 7 px per scalar clipped “Máximo” even in the full 250 px pill; this
        // budget leaves room for accents and wider glyphs without changing the
        // pill or its narrow-width cap. Final truncation still uses host fonts.
        (visible_effort.chars().count() as f32 * 8.0 + 8.0)
            .min(width * 0.45)
            // Preserve at least the model's ellipsis at narrow widths.
            .min((width - 82.0).max(0.0))
    };
    // Each part takes the room its text needs, up to its share of the
    // budget, so nothing is left hanging between model, effort and chevron.
    let model_budget = (width - 12.0 - effort_width - 12.0 - 2.0 * 6.0).max(0.0);
    let mut parts = vec![Element::truncated_text(model)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(theme::FOREGROUND)
        .size(None, Some(16.0))
        .max_width(model_budget)];
    if !visible_effort.is_empty() {
        parts.push(
            Element::truncated_text(visible_effort)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::MENU_MUTED)
                .size(None, Some(16.0))
                .max_width(effort_width),
        );
    }
    parts.push(
        Element::icon(Icon::ChevronDown)
            .foreground(theme::MENU_MUTED)
            .size(Some(12.0), Some(16.0)),
    );
    let visual = Element::row(parts).gap(6.0).size(None, Some(16.0));
    let trigger = theme::interactive_surface(Element::clickable("composer_model_toggle", visual))
        .background(Color::TRANSPARENT)
        .padding(6.0)
        .size(None, Some(SELECTOR_HEIGHT))
        .max_width(width);
    let trigger = Tooltip::new(
        trigger,
        "composer_model_toggle",
        if model_unavailable {
            composer_model_display(props)
        } else {
            "Elegir modelo y esfuerzo"
        },
        theme::component_palette(),
    )
    .description(if props.running { "Tu selección se aplicará al finalizar la respuesta actual, para el próximo mensaje. Las indicaciones que envíes mientras tanto usan la configuración actual." } else { "Modelo y esfuerzo para el próximo mensaje." })
    .max_width(320.0)
    .shortcut(props.keymap_profile.shortcut(ShortcutAction::ModelPicker))
    .render();
    let sizing = if props.model_list_open {
        SelectorSizing::CATALOG
    } else {
        SelectorSizing::COMPACT
    };
    let menu_width = sizing.fit_width(props.width - 16.0);
    // The fixed header and connection action leave this budget for rows.
    let max_visible_items = if compact { 8 } else { 10 };
    let menu_entries = model_effort_entries(props, menu_width);
    let mut dropdown = DropdownMenu::new(
        trigger,
        menu_entries,
        open,
        "composer_model_dismiss",
        theme::selector_palette(),
    )
    .context_menu_style()
    .width(menu_width)
    .placement(PopoverPlacement::TopEnd)
    .motion(MenuMotion {
        open_ms: 220,
        item_ms: 0,
    })
    .close_animation_ms(150)
    .open_scale(0.96)
    .view_key(model_menu_view_key(props))
    .max_visible_items(max_visible_items);
    if props.model_list_open {
        dropdown = dropdown.header(model_list_fixed_header(props, menu_width - 8.0));
    }
    if props.selected_backend == "native" && props.model_list_open {
        dropdown = dropdown.footer(connect_provider_action(menu_width - 8.0));
    }
    Some(dropdown.render())
}

/// The way to another provider, under the catalog however far it scrolls.
fn connect_provider_action(width: f32) -> Element {
    Button::new(
        "composer_connect_provider",
        "Conectar proveedor",
        theme::selector_palette(),
    )
    .icon(Icon::Add)
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Compact)
    .width(width)
    .render()
}

fn send_control(props: &ComposerProps<'_>) -> Element {
    let model_available =
        props.selected_backend != "native" || selected_native_model_available(props);
    let surface = if props.running || !model_available {
        theme::SELECTED
    } else if props.home {
        Color::WHITE
    } else {
        SEND_BLUE
    };
    let ink = if props.home && !props.running && model_available {
        PILL_SURFACE
    } else {
        Color::WHITE
    };
    let mut palette = theme::action_palette(surface, ink);
    palette.hover = if props.running || !model_available {
        theme::hover_for(surface)
    } else if props.home {
        Color::rgb(224, 224, 224)
    } else {
        Color::rgb(52, 128, 255)
    };
    palette.selected = surface;
    palette.muted = ink;
    let trigger = IconButton::new(
        "run_toggle",
        if props.running {
            Icon::Stop
        } else {
            Icon::ArrowUp
        },
        palette,
    )
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Compact)
    .enabled(props.running || (!props.attachment_loading && model_available))
    .render()
    .size(Some(CONTROL_HEIGHT), Some(CONTROL_HEIGHT))
    .center_content()
    .radius(6.0);
    let tooltip = Tooltip::new(
        trigger,
        "run_toggle",
        if props.running {
            "Detener"
        } else if props.attachment_loading {
            "Preparando adjuntos…"
        } else if !model_available {
            "Elige un modelo disponible para enviar"
        } else {
            "Enviar"
        },
        theme::component_palette(),
    );
    if props.running || props.attachment_loading || !model_available {
        tooltip.render()
    } else {
        tooltip.shortcut("Enter").render()
    }
}

fn microphone_control(props: &ComposerProps<'_>) -> Element {
    Tooltip::new(
        IconButton::new(
            "composer_microphone",
            Icon::Microphone,
            theme::action_palette(
                if props.listening {
                    SEND_BLUE
                } else {
                    PILL_SURFACE
                },
                Color::rgb(240, 240, 240),
            ),
        )
        .variant(ButtonVariant::Subtle)
        .size(ControlSize::Compact)
        .render()
        .border(
            if props.listening {
                SEND_BLUE
            } else if props.home {
                Color::TRANSPARENT
            } else {
                Color::rgb(68, 68, 68)
            },
            1.0,
        )
        .size(Some(CONTROL_HEIGHT), Some(CONTROL_HEIGHT))
        .radius(6.0),
        "composer_microphone",
        if props.listening {
            "Detener dictado"
        } else {
            "Dictar mensaje"
        },
        theme::component_palette(),
    )
    .render()
}

fn trailing_actions(props: &ComposerProps<'_>) -> Element {
    let compact = props.width < COMPACT_CONTEXT_WIDTH;
    let mut row = Vec::new();
    if props.width >= 260.0 {
        if let Some(model) = model_effort_group(props, compact) {
            row.push(model);
        }
    }
    if props.width >= 380.0 {
        row.push(microphone_control(props));
    }
    row.push(send_control(props));
    Element::row(row).gap(4.0).size(None, Some(CONTROL_HEIGHT))
}

fn attachment_rows(props: &ComposerProps<'_>) -> Vec<Element> {
    let viewport_width = (props.width - 16.0).max(80.0);
    // Keep the scrollbar beside the cards, clear of their remove buttons.
    let inner_width = (viewport_width - 12.0).max(80.0);
    let mut rows = Vec::new();
    let mut palette = theme::component_palette();
    palette.surface = theme::PANEL;
    palette.raised = theme::INPUT;
    for images in [true, false] {
        let mut cards = Vec::new();
        let mut occupied = 0.0;
        let row_height = if images {
            inner_width.min(Attachment::PREVIEW_SIZE)
        } else {
            Attachment::FILE_HEIGHT
        };
        for (index, attachment) in props.attachments.iter().enumerate() {
            let fallback =
                crate::composer_attachments::fallback_visual(&attachment.path, attachment.is_dir);
            let key = crate::composer_attachments::visual_key(props.workspace, &attachment.path);
            let visual = props
                .attachment_visuals
                .and_then(|visuals| visuals.get(&key))
                .unwrap_or(&fallback);
            if visual.is_image != images {
                continue;
            }
            let width = inner_width.min(if images {
                Attachment::PREVIEW_SIZE
            } else {
                220.0
            });
            if !cards.is_empty() && occupied + 8.0 + width > inner_width {
                rows.push(
                    Element::row(std::mem::take(&mut cards))
                        .gap(8.0)
                        .size(Some(inner_width), Some(row_height)),
                );
                occupied = 0.0;
            }
            let mut card = Attachment::new(
                format!("composer_attachment:{index}"),
                &attachment.label,
                palette,
            )
            .description(&visual.description)
            .help(format!("{}\n{}", attachment.path, visual.description))
            .icon(visual.icon)
            .orientation(if images {
                AttachmentOrientation::Vertical
            } else {
                AttachmentOrientation::Horizontal
            })
            .remove_action(format!("composer_attachment_remove:{index}"))
            .width(width);
            if images {
                card = card
                    .preview_only()
                    .open_action(format!("composer_attachment_open:{index}"));
            }
            if let Some(preview) = &visual.preview {
                card = card.preview(preview.clone());
            }
            occupied += if cards.is_empty() { width } else { 8.0 + width };
            cards.push(card.render());
        }
        if images && props.attachment_loading {
            if !cards.is_empty() && occupied + 8.0 + row_height > inner_width {
                rows.push(
                    Element::row(std::mem::take(&mut cards))
                        .gap(8.0)
                        .size(Some(inner_width), Some(row_height)),
                );
            }
            let mut loading_palette = palette;
            loading_palette.hover = theme::hover_for(palette.raised);
            cards.push(
                Skeleton::block(
                    "composer_attachment_loading",
                    loading_palette,
                    row_height,
                    row_height,
                )
                .radius(16.0)
                .render(),
            );
        }
        if !cards.is_empty() {
            rows.push(
                Element::row(cards)
                    .gap(8.0)
                    .size(Some(inner_width), Some(row_height)),
            );
        }
    }
    if rows.is_empty() {
        return rows;
    }
    let height = rows
        .iter()
        .map(|row| row.style.height.unwrap_or(0.0))
        .sum::<f32>()
        + 8.0 * rows.len().saturating_sub(1) as f32;
    vec![openide_ui::ScrollArea::new(
        "composer_attachments",
        Element::column(rows).gap(8.0).size(Some(inner_width), None),
    )
    .appearance(openide_ui::ScrollbarAppearance::reserved().tint(theme::MENU_MUTED))
    .render()
    .size(Some(viewport_width), Some(height.min(160.0)))]
}

fn policy_controls(props: &ComposerProps<'_>) -> Element {
    let Some((permissions, command_isolation, open)) = props.permission_controls else {
        return Element::spacer();
    };
    let selector = crate::composer_permissions_ui::selector(
        crate::composer_permissions_ui::ComposerPermissionsProps {
            backend: props.selected_backend,
            turn_mode: props.turn_mode,
            permissions,
            command_isolation,
            full_access: props.full_access,
            open,
            running: props.running,
            width: props.width,
        },
    );
    selector.centered_y().size(None, Some(CONTROL_HEIGHT))
}

fn leading_actions(props: &ComposerProps<'_>) -> Element {
    let mut actions = vec![add_context_control(props)];
    if props.permission_controls.is_some() {
        actions.push(policy_controls(props));
    }
    Element::row(actions)
        .gap(4.0)
        .size(None, Some(CONTROL_HEIGHT))
}

/// The project, branch and goal of the next message, as wide as the strip
/// above the card leaves them.
fn composer_metadata(props: &ComposerProps<'_>) -> Element {
    project_metadata(&ComposerProps {
        width: Composer::header_strip_width(props.width),
        ..*props
    })
}

fn expand_control(props: &ComposerProps<'_>) -> Element {
    IconButton::new(
        "composer_expand_toggle",
        if props.expanded {
            Icon::Collapse
        } else {
            Icon::Expand
        },
        theme::action_palette(PILL_SURFACE, theme::MENU_MUTED),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Compact)
    .tooltip(if props.expanded {
        "Reducir editor"
    } else {
        "Ampliar editor"
    })
    .render()
    .size(Some(28.0), Some(28.0))
}

/// Builds one responsive composer. Visual labels come from the actual backend
/// and its supported options; unsupported controls are omitted.
pub(crate) fn build_composer(props: &ComposerProps<'_>) -> Element {
    build_composer_with_height(props, None)
}

/// Keep a growing editor within the space left by its host. The document and
/// cursor remain intact; the text area's existing internal scroll takes over.
pub(crate) fn build_composer_with_height(
    props: &ComposerProps<'_>,
    available_height: Option<f32>,
) -> Element {
    if props.selected_backend != "native" {
        return Element::column(vec![
        Element::text("Historial de una CLI externa")
            .font_size(14.0)
            .foreground(MUTED_INK),
        Element::text("Esta conversación conserva su motor original. Continúa tu trabajo en una nueva tarea del harness nativo.")
            .font_size(13.0)
            .foreground(MUTED_INK),
        theme::interactive_surface(Element::button("new_session", "Nueva tarea nativa"))
            .padding(10.0)
            .background(PILL_SURFACE),
    ]).gap(8.0).size(Some(props.width), None);
    }
    let compact = props.width < COMPACT_CONTEXT_WIDTH;
    let attachments = attachment_rows(props);
    let (link_highlights, link_preview) =
        crate::rich_text::composer_links(props.prompt, (props.width - 16.0).max(0.0));
    let requested_minimum = if props.expanded {
        280.0
    } else {
        CONTROL_HEIGHT
    };
    let requested_maximum = if props.expanded {
        320.0
    } else if compact {
        128.0
    } else if props.home {
        160.0
    } else {
        180.0
    };
    // The card uses 8px padding and a 4px editor/control gap. Its inset
    // environment strip occupies 32px directly above it. Attachment rows
    // expose their bounded height. Pending
    // selection/error notices retain their natural height outside this pill;
    // they can require more space than the normal editor budget permits.
    let chrome_height = 16.0
        + 4.0
        + if link_preview.is_some() { 28.0 } else { 0.0 }
        + CONTROL_HEIGHT
        + CONTEXT_STRIP_HEIGHT
        + attachments
            .iter()
            .map(|row| row.style.height.unwrap_or(0.0) + 6.0)
            .sum::<f32>();
    let editor_maximum =
        available_height
            .filter(|height| height.is_finite())
            .map_or(requested_maximum, |height| {
                (height - chrome_height)
                    .max(CONTROL_HEIGHT)
                    .min(requested_maximum)
            });
    let mut palette = theme::component_palette();
    palette.selected = PILL_SURFACE;
    let mut composer = Composer::new(
        "prompt",
        props.prompt,
        props.placeholder,
        props.width,
        palette,
    )
    .auto_grow(requested_minimum.min(editor_maximum), editor_maximum)
    .compact(compact)
    .attachments(attachments)
    .pill_controls(leading_actions(props), trailing_actions(props))
    .editor_font_size(13.0)
    .editor_highlights(link_highlights)
    .border_color(if props.home {
        Color::rgb(48, 48, 48)
    } else {
        PILL_SURFACE
    })
    .shadow_color(Color::TRANSPARENT)
    .card_radius(CARD_RADIUS);
    if let Some(preview) = link_preview {
        composer = composer.link_preview(preview);
    }
    if props.home && (!props.prompt.is_empty() || props.expanded) {
        composer = composer.editor_action(expand_control(props));
    }
    let composer = composer
        .header_strip(
            composer_metadata(props),
            theme::action_palette(theme::PANEL, theme::FOREGROUND),
        )
        .render();
    let composer = if let Some(suggestions) = props.suggestions {
        crate::composer_suggestions::popup(composer, suggestions, props.width)
    } else {
        composer
    };
    let composer = if let Some(pending) = props.pending {
        let mut content = vec![
            Element::text("Para el próximo mensaje")
                .font_size(12.0)
                .foreground(theme::FOREGROUND),
            Element::text(pending.summary())
                .font_size(12.0)
                .foreground(theme::FOREGROUND_SUBTLE),
        ];
        if let Some(error) = &pending.error {
            content.push(
                Element::text(error)
                    .font_size(12.0)
                    .foreground(theme::DESTRUCTIVE),
            );
        }
        let notice = Element::row(vec![
            Element::column(content).gap(3.0).flex(1.0),
            Button::new(
                format!("composer_pending_undo:{}", pending.token),
                "Deshacer",
                theme::action_palette(theme::CARD, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .width(80.0)
            .render(),
        ])
        .gap(8.0)
        .padding(10.0)
        .background(theme::CARD)
        .radius(8.0)
        .size(Some(props.width), None);
        let notice = Tooltip::new(notice, "composer_pending_notice", "Para el próximo mensaje", theme::component_palette())
            .description("Se aplicará al finalizar la respuesta actual. Las indicaciones que envíes mientras tanto usan la configuración actual.")
            .max_width(320.0)
            .render();
        Element::column(vec![notice, composer]).gap(6.0)
    } else {
        composer
    };
    let mut notices = Vec::new();
    if let Some(error) = props.error {
        notices.push(crate::settings_parts::feedback_banner(
            error,
            crate::settings_parts::FeedbackTone::Error,
            None,
            compact,
        ));
    }
    if let Some(failure) = props.check_failure {
        // Only the last line fits a banner; "Corregir" carries the full output.
        let summary = failure.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or(failure);
        let summary: String = summary.chars().take(200).collect();
        let actions = Element::row(vec![
            Button::new("turn_check_dismiss", "Descartar", theme::component_palette())
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
            Button::new("turn_check_fix", "Corregir", theme::component_palette())
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
        ])
        .gap(6.0);
        notices.push(crate::settings_parts::feedback_banner(
            format!("El chequeo del proyecto falló: {summary}"),
            crate::settings_parts::FeedbackTone::Warning,
            Some(actions),
            compact,
        ));
    }
    if notices.is_empty() {
        composer
    } else {
        notices.push(composer);
        Element::column(notices).gap(8.0)
    }
}

#[cfg(test)]
#[path = "composer_layout_visual_tests.rs"]
mod layout_visual_tests;

#[cfg(test)]
#[path = "composer_menu_visual_tests.rs"]
mod menu_visual_tests;

#[cfg(test)]
#[path = "model_card_visual_tests.rs"]
mod model_card_visual_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::composer_selection::RouteSource;
    use openide_ui::{DrawCommand, ElementKind, HitKind, Point, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn running_composer_keeps_model_permissions_and_pending_undo_accessible_at_small_widths() {
        let mut pending = crate::composer_pending::PendingComposerSelection::new(
            crate::composer_pending::ComposerScope {
                workspace: "/workspace".into(),
                session: Some("owner".into()),
            },
        );
        pending.permissions = Some(crate::composer_permissions_ui::settings_for_preset(
            crate::agent_tool_settings::AgentToolSettings::default(),
            crate::composer_permissions_ui::ComposerPermissionPreset::ReadOnly,
        ));
        for width in [280.0, 480.0, 900.0] {
            let mut props = props("Revisa el proyecto", width);
            props.running = true;
            props.pending = Some(&pending);
            let settings = pending.permissions.unwrap();
            props.permission_controls =
                Some((settings.permissions(), settings.command_isolation(), false));
            let mut tree = UiTree::new(build_composer(&props));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 440.0));
            for id in [
                "composer_model_toggle".to_owned(),
                "composer:permissions".to_owned(),
                format!("composer_pending_undo:{}", pending.token),
            ] {
                let region = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap_or_else(|| panic!("missing enabled {id} at {width}"));
                assert!(
                    region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.1,
                    "{id} at {width}"
                );
                assert!(tree.focus(&id), "keyboard access for {id}");
            }
            assert!(frame
                .text_layouts()
                .iter()
                .any(|block| block.source == "Para el próximo mensaje"));
        }
    }

    fn fixture_native_routes(models: &[(String, String)]) -> Vec<ModelRouteOption> {
        models
            .iter()
            .map(|(id, name)| ModelRouteOption {
                route_id: format!("native:openai:{id}"),
                backend: "native".into(),
                provider_id: "openai".into(),
                provider_name: "OpenAI".into(),
                model_id: id.clone(),
                model_name: name.clone(),
                credential: CredentialAvailability::EnvPresentUnverified,
                execution: ExecutionSupport::Verified,
                source: RouteSource::AdapterReview,
                catalog_present: true,
                provider_listed: Some(true),
                effort_options: Vec::new(),
            })
            .collect()
    }

    fn default_fixture_routes() -> &'static [ModelRouteOption] {
        static ROUTES: std::sync::LazyLock<Vec<ModelRouteOption>> =
            std::sync::LazyLock::new(|| {
                fixture_native_routes(&[("gpt-6-sol".into(), "GPT-6 Sol".into())])
            });
        ROUTES.as_slice()
    }

    pub(crate) fn props<'a>(prompt: &'a str, width: f32) -> ComposerProps<'a> {
        ComposerProps {
            check_failure: None,
            keymap_profile: KeymapProfile::default(),
            embedded_in_ide: false,
            environment: None,
            workspace: "C:/projects/openIDE",
            project_action: None,
            project_branch: Some("main"),
            branch_picker: None,
            branch_menu_open: false,
            project_menu_open: false,
            project_query: "",
            project_options: &[],
            prompt,
            attachments: &[],
            attachment_visuals: None,
            attachment_loading: false,
            add_menu_open: false,
            listening: false,
            placeholder: "Ask me anything",
            selected_backend: "native",
            native_provider_id: "openai",
            model_label: "GPT-6 Sol",
            model_options: &[],
            model_routes: default_fixture_routes(),
            model_metadata: None,
            provider_status: None,
            selected_model: "gpt-6-sol",
            model_menu_open: false,
            model_list_open: false,
            model_query: "",
            collapsed_model_providers: None,
            search_collapsed_model_providers: None,
            advanced_open: false,
            effort_label: "",
            effort_options: &[],
            selected_effort: "default",
            effort_menu_open: false,
            effort_preview: None,
            turn_mode: TurnMode::Build,
            permission_controls: None,
            full_access: false,
            suggestions: None,
            running: false,
            pending: None,
            error: None,
            native_context: None,
            context_compaction: Default::default(),
            context_open: false,
            goal: None,
            goal_editor_open: false,
            goal_draft: "",
            native_goal: None,
            width,
            home: false,
            expanded: false,
        }
    }

    pub(crate) fn configured_effort_route() -> ModelRouteOption {
        ModelRouteOption {
            route_id: "native:codex:gpt-5.6-luna".into(),
            backend: "native".into(),
            provider_id: "codex".into(),
            provider_name: "Codex".into(),
            model_id: "gpt-5.6-luna".into(),
            model_name: "GPT-5.6 Luna".into(),
            credential: CredentialAvailability::EnvPresentUnverified,
            execution: ExecutionSupport::ProviderListedUnverified,
            source: RouteSource::ProviderDiscovery,
            catalog_present: true,
            provider_listed: Some(true),
            effort_options: vec![
                ("default".into(), "Automático".into()),
                ("max".into(), "Máximo".into()),
            ],
        }
    }

    #[test]
    fn effort_pill_reserves_proportional_font_width_without_growing_the_control() {
        let routes = [configured_effort_route()];
        let context = eframe::egui::Context::default();
        context.begin_pass(eframe::egui::RawInput::default());
        for width in [360.0, 900.0] {
            for (effort, label) in [("max", "Máximo"), ("default", "Automático")] {
                let mut state = props("Escribir un mensaje", width);
                state.native_provider_id = "codex";
                state.selected_model = "gpt-5.6-luna";
                state.model_label = "GPT-5.6 Luna";
                state.model_routes = &routes;
                state.effort_options = &routes[0].effort_options;
                state.selected_effort = effort;
                state.effort_label = label;
                state.permission_controls = Some((
                    openide_harness::ToolPermissions::default(),
                    openide_harness::CommandIsolation::HostUnrestricted,
                    false,
                ));
                let mut tree = UiTree::new(build_composer(&state));
                let measure_context = context.clone();
                tree.set_text_measurer(move |text, size, family| {
                    measure_context.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                text.to_owned(),
                                eframe::egui::FontId::new(
                                    size,
                                    match family {
                                        FontFamily::Sans => eframe::egui::FontFamily::Proportional,
                                        FontFamily::Monospace => {
                                            eframe::egui::FontFamily::Monospace
                                        }
                                    },
                                ),
                                eframe::egui::Color32::WHITE,
                            )
                            .size()
                            .x
                    })
                });
                let frame = tree.layout(Size::new(width, 420.0));
                let pill = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "composer_model_toggle")
                    .expect("model pill fits the composer");
                assert!(pill.rect.width <= 250.0);
                assert!(pill.rect.x >= 0.0 && pill.rect.x + pill.rect.width <= width);
                if width == 900.0 {
                    assert!(
                        frame.commands.iter().any(|command| matches!(command,
                            DrawCommand::Text { text, .. } if text == label
                        )),
                        "{label} should be complete in the standard pill"
                    );
                    assert!(frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, .. } if text == "GPT-5.6 Luna"
                    )));
                } else {
                    // Extra toolbar space can now fit a complete label at
                    // narrow widths. Check the painted text, not truncation.
                    for command in &frame.commands {
                        if let DrawCommand::Text {
                            position,
                            text,
                            font_size,
                            ..
                        } = command
                        {
                            if pill.rect.contains(*position) {
                                let text_width = context.fonts_mut(|fonts| {
                                    fonts
                                        .layout_no_wrap(
                                            text.clone(),
                                            eframe::egui::FontId::proportional(*font_size),
                                            eframe::egui::Color32::WHITE,
                                        )
                                        .size()
                                        .x
                                });
                                assert!(
                                    position.x + text_width <= pill.rect.x + pill.rect.width + 0.1,
                                    "{text} must stay inside the narrow model control"
                                );
                            }
                        }
                    }
                }
            }
        }
        let mut output = context.end_pass();
        output.textures_delta.clear();
    }

    #[test]
    fn configured_effort_stays_visible_during_verification_without_enabling_send_or_selection() {
        let routes = [configured_effort_route()];
        for status in [
            ProviderModelStatus::Checking,
            ProviderModelStatus::CheckFailed,
        ] {
            let statuses = BTreeMap::from([("codex".into(), status)]);
            let mut state = props("Hello", 620.0);
            state.native_provider_id = "codex";
            state.selected_model = "gpt-5.6-luna";
            state.model_label = "GPT-5.6 Luna";
            state.model_routes = &routes;
            state.provider_status = Some(&statuses);
            state.effort_options = &routes[0].effort_options;
            state.selected_effort = "max";
            state.effort_label = "Máximo";
            state.model_menu_open = true;
            assert!(selected_native_model_configured(&state));
            assert!(!selected_native_model_available(&state));
            assert_eq!(composer_model_display(&state), "GPT-5.6 Luna");
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(620.0, 720.0));
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "composer_advanced_toggle"));
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "composer_advanced_toggle"));
            assert!(frame.commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == "Máximo")
            ));

            state.advanced_open = true;
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(620.0, 720.0));
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "composer_advanced_back"));

            state.model_menu_open = false;
            state.advanced_open = false;
            let mut tree = UiTree::new(build_composer(&state));
            let frame = tree.layout(Size::new(620.0, 420.0));
            if let Some(region) = frame
                .regions
                .iter()
                .find(|region| region.id == "run_toggle")
            {
                let point = Point::new(
                    region.rect.x + region.rect.width / 2.0,
                    region.rect.y + region.rect.height / 2.0,
                );
                tree.handle_event(UiEvent::PointerDown(point));
                assert!(!tree.handle_event(UiEvent::PointerUp(point)).iter().any(
                    |action| matches!(action, UiAction::Clicked { id } if id == "run_toggle")
                ));
            }
            state.model_menu_open = true;
            state.model_list_open = true;
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(620.0, 720.0));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("composer_route:")
                    && region.kind == HitKind::Button));
        }
    }

    #[test]
    fn configured_effort_hides_when_route_is_denied_or_has_no_announced_options() {
        for denied in [false, true] {
            let mut routes = [configured_effort_route()];
            let mut state = props("Hello", 620.0);
            state.native_provider_id = "codex";
            state.selected_model = "gpt-5.6-luna";
            state.model_label = "GPT-5.6 Luna";
            if denied {
                routes[0].execution = ExecutionSupport::Unsupported;
            } else {
                routes[0].effort_options.clear();
            }
            state.model_routes = &routes;
            state.effort_options = &routes[0].effort_options;
            state.effort_label = if denied { "Máximo" } else { "" };
            state.model_menu_open = true;
            state.advanced_open = true;
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(620.0, 720.0));
            assert!(!frame.regions.iter().any(|region| matches!(
                region.id.as_str(),
                "composer_advanced_toggle"
                    | "composer_advanced_back"
                    | "composer_advanced_chip_toggle"
            )));
            assert!(!frame.commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == "Máximo")
            ));
        }
    }

    #[test]
    fn configured_effort_hides_with_missing_credentials_or_unlisted_model() {
        for missing_credential in [false, true] {
            let mut routes = [configured_effort_route()];
            if missing_credential {
                routes[0].credential = CredentialAvailability::MissingCredential;
            } else {
                routes[0].provider_listed = Some(false);
            }
            let mut state = props("Hello", 620.0);
            state.native_provider_id = "codex";
            state.selected_model = "gpt-5.6-luna";
            state.model_routes = &routes;
            state.effort_options = &routes[0].effort_options;
            state.effort_label = "Máximo";
            state.model_menu_open = true;
            assert!(!selected_native_model_configured(&state));
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(620.0, 720.0));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "composer_advanced_toggle"));
            assert!(!frame.commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == "Máximo")
            ));
        }
    }

    #[test]
    fn configured_effort_does_not_use_another_model_or_providers_contract() {
        let routes = [configured_effort_route()];
        for (provider_id, model_id) in [
            ("codex", "different-model"),
            ("different-provider", "gpt-5.6-luna"),
        ] {
            let mut state = props("Hello", 620.0);
            state.native_provider_id = provider_id;
            state.selected_model = model_id;
            state.model_routes = &routes;
            state.effort_options = &routes[0].effort_options;
            state.effort_label = "Máximo";
            state.model_menu_open = true;
            assert!(!selected_native_model_configured(&state));
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(620.0, 720.0));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "composer_advanced_toggle"));
            assert!(!frame.commands.iter().any(
                |command| matches!(command, DrawCommand::Text { text, .. } if text == "Máximo")
            ));
        }
    }

    #[test]
    fn project_context_is_above_the_card_and_primary_controls_remain_inside() {
        let frame = UiTree::new(build_composer(&props("", 620.0))).layout(Size::new(620.0, 420.0));
        let rect = |id: &str| frame.regions.iter().find(|r| r.id == id).unwrap().rect;
        let editor = rect("prompt");
        let project = rect("composer_project_toggle");
        let plus = rect("composer_add_context");
        let mic = rect("composer_microphone");
        let send = rect("run_toggle");
        assert!(project.y + project.height < editor.y);
        assert_eq!(editor.width, 604.0);
        assert!(editor.y + editor.height < plus.y);
        assert_eq!(plus.y, mic.y);
        assert_eq!(mic.y, send.y);
        assert!(plus.x < mic.x && mic.x < send.x);
        assert_eq!(plus.width, CONTROL_HEIGHT);
        assert_eq!(send.width, CONTROL_HEIGHT);
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, radius, .. }
                if *color == PILL_SURFACE && *radius == CARD_RADIUS
        )));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == SEND_BLUE
        )));
    }

    #[test]
    fn composer_header_has_context_and_project_without_plan_usage_action() {
        for width in [420.0, 620.0, 900.0, 1400.0] {
            let mut state = props("Un mensaje\ncon dos líneas", width);
            state.native_provider_id = "codex";
            let frame = UiTree::new(build_composer(&state)).layout(Size::new(width, 600.0));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "provider_usage"));
            assert!(!frame
                .text_layouts()
                .iter()
                .any(|block| block.source.contains("Uso del plan")));
            for id in ["composer_project_toggle", "composer_context_toggle"] {
                let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.1);
            }
        }
    }

    #[test]
    fn project_environment_branch_strip_stays_above_input_and_permissions_stay_next_to_add() {
        let environment = EnvironmentUiState::default();
        for width in [280.0, 312.0, 360.0, 620.0, 800.0] {
            for home in [false, true] {
                for embedded_in_ide in [false, true] {
                    let mut state = props("", width);
                    state.home = home;
                    state.embedded_in_ide = embedded_in_ide;
                    state.environment = Some(&environment);
                    state.workspace = "/workspace/a-project-with-a-long-name";
                    state.project_branch = Some("feature/a-long-branch-name");
                    state.permission_controls = Some((
                        openide_harness::ToolPermissions::default(),
                        openide_harness::CommandIsolation::HostUnrestricted,
                        false,
                    ));
                    let frame = UiTree::new(build_composer(&state)).layout(Size::new(width, 600.0));
                    let rect = |id: &str| {
                        frame
                            .regions
                            .iter()
                            .find(|region| region.id == id)
                            .unwrap_or_else(|| {
                                panic!(
                                    "missing {id} at {width}, home={home}, ide={embedded_in_ide}"
                                )
                            })
                            .rect
                    };
                    let editor = rect("prompt");
                    let local = rect("composer_environment_toggle");
                    let context = rect("composer_context_toggle");
                    assert_eq!(local.height, CONTEXT_HEIGHT);
                    assert_eq!(context.height, CONTEXT_HEIGHT);
                    assert_eq!(local.y, context.y);
                    assert!(local.y + local.height < editor.y);
                    assert!(local.x + local.width <= context.x);
                    assert!(context.x + context.width <= width + 0.1);
                    if embedded_in_ide {
                        assert!(!frame.regions.iter().any(|region| matches!(
                            region.id.as_str(),
                            "composer_project_toggle" | "composer_branch_toggle"
                        )));
                    } else {
                        let project = rect("composer_project_toggle");
                        let branch = rect("composer_branch_toggle");
                        assert_eq!(project.y, local.y);
                        assert_eq!(branch.y, local.y);
                        assert!(
                            project.width >= 72.0,
                            "project remains readable: {project:?}"
                        );
                        assert!(project.x + project.width <= local.x);
                        assert!(local.x + local.width <= branch.x);
                        assert!(branch.x + branch.width <= context.x);
                    }
                    let plus = rect("composer_add_context");
                    let permissions = rect("composer:permissions");
                    let send = rect("run_toggle");
                    assert_eq!(permissions.y, plus.y);
                    assert_eq!(permissions.y, send.y);
                    assert!(permissions.x >= plus.x + plus.width);
                    assert!(permissions.x + permissions.width < send.x);
                    assert!(editor.width >= width - 18.0);
                    assert!(editor.y + editor.height <= plus.y);
                    let strip = frame
                        .commands
                        .iter()
                        .find_map(|command| match command {
                            DrawCommand::FillCornerRect { rect, color, radii }
                                if *color == theme::PANEL
                                    && *radii == openide_ui::CornerRadii::top(CARD_RADIUS)
                                    && (rect.height - CONTEXT_STRIP_HEIGHT).abs() < 0.1 =>
                            {
                                Some(*rect)
                            }
                            _ => None,
                        })
                        .expect("integrated context strip");
                    let card = frame
                        .commands
                        .iter()
                        .find_map(|command| match command {
                            DrawCommand::FillRect {
                                rect,
                                color,
                                radius,
                            } if *color == PILL_SURFACE && *radius == CARD_RADIUS => Some(*rect),
                            _ => None,
                        })
                        .expect("composer card");
                    assert!(
                        (strip.y + strip.height - card.y).abs() < 0.1,
                        "the square strip base must meet the card without a gap"
                    );
                    assert!((strip.x - CONTEXT_STRIP_INSET).abs() < 0.1);
                    assert!((strip.width - (width - 2.0 * CONTEXT_STRIP_INSET)).abs() < 0.1);
                    assert!(local.x >= strip.x + CONTEXT_STRIP_PADDING);
                    assert!(
                        context.x + context.width
                            <= strip.x + strip.width - CONTEXT_STRIP_PADDING + 0.1
                    );
                }
            }
        }
    }

    #[test]
    fn add_menu_exposes_real_actions_and_goal_editor() {
        let mut state = props("", 620.0);
        state.add_menu_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 600.0));
        for id in [
            "composer_add_files",
            "composer_goal_open",
            "turn_mode_toggle",
            "composer_plugin_documents",
            "composer_plugin_spreadsheets",
            "composer_plugin_presentations",
            "composer_plugin_code_blocks",
        ] {
            assert!(
                frame.regions.iter().any(|region| region.id == id),
                "missing {id}"
            );
        }
        state.goal_editor_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 600.0));
        for id in [
            "composer_goal_draft",
            "composer_goal_save",
            "composer_goal_clear",
        ] {
            assert!(
                frame.regions.iter().any(|region| region.id == id),
                "missing {id}"
            );
        }
    }

    #[test]
    fn native_goal_maintenance_exposes_full_report_and_retry_in_own_ui() {
        let native = crate::native_goal_ui::NativeGoalUiState {
            recovery: openide_core::NativeGoalRecoveryReport {
                removed_temporaries: 2,
                busy_session_ids: vec!["busy-session-exact-id".into()],
                diagnostics: vec![openide_core::NativeGoalRecoveryDiagnostic {
                    session_id: "corrupt-session-exact-id".into(),
                    error: format!("Detalle completo: {} FIN", "diagnóstico largo ".repeat(100)),
                }],
            },
            ..Default::default()
        };
        let description = native.recovery_description();
        assert!(description.ends_with(" FIN"));
        for width in [420.0, 900.0] {
            let mut state = props("", width);
            state.add_menu_open = true;
            state.goal_editor_open = true;
            state.native_goal = Some(&native);
            let mut tree = UiTree::new(build_composer(&state));
            let frame = tree.layout(Size::new(width, 900.0));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|block| block.source == description));
            assert!(frame
                .regions
                .iter()
                .any(|r| r.id == "native_goal_recovery_details"));
            let rect = frame
                .regions
                .iter()
                .find(|r| r.id == "native_goal_maintenance_retry")
                .unwrap()
                .rect;
            assert!(rect.x >= 0.0 && rect.x + rect.width <= width + 0.1);
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert!(tree.handle_event(UiEvent::PointerUp(point)).iter().any(|action| matches!(action, UiAction::Clicked { id } if id == "native_goal_maintenance_retry")));
            state.running = true;
            let mut tree = UiTree::new(build_composer(&state));
            tree.layout(Size::new(width, 900.0));
            tree.handle_event(UiEvent::PointerDown(point));
            assert!(!tree.handle_event(UiEvent::PointerUp(point)).iter().any(|action| matches!(action, UiAction::Clicked { id } if id == "native_goal_maintenance_retry")));
        }
    }

    #[test]
    fn native_goal_editor_renders_activation_criteria_and_scoped_pause_action() {
        use openide_core::{NativeGoalCriterion, NativeGoalStore, SessionStore};
        let root = tempfile::tempdir().unwrap();
        let sessions = std::sync::Arc::new(SessionStore::open(root.path()).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let goal = NativeGoalStore::new(sessions)
            .create(
                &lease,
                "goal",
                "Complete all requirements",
                vec![NativeGoalCriterion {
                    id: "verify".into(),
                    description: "Verify actual recovery".into(),
                    evidence: None,
                }],
            )
            .unwrap();
        let mut native = crate::native_goal_ui::NativeGoalUiState {
            session_id: Some(session.id),
            ..Default::default()
        };
        for width in [420.0, 900.0] {
            let mut state = props("", width);
            state.add_menu_open = true;
            state.goal_editor_open = true;
            state.native_goal = Some(&native);
            let mut tree = UiTree::new(build_composer(&state));
            let frame = tree.layout(Size::new(width, 900.0));
            assert!(frame.regions.iter().any(|r| r.id == "native_goal_criteria"));
            let rect = frame
                .regions
                .iter()
                .find(|r| r.id == "native_goal_activate")
                .unwrap()
                .rect;
            assert!(rect.x >= 0.0 && rect.x + rect.width <= width + 0.1);
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert!(tree
                .handle_event(UiEvent::PointerUp(point))
                .iter()
                .any(|a| matches!(a, UiAction::Clicked {id} if id == "native_goal_activate")));
        }
        native.goal = Some(goal);
        let control = native.control_id().unwrap();
        let mut state = props("", 620.0);
        state.add_menu_open = true;
        state.goal_editor_open = true;
        state.native_goal = Some(&native);
        let mut tree = UiTree::new(build_composer(&state));
        let frame = tree.layout(Size::new(620.0, 900.0));
        assert!(!frame.regions.iter().any(|r| r.id == "native_goal_activate"));
        assert!(frame
            .text_layouts()
            .iter()
            .any(|b| b.source.contains("Verify actual recovery")));
        let rect = frame.regions.iter().find(|r| r.id == control).unwrap().rect;
        let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert!(tree
            .handle_event(UiEvent::PointerUp(point))
            .iter()
            .any(|a| matches!(a, UiAction::Clicked {id} if id == &control)));
    }
    #[test]
    fn project_menu_is_anchored_without_moving_pill() {
        let projects = [
            ("C:/projects/openIDE".into(), "openIDE".into()),
            ("C:/projects/other".into(), "other".into()),
        ];
        let mut state = props("", 620.0);
        state.project_options = &projects;
        let closed = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 420.0));
        let closed_prompt = closed
            .regions
            .iter()
            .find(|r| r.id == "prompt")
            .unwrap()
            .rect;
        state.project_menu_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        let open = tree.layout(Size::new(620.0, 420.0));
        assert_eq!(
            closed_prompt,
            open.regions.iter().find(|r| r.id == "prompt").unwrap().rect
        );
        let option = open
            .regions
            .iter()
            .find(|r| r.id == "composer_project_select:1")
            .unwrap()
            .rect;
        let point = Point::new(
            option.x + option.width / 2.0,
            option.y + option.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "composer_project_select:1".into()
            }]
        );
    }

    #[test]
    fn attachment_and_context_meter_reflect_supplied_state() {
        let attachments = [ComposerAttachmentView {
            path: "src/main.rs".into(),
            label: "main.rs".into(),
            is_dir: false,
        }];
        let mut state = props("", 780.0);
        state.attachments = &attachments;
        let usage = crate::native_context_ui::NativeContextUiState {
            remaining_percent: Some(43),
            window: Some(100),
            last_usage: Some(openide_core::ModelTokenUsage {
                input_tokens: 50,
                output_tokens: 7,
                total_tokens: 57,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
            ..Default::default()
        };
        state.native_context = Some(&usage);
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(780.0, 420.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_attachment_remove:0"));
        assert!(frame.commands.iter().any(|command| matches!(
            command, DrawCommand::Text { text, .. } if text == "57%"
        )));
    }

    #[test]
    fn native_context_panel_keeps_unknown_apart_from_zero_and_dismisses_by_keyboard() {
        use crate::native_context_ui::{
            NativeContextRow, NativeContextSection, NativeContextUiState,
        };
        let usage = NativeContextUiState {
            provider: "tenant.provider".into(),
            model: "models/exact:v2".into(),
            sections: vec![NativeContextSection {
                title: "Instrucciones y skills".into(),
                rows: (0..24)
                    .map(|index| {
                        NativeContextRow::new(
                    format!("SKILL-{index}.md"), "123 bytes · tokens desconocidos",
                    "Copia local revalidada; no representa tokens atribuidos por el proveedor.",
                )
                    })
                    .collect(),
            }],
            ..Default::default()
        };
        for width in [320.0, 620.0] {
            let mut state = props("Draft", width);
            state.native_context = Some(&usage);
            state.context_open = true;
            let viewport = Size::new(width, 640.0);
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(viewport);
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("backend:")));
            let meter = frame
                .regions
                .iter()
                .find(|region| region.id == "composer_context_toggle")
                .expect("context trigger remains accessible in compact mode");
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::ProgressRing { rect, fraction: None, .. }
                    if meter.rect.contains(Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)))));
            let painted = |needle: &str| {
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text.contains(needle)))
            };
            assert!(painted("Ventana no publicada: models/exact:v2"));
            assert!(!painted("0%"), "unknown is not a measured zero");
            assert!(!painted("SKILL-"), "provenance rows are only copied");
            let area = frame
                .regions
                .iter()
                .find(|region| region.id == "composer_context_summary")
                .unwrap()
                .rect;
            assert!(area.x >= 0.0 && area.x + area.width <= width + 0.1);
            assert!(crate::composer_context_ui::diagnostic(&usage).contains(
                "SKILL-23.md: 123 bytes · tokens desconocidos — Copia local revalidada"
            ));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: openide_ui::Key::Escape,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "composer_context_dismiss".into()
                }]
            );
        }
    }

    #[test]
    fn historical_cli_conversation_offers_a_native_task_without_an_engine_picker() {
        let mut state = props("Preserved historical draft", 620.0);
        state.selected_backend = "codex";
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 300.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "new_session"));
        for id in [
            "prompt",
            "run_toggle",
            "backend_menu_toggle",
            "composer_model_toggle",
            "composer_context_toggle",
        ] {
            assert!(
                !frame.regions.iter().any(|region| region.id == id),
                "legacy action {id}"
            );
        }
    }

    #[test]
    fn context_popover_lists_usage_and_categories_and_copies_the_provenance() {
        use crate::native_context_ui::{
            NativeContextCategory, NativeContextRow, NativeContextSection, NativeContextUiState,
        };
        let usage = NativeContextUiState {
            window: Some(2000),
            remaining_percent: Some(85),
            last_usage: Some(openide_core::ModelTokenUsage {
                input_tokens: 240,
                output_tokens: 60,
                total_tokens: 300,
                cached_input_tokens: Some(40),
                reasoning_tokens: Some(20),
            }),
            breakdown: vec![NativeContextCategory {
                id: "skills".into(),
                label: "Skills".into(),
                value: "2 · 400 bytes".into(),
                detail: "Diagnostic SHA-256 must be optional".into(),
                bytes: Some(400),
            }],
            sections: vec![NativeContextSection {
                title: "Detalles".into(),
                rows: vec![NativeContextRow::new(
                    "Identidad",
                    "Diagnostic SHA-256",
                    "Exact origin details",
                )],
            }],
            ..Default::default()
        };
        for width in [320.0, 620.0] {
            let mut state = props("Draft", width);
            state.native_context = Some(&usage);
            state.context_open = true;
            let viewport = Size::new(width, 640.0);
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(viewport);
            for label in [
                "Uso de contexto",
                "15% · 300 / 2K tokens",
                "Skills",
                "~100",
            ] {
                assert!(
                    frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text {text,..} if text == label)),
                    "missing {label}"
                );
            }
            assert!(!frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text {text,..} if text.contains("SHA-256"))));
            for copied in ["Identidad", "disponibles", "reportada"] {
                assert!(
                    !frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text {text,..} if text.contains(copied))),
                    "{copied} is not part of the popover"
                );
            }
            let free_track = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect { rect, color, .. }
                        if *color == theme::TRACK && (rect.height - 6.0).abs() < 0.01 =>
                    {
                        Some(rect)
                    }
                    _ => None,
                })
                .expect("the free context window must have a visible track");
            assert!(free_track.width > width.min(344.0) * 0.60);
            let summary = frame
                .regions
                .iter()
                .find(|region| region.id == "composer_context_summary")
                .unwrap();
            assert!(summary.rect.width <= 344.0 && summary.rect.height <= viewport.height - 16.0);
            assert!(tree.focus("composer_context_copy_diagnostic"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: openide_ui::Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "composer_context_copy_diagnostic".into()
                }]
            );
            assert_eq!(
                crate::composer_context_ui::diagnostic(&usage),
                "Diagnóstico del contexto\n · \nEstado: Aún no hay una respuesta medida\n\n\
                 Instrucciones y herramientas\n\
                 Skills: 2 · 400 bytes — Diagnostic SHA-256 must be optional\n\n\
                 Detalles\nIdentidad: Diagnostic SHA-256 — Exact origin details\n"
            );
        }
    }

    #[test]
    fn narrow_pill_keeps_editor_and_send_inside_bounds() {
        for width in [320.0, 360.0, 410.0, 780.0] {
            for permissions in [false, true] {
                let mut state = props("hello", width);
                if permissions {
                    state.permission_controls = Some((
                        openide_harness::ToolPermissions::default(),
                        openide_harness::CommandIsolation::HostUnrestricted,
                        false,
                    ));
                }
                let frame = UiTree::new(build_composer(&state)).layout(Size::new(width, 420.0));
                let editor = frame
                    .regions
                    .iter()
                    .find(|r| r.id == "prompt")
                    .unwrap()
                    .rect;
                assert_eq!(editor.width, width - 16.0);
                for region in frame.regions.iter().filter(|r| {
                    matches!(
                        r.id.as_str(),
                        "composer_add_context"
                            | "composer:permissions"
                            | "prompt"
                            | "composer_model_toggle"
                            | "composer_microphone"
                            | "run_toggle"
                    )
                }) {
                    let rect = region.rect;
                    assert!(
                        rect.x >= 0.0 && rect.x + rect.width <= width + 0.1,
                        "{} outside {width}: {rect:?}",
                        region.id
                    );
                    if region.id != "prompt" {
                        assert!(rect.y >= editor.y + editor.height);
                    }
                }
            }
        }
    }

    #[test]
    fn native_running_composer_shows_only_white_centered_stop() {
        for width in [320.0, 780.0] {
            for prompt in ["", "Change direction"] {
                let mut state = props(prompt, width);
                state.running = true;
                let frame = UiTree::new(build_composer(&state)).layout(Size::new(width, 420.0));
                assert!(!frame.regions.iter().any(|region| region.id == "run_steer"));
                assert!(!frame.commands.iter().any(|command| matches!(
                    command,
                    DrawCommand::Icon {
                        icon: Icon::ArrowUp,
                        ..
                    }
                )));
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "run_toggle")
                    .unwrap()
                    .rect;
                assert!(rect.x >= 0.0 && rect.x + rect.width <= width + 0.1);
                let (icon, tint) = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Icon {
                            icon: Icon::Stop,
                            rect,
                            tint,
                        } => Some((rect, tint)),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(*tint, Color::WHITE);
                assert!((icon.x + icon.width / 2.0 - rect.x - rect.width / 2.0).abs() < 0.1);
                assert!((icon.y + icon.height / 2.0 - rect.y - rect.height / 2.0).abs() < 0.1);
            }
        }
    }

    #[test]
    fn long_model_keeps_controls_inside_pill_at_compact_widths() {
        let models = [(
            "long-model".into(),
            "Un modelo con un nombre muy largo".into(),
        )];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("high".into(), "Extremadamente alto".into()),
        ];
        for width in [320.0, 410.0] {
            let mut state = props("hello", width);
            state.model_label = "Un modelo con un nombre muy largo";
            state.model_options = &models;
            let routes = fixture_native_routes(&models);
            state.model_routes = &routes;
            state.effort_label = "Extremadamente alto";
            state.effort_options = &efforts;
            state.selected_effort = "high";
            let frame = UiTree::new(build_composer(&state)).layout(Size::new(width, 420.0));
            let pill = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect {
                        rect,
                        color,
                        radius,
                    } if *color == PILL_SURFACE && *radius == CARD_RADIUS => Some(*rect),
                    _ => None,
                })
                .unwrap();
            let controls = if width >= 380.0 {
                &[
                    "composer_add_context",
                    "prompt",
                    "composer_model_toggle",
                    "composer_microphone",
                    "run_toggle",
                ][..]
            } else {
                &[
                    "composer_add_context",
                    "prompt",
                    "composer_model_toggle",
                    "run_toggle",
                ][..]
            };
            for id in controls {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == *id)
                    .unwrap()
                    .rect;
                assert!(
                    rect.x >= pill.x - 0.1 && rect.x + rect.width <= pill.x + pill.width + 0.1,
                    "{id} outside pill at {width}: {rect:?}, pill {pill:?}"
                );
            }
            let editor = frame
                .regions
                .iter()
                .find(|region| region.id == "prompt")
                .unwrap();
            assert!(editor.rect.width >= 80.0 - 0.1);
        }
    }

    #[test]
    fn metadata_hides_optional_items_before_overflowing() {
        for width in [320.0, 410.0, 490.0, 650.0] {
            let mut state = props("hello", width);
            state.workspace = "C:/projects/a-very-long-project-directory-name";
            state.project_branch = Some("a-very-long-feature-branch-name");
            state.goal = Some("A very long goal for the current project");
            let frame = UiTree::new(build_composer(&state)).layout(Size::new(width, 420.0));
            for id in ["composer_project_toggle", "composer_context_toggle"] {
                let region = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .expect(id);
                assert!(
                    region.rect.x >= -0.1 && region.rect.x + region.rect.width <= width + 0.1,
                    "{id} outside {width}: {:?}",
                    region.rect
                );
            }
        }
    }

    #[test]
    fn growing_editor_keeps_compact_controls_on_bottom_row() {
        let draft = "first line\nsecond line\nthird line\nfourth line";
        let frame =
            UiTree::new(build_composer(&props(draft, 620.0))).layout(Size::new(620.0, 500.0));
        let rect = |id: &str| frame.regions.iter().find(|r| r.id == id).unwrap().rect;
        let editor = rect("prompt");
        let plus = rect("composer_add_context");
        let mic = rect("composer_microphone");
        let send = rect("run_toggle");
        // The editor uses the full card width, and controls stay beneath it
        // as it grows. Their hit targets must never cover the text area.
        assert_eq!(editor.width, 604.0);
        assert_eq!(plus.y, editor.y + editor.height + 4.0);
        for control in [plus, mic, send] {
            assert_eq!(control.width, CONTROL_HEIGHT);
            assert_eq!(control.height, CONTROL_HEIGHT);
            assert_eq!(control.y, plus.y);
        }
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, color, radius }
                if *color == Color::rgb(64, 64, 64)
                    && *radius == 4.0
                    && rect.width == rect.height
        )));
    }

    #[test]
    fn model_menu_has_main_advanced_and_dynamic_route_views() {
        let models = [
            ("gpt-6-sol".into(), "GPT-6 Sol".into()),
            ("gpt-6-astra".into(), "GPT-6 Astra".into()),
        ];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("medium".into(), "Medio".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.effort_options = &efforts;
        state.selected_effort = "medium";
        state.effort_label = "Medio";
        state.model_menu_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_effort_slider"));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_model_list_toggle"));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_advanced_toggle"));
        assert!(!frame.regions.iter().any(|r| r.id.contains("speed")));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_route:native:openai:gpt-6-astra"));

        state.advanced_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_effort_slider"));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_advanced_back"));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_model_list_toggle"));

        state.advanced_open = false;
        state.model_list_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "composer_effort_slider"));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "composer_route:native:openai:gpt-6-astra"));
    }

    #[test]
    fn model_search_filters_by_provider_and_model_and_keeps_refresh() {
        let routes = [
            ("openai", "OpenAI", "gpt-6-sol", "GPT-6 Sol"),
            ("anthropic", "Anthropic", "claude-haiku", "Claude Haiku"),
        ]
        .map(
            |(provider_id, provider_name, model_id, model_name)| ModelRouteOption {
                route_id: format!("native:{provider_id}:{model_id}"),
                backend: "native".into(),
                provider_id: provider_id.into(),
                provider_name: provider_name.into(),
                model_id: model_id.into(),
                model_name: model_name.into(),
                credential: CredentialAvailability::NoKeyRequired,
                execution: ExecutionSupport::Verified,
                source: RouteSource::AdapterReview,
                catalog_present: true,
                provider_listed: None,
                effort_options: Vec::new(),
            },
        );
        let mut state = props("", 620.0);
        state.model_routes = &routes;
        state.model_menu_open = true;
        state.model_list_open = true;
        state.model_query = "ÁNTHRÓPIC haiku";
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        for id in [
            "composer_model_search",
            "composer_routes_refresh",
            "composer_route:native:anthropic:claude-haiku",
        ] {
            assert!(frame.regions.iter().any(|region| region.id == id), "{id}");
        }
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "composer_route:native:openai:gpt-6-sol"));

        state.model_query = "sin coincidencias";
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 530.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_routes_refresh"));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.contains("Sin resultados")
        )));
    }

    #[test]
    fn selector_views_and_actions_fit_compact_viewport() {
        let models = [
            ("gpt-6-sol".into(), "GPT-6 Sol".into()),
            ("gpt-6-astra".into(), "GPT-6 Astra".into()),
        ];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let width = 320.0;
        let viewport = Size::new(width, 520.0);
        let mut state = props("", width);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        let cases = [
            (false, false, "composer_advanced_toggle"),
            (true, false, "composer_effort_slider"),
            (false, true, "composer_route:native:openai:gpt-6-astra"),
        ];
        for (advanced, model_list, expected_id) in cases {
            state.advanced_open = advanced;
            state.model_list_open = model_list;
            let frame = UiTree::new(build_composer(&state)).layout(viewport);
            let surface = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:composer_model_dismiss")
                .expect("selector surface")
                .rect;
            assert!(surface.x >= -0.1);
            assert!(surface.x + surface.width <= width + 0.1);
            assert!(surface.y >= -0.1);
            assert!(surface.y + surface.height <= viewport.height + 0.1);
            let trigger = frame
                .regions
                .iter()
                .find(|region| region.id == "composer_model_toggle")
                .expect("selector trigger")
                .rect;
            assert!(trigger.x >= -0.1 && trigger.x + trigger.width <= width + 0.1);
            let action = frame
                .regions
                .iter()
                .find(|region| region.id == expected_id)
                .unwrap_or_else(|| panic!("missing {expected_id} in compact selector"))
                .rect;
            assert!(action.x >= surface.x - 0.1);
            assert!(action.x + action.width <= surface.x + surface.width + 0.1);
        }
    }

    #[test]
    fn selector_main_view_emits_real_navigation_and_no_speed_action() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        // Inspect the settled card geometry; the opening animation scales
        // its corner radius from 0.96 on the first frame.
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(620.0, 520.0));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { color, radius, .. }
                if color.r == theme::MENU_SURFACE.r && color.g == theme::MENU_SURFACE.g
                    && color.b == theme::MENU_SURFACE.b && *radius == 6.0
        )));
        assert!(!frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Velocidad" || text == "No disponible"
        )));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.contains("speed")));
        let advanced = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_advanced_toggle")
            .expect("advanced action")
            .rect;
        let point = Point::new(
            advanced.x + advanced.width / 2.0,
            advanced.y + advanced.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "composer_advanced_toggle".into()
            }]
        );
    }

    #[test]
    fn main_selector_only_highlights_the_control_under_the_pointer() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        tree.set_reduced_motion(true);
        let viewport = Size::new(620.0, 520.0);
        let frame = tree.layout(viewport);
        let effort = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_advanced_toggle")
            .expect("effort row")
            .rect;
        let model = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_model_list_toggle")
            .expect("model row")
            .rect;
        assert_ne!(effort, model);
        let center = |rect: openide_ui::Rect| {
            Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
        };
        // Hit regions intersect the popup clip and can round the far edge
        // differently from the original painted rectangle.
        let same_rect = |left: openide_ui::Rect, right: openide_ui::Rect| {
            (left.x - right.x).abs() < 0.001
                && (left.y - right.y).abs() < 0.001
                && (left.width - right.width).abs() < 0.001
                && (left.height - right.height).abs() < 0.001
        };
        for (pointer, expected_id, highlighted, idle) in [
            (center(effort), "composer_advanced_toggle", effort, model),
            (center(model), "composer_model_list_toggle", model, effort),
        ] {
            tree.handle_event(UiEvent::PointerMove(pointer));
            assert_eq!(tree.hovered_id(), Some(expected_id));
            let frame = tree.layout(viewport);
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillRect { rect, color, .. }
                    if same_rect(*rect, highlighted) && *color == theme::INPUT
                )),
                "{expected_id}: expected {highlighted:?}; surfaces: {:?}",
                frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::FillRect { rect, color, .. } if *color == theme::INPUT =>
                            Some(*rect),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            );
            assert!(!frame.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color, .. }
                    if same_rect(*rect, idle) && *color == theme::INPUT
            )));
        }
    }

    #[test]
    fn model_menu_has_clear_sections_and_one_effort_route() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let routes = fixture_native_routes(&models);
        for width in [280.0, 360.0, 620.0] {
            let mut state = props("", width);
            state.model_routes = &routes;
            state.model_options = &models;
            state.effort_options = &efforts;
            state.model_menu_open = true;
            state.effort_label = "Alto";
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 620.0));
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|r| r.id == "composer_advanced_chip_toggle")
            );
            let model = frame
                .regions
                .iter()
                .find(|r| r.id == "composer_model_list_toggle")
                .unwrap()
                .rect;
            let effort = frame
                .regions
                .iter()
                .find(|r| r.id == "composer_advanced_toggle")
                .unwrap()
                .rect;
            assert_eq!(model.height, 40.0);
            assert_eq!(effort.height, 40.0);
            assert!(model.y + model.height < effort.y, "groups need a divider");
            assert_eq!(model.width, effort.width);
            assert!(frame.commands.iter().any(|c| matches!(c, DrawCommand::Icon { icon: Icon::Brain, rect, .. } if rect.y >= effort.y && rect.y + rect.height <= effort.y + effort.height)));
            assert!(frame.commands.iter().any(
                |c| matches!(c, DrawCommand::Text { text, .. } if text == "Para el próximo mensaje")
            ));
            assert!(frame.commands.iter().any(|c| matches!(c, DrawCommand::FillRect { color, radius, .. } if *color == theme::MENU_SURFACE && *radius == 6.0)));
        }
    }

    #[test]
    fn effort_drag_previews_nearest_real_tier_without_persisting() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("medium".into(), "Medio".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.effort_options = &efforts;
        state.selected_effort = "low";
        state.effort_label = "Bajo";
        state.model_menu_open = true;
        state.advanced_open = true;
        state.effort_preview = Some(1.0);
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 520.0));
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "Alto"
            )),
            "pill should preview Alto"
        );
        for expected in ["Esfuerzo", "Auto"] {
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == expected
                )),
                "drag should keep {expected} in the advanced header"
            );
        }
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_advanced_back"));
        assert_eq!(
            state.selected_effort, "low",
            "drag preview must not persist"
        );
    }

    #[test]
    fn selector_surface_and_labels_follow_dark_theme() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        let frame = {
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(620.0, 520.0))
        };
        let surface = frame
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:composer_model_dismiss")
            .expect("selector surface")
            .rect;
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. }
                if *rect == surface && *color == theme::MENU_SURFACE
        )));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. }
                if text == "Modelo" && *color == theme::FOREGROUND
        )));
        state.advanced_open = true;
        let advanced = {
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(620.0, 520.0))
        };
        assert!(advanced.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. }
                if text == "Esfuerzo" && *color == theme::FOREGROUND
        )));
        assert!(advanced.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, font_size, .. }
                if text == "Auto" && *font_size == SelectorSizing::COMPACT.label_size
        )));
        assert!(!advanced.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.starts_with("Automát")
        )));
        let surface = advanced
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:composer_model_dismiss")
            .expect("advanced surface")
            .rect;
        let auto = advanced
            .regions
            .iter()
            .find(|region| region.id == "composer_effort:default")
            .expect("auto action")
            .rect;
        assert!(auto.x + auto.width <= surface.x + surface.width + 0.1);
        let header_text_y = |label: &str| {
            advanced.commands.iter().find_map(|command| match command {
                DrawCommand::Text { text, position, .. }
                    if text == label
                        && position.y >= surface.y
                        && position.y < surface.y + 36.0 =>
                {
                    Some(position.y)
                }
                _ => None,
            })
        };
        let advanced_y = header_text_y("Esfuerzo").expect("advanced label");
        let level_y = header_text_y("Bajo").expect("effort level");
        let auto_y = header_text_y("Auto").expect("auto label");
        assert!(
            (advanced_y - level_y).abs() <= 1.0,
            "advanced={advanced_y}, level={level_y}, auto={auto_y}"
        );
        assert!(
            (advanced_y - auto_y).abs() <= 1.0,
            "advanced={advanced_y}, level={level_y}, auto={auto_y}"
        );
    }

    #[test]
    fn native_picker_excludes_external_cli_account_routes() {
        let routes = ["gpt-6-sol", "gpt-6-astra"].map(|model_id| ModelRouteOption {
            route_id: format!("codex:codex:{model_id}"),
            backend: "codex".into(),
            provider_id: "codex".into(),
            provider_name: "Codex CLI".into(),
            model_id: model_id.into(),
            model_name: model_id.into(),
            credential: CredentialAvailability::ExternalCliManaged,
            execution: ExecutionSupport::DelegatedCli,
            source: RouteSource::CliLiveCatalog,
            catalog_present: false,
            provider_listed: Some(true),
            effort_options: Vec::new(),
        });
        let mut state = props("", 620.0);
        state.model_routes = &routes;
        state.model_list_open = true;
        state.model_menu_open = true;
        let frame = UiTree::new(build_composer(&state)).layout(Size::new(620.0, 620.0));
        assert!(frame
            .regions
            .iter()
            .all(|region| region.id != "composer_model_provider_toggle:codex"));
        assert_eq!(
            frame
                .commands
                .iter()
                .filter(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text.contains("En tu cuenta")
                ))
                .count(),
            0
        );
        for route in &routes {
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == format!("composer_route:{}", route.route_id)));
        }
    }

    #[test]
    fn model_picker_preserves_exact_ids_and_distinguishes_unknown_from_unsupported() {
        let routes = [
            ("exact-reviewed-id", ExecutionSupport::Verified),
            (
                "exact-unknown-id",
                ExecutionSupport::ProviderListedUnverified,
            ),
            ("exact-unsupported-id", ExecutionSupport::Unsupported),
        ]
        .map(|(model_id, execution)| ModelRouteOption {
            route_id: format!("native:openai:{model_id}"),
            backend: "native".into(),
            provider_id: "openai".into(),
            provider_name: "OpenAI".into(),
            model_id: model_id.into(),
            model_name: "Nombre compartido".into(),
            credential: CredentialAvailability::EnvPresentUnverified,
            execution,
            source: RouteSource::ProviderDiscovery,
            catalog_present: true,
            provider_listed: Some(true),
            effort_options: Vec::new(),
        });
        let mut state = props("", 620.0);
        state.model_routes = &routes;
        state.selected_model = "exact-reviewed-id";
        state.model_menu_open = true;
        state.model_list_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(620.0, 620.0));
        for route in &routes {
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == &route.model_id
                )),
                "exact model ID stays visible: {}",
                route.model_id
            );
            assert_eq!(
                frame.regions.iter().any(|region| {
                    region.id == format!("composer_route:{}", route.route_id)
                        && region.kind == HitKind::Button
                }),
                route.selectable()
            );
        }
        // Only the standing that changes what the user can do is on the row:
        // a reviewed and an unverified model are chosen and run the same way.
        for (label, shown) in [("Revisado", false), ("Sin verificar", false), ("No compatible", true)] {
            assert_eq!(
                frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == label
                )),
                shown,
                "route state {label}"
            );
        }
        // A disabled model still exposes its explanation to pointer hover.
        // Its tooltip region must never become an action or a keyboard stop.
        let unsupported_action = format!("composer_route:{}", routes[2].route_id);
        let unsupported_description = format!("{unsupported_action}:description");
        let unsupported_region = frame
            .regions
            .iter()
            .find(|region| region.id == unsupported_description)
            .expect("unsupported model tooltip remains reachable");
        assert_eq!(unsupported_region.kind, HitKind::Hover);
        assert!(!tree.focus(&unsupported_action));
        assert!(!tree.focus(&unsupported_description));
        let unsupported_point = Point::new(
            unsupported_region.rect.x + unsupported_region.rect.width / 2.0,
            unsupported_region.rect.y + unsupported_region.rect.height / 2.0,
        );
        assert!(tree
            .handle_event(UiEvent::PointerDown(unsupported_point))
            .is_empty());
        assert!(tree
            .handle_event(UiEvent::PointerUp(unsupported_point))
            .is_empty());
        let unknown_action = format!("composer_route:{}", routes[1].route_id);
        assert!(tree.focus(&unknown_action));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: openide_ui::Key::Enter,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked { id: unknown_action }]
        );
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: openide_ui::Key::Escape,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "composer_model_dismiss".into()
            }]
        );
        let (contract, credential) = model_route_details(&routes[1]);
        let description = format!("{contract}\n{credential}");
        assert!(description.contains("herramientas desconocida"));
        assert!(description.contains("Credencial disponible"));
        assert!(!description.contains("Contrato del adaptador revisado"));
    }

    #[test]
    fn model_search_and_rows_clear_the_scrollbar_rail() {
        let models: Vec<(String, String)> = (0..12)
            .map(|index| (format!("model-{index}"), format!("Model {index}")))
            .collect();
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.model_menu_open = true;
        state.model_list_open = true;
        let mut tree = UiTree::new(build_composer(&state));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(620.0, 530.0));
        let scrollbar = frame
            .regions
            .iter()
            .find(|region| region.kind == HitKind::Scrollbar)
            .expect("model list scrollbar")
            .rect;
        let search = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_model_search")
            .expect("fixed model search")
            .rect;
        let results = frame
            .regions
            .iter()
            .find(|region| region.id == "composer_model_dismiss:options")
            .expect("results scroll viewport")
            .rect;
        assert!(search.y + search.height <= results.y);
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "composer_route:native:openai:model-0"));
        for row in frame.regions.iter().filter(|region| {
            region.id.starts_with("composer_route:")
                || region.id.starts_with("composer_model_provider_toggle:")
        }) {
            let content = row.rect;
            assert!(
                content.x + content.width <= scrollbar.x + 0.1,
                "{} overlaps the scrollbar",
                row.id
            );
        }
    }

    #[test]
    fn model_picker_keeps_search_and_header_fixed_while_scrolling_results() {
        let models: Vec<(String, String)> = (0..24)
            .map(|index| (format!("model-{index}"), format!("Model {index}")))
            .collect();
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.model_menu_open = true;
        state.model_list_open = true;
        let viewport = Size::new(620.0, 620.0);
        let mut tree = UiTree::new(build_composer(&state));
        tree.set_reduced_motion(true);
        let region = |frame: &openide_ui::Frame, id: &str| {
            frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
                .rect
        };
        let first = tree.layout(viewport);
        let fixed_ids = [
            "composer_model_list_toggle",
            "composer_routes_refresh",
            "composer_model_search",
        ];
        let fixed_rects = fixed_ids.map(|id| region(&first, id));
        let results = region(&first, "composer_model_dismiss:options");
        assert!(fixed_rects[2].y + fixed_rects[2].height <= results.y);
        assert!(first
            .regions
            .iter()
            .any(|region| region.id == "composer_route:native:openai:model-0"));
        assert!(!first.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Esc"
        )));
        assert!(tree.focus("composer_model_search"));
        tree.layout(viewport);
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(results.x + 12.0, results.y + results.height / 2.0),
            delta_y: -10_000.0,
        });
        let bottom = tree.layout(viewport);
        assert!(tree.scroll_offset("composer_model_dismiss:options") > 0.0);
        assert!(!bottom
            .regions
            .iter()
            .any(|region| region.id == "composer_route:native:openai:model-0"));
        assert!(bottom
            .regions
            .iter()
            .any(|region| region.id == "composer_route:native:openai:model-23"));
        for (id, original) in fixed_ids.into_iter().zip(fixed_rects) {
            let current = region(&bottom, id);
            assert_eq!(current, original, "{id} moved with the results");
            assert_eq!(
                bottom.hit_test(Point::new(
                    current.x + current.width / 2.0,
                    current.y + current.height / 2.0,
                )),
                Some(id),
            );
        }
        assert_eq!(tree.focused_id(), Some("composer_model_search"));
        assert_eq!(
            tree.handle_event(UiEvent::Text("model".into())),
            vec![UiAction::InputChanged {
                id: "composer_model_search".into(),
                value: "model".into(),
            }],
        );
        state.model_query = "model";
        tree.set_root(build_composer(&state));
        let filtered = tree.layout(viewport);
        assert_eq!(tree.focused_id(), Some("composer_model_search"));
        assert_eq!(tree.input_value("composer_model_search"), Some("model"));
        assert_eq!(region(&filtered, "composer_model_search"), fixed_rects[2]);
        let refresh = region(&filtered, "composer_routes_refresh");
        let refresh_point = Point::new(
            refresh.x + refresh.width / 2.0,
            refresh.y + refresh.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(refresh_point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(refresh_point)),
            vec![UiAction::Clicked {
                id: "composer_routes_refresh".into(),
            }],
        );
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: openide_ui::Key::Escape,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "composer_model_dismiss".into(),
            }],
        );
    }

    #[test]
    fn model_refresh_hover_matches_its_square_hit_target() {
        let models = [("model-0".into(), "Model 0".into())];
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.model_menu_open = true;
        state.model_list_open = true;
        let viewport = Size::new(620.0, 620.0);
        let mut tree = UiTree::new(build_composer(&state));
        tree.set_reduced_motion(true);
        let idle = tree.layout(viewport);
        let refresh = idle
            .regions
            .iter()
            .find(|region| region.id == "composer_routes_refresh")
            .expect("refresh action")
            .rect;
        let back = idle
            .regions
            .iter()
            .find(|region| region.id == "composer_model_list_toggle")
            .expect("back action")
            .rect;
        assert!((refresh.width - 28.0).abs() < 0.001);
        assert!((refresh.height - 28.0).abs() < 0.001);
        let center = Point::new(
            refresh.x + refresh.width / 2.0,
            refresh.y + refresh.height / 2.0,
        );
        assert_eq!(idle.hit_test(center), Some("composer_routes_refresh"));
        assert!(idle.commands.iter().any(|command| matches!(command,
            DrawCommand::Icon { rect, icon: Icon::Refresh, .. }
                if rect.width == 16.0 && rect.height == 16.0
                    && (rect.x + rect.width / 2.0 - center.x).abs() < 0.1
                    && (rect.y + rect.height / 2.0 - center.y).abs() < 0.1
        )));
        tree.handle_event(UiEvent::PointerMove(center));
        let hovered = tree.layout(viewport);
        assert_eq!(tree.hovered_id(), Some("composer_routes_refresh"));
        assert!(hovered.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. }
                if (rect.x - refresh.x).abs() < 0.001
                    && (rect.y - refresh.y).abs() < 0.001
                    && (rect.width - refresh.width).abs() < 0.001
                    && (rect.height - refresh.height).abs() < 0.001
                    && *color == theme::selector_palette().hover
        )));
        assert!(!hovered.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. }
                if (rect.x - back.x).abs() < 0.001
                    && (rect.y - back.y).abs() < 0.001
                    && (rect.width - back.width).abs() < 0.001
                    && (rect.height - back.height).abs() < 0.001
                    && *color == theme::selector_palette().hover
        )));
    }

    #[test]
    #[ignore = "export actual model picker paint commands for manual visual review"]
    fn export_model_picker_visual_review() {
        let output = std::path::PathBuf::from(format!(
            "{}/../../target/model-dropdown-visual-qa",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("inspection-notes.txt"), "Las capturas Effort sustituyen únicamente el shader animado por un bloque azul de sus dimensiones reales. Sirven para inspeccionar geometría, encabezado y texto; el efecto visual se valida en el renderizador nativo.\n").unwrap();
        let models: Vec<(String, String)> = (0..24)
            .map(|index| (format!("model-{index}"), format!("Model {index}")))
            .collect();
        let collapsed = BTreeSet::from(["openai".to_owned()]);
        for width in [280.0, 360.0, 620.0] {
            let mut state = props("", width);
            state.model_options = &models;
            state.selected_model = &models[0].0;
            state.model_label = &models[0].1;
            let routes = fixture_native_routes(&models);
            state.model_routes = &routes;
            state.model_menu_open = true;
            state.model_list_open = true;
            let viewport = Size::new(width, 620.0);
            let context = eframe::egui::Context::default();
            crate::fonts::install(&context);
            context.begin_pass(eframe::egui::RawInput::default());
            let fonts = context.clone();
            let mut tree = UiTree::new(build_composer(&state));
            tree.set_text_measurer(move |text, size, family| {
                fonts.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(
                            text.into(),
                            eframe::egui::FontId::new(
                                size,
                                match family {
                                    FontFamily::Sans => eframe::egui::FontFamily::Proportional,
                                    FontFamily::Monospace => eframe::egui::FontFamily::Monospace,
                                },
                            ),
                            eframe::egui::Color32::WHITE,
                        )
                        .size()
                        .x
                })
            });
            tree.set_reduced_motion(true);
            let export = |frame: &openide_ui::Frame, name: &str| {
                let path = output.join(format!("{name}-{width}.svg"));
                // SVG has no GPU effort shader. Preserve its true allocation
                // with an explicitly documented solid inspection placeholder.
                let mut inspection = frame.clone();
                for command in &mut inspection.commands {
                    if let DrawCommand::EffortSlider { rect, .. } = command {
                        *command = DrawCommand::FillRect {
                            rect: *rect,
                            color: theme::EFFORT_BLUE,
                            radius: 4.0,
                        };
                    }
                }
                crate::settings_ui::visual_qa::write_native_frame_svg(
                    &inspection,
                    &path,
                    width,
                    viewport.height,
                )
                .unwrap();
                let svg = std::fs::read(&path).unwrap();
                let tree = resvg::usvg::Tree::from_data(
                    &svg,
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixmap =
                    resvg::tiny_skia::Pixmap::new(width as u32, viewport.height as u32).unwrap();
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                pixmap.save_png(path.with_extension("png")).unwrap();
            };
            let first = tree.layout(viewport);
            export(&first, "Models");
            let results = first
                .regions
                .iter()
                .find(|region| region.id == "composer_model_dismiss:options")
                .expect("model result viewport")
                .rect;
            tree.handle_event(UiEvent::Scroll {
                position: Point::new(results.x + 12.0, results.y + results.height / 2.0),
                delta_y: -10_000.0,
            });
            let bottom = tree.layout(viewport);
            export(&bottom, "Models-bottom");
            let refresh = bottom
                .regions
                .iter()
                .find(|region| region.id == "composer_routes_refresh")
                .expect("refresh action")
                .rect;
            tree.handle_event(UiEvent::PointerMove(Point::new(
                refresh.x + refresh.width / 2.0,
                refresh.y + refresh.height / 2.0,
            )));
            export(&tree.layout(viewport), "Models-refresh-hover");
            tree.handle_event(UiEvent::PointerLeave);
            state.model_options = &models[..3];
            state.model_routes = &routes[..3];
            state.collapsed_model_providers = Some(&collapsed);
            tree.set_root(build_composer(&state));
            export(&tree.layout(viewport), "Provider-collapsed");
            state.model_list_open = false;
            let efforts = [
                ("default".into(), "Automático".into()),
                ("low".into(), "Bajo".into()),
                ("high".into(), "Alto".into()),
            ];
            state.effort_options = &efforts;
            state.effort_label = "Alto";
            state.selected_effort = "high";
            tree.set_root(build_composer(&state));
            export(&tree.layout(viewport), "Main");
            state.advanced_open = true;
            tree.set_root(build_composer(&state));
            export(&tree.layout(viewport), "Effort");
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn provider_collapse_reuses_submenu_fade_and_height_motion() {
        use std::time::{Duration, Instant};

        let models: Vec<(String, String)> = (0..3)
            .map(|index| (format!("model-{index}"), format!("Model {index}")))
            .collect();
        let collapsed = BTreeSet::from(["openai".to_owned()]);
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.model_menu_open = true;
        state.model_list_open = true;
        let viewport = Size::new(620.0, 620.0);
        let start = Instant::now();
        let mut tree = UiTree::new(build_composer(&state));
        tree.layout_at(viewport, start);
        let expanded = tree.layout_at(viewport, start + Duration::from_millis(300));
        let shell_height = |frame: &openide_ui::Frame| {
            frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::FillRect {
                        rect,
                        color,
                        radius,
                    } if *color == theme::MENU_SURFACE && *radius == 6.0 => Some(rect.height),
                    _ => None,
                })
                .expect("opaque model menu shell")
        };
        let expanded_height = shell_height(&expanded);
        let provider = expanded
            .regions
            .iter()
            .find(|region| region.id == "composer_model_provider_toggle:openai")
            .expect("provider toggle")
            .rect;
        let center = Point::new(
            provider.x + provider.width / 2.0,
            provider.y + provider.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(center));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(center)),
            vec![UiAction::Clicked {
                id: "composer_model_provider_toggle:openai".into(),
            }],
        );
        tree.handle_event(UiEvent::PointerLeave);
        state.collapsed_model_providers = Some(&collapsed);
        assert_ne!(model_menu_view_key(&state), "models");
        tree.set_root(build_composer(&state));
        let switch_at = start + Duration::from_millis(310);
        let switching = tree.layout_at(viewport, switch_at);
        let collapsed_height = switching
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:composer_model_dismiss")
            .expect("collapsed menu target")
            .rect
            .height;
        assert!(collapsed_height < expanded_height);
        assert!((shell_height(&switching) - expanded_height).abs() < 0.1);
        assert!(!switching
            .regions
            .iter()
            .any(|region| region.id.starts_with("composer_route:")));
        let middle = tree.layout_at(viewport, switch_at + Duration::from_millis(90));
        assert!(shell_height(&middle) > collapsed_height);
        assert!(shell_height(&middle) < expanded_height);
        assert!(middle.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. }
                if text == "Model 0" && color.a > 0 && color.a < 255
        )));
        let faded = tree.layout_at(viewport, switch_at + Duration::from_millis(180));
        assert!(!faded.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Model 0"
        )));
        let finished = tree.layout_at(viewport, switch_at + Duration::from_millis(260));
        assert!((shell_height(&finished) - collapsed_height).abs() < 0.1);

        let collapsed_key = model_menu_view_key(&state);
        state.search_collapsed_model_providers = Some(&collapsed);
        for query in ["m", "model", "model-0"] {
            state.model_query = query;
            assert_eq!(model_menu_view_key(&state), collapsed_key);
        }
        tree.set_reduced_motion(true);
        state.model_query = "";
        state.collapsed_model_providers = None;
        tree.set_root(build_composer(&state));
        let reduced = tree.layout_at(viewport, switch_at + Duration::from_millis(270));
        assert!((shell_height(&reduced) - expanded_height).abs() < 0.1);
        assert!(reduced.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. } if text == "Model 0" && color.a == 255
        )));
    }

    #[test]
    fn advanced_header_stays_stable_and_only_real_ultra_is_violet() {
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
            ("ultra".into(), "Ultra".into()),
        ];
        let mut state = props("", 620.0);
        state.effort_options = &efforts;
        state.selected_effort = "high";
        state.model_menu_open = true;
        state.advanced_open = true;

        let settled_frame = |state: &ComposerProps<'_>| {
            let mut tree = UiTree::new(build_composer(state));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(620.0, 520.0))
        };
        let idle = settled_frame(&state);
        assert!(idle
            .regions
            .iter()
            .any(|region| region.id == "composer_advanced_back"));
        assert!(!idle.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. }
                if text == "Ultra" && *color == theme::EFFORT_ULTRA
        )));

        assert!(idle.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Alto"
        )));
        assert!(!idle.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "Más rápido" || text == "Más capaz"
        )));

        state.effort_preview = Some(1.0);
        let ultra = settled_frame(&state);
        assert!(ultra.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, color, .. }
                if text == "Ultra" && *color == theme::EFFORT_ULTRA
        )));
        assert!(!ultra.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.contains("usage limits")
        )));
    }

    #[test]
    fn selector_popover_uses_reference_motion_and_stable_view_keys() {
        let models = [("gpt-6-sol".into(), "GPT-6 Sol".into())];
        let efforts = [
            ("default".into(), "Automático".into()),
            ("low".into(), "Bajo".into()),
            ("high".into(), "Alto".into()),
        ];
        let mut state = props("", 620.0);
        state.model_options = &models;
        let routes = fixture_native_routes(&models);
        state.model_routes = &routes;
        state.effort_options = &efforts;
        state.model_menu_open = true;
        for (models, advanced, expected_key) in [
            (false, false, "main"),
            (true, false, "models"),
            (false, true, "advanced"),
        ] {
            state.model_list_open = models;
            state.advanced_open = advanced;
            let selector = model_effort_group(&state, false).expect("selector");
            match selector.kind {
                ElementKind::Popover {
                    animation_ms,
                    close_animation_ms,
                    open_scale,
                    view_key,
                    ..
                } => {
                    assert_eq!(animation_ms, 220);
                    assert_eq!(close_animation_ms, Some(150));
                    assert!((open_scale - 0.96).abs() < f32::EPSILON);
                    assert_eq!(view_key.as_deref(), Some(expected_key));
                }
                _ => panic!("model selector must render as a popover"),
            }
        }
    }

    #[test]
    fn selector_shows_only_available_connections_and_exact_models() {
        for (provider_id, provider_name) in [
            ("openai", "OpenAI"),
            ("tenant.provider", "Proveedor propio"),
        ] {
            for width in [420.0, 620.0] {
                let base_routes =
                    ["generation/v1", "model.v2:exact"].map(|model_id| ModelRouteOption {
                        route_id: format!("native:{provider_id}:{model_id}"),
                        backend: "native".into(),
                        provider_id: provider_id.into(),
                        provider_name: provider_name.into(),
                        model_id: model_id.into(),
                        model_name: model_id.into(),
                        credential: CredentialAvailability::EnvPresentUnverified,
                        execution: ExecutionSupport::Verified,
                        source: RouteSource::AdapterReview,
                        catalog_present: false,
                        provider_listed: None,
                        effort_options: Vec::new(),
                    });
                let action_id = format!("composer_route:{}", base_routes[0].route_id);
                let mut available_model_point = None;
                for credential in [
                    CredentialAvailability::EnvPresentUnverified,
                    CredentialAvailability::NoKeyRequired,
                ] {
                    let mut routes = base_routes.clone();
                    for route in &mut routes {
                        route.credential = credential;
                    }
                    let mut state = props("", width);
                    state.model_routes = &routes;
                    state.model_menu_open = true;
                    state.model_list_open = true;
                    let mut tree = UiTree::new(build_composer(&state));
                    tree.set_reduced_motion(true);
                    let frame = tree.layout(Size::new(width, 650.0));
                    assert_eq!(
                        frame
                            .regions
                            .iter()
                            .filter(|region| region.id == "composer_connect_provider"
                                && region.kind == HitKind::Button)
                            .count(),
                        1
                    );
                    let model_regions: Vec<_> = frame
                        .regions
                        .iter()
                        .filter(|region| region.id.starts_with("composer_route:"))
                        .collect();
                    assert_eq!(model_regions.len(), 2);
                    for route in &routes {
                        assert!(frame.commands.iter().any(|command| matches!(command,
                            DrawCommand::Text { text, .. } if text == &route.model_id
                        )));
                    }
                    let region = model_regions
                        .iter()
                        .find(|region| region.id == action_id)
                        .unwrap();
                    assert_eq!(region.kind, HitKind::Button);
                    let point = Point::new(
                        region.rect.x + region.rect.width / 2.0,
                        region.rect.y + region.rect.height / 2.0,
                    );
                    available_model_point = Some(point);
                    tree.handle_event(UiEvent::PointerDown(point));
                    assert_eq!(
                        tree.handle_event(UiEvent::PointerUp(point)),
                        vec![UiAction::Clicked {
                            id: action_id.clone()
                        }]
                    );
                }
                let point = available_model_point.unwrap();
                for (credential, status) in [
                    (CredentialAvailability::MissingCredential, None),
                    (
                        CredentialAvailability::MissingCredential,
                        Some(ProviderModelStatus::Checking),
                    ),
                    (
                        CredentialAvailability::MissingCredential,
                        Some(ProviderModelStatus::CheckFailed),
                    ),
                    (
                        CredentialAvailability::EnvPresentUnverified,
                        Some(ProviderModelStatus::Checking),
                    ),
                    (
                        CredentialAvailability::EnvPresentUnverified,
                        Some(ProviderModelStatus::CheckFailed),
                    ),
                    (
                        CredentialAvailability::NoKeyRequired,
                        Some(ProviderModelStatus::Checking),
                    ),
                    (
                        CredentialAvailability::NoKeyRequired,
                        Some(ProviderModelStatus::CheckFailed),
                    ),
                ] {
                    let mut routes = base_routes.clone();
                    for route in &mut routes {
                        route.credential = credential;
                    }
                    let provider_status =
                        status.map(|status| BTreeMap::from([(provider_id.to_owned(), status)]));
                    let mut state = props("", width);
                    state.model_routes = &routes;
                    state.provider_status = provider_status.as_ref();
                    state.model_menu_open = true;
                    state.model_list_open = true;
                    let mut tree = UiTree::new(build_composer(&state));
                    tree.set_reduced_motion(true);
                    let frame = tree.layout(Size::new(width, 650.0));
                    assert_eq!(
                        frame
                            .regions
                            .iter()
                            .filter(|region| region.id == "composer_connect_provider"
                                && region.kind == HitKind::Button)
                            .count(),
                        1
                    );
                    assert!(!frame.regions.iter().any(|region| {
                        region.id.starts_with("composer_route:")
                            || region.id == format!("composer_model_provider_toggle:{provider_id}")
                    }), "unavailable provider {provider_id} exposes an action: {credential:?}/{status:?}");
                    assert!(frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, .. } if text == "No hay modelos disponibles"
                    )));
                    assert!(!frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, .. } if text == provider_name
                            || text.starts_with("Revisar conexión")
                            || text.starts_with("Consultando ")
                            || routes.iter().any(|route| text == &route.model_id)
                    )));
                    assert!(!tree.focus(&action_id));
                    // Clicking the old model position after a connection becomes
                    // unavailable cannot select it or open provider settings.
                    tree.handle_event(UiEvent::PointerDown(point));
                    assert!(!tree.handle_event(UiEvent::PointerUp(point)).iter().any(|action| {
                        matches!(action, UiAction::Clicked { id } if id.starts_with("composer_route:"))
                    }));
                }
            }
        }
    }

    #[test]
    fn empty_native_picker_never_promotes_saved_model_or_effort_and_connects_in_models_view() {
        let models = [("gpt-4.1".into(), "gpt-4.1".into())];
        let efforts = [("high".into(), "Alto".into())];
        for width in [360.0, 900.0] {
            for (model_list, advanced) in [(false, false), (true, false), (false, true)] {
                let mut state = props("Borrador conservado", width);
                state.model_label = "gpt-4.1";
                state.selected_model = "gpt-4.1";
                state.model_options = &models;
                state.model_routes = &[];
                state.effort_options = &efforts;
                state.selected_effort = "high";
                state.effort_label = "Alto";
                state.model_menu_open = true;
                state.model_list_open = model_list;
                state.advanced_open = advanced;
                let mut tree = UiTree::new(build_composer(&state));
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, 620.0));
                assert!(
                    frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, .. } if text == "No hay modelos disponibles"
                    )),
                    "empty copy must be legible in the first and model-list views at {width}"
                );
                assert!(!frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text.contains("gpt-4.1") || text == "Alto"
                )));
                assert!(!frame.regions.iter().any(|region| region
                    .id
                    .starts_with("composer_route:")
                    || region.id == "composer_advanced_toggle"
                    || region.id == "composer_effort_slider"));
                assert!(!tree.focus("run_toggle"));
                assert!(
                    !frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::FillRect { color, .. } if *color == SEND_BLUE
                    )),
                    "a disabled idle send action must not look active"
                );
                let connects: Vec<_> = frame
                    .regions
                    .iter()
                    .filter(|region| {
                        region.id == "composer_connect_provider" && region.kind == HitKind::Button
                    })
                    .collect();
                if !model_list {
                    assert!(connects.is_empty());
                    assert!(frame
                        .regions
                        .iter()
                        .any(|region| region.id == "composer_model_list_toggle"));
                    continue;
                }
                assert_eq!(connects.len(), 1);
                let connect = connects[0].rect;
                let shell = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "popover_surface:composer_model_dismiss")
                    .unwrap()
                    .rect;
                assert!(
                    connect.x >= shell.x
                        && connect.x + connect.width <= shell.x + shell.width + 0.1
                );
                assert!(
                    connect.y >= shell.y
                        && connect.y + connect.height <= shell.y + shell.height + 0.1
                );
                let point = Point::new(
                    connect.x + connect.width / 2.0,
                    connect.y + connect.height / 2.0,
                );
                assert_eq!(frame.hit_test(point), Some("composer_connect_provider"));
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked {
                        id: "composer_connect_provider".into()
                    }]
                );
                state.running = true;
                state.model_menu_open = false;
                tree.set_root(build_composer(&state));
                let active = tree.layout(Size::new(width, 620.0));
                assert!(active
                    .regions
                    .iter()
                    .any(|region| region.id == "run_toggle" && region.kind == HitKind::Button));
                assert!(active.commands.iter().any(|command| matches!(
                    command,
                    DrawCommand::Icon {
                        icon: Icon::Stop,
                        ..
                    }
                )));
            }
        }
    }

    #[test]
    fn connected_native_picker_has_fixed_connect_footer_only_in_models_view_and_width() {
        let models: Vec<(String, String)> = (0..24)
            .map(|index| (format!("model-{index}"), format!("Model {index}")))
            .collect();
        let routes = fixture_native_routes(&models);
        let efforts = [
            ("default".into(), "Automático".into()),
            ("high".into(), "Alto".into()),
        ];
        for width in [360.0, 900.0] {
            for (list, advanced) in [(false, false), (true, false), (false, true)] {
                let mut state = props("", width);
                state.model_routes = &routes;
                state.selected_model = "model-0";
                state.model_label = "Model 0";
                state.effort_options = &efforts;
                state.model_menu_open = true;
                state.model_list_open = list;
                state.advanced_open = advanced;
                let viewport = Size::new(width, 620.0);
                let mut tree = UiTree::new(build_composer(&state));
                tree.set_reduced_motion(true);
                let first = tree.layout(viewport);
                if !list {
                    assert!(!first
                        .regions
                        .iter()
                        .any(|region| region.id == "composer_connect_provider"));
                    assert!(!first.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, .. } if text == "Conectar proveedor"
                    )));
                    continue;
                }
                let connect = first
                    .regions
                    .iter()
                    .find(|region| region.id == "composer_connect_provider")
                    .unwrap()
                    .rect;
                assert!(first.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, .. } if text == "Conectar proveedor"
                )));
                assert!(connect.x >= 0.0 && connect.x + connect.width <= width + 0.1);
                assert!(connect.y >= 0.0 && connect.y + connect.height <= viewport.height + 0.1);
                if list {
                    let results = first
                        .regions
                        .iter()
                        .find(|region| region.id == "composer_model_dismiss:options")
                        .unwrap()
                        .rect;
                    assert!(results.y + results.height <= connect.y);
                    tree.handle_event(UiEvent::Scroll {
                        position: Point::new(results.x + 12.0, results.y + results.height / 2.0),
                        delta_y: -10_000.0,
                    });
                    let bottom = tree.layout(viewport);
                    assert!(tree.scroll_offset("composer_model_dismiss:options") > 0.0);
                    assert_eq!(
                        bottom
                            .regions
                            .iter()
                            .find(|region| region.id == "composer_connect_provider")
                            .unwrap()
                            .rect,
                        connect
                    );
                    assert!(bottom
                        .regions
                        .iter()
                        .any(|region| region.id == "composer_route:native:openai:model-23"));
                }
                let point = Point::new(
                    connect.x + connect.width / 2.0,
                    connect.y + connect.height / 2.0,
                );
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked {
                        id: "composer_connect_provider".into()
                    }]
                );
            }
        }
    }
}
