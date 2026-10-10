//! The post-render overrides and `interactive_surface` wrappers that limn's
//! `Button::sizing`, `ButtonGroup::sizing` and a palette replaced in
//! c01-converge-controls, kept so each replacement is provably identical:
//! same draw commands idle, hovered, with the hint open and pressed, and the
//! same action on click, at 360 and 900.

use std::time::{Duration, Instant};

use crate::{environment_ui, theme, ui_sizing};
use openide_ui::{
    Button, ButtonGroup, ButtonGroupSizing, ButtonVariant, Color, Component, ControlSize, Element,
    Frame, Icon, IconButton, Point, RadiusRole, Size, Tooltip, TooltipPlacement, TooltipSpec,
    UiAction, UiEvent, UiTree,
};

/// Every frame a pointer can produce over the control, and what a click reports.
pub(crate) fn states(control: Element, width: f32) -> (Vec<Frame>, Vec<UiAction>) {
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

pub(crate) fn assert_same(name: &str, legacy: Element, current: Element) {
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
fn the_comparison_sees_hover_fills_open_hints_and_a_different_label_size() {
    let tool = |size| {
        Button::new("tool", "Abrir", theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(size)
            .tooltip_with(TooltipSpec::new("Nuevo").placement(TooltipPlacement::Below))
            .render()
    };
    let (frames, actions) = states(tool(ControlSize::Compact), 360.0);
    assert_ne!(frames[0], frames[1], "hover paints a fill");
    assert_ne!(frames[1], frames[2], "the hint opens");
    assert_ne!(frames[1], frames[3], "press paints another fill");
    assert_eq!(actions, vec![UiAction::Clicked { id: "tool".into() }]);
    assert_ne!(frames[0], states(tool(ControlSize::Regular), 360.0).0[0]);
}

#[test]
fn the_agent_tab_close_matches_the_patched_dense_button_in_a_tooltip() {
    let palette = theme::action_palette(theme::BACKGROUND, theme::FOREGROUND);
    assert_same(
        "agent tab close",
        Tooltip::new(
            IconButton::new("close", Icon::Close, palette)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .render()
                .padding(2.0)
                .size(Some(20.0), Some(20.0)),
            "close",
            "Cerrar pestaña",
            theme::component_palette(),
        )
        .placement(TooltipPlacement::Below)
        .render(),
        IconButton::new("close", Icon::Close, palette)
            .variant(ButtonVariant::Ghost)
            .sizing(ui_sizing::AGENT_TAB_CLOSE)
            .tooltip_with(
                TooltipSpec::new("Cerrar pestaña")
                    .placement(TooltipPlacement::Below)
                    .palette(theme::component_palette()),
            )
            .render(),
    );
}

#[test]
fn revoke_and_the_small_label_buttons_match_their_font_size_overrides() {
    let palette = theme::component_palette();
    assert_same(
        "Revocar",
        Button::new("revoke", "Revocar", palette)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .ink(theme::DESTRUCTIVE)
            .hover_ink(theme::FOREGROUND)
            .render()
            .font_size(11.0)
            .size(Some(72.0), Some(32.0)),
        Button::new("revoke", "Revocar", palette)
            .variant(ButtonVariant::Ghost)
            .sizing(ui_sizing::GRANT_ACTION)
            .width(72.0)
            .ink(theme::DESTRUCTIVE)
            .hover_ink(theme::FOREGROUND)
            .render(),
    );
    for surface in [theme::SIDEBAR, theme::BACKGROUND] {
        let palette = theme::action_palette(surface, theme::FOREGROUND);
        for icon in [None, Some(Icon::Github)] {
            let link = || {
                let button = Button::new("link", "#12 Arregla el panel", palette)
                    .variant(ButtonVariant::Ghost);
                match icon {
                    Some(icon) => button.icon(icon),
                    None => button,
                }
            };
            assert_same(
                &format!("git link icon={}", icon.is_some()),
                link().size(ControlSize::Compact).render().font_size(11.0),
                link().sizing(ui_sizing::SMALL_LABEL_BUTTON).render(),
            );
        }
    }
    let palette = theme::action_palette(theme::SIDEBAR, theme::FOREGROUND);
    assert_same(
        "repository link",
        Tooltip::new(
            Button::new("repo", "nihuel/openADE", palette)
                .icon(Icon::Github)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render()
                .font_size(11.0),
            "repo",
            "Abrir repositorio en GitHub",
            theme::component_palette(),
        )
        .render(),
        Button::new("repo", "nihuel/openADE", palette)
            .icon(Icon::Github)
            .variant(ButtonVariant::Ghost)
            .sizing(ui_sizing::SMALL_LABEL_BUTTON)
            .tooltip_with(
                TooltipSpec::new("Abrir repositorio en GitHub").palette(theme::component_palette()),
            )
            .render(),
    );
    for enabled in [true, false] {
        assert_same(
            &format!("Añadir enabled={enabled}"),
            environment_ui::interactive_surface(
                Button::new("preset", "Añadir", theme::component_palette())
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .enabled(enabled)
                    .render()
                    .font_size(11.0),
            ),
            crate::project_actions_ui::preset_add("preset".into(), enabled),
        );
    }
}

#[test]
fn environment_card_buttons_match_the_interactive_surface_wrapper() {
    for enabled in [true, false] {
        let compare = |palette| {
            Button::new("compare", "main", palette)
                .icon(Icon::GitBranch)
                .trailing_icon(Icon::ChevronDown)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Regular)
                .enabled(enabled)
                .width(244.0)
                .render()
        };
        assert_same(
            &format!("compare trigger enabled={enabled}"),
            environment_ui::interactive_surface(compare(theme::component_palette())),
            compare(environment_ui::card_button_palette()),
        );
        for (variant, size) in [
            (ButtonVariant::Ghost, ControlSize::Regular),
            (ButtonVariant::Ghost, ControlSize::Compact),
            (ButtonVariant::Subtle, ControlSize::Compact),
        ] {
            let action = |palette| {
                Button::new("action", "Actualizar comparación", palette)
                    .variant(variant)
                    .size(size)
                    .enabled(enabled)
                    .render()
            };
            assert_same(
                &format!("card action {variant:?} {size:?} enabled={enabled}"),
                environment_ui::interactive_surface(action(theme::component_palette())),
                action(environment_ui::card_button_palette()),
            );
        }
    }
}

#[test]
fn terminal_buttons_match_their_fixed_sizes_under_the_wrapper() {
    for (label, icon, variant, width) in [
        ("Reintentar", None, ButtonVariant::Ghost, 110.0),
        ("Volver al prompt", Some(Icon::ArrowDown), ButtonVariant::Subtle, 164.0),
        ("Reiniciar", None, ButtonVariant::Ghost, 88.0),
        ("Reintentar", None, ButtonVariant::Subtle, 110.0),
    ] {
        let button = || {
            let button = Button::new("terminal", label, theme::component_palette()).variant(variant);
            match icon {
                Some(icon) => button.icon(icon),
                None => button,
            }
        };
        assert_same(
            &format!("terminal {label} {variant:?}"),
            theme::interactive_surface(button().render().size(Some(width), Some(32.0))),
            button().width(width).render(),
        );
    }
}

#[test]
fn action_groups_match_their_row_or_column_switches() {
    let actions = || {
        ["Probar", "Editar", "Quitar"]
            .into_iter()
            .map(|label| crate::settings_ui::outline(label, label).render())
            .collect::<Vec<_>>()
    };
    let legacy = |sizing: ButtonGroupSizing, compact: bool| {
        if compact {
            Element::column(actions()).gap(sizing.stacked_gap)
        } else {
            Element::row(actions()).gap(sizing.gap)
        }
    };
    for (sizing, gaps) in [
        (ui_sizing::ENTITY_ACTIONS, (8.0, 6.0)),
        (ui_sizing::OUTPUT_LIMIT_ACTIONS, (8.0, 8.0)),
        (ui_sizing::INTERRUPTION_ACTIONS, (10.0, 8.0)),
    ] {
        // The numbers the hand-written switches had.
        assert_eq!((sizing.gap, sizing.stacked_gap), gaps);
        for compact in [false, true] {
            assert_same(
                &format!("group {gaps:?} compact={compact}"),
                legacy(sizing, compact),
                ButtonGroup::new(actions()).sizing(sizing).stacked(compact).render(),
            );
        }
    }
}

#[test]
fn in_row_controls_match_their_transparent_overrides() {
    for surface in [theme::SIDEBAR, theme::BACKGROUND] {
        let palette = theme::action_palette(surface, theme::FOREGROUND);
        for enabled in [true, false] {
            assert_same(
                &format!("stage control enabled={enabled}"),
                Tooltip::new(
                    IconButton::new("stage", Icon::Add, palette)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Dense)
                        .enabled(enabled)
                        .render()
                        .background(Color::TRANSPARENT)
                        .hover_background(Color::TRANSPARENT)
                        .pressed_background(Color::TRANSPARENT)
                        .radius(0.0)
                        .transition_ms(0),
                    "stage",
                    "Preparar archivo",
                    theme::component_palette(),
                )
                .placement(TooltipPlacement::Below)
                .render(),
                IconButton::new("stage", Icon::Add, theme::in_row(palette))
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Dense)
                    .radius(RadiusRole::Square)
                    .enabled(enabled)
                    .tooltip_with(
                        TooltipSpec::new("Preparar archivo")
                            .placement(TooltipPlacement::Below)
                            .palette(theme::component_palette()),
                    )
                    .render(),
            );
        }
    }
    let palette = theme::action_palette(Color::TRANSPARENT, theme::FOREGROUND_SUBTLE);
    for menu_open in [false, true] {
        let legacy = IconButton::new("more", Icon::More, palette)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .render()
            .background(Color::TRANSPARENT)
            .hover_background(Color::TRANSPARENT)
            .pressed_background(Color::TRANSPARENT)
            .transition_ms(0)
            .padding(4.0)
            .min_size(0.0, 0.0)
            .size(Some(24.0), Some(24.0));
        assert_same(
            &format!("session menu open={menu_open}"),
            if menu_open {
                legacy
            } else {
                Tooltip::new(legacy, "more", "Más acciones", theme::component_palette())
                    .placement(TooltipPlacement::Right)
                    .render()
            },
            IconButton::new("more", Icon::More, theme::in_row(palette))
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .tooltip_with(
                    TooltipSpec::new("Más acciones")
                        .placement(TooltipPlacement::Right)
                        .palette(theme::component_palette())
                        .suppressed(menu_open),
                )
                .render(),
        );
    }
}

