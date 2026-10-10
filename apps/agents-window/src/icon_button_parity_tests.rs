//! The private "ghost icon button + tooltip" helpers that limn's `IconButton`
//! replaced, kept so each replacement stays provably identical: same draw
//! commands idle, hovered, with the hint open and pressed, and the same
//! action on click. Delete with the final audit (S18).

use std::time::{Duration, Instant};

use crate::theme;
use openide_ui::{
    ActiveHover, ButtonSize, ButtonVariant, Color, Component, ComponentPalette, ControlSize,
    Element, Frame, Icon, IconButton, Point, Size, Tooltip, TooltipPlacement, TooltipSpec,
    UiAction, UiEvent, UiTree,
};

/// Every frame a pointer can produce over the control, and what a click reports.
fn states(control: Element, width: f32) -> (Vec<Frame>, Vec<UiAction>) {
    let size = Size::new(width, 240.0);
    // Room on every side, so a hint can open where it was asked to.
    let mut tree = UiTree::new(Element::column(vec![
        Element::spacer().size(None, Some(100.0)),
        Element::row(vec![
            Element::spacer().size(Some(120.0), None),
            control,
            Element::spacer().flex(1.0),
        ]),
        Element::spacer().flex(1.0),
    ]));
    tree.set_reduced_motion(true);
    let idle = tree.layout(size);
    let Some(target) = idle.regions.first().map(|region| region.rect) else {
        return (vec![idle], Vec::new());
    };
    let point = Point::new(
        target.x + target.width / 2.0,
        target.y + target.height / 2.0,
    );
    tree.handle_event(UiEvent::PointerMove(point));
    let hovered = tree.layout(size);
    let hinted = tree.layout_at(size, Instant::now() + Duration::from_secs(2));
    tree.handle_event(UiEvent::PointerDown(point));
    let pressed = tree.layout(size);
    let actions = tree.handle_event(UiEvent::PointerUp(point));
    (vec![idle, hovered, hinted, pressed], actions)
}

fn assert_same(name: &str, legacy: Element, current: Element) {
    for width in [360.0, 900.0] {
        let (legacy_frames, legacy_actions) = states(legacy.clone(), width);
        let (frames, actions) = states(current.clone(), width);
        assert_eq!(frames.len(), legacy_frames.len(), "{name} width={width}");
        for (state, (frame, legacy)) in ["idle", "hover", "hint", "pressed"]
            .iter()
            .zip(frames.iter().zip(&legacy_frames))
        {
            assert_eq!(frame, legacy, "{name} width={width} {state}");
        }
        assert_eq!(actions, legacy_actions, "{name} width={width} click");
    }
}

#[test]
fn the_comparison_sees_hover_fills_and_open_hints() {
    let tool = IconButton::new("tool", Icon::Add, component())
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(below("Nuevo"))
        .render();
    let (frames, actions) = states(tool, 360.0);
    assert_ne!(frames[0], frames[1], "hover paints a fill");
    assert_ne!(frames[1], frames[2], "the hint opens");
    assert_ne!(frames[1], frames[3], "press paints another fill");
    assert_eq!(actions, vec![UiAction::Clicked { id: "tool".into() }]);
}

fn component() -> ComponentPalette {
    theme::component_palette()
}

fn below(hint: &str) -> TooltipSpec {
    TooltipSpec::new(hint).placement(TooltipPlacement::Below)
}

#[allow(clippy::too_many_arguments)]
fn legacy_ide_tool(
    id: &str,
    action: &str,
    icon: Icon,
    hint: &str,
    shortcut: Option<&str>,
    active: bool,
    size: f32,
    surface: Color,
) -> Element {
    let palette = theme::action_palette(surface, theme::FOREGROUND);
    let icon = if active { icon.filled() } else { icon };
    let mut tooltip = Tooltip::new(
        palette.interactive_surface(
            IconButton::new(id, icon, palette)
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::IconSmall)
                .active(active)
                .render()
                .dispatch_as(action)
                .padding(((size - 16.0) / 2.0).max(0.0))
                .size(Some(size), Some(size)),
        ),
        id,
        hint,
        component(),
    )
    .placement(TooltipPlacement::Below);
    if let Some(shortcut) = shortcut.filter(|shortcut| !shortcut.is_empty()) {
        tooltip = tooltip.shortcut(shortcut);
    }
    tooltip.render()
}

