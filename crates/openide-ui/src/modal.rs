//! A reusable centered modal for forms and other multi-step content.

use crate::components::{Component, ComponentPalette};
use crate::{
    Color, Element, FontFamily, Icon, Point, PopoverPlacement, ScrollArea, ScrollbarAppearance,
};

/// A controlled modal with a fixed header and footer. When `height` is set,
/// only the body scrolls, keeping its actions visible in a short viewport.
pub struct Modal {
    base: Element,
    open: bool,
    title: String,
    description: String,
    close_id: String,
    palette: ComponentPalette,
    width: f32,
    height: Option<f32>,
    body: Option<Element>,
    footer: Option<Element>,
    initial_focus: Option<String>,
    return_focus: Option<String>,
    backdrop_dismiss: bool,
    animation_ms: u16,
}

impl Modal {
    pub fn new(
        base: Element,
        open: bool,
        title: impl Into<String>,
        description: impl Into<String>,
        close_id: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            base,
            open,
            title: title.into(),
            description: description.into(),
            close_id: close_id.into(),
            palette,
            width: 520.0,
            height: None,
            body: None,
            footer: None,
            initial_focus: None,
            return_focus: None,
            backdrop_dismiss: false,
            animation_ms: 160,
        }
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.max(240.0);
        }
        self
    }

    /// Set a bounded card height. The body receives the remaining space and
    /// becomes scrollable; the viewport can clamp the card further.
    pub fn height(mut self, height: f32) -> Self {
        if height.is_finite() {
            self.height = Some(height.max(160.0));
        }
        self
    }

    pub fn body(mut self, body: Element) -> Self {
        self.body = Some(body);
        self
    }

    pub fn footer(mut self, footer: Element) -> Self {
        self.footer = Some(footer);
        self
    }

    pub fn initial_focus(mut self, id: impl Into<String>) -> Self {
        self.initial_focus = Some(id.into());
        self
    }

    pub fn return_focus(mut self, id: impl Into<String>) -> Self {
        self.return_focus = Some(id.into());
        self
    }

    pub fn backdrop_dismiss(mut self, enabled: bool) -> Self {
        self.backdrop_dismiss = enabled;
        self
    }

    pub fn animation_ms(mut self, duration: u16) -> Self {
        self.animation_ms = duration;
        self
    }
}

impl Component for Modal {
    fn render(self) -> Element {
        if !self.open {
            return self.base;
        }

        let close = Element::clickable(
            format!("{}:close", self.close_id),
            Element::row(vec![
                Element::spacer().flex(1.0),
                Element::icon(Icon::CloseBold)
                    .foreground(self.palette.muted)
                    .size(Some(16.0), Some(16.0)),
                Element::spacer().flex(1.0),
            ])
            .size(None, Some(30.0)),
        )
        .background(self.palette.surface)
        .hover_background(self.palette.hover)
        .radius(7.0)
        .size(Some(30.0), Some(30.0))
        .dispatch_as(self.close_id.clone());

        let mut sections = vec![Element::row(vec![
            Element::truncated_text(self.title)
                .foreground(self.palette.text)
                .font_family(FontFamily::Sans)
                .font_size(18.0)
                .flex(1.0),
            close,
        ])
        .gap(12.0)
        .size(None, Some(30.0))];

        if !self.description.is_empty() {
            sections.push(
                Element::text(self.description)
                    .foreground(self.palette.muted)
                    .font_family(FontFamily::Sans)
                    .font_size(12.0),
            );
        }

        if let Some(body) = self.body {
            sections.push(if self.height.is_some() {
                ScrollArea::new(format!("modal_body:{}", self.close_id), body)
                    // A dedicated rail keeps the thumb outside bordered form
                    // sections and never covers controls at the right edge.
                    .appearance(
                        ScrollbarAppearance::reserved()
                            .rail_width(14.0)
                            .thumb_width(4.0)
                            .tint(self.palette.muted),
                    )
                    .render()
                    .flex(1.0)
            } else {
                body
            });
        }

        if let Some(footer) = self.footer {
            sections.push(footer);
        }

        let card = Element::panel(Element::column(sections).gap(14.0))
            .padding(20.0)
            .size(Some(self.width), self.height)
            .background(self.palette.surface)
            .border(self.palette.border, 1.0)
            .radius(14.0)
            .shadow(self.palette.shadow, Point::new(0.0, 16.0), 32.0, 0.0)
            .clip_children();

        let mut popover =
            Element::popover(self.base, card, self.close_id, PopoverPlacement::Center)
                .popover_scrim(Color::rgba(0, 0, 0, 180))
                .popover_backdrop_dismiss(self.backdrop_dismiss)
                .popover_animation_ms(self.animation_ms);
        if let Some(id) = self.initial_focus {
            popover = popover.popover_initial_focus(id);
        }
        if let Some(id) = self.return_focus {
            popover = popover.popover_return_focus(id);
        }
        popover
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HitKind, Key, Size, UiAction, UiEvent, UiTree};

