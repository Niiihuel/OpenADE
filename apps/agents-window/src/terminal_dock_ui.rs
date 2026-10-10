//! Bottom-terminal tabs reuse the rounded resource strip from the right dock.
//! Session controls remain fixed while the complete tab list scrolls.
//! Session processes and launch profiles belong to the host, never this view.

use crate::{
    dock_tab_ui::HEADER_SIDE_INSET,
    theme,
    view::{DockTabView, ViewState},
};
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, DropdownMenu, DropdownMenuEntry,
    DropdownMenuItem, Element, FontFamily, Icon, IconButton, MenuItem, Popover, PopoverPlacement,
    Separator, TabItem, TabStrip, TooltipPlacement, TooltipSpec,
};

pub(crate) const HEADER_HEIGHT: f32 = crate::dock_tab_ui::HEADER_HEIGHT;
const TOOL_SIZE: f32 = crate::dock_tab_ui::TOOL_SIZE;
use crate::theme::ACTIVE_PANE_INDICATOR;

#[derive(Clone, Debug, Default)]
pub(crate) struct TerminalControlsView {
    pub sessions_open: bool,
    pub profiles_open: bool,
    pub profiles: Vec<TerminalProfileView>,
    pub rename: Option<TerminalRenameView>,
    /// Exactly two panes when split; empty for the single-session presentation.
    pub panes: Vec<TerminalPaneView>,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalPaneView {
    pub id: u64,
    pub snapshot: Option<crate::terminal::TerminalSnapshot>,
    pub error: Option<String>,
}

pub(crate) fn screen_id(id: u64, split: bool) -> String {
    if split {
        format!("terminal_screen:{id}")
    } else {
        "terminal_screen".into()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalProfileView {
    pub id: String,
    pub label: String,
    pub is_default: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalRenameView {
    pub id: u64,
    pub name: String,
    pub error: Option<String>,
}

fn header_tooltip_placement(state: &ViewState) -> TooltipPlacement {
    if state.terminal_maximized {
        TooltipPlacement::Below
    } else {
        TooltipPlacement::Above
    }
}

fn tab_label(state: &ViewState, tab: &DockTabView) -> String {
    if tab.active {
        if let Some(snapshot) = &state.terminal_snapshot {
            if tab.title == "Terminal" {
                return snapshot.shell.clone();
            }
            if let Some(number) = tab.title.strip_prefix("Terminal ") {
                return format!("{} {number}", snapshot.shell);
            }
        }
    }
    tab.title.clone()
}

fn menu_width(width: f32) -> f32 {
    (width - 16.0).clamp(160.0, 320.0)
}

fn session_menu_row(state: &ViewState, tab: &DockTabView) -> Element {
    let title = tab_label(state, tab);
    // The list action keeps its own ID: the retained tab below the popover and
    // this row must never share a keyboard target or an independent close hit.
    let row = MenuItem::new(
        format!("terminal_tab_menu:{}", tab.id),
        &title,
        theme::menu_palette(),
    )
    .icon(Icon::Terminal)
    .tooltip_with(
        TooltipSpec::new(&title)
            .description(format!(
                "Directorio inicial: {}",
                tab.full_path.as_deref().unwrap_or(&state.workspace)
            ))
            .placement(TooltipPlacement::Above),
    )
    .trailing_action(
        format!("terminal_session_close:{}", tab.id),
        Icon::Trash,
        TooltipSpec::new("Cerrar terminal")
            .description(format!("{title} · Termina esta sesión de terminal"))
            .placement(TooltipPlacement::Above),
    )
    .trailing_dispatch_as(format!("terminal_close:{}", tab.id));
    let row = if tab.active {
        row.trailing("Activa")
    } else {
        row
    };
    row.render().size(None, Some(26.0))
}

fn session_selector(state: &ViewState, panel_width: f32) -> Element {
    let trigger = IconButton::new(
        "terminal_sessions_toggle",
        Icon::Menu,
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Dense)
    .tooltip_with(
        TooltipSpec::new("Todas las terminales").placement(header_tooltip_placement(state)),
    )
    .render();
    if let Some(rename) = &state.terminal_controls.rename {
        let field = format!("terminal_rename_input:{}", rename.id);
        let mut rows = vec![
            Element::text("Renombrar sesión")
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .foreground(theme::FOREGROUND),
            Element::text_input(&field, &rename.name, "Nombre de la terminal")
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .padding(8.0)
                .background(theme::INPUT)
                .foreground(theme::FOREGROUND)
                .border(theme::BORDER, 1.0)
                .radius(6.0)
                .size(None, Some(36.0)),
        ];
        if let Some(error) = &rename.error {
            rows.push(
                Element::text(error)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(theme::DESTRUCTIVE),
            );
        }
        rows.push(
            Element::row(vec![
                Element::spacer().flex(1.0),
                Button::new(
                    "terminal_rename_dismiss",
                    "Cancelar",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render(),
                Button::new(
                    format!("terminal_rename_confirm:{}", rename.id),
                    "Guardar",
                    theme::component_palette(),
                )
                .size(ControlSize::Compact)
                .render(),
            ])
            .gap(6.0),
        );
        return Popover::new(
            trigger,
            Element::column(rows).gap(10.0).padding(12.0),
            true,
            "terminal_rename_dismiss",
            theme::menu_palette(),
        )
        .width(menu_width(panel_width))
        .placement(PopoverPlacement::TopEnd)
        .render()
        .popover_initial_focus(field)
        .popover_return_focus("terminal_sessions_toggle");
    }
    let mut entries = vec![DropdownMenuEntry::Heading(format!(
        "Sesiones · {}",
        state.terminal_tabs.len()
    ))];
    entries.extend(
        state
            .terminal_tabs
            .iter()
            .map(|tab| DropdownMenuEntry::custom(session_menu_row(state, tab))),
    );
    DropdownMenu::new(
        trigger,
        entries,
        state.terminal_controls.sessions_open,
        "terminal_sessions_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .width(menu_width(panel_width))
    .max_visible_items(8)
    .placement(PopoverPlacement::TopEnd)
    .render()
    .popover_return_focus("terminal_sessions_toggle")
}

fn profiles_menu(state: &ViewState, width: f32) -> Element {
    let mut entries = vec![DropdownMenuEntry::Heading(
        "Nueva terminal con perfil".into(),
    )];
    entries.extend(state.terminal_controls.profiles.iter().map(|profile| {
        let item = DropdownMenuItem::new(
            format!("terminal_new_profile:{}", profile.id),
            &profile.label,
        )
        .icon(Icon::Terminal);
        if profile.is_default {
            item.annotation("Predeterminado").into()
        } else {
            item.into()
        }
    }));
    entries.push(DropdownMenuEntry::Separator);
    entries.push(
        DropdownMenuItem::new(
            if state.terminal_controls.panes.len() == 2 {
                "terminal_unsplit"
            } else {
                "terminal_split"
            },
            if state.terminal_controls.panes.len() == 2 {
                "Quitar división"
            } else {
                "Dividir terminal"
            },
        )
        .icon(Icon::PanelRight)
        .into(),
    );
    entries.push(
        DropdownMenuItem::new("terminal_settings", "Configurar perfil predeterminado")
            .icon(Icon::Settings)
            .into(),
    );
    DropdownMenu::new(
        IconButton::new(
            "terminal_profiles_toggle",
            Icon::ChevronDown,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Perfiles de terminal").placement(header_tooltip_placement(state)),
        )
        .render(),
        entries,
        state.terminal_controls.profiles_open,
        "terminal_profiles_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .width(menu_width(width))
    .placement(PopoverPlacement::TopEnd)
    .render()
    .popover_return_focus("terminal_profiles_toggle")
}

fn actions_menu(state: &ViewState, width: f32) -> Element {
    let snapshot = state.terminal_snapshot.as_ref();
    let available = snapshot.is_some();
    let running = snapshot.is_some_and(|snapshot| snapshot.running);
    let active = state.terminal_tabs.iter().find(|tab| tab.active);
    let copy_label = if snapshot.is_some_and(|snapshot| {
        snapshot
            .selection
            .as_ref()
            .is_some_and(|text| !text.is_empty())
    }) {
        "Copiar selección"
    } else {
        "Copiar salida"
    };
    let enabled =
        |item: DropdownMenuItem, condition| if condition { item } else { item.disabled() };
    let mut entries: Vec<DropdownMenuEntry> = vec![enabled(
        DropdownMenuItem::new("terminal_copy", copy_label).shortcut("Ctrl+Shift+C"),
        available,
    )
    .into()];
    if let Some(active) = active {
        entries.push(
            enabled(
                DropdownMenuItem::new(format!("terminal_paste:{}", active.id), "Pegar")
                    .shortcut("Ctrl+Shift+V"),
                running,
            )
            .into(),
        );
    }
    entries.extend([
        enabled(
            DropdownMenuItem::new("terminal_clear", "Limpiar pantalla").shortcut("Ctrl+L"),
            running,
        )
        .into(),
        enabled(
            DropdownMenuItem::new("terminal_interrupt", "Interrumpir proceso")
                .icon(Icon::Stop)
                .shortcut("Ctrl+C"),
            running,
        )
        .into(),
    ]);
    if snapshot.is_some_and(|snapshot| snapshot.scrollback_position > 0) {
        entries.push(
            DropdownMenuItem::new("terminal_bottom", "Volver al prompt")
                .icon(Icon::ArrowDown)
                .into(),
        );
    }
    if !running {
        entries.push(
            DropdownMenuItem::new("terminal_restart", "Reiniciar terminal")
                .icon(Icon::Refresh)
                .into(),
        );
    }
    if let Some(active) = active {
        entries.push(
            DropdownMenuItem::new(
                format!("terminal_rename:{}", active.id),
                "Renombrar sesión…",
            )
            .icon(Icon::Edit)
            .into(),
        );
    }
    entries.push(DropdownMenuEntry::Separator);
    entries.push(
        DropdownMenuItem::new(
            "terminal_panel_maximize",
            if state.terminal_maximized {
                "Restaurar tamaño"
            } else {
                "Ampliar panel"
            },
        )
        .icon(if state.terminal_maximized {
            Icon::Collapse
        } else {
            Icon::Expand
        })
        .into(),
    );
    entries.push(
        DropdownMenuItem::new("terminal_settings", "Configurar terminal")
            .icon(Icon::Settings)
            .into(),
    );
    if let Some(active) = active {
        entries.push(
            DropdownMenuItem::new(format!("terminal_close:{}", active.id), "Terminar sesión")
                .icon(Icon::Trash)
                .destructive(theme::DESTRUCTIVE)
                .into(),
        );
    }
    DropdownMenu::new(
        IconButton::new(
            "terminal_menu_toggle",
            Icon::More,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Acciones de la terminal").placement(header_tooltip_placement(state)),
        )
        .render(),
        entries,
        state.terminal_menu_open,
        "terminal_menu_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .width(menu_width(width))
    .max_visible_items(10)
    .placement(PopoverPlacement::TopEnd)
    .render()
    .popover_return_focus("terminal_menu_toggle")
}

fn terminal_tab(state: &ViewState, tab: &DockTabView, width: f32) -> TabItem {
    let title = tab_label(state, tab);
    let visible_in_split = state
        .terminal_controls
        .panes
        .iter()
        .any(|pane| pane.id == tab.id);
    let help = format!(
        "{title}{}\nDirectorio inicial: {}",
        if visible_in_split {
            " · Visible en la división"
        } else {
            ""
        },
        tab.full_path.as_deref().unwrap_or(&state.workspace)
    );
    let close_id = format!("terminal_close:{}", tab.id);
    let close = IconButton::new(close_id, Icon::Close, theme::component_palette())
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new(format!("Terminar {title}"))
                .placement(header_tooltip_placement(state)),
        )
        .render();
    TabItem::new(
        format!("terminal_tab:{}", tab.id),
        format!("terminal_tab_extent:{}", tab.id),
        Icon::Terminal,
        title,
        width,
    )
    .tooltip(help)
    .tooltip_placement(header_tooltip_placement(state))
    .active(tab.active)
    .close(close)
}

pub(crate) fn header(state: &ViewState, width: f32) -> Element {
    header_with_sections(state, width, None)
}

fn session_tabs(state: &ViewState, available: f32) -> Element {
    let items = state
        .terminal_tabs
        .iter()
        .map(|tab| {
            let width = crate::dock_tab_ui::tab_width(
                &tab_label(state, tab),
                available,
                crate::dock_tab_ui::TAB_MAX_WIDTH,
            );
            terminal_tab(state, tab, width)
        })
        .collect();
    TabStrip::new("terminal_tabs", items, theme::tab_strip_palette()).render()
}

fn header_with_sections(
    state: &ViewState,
    width: f32,
    sections: Option<(Element, f32)>,
) -> Element {
    let wide = width >= 580.0;
    let mut actions = vec![Element::row(vec![
        IconButton::new("terminal_new", Icon::Add, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(
                TooltipSpec::new("Nueva terminal").placement(header_tooltip_placement(state)),
            )
            .render(),
        profiles_menu(state, width),
    ])
    .gap(0.0)
    .size(Some(TOOL_SIZE * 2.0), Some(TOOL_SIZE))];
    if wide {
        actions.push(
            IconButton::new(
                "terminal_split",
                Icon::PanelRight,
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .dispatch_as(if state.terminal_controls.panes.len() == 2 {
                "terminal_unsplit"
            } else {
                "terminal_split"
            })
            .tooltip_with(
                TooltipSpec::new(if state.terminal_controls.panes.len() == 2 {
                    "Quitar división · conservar ambas sesiones"
                } else {
                    "Dividir terminal · nueva sesión independiente"
                })
                .placement(header_tooltip_placement(state)),
            )
            .render(),
        );
    }
    actions.push(actions_menu(state, width));
    actions.push(session_selector(state, width));
    if wide {
        actions.push(
            IconButton::new(
                "terminal_panel_maximize_button",
                if state.terminal_maximized {
                    Icon::Collapse
                } else {
                    Icon::Expand
                },
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .dispatch_as("terminal_panel_maximize")
            .tooltip_with(
                TooltipSpec::new(if state.terminal_maximized {
                    "Restaurar tamaño del panel"
                } else {
                    "Ampliar panel"
                })
                .placement(header_tooltip_placement(state)),
            )
            .render(),
        );
    }
    actions.push(
        IconButton::new(
            "terminal_panel_hide",
            Icon::Close,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(
            TooltipSpec::new("Ocultar panel · las sesiones siguen activas")
                .placement(header_tooltip_placement(state)),
        )
        .render(),
    );
    let actions_width = TOOL_SIZE * 2.0 + (actions.len() - 1) as f32 * (TOOL_SIZE + 2.0);
    let available = (width - HEADER_SIDE_INSET * 2.0 - 8.0 - actions_width).max(0.0);
    let leading = if let Some((sections, sections_width)) = sections {
        let sections_width = sections_width.min(available);
        let mut leading = vec![sections.size(Some(sections_width), Some(HEADER_HEIGHT))];
        if !crate::ide_workspace::active(state) && available - sections_width - 9.0 >= 72.0 {
            // Keep Agents' direct session tabs and their close/drag/scroll
            // interactions beside the shared bottom-panel navigation.
            leading.push(Element::panel(
                Separator::vertical(theme::component_palette()).length(16.0).render(),
            ).center_content().size(Some(9.0), Some(HEADER_HEIGHT)));
            leading.push(session_tabs(state, (available - sections_width - 9.0).max(0.0)).flex(1.0));
        } else {
            leading.push(Element::spacer().flex(1.0));
        }
        Element::row(leading)
        .gap(0.0)
        .size(None, Some(HEADER_HEIGHT))
        .flex(1.0)
    } else {
        session_tabs(state, available)
    };
    let actions = Element::row(actions)
        .gap(2.0)
        .size(Some(actions_width), Some(TOOL_SIZE));
    let header = TabStrip::header(leading, actions, width, theme::tab_strip_palette()).render();
    Element::overlay(
        header,
        Element::column(vec![
            Separator::horizontal(theme::component_palette()).render(),
            Element::spacer().flex(1.0),
        ])
        .size(Some(width), Some(HEADER_HEIGHT)),
    )
    .clip_children()
    .size(Some(width), Some(HEADER_HEIGHT))
}

#[cfg(test)]
pub(crate) fn build(state: &ViewState, width: f32, height: f32) -> Element {
    clipped_panel(state, header(state, width), width, height)
}

fn clipped_panel(state: &ViewState, header: Element, width: f32, height: f32) -> Element {
    let viewport_height = height.max(0.0);
    let content_height = viewport_height.max(HEADER_HEIGHT);
    // Keep the header's normal geometry while its presentation shrinks below
    // 32px. The outer viewport alone owns allocation and clips paint and hits,
    // so the last closing frames never push their following sibling downward.
    // A CLI task's terminal is drawn once, in the task's own surface.
    let panes = if crate::view::cli_task(state) {
        openide_ui::Text::new(
            "La terminal de esta tarea está en el panel Agente.",
            theme::component_palette(),
        )
        .muted()
        .render()
        .centered()
            .size(Some(width), Some((content_height - HEADER_HEIGHT).max(0.0)))
    } else {
        build_panes(state, width, content_height - HEADER_HEIGHT)
    };
    Element::column(vec![Element::column(vec![header, panes])
    .size(Some(width), Some(content_height))])
    .background(theme::BACKGROUND)
    .size(Some(width), Some(viewport_height))
    .clip_children()
}

/// Shared section navigation and terminal controls stay in one compact header.
/// Agents additionally retains its scrollable session tabs in the leading slot.
pub(crate) fn build_ide(
    state: &ViewState,
    width: f32,
    height: f32,
    sections: Element,
    sections_width: f32,
) -> Element {
    clipped_panel(
        state,
        header_with_sections(state, width, Some((sections, sections_width))),
        width,
        height,
    )
}

pub(crate) fn build_panes(state: &ViewState, width: f32, height: f32) -> Element {
    let panes = &state.terminal_controls.panes;
    if panes.len() != 2 {
        return crate::terminal_ui::build_body(state, width, height);
    }
    let active = state
        .terminal_tabs
        .iter()
        .find(|tab| tab.active)
        .map(|tab| tab.id);
    let pane_width = ((width - 1.0) * 0.5).max(0.0);
    let pane = |index: usize| {
        let pane = &panes[index];
        let selected = active == Some(pane.id);
        let content = crate::terminal_ui::build_pane(
            if selected {
                state.terminal_snapshot.as_ref()
            } else {
                pane.snapshot.as_ref()
            },
            if selected {
                state.terminal_error.as_deref()
            } else {
                pane.error.as_deref()
            },
            pane.id,
            &screen_id(pane.id, true),
            pane_width,
            height,
        );
        if selected {
            // Paint inside the existing viewport padding. Passive overlay:
            // neither terminal geometry nor pointer/context-menu hits change.
            Element::overlay(
                content,
                Element::column(vec![
                    Element::spacer()
                        .background(ACTIVE_PANE_INDICATOR)
                        .size(None, Some(2.0)),
                    Element::spacer().flex(1.0),
                ])
                .size(Some(pane_width), Some(height)),
            )
            .clip_children()
            .size(Some(pane_width), Some(height))
        } else {
            content
        }
    };
    Element::row(vec![
        pane(0),
        Separator::vertical(theme::component_palette())
            .length(height)
            .render(),
        pane(1),
    ])
    .gap(0.0)
    .clip_children()
    .size(Some(width), Some(height))
}

#[cfg(test)]
#[path = "terminal_dock_ui_tests.rs"]
mod tests;

/// The hand-built session row that `MenuItem::trailing_action` replaced, kept
/// so the replacement stays provably identical. Delete with the final audit
/// (S18).
#[cfg(test)]
mod menu_parity_tests {
    use super::*;
    use crate::view::SideTab;
    use openide_ui::{ButtonSize, Frame, Size, Tooltip, UiTree};

    fn legacy_session_menu_row(state: &ViewState, tab: &DockTabView) -> Element {
        let palette = theme::menu_palette();
        let title = tab_label(state, tab);
        let select_id = format!("terminal_tab_menu:{}", tab.id);
        // The list action keeps its own ID: the retained tab below the popover and
        // this row must never share a keyboard target or an independent close hit.
        let select = MenuItem::new(&select_id, &title, palette).icon(Icon::Terminal);
        let select = if tab.active {
            select.trailing("Activa")
        } else {
            select
        };
        let select = Tooltip::new(
            select.render().size(None, Some(26.0)).flex(1.0),
            &select_id,
            &title,
            palette,
        )
        .description(format!(
            "Directorio inicial: {}",
            tab.full_path.as_deref().unwrap_or(&state.workspace)
        ))
        .placement(TooltipPlacement::Above)
        .render()
        .flex(1.0);
        let close_id = format!("terminal_session_close:{}", tab.id);
        let close = Tooltip::new(
            palette.interactive_surface(
                IconButton::new(&close_id, Icon::Trash, palette)
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::IconExtraSmall)
                    .render()
                    .dispatch_as(format!("terminal_close:{}", tab.id))
                    .menu_secondary_action(),
            ),
            &close_id,
            "Cerrar terminal",
            palette,
        )
        .description(format!("{title} · Termina esta sesión de terminal"))
        .placement(TooltipPlacement::Above)
        .render();
        Element::row(vec![
            select,
            close.centered_y().size(Some(TOOL_SIZE), Some(26.0)),
        ])
        .gap(2.0)
        .size(None, Some(26.0))
        .clip_children()
    }

    fn frame(rows: Vec<Element>, width: f32) -> Frame {
        let mut tree = UiTree::new(Element::column(vec![
            DropdownMenu::new(
                Element::button("trigger", "Abrir").size(Some(24.0), Some(24.0)),
                rows.into_iter().map(DropdownMenuEntry::custom).collect(),
                true,
                "terminal_sessions_dismiss",
                theme::menu_palette(),
            )
            .context_menu_style()
            .width(menu_width(width))
            .max_visible_items(8)
            .render(),
            Element::spacer().flex(1.0),
        ]));
        tree.set_reduced_motion(true);
        tree.layout(Size::new(width, 400.0))
    }

    #[test]
    fn the_session_rows_match_their_hand_built_bodies() {
        let state = crate::view::tests::empty_state();
        let tabs: Vec<_> = [("Terminal", true), ("Un nombre de sesión bastante largo para la fila", false)]
            .into_iter()
            .enumerate()
            .map(|(index, (title, active))| DockTabView {
                id: index as u64 + 1,
                title: title.into(),
                full_path: (!active).then(|| "/home/usuario/proyecto".into()),
                active,
                kind: SideTab::Terminal,
                closable: true,
            })
            .collect();
        for width in [200.0, 520.0] {
            let rows = |build: fn(&ViewState, &DockTabView) -> Element| {
                tabs.iter().map(|tab| build(&state, tab)).collect::<Vec<_>>()
            };
            assert!(
                frame(rows(session_menu_row), width) == frame(rows(legacy_session_menu_row), width),
                "{width}"
            );
        }
    }
}
