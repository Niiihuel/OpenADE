//! The private text-button wrappers and post-render overrides that plain limn
//! `Button` calls, `settings_ui::outline`, `theme::primary` and `ButtonGroup`
//! replaced, kept so each replacement stays provably identical: same draw
//! commands idle, hovered, with the hint open and pressed, and the same
//! action on click. Delete with the final audit (S18).

use crate::{icon_button_parity_tests::assert_same, settings_ui::outline, theme};
use openide_ui::{
    ActiveHover, Button, ButtonGroup, ButtonSize, ButtonVariant, Component, ControlSize, Element,
    Icon, Tooltip, TooltipPlacement,
};

fn control() -> openide_ui::ComponentPalette {
    theme::control_palette()
}

fn component() -> openide_ui::ComponentPalette {
    theme::component_palette()
}

fn legacy_action_button(id: &str, label: &str, icon: Icon) -> Element {
    let page = id.strip_prefix("settings_page:");
    let widget_id = page
        .map(|page| format!("settings_link:{page}"))
        .unwrap_or_else(|| id.to_owned());
    let button = Button::new(widget_id, label, control())
        .icon(icon)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .render();
    if page.is_some() {
        button.dispatch_as(id)
    } else {
        button
    }
}

fn legacy_mcp_action_button_loading(
    id: &str,
    label: &str,
    icon: Icon,
    enabled: bool,
    loading: bool,
) -> Element {
    Button::new(id, label, control())
        .icon(icon)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .enabled(enabled)
        .loading(loading)
        .render()
}

fn legacy_isolation_choice(id: &str, label: &str, active: bool, enabled: bool) -> Element {
    Button::new(id, label, control())
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .active(active)
        .enabled(enabled)
        .render()
}

fn legacy_choice_group(options: Vec<Element>, compact: bool) -> Element {
    if compact {
        Element::column(options).gap(6.0)
    } else {
        Element::row(options).gap(6.0)
    }
}

#[test]
fn settings_action_buttons_match_outline() {
    for id in ["workspace_apply", "settings_page:appearance"] {
        assert_same(
            &format!("action_button {id}"),
            legacy_action_button(id, "Personalizar", Icon::ArrowRight),
            outline(id, "Personalizar").icon(Icon::ArrowRight).render(),
        );
    }
    for enabled in [true, false] {
        for loading in [false, true] {
            assert_same(
                &format!("mcp_action_button enabled={enabled} loading={loading}"),
                legacy_mcp_action_button_loading(
                    "mcp_reload",
                    "Recargar",
                    Icon::Refresh,
                    enabled,
                    loading,
                ),
                outline("mcp_reload", "Recargar")
                    .icon(Icon::Refresh)
                    .enabled(enabled)
                    .loading(loading)
                    .render(),
            );
        }
        for active in [false, true] {
            assert_same(
                &format!("choice active={active} enabled={enabled}"),
                legacy_isolation_choice("choice", "Proyecto", active, enabled),
                outline("choice", "Proyecto")
                    .active(active)
                    .enabled(enabled)
                    .render(),
            );
        }
    }
}

#[test]
fn settings_choice_groups_match_button_group() {
    let options = || {
        vec![
            legacy_isolation_choice("one", "Preguntar", true, true),
            legacy_isolation_choice("two", "Permitir", false, true),
            legacy_isolation_choice("three", "Denegar", false, false),
        ]
    };
    for compact in [false, true] {
        assert_same(
            &format!("choice_group compact={compact}"),
            legacy_choice_group(options(), compact),
            ButtonGroup::new(options()).stacked(compact).render(),
        );
    }
}

#[test]
fn primary_matches_the_brand_solid_button() {
    assert_same(
        "primary",
        Button::new(
            "save",
            "Guardar cambios",
            theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
        )
        .variant(ButtonVariant::Solid)
        .size(ControlSize::Compact)
        .render(),
        theme::primary("save", "Guardar cambios")
            .size(ControlSize::Compact)
            .render(),
    );
}

