//! The private rule and centering helpers that limn's `Separator`,
//! `centered_y`, `centered_x`, `centered` and `clip_to` replaced, kept so the
//! replacements stay provably identical. Delete with the final audit (S18).

use crate::theme;
use openide_ui::{Component, Element, Icon, Separator, Size, UiTree};

fn fill() -> Element {
    Element::spacer().flex(1.0)
}

fn legacy_divider() -> Element {
    Element::spacer()
        .size(None, Some(1.0))
        .background(theme::BORDER)
}

fn legacy_separator(width: f32) -> Element {
    Element::spacer()
        .background(theme::BORDER)
        .size(Some(width), Some(1.0))
}

fn legacy_workspace_divider() -> Element {
    Element::spacer()
        .size(Some(1.0), None)
        .background(theme::BORDER)
}

fn legacy_centered_rule(length: f32, slot: f32) -> Element {
    Element::column(vec![
        fill(),
        Element::spacer()
            .background(theme::BORDER)
            .size(Some(1.0), Some(length)),
        fill(),
    ])
    .size(Some(1.0), Some(slot))
}

fn legacy_padded_rule(length: f32, slot: f32) -> Element {
    Element::row(vec![
        Element::spacer().size(Some(4.0), None),
        legacy_centered_rule(length, slot),
        Element::spacer().size(Some(4.0), None),
    ])
    .size(Some(9.0), Some(slot))
}

fn legacy_vertically_centered(mut control: Element) -> Element {
    let width = control.style.width;
    let flex = control.style.flex;
    control.style.flex = 0.0;
    Element::column(vec![fill(), control, fill()])
        .size(width, None)
        .flex(flex)
}

fn legacy_centered_in(child: Element, width: Option<f32>, height: f32) -> Element {
    Element::column(vec![fill(), child, fill()]).size(width, Some(height))
}

fn legacy_centered_x(element: Element) -> Element {
    Element::row(vec![fill(), element, fill()])
}

fn legacy_row_icon(icon: Icon, color: openide_ui::Color, size: f32) -> Element {
    Element::column(vec![
        fill(),
        Element::icon(icon)
            .foreground(color)
            .size(Some(size), Some(size)),
        fill(),
    ])
    .size(Some(size), Some(24.0))
}

fn legacy_lateral_viewport(content: Element, width: f32, height: f32) -> Element {
    Element::row(vec![content])
        .size(Some(width), Some(height))
        .flex(0.0)
        .clip_children()
}

fn mark(width: f32, height: f32) -> Element {
    Element::spacer()
        .background(theme::BRAND)
        .size(Some(width), Some(height))
}

/// Same element tree, and the same draw commands at a narrow and a wide size.
fn assert_same(name: &str, legacy: Element, current: Element) {
    assert_eq!(format!("{current:?}"), format!("{legacy:?}"), "{name}");
    for width in [360.0, 900.0] {
        // A row and a column parent exercise both the main and the cross axis.
        for in_row in [true, false] {
            let host = |element: Element| {
                let children = vec![element, Element::spacer().flex(1.0)];
                let mut tree = UiTree::new(if in_row {
                    Element::row(children)
                } else {
                    Element::column(children)
                });
                tree.set_reduced_motion(true);
                tree.layout(Size::new(width, 120.0))
            };
            assert_eq!(
                host(current.clone()),
                host(legacy.clone()),
                "{name} width={width} in_row={in_row}"
            );
        }
    }
}

#[test]
fn limn_separator_matches_every_private_rule_helper() {
    let palette = theme::component_palette();
    assert_same(
        "divider",
        legacy_divider(),
        Separator::horizontal(palette).render(),
    );
    assert_same(
        "separator(width)",
        legacy_separator(212.0),
        Separator::horizontal(palette).length(212.0).render(),
    );
    assert_same(
        "workspace_divider",
        legacy_workspace_divider(),
        Separator::vertical(palette).render(),
    );
    assert_same(
        "bare vertical rule",
        Element::spacer()
            .background(theme::BORDER)
            .size(Some(1.0), Some(16.0)),
        Separator::vertical(palette).length(16.0).render(),
    );
    assert_same(
        "titlebar rule",
        legacy_centered_rule(16.0, 24.0),
        Separator::vertical(palette)
            .length(16.0)
            .centered_in(24.0)
            .render(),
    );
    assert_same(
        "tool group rule",
        legacy_padded_rule(14.0, 24.0),
        Separator::vertical(palette)
            .length(14.0)
            .centered_in(24.0)
            .margin(4.0)
            .render()
            .size(Some(9.0), Some(24.0)),
    );
    assert_same(
        "git stats rule",
        legacy_centered_rule(12.0, 24.0)
            .padding(4.0)
            .size(Some(9.0), Some(24.0)),
        Separator::vertical(palette)
            .length(12.0)
            .centered_in(24.0)
            .render()
            .padding(4.0)
            .size(Some(9.0), Some(24.0)),
    );
    assert_same(
        "tree guide rule",
        Element::spacer()
            .background(theme::TREE_GUIDE)
            .size(None, Some(1.0)),
        Separator::horizontal(palette)
            .ink(theme::TREE_GUIDE)
            .render(),
    );
}

#[test]
fn limn_centering_matches_every_private_centering_helper() {
    for control in [
        mark(40.0, 12.0),
        mark(40.0, 12.0).flex(1.0),
        Element::text("Activa"),
    ] {
        assert_same(
            "vertically_centered",
            legacy_vertically_centered(control.clone()),
            control.centered_y(),
        );
    }
    assert_same(
        "command center centered",
        legacy_centered_in(mark(40.0, 12.0), None, 30.0),
        mark(40.0, 12.0).centered_y().size(None, Some(30.0)),
    );
    assert_same(
        "editor review centered",
        legacy_centered_in(mark(40.0, 12.0), Some(64.0), 24.0),
        mark(40.0, 12.0)
            .centered_y()
            .size(Some(64.0), Some(24.0)),
    );
    assert_same(
        "editor review row",
        legacy_centered_in(
            Element::row(vec![mark(40.0, 12.0)]).size(None, Some(24.0)),
            Some(180.0),
            36.0,
        ),
        Element::row(vec![mark(40.0, 12.0)])
            .size(None, Some(24.0))
            .centered_y()
            .size(Some(180.0), Some(36.0)),
    );
    assert_same(
        "browser centered",
        legacy_centered_x(mark(40.0, 12.0)),
        mark(40.0, 12.0).centered_x(),
    );
    assert_same(
        "both axes",
        Element::column(vec![fill(), legacy_centered_x(mark(40.0, 12.0)), fill()]),
        mark(40.0, 12.0).centered(),
    );
    assert_same(
        "row_icon",
        legacy_row_icon(Icon::ChevronDown, theme::TOOL_RESULT, 12.0),
        Element::icon(Icon::ChevronDown)
            .foreground(theme::TOOL_RESULT)
            .size(Some(12.0), Some(12.0))
            .centered_y()
            .size(Some(12.0), Some(24.0)),
    );
    assert_same(
        "lateral_viewport",
        legacy_lateral_viewport(mark(300.0, 60.0), 80.0, 60.0),
        mark(300.0, 60.0).clip_to(80.0, 60.0),
    );
}