#[test]
fn ide_tools_match_tool_on_surface_shortcut_idle_and_active() {
    for (size, control) in [(24.0, ControlSize::Dense), (32.0, ControlSize::Regular)] {
        for surface in [theme::WINDOW, theme::BACKGROUND] {
            for active in [false, true] {
                for shortcut in [None, Some(""), Some("Ctrl+B")] {
                    let icon = Icon::PanelLeft;
                    assert_same(
                        &format!("ide tool {size} active={active} {shortcut:?}"),
                        legacy_ide_tool(
                            "ide_tool",
                            "ide_action",
                            icon,
                            "Mostrar",
                            shortcut,
                            active,
                            size,
                            surface,
                        ),
                        IconButton::new(
                            "ide_tool",
                            if active { icon.filled() } else { icon },
                            theme::action_palette(surface, theme::FOREGROUND),
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(control)
                        .active(active)
                        .active_hover(ActiveHover::Palette)
                        .dispatch_as("ide_action")
                        .tooltip_with(below("Mostrar").shortcut(shortcut.unwrap_or_default()))
                        .render(),
                    );
                }
            }
        }
    }
    // The 20 px agent-tab close keeps its size override on the rendered button.
    assert_same(
        "agent tab close",
        legacy_ide_tool(
            "close",
            "close",
            Icon::Close,
            "Cerrar pestaña",
            None,
            false,
            20.0,
            theme::BACKGROUND,
        ),
        Tooltip::new(
            IconButton::new(
                "close",
                Icon::Close,
                theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .render()
            .padding(2.0)
            .size(Some(20.0), Some(20.0)),
            "close",
            "Cerrar pestaña",
            component(),
        )
        .placement(TooltipPlacement::Below)
        .render(),
    );
}

/// sidebar_chrome::tool, environment_git_ui::git_tool, files_panel_ui::tool,
/// file_activity_ui::icon_action_alias, browser_ui::action,
/// terminal_dock_ui::tool, editor_review_ui::arrow and the search popover's
/// icon closure differed only in these arguments.
#[allow(clippy::too_many_arguments)]
fn legacy_extra_small(
    id: &str,
    action: &str,
    palette: ComponentPalette,
    surface: Option<ComponentPalette>,
    hint: &str,
    shortcut: Option<&str>,
    placement: TooltipPlacement,
    active: bool,
    enabled: bool,
) -> Element {
    let mut button = IconButton::new(id, Icon::Refresh, palette)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::IconExtraSmall)
        .active(active)
        .enabled(enabled)
        .render()
        .dispatch_as(action);
    if let Some(surface) = surface {
        button = surface.interactive_surface(button);
    }
    let mut tooltip = Tooltip::new(button, id, hint, component()).placement(placement);
    if let Some(shortcut) = shortcut {
        tooltip = tooltip.shortcut(shortcut);
    }
    tooltip.render()
}

#[test]
fn dense_ghost_tools_match_every_extra_small_helper() {
    let sidebar = theme::sidebar_palette();
    let card = theme::action_palette(theme::CARD, theme::FOREGROUND);
    for enabled in [true, false] {
        for (name, palette, surface, button_palette) in [
            ("sidebar_chrome::tool, git_tool", sidebar, None, sidebar),
            (
                "file_activity, editor_review::arrow",
                component(),
                None,
                component(),
            ),
            (
                "browser, terminal_dock, preview",
                component(),
                Some(component()),
                component(),
            ),
            ("environment icon_control", component(), Some(card), card),
        ] {
            for (placement, shortcut) in [
                (TooltipPlacement::Below, None),
                (TooltipPlacement::Above, Some("F7")),
            ] {
                // environment_ui::icon_control dropped its hint when
                // disabled; see the ledger. Every other helper kept it.
                if !enabled && name.starts_with("environment") {
                    continue;
                }
                assert_same(
                    &format!("{name} enabled={enabled} {placement:?}"),
                    legacy_extra_small(
                        "tool",
                        "act",
                        palette,
                        surface,
                        "Actualizar",
                        shortcut,
                        placement,
                        false,
                        enabled,
                    ),
                    IconButton::new("tool", Icon::Refresh, button_palette)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Dense)
                        .enabled(enabled)
                        .dispatch_as("act")
                        .tooltip_with(
                            TooltipSpec::new("Actualizar")
                                .shortcut(shortcut.unwrap_or_default())
                                .placement(placement),
                        )
                        .render(),
                );
            }
        }
        // document_preview_ui::action_tool and the editor's find and menu
        // toggles: active, under the canvas palette's interactive surface.
        assert_same(
            &format!("active tool enabled={enabled}"),
            legacy_extra_small(
                "tool",
                "tool",
                component(),
                Some(component()),
                "Ajustar",
                None,
                TooltipPlacement::Below,
                true,
                enabled,
            )
            .size(Some(24.0), Some(24.0)),
            IconButton::new("tool", Icon::Refresh, component())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .active(true)
                .active_hover(ActiveHover::Palette)
                .enabled(enabled)
                .tooltip_with(below("Ajustar"))
                .render(),
        );
    }
}

