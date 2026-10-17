//! Editor-first workspace assembled from the same surfaces as Agent mode.
//! Docks change placement; documents, terminals and the native harness remain
//! owned by the existing host and retain their identity when the mode changes.

use super::*;
use crate::ide_workspace::{
    IdeActivity, IdePanel, ACTIVITY_BAR_WIDTH, OUTER_INSET, SIDEBAR_GAP, STATUSBAR_HEIGHT,
    TITLEBAR_HEIGHT,
};
use crate::keymap::{KeymapProfile, ShortcutAction};
use openide_ui::{Component, Notice, Separator, TooltipPlacement};

#[path = "ide_menu_ui.rs"]
mod menus;
#[path = "workspace_mode_ui.rs"]
mod mode_switch;
#[path = "ide_new_file_ui.rs"]
mod new_file;

pub(super) fn new_file_modal(state: &ViewState, base: Element, width: f32) -> Element {
    new_file::modal(state, base, width)
}
use crate::ide_status_ui as status;

#[path = "ide_bottom_ui.rs"]
pub(crate) mod bottom;

const PANE_HEADER_HEIGHT: f32 = crate::dock_tab_ui::HEADER_HEIGHT;

/// The mode switch is shared by both workspace presentations.
pub(super) fn mode_button(state: &ViewState) -> Element {
    mode_button_sized(state, false)
}

fn mode_button_sized(state: &ViewState, compact: bool) -> Element {
    mode_switch::render(state, compact)
}

fn quick_open_button(profile: KeymapProfile, width: f32) -> Element {
    let icon = Element::column(vec![
        Element::spacer().size(None, Some(3.0)),
        Element::icon(Icon::Search)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(Some(14.0), Some(14.0)),
    ])
    .size(Some(14.0), Some(20.0));
    let mut content = vec![icon];
    if width >= 150.0 {
        content.push(
            Element::truncated_text("Buscar archivos…")
                .foreground(theme::FOREGROUND_SUBTLE)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .flex(1.0),
        );
    }
    if width >= 260.0 {
        content.push(Element::column(vec![
            Element::spacer().size(None, Some(1.0)),
            Kbd::new(
                profile.shortcut(ShortcutAction::QuickOpen),
                theme::component_palette(),
            )
            .max_width(84.0)
            .render(),
        ]));
    }
    // Keep the keycaps centered in the 24px field, with a separate
    // horizontal inset so they never crowd the rounded edge.
    let extra_inset = if width >= 150.0 { 6.0 } else { 2.0 };
    let content = Element::row(vec![
        Element::spacer().size(Some(extra_inset), None),
        Element::row(content).gap(8.0).flex(1.0),
        Element::spacer().size(Some(extra_inset), None),
    ])
    .clip_children();
    Tooltip::new(
        theme::interactive_surface(Element::clickable("ide_titlebar_quick_open", content))
            .dispatch_as("ide_quick_open")
            .padding(2.0)
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(7.0)
            .size(Some(width), Some(24.0)),
        "ide_titlebar_quick_open",
        "Abrir un archivo del proyecto",
        theme::component_palette(),
    )
    .shortcut(profile.shortcut(ShortcutAction::QuickOpen))
    .placement(TooltipPlacement::Below)
    .render()
}