// native_command_control_ui::action and native_command_history_ui::button.
fn legacy_surface_outline(id: &str, label: &str, enabled: bool) -> Element {
    theme::interactive_surface(
        Button::new(id, label, component())
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(enabled)
            .render(),
    )
}

fn legacy_dock_compact_action(id: &str, title: &str, icon: Icon) -> Element {
    theme::interactive_surface(
        Button::new(id, title, component())
            .icon(icon)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render(),
    )
}

#[test]
fn canvas_buttons_match_without_the_surface_wrapper() {
    for enabled in [true, false] {
        assert_same(
            &format!("outline enabled={enabled}"),
            legacy_surface_outline(
                "native_command_control_refresh",
                "Actualizar estado",
                enabled,
            ),
            Button::new(
                "native_command_control_refresh",
                "Actualizar estado",
                component(),
            )
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .enabled(enabled)
            .render(),
        );
    }
    assert_same(
        "dock compact_action",
        legacy_dock_compact_action("native_command_history_open", "Historial", Icon::History),
        Button::new("native_command_history_open", "Historial", component())
            .icon(Icon::History)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render(),
    );
    // The history selector's trigger.
    let trigger = || {
        Button::new("selector", "3 comandos guardados", component())
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .icon(Icon::Terminal)
            .trailing_icon(Icon::ChevronDown)
            .width(260.0)
            .render()
    };
    assert_same(
        "history trigger",
        theme::interactive_surface(trigger()),
        trigger(),
    );
    // The git panel's repository and pull request links.
    let palette = theme::sidebar_palette();
    let link = |font: bool| {
        let button = Button::new("github_repo_open", "owner/repo", palette)
            .icon(Icon::Github)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render();
        if font {
            button.font_size(11.0)
        } else {
            button
        }
    };
    assert_same(
        "git link",
        palette.interactive_surface(link(true)),
        link(true),
    );
    assert_same(
        "git link",
        palette.interactive_surface(link(false)),
        link(false),
    );
}

fn legacy_workbench_action(
    id: &str,
    title: &str,
    tip: &str,
    shortcut: Option<&str>,
    active: bool,
) -> Element {
    let mut button = Button::new(id, title, component())
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .active(active)
        .tooltip(tip);
    if let Some(shortcut) = shortcut {
        button = button.tooltip_shortcut(shortcut);
    }
    theme::interactive_surface(button.render())
}

#[test]
fn workbench_actions_match_with_the_palette_hover() {
    for active in [false, true] {
        assert_same(
            &format!("action active={active}"),
            legacy_workbench_action(
                "editor_tab_symbols",
                "Símbolos",
                "Ver símbolos",
                None,
                active,
            ),
            Button::new("editor_tab_symbols", "Símbolos", component())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .active(active)
                .active_hover(ActiveHover::Palette)
                .tooltip("Ver símbolos")
                .render(),
        );
        assert_same(
            &format!("option active={active}"),
            theme::interactive_surface(
                Button::new("editor_case", "Aa", component())
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::IconExtraSmall)
                    .width(28.0)
                    .active(active)
                    .tooltip("Distinguir mayúsculas y minúsculas")
                    .render(),
            ),
            Button::new("editor_case", "Aa", component())
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::IconExtraSmall)
                .width(28.0)
                .active(active)
                .active_hover(ActiveHover::Palette)
                .tooltip("Distinguir mayúsculas y minúsculas")
                .render(),
        );
    }
    assert_same(
        "action with shortcut",
        legacy_workbench_action(
            "editor_form_apply",
            "Aplicar",
            "Confirmar",
            Some("Enter"),
            false,
        ),
        Button::new("editor_form_apply", "Aplicar", component())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .tooltip("Confirmar")
            .tooltip_shortcut("Enter")
            .render(),
    );
}