fn legacy_debug_tool(
    id: &str,
    hint: &str,
    shortcut: Option<&str>,
    enabled: bool,
    size: ButtonSize,
    surface: Color,
) -> Element {
    let palette = theme::action_palette(surface, theme::FOREGROUND);
    let mut tooltip = Tooltip::new(
        palette.interactive_surface(
            IconButton::new(id, Icon::Play, palette)
                .variant(ButtonVariant::Ghost)
                .button_size(size)
                .enabled(enabled)
                .render(),
        ),
        id,
        hint,
        component(),
    )
    .placement(TooltipPlacement::Above);
    if let Some(shortcut) = shortcut {
        tooltip = tooltip.shortcut(shortcut);
    }
    tooltip.render()
}

#[test]
fn debug_tools_match_surface_tool() {
    for (legacy, size, surface) in [
        (ButtonSize::IconSmall, ControlSize::Regular, theme::PANEL),
        (
            ButtonSize::IconExtraSmall,
            ControlSize::Dense,
            theme::SIDEBAR,
        ),
        (ButtonSize::IconExtraSmall, ControlSize::Dense, theme::PANEL),
    ] {
        for enabled in [true, false] {
            for shortcut in [None, Some("Shift+F5")] {
                assert_same(
                    &format!("debug {size:?} enabled={enabled} {shortcut:?}"),
                    legacy_debug_tool("debug:op", "Detener", shortcut, enabled, legacy, surface),
                    IconButton::new(
                        "debug:op",
                        Icon::Play,
                        theme::action_palette(surface, theme::FOREGROUND),
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(size)
                    .enabled(enabled)
                    .tooltip_with(
                        TooltipSpec::new("Detener")
                            .shortcut(shortcut.unwrap_or_default())
                            .placement(TooltipPlacement::Above),
                    )
                    .render(),
                );
            }
        }
    }
}

#[test]
fn regular_ghost_tools_match_the_icon_small_helpers() {
    // terminal_ui::tool
    assert_same(
        "terminal_ui::tool",
        Tooltip::new(
            theme::interactive_surface(
                IconButton::new("clear", Icon::Trash, component())
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::IconSmall)
                    .render(),
            ),
            "clear",
            "Limpiar pantalla",
            component(),
        )
        .shortcut("Ctrl+L")
        .placement(TooltipPlacement::Below)
        .render(),
        IconButton::new("clear", Icon::Trash, component())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Regular)
            .tooltip_with(below("Limpiar pantalla").shortcut("Ctrl+L"))
            .render(),
    );
    // project_actions_ui::icon_action, enabled; disabled it had no hint.
    let card = theme::action_palette(theme::CARD, theme::FOREGROUND);
    assert_same(
        "project_actions_ui::icon_action",
        card.interactive_surface(
            IconButton::new("edit", Icon::Settings, component())
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::IconSmall)
                .tooltip("Editar")
                .render()
                .size(Some(32.0), Some(32.0)),
        ),
        IconButton::new("edit", Icon::Settings, card)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Regular)
            .tooltip_with(TooltipSpec::new("Editar").placement(TooltipPlacement::Above))
            .render(),
    );
    // provider_usage_ui: the account action and the compact refresh.
    assert_same(
        "provider usage action",
        Tooltip::new(
            theme::interactive_surface(
                IconButton::new("usage_action", Icon::Settings, theme::menu_palette())
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::IconSmall)
                    .render(),
            ),
            "usage_action",
            "Administrar",
            component(),
        )
        .description("Proveedor · cuenta")
        .render(),
        IconButton::new("usage_action", Icon::Settings, component())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Regular)
            .tooltip_with(
                TooltipSpec::new("Administrar")
                    .description("Proveedor · cuenta")
                    .placement(TooltipPlacement::Above),
            )
            .render(),
    );
    for loading in [false, true] {
        assert_same(
            &format!("provider usage refresh loading={loading}"),
            Tooltip::new(
                IconButton::new("usage_refresh", Icon::Refresh, theme::menu_palette())
                    .variant(ButtonVariant::Subtle)
                    .button_size(ButtonSize::IconSmall)
                    .loading(loading)
                    .render(),
                "usage_refresh",
                "Actualizar consumo",
                component(),
            )
            .placement(TooltipPlacement::Below)
            .render(),
            IconButton::new("usage_refresh", Icon::Refresh, theme::menu_palette())
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Regular)
                .loading(loading)
                .tooltip_with(below("Actualizar consumo").palette(component()))
                .render(),
        );
    }
}