fn titlebar(state: &ViewState, width: f32, body_height: f32) -> Element {
    let layout = crate::ide_workspace::nominal_layout(state, width + 2.0 * OUTER_INSET);
    let compact = width < 900.0;
    let compact_menu = width < 1200.0;
    let menu_bar = menus::menubar(state, compact_menu);
    let menu_width = menu_bar.width();
    let mode_width = if compact { 24.0 } else { 84.0 };
    let right_min = mode_width + 76.0 + 1.0 + 24.0;
    let left_min = menu_width;
    let available = (width - 22.0).max(0.0);
    let (left_width, search_width, right_width) = if width >= 700.0 {
        let side_min = left_min.max(right_min);
        let search = (available - side_min * 2.0).clamp(24.0, 430.0);
        let side = ((available - search) / 2.0).max(0.0);
        (side, search, side)
    } else {
        (
            left_min,
            (available - left_min - right_min).max(24.0),
            right_min,
        )
    };
    let search = quick_open_button(state.keymap_profile, search_width);
    let explorer_visible = layout.left > 0.0;
    let agent_visible = layout.right.is_some();
    let explorer_open = explorer_visible || state.ide.compact_panel == Some(IdePanel::Explorer);
    let agent_open = agent_visible || state.ide.compact_panel == Some(IdePanel::Agent);
    let titlebar_palette = theme::action_palette(theme::WINDOW, theme::FOREGROUND);
    let explorer_toggle = compact_panel(
        UiIconButton::new(
            "ide_explorer_toggle",
            if explorer_open {
                Icon::PanelLeft.filled()
            } else {
                Icon::PanelLeft
            },
            titlebar_palette,
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .active(explorer_open)
        .active_hover(ActiveHover::Palette)
        .tooltip_with(
            TooltipSpec::new(if explorer_open {
                "Ocultar barra lateral"
            } else {
                "Mostrar barra lateral"
            })
            .shortcut(state.keymap_profile.shortcut(ShortcutAction::ToggleSidebar))
            .placement(TooltipPlacement::Below),
        )
        .render(),
        state,
        IdePanel::Explorer,
        explorer_visible,
        width,
        body_height,
    );
    let agent_toggle = compact_panel(
        UiIconButton::new(
            "ide_agent_toggle",
            if agent_open {
                Icon::PanelRight.filled()
            } else {
                Icon::PanelRight
            },
            titlebar_palette,
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .active(agent_open)
        .active_hover(ActiveHover::Palette)
        .tooltip_with(
            TooltipSpec::new(if agent_open {
                "Ocultar agente"
            } else {
                "Mostrar agente"
            })
            .placement(TooltipPlacement::Below),
        )
        .render(),
        state,
        IdePanel::Agent,
        agent_visible,
        width,
        body_height,
    );
    let left =
        Element::row(vec![menu_bar.render()]).size(Some(left_width), Some(24.0));
    let bottom_open = state.show_terminal_panel || state.ide.bottom_open;
    let layout_controls = Element::row(vec![
        explorer_toggle,
        UiIconButton::new(
            "ide_terminal_toggle",
            if bottom_open {
                Icon::PanelBottom.filled()
            } else {
                Icon::PanelBottom
            },
            titlebar_palette,
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .active(bottom_open)
        .active_hover(ActiveHover::Palette)
        .dispatch_as("ide_bottom_toggle")
        .tooltip_with(
            TooltipSpec::new(if bottom_open {
                "Ocultar panel inferior"
            } else {
                "Mostrar panel inferior"
            })
            .shortcut(
                state
                    .keymap_profile
                    .shortcut(ShortcutAction::ToggleBottomPanel),
            )
            .placement(TooltipPlacement::Below),
        )
        .render(),
        agent_toggle,
    ])
    .gap(2.0)
    .size(Some(76.0), Some(24.0));
    // The spacer absorbs the mode label reveal, keeping the panel controls
    // against the right edge and leaving menus and quick-open stationary.
    let right = Element::row(vec![
        Element::spacer().flex(1.0),
        mode_button_sized(state, compact),
        Separator::vertical(theme::component_palette())
            .length(16.0)
            .centered_in(24.0)
            .render(),
        layout_controls,
    ])
    .gap(8.0)
    .size(Some(right_width), Some(24.0));
    let row = Element::row(vec![left, search, right])
        .gap(8.0)
        .padding(3.0)
        .size(Some(width), Some(TITLEBAR_HEIGHT - 1.0));
    Element::column(vec![
        row,
        Separator::horizontal(theme::component_palette()).render(),
    ])
    .size(Some(width), Some(TITLEBAR_HEIGHT))
}

fn activity_bar(state: &ViewState, width: f32, height: f32) -> Element {
    let palette = theme::action_palette(theme::WINDOW, theme::FOREGROUND);
    let mut items = Vec::new();
    for (activity, icon) in [
        (IdeActivity::Files, Icon::Folder),
        (IdeActivity::Search, Icon::Search),
        (IdeActivity::Git, Icon::GitBranch),
        (IdeActivity::Debug, crate::icons::BUG),
        (IdeActivity::Extensions, crate::icons::SHOP),
    ] {
        let id = format!("ide_activity:{}", activity.key());
        let shown = state.ide.activity == activity
            && (crate::ide_workspace::nominal_layout(state, width).left > 0.0
                || state.ide.compact_panel == Some(IdePanel::Explorer));
        let (hint, shortcut) = match activity {
            IdeActivity::Files => ("Archivos", ShortcutAction::Files),
            IdeActivity::Search => ("Buscar", ShortcutAction::Search),
            IdeActivity::Git => ("Control de versiones", ShortcutAction::Git),
            IdeActivity::Debug => ("Depuración", ShortcutAction::Debug),
            IdeActivity::Extensions => ("Complementos", ShortcutAction::Extensions),
        };
        items.push(
            UiIconButton::new(
                &id,
                match (shown, activity) {
                    (true, IdeActivity::Debug) => crate::icons::BUG_FILL,
                    (true, _) => icon.filled(),
                    (false, _) => icon,
                },
                palette,
            )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Regular)
                .active(shown)
                .active_hover(ActiveHover::Palette)
                .tooltip_with(
                    TooltipSpec::new(hint)
                        .shortcut(state.keymap_profile.shortcut(shortcut))
                        .placement(TooltipPlacement::Right),
                )
                .render(),
        );
    }
    items.push(Element::spacer().flex(1.0));
    items.push(
        UiIconButton::new("ide_settings", Icon::Settings, palette)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Regular)
            .dispatch_as("toggle_settings_header")
            .tooltip_with(
                TooltipSpec::new("Configuración")
                    .shortcut(state.keymap_profile.shortcut(ShortcutAction::Settings))
                    .placement(TooltipPlacement::Right),
            )
            .render(),
    );
    Element::column(items)
        .gap(4.0)
        .padding(4.0)
        .size(Some(ACTIVITY_BAR_WIDTH), Some(height))
}

fn explorer(state: &ViewState, width: f32, height: f32) -> Element {
    let content = match state.ide.activity {
        IdeActivity::Files => crate::files_panel_ui::build_ide(state, width),
        IdeActivity::Search => super::ide_search_ui::panel(state, width, height),
        IdeActivity::Git => super::ide_activity_ui::git_sidebar(state, width, height),
        IdeActivity::Debug => crate::debug_ui::build_sidebar(&state.debug, width, height),
        IdeActivity::Extensions => super::ide_activity_ui::extensions_sidebar(state, width, height),
    };
    content
        .background(theme::SIDEBAR)
        .size(Some(width), Some(height))
        .flex(0.0)
        .clip_children()
}

fn agent_home(state: &ViewState, width: f32, height: f32) -> Element {
    let content_width = (width - 32.0).max(0.0);
    let composer = crate::composer_ui::build_composer_with_height(
        &ComposerProps::from_state(
            state,
            content_width,
            false,
            None,
            "Pregúntale a openADE",
            None,
        ),
        Some((height - 52.0).max(0.0)),
    );
    let hint = Notice::new("Tu agente, junto al código", theme::component_palette())
        .icon(Icon::Chat)
        .description("Explorá el proyecto, pedí cambios o resolvé una tarea.")
        .stacked()
        .sizing(crate::ui_sizing::AGENT_HINT)
        .render();
    let mut contents = Vec::new();
    if height > 300.0 {
        contents.push(hint);
    }
    contents.push(Element::spacer().flex(1.0));
    contents.push(Element::panel(composer).padding(16.0));
    Element::column(contents)
        .size(Some(width), Some(height))
        .clip_children()
}

/// The mark of a task: its CLI, or a conversation with the native agent.
fn task_icon(backend: &str) -> Icon {
    backend
        .strip_prefix(crate::view::CLI_BACKEND)
        .map_or(Icon::Chat, crate::provider_icons::icon_for_agent_cli)
}

/// One folder tab per open task, as the docks draw theirs; a task that is
/// not saved yet is the last one. The tabs share `width` equally.
fn agent_tabs(state: &ViewState, width: f32) -> Element {
    let open: Vec<_> = state
        .ide
        .agent_tabs
        .iter()
        .filter_map(|id| state.sessions.iter().find(|session| &session.id == id))
        .collect();
    let draft = state.selected_session.is_none();
    let count = open.len() + usize::from(draft);
    let tab_width = (width / count.max(1) as f32).clamp(44.0, 168.0);
    let mut items: Vec<_> = open
        .into_iter()
        .map(|session| {
            let active = state.selected_session.as_deref() == Some(session.id.as_str());
            let icon = task_icon(&session.backend);
            openide_ui::TabItem::new(
                format!("session:{}", session.id),
                format!("ide_agent_tab_extent:{}", session.id),
                if active { icon.filled() } else { icon },
                &session.title,
                tab_width,
            )
            .tooltip_placement(TooltipPlacement::Below)
            .active(active)
            .close(
                UiIconButton::new(
                    format!("ide_agent_tab_close:{}", session.id),
                    Icon::Close,
                    theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND_SUBTLE),
                )
                .variant(ButtonVariant::Ghost)
                .sizing(crate::ui_sizing::TAB_CLOSE)
                .tooltip_with(
                    TooltipSpec::new("Cerrar pestaña").placement(TooltipPlacement::Below),
                )
                .render(),
            )
        })
        .collect();
    if draft {
        items.push(
            openide_ui::TabItem::new(
                "ide_agent_tab_draft",
                "ide_agent_tab_extent_draft",
                Icon::Chat.filled(),
                "Nueva tarea",
                tab_width,
            )
            .tooltip_placement(TooltipPlacement::Below)
            .active(true),
        );
    }
    openide_ui::TabStrip::new("ide_agent_tabs", items, theme::tab_strip_palette()).render()
}

