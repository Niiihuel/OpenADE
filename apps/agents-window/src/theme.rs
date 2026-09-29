//! Semantic tokens for the Agents Window's neutral dark presentation.
//!
//! Layout and token roles are informed by ZCode's `theme-zai-dark` CSS theme
//! (Apache-2.0). The application uses an achromatic palette for native chrome;
//! the composer send button and reasoning-effort control carry local accents.

use openide_ui::{Color, ComponentPalette};

pub const BACKGROUND: Color = Color::rgb(22, 22, 22);
pub const WINDOW: Color = Color::rgb(43, 43, 43);
pub const SIDEBAR: Color = Color::rgb(43, 43, 43);
pub const PANEL: Color = Color::rgb(32, 32, 32);
pub const CARD: Color = Color::rgb(43, 43, 43);
/// Floating action menus share the composer popover's dark surface.
pub const MENU_SURFACE: Color = Color::rgb(30, 30, 30);
pub const MENU_HOVER: Color = Color::rgb(48, 48, 48);
pub const MENU_MUTED: Color = Color::rgb(158, 158, 158);
pub const INPUT: Color = Color::rgb(43, 43, 43);
pub const HOVER: Color = Color::rgb(34, 34, 34);
pub const SELECTED: Color = Color::rgb(54, 54, 54);
pub const BORDER: Color = Color::rgb(46, 46, 46);
pub const TREE_GUIDE: Color = Color::rgb(72, 72, 72);
pub const BRAND: Color = Color::rgb(255, 255, 255);
pub const FOREGROUND: Color = Color::rgb(212, 212, 212);
pub const FOREGROUND_SUBTLE: Color = Color::rgb(136, 136, 136);
pub const PRIMARY_FOREGROUND: Color = Color::rgb(0, 0, 0);
pub const USER: Color = Color::rgb(235, 235, 235);
pub const ASSISTANT: Color = Color::rgb(212, 212, 212);
pub const TOOL: Color = Color::rgb(184, 184, 184);
pub const TOOL_RESULT: Color = Color::rgb(162, 162, 162);
pub const DESTRUCTIVE: Color = Color::rgb(255, 92, 92);
pub const DIFF_ADDED: Color = Color::rgb(139, 207, 163);
pub const DIFF_REMOVED: Color = Color::rgb(224, 145, 145);
pub const DIFF_ADDED_BG: Color = Color::rgb(29, 49, 37);
pub const DIFF_REMOVED_BG: Color = Color::rgb(55, 35, 35);
pub const DIFF_HUNK_BG: Color = Color::rgb(39, 39, 39);
// Syntax color is confined to file contents. The surrounding native chrome
// remains achromatic, including the file preview's surface and gutter.
pub const CODE_SURFACE: Color = Color::rgb(25, 25, 25);
pub const CODE_GUTTER: Color = Color::rgb(108, 108, 108);
pub const CODE_KEYWORD: Color = Color::rgb(191, 177, 200);
pub const CODE_STRING: Color = Color::rgb(168, 192, 166);
pub const CODE_NUMBER: Color = Color::rgb(199, 182, 154);
pub const CODE_COMMENT: Color = Color::rgb(118, 118, 118);
pub const CODE_PUNCTUATION: Color = Color::rgb(159, 159, 159);
pub const ICON_PRIMARY: Color = Color::rgb(224, 224, 224);
/// Accent tones used only by the reasoning-effort control.
pub const EFFORT_BLUE: Color = Color::rgb(80, 145, 255);
pub const EFFORT_ULTRA: Color = Color::rgb(190, 126, 255);

pub const SIDEBAR_WIDTH: f32 = 264.0;
pub const UI_FONT_SIZE: f32 = 14.0;

/// Hover moves one neutral step above the control's own surface. A fixed
/// global hover color would darken the sidebar while brightening the canvas.
pub fn hover_for(surface: Color) -> Color {
    if surface == BRAND || surface == DESTRUCTIVE {
        return surface;
    }
    Color::rgba(
        surface.r.saturating_add(8),
        surface.g.saturating_add(8),
        surface.b.saturating_add(8),
        surface.a,
    )
}

