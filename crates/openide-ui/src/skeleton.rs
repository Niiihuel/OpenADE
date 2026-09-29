//! Passive loading placeholders with one stable, paint-only pulse.

use crate::{Component, ComponentPalette, Element};

/// A block for content whose shape is known while its data is loading.
/// Reuse `id` across tree rebuilds to retain the pulse phase. Without a width,
/// it fills the available horizontal space.
pub struct Skeleton {
    id: String,
    palette: ComponentPalette,
    width: Option<f32>,
    height: f32,
    radius: f32,
}

impl Skeleton {
    pub fn new(id: impl Into<String>, palette: ComponentPalette) -> Self {
        Self {
            id: id.into(),
            palette,
            width: None,
            height: 12.0,
            radius: 6.0,
        }
    }

    /// A rounded placeholder for one line of text. Pair lines of different
    /// widths to approximate the copy that will replace them.
    pub fn line(id: impl Into<String>, palette: ComponentPalette, width: f32) -> Self {
        Self::new(id, palette).width(width).height(12.0).radius(6.0)
    }

    /// A large content preview, such as a card, image, or result snippet.
    pub fn block(
        id: impl Into<String>,
        palette: ComponentPalette,
        width: f32,
        height: f32,
    ) -> Self {
        Self::new(id, palette)
            .width(width)
            .height(height)
            .radius(12.0)
    }

    /// A circular avatar or icon placeholder beside related text lines.
    pub fn circle(id: impl Into<String>, palette: ComponentPalette, diameter: f32) -> Self {
        let diameter = if diameter.is_finite() {
            diameter.max(1.0)
        } else {
            32.0
        };
        Self::new(id, palette)
            .width(diameter)
            .height(diameter)
            .radius(diameter / 2.0)
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = Some(width.max(0.0));
        }
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        if height.is_finite() {
            self.height = height.max(1.0);
        }
        self
    }

    pub fn radius(mut self, radius: f32) -> Self {
        if radius.is_finite() {
            self.radius = radius.max(0.0);
        }
        self
    }
}

impl Component for Skeleton {
    fn render(self) -> Element {
        Element::skeleton(self.id, self.palette.raised, self.palette.hover)
            .size(self.width, Some(self.height))
            .radius(self.radius)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, ElementKind, Size, UiTree};

    #[test]
    fn skeleton_is_a_passive_surface_that_fills_available_width() {
        let palette = ComponentPalette::neutral_dark();
        let element = Skeleton::new("loading-title", palette)
            .height(14.0)
            .render();
        assert!(matches!(element.kind, ElementKind::Skeleton { .. }));
        let mut tree = UiTree::new(Element::column(vec![element]));
        let frame = tree.layout(Size::new(320.0, 20.0));
        assert!(frame.regions.is_empty());
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { rect, radius, .. }
                if rect.width == 320.0 && rect.height == 14.0 && *radius == 6.0
        )));
        assert!(tree.is_animating());
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(320.0, 20.0));
        assert!(!tree.is_animating());
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == palette.raised
        )));
    }

    #[test]
    fn removing_skeleton_stops_repaint_after_layout() {
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(Element::column(vec![Skeleton::new(
            "temporary-load",
            palette,
        )
        .render()]));
        tree.layout(Size::new(320.0, 40.0));
        assert!(tree.is_animating());
        tree.set_root(Element::spacer());
        tree.layout(Size::new(320.0, 40.0));
        assert!(!tree.is_animating());
    }

    #[test]
    fn semantic_shapes_keep_their_geometry_and_remain_passive() {
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(Element::column(vec![
            Skeleton::block("preview", palette, 250.0, 125.0).render(),
            Skeleton::line("title", palette, 250.0).render(),
            Skeleton::line("description", palette, 200.0).render(),
            Skeleton::circle("avatar", palette, 32.0).render(),
        ]));
        let frame = tree.layout(Size::new(260.0, 200.0));
        assert!(frame.regions.is_empty());
        let sizes = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, radius, .. } => {
                    Some((rect.width, rect.height, *radius))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(sizes.contains(&(250.0, 125.0, 12.0)));
        assert!(sizes.contains(&(250.0, 12.0, 6.0)));
        assert!(sizes.contains(&(200.0, 12.0, 6.0)));
        assert!(sizes.contains(&(32.0, 32.0, 16.0)));
    }
}
