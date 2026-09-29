//! Shared menu primitives for popovers and context menus.
//!
//! The caller owns the menu's open state and the meaning of each action ID.
//! Each item owns one complete hit target, so moving between its icon, label,
//! and trailing hint never creates separate hover patches.

use crate::{Color, Component, ComponentPalette, Element, FontFamily, Icon, Kbd, ICON_SIZE};

/// A full-width menu action with an optional icon, annotation or shortcut.
pub struct MenuItem {
    action_id: String,
    label: String,
    icon: Option<Icon>,
    trailing: Option<String>,
    shortcut: Option<String>,
    width: f32,
    height: f32,
    palette: ComponentPalette,
    destructive: Option<Color>,
}

impl MenuItem {
    pub fn new(
        action_id: impl Into<String>,
        label: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            action_id: action_id.into(),
            label: label.into(),
            icon: None,
            trailing: None,
            shortcut: None,
            width: 220.0,
            height: 36.0,
            palette,
            destructive: None,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn trailing(mut self, trailing: impl Into<String>) -> Self {
        self.trailing = Some(trailing.into());
        self.shortcut = None;
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self.trailing = None;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.max(40.0);
        }
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        if height.is_finite() {
            self.height = height.max(24.0);
        }
        self
    }

    pub fn destructive(mut self, ink: Color) -> Self {
        self.destructive = Some(ink);
        self
    }
}

impl Component for MenuItem {
    fn render(self) -> Element {
        let ink = self.destructive.unwrap_or(self.palette.text);
        let content_height = (self.height - 14.0).max(10.0);
        let mut children = Vec::with_capacity(3);
        if let Some(icon) = self.icon {
            children.push(
                Element::icon(icon)
                    .foreground(ink)
                    .size(Some(ICON_SIZE), Some(content_height)),
            );
        }
        children.push(
            Element::truncated_text(self.label)
                .foreground(ink)
                .font_family(FontFamily::Sans)
                .font_size(13.0)
                .size(None, Some(content_height))
                .flex(1.0),
        );
        if let Some(shortcut) = self.shortcut {
            children.push(Kbd::new(shortcut, self.palette).render());
        } else if let Some(trailing) = self.trailing {
            children.push(
                Element::text(trailing)
                    .foreground(self.palette.muted)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .size(None, Some(content_height)),
            );
        }
        Element::clickable(self.action_id, Element::row(children).gap(10.0))
            .hover_background(self.palette.hover)
            .pressed_background(self.palette.selected)
            .transition_ms(0)
            .foreground(ink)
            .padding(7.0)
            .radius(7.0)
            .size(Some(self.width), Some(self.height))
    }
}

/// A quiet separator that retains the menu's inner horizontal inset.
pub struct MenuSeparator {
    width: f32,
    color: Color,
}

impl MenuSeparator {
    pub fn new(width: f32, color: Color) -> Self {
        Self { width, color }
    }
}

impl Component for MenuSeparator {
    fn render(self) -> Element {
        Element::panel(Element::spacer().background(self.color))
            .padding(5.0)
            .size(Some(self.width), Some(11.0))
    }
}

#[cfg(test)]
mod tests {
    use crate::{DrawCommand, Point, Size, UiEvent, UiTree};

    use super::*;

    #[test]
    fn whole_menu_item_is_one_hover_and_click_target() {
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(
            MenuItem::new("rename", "Renombrar", palette)
                .icon(Icon::Edit)
                .shortcut("Ctrl+R")
                .width(236.0)
                .render(),
        );
        tree.set_reduced_motion(true);
        let initial = tree.layout(Size::new(236.0, 36.0));
        assert!(initial.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, font_family: FontFamily::Monospace, .. }
                if text == "Ctrl+R"
        )));
        for x in [8.0, 42.0, 205.0] {
            tree.handle_event(UiEvent::PointerMove(Point::new(x, 18.0)));
            let frame = tree.layout(Size::new(236.0, 36.0));
            assert_eq!(frame.hit_test(Point::new(x, 18.0)), Some("rename"));
            assert!(frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::FillRect { color, rect, .. }
                    if *color == palette.hover && rect.width == 236.0
            )));
        }
    }

    #[test]
    fn hovering_one_menu_item_does_not_highlight_its_sibling() {
        let palette = ComponentPalette::neutral_dark();
        let items = Element::column(vec![
            MenuItem::new("rename", "Renombrar", palette)
                .icon(Icon::Edit)
                .width(236.0)
                .render(),
            MenuItem::new("archive", "Archivar", palette)
                .icon(Icon::Archive)
                .width(236.0)
                .render(),
        ]);
        let mut tree = UiTree::new(items);
        tree.set_reduced_motion(true);
        tree.layout(Size::new(236.0, 72.0));
        for (y, id) in [(18.0, "rename"), (54.0, "archive")] {
            tree.handle_event(UiEvent::PointerMove(Point::new(42.0, y)));
            let frame = tree.layout(Size::new(236.0, 72.0));
            assert_eq!(frame.hit_test(Point::new(42.0, y)), Some(id));
            assert_eq!(
                frame
                    .commands
                    .iter()
                    .filter(|command| matches!(
                        command,
                        DrawCommand::FillRect { color, rect, .. }
                            if *color == palette.hover && rect.width == 236.0
                    ))
                    .count(),
                1,
            );
        }
    }
}
