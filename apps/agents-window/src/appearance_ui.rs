//! Appearance controls use the same native rows and selectors as other settings.
use crate::{
    appearance_preferences::MotionPreference, settings_parts, settings_ui, theme, view::ViewState,
};
use openide_ui::{
    Button, Component, ControlSize, DropdownOption, Element, NumberInput,
    NumberInputButtonPlacement, SearchField, Select, SelectVariant, Slider, Text,
};

fn slider(
    id: &str,
    name: &str,
    fraction: f32,
    steps: usize,
    value: &str,
    enabled: bool,
) -> Element {
    Element::row(vec![
        Slider::new(id, fraction, theme::control_palette())
            .accessibility_label(name)
            .steps(steps)
            .enabled(enabled)
            .width(160.0)
            .render(),
        Text::new(value, theme::card_palette()).render(),
    ])
    .gap(8.0)
    .centered_y()
}

fn size_control(kind: &str, value: u8, min: u8, max: u8) -> Element {
    let target = if kind == "ui" { "interfaz" } else { "código" };
    Element::row(vec![
        NumberInput::new(
            format!("appearance_size:{kind}"),
            value.to_string(),
            theme::control_palette(),
        )
        .min(f64::from(min))
        .max(f64::from(max))
        .size(ControlSize::Compact)
        .button_placement(NumberInputButtonPlacement::Split)
        .width(144.0)
        .accessibility_label(format!("Tamaño de {target}"))
        .step_labels(
            format!("Reducir tamaño de {target}"),
            format!("Aumentar tamaño de {target}"),
        )
        .render(),
        Button::new(
            format!("appearance_size:{kind}:reset"),
            "Restablecer",
            theme::card_palette(),
        )
        .size(ControlSize::Compact)
        .accessibility_label(format!("Restablecer tamaño de {target}"))
        .render(),
    ])
    .gap(6.0)
    .centered_y()
}

fn family_control(state: &ViewState, kind: &str, current: &str) -> Element {
    let index = usize::from(kind == "code");
    let query = &state.appearance_font_query[index];
    let query_id = format!("appearance_font_query:{kind}");
    let choice = format!("appearance_{kind}_font");
    let trigger_id = format!("settings_choice_toggle:{choice}");
    let label = if kind == "code" {
        "Fuente de código"
    } else {
        "Fuente de interfaz"
    };
    let default_label =
        crate::i18n::text("Predeterminada").unwrap_or_else(|| "Predeterminada".to_owned());
    Select::new(
        &trigger_id,
        if current.is_empty() {
            &default_label
        } else {
            current
        },
        // Editor and terminal grid measurements currently require monospaced code fonts.
        family_options(
            kind,
            current,
            query,
            crate::fonts::families_for(kind),
            &default_label,
        ),
        state.settings_choice_menu_open.as_deref() == Some(choice.as_str()),
        "settings_choice_dismiss",
        theme::control_palette(),
    )
    .variant(SelectVariant::Field)
    .size(ControlSize::Compact)
    .width(240.0)
    .menu_width(280.0)
    .accessibility_label(label)
    .menu_palette(theme::menu_palette())
    .header(
        SearchField::new(&query_id, query, "Buscar fuente", theme::menu_palette())
            .size(ControlSize::Compact)
            .width(264.0)
            .accessibility_label(format!("Buscar {}", label.to_lowercase()))
            .clearable(true)
            .render()
            .padding(4.0),
    )
    .render()
    .popover_initial_focus(query_id)
    .popover_return_focus(trigger_id)
}

