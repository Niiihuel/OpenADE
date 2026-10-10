//! The comparison used by the parity tests of c03-converge-panels-forms-menus
//! and the shapes that need no fixture: the same draw commands and regions
//! idle and, over every target, hovered, with the hint open and pressed, and
//! the same action on click.

use std::time::{Duration, Instant};

use crate::{theme, ui_sizing};
use openide_ui::{
    Alert, AlertTone, Button, ButtonGroup, ButtonGroupSizing, ButtonVariant, Component,
    ControlSize, DrawCommand, Element, FormField, Frame, Point, PopoverPanel, ScrollArea, Separator, Size,
    Text, TextField, TextRole, UiAction, UiEvent, UiTree,
};

/// The idle frame, then three frames and a click for each target in it.
pub(crate) fn states(element: Element, width: f32) -> (Vec<Frame>, Vec<Vec<UiAction>>) {
    let size = Size::new(width, 700.0);
    let mut tree = UiTree::new(Element::column(vec![
        Element::spacer().size(None, Some(100.0)),
        element,
        Element::spacer().flex(1.0),
    ]));
    tree.set_reduced_motion(true);
    let idle = tree.layout(size);
    let targets: Vec<_> = idle.regions.iter().map(|region| region.rect).collect();
    let (mut frames, mut actions) = (vec![idle], Vec::new());
    for target in targets {
        let point = Point::new(
            target.x + target.width / 2.0,
            target.y + target.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerMove(point));
        frames.push(tree.layout(size));
        frames.push(tree.layout_at(size, Instant::now() + Duration::from_secs(2)));
        tree.handle_event(UiEvent::PointerDown(point));
        frames.push(tree.layout(size));
        actions.push(tree.handle_event(UiEvent::PointerUp(point)));
    }
    (frames, actions)
}

pub(crate) fn assert_same_at(name: &str, widths: &[f32], legacy: Element, current: Element) {
    compare(name, widths, legacy, current, true);
}

/// The same comparison without the clip brackets, for a row whose content
/// is cut to a box two pixels taller than the one it replaced. Everything
/// drawn inside, every region and every click must still be equal.
pub(crate) fn assert_same_paint_at(name: &str, widths: &[f32], legacy: Element, current: Element) {
    compare(name, widths, legacy, current, false);
}

fn compare(name: &str, widths: &[f32], legacy: Element, current: Element, clips: bool) {
    let drawn = |frame: &Frame| -> Vec<DrawCommand> {
        frame
            .commands
            .iter()
            .filter(|command| {
                clips || !matches!(command, DrawCommand::PushClip(_) | DrawCommand::PopClip)
            })
            .cloned()
            .collect()
    };
    for &width in widths {
        let (legacy_frames, legacy_actions) = states(legacy.clone(), width);
        let (frames, actions) = states(current.clone(), width);
        assert_eq!(frames.len(), legacy_frames.len(), "{name} width={width}");
        for (index, (frame, legacy)) in frames.iter().zip(&legacy_frames).enumerate() {
            let (commands, legacy_commands) = (drawn(frame), drawn(legacy));
            // Name the first command that differs; a whole frame is unreadable.
            for (command, legacy) in commands.iter().zip(&legacy_commands) {
                assert_eq!(command, legacy, "{name} width={width} frame={index}");
            }
            assert_eq!(commands.len(), legacy_commands.len(), "{name} width={width} frame={index}");
            assert_eq!(frame.regions, legacy.regions, "{name} width={width} frame={index}");
            if clips {
                assert_eq!(frame, legacy, "{name} width={width} frame={index}");
            }
        }
        assert_eq!(actions, legacy_actions, "{name} width={width} clicks");
    }
}

fn action(id: &str, label: &str) -> Element {
    Button::new(id, label, theme::component_palette())
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .render()
}

#[test]
fn the_comparison_sees_a_gap_a_hover_fill_and_a_click() {
    let row = |gap| Element::row(vec![action("a", "Cancelar"), action("b", "Guardar")]).gap(gap);
    let (frames, actions) = states(row(8.0), 360.0);
    assert_eq!(frames.len(), 7, "idle and three frames for each button");
    assert_ne!(frames[0], frames[1], "hover paints a fill");
    assert_ne!(frames[1], frames[3], "press paints another fill");
    assert_eq!(actions[1], vec![UiAction::Clicked { id: "b".into() }]);
    assert_ne!(frames[0], states(row(6.0), 360.0).0[0]);
}