fn legacy_automation_action(
    id: &str,
    title: &str,
    icon: Option<Icon>,
    variant: ButtonVariant,
) -> Element {
    let mut button = Button::new(id, title, component())
        .variant(variant)
        .size(ControlSize::Regular);
    if let Some(icon) = icon {
        button = button.icon(icon);
    }
    button.render()
}

fn legacy_plugins_button(id: &str, text: &str, icon: Icon, enabled: bool) -> Element {
    Button::new(id, text, component())
        .icon(icon)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .enabled(enabled)
        .render()
}

fn legacy_git_action(id: &str, title: &str, enabled: bool, primary: bool) -> Element {
    let button = Button::new(id, title, component())
        .variant(if primary {
            ButtonVariant::Default
        } else {
            ButtonVariant::Ghost
        })
        .size(ControlSize::Regular)
        .enabled(enabled)
        .render();
    if primary {
        button
    } else {
        crate::environment_ui::interactive_surface(button)
    }
}

fn legacy_git_compact_action(
    id: &str,
    title: &str,
    icon: Icon,
    enabled: bool,
    primary: bool,
    width: f32,
) -> Element {
    let palette = theme::action_palette(theme::SIDEBAR, theme::FOREGROUND);
    Button::new(id, title, palette)
        .icon(icon)
        .variant(if primary {
            ButtonVariant::Default
        } else {
            ButtonVariant::Outline
        })
        .size(ControlSize::Compact)
        .enabled(enabled)
        .width(width)
        .render()
}

#[test]
fn inlined_wrappers_match_their_direct_calls() {
    for (icon, variant) in [
        (None, ButtonVariant::Default),
        (Some(Icon::Add), ButtonVariant::Default),
        (None, ButtonVariant::Ghost),
        (Some(Icon::Trash), ButtonVariant::Secondary),
    ] {
        let mut direct = Button::new("automation_new", "Nueva", component()).variant(variant);
        if let Some(icon) = icon {
            direct = direct.icon(icon);
        }
        assert_same(
            &format!("automation action {icon:?} {variant:?}"),
            legacy_automation_action("automation_new", "Nueva", icon, variant),
            direct.size(ControlSize::Regular).render(),
        );
    }
    for enabled in [true, false] {
        assert_same(
            &format!("plugins button enabled={enabled}"),
            legacy_plugins_button("plugins_refresh", "Actualizar", Icon::Refresh, enabled),
            Button::new("plugins_refresh", "Actualizar", component())
                .icon(Icon::Refresh)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(enabled)
                .render(),
        );
        assert_same(
            &format!("git primary enabled={enabled}"),
            legacy_git_action("environment_git_compare_refresh", "Comparar", enabled, true),
            Button::new("environment_git_compare_refresh", "Comparar", component())
                .size(ControlSize::Regular)
                .enabled(enabled)
                .render(),
        );
        assert_same(
            &format!("git ghost enabled={enabled}"),
            legacy_git_action(
                "environment_git_compare_refresh",
                "Comparar",
                enabled,
                false,
            ),
            crate::environment_ui::interactive_surface(
                Button::new("environment_git_compare_refresh", "Comparar", component())
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Regular)
                    .enabled(enabled)
                    .render(),
            ),
        );
        for primary in [true, false] {
            assert_same(
                &format!("git compact enabled={enabled} primary={primary}"),
                legacy_git_compact_action(
                    "environment_git_commit",
                    "Commit",
                    Icon::Check,
                    enabled,
                    primary,
                    120.0,
                ),
                Button::new("environment_git_commit", "Commit", theme::sidebar_palette())
                    .icon(Icon::Check)
                    .variant(if primary {
                        ButtonVariant::Default
                    } else {
                        ButtonVariant::Outline
                    })
                    .size(ControlSize::Compact)
                    .enabled(enabled)
                    .width(120.0)
                    .render(),
            );
        }
    }
}