fn family_options(
    kind: &str,
    current: &str,
    query: &str,
    families: &[String],
    default_label: &str,
) -> Vec<DropdownOption> {
    let query = query.trim().to_lowercase();
    let mut options = Vec::new();
    if default_label.to_lowercase().contains(&query) || "predeterminada".contains(&query) {
        options.push(
            DropdownOption::new(format!("appearance_family:{kind}:default"), default_label)
                .selected(current.is_empty()),
        );
    }
    if !current.is_empty() && !families.iter().any(|family| family == current) {
        let unavailable =
            crate::i18n::text("No disponible").unwrap_or_else(|| "No disponible".to_owned());
        let label = format!("{current} · {unavailable}");
        if label.to_lowercase().contains(&query) {
            options.push(
                DropdownOption::new(format!("appearance_family:{kind}:unavailable"), label)
                    .selected(true)
                    .disabled(),
            );
        }
    }
    options.extend(
        families
            .iter()
            .enumerate()
            .filter(|(_, family)| family.to_lowercase().contains(&query))
            .map(|(index, family)| {
                DropdownOption::new(format!("appearance_family:{kind}:{index}"), family)
                    .selected(current == family)
            }),
    );
    if options.is_empty() {
        options.push(
            DropdownOption::new(
                format!("appearance_family:{kind}:none"),
                "No se encontraron fuentes",
            )
            .disabled(),
        );
    }
    options
}

