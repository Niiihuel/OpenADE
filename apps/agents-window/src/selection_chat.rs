//! The control over a selection in the code editor, and what a selection
//! becomes for an agent CLI. Inline edit (Ctrl+K) is a separate feature.
use crate::{theme, view::ViewState};
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, Element, Kbd, Point, Rect,
};

pub(crate) const ACTION: &str = "editor_add_selection";
pub(crate) const SHORTCUT: &str = "Ctrl+L";
pub(crate) const WIDTH: f32 = 172.0;
pub(crate) const HEIGHT: f32 = 30.0;
const GAP: f32 = 4.0;

/// Where the control goes for a caret at the moving end of a selection,
/// inside the editor's visible `body`. `rows` is how many lines that end is
/// below the other one. The control sits outside the selected lines: under
/// the end of a selection made downwards, over one made upwards or within a
/// line, and past the far end when there is no room there.
pub(crate) fn place(body: Rect, caret: Rect, rows: isize) -> Point {
    // shortcut: the far end is found by counting lines, so wrapped or folded
    // lines misplace it; ask the editor for that rectangle if it matters.
    let span = rows.unsigned_abs() as f32 * caret.height;
    let (top, bottom) = if rows > 0 {
        (caret.y - span, caret.y + caret.height)
    } else {
        (caret.y, caret.y + caret.height + span)
    };
    let above = top - GAP - HEIGHT;
    let below = bottom + GAP;
    let fits_above = above >= body.y;
    let fits_below = below + HEIGHT <= body.y + body.height;
    let y = match (fits_above, fits_below) {
        (true, true) if rows > 0 => below,
        (true, _) => above,
        (false, true) => below,
        // The selection fills the editor: the bottom edge, over its last lines.
        (false, false) => body.y + body.height - HEIGHT - GAP,
    };
    let x = caret.x.min(body.x + body.width - WIDTH).max(body.x);
    Point::new(x, y)
}

/// The text typed into an agent CLI's prompt for a range of a file.
pub(crate) fn cli_reference(template: &str, path: &str, lines: &str) -> String {
    // The space ends the reference; nothing is submitted.
    template.replace("{path}", path).replace("{lines}", lines) + " "
}

pub(crate) fn layer(state: &ViewState, base: Element) -> Element {
    let Some(at) = state.editor_workbench.selection_control else {
        return base;
    };
    let palette = theme::menu_palette();
    let control = Element::row(vec![
        Button::new(ACTION, "Añadir al chat", palette)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render()
            .flex(1.0),
        Kbd::new(SHORTCUT, palette).render().centered_y(),
    ])
    .gap(6.0)
    .padding(3.0)
    .background(palette.raised)
    .border(palette.border, 1.0)
    .radius(openide_ui::tokens().radius.surface)
    .shadow(palette.shadow, Point::new(0.0, 3.0), 10.0, 0.0)
    .size(Some(WIDTH), Some(HEIGHT));
    // Spacers put it at the caret and let the pointer through around it.
    Element::overlay(
        base,
        Element::column(vec![
            Element::spacer().size(None, Some(at.y)),
            Element::row(vec![Element::spacer().size(Some(at.x), None), control])
                .size(None, Some(HEIGHT)),
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_stays_outside_the_selection_and_inside_the_editor() {
        let body = Rect::new(100.0, 50.0, 400.0, 300.0);
        let caret = |x, y| Rect::new(x, y, 1.5, 20.0);
        // Downwards: under the last line. Upwards or in one line: over the first.
        assert_eq!(place(body, caret(200.0, 150.0), 2), Point::new(200.0, 174.0));
        assert_eq!(place(body, caret(200.0, 150.0), -2), Point::new(200.0, 116.0));
        assert_eq!(place(body, caret(200.0, 150.0), 0), Point::new(200.0, 116.0));
        // No room there: past the other end, two lines away, never on a selected line.
        assert_eq!(place(body, caret(200.0, 330.0), 2).y, 330.0 - 40.0 - 34.0);
        assert_eq!(place(body, caret(200.0, 55.0), -2).y, 55.0 + 60.0 + 4.0);
        // Neither side has room: inside the editor, at its bottom.
        assert_eq!(place(body, caret(200.0, 330.0), 14).y, 316.0);
        // The right edge of the editor holds it in.
        assert_eq!(place(body, caret(495.0, 150.0), 1).x, 500.0 - WIDTH);
    }

    #[test]
    fn each_cli_gets_its_own_reference_and_nothing_that_submits() {
        let reference = |id: &str, lines: &str| {
            let (agent, _) = crate::agent_cli_definitions::profile(id).unwrap();
            cli_reference(&agent.selection_reference, "src/main.rs", lines)
        };
        assert_eq!(reference("claude", "12-30"), "@src/main.rs#L12-30 ");
        assert_eq!(reference("opencode", "12"), "@src/main.rs#L12 ");
        assert_eq!(reference("codex", "12-30"), "src/main.rs:12-30 ");
        assert!(!reference("claude", "1").contains(['\r', '\n']));
    }
}
