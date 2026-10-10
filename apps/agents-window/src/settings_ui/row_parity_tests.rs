//! The hand-built settings rows that limn's `ActionRow`, `Empty::framed`
//! and `Text` replaced, kept so the replacements stay provably identical.
//! Delete with the final audit (S18).

use super::*;
use openide_ui::{Size, Switch, UiTree};

const LONG: &str = "Personalizado: modificar archivos está activo; ejecutar comandos está desactivado. Puedes ajustar los permisos por separado.";

fn legacy_label(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}

fn legacy_settings_row(title: &str, description: &str, control: Element, compact: bool) -> Element {
    let copy = Element::column(vec![
        legacy_label(title, theme::FOREGROUND, 12.0),
        legacy_label(description, theme::SETTINGS_FOREGROUND_SUBTLE, 11.0),
    ])
    .gap(4.0);
    let stack_control = compact && !control.style.width.is_some_and(|width| width <= 64.0);
    let content = if stack_control {
        Element::column(vec![copy, control]).gap(8.0)
    } else {
        Element::row(vec![copy.flex(1.0), control.centered_y()])
            .gap(16.0)
            .min_size(0.0, 34.0)
    };
    content.padding(10.0)
}

fn legacy_value_badge(value: &str) -> Element {
    legacy_label(value, theme::SETTINGS_FOREGROUND_SUBTLE, 12.0)
}

fn legacy_page_note(title: &str, description: &str, icon: Icon) -> Element {
    Element::row(vec![
        Element::icon(icon)
            .foreground(theme::SETTINGS_FOREGROUND_SUBTLE)
            .size(Some(16.0), Some(16.0)),
        Element::column(vec![
            legacy_label(title, theme::FOREGROUND, 12.0),
            legacy_label(description, theme::SETTINGS_FOREGROUND_SUBTLE, 11.0),
        ])
        .gap(5.0)
        .flex(1.0),
    ])
    .gap(10.0)
    .padding(10.0)
}

fn legacy_provider_value(label: &str, val: &str) -> Element {
    ActionRow::new(label, control_palette())
        .suffix(legacy_label(val, theme::FOREGROUND_SUBTLE, 12.0))
        .render()
}

fn legacy_empty_state(icon: Icon, title: &str, description: &str, action: Option<Element>) -> Element {
    let mut palette = theme::component_palette();
    palette.muted = theme::SETTINGS_FOREGROUND_SUBTLE;
    let mut empty = openide_ui::Empty::new(title, description, palette)
        .icon(icon)
        .compact();
    if let Some(action) = action {
        empty = empty.action_element(action);
    }
    Element::panel(empty.render())
        .background(theme::CARD)
        .border(theme::BORDER, 1.0)
        .radius(8.0)
}

/// The same draw commands and hit regions at a narrow and a wide size.
fn assert_same(name: &str, legacy: Element, current: Element) {
    for width in [360.0, 900.0] {
        let frame = |element: Element| {
            let mut tree = UiTree::new(Element::column(vec![element, Element::spacer().flex(1.0)]));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(width, 600.0))
        };
        assert_eq!(frame(current.clone()), frame(legacy.clone()), "{name} width={width}");
    }
}

fn controls() -> Vec<(&'static str, Element)> {
    vec![
        ("switch", Switch::new("write_mode", false, control_palette()).render()),
        ("disabled switch", Switch::new("write_mode", true, control_palette()).enabled(false).render()),
        ("button", outline("open_automations", "Abrir automatizaciones").icon(Icon::ArrowRight).render()),
        ("260 wide", field("settings_field", "valor", "Escribe").size(Some(260.0), None)),
        ("flexible", field("settings_field", "valor", "Escribe").flex(1.0)),
        ("value", legacy_value_badge("3 activas · 5 instaladas")),
    ]
}

#[test]
fn settings_row_matches_the_hand_built_row() {
    for compact in [false, true] {
        for (title, description) in [("Tema", "Tema activo"), ("Activar ambos permisos", LONG), (LONG, LONG), ("Tema", "")] {
            for (name, control) in controls() {
                let legacy = legacy_settings_row(title, description, control.clone(), compact);
                let current = settings_parts::settings_row(title, description, control, compact);
                // The element trees are equal, not only what they paint.
                assert_eq!(format!("{current:?}"), format!("{legacy:?}"), "{name} compact={compact}");
                assert_same(&format!("{name} compact={compact} {title:?}"), legacy, current);
            }
        }
    }
}

#[test]
fn value_and_switch_rows_match_the_hand_built_row() {
    for compact in [false, true] {
        for description in ["Instrucciones disponibles por usuario o por proyecto.", LONG, ""] {
            assert_same(
                "value",
                legacy_settings_row("Skills instaladas", description, legacy_value_badge("3 activas"), compact),
                settings_parts::value_row("Skills instaladas", description, "3 activas", compact),
            );
            for (on, enabled) in [(false, true), (true, true), (true, false)] {
                let mut switch = Switch::new("settings_vim_toggle", on, control_palette());
                if !enabled {
                    switch = switch.enabled(false);
                }
                assert_same(
                    "switch",
                    legacy_settings_row("Modo Vim", description, switch.render(), compact),
                    settings_parts::switch_row("settings_vim_toggle", "Modo Vim", description, on, enabled, compact),
                );
            }
        }
    }
    // A status beside an entity's name is the same muted text.
    assert_eq!(
        format!("{:?}", Text::new("Conectado", theme::card_palette()).muted().render()),
        format!("{:?}", legacy_value_badge("Conectado"))
    );
}

#[test]
fn page_note_matches_the_hand_built_note() {
    for (title, description) in [("Sin hooks", "Agrega acciones."), ("Plan y Build", LONG), (LONG, LONG), ("Sin reglas", "")] {
        let legacy = legacy_page_note(title, description, Icon::Terminal);
        let current = page_note(title, description, Icon::Terminal);
        assert_same(&format!("note {title:?}"), legacy, current);
    }
}

#[test]
fn provider_value_row_matches_the_suffix_text() {
    for (label, val) in [("Contexto", "200000 tokens"), ("Salida estructurada", "Dato no disponible")] {
        assert_same(label, legacy_provider_value(label, val), providers::value(label, val));
    }
}

#[test]
fn empty_state_matches_the_hand_built_card() {
    for action in [None, Some(outline("plugin_install", "Elegir carpeta").render())] {
        for description in ["Agrega un elemento.", LONG] {
            assert_same(
                "empty",
                legacy_empty_state(Icon::Puzzle, "Todavía no hay plugins instalados", description, action.clone()),
                settings_parts::empty_state_with_icon(Icon::Puzzle, "Todavía no hay plugins instalados", description, action.clone()),
            );
        }
    }
}