pub fn action_palette(surface: Color, ink: Color) -> ComponentPalette {
    ComponentPalette {
        surface,
        raised: surface,
        hover: hover_for(surface),
        selected: if surface == BRAND || surface == DESTRUCTIVE {
            surface
        } else {
            SELECTED
        },
        border: BORDER,
        text: ink,
        muted: FOREGROUND_SUBTLE,
        shadow: Color::rgba(0, 0, 0, 125),
    }
}

pub fn component_palette() -> ComponentPalette {
    ComponentPalette {
        surface: BACKGROUND,
        raised: CARD,
        hover: hover_for(SIDEBAR),
        selected: SELECTED,
        border: BORDER,
        text: FOREGROUND,
        muted: FOREGROUND_SUBTLE,
        shadow: Color::rgba(0, 0, 0, 125),
    }
}

pub fn menu_palette() -> ComponentPalette {
    ComponentPalette {
        surface: MENU_SURFACE,
        raised: MENU_SURFACE,
        hover: MENU_HOVER,
        selected: MENU_HOVER,
        border: BORDER,
        text: FOREGROUND,
        muted: MENU_MUTED,
        shadow: Color::rgba(0, 0, 0, 125),
    }
}

pub fn segmented_palette() -> ComponentPalette {
    ComponentPalette {
        surface: SELECTED,
        raised: SELECTED,
        hover: Color::rgb(63, 63, 63),
        selected: BACKGROUND,
        border: BORDER,
        text: FOREGROUND,
        muted: FOREGROUND_SUBTLE,
        shadow: Color::rgba(0, 0, 0, 125),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Button, Component, Element, ElementKind, Tooltip, TooltipStyle};

    fn tooltip_style(element: Element) -> TooltipStyle {
        let ElementKind::Tooltip { colors, .. } = element.kind else {
            panic!("expected a shared tooltip");
        };
        colors
    }

    #[test]
    fn native_chrome_has_no_hue() {
        for color in [
            BACKGROUND,
            WINDOW,
            SIDEBAR,
            PANEL,
            CARD,
            MENU_SURFACE,
            MENU_HOVER,
            INPUT,
            HOVER,
            SELECTED,
            BORDER,
            BRAND,
            FOREGROUND,
            FOREGROUND_SUBTLE,
            PRIMARY_FOREGROUND,
            USER,
            ASSISTANT,
            TOOL,
            TOOL_RESULT,
            ICON_PRIMARY,
        ] {
            assert_eq!((color.r, color.g), (color.g, color.b));
        }
    }

    #[test]
    fn hover_progresses_from_each_surface_without_changing_geometry() {
        assert!(hover_for(SIDEBAR).r > SIDEBAR.r);
        assert!(hover_for(BACKGROUND).r > BACKGROUND.r);
        assert!(hover_for(SIDEBAR).r < SELECTED.r);
        assert_eq!(hover_for(BRAND), BRAND);
    }

    #[test]
    fn menu_palette_uses_one_composer_surface_without_changing_regular_controls() {
        let menu = menu_palette();
        assert_eq!(menu.surface, MENU_SURFACE);
        assert_eq!(menu.raised, MENU_SURFACE);
        assert_eq!(menu.hover, MENU_HOVER);
        assert_ne!(component_palette().surface, menu.surface);
        assert_ne!(component_palette().raised, menu.raised);
    }

    #[test]
    fn native_tooltip_style_is_independent_of_action_surface_and_ink() {
        let base = tooltip_style(
            Tooltip::new(
                Element::button("base", "Ayuda"),
                "base",
                "Ayuda",
                component_palette(),
            )
            .render(),
        );
        for (surface, ink) in [
            (SIDEBAR, FOREGROUND),
            (PANEL, FOREGROUND_SUBTLE),
            (BRAND, PRIMARY_FOREGROUND),
            (DESTRUCTIVE, BRAND),
        ] {
            let action = Button::new("action", "Ayuda", action_palette(surface, ink))
                .tooltip("Ayuda")
                .render();
            assert_eq!(tooltip_style(action), base);
        }
        assert!(base.background.r < PANEL.r);
        assert!(base.border.r > BORDER.r);
    }
}
