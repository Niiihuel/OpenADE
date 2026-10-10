//! Shared integrated resource tabs. limn's `TabStrip` draws them; this module
//! keeps the names the docks read and the width estimate, which is the app's.
use openide_ui::TabStrip;

// The active tab reaches the content below. Its 24px contents and the fixed
// toolbar share a center at y=16; the last 4px form the connection to the body.
pub(crate) const HEADER_HEIGHT: f32 = TabStrip::HEADER_HEIGHT;
// Resource strips share the editor's inset, including lower panel sections
// and terminal sessions, so their first tab and trailing tools line up.
pub(crate) const HEADER_SIDE_INSET: f32 = TabStrip::HEADER_SIDE_INSET;
pub(crate) const TOOL_SIZE: f32 = TabStrip::TOOL_SIZE;
pub(crate) const TAB_HEIGHT: f32 = TabStrip::TAB_HEIGHT;
#[cfg(test)]
pub(crate) const CLOSE_SIZE: f32 = crate::ui_sizing::TAB_CLOSE.height;
#[cfg(test)]
pub(crate) const TAB_GAP: f32 = 0.0;
#[cfg(test)]
pub(crate) const STRIP_BACKGROUND: openide_ui::Color = crate::theme::PANEL;
/// Widest tab outside the IDE's editor group, which allows longer file names.
pub(crate) const TAB_MAX_WIDTH: f32 = 208.0;

pub(crate) fn tab_width(title: &str, available: f32, max: f32) -> f32 {
    // This estimate reserves ordinary Sans13 names. The native measurer still
    // owns text paint; long labels fade and the strip scrolls continuously.
    (68.0 + title.chars().count() as f32 * 7.8)
        .clamp(112.0, max)
        .min((available - 32.0).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    #[test]
    fn the_shim_palette_holds_the_colors_the_strip_painted() {
        let palette = theme::tab_strip_palette();
        assert_eq!(
            (palette.surface, palette.raised, palette.hover),
            (theme::BACKGROUND, STRIP_BACKGROUND, theme::HOVER)
        );
        assert_eq!(
            (palette.text, palette.muted, palette.border),
            (theme::FOREGROUND, theme::FOREGROUND_SUBTLE, theme::component_palette().border)
        );
    }
}