    fn sample_modal() -> Element {
        Modal::new(
            Element::button("behind", "Background"),
            true,
            "Add source",
            "Choose a folder.",
            "dismiss",
            ComponentPalette::neutral_dark(),
        )
        .width(420.0)
        .height(300.0)
        .body(Element::column(vec![
            Element::text_input("path", "", "Folder"),
            Element::spacer().size(None, Some(600.0)),
            Element::text_input("later", "", "Later field"),
        ]))
        .footer(Element::button("save", "Save"))
        .initial_focus("path")
        .render()
    }

    #[test]
    fn modal_centers_body_scrolls_and_footer_stays_visible() {
        let mut tree = UiTree::new(sample_modal());
        let frame = tree.layout(Size::new(640.0, 480.0));
        let surface = frame
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:dismiss")
            .expect("modal card");
        assert!((surface.rect.x + surface.rect.width / 2.0 - 320.0).abs() < 1.0);
        assert!((surface.rect.y + surface.rect.height / 2.0 - 240.0).abs() < 1.0);
        assert_eq!(tree.focused_id(), Some("path"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "modal_body:dismiss" && region.kind == HitKind::Scroll));
        let scrollbar = frame
            .regions
            .iter()
            .find(|region| region.id == "modal_body:dismiss" && region.kind == HitKind::Scrollbar)
            .expect("modal scrollbar");
        let field = frame
            .regions
            .iter()
            .find(|region| region.id == "path")
            .unwrap();
        assert!(
            field.rect.x + field.rect.width <= scrollbar.rect.x,
            "scrollbar stays outside the form's available width"
        );
        let footer = frame
            .regions
            .iter()
            .find(|region| region.id == "save")
            .unwrap();
        assert!(footer.rect.y >= surface.rect.y);
        assert!(footer.rect.y + footer.rect.height <= surface.rect.y + surface.rect.height);
    }

    #[test]
    fn modal_traps_focus_blocks_backdrop_and_escape_dismisses() {
        let mut tree = UiTree::new(Element::button("behind", "Background"));
        tree.layout(Size::new(640.0, 480.0));
        assert!(tree.focus("behind"));
        tree.set_root(sample_modal());
        let frame = tree.layout(Size::new(640.0, 480.0));
        assert_eq!(tree.focused_id(), Some("path"));
        assert!(frame
            .regions
            .iter()
            .any(|region| { region.id == "dismiss" && region.kind == HitKind::Surface }));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        assert_ne!(tree.focused_id(), Some("behind"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "dismiss".into()
            }]
        );
        tree.set_root(Element::button("behind", "Background"));
        tree.layout(Size::new(640.0, 480.0));
        assert_eq!(tree.focused_id(), Some("behind"));
    }

    #[test]
    fn focusing_an_offscreen_modal_field_reveals_it_in_the_outer_scroll_area() {
        let mut tree = UiTree::new(sample_modal());
        tree.layout(Size::new(640.0, 480.0));
        assert!(tree.focus("later"));
        let frame = tree.layout(Size::new(640.0, 480.0));
        assert!(tree.scroll_offset("modal_body:dismiss") > 0.0);
        assert!(frame.regions.iter().any(|region| region.id == "later"));
    }

    #[test]
    fn tab_reaches_an_offscreen_modal_field_and_reveals_it() {
        let mut tree = UiTree::new(sample_modal());
        tree.layout(Size::new(640.0, 480.0));
        assert!(tree.focus("path"));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("later"));
        let frame = tree.layout(Size::new(640.0, 480.0));
        assert!(tree.scroll_offset("modal_body:dismiss") > 0.0);
        assert!(frame.regions.iter().any(|region| region.id == "later"));
    }

    #[test]
    fn narrow_viewport_keeps_close_field_and_footer_inside_card() {
        let mut tree = UiTree::new(sample_modal());
        let frame = tree.layout(Size::new(320.0, 240.0));
        let card = frame
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:dismiss")
            .unwrap()
            .rect;
        assert!(card.x >= 8.0 && card.y >= 8.0);
        assert!(card.x + card.width <= 312.0);
        assert!(card.y + card.height <= 232.0);
        for id in ["dismiss:close", "path", "save"] {
            let control = frame.regions.iter().find(|region| region.id == id).unwrap();
            assert!(control.rect.x >= card.x, "{id}");
            assert!(control.rect.y >= card.y, "{id}");
            assert!(
                control.rect.x + control.rect.width <= card.x + card.width,
                "{id}"
            );
            assert!(
                control.rect.y + control.rect.height <= card.y + card.height,
                "{id}"
            );
        }
    }

    #[test]
    fn return_focus_uses_stable_control_when_opening_menu_item_disappears() {
        let mut tree = UiTree::new(Element::button("temporary_menu_item", "Folder"));
        tree.layout(Size::new(640.0, 480.0));
        assert!(tree.focus("temporary_menu_item"));
        let base = || Element::button("add_menu", "Add source");
        tree.set_root(
            Modal::new(
                base(),
                true,
                "Add source",
                "",
                "cancel",
                ComponentPalette::neutral_dark(),
            )
            .body(Element::text_input("path", "", "Folder"))
            .initial_focus("path")
            .return_focus("add_menu")
            .render(),
        );
        tree.layout(Size::new(640.0, 480.0));
        assert_eq!(tree.focused_id(), Some("path"));
        tree.set_root(base());
        tree.layout(Size::new(640.0, 480.0));
        assert_eq!(tree.focused_id(), Some("add_menu"));
    }
}