#[test]
fn named_options_match_the_post_render_overrides() {
    // files_panel_ui: "Más" under a folder that has more entries.
    let palette = theme::sidebar_palette();
    let hinted = |button: Element| {
        Tooltip::new(button, "file_more:3", "Cargar más archivos", component())
            .description("src/app")
            .placement(TooltipPlacement::Left)
            .render()
    };
    assert_same(
        "files more",
        hinted(
            Button::new("file_more:3", "Más", palette)
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::Small)
                .render()
                .font_size(11.0)
                .padding(3.0)
                .min_size(0.0, 0.0)
                .size(Some(36.0), Some(22.0)),
        ),
        hinted(
            Button::new("file_more:3", "Más", palette)
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::Mini)
                .width(36.0)
                .render(),
        ),
    );
    // code_editor_ui: the destructive ghost in the quit dialog.
    for enabled in [true, false] {
        assert_same(
            &format!("discard enabled={enabled}"),
            Button::new(
                "code_editor_quit_discard",
                "Descartar los borradores y salir",
                component(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .enabled(enabled)
            .render()
            .foreground(theme::DESTRUCTIVE),
            Button::new(
                "code_editor_quit_discard",
                "Descartar los borradores y salir",
                component(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .enabled(enabled)
            .ink(theme::DESTRUCTIVE)
            .hover_ink(theme::FOREGROUND)
            .render(),
        );
    }
    // dock_activity: revoking a session permission.
    assert_same(
        "revoke",
        theme::interactive_surface(
            Button::new("session_approval_revoke:1", "Revocar", component())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render()
                .padding(6.0)
                .font_size(11.0)
                .foreground(theme::DESTRUCTIVE)
                .size(Some(72.0), Some(32.0)),
        ),
        Button::new("session_approval_revoke:1", "Revocar", component())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .ink(theme::DESTRUCTIVE)
            .hover_ink(theme::FOREGROUND)
            .render()
            .font_size(11.0)
            .size(Some(72.0), Some(32.0)),
    );
    // session_surface: Compact is already 28 px tall with a 12 px label.
    for variant in [ButtonVariant::Solid, ButtonVariant::Subtle] {
        let button = || {
            Button::new(
                "subagent_resume:1",
                "Reanudar subagente",
                theme::sidebar_palette(),
            )
            .variant(variant)
            .size(ControlSize::Compact)
        };
        assert_same(
            &format!("fixed width {variant:?}"),
            button()
                .render()
                .font_size(12.0)
                .size(Some(176.0), Some(28.0)),
            button().width(176.0).render(),
        );
    }
    // github_account_menu_ui::connection_button. Busy, the override never
    // reached the passive row, so the spinner keeps the Dense gap.
    for (dropdown, loading) in [(false, false), (true, false), (false, true)] {
        let button = || {
            let button = Button::new("github_account_connect", "Conectar GitHub", palette)
                .icon(Icon::Github)
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Dense)
                .width(220.0)
                .loading(loading);
            if dropdown {
                button.trailing_icon(Icon::ChevronDown)
            } else {
                button
            }
        };
        assert_same(
            &format!("connection dropdown={dropdown} loading={loading}"),
            button().render().gap(8.0),
            if loading {
                button().render()
            } else {
                button().gap(8.0).render()
            },
        );
    }
}

#[test]
fn the_context_footer_matches_an_end_aligned_group() {
    let details = || {
        Button::new(
            "composer_context_details_toggle",
            "Detalles",
            theme::menu_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .render()
    };
    let compact = || {
        Button::new(
            "composer_context_compact:1",
            "Compactar",
            theme::menu_palette(),
        )
        .icon(Icon::Collapse)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .render()
    };
    assert_same(
        "footer",
        Element::row(vec![Element::spacer().flex(1.0), details()])
            .gap(6.0)
            .size(None, Some(28.0)),
        ButtonGroup::new(vec![details()])
            .align_end()
            .render()
            .size(None, Some(28.0)),
    );
    assert_same(
        "footer with compaction",
        Element::row(vec![compact(), Element::spacer().flex(1.0), details()])
            .gap(6.0)
            .size(None, Some(28.0)),
        ButtonGroup::new(vec![details()])
            .align_end()
            .leading(compact())
            .render()
            .size(None, Some(28.0)),
    );
}

#[test]
fn the_comparison_sees_a_label_ink_that_differs_only_under_the_pointer() {
    let button = |hover: bool| {
        let button = Button::new("discard", "Descartar", component())
            .variant(ButtonVariant::Ghost)
            .ink(theme::DESTRUCTIVE);
        if hover {
            button.hover_ink(theme::FOREGROUND).render()
        } else {
            button.render()
        }
    };
    let frames = |element| crate::icon_button_parity_tests::states(element, 360.0).0;
    let (plain, hovering) = (frames(button(false)), frames(button(true)));
    assert_eq!(plain[0], hovering[0]);
    assert_ne!(plain[1], hovering[1]);
}

/// The replaced controls that no other export shows, old form on the left.
#[test]
#[ignore = "writes review frames to /tmp/openade-text-buttons-review"]
fn export_text_buttons_visual_review() {
    let sidebar = theme::sidebar_palette();
    let pair = |legacy: Element, current: Element| {
        Element::row(vec![
            legacy,
            Element::spacer().size(Some(24.0), None),
            current,
            Element::spacer().flex(1.0),
        ])
    };
    let review = |suffix: &str| {
        [
            "Revisar entrada",
            "Revisar cierre de entrada (EOF)",
            "Revisar terminación",
        ]
        .into_iter()
        .map(|label| {
            Button::new(format!("{label}:{suffix}"), label, component())
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render()
        })
        .collect::<Vec<_>>()
    };
    let rows = |stacked: bool| {
        vec![
            pair(
                Button::new("more:old", "Más", sidebar)
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::Small)
                    .render()
                    .font_size(11.0)
                    .padding(3.0)
                    .min_size(0.0, 0.0)
                    .size(Some(36.0), Some(22.0)),
                Button::new("more:new", "Más", sidebar)
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::Mini)
                    .width(36.0)
                    .render(),
            ),
            pair(
                theme::interactive_surface(
                    Button::new("revoke:old", "Revocar", component())
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Compact)
                        .render()
                        .padding(6.0)
                        .font_size(11.0)
                        .foreground(theme::DESTRUCTIVE)
                        .size(Some(72.0), Some(32.0)),
                ),
                Button::new("revoke:new", "Revocar", component())
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .ink(theme::DESTRUCTIVE)
                    .hover_ink(theme::FOREGROUND)
                    .render()
                    .font_size(11.0)
                    .size(Some(72.0), Some(32.0)),
            ),
            pair(
                legacy_dock_compact_action("history:old", "Historial", Icon::History),
                Button::new("history:new", "Historial", component())
                    .icon(Icon::History)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .render(),
            ),
            pair(
                Button::new(
                    "resume:old",
                    "Reanudar subagente",
                    theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND),
                )
                .variant(ButtonVariant::Solid)
                .size(ControlSize::Compact)
                .render()
                .font_size(12.0)
                .size(Some(176.0), Some(28.0)),
                theme::primary("resume:new", "Reanudar subagente")
                    .size(ControlSize::Compact)
                    .width(176.0)
                    .render(),
            ),
            legacy_choice_group(review("old"), stacked),
            ButtonGroup::new(review("new")).stacked(stacked).render(),
        ]
    };
    let directory = std::path::Path::new("/tmp/openade-text-buttons-review");
    std::fs::create_dir_all(directory).unwrap();
    for width in [360.0, 900.0] {
        let mut tree = openide_ui::UiTree::new(
            Element::panel(Element::column(rows(width < 620.0)).gap(12.0))
                .padding(16.0)
                .background(theme::BACKGROUND),
        );
        tree.set_reduced_motion(true);
        let frame = tree.layout(openide_ui::Size::new(width, 420.0));
        crate::settings_ui::visual_qa::write_frame_svg(
            &frame,
            &directory.join(format!("text-buttons-{width}.svg")),
            width,
            420.0,
        )
        .unwrap();
    }
}
