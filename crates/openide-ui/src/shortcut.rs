//! Shared geometry for keyboard shortcut hints across components and painters.

use crate::Color;

pub(crate) const FONT_SIZE: f32 = 11.0;
pub(crate) const HEIGHT: f32 = 22.0;
pub(crate) const HORIZONTAL_PADDING: f32 = 6.0;
pub(crate) const RADIUS: f32 = 4.0;

pub(crate) fn label(value: &str) -> &str {
    value.trim()
}

/// Keep the key cap visible when it sits on a selected control surface.
pub(crate) fn cap_fill(selected: Color) -> Color {
    Color::rgba(
        selected.r.saturating_add(10),
        selected.g.saturating_add(10),
        selected.b.saturating_add(10),
        selected.a,
    )
}