/// Starts a task with the native agent or with an installed CLI.
fn agent_new_menu(state: &ViewState) -> Element {
    let trigger = UiIconButton::new(
        "ide_agent_new_toggle",
        Icon::Add,
        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Dense)
    .active(state.ide.agent_new_open)
    .active_hover(ActiveHover::Palette)
    .tooltip_with(TooltipSpec::new("Nueva tarea").placement(TooltipPlacement::Below))
    .render();
    let mut entries = vec![
        DropdownMenuEntry::Heading("Nueva tarea con".into()),
        DropdownMenuItem::new("new_session_header", "openADE")
            .icon(Icon::Chat)
            .into(),
    ];
    entries.extend(
        state
            .agent_clis
            .iter()
            .filter(|agent| agent.resolved.is_some())
            .map(|agent| {
                DropdownMenuItem::new(format!("cli_task:{}", agent.id), &agent.label)
                    .icon(crate::provider_icons::icon_for_agent_cli(&agent.id))
                    .into()
            }),
    );
    entries.extend(state.acp_agents.iter().map(|agent| {
        DropdownMenuItem::new(format!("acp_task:{}", agent.id), &agent.name)
            .icon(crate::provider_icons::icon_for_acp_agent(&agent.id, &agent.name))
            .into()
    }));
    DropdownMenu::new(
        trigger,
        entries,
        state.ide.agent_new_open,
        "ide_agent_new_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .placement(PopoverPlacement::BottomEnd)
    .width(200.0)
    .render()
}

/// Every task of the project, the same ones the agents window lists.
/// A task of the IDE's task list: the open one wears the solid icon, full
/// ink and the selected fill.
fn agent_row(id: String, title: &str, icon: Icon, selected: bool) -> Element {
    let ink = if selected {
        theme::FOREGROUND
    } else {
        theme::FOREGROUND_SUBTLE
    };
    let palette = openide_ui::ComponentPalette {
        surface: if selected {
            theme::SELECTED
        } else {
            Color::TRANSPARENT
        },
        ..theme::component_palette()
    };
    ListRow::new(id, title, palette)
        .icon(if selected { icon.filled() } else { icon })
        .icon_ink(ink)
        .title_ink(ink)
        .filled()
        .radius(openide_ui::RadiusRole::Surface)
        .sizing(crate::ui_sizing::AGENT_LIST)
        .render()
}

fn agent_list(state: &ViewState, width: f32, height: f32) -> Element {
    let row = agent_row;
    let query = state.ide.agent_list_query.trim().to_lowercase();
    let mut rows = vec![row(
        "ide_agent_list_new".into(),
        "Nueva tarea",
        Icon::Add,
        state.selected_session.is_none(),
    )
    .dispatch_as("new_session_header")];
    rows.extend(
        state
            .sessions
            .iter()
            .filter(|session| {
                !session.archived
                    && (query.is_empty() || session.title.to_lowercase().contains(&query))
            })
            .map(|session| {
                row(
                    format!("ide_agent_list:{}", session.id),
                    &session.title,
                    task_icon(&session.backend),
                    state.selected_session.as_deref() == Some(session.id.as_str()),
                )
                .dispatch_as(format!("session:{}", session.id))
            }),
    );
    let inset = crate::sidebar_chrome::INSET;
    let content_width = (width - inset * 2.0).max(0.0);
    // The title stands 4 further in than the header's own inset.
    let title = UiText::new("Tareas", theme::component_palette())
        .truncate()
        .render()
        .padding(4.0);
    let header = openide_ui::PanelHeader::new("", theme::component_palette())
        .leading(title)
        .trailing(vec![agent_list_toggle(true)])
        .width(width)
        .sizing(crate::ui_sizing::PANE_HEADER)
        .render();
    let search = Element::panel(
        openide_ui::SearchField::new(
            "ide_agent_list_query",
            &state.ide.agent_list_query,
            "Buscar tareas…",
            theme::component_palette(),
        )
        .size(ControlSize::Compact)
        .width(content_width)
        .render(),
    )
    .padding(inset)
    .size(Some(width), None);
    let list_height =
        (height - PANE_HEADER_HEIGHT - crate::sidebar_chrome::SEARCH_HEIGHT - inset * 2.0).max(0.0);
    Element::column(vec![
        header,
        search,
        openide_ui::ScrollArea::new(
            "ide_agent_list",
            Element::column(rows).gap(2.0).padding(inset),
        )
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .size(Some(width), Some(list_height)),
    ])
    .size(Some(width), Some(height))
    .background(theme::BACKGROUND)
    .clip_children()
}

fn agent_list_toggle(open: bool) -> Element {
    UiIconButton::new(
        "ide_agent_list_toggle",
        if open {
            Icon::PanelRight.filled()
        } else {
            Icon::PanelRight
        },
        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Dense)
    .active(open)
    .active_hover(ActiveHover::Palette)
    .tooltip_with(
        TooltipSpec::new(if open {
            "Ocultar la lista de tareas"
        } else {
            "Mostrar la lista de tareas"
        })
        .placement(TooltipPlacement::Below),
    )
    .render()
}

fn agent(state: &ViewState, width: f32, height: f32, list_shown: bool) -> Element {
    // While the list is shown its own header carries the toggle.
    let mut tools = vec![agent_new_menu(state)];
    if !list_shown {
        tools.push(agent_list_toggle(false));
    }
    let tools_width = crate::dock_tab_ui::TOOL_SIZE * tools.len() as f32;
    let tabs_width = (width - crate::dock_tab_ui::HEADER_SIDE_INSET * 2.0 - 8.0 - tools_width).max(0.0);
    let header = openide_ui::TabStrip::header(
        agent_tabs(state, tabs_width),
        Element::row(tools).size(Some(tools_width), Some(crate::dock_tab_ui::TOOL_SIZE)),
        width,
        theme::tab_strip_palette(),
    )
    .render();
    let body_height = (height - PANE_HEADER_HEIGHT).max(0.0);
    let empty = state.selected_session.is_none()
        && state.transcript.is_empty()
        && state.pending_permissions.is_empty()
        && state.pending_tool_verification.is_none()
        && state.pending_batch_discard.is_none();
    let content = if crate::view::cli_task(state) {
        crate::terminal_dock_ui::build_panes(state, width, body_height)
    } else if empty {
        agent_home(state, width, body_height)
    } else {
        // This shared builder budgets 64 px for its external session header.
        // Our tab-height header already owns its space in the shell.
        crate::session_surface::build_session_body_sized(
            state,
            width,
            body_height + 64.0,
            true,
            false,
        )
    };
    Element::column(vec![
        header,
        content.size(Some(width), Some(body_height)).flex(1.0),
    ])
    .size(Some(width), Some(height))
    .background(theme::BACKGROUND)
    .clip_children()
}

fn compact_panel(
    anchor: Element,
    state: &ViewState,
    panel: IdePanel,
    inline_visible: bool,
    width: f32,
    body_height: f32,
) -> Element {
    if inline_visible || state.ide.compact_panel != Some(panel) {
        return anchor;
    }
    let available = (width - 16.0).max(0.0);
    let height = (body_height - 8.0).max(0.0);
    let (content, placement, focus, return_focus) = match panel {
        IdePanel::Explorer => {
            let width = state.ide.explorer_width.max(240.0).min(available);
            (
                explorer(state, width, height),
                PopoverPlacement::BottomStart,
                match state.ide.activity {
                    IdeActivity::Files => "files_search",
                    IdeActivity::Search => "ide_search_query",
                    IdeActivity::Git => "review_refresh",
                    IdeActivity::Extensions => "ide_extensions_search",
                    IdeActivity::Debug => "ide_explorer_toggle",
                },
                "ide_explorer_toggle",
            )
        }
        IdePanel::Agent => {
            let width = state.ide.agent_width.max(320.0).min(available);
            (
                agent(state, width, height, false),
                PopoverPlacement::BottomEnd,
                "prompt",
                "ide_agent_toggle",
            )
        }
    };
    let popup = Element::popover(
        anchor,
        content
            .border(theme::BORDER, 1.0)
            .radius(WORKSPACE_RADIUS)
            .shadow(
                theme::menu_palette().shadow,
                openide_ui::Point::new(0.0, 6.0),
                18.0,
                0.0,
            ),
        "ide_panel_dismiss",
        placement,
    )
    .popover_return_focus(return_focus);
    if panel == IdePanel::Explorer && state.ide.activity == IdeActivity::Debug {
        popup
    } else {
        popup.popover_initial_focus(focus)
    }
}

fn statusbar(state: &ViewState, width: f32) -> Element {
    Element::column(vec![
        Separator::horizontal(theme::component_palette()).render(),
        status::footer(state, width),
    ])
    .size(Some(width), Some(STATUSBAR_HEIGHT))
}

pub(super) fn build(state: &ViewState, width: f32, height: f32) -> Element {
    let layout = dock_layout(state, width);
    let inner_width = (width - 2.0 * OUTER_INSET).max(0.0);
    let body_height = (height - 2.0 * OUTER_INSET - TITLEBAR_HEIGHT - STATUSBAR_HEIGHT).max(0.0);
    let terminal_height = terminal_panel_height(state, height).min(body_height);
    let editor_height = (body_height - terminal_height).max(0.0);
    let center = if state.ide.extensions_open {
        let sidebar_list_visible = state.ide.activity == IdeActivity::Extensions
            && crate::ide_workspace::nominal_layout(state, width).left > 0.0;
        super::ide_activity_ui::extensions_center(
            state,
            layout.center,
            editor_height,
            sidebar_list_visible,
        )
    } else {
        side_pane_sized(state, layout.center, editor_height)
    };
    let mut editor = vec![center
        .size(Some(layout.center), Some(editor_height))
        .flex(1.0)
        .clip_children()];
    if terminal_height > 0.0 {
        editor.push(bottom::build(state, layout.center, terminal_height));
    }
    let mut workspace = vec![Element::column(editor)
        .size(Some(layout.center), Some(body_height))
        .clip_children()];
    if let Some(right) = layout.right {
        let list = crate::ide_workspace::agent_list_width(state, width);
        let list_shown = crate::ide_workspace::agent_list_shown(state, width);
        let right = (right - if list > 0.0 { list + 1.0 } else { 0.0 }).max(0.0);
        workspace.push(Separator::vertical(theme::component_palette()).render());
        workspace.push(
            agent(
                state,
                state.ide.agent_content_width.unwrap_or(right).max(right),
                body_height,
                list_shown,
            )
            .clip_to(right, body_height),
        );
        if list > 0.0 {
            // The content keeps its settled width; the transition only reveals it.
            let settled = if list_shown {
                crate::ide_workspace::agent_list_room(state, width)
            } else {
                crate::ide_workspace::AGENT_LIST_WIDTH
            };
            workspace.push(Separator::vertical(theme::component_palette()).render());
            workspace
                .push(agent_list(state, settled.max(list), body_height).clip_to(list, body_height));
        }
    }
    let mut panes = vec![activity_bar(state, width, body_height)];
    if layout.left > 0.0 {
        panes.push(
            explorer(
                state,
                state
                    .ide
                    .explorer_content_width
                    .unwrap_or(layout.left)
                    .max(layout.left),
                body_height,
            )
            .clip_to(layout.left, body_height),
        );
        panes.push(Element::spacer().size(Some(SIDEBAR_GAP), None));
    }
    // As in Agents mode, only the working surfaces share the rounded island.
    // Project navigation and window chrome belong to the gray window surface.
    panes.push(workspace_frame(Element::row(workspace)).size(None, Some(body_height)));
    let surface = Element::column(vec![
        titlebar(state, inner_width, body_height),
        Element::row(panes)
            .size(Some(inner_width), Some(body_height))
            .flex(1.0),
        statusbar(state, inner_width),
    ])
    .size(
        Some(inner_width),
        Some((height - 2.0 * OUTER_INSET).max(0.0)),
    );
    Element::panel(surface)
        .padding(OUTER_INSET)
        .background(theme::WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ide_workspace::WindowMode;
    use openide_ui::{DrawCommand, Frame, Point, Rect, Size, UiAction, UiEvent, UiTree};

    fn state() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.ide.mode = WindowMode::Ide;
        state.selected_backend = "native".into();
        state.workspace = "/workspace/openADE".into();
        state.project_branch = Some("main".into());
        state.file_rows = vec![FileRowView {
            name: "README.md".into(),
            depth: 0,
            is_dir: false,
            expanded: false,
            can_load_more: false,
        }];
        state
    }

    fn rect(frame: &Frame, id: &str) -> Rect {
        frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap_or_else(|| panic!("missing control {id}"))
            .rect
    }

    fn click(tree: &mut UiTree, viewport: Size, id: &str) -> Vec<UiAction> {
        let bounds = rect(&tree.layout(viewport), id);
        let point = Point::new(
            bounds.x + bounds.width / 2.0,
            bounds.y + bounds.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        tree.handle_event(UiEvent::PointerUp(point))
    }

    #[test]
    fn animated_lateral_viewports_preserve_text_layout_and_clip_controls() {
        let state = state();
        let height = 720.0;
        let hint = "Explorá el proyecto, pedí cambios o resolvé una tarea.";
        let full_width = 380.0;
        let (context, mut tree) =
            crate::view::ide_visual_tests::native_tree(agent(&state, full_width, height, false));
        let frame = tree.layout(Size::new(full_width, height));
        let original = frame
            .text_layouts()
            .into_iter()
            .find(|block| block.source == hint)
            .unwrap();
        let runs = |block: &openide_ui::TextLayoutBlock| {
            block
                .runs
                .iter()
                .map(|run| (run.text.clone(), run.rect))
                .collect::<Vec<_>>()
        };
        for visible_width in [0.0, 80.0, 180.0, 220.0] {
            tree.set_root(Element::row(vec![
                agent(&state, full_width, height, false).clip_to(visible_width, height)
            ]));
            let frame = tree.layout(Size::new(full_width, height));
            let clipped = frame
                .text_layouts()
                .into_iter()
                .find(|block| block.source == hint);
            if visible_width > 0.0 {
                assert_eq!(
                    runs(&clipped.expect("partly visible agent retains its text")),
                    runs(&original)
                );
            }
            assert!(frame.regions.iter().all(|region| {
                region.rect.x >= 0.0 && region.rect.x + region.rect.width <= visible_width + 0.01
            }));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "ide_agent_new_toggle"));
            assert_eq!(frame.hit_test(Point::new(visible_width + 1.0, 16.0)), None);
        }
        for visible_width in [0.0, 80.0, 180.0, 220.0] {
            tree.set_root(Element::row(vec![
                explorer(&state, 240.0, height).clip_to(visible_width, height)
            ]));
            let frame = tree.layout(Size::new(240.0, height));
            assert!(frame.regions.iter().all(|region| {
                region.rect.x >= 0.0 && region.rect.x + region.rect.width <= visible_width + 0.01
            }));
            if visible_width >= 80.0 {
                let row = rect(&frame, "file_row:0");
                assert!(row.width <= visible_width);
            }
            assert_eq!(frame.hit_test(Point::new(visible_width + 1.0, 16.0)), None);
        }
        context.end_pass().textures_delta.clear();
    }

    #[test]
    fn titlebar_toggle_state_follows_requested_visibility_during_motion() {
        use std::time::{Duration, Instant};
        let mut state = state();
        let viewport = Size::new(1440.0, 240.0);
        for (open, presented) in [(false, 220.0), (false, 80.0), (true, 0.0), (true, 80.0)] {
            state.ide.explorer_open = open;
            state.ide.agent_open = open;
            state.left_presentation_width = Some(presented);
            state.right_presentation_width = Some(presented);
            for (id, noun) in [
                ("ide_explorer_toggle", "barra lateral"),
                ("ide_agent_toggle", "agente"),
            ] {
                let mut tree = UiTree::new(titlebar(&state, viewport.width, 720.0));
                tree.set_reduced_motion(true);
                let now = Instant::now();
                let frame = tree.layout_at(viewport, now);
                let anchor = rect(&frame, id);
                assert_eq!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillRect { rect, color, .. } if *rect == anchor && *color == theme::SELECTED
                )), open, "{id} at {presented}px");
                tree.handle_event_at(
                    UiEvent::PointerMove(Point::new(anchor.x + 12.0, anchor.y + 12.0)),
                    now,
                );
                let frame = tree.layout_at(viewport, now + Duration::from_millis(700));
                let expected = format!("{} {noun}", if open { "Ocultar" } else { "Mostrar" });
                assert!(
                    frame.commands.iter().any(|command| matches!(command,
                        DrawCommand::Text { text, .. } if text == &expected
                    )),
                    "{id} must describe the next action during reversal"
                );
            }
        }
    }

    #[test]
    fn ide_rail_and_titlebar_tooltips_open_toward_the_workspace() {
        use std::time::{Duration, Instant};

        let state = state();
        let viewport = Size::new(1440.0, 800.0);
        for (id, placement) in [
            ("ide_activity:files", TooltipPlacement::Right),
            ("ide_activity:search", TooltipPlacement::Right),
            ("ide_settings", TooltipPlacement::Right),
            ("ide_titlebar_quick_open", TooltipPlacement::Below),
            ("ide_explorer_toggle", TooltipPlacement::Below),
        ] {
            let mut tree = UiTree::new(build(&state, viewport.width, viewport.height));
            tree.set_reduced_motion(true);
            let started = Instant::now();
            let anchor = rect(&tree.layout_at(viewport, started), id);
            tree.handle_event_at(
                UiEvent::PointerMove(Point::new(
                    anchor.x + anchor.width / 2.0,
                    anchor.y + anchor.height / 2.0,
                )),
                started,
            );
            let frame = tree.layout_at(viewport, started + Duration::from_millis(700));
            let tooltip = frame
                .commands
                .iter()
                .rev()
                .find_map(|command| match command {
                    DrawCommand::Shadow { rect, .. } => Some(*rect),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing tooltip for {id}"));
            match placement {
                TooltipPlacement::Right => assert!(
                    tooltip.x >= anchor.x + anchor.width,
                    "{id}: {tooltip:?} must be to the right of {anchor:?}"
                ),
                TooltipPlacement::Below => assert!(
                    tooltip.y >= anchor.y + anchor.height,
                    "{id}: {tooltip:?} must be below {anchor:?}"
                ),
                _ => unreachable!(),
            }
            assert!(tooltip.intersection(anchor).is_none(), "{id}");
        }
    }

    #[test]
    fn titlebar_quick_open_contains_native_keycaps_and_centers_its_icon() {
        let state = state();
        let fields = [24.0, 80.0, 149.0, 150.0, 259.0, 260.0, 430.0]
            .into_iter()
            .map(|width| {
                (
                    width,
                    Element::column(vec![quick_open_button(KeymapProfile::default(), width)])
                        .size(Some(width), Some(TITLEBAR_HEIGHT)),
                )
            });
        let headers = [320.0, 520.0, 900.0, 1440.0, 2048.0]
            .into_iter()
            .map(|width| (width, titlebar(&state, width, 680.0)));
        for (width, root) in fields.chain(headers) {
            let viewport = Size::new(width, TITLEBAR_HEIGHT);
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
            let frame = tree.layout(viewport);
            let search = rect(&frame, "ide_titlebar_quick_open");
            assert_eq!(search.height, 24.0);
            assert!(search.y >= 0.0 && search.y + search.height <= TITLEBAR_HEIGHT);
            let icon = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Icon {
                        rect,
                        icon: Icon::Search,
                        ..
                    } if rect.x >= search.x && rect.x + rect.width <= search.x + search.width => {
                        Some(*rect)
                    }
                    _ => None,
                })
                .expect("quick-open search icon remains visible");
            assert_eq!(icon.width, 14.0);
            assert_eq!(icon.height, 14.0);
            assert!((icon.y + icon.height / 2.0 - search.y - search.height / 2.0).abs() < 0.01);
            let caps: Vec<_> = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::FillRect { rect, radius, .. }
                        if *radius == 3.0 && rect.height == 18.0 =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(caps.len(), if search.width >= 260.0 { 2 } else { 0 });
            for cap in &caps {
                assert!(cap.x >= search.x + 8.0);
                assert!(cap.x + cap.width <= search.x + search.width - 8.0 + 0.01);
                assert!((cap.y - search.y - 3.0).abs() < 0.01);
                assert!((search.y + search.height - cap.y - cap.height - 3.0).abs() < 0.01);
            }
            if let Some(last) = caps.last() {
                assert!((search.x + search.width - last.x - last.width - 8.0).abs() < 0.01);
            }
            assert!(
                click(&mut tree, viewport, "ide_titlebar_quick_open").contains(
                    &UiAction::Clicked {
                        id: "ide_quick_open".into()
                    }
                )
            );
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn ide_shell_controls_dispatch_existing_host_actions_at_responsive_widths() {
        let mut state = state();
        for (width, reveal) in [
            320.0, 520.0, 708.0, 800.0, 900.0, 908.0, 1024.0, 1200.0, 1208.0, 1440.0,
        ]
        .into_iter()
        .flat_map(|width| [0.0, 0.5, 1.0].map(|reveal| (width, reveal)))
        {
            state.ide.mode_reveal = reveal;
            let viewport = Size::new(width, 800.0);
            let mut tree = UiTree::new(build(&state, viewport.width, viewport.height));
            tree.set_reduced_motion(true);
            let frame = tree.layout(viewport);
            let layout = dock_layout(&state, width);
            for (id, action) in [
                ("window_mode_toggle", "window_mode_toggle"),
                ("ide_titlebar_quick_open", "ide_quick_open"),
                ("ide_explorer_toggle", "ide_explorer_toggle"),
                ("ide_terminal_toggle", "ide_bottom_toggle"),
                ("ide_agent_toggle", "ide_agent_toggle"),
                ("ide_activity:debug", "ide_activity:debug"),
                ("ide_settings", "toggle_settings_header"),
            ] {
                let bounds = rect(&frame, id);
                assert!(
                    bounds.x >= 0.0 && bounds.x + bounds.width <= width + 0.01,
                    "{id} at {width}: {bounds:?}"
                );
                assert!(
                    bounds.y >= 0.0 && bounds.y + bounds.height <= viewport.height + 0.01,
                    "{id} at {width}: {bounds:?}"
                );
                if matches!(
                    id,
                    "ide_explorer_toggle" | "ide_terminal_toggle" | "ide_agent_toggle"
                ) {
                    assert!(bounds.y + bounds.height <= OUTER_INSET + TITLEBAR_HEIGHT);
                }
                if id == "ide_settings" {
                    assert!(bounds.x < ACTIVITY_BAR_WIDTH);
                    assert!(bounds.y > viewport.height - OUTER_INSET - STATUSBAR_HEIGHT - 44.0);
                }
                assert!(
                    click(&mut tree, viewport, id)
                        .contains(&UiAction::Clicked { id: action.into() }),
                    "{id} at {width}"
                );
            }
            let search = rect(&frame, "ide_titlebar_quick_open");
            let mode = rect(&frame, "window_mode_toggle");
            let first_menu = rect(
                &frame,
                if width - 2.0 * OUTER_INSET < 1200.0 {
                    "ide_menu:all"
                } else {
                    "ide_menu:file"
                },
            );
            assert_eq!(first_menu.x, OUTER_INSET + 3.0);
            assert!(first_menu.x + first_menu.width <= search.x);
            let explorer = rect(&frame, "ide_explorer_toggle");
            let terminal = rect(&frame, "ide_terminal_toggle");
            let agent = rect(&frame, "ide_agent_toggle");
            assert!(search.x + search.width <= mode.x);
            assert_eq!(mode.x + mode.width + 8.0 + 1.0 + 8.0, explorer.x);
            assert_eq!(explorer.x + explorer.width + 2.0, terminal.x);
            assert_eq!(terminal.x + terminal.width + 2.0, agent.x);
            assert!((agent.x + agent.width - (width - OUTER_INSET - 3.0)).abs() < 0.01);
            if width >= 708.0 {
                assert!((search.x + search.width / 2.0 - width / 2.0).abs() < 0.01);
            }
            assert!(rect(&frame, "ide_settings").x < OUTER_INSET + ACTIVITY_BAR_WIDTH);
            if layout.left > 0.0 {
                let file = rect(&frame, "file_row:0");
                assert!(
                    file.x + file.width <= OUTER_INSET + ACTIVITY_BAR_WIDTH + layout.left + 0.01
                );
            }
            if let Some(agent_width) = layout.right {
                let prompt = rect(&frame, "prompt");
                assert!(prompt.x >= width - OUTER_INSET - agent_width);
                assert!(prompt.y > viewport.height * 0.5);
            }
        }
    }

    #[test]
    fn ide_activity_access_remains_visible_when_the_sidebar_is_hidden() {
        for width in [320.0, 520.0, 1440.0] {
            let mut state = state();
            state.ide.explorer_open = false;
            let viewport = Size::new(width, 680.0);
            let mut tree = UiTree::new(build(&state, viewport.width, viewport.height));
            tree.set_reduced_motion(true);
            let frame = tree.layout(viewport);
            for activity in [
                IdeActivity::Files,
                IdeActivity::Search,
                IdeActivity::Git,
                IdeActivity::Debug,
                IdeActivity::Extensions,
            ] {
                let id = format!("ide_activity:{}", activity.key());
                let bounds = rect(&frame, &id);
                assert!(
                    bounds.x >= OUTER_INSET
                        && bounds.x + bounds.width <= OUTER_INSET + ACTIVITY_BAR_WIDTH
                );
                assert!(bounds.y >= OUTER_INSET + TITLEBAR_HEIGHT);
                assert!(click(&mut tree, viewport, &id).contains(&UiAction::Clicked { id }));
            }
        }
    }

    #[test]
    fn ide_history_reuses_context_menu_and_session_navigation() {
        let mut state = state();
        state.ide.agent_list_open = true;
        state.sessions = vec![SessionView {
            id: "native-task".into(),
            title: "Revisar el editor".into(),
            project: state.workspace.clone(),
            backend: "native".into(),
            pinned: false,
            archived: false,
            unread: false,
            native_automation: None,
        }];
        let viewport = Size::new(1440.0, 800.0);
        let mut tree = UiTree::new(build(&state, viewport.width, viewport.height));
        tree.set_reduced_motion(true);
        let frame = tree.layout(viewport);
        assert_eq!(rect(&frame, "ide_agent_list:native-task").height, 26.0);
        assert!(
            click(&mut tree, viewport, "ide_agent_list:native-task").contains(&UiAction::Clicked {
                id: "session:native-task".into(),
            })
        );
    }

    #[test]
    fn ide_compact_drawers_keep_file_navigation_and_composer_reachable() {
        for (width, panel, control, action) in [
            (320.0, IdePanel::Explorer, "file_row:0", "file_row:0"),
            (
                520.0,
                IdePanel::Agent,
                "ide_agent_new_toggle",
                "ide_agent_new_toggle",
            ),
            (
                800.0,
                IdePanel::Agent,
                "ide_agent_new_toggle",
                "ide_agent_new_toggle",
            ),
        ] {
            let mut state = state();
            state.ide.compact_panel = Some(panel);
            let viewport = Size::new(width, 700.0);
            let mut tree = UiTree::new(build(&state, viewport.width, viewport.height));
            tree.set_reduced_motion(true);
            let frame = tree.layout(viewport);
            let bounds = rect(&frame, control);
            assert!(bounds.x >= 0.0 && bounds.x + bounds.width <= width + 0.01);
            assert!(
                bounds.y >= TITLEBAR_HEIGHT
                    && bounds.y + bounds.height < viewport.height - STATUSBAR_HEIGHT
            );
            assert!(click(&mut tree, viewport, control)
                .contains(&UiAction::Clicked { id: action.into() }));
            if panel == IdePanel::Agent {
                let prompt = rect(&frame, "prompt");
                assert!(prompt.x >= 0.0 && prompt.x + prompt.width <= width);
                assert!(frame.regions.iter().any(|region| region.id == "run_toggle"));
            }
        }
    }

    #[test]
    fn ide_chrome_preserves_standard_sixteen_pixel_icons() {
        let state = state();
        let viewport = Size::new(1440.0, 800.0);
        let frame = UiTree::new(build(&state, viewport.width, viewport.height)).layout(viewport);
        for (id, icon) in [
            ("window_mode_toggle", None),
            ("ide_terminal_toggle", Some(Icon::PanelBottom)),
            // Both side panels are open, so their toggles wear the solid form.
            ("ide_agent_toggle", Some(Icon::PanelRight.filled())),
            ("ide_explorer_toggle", Some(Icon::PanelLeft.filled())),
            ("ide_agent_new_toggle", Some(Icon::Add)),
        ] {
            let control = rect(&frame, id);
            let glyph = frame
                .commands
                .iter()
                .find_map(|command| {
                    let glyph = match command {
                        DrawCommand::Icon {
                            rect, icon: drawn, ..
                        } if Some(*drawn) == icon => Some(*rect),
                        DrawCommand::RasterImage { rect, image, .. }
                            if icon.is_none() && image.alt() == "openADE" =>
                        {
                            Some(*rect)
                        }
                        _ => None,
                    };
                    glyph.filter(|rect| {
                        rect.x >= control.x
                            && rect.x + rect.width <= control.x + control.width + 0.01
                            && rect.y >= control.y
                            && rect.y + rect.height <= control.y + control.height + 0.01
                    })
                })
                .unwrap_or_else(|| panic!("missing icon inside {id}"));
            assert_eq!(glyph.width, 16.0, "{id}: {glyph:?}");
            assert_eq!(glyph.height, 16.0, "{id}: {glyph:?}");
            assert!(
                (glyph.y + glyph.height / 2.0 - control.y - control.height / 2.0).abs() < 0.01,
                "{id}: {glyph:?}"
            );
            if id != "window_mode_toggle" {
                assert!(
                    (glyph.x + glyph.width / 2.0 - control.x - control.width / 2.0).abs() < 0.01,
                    "{id}: {glyph:?}"
                );
            }
        }
    }

    #[test]
    fn ide_agent_uses_project_chrome_without_duplicate_workspace_controls() {
        for selected in [false, true] {
            let mut state = state();
            state.project_branch = Some("feature/ide".into());
            if selected {
                state.selected_session = Some("task".into());
            }
            let size = Size::new(1440.0, 800.0);
            let frame = UiTree::new(build(&state, size.width, size.height)).layout(size);
            for absent in [
                "ide_agent_close",
                "composer_branch_toggle",
                "composer_project_toggle",
            ] {
                assert!(
                    !frame.regions.iter().any(|region| region.id == absent),
                    "duplicate {absent}"
                );
            }
            for present in ["ide_agent_toggle", "ide_git_review", "prompt", "run_toggle"] {
                assert!(
                    frame.regions.iter().any(|region| region.id == present),
                    "missing {present}"
                );
            }
        }
    }

    #[test]
    fn agent_tabs_are_the_folder_tabs_of_the_docks() {
        let mut state = state();
        state.sessions = ["a", "b"]
            .into_iter()
            .map(|id| SessionView {
                id: id.into(),
                title: format!("Tarea {id}"),
                project: state.workspace.clone(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
            })
            .collect();
        state.ide.agent_tabs = vec!["a".into(), "b".into()];
        let frame = UiTree::new(agent(&state, 420.0, 400.0, false)).layout(Size::new(420.0, 400.0));
        // No task is selected: the draft is a third tab, without a close.
        let tab = |id: &str| rect(&frame, id);
        let inset = crate::dock_tab_ui::HEADER_SIDE_INSET;
        let tool = crate::dock_tab_ui::TOOL_SIZE;
        // They share the room left by the two tools, on the bar's baseline.
        let width = (420.0 - inset * 2.0 - 8.0 - tool * 2.0) / 3.0;
        for (index, id) in ["session:a", "session:b", "ide_agent_tab_draft"]
            .into_iter()
            .enumerate()
        {
            let tab = tab(id);
            assert!((tab.x - (inset + width * index as f32)).abs() < 0.01, "{id}: {tab:?}");
            assert!((tab.width - width).abs() < 0.01, "{id}: {tab:?}");
            assert_eq!(tab.height, crate::dock_tab_ui::TAB_HEIGHT);
            assert_eq!(tab.y + tab.height, PANE_HEADER_HEIGHT);
        }
        let close = tab("ide_agent_tab_close:a");
        assert_eq!((close.width, close.height), (20.0, 20.0));
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.id.starts_with("ide_agent_tab_close:"))
                .count(),
            2
        );
        let toggle = tab("ide_agent_list_toggle");
        assert_eq!(toggle.x + toggle.width, 420.0 - inset);
    }
}
