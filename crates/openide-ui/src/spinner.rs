//! A centered, passive progress indicator with a stable animation phase.

use crate::{Color, Component, Element, SpinnerVariant};

/// A rotating outlined icon marks ongoing work without implying a completion time.
/// Reuse the ID while rebuilding the tree so the animation does not restart.
pub struct Spinner {
    id: String,
    color: Color,
    side: f32,
    variant: SpinnerVariant,
}

impl Spinner {
    pub fn new(id: impl Into<String>, color: Color) -> Self {
        Self {
            id: id.into(),
            color,
            side: 16.0,
            variant: SpinnerVariant::Circle,
        }
    }

    pub fn size(mut self, side: f32) -> Self {
        if side.is_finite() {
            self.side = side.max(8.0);
        }
        self
    }

    pub fn variant(mut self, variant: SpinnerVariant) -> Self {
        self.variant = variant;
        self
    }
}

impl Component for Spinner {
    fn render(self) -> Element {
        Element::spinner(self.id, self.color, self.variant).size(Some(self.side), Some(self.side))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, ElementKind, Size, UiTree};

    #[test]
    fn spinner_is_passive_animated_and_static_under_reduced_motion() {
        let element = Spinner::new("search-progress", Color::rgb(220, 220, 220))
            .size(32.0)
            .render();
        assert!(matches!(element.kind, ElementKind::Spinner { .. }));
        let mut tree = UiTree::new(element);
        let first = tree.layout(Size::new(32.0, 32.0));
        assert!(first.regions.is_empty());
        assert_eq!(spinners(&first).len(), 1);
        assert!(tree.is_animating());

        tree.set_reduced_motion(true);
        let static_first = tree.layout(Size::new(32.0, 32.0));
        let static_next = tree.layout(Size::new(32.0, 32.0));
        assert_eq!(spinners(&static_first), spinners(&static_next));
        assert!(!tree.is_animating());

        tree.set_reduced_motion(false);
        tree.set_root(Element::spacer());
        tree.layout(Size::new(32.0, 32.0));
        assert!(!tree.is_animating());
    }

    #[test]
    fn spinner_supports_both_outline_variants() {
        for variant in [SpinnerVariant::Circle, SpinnerVariant::Spokes] {
            let frame = UiTree::new(
                Spinner::new("loading", Color::WHITE)
                    .variant(variant)
                    .render(),
            )
            .layout(Size::new(16.0, 16.0));
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Spinner { variant: painted, .. } if *painted == variant
            )));
        }
    }

    fn spinners(frame: &crate::Frame) -> Vec<f32> {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Spinner { angle, .. } => Some(*angle),
                _ => None,
            })
            .collect()
    }
}