#[test]
fn sidebar_tools_match_their_external_tooltips() {
    for menu_open in [false, true] {
        let anchor = || {
            IconButton::new("group", Icon::More, theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Dense)
                .width(24.0)
                .active(menu_open)
        };
        assert_same(
            &format!("group menu open={menu_open}"),
            if menu_open {
                anchor().render()
            } else {
                Tooltip::new(anchor().render(), "group", "Opciones del grupo", theme::component_palette())
                    .placement(TooltipPlacement::Right)
                    .render()
            },
            anchor()
                .tooltip_with(
                    TooltipSpec::new("Opciones del grupo")
                        .placement(TooltipPlacement::Right)
                        .suppressed(menu_open),
                )
                .render(),
        );
    }
    for enabled in [true, false] {
        let expand = || {
            IconButton::new(
                "expand",
                Icon::Expand,
                theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
            )
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Dense)
            .enabled(enabled)
        };
        assert_same(
            &format!("expand all enabled={enabled}"),
            Tooltip::new(
                expand().render().size(Some(24.0), Some(24.0)),
                "expand",
                "Expandir proyectos",
                theme::component_palette(),
            )
            .placement(TooltipPlacement::Below)
            .render(),
            expand()
                .tooltip_with(
                    TooltipSpec::new("Expandir proyectos")
                        .placement(TooltipPlacement::Below)
                        .palette(theme::component_palette()),
                )
                .render(),
        );
    }
}

