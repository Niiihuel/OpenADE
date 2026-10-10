//! The comparison used by the parity tests of c02-converge-pages-rows: the
//! same draw commands idle, hovered, with the hint open and pressed, and the
//! same action on click, at two widths.

use std::time::{Duration, Instant};

use openide_ui::{Element, Frame, Point, Size, UiAction, UiEvent, UiTree};

/// Every frame a pointer can produce over the element's first target, and
/// what a click reports. `stretch` lays the element out as wide as the pane.
pub(crate) fn states(element: Element, width: f32, stretch: bool) -> (Vec<Frame>, Vec<UiAction>) {
    let size = Size::new(width, 700.0);
    let mut tree = UiTree::new(Element::column(vec![
        Element::spacer().size(None, Some(100.0)),
        if stretch {
            element
        } else {
            Element::row(vec![element, Element::spacer().flex(1.0)])
        },
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

pub(crate) fn assert_same_at(name: &str, widths: &[f32], legacy: Element, current: Element) {
    for &width in widths {
        for stretch in [false, true] {
            let (legacy_frames, legacy_actions) = states(legacy.clone(), width, stretch);
            let (frames, actions) = states(current.clone(), width, stretch);
            assert_eq!(frames.len(), legacy_frames.len(), "{name} width={width}");
            for (state, (frame, legacy)) in ["idle", "hover", "hint", "pressed"]
                .iter()
                .zip(frames.iter().zip(&legacy_frames))
            {
                assert_eq!(
                    frame, legacy,
                    "{name} width={width} stretch={stretch} {state}"
                );
            }
            assert_eq!(actions, legacy_actions, "{name} width={width} click");
        }
    }
}

pub(crate) fn assert_same(name: &str, legacy: Element, current: Element) {
    assert_same_at(name, &[360.0, 900.0], legacy, current);
}

/// What a frame draws and where it can be hit, as text with every number
/// rounded to a hundredth of a pixel, for a row whose nesting moves a
/// float's last digit (375.99997 for 376).
pub(crate) fn rounded(frame: &Frame) -> String {
    let text = format!("{:?} {:?}", frame.commands, frame.regions);
    let mut out = String::with_capacity(text.len());
    let mut number = String::new();
    for character in text.chars().chain(std::iter::once(' ')) {
        if character.is_ascii_digit() || character == '.' {
            number.push(character);
            continue;
        }
        match number.parse::<f64>() {
            Ok(value) if number.contains('.') => out.push_str(&format!("{value:.2}")),
            _ => out.push_str(&number),
        }
        number.clear();
        out.push(character);
    }
    out
}