fn legacy_icon_button(id: &str, icon: Icon, fill: Color, ink: Color) -> Element {
    IconButton::new(id, icon, theme::action_palette(fill, ink))
        .label("")
        .variant(if fill == theme::BRAND || fill == theme::DESTRUCTIVE {
            ButtonVariant::Solid
        } else {
            ButtonVariant::Subtle
        })
        .size(ControlSize::Regular)
        .active(fill == theme::SELECTED)
        .render()
        .font_size(theme::UI_FONT_SIZE)
        .padding(4.0)
        .min_size(0.0, 0.0)
}

fn legacy_hinted_icon_button(
    id: &str,
    icon: Icon,
    hint: &str,
    shortcut: Option<&str>,
    fill: Color,
    ink: Color,
    size: f32,
) -> Element {
    let mut tooltip = Tooltip::new(
        legacy_icon_button(id, icon, fill, ink)
            .padding(((size - 16.0) / 2.0).max(0.0))
            .size(Some(size), Some(size)),
        id,
        hint,
        component(),
    );
    if let Some(shortcut) = shortcut {
        tooltip = tooltip.shortcut(shortcut);
    }
    tooltip.render()
}

#[test]
fn view_icon_buttons_match_icon_button_and_its_hinted_forms() {
    // Every (fill, ink, size) the app passed, with the form that replaced it.
    for (fill, ink, size, control) in [
        (theme::SIDEBAR, theme::FOREGROUND, 24.0, ControlSize::Dense),
        (
            theme::SIDEBAR,
            theme::FOREGROUND_SUBTLE,
            24.0,
            ControlSize::Dense,
        ),
        (
            theme::SIDEBAR,
            theme::FOREGROUND_SUBTLE,
            28.0,
            ControlSize::Compact,
        ),
        (
            theme::BACKGROUND,
            theme::FOREGROUND,
            24.0,
            ControlSize::Dense,
        ),
        (
            theme::BACKGROUND,
            theme::FOREGROUND,
            28.0,
            ControlSize::Compact,
        ),
        (
            theme::BACKGROUND,
            theme::FOREGROUND_SUBTLE,
            24.0,
            ControlSize::Dense,
        ),
        (
            theme::BACKGROUND,
            theme::FOREGROUND_SUBTLE,
            28.0,
            ControlSize::Compact,
        ),
        (theme::PANEL, theme::FOREGROUND, 24.0, ControlSize::Dense),
    ] {
        for active in [false, true] {
            assert_same(
                &format!("subtle {fill:?} {ink:?} {size} active={active}"),
                legacy_hinted_icon_button(
                    "tool",
                    Icon::Filter,
                    "Filtrar",
                    None,
                    if active { theme::SELECTED } else { fill },
                    ink,
                    size,
                )
                .tooltip_placement(TooltipPlacement::Below),
                IconButton::new("tool", Icon::Filter, theme::action_palette(fill, ink))
                    .variant(ButtonVariant::Subtle)
                    .size(control)
                    .active(active)
                    .tooltip_with(below("Filtrar"))
                    .render(),
            );
        }
    }
    // A transparent fill is a Ghost button; the dock header's are 24 px.
    for ink in [theme::FOREGROUND, theme::FOREGROUND_SUBTLE] {
        for active in [false, true] {
            for shortcut in [None, Some("Ctrl+Tab")] {
                assert_same(
                    &format!("ghost {ink:?} active={active} {shortcut:?}"),
                    legacy_hinted_icon_button(
                        "tool",
                        Icon::Menu,
                        "Todas las pestañas",
                        shortcut,
                        if active {
                            theme::SELECTED
                        } else {
                            Color::TRANSPARENT
                        },
                        ink,
                        24.0,
                    )
                    .tooltip_placement(TooltipPlacement::Below),
                    IconButton::new(
                        "tool",
                        Icon::Menu,
                        theme::action_palette(Color::TRANSPARENT, ink),
                    )
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Dense)
                    .active(active)
                    .tooltip_with(
                        below("Todas las pestañas").shortcut(shortcut.unwrap_or_default()),
                    )
                    .render(),
                );
            }
        }
    }
    // An open menu's trigger had no hint: the bare icon_button.
    assert_same(
        "open menu trigger",
        legacy_icon_button("add", Icon::Add, theme::SELECTED, theme::FOREGROUND)
            .size(Some(24.0), Some(24.0)),
        IconButton::new(
            "add",
            Icon::Add,
            theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .active(true)
        .tooltip_with(below("Abrir pestaña").suppressed(true))
        .render(),
    );
}