pub(crate) fn groups(state: &ViewState, compact: bool) -> Vec<Element> {
    use crate::appearance_preferences::{CODE_FONT_MAX, CODE_FONT_MIN, UI_FONT_MAX, UI_FONT_MIN};
    let p = &state.appearance;
    let tint_enabled = crate::theme_mode::choice() != crate::theme_mode::ThemeChoice::Custom;
    vec![
        settings_parts::settings_group(
            "Conversaciones del agente",
            vec![
                settings_parts::settings_row(
                    "Densidad de herramientas",
                    "Compacta agrupa llamadas; equilibrada las separa; detallada abre sus resultados.",
                    Slider::new(
                        "appearance_density",
                        p.tool_call_density.fraction(),
                        theme::control_palette(),
                    )
                    .accessibility_label("Densidad de herramientas")
                    .steps(3)
                    .mark_labels(["Compacta", "Equilibrada", "Detallada"])
                    .width(230.0)
                    .render(),
                    compact,
                ),
                settings_parts::switch_row(
                    "appearance_wrap",
                    "Ajustar líneas de código",
                    "Ajusta las líneas largas de los bloques de código de la conversación.",
                    p.code_word_wrap,
                    true,
                    compact,
                ),
                settings_parts::switch_row(
                    "appearance_diff",
                    "Fondos de diferencias",
                    "Resalta el fondo de las líneas agregadas y eliminadas.",
                    p.themed_diff_backgrounds,
                    true,
                    compact,
                ),
            ],
        ),
        settings_parts::settings_group(
            "Colores",
            vec![
                settings_parts::settings_row(
                    "Tono",
                    if tint_enabled {
                        "Elige el color del tinte de las superficies."
                    } else {
                        "Los colores de este tema se ajustan desde Editar tema."
                    },
                    slider(
                        "appearance_hue",
                        "Tono",
                        p.hue_degrees as f32 / 360.0,
                        361,
                        &format!("{}°", p.hue_degrees),
                        tint_enabled,
                    ),
                    compact,
                ),
                settings_parts::settings_row(
                    "Intensidad",
                    "Controla cuánto tinte se aplica a las superficies.",
                    slider(
                        "appearance_intensity",
                        "Intensidad",
                        p.tint_intensity as f32 / 1000.0,
                        101,
                        &format!("{}%", p.tint_intensity / 10),
                        tint_enabled,
                    ),
                    compact,
                ),
                settings_parts::switch_row(
                    "appearance_transparency",
                    "Reducir transparencia",
                    "Usa fondos opacos en las superficies translúcidas.",
                    p.reduce_transparency,
                    true,
                    compact,
                ),
            ],
        ),
        settings_parts::settings_group(
            "Tipografía",
            vec![
                settings_parts::settings_row(
                    "Tamaño de interfaz",
                    "Escala la interfaz completa, incluidos sus controles.",
                    size_control("ui", p.ui_font_size, UI_FONT_MIN, UI_FONT_MAX),
                    compact,
                ),
                settings_parts::settings_row(
                    "Tamaño de código",
                    "Tamaño independiente para editores, bloques de código y diferencias.",
                    size_control("code", p.code_font_size, CODE_FONT_MIN, CODE_FONT_MAX),
                    compact,
                ),
                settings_parts::settings_row(
                    "Fuente de interfaz",
                    "Elige una familia instalada en este equipo.",
                    family_control(state, "ui", &p.ui_font_family),
                    compact,
                ),
                settings_parts::settings_row(
                    "Fuente de código",
                    "Elige una fuente monoespaciada para editores y diferencias.",
                    family_control(state, "code", &p.code_font_family),
                    compact,
                ),
                settings_parts::switch_row(
                    "appearance_smoothing",
                    "Suavizado de fuentes",
                    "Activa la colocación subpíxel; al desactivarlo, los trazos quedan más nítidos. El antialiasing permanece activo.",
                    p.font_smoothing,
                    true,
                    compact,
                ),
                crate::appearance_preview::typography_preview(p, compact),
            ],
        ),
        settings_parts::settings_group(
            "Movimiento",
            vec![settings_parts::settings_row(
                "Reducir movimiento",
                "Sistema sigue la preferencia del escritorio cuando está disponible.",
                settings_ui::settings_choice_selector(
                    state,
                    "appearance_motion",
                    match p.reduce_motion {
                        MotionPreference::System => "Sistema",
                        MotionPreference::Always => "Activado",
                        MotionPreference::Never => "Desactivado",
                    },
                    [
                        ("system", "Sistema", MotionPreference::System),
                        ("always", "Activado", MotionPreference::Always),
                        ("never", "Desactivado", MotionPreference::Never),
                    ]
                    .into_iter()
                    .map(|(id, label, value)| {
                        DropdownOption::new(format!("appearance_motion:{id}"), label)
                            .selected(p.reduce_motion == value)
                    })
                    .collect(),
                ),
                compact,
            )],
        ),
        settings_parts::settings_group(
            "Privacidad",
            vec![settings_parts::switch_row(
                "appearance_email",
                "Ocultar correo electrónico",
                "Oculta parcialmente los correos de las cuentas en la interfaz.",
                p.hide_email,
                true,
                compact,
            )],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{
        ControlState, Frame, HitKind, Key, Point, Role, Size, UiAction, UiEvent, UiTree,
    };

    fn export_frame(frame: &Frame, name: &str, width: f32, height: f32) {
        if let Some(directory) = std::env::var_os("OPENIDE_APPEARANCE_VISUAL_DIR") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join(format!("{name}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, width, height)
                .unwrap();
            let mut options = resvg::usvg::Options::default();
            options.fontdb_mut().load_system_fonts();
            let svg = std::fs::read(&path).unwrap();
            let tree = resvg::usvg::Tree::from_data(&svg, &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
        }
    }

    #[test]
    fn installed_font_picker_scrolls_to_last_family_while_search_stays_fixed() {
        for kind in ["ui", "code"] {
            let families = crate::fonts::families_for(kind);
            for width in [360.0, 900.0] {
                let mut state = crate::view::tests::empty_state();
                state.settings_choice_menu_open = Some(format!("appearance_{kind}_font"));
                let mut tree = UiTree::new(family_control(&state, kind, ""));
                tree.set_reduced_motion(true);
                let viewport = Size::new(width, 520.0);
                let frame = tree.layout(viewport);
                let region = |frame: &Frame, id: &str| {
                    frame
                        .regions
                        .iter()
                        .find(|region| region.id == id)
                        .unwrap()
                        .rect
                };
                let query_id = format!("appearance_font_query:{kind}");
                let header = region(&frame, &query_id);
                let surface = region(&frame, "popover_surface:settings_choice_dismiss");
                assert!(
                    header.x >= surface.x && header.x + header.width <= surface.x + surface.width
                );
                assert!(surface.x >= 0.0 && surface.x + surface.width <= width);
                assert!(surface.y >= 0.0 && surface.y + surface.height <= viewport.height);
                export_frame(
                    &frame,
                    &format!("font-picker-{kind}-{width:.0}"),
                    width,
                    viewport.height,
                );
                if families.len() > 16 {
                    let options_id = "settings_choice_dismiss:options";
                    let options = frame
                        .regions
                        .iter()
                        .find(|region| region.id == options_id && region.kind == HitKind::Scroll)
                        .unwrap()
                        .rect;
                    assert!(
                        frame
                            .regions
                            .iter()
                            .any(|region| region.id == options_id
                                && region.kind == HitKind::Scrollbar)
                    );
                    tree.handle_event(UiEvent::ScrollDirect {
                        position: Point::new(options.x + 16.0, options.y + 16.0),
                        delta_y: -100_000.0,
                    });
                    let scrolled = tree.layout(viewport);
                    assert!(tree.scroll_offset(options_id) > 0.0);
                    assert_eq!(region(&scrolled, &query_id), header);
                    let last = region(
                        &scrolled,
                        &format!("appearance_family:{kind}:{}", families.len() - 1),
                    );
                    assert!(
                        last.y >= options.y
                            && last.y + last.height <= options.y + options.height + 0.5
                    );
                    export_frame(
                        &scrolled,
                        &format!("font-picker-{kind}-{width:.0}-scrolled"),
                        width,
                        viewport.height,
                    );
                }
            }
        }
    }

    #[test]
    fn family_search_keeps_original_indices_and_disables_an_empty_result() {
        let families = ["Inter", "JetBrains Mono", "Liberation Sans"].map(String::from);
        let all = family_options("ui", "", "", &families, "Predeterminada");
        assert_eq!(all.len(), 4);
        assert!(all[0].selected);
        let filtered = family_options(
            "code",
            "JetBrains Mono",
            "  jEtBrAiNs  ",
            &families,
            "Predeterminada",
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].action_id, "appearance_family:code:1");
        assert_eq!(filtered[0].label, "JetBrains Mono");
        assert!(filtered[0].selected);
        let last = family_options("ui", "", "liberation", &families, "Predeterminada");
        assert_eq!(last[0].action_id, "appearance_family:ui:2");
        let empty = family_options("ui", "", "no-such-family", &families, "Predeterminada");
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].state, ControlState::Disabled);
        assert_eq!(empty[0].label, "No se encontraron fuentes");
        let english = family_options("ui", "", "default", &families, "Default");
        assert_eq!(english[0].action_id, "appearance_family:ui:default");
        assert_eq!(english[0].label, "Default");
        let missing = family_options(
            "code",
            "Missing Code Font",
            " missing code ",
            &families,
            "Default",
        );
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].action_id, "appearance_family:code:unavailable");
        assert!(missing[0].selected);
        assert_eq!(missing[0].state, ControlState::Disabled);
        assert!(missing[0].label.starts_with("Missing Code Font · "));
        let other = family_options("code", "Missing Code Font", "inter", &families, "Default");
        assert!(
            !other
                .iter()
                .any(|option| option.action_id.ends_with(":unavailable"))
        );
    }

    #[test]
    fn font_picker_focuses_search_and_returns_to_its_trigger_after_escape() {
        for kind in ["ui", "code"] {
            for width in [360.0, 900.0] {
                let mut state = crate::view::tests::empty_state();
                let index = usize::from(kind == "code");
                let choice = format!("appearance_{kind}_font");
                let query_id = format!("appearance_font_query:{kind}");
                let trigger_id = format!("settings_choice_toggle:{choice}");
                state.settings_choice_menu_open = Some(choice);
                state.appearance_font_query[index] = "no-such-family-012345".into();
                let mut tree = UiTree::new(family_control(&state, kind, ""));
                tree.set_reduced_motion(true);
                let frame = tree.layout(Size::new(width, 520.0));
                assert_eq!(tree.focused_id(), Some(query_id.as_str()));
                let search = frame
                    .access
                    .iter()
                    .find(|node| node.id == query_id)
                    .unwrap();
                assert_eq!(search.role, Role::SearchInput);
                let empty_id = format!("appearance_family:{kind}:none");
                let empty = frame
                    .access
                    .iter()
                    .find(|node| node.id == empty_id)
                    .unwrap();
                assert!(empty.disabled);
                assert!(!tree.focus(&empty_id));
                let surface = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "popover_surface:settings_choice_dismiss")
                    .unwrap()
                    .rect;
                assert!(surface.x >= 0.0 && surface.x + surface.width <= width);
                assert!(surface.y >= 0.0 && surface.y + surface.height <= 520.0);
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Escape,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Clicked {
                        id: "settings_choice_dismiss".into()
                    }]
                );
                state.settings_choice_menu_open = None;
                tree.set_root(family_control(&state, kind, ""));
                tree.layout(Size::new(width, 520.0));
                assert_eq!(tree.focused_id(), Some(trigger_id.as_str()));
            }
        }
    }

    #[test]
    fn appearance_controls_fit_narrow_and_wide_pages() {
        let mut state = crate::view::tests::empty_state();
        state.show_settings = true;
        state.settings_page = crate::settings_ui::SettingsPage::Appearance;
        for width in [360.0, 900.0] {
            let mut tree = UiTree::new(settings_ui::build_settings_page(
                &state,
                state.settings_page,
                width,
                true,
            ));
            tree.set_reduced_motion(true);
            let viewport = Size::new(width, 4000.0);
            let mut frame = tree.layout(viewport);
            if std::env::var_os("OPENIDE_APPEARANCE_VISUAL_DIR").is_some() {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while tree.animation_repaint_after().is_some()
                    && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(
                        std::time::Duration::from_millis(16)
                            .min(deadline.saturating_duration_since(std::time::Instant::now())),
                    );
                    frame = tree.layout(viewport);
                }
            }
            for id in [
                "appearance_density",
                "appearance_wrap",
                "appearance_diff",
                "appearance_hue",
                "appearance_intensity",
                "appearance_transparency",
                "appearance_size:ui",
                "appearance_size:ui:decrement",
                "appearance_size:ui:increment",
                "appearance_size:ui:reset",
                "appearance_size:code",
                "appearance_size:code:decrement",
                "appearance_size:code:increment",
                "appearance_size:code:reset",
                "settings_choice_toggle:appearance_ui_font",
                "settings_choice_toggle:appearance_code_font",
                "settings_choice_toggle:appearance_motion",
                "appearance_smoothing",
                "appearance_email",
            ] {
                let region = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap_or_else(|| panic!("missing {id}"));
                assert!(
                    region.rect.x >= -0.5 && region.rect.x + region.rect.width <= width + 0.5,
                    "{id} at {width}: {:?}",
                    region.rect
                );
                assert!(tree.focus(id), "{id} must be keyboard accessible");
            }
            export_frame(&frame, &format!("appearance-{width:.0}"), width, 4000.0);
        }
    }

    #[test]
    fn size_fields_and_joined_buttons_have_names_focus_order_and_disabled_bounds() {
        for (kind, target, min, max) in [("ui", "interfaz", 7, 38), ("code", "código", 10, 22)] {
            for value in [min, min + 1, max] {
                let id = format!("appearance_size:{kind}");
                let down = format!("{id}:decrement");
                let up = format!("{id}:increment");
                let reset = format!("{id}:reset");
                let mut tree = UiTree::new(size_control(kind, value, min, max));
                let frame = tree.layout(Size::new(320.0, 60.0));
                let field = frame.access.iter().find(|node| node.id == id).unwrap();
                assert_eq!(field.role, Role::TextInput);
                assert_eq!(field.name, format!("Tamaño de {target}"));
                assert_eq!(
                    field.value_text.as_deref(),
                    Some(value.to_string().as_str())
                );
                for (button, disabled, label) in [
                    (&down, value == min, "Reducir"),
                    (&up, value == max, "Aumentar"),
                ] {
                    let node = frame.access.iter().find(|node| &node.id == button).unwrap();
                    assert_eq!(node.disabled, disabled);
                    assert!(
                        node.name
                            .starts_with(&format!("{label} tamaño de {target}"))
                    );
                    assert_eq!(tree.focus(button), !disabled);
                }
                let down_rect = frame
                    .access
                    .iter()
                    .find(|node| node.id == down)
                    .unwrap()
                    .rect;
                let up_rect = frame.access.iter().find(|node| node.id == up).unwrap().rect;
                assert!(down_rect.x + down_rect.width <= field.rect.x);
                assert!(field.rect.x + field.rect.width <= up_rect.x);
                let mut expected = vec![down, id, up];
                expected.retain(|button| frame.regions.iter().any(|region| &region.id == button));
                expected.push(reset);
                assert!(tree.focus(&expected[0]));
                for next in expected.into_iter().skip(1) {
                    tree.handle_event(UiEvent::Key {
                        key: Key::Tab,
                        shift: false,
                        ctrl: false,
                    });
                    assert_eq!(tree.focused_id(), Some(next.as_str()));
                }
            }
        }
    }
}
