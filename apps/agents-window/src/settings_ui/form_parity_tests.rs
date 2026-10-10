//! The hand-built form fields, select triggers and modal footers that limn's
//! `FormField`, `Select` and `ButtonGroup` replaced, kept so the replacements
//! stay provably identical. Delete with the final audit (S18).

use super::*;
use openide_ui::{DropdownList, Frame, Size, UiTree};

const HINT: &str = "Ruta absoluta a un archivo existente. Se ejecuta sin shell; el agente no puede configurar esta ruta.";

fn frame(element: Element, width: f32) -> Frame {
    let mut tree = UiTree::new(Element::column(vec![element, Element::spacer().flex(1.0)]));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, 700.0))
}

fn legacy_mono_input(id: &str, value: &str, placeholder: &str, width: f32) -> Element {
    field(id, value, placeholder)
        .font_family(FontFamily::Monospace)
        .size(Some(width), Some(40.0))
}

fn legacy_field(label: &str, hints: &[&str], control: Element) -> Vec<Element> {
    let mut rows = vec![text(label, theme::FOREGROUND, settings_parts::LABEL_SIZE)];
    rows.extend(hints.iter().map(|hint| subtle(*hint)));
    rows.push(control);
    rows
}

fn current_field(label: &str, hints: &[&str], control: Element) -> Element {
    hints
        .iter()
        .fold(form_field(label, control), |field, hint| field.hint(*hint))
        .render()
}

#[test]
fn a_settings_form_field_matches_the_hand_built_rows() {
    for width in [306.0, 506.0] {
        let input = |legacy: bool| {
            if legacy {
                legacy_mono_input("hook_draft_executable", "/usr/bin/true", "C:\\tools\\hook.exe", width)
            } else {
                mono_field("hook_draft_executable", "/usr/bin/true", "C:\\tools\\hook.exe")
                    .size(Some(width), Some(40.0))
            }
        };
        assert_eq!(format!("{:?}", input(false)), format!("{:?}", input(true)));
        for hints in [&[][..], &[HINT][..], &[HINT, "Ruta absoluta a una carpeta existente."][..]] {
            // Alone, the field is the same element tree as its rows in a column.
            let legacy = Element::column(legacy_field("Ejecutable", hints, input(true))).gap(7.0);
            let current = current_field("Ejecutable", hints, input(false));
            assert_eq!(format!("{current:?}"), format!("{legacy:?}"), "{hints:?}");

            // In a form, the fields nest where the rows used to be siblings.
            let mut rows = legacy_field("Ejecutable", hints, input(true));
            rows.extend(legacy_field("Directorio de trabajo", hints, input(true)));
            rows.push(subtle(HINT));
            let legacy = Element::column(rows).gap(7.0).size(Some(width), None);
            let current = Element::column(vec![
                current_field("Ejecutable", hints, input(false)),
                current_field("Directorio de trabajo", hints, input(false)),
                subtle(HINT),
            ])
            .gap(7.0)
            .size(Some(width), None);
            for viewport in [360.0, 900.0] {
                visual_qa::assert_same_paint_when_nested(
                    &frame(current.clone(), viewport),
                    &frame(legacy.clone(), viewport),
                    &format!("{hints:?} width={width} viewport={viewport}"),
                );
            }
        }
    }
}

#[test]
fn a_stacked_group_editor_matches_the_hand_built_column() {
    let control = || outline("workspace_apply", "Abrir proyecto").render();
    let legacy = Element::column(vec![
        text("Abrir otra carpeta", theme::FOREGROUND, settings_parts::LABEL_SIZE),
        subtle(HINT),
        control(),
    ])
    .gap(8.0)
    .padding(settings_parts::ROW_PADDING);
    let current = form_field("Abrir otra carpeta", control())
        .hint(HINT)
        .sizing(GROUP_FIELD)
        .render()
        .padding(settings_parts::ROW_PADDING);
    assert_eq!(format!("{current:?}"), format!("{legacy:?}"));
}

fn options() -> Vec<DropdownOption> {
    vec![
        DropdownOption::new("choice:a", "Por proyecto").selected(true),
        DropdownOption::new("choice:b", "Cronológica"),
        DropdownOption::new("choice:c", "No disponible").disabled(),
    ]
}

fn legacy_settings_choice_selector(
    state: &ViewState,
    key: &str,
    label: &str,
    options: Vec<DropdownOption>,
) -> Element {
    let sizing = SelectorSizing::COMPACT;
    let trigger = outline(format!("settings_choice_toggle:{key}"), label)
        .trailing_icon(Icon::ChevronDown)
        .width(sizing.min_width)
        .render();
    DropdownList::new(
        trigger,
        options,
        state.settings_choice_menu_open.as_deref() == Some(key),
        "settings_choice_dismiss",
        theme::menu_palette(),
    )
    .sizing(sizing)
    .placement(PopoverPlacement::BottomEnd)
    .render()
    .size(Some(sizing.min_width), Some(ControlSize::Compact.height()))
}