#[test]
fn modal_footers_match_their_hand_built_rows() {
    let pair = || vec![action("cancel", "Cancelar"), action("save", "Guardar cambios")];
    let row = |gap: f32| {
        let mut children = vec![Element::spacer().flex(1.0)];
        children.extend(pair());
        Element::row(children).gap(gap)
    };
    let group = |sizing: ButtonGroupSizing| ButtonGroup::new(pair()).align_end().sizing(sizing);
    let widths = [240.0, 360.0, 900.0];
    // The hook, rule and MCP editors: a row, or a column led by the primary action.
    assert_same_at(
        "form footer",
        &widths,
        row(8.0),
        group(ui_sizing::FORM_FOOTER).reverse_when_stacked(true).render(),
    );
    assert_same_at(
        "form footer, stacked",
        &widths,
        Element::column(pair().into_iter().rev().collect()).gap(6.0),
        group(ui_sizing::FORM_FOOTER)
            .stacked(true)
            .reverse_when_stacked(true)
            .render(),
    );
    // The new file dialog, the source form and both provider modals.
    assert_same_at("modal footer", &widths, row(8.0), group(ui_sizing::MODAL_FOOTER).render());
    // The two reset dialogs, the external approval and the account chooser.
    assert_same_at("confirm footer", &widths, Element::row({
        let mut children = vec![Element::spacer().flex(1.0)];
        children.extend(pair());
        children
    }), group(ui_sizing::CONFIRM_FOOTER).render());
    assert_same_at(
        "one action",
        &widths,
        Element::row(vec![Element::spacer().flex(1.0), action("cancel", "Cancelar")]),
        ButtonGroup::new(vec![action("cancel", "Cancelar")])
            .align_end()
            .sizing(ui_sizing::CONFIRM_FOOTER)
            .render(),
    );
}

#[test]
fn a_form_input_matches_its_resized_text_field() {
    let field = || {
        TextField::new("id", "valor", "ej. filesystem", theme::component_palette())
            .size(ControlSize::Compact)
            .monospace(true)
    };
    for width in [186.0, 506.0] {
        let legacy = field().render().size(Some(width), Some(40.0));
        let current = field().width(width).height(40.0).render();
        assert_eq!(format!("{current:?}"), format!("{legacy:?}"));
        assert_same_at("form input", &[360.0, 900.0], legacy, current);
    }
}

#[test]
fn the_source_form_matches_its_hand_built_rows() {
    let palette = theme::component_palette();
    let field = || {
        TextField::new("marketplace_source_draft", "", "https://github.com/usuario/repositorio", palette)
            .size(ControlSize::Compact)
            .render()
    };
    let hint = "Usa una URL de GitHub que contenga archivos SKILL.md.";
    let status = || {
        Alert::new("Aviso de la fuente", "No se pudo leer el repositorio.", palette)
            .tone(AlertTone::Error)
            .compact(true)
            .render()
    };
    for with_status in [false, true] {
        let mut legacy = vec![
            Text::new("URL del repositorio", palette).render(),
            field(),
            Text::new(hint, palette).role(TextRole::Small).muted().render(),
        ];
        let mut current = FormField::new("URL del repositorio", field(), palette).hint(hint).rows();
        if with_status {
            legacy.push(status());
            current.push(status());
        }
        let (legacy, current) = (Element::column(legacy).gap(8.0), Element::column(current).gap(8.0));
        assert_eq!(format!("{current:?}"), format!("{legacy:?}"));
        assert_same_at("source form", &[360.0, 900.0], legacy, current);
    }
}

#[test]
fn the_context_summary_frame_matches_its_hand_built_column() {
    let header = || Text::new("Uso de contexto", theme::component_palette()).render().size(None, Some(24.0));
    let body = |lines: usize| {
        ScrollArea::new(
            "composer_context_summary",
            Element::column(
                (0..lines)
                    .map(|line| Text::new(format!("Línea {line}"), theme::component_palette()).render())
                    .collect(),
            )
            .gap(5.0),
        )
        .render()
    };
    let footer = || {
        ButtonGroup::new(vec![action("details", "Detalles")])
            .align_end()
            .leading(action("compact", "Compactar"))
            .render()
            .size(None, Some(28.0))
    };
    for lines in [3, 40] {
        let legacy = Element::column(vec![
            header(),
            body(lines).max_height(180.0).flex(1.0),
            Separator::horizontal(theme::component_palette()).render(),
            footer(),
        ])
        .gap(8.0)
        .padding(10.0);
        let current = PopoverPanel::new(theme::component_palette())
            .header(header())
            .body(body(lines))
            .body_max_height(180.0)
            .footer(footer())
            .sizing(ui_sizing::CONTEXT_POPOVER)
            .render();
        assert_eq!(format!("{current:?}"), format!("{legacy:?}"));
        assert_same_at("context summary", &[280.0, 360.0], legacy, current);
    }
}