/// Why the project search submit stays patched: a profile equals it while
/// enabled, but centers the disabled glyph the patch leaves off center.
#[test]
fn a_profile_cannot_draw_the_disabled_search_submit() {
    const SUBMIT: openide_ui::ButtonSizing = openide_ui::ButtonSizing {
        height: 24.0,
        font_size: 13.0,
        icon_size: 16.0,
        horizontal_padding: 4.0,
        inner_spacing: 6.0,
    };
    let legacy = |enabled: bool| {
        IconButton::new("submit", Icon::Search, theme::sidebar_palette())
            .variant(ButtonVariant::Ghost)
            .enabled(enabled)
            .render()
            .padding(4.0)
            .radius(4.0)
            .size(Some(24.0), Some(24.0))
    };
    let profiled = |enabled: bool| {
        IconButton::new("submit", Icon::Search, theme::sidebar_palette())
            .variant(ButtonVariant::Ghost)
            .sizing(SUBMIT)
            .enabled(enabled)
            .render()
    };
    assert_same("search submit enabled", legacy(true), profiled(true));
    assert_ne!(states(legacy(false), 360.0).0, states(profiled(false), 360.0).0);
}

#[test]
fn the_session_changes_refresh_matches_its_wrapped_button() {
    for enabled in [true, false] {
        let refresh = || {
            IconButton::new("refresh", Icon::Refresh, theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .button_size(openide_ui::ButtonSize::IconSmall)
                .enabled(enabled)
        };
        assert_same(
            &format!("session changes refresh enabled={enabled}"),
            Tooltip::new(
                theme::interactive_surface(refresh().render()),
                "refresh",
                "Actualizar cambios del chat",
                theme::component_palette(),
            )
            .render(),
            refresh().tooltip("Actualizar cambios del chat").render(),
        );
    }
}