fn assert_same_frames(name: &str, legacy: impl Fn() -> Element, current: impl Fn() -> Element) {
    for viewport in [360.0, 900.0] {
        // Beside a title, as a settings row sets it, and alone in a column.
        let in_row = |control: Element| {
            settings_parts::settings_row("Organización", HINT, control, viewport < 500.0)
        };
        assert_eq!(frame(in_row(current()), viewport), frame(in_row(legacy()), viewport), "{name} row {viewport}");
        assert_eq!(frame(current(), viewport), frame(legacy(), viewport), "{name} {viewport}");
    }
}

#[test]
fn settings_choice_selector_matches_the_hand_built_dropdown() {
    for open in [None, Some("organization".to_owned()), Some("sort".to_owned())] {
        let mut state = crate::view::tests::empty_state();
        state.settings_choice_menu_open = open.clone();
        let legacy = || legacy_settings_choice_selector(&state, "organization", "Por proyecto", options());
        let current = || settings_choice_selector(&state, "organization", "Por proyecto", options());
        // The element trees are equal, not only what they paint.
        assert_eq!(format!("{:?}", current()), format!("{:?}", legacy()), "{open:?}");
        assert_same_frames(&format!("{open:?}"), legacy, current);
    }
}

#[test]
fn the_other_selects_match_their_hand_built_dropdowns() {
    for open in [false, true] {
        // The terminal's shell: a wider trigger and a list wider still.
        let legacy = || {
            let trigger = outline("settings_terminal_shell_toggle", "Bash")
                .trailing_icon(Icon::ChevronDown)
                .width(238.0)
                .render();
            DropdownList::new(trigger, options(), open, "settings_terminal_shell_dismiss", theme::menu_palette())
                .sizing(SelectorSizing::COMPACT)
                .width(254.0)
                .placement(PopoverPlacement::BottomEnd)
                .render()
                .size(Some(238.0), None)
        };
        let current = || {
            Select::new("settings_terminal_shell_toggle", "Bash", options(), open, "settings_terminal_shell_dismiss", control_palette())
                .menu_palette(theme::menu_palette())
                .sizing(SelectorSizing::COMPACT)
                .width(238.0)
                .menu_width(254.0)
                .render()
        };
        assert_same_frames("shell", legacy, current);

        // A modal's selector: as wide as the form, the list under its start,
        // with or without an icon.
        for (palette, icon) in [(control_palette(), None), (theme::component_palette(), Some(Icon::Terminal))] {
            for width in [306.0, 506.0] {
                let legacy = || {
                    let mut trigger = Button::new("selector_toggle", "Copia del evento 4", palette)
                        .variant(ButtonVariant::Outline)
                        .size(ControlSize::Compact)
                        .trailing_icon(Icon::ChevronDown)
                        .width(width);
                    if let Some(icon) = icon {
                        trigger = trigger.icon(icon);
                    }
                    DropdownList::new(trigger.render(), options(), open, "selector_dismiss", theme::menu_palette())
                        .width(width)
                        .placement(PopoverPlacement::BottomStart)
                        .render()
                        .size(Some(width), None)
                };
                let current = || {
                    let mut select = Select::new("selector_toggle", "Copia del evento 4", options(), open, "selector_dismiss", palette)
                        .menu_palette(theme::menu_palette())
                        .width(width)
                        .placement(PopoverPlacement::BottomStart);
                    if let Some(icon) = icon {
                        select = select.icon(icon);
                    }
                    select.render()
                };
                assert_same_frames(&format!("modal selector {width} {open}"), legacy, current);
            }
        }
    }
}

#[test]
fn modal_footers_match_their_hand_built_rows() {
    let buttons = || {
        let cancel = outline("cancel", "Cancelar").render();
        let save = theme::primary("save", "Guardar cambios").size(ControlSize::Compact).render();
        (cancel, save)
    };
    for stacked in [false, true] {
        // The primary action leads the column and ends the row.
        let (cancel, save) = buttons();
        let legacy = if stacked {
            Element::column(vec![save, cancel]).gap(6.0)
        } else {
            Element::row(vec![Element::spacer().flex(1.0), cancel, save]).gap(6.0)
        };
        let (cancel, save) = buttons();
        let current = ButtonGroup::new(vec![cancel, save])
            .align_end()
            .stacked(stacked)
            .reverse_when_stacked(true)
            .render();
        assert_eq!(format!("{current:?}"), format!("{legacy:?}"), "stacked={stacked}");

        // The same order in both directions.
        let (cancel, save) = buttons();
        let legacy = if stacked {
            Element::column(vec![cancel, save]).gap(6.0)
        } else {
            Element::row(vec![Element::spacer().flex(1.0), cancel, save]).gap(6.0)
        };
        let (cancel, save) = buttons();
        let current = ButtonGroup::new(vec![cancel, save]).align_end().stacked(stacked).render();
        assert_eq!(format!("{current:?}"), format!("{legacy:?}"), "stacked={stacked}");
    }
    // A lasting choice apart from the answers.
    let (cancel, save) = buttons();
    let legacy = Element::row(vec![
        outline("remove", "Desinstalar").render(),
        Element::spacer().flex(1.0),
        cancel,
        save,
    ])
    .gap(6.0);
    let (cancel, save) = buttons();
    let current = ButtonGroup::new(vec![cancel, save])
        .leading(outline("remove", "Desinstalar").render())
        .align_end()
        .render();
    assert_eq!(format!("{current:?}"), format!("{legacy:?}"));
}