#[test]
fn remaining_inline_icon_buttons_match_their_hand_built_forms() {
    // composer_context_ui::close and the notification row's dismiss.
    assert_same(
        "menu palette close",
        IconButton::new("dismiss", Icon::Close, theme::menu_palette())
            .button_size(ButtonSize::IconExtraSmall)
            .variant(ButtonVariant::Ghost)
            .tooltip("Cerrar contexto")
            .tooltip_shortcut("Esc")
            .tooltip_placement(TooltipPlacement::Below)
            .render(),
        IconButton::new("dismiss", Icon::Close, theme::menu_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .tooltip_with(below("Cerrar contexto").shortcut("Esc"))
            .render(),
    );
    // environment_ui::header_control, closed and open.
    for variant in [ButtonVariant::Ghost, ButtonVariant::Subtle] {
        assert_same(
            &format!("environment header {variant:?}"),
            theme::interactive_surface(
                IconButton::new("environment_open", Icon::Laptop, component())
                    .variant(variant)
                    .tooltip("Entorno del chat")
                    .tooltip_placement(TooltipPlacement::Below)
                    .button_size(ButtonSize::IconExtraSmall)
                    .render(),
            ),
            IconButton::new("environment_open", Icon::Laptop, component())
                .variant(variant)
                .size(ControlSize::Dense)
                .tooltip_with(below("Entorno del chat"))
                .render(),
        );
    }
    // marketplace_view's sidebar search, idle, busy and disabled.
    for (loading, enabled) in [(false, true), (true, false), (false, false)] {
        assert_same(
            &format!("marketplace search loading={loading} enabled={enabled}"),
            Tooltip::new(
                IconButton::new("ide_search", Icon::ArrowRight, component())
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::IconExtraSmall)
                    .loading(loading)
                    .enabled(enabled)
                    .render()
                    .dispatch_as("search"),
                "ide_search",
                "Buscar en skills.sh",
                component(),
            )
            .shortcut("Enter")
            .placement(TooltipPlacement::Below)
            .render(),
            IconButton::new("ide_search", Icon::ArrowRight, component())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .loading(loading)
                .enabled(enabled)
                .dispatch_as("search")
                .tooltip_with(below("Buscar en skills.sh").shortcut("Enter"))
                .render(),
        );
    }
    // marketplace_view's uninstall and remove-source, idle and busy.
    let panel = theme::action_palette(theme::PANEL, theme::FOREGROUND_SUBTLE);
    for enabled in [true, false] {
        assert_same(
            &format!("marketplace uninstall enabled={enabled}"),
            theme::interactive_surface(
                IconButton::new("marketplace_uninstall", Icon::Trash, panel)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .tooltip("Desinstalar skill")
                    .enabled(enabled)
                    .render(),
            ),
            IconButton::new("marketplace_uninstall", Icon::Trash, panel)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .enabled(enabled)
                .tooltip_with(
                    TooltipSpec::new("Desinstalar skill").placement(TooltipPlacement::Above),
                )
                .render(),
        );
    }
    // document_preview_ui's zoom-out: the old text "−" control with the icon
    // in its place, so only the glyph's shape changed.
    assert_same(
        "preview zoom out",
        Tooltip::new(
            theme::interactive_surface(Element::clickable(
                "zoom_out",
                Element::icon(Icon::Minus)
                    .foreground(theme::FOREGROUND_SUBTLE)
                    .center_content(),
            ))
            // A Ghost button always emits its fill, here fully transparent.
            .background(Color::TRANSPARENT)
            .size(Some(24.0), Some(24.0)),
            "zoom_out",
            "Alejar",
            component(),
        )
        .placement(TooltipPlacement::Below)
        .render(),
        IconButton::new(
            "zoom_out",
            Icon::Minus,
            theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND_SUBTLE),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .tooltip_with(below("Alejar").palette(component()))
        .render(),
    );
    // The sidebar's settings button and the account usage toggle.
    let quiet = theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE);
    assert_same(
        "sidebar settings",
        Tooltip::new(
            IconButton::new("toggle_settings", Icon::Settings, quiet)
                .variant(ButtonVariant::Subtle)
                .button_size(ButtonSize::IconExtraSmall)
                .render(),
            "toggle_settings",
            "Configuración",
            component(),
        )
        .placement(TooltipPlacement::Right)
        .render(),
        IconButton::new("toggle_settings", Icon::Settings, quiet)
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Dense)
            .tooltip_with(TooltipSpec::new("Configuración").placement(TooltipPlacement::Right))
            .render(),
    );
    let sidebar = theme::sidebar_palette();
    for open in [false, true] {
        assert_same(
            &format!("usage toggle open={open}"),
            sidebar.interactive_surface(
                IconButton::new("usage_toggle", Icon::Settings, sidebar)
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::IconExtraSmall)
                    .active(open)
                    .tooltip("Cuenta y uso")
                    .tooltip_placement(TooltipPlacement::Right)
                    .render(),
            ),
            IconButton::new("usage_toggle", Icon::Settings, sidebar)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .active(open)
                .active_hover(ActiveHover::Palette)
                .tooltip_with(TooltipSpec::new("Cuenta y uso").placement(TooltipPlacement::Right))
                .render(),
        );
    }
}
