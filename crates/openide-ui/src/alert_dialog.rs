//! A centered confirmation surface for consequential actions.

use crate::components::{Component, ComponentPalette};
use crate::{Color, Element, FontFamily, Icon, Point, PopoverPlacement, TextSpan};

/// Controlled modal dialog. The host performs the action only after receiving
/// `confirm_id`; close, Cancel and Escape emit `cancel_id`. The scrim blocks
/// interaction with the background and does not dismiss the dialog.
pub struct AlertDialog {
    base: Element,
    open: bool,
    title: String,
    description: String,
    confirm_id: String,
    cancel_id: String,
    confirm_label: String,
    cancel_label: String,
    palette: ComponentPalette,
    width: f32,
    destructive: Option<Color>,
    animation_ms: u16,
    body: Option<Element>,
}

impl AlertDialog {
    pub fn new(
        base: Element,
        open: bool,
        title: impl Into<String>,
        description: impl Into<String>,
        confirm_id: impl Into<String>,
        cancel_id: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            base,
            open,
            title: title.into(),
            description: description.into(),
            confirm_id: confirm_id.into(),
            cancel_id: cancel_id.into(),
            confirm_label: "Continuar".into(),
            cancel_label: "Cancelar".into(),
            palette,
            width: 440.0,
            destructive: None,
            animation_ms: 160,
            body: None,
        }
    }

    pub fn confirm_label(mut self, label: impl Into<String>) -> Self {
        self.confirm_label = label.into();
        self
    }

    pub fn cancel_label(mut self, label: impl Into<String>) -> Self {
        self.cancel_label = label.into();
        self
    }

    pub fn destructive(mut self, color: Color) -> Self {
        self.destructive = Some(color);
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.max(240.0);
        }
        self
    }

    pub fn animation_ms(mut self, duration: u16) -> Self {
        self.animation_ms = duration;
        self
    }

    /// Optional form or other content between the description and actions.
    pub fn body(mut self, body: Element) -> Self {
        self.body = Some(body);
        self
    }
}

impl Component for AlertDialog {
    fn render(self) -> Element {
        if !self.open {
            return self.base;
        }
        let ink = self.palette.text;
        let confirm_fill = self.destructive.unwrap_or(ink);
        let confirm_ink = if self.destructive.is_some() {
            Color::WHITE
        } else {
            self.palette.surface
        };
        let narrow = self.width < 360.0;
        // Wide dialogs right-align intrinsic action buttons. Fixed 128 px
        // buttons overflow when the viewport clamps a 440 px dialog.
        let button_width = narrow.then_some(self.width - 48.0);
        // The close control and Cancel share an action, but each needs its own
        // focus identity so keyboard users can reach both visible controls.
        let close = Element::clickable(
            format!("{}:close", self.cancel_id),
            Element::row(vec![
                Element::spacer().flex(1.0),
                Element::icon(Icon::CloseBold).foreground(self.palette.muted),
                Element::spacer().flex(1.0),
            ])
            .size(None, Some(30.0)),
        )
        .background(self.palette.surface)
        .hover_background(self.palette.hover)
        .radius(6.0)
        .size(Some(30.0), Some(30.0))
        .dispatch_as(self.cancel_id.clone());
        let button = |id: String, label: String, fill: Color, foreground: Color, border: bool| {
            let visual = Element::row(vec![
                Element::spacer().flex(1.0),
                Element::text(label)
                    .foreground(foreground)
                    .font_family(FontFamily::Sans)
                    .font_size(14.0),
                Element::spacer().flex(1.0),
            ])
            .padding(10.0)
            .size(None, Some(38.0));
            let button = Element::clickable(id, visual)
                .background(fill)
                .hover_background(if border { self.palette.hover } else { fill })
                .pressed_background(self.palette.selected)
                .radius(8.0)
                .size(button_width, Some(38.0));
            if border {
                button.border(self.palette.border, 1.0)
            } else {
                button
            }
        };
        let cancel = button(
            self.cancel_id.clone(),
            self.cancel_label,
            self.palette.surface,
            ink,
            true,
        );
        let confirm = button(
            self.confirm_id,
            self.confirm_label,
            confirm_fill,
            confirm_ink,
            false,
        );
        let footer = if narrow {
            Element::column(vec![cancel, confirm]).gap(8.0)
        } else {
            Element::row(vec![Element::spacer().flex(1.0), cancel, confirm])
                .gap(8.0)
                .size(None, Some(38.0))
        };
        let mut sections = vec![Element::row(vec![
            Element::truncated_text(self.title)
                .foreground(ink)
                .font_family(FontFamily::Sans)
                .font_size(20.0)
                .flex(1.0),
            close,
        ])
        .gap(12.0)];
        if !self.description.is_empty() {
            sections.push(
                Element::formatted_text(vec![TextSpan::new(
                    self.description,
                    self.palette.muted,
                    14.0,
                    FontFamily::Sans,
                )])
                .size(None, None),
            );
        }
        if let Some(body) = self.body {
            sections.push(body);
        }
        sections.push(footer);
        let card = Element::panel(Element::column(sections).gap(16.0))
            .padding(24.0)
            .size(Some(self.width), None)
            .background(self.palette.surface)
            .border(self.palette.border, 1.0)
            .radius(16.0)
            .shadow(self.palette.shadow, Point::new(0.0, 16.0), 32.0, 0.0);
        Element::popover(self.base, card, self.cancel_id, PopoverPlacement::Center)
            .popover_scrim(Color::rgba(0, 0, 0, 180))
            .popover_backdrop_dismiss(false)
            .popover_animation_ms(self.animation_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, HitKind, Key, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn modal_blocks_backdrop_and_traps_keyboard_until_dismissed() {
        let palette = ComponentPalette::neutral_dark();
        let base = || Element::button("underlying", "Fondo").size(Some(400.0), Some(300.0));
        let mut tree = UiTree::new(base());
        tree.layout(Size::new(400.0, 300.0));
        assert!(tree.focus("underlying"));
        tree.set_root(
            AlertDialog::new(
                base(),
                true,
                "¿Eliminar la tarea?",
                "Se perderá el historial de esta tarea.",
                "confirm",
                "cancel",
                palette,
            )
            .render(),
        );
        let frame = tree.layout(Size::new(400.0, 300.0));
        assert!(frame.regions.iter().any(|region| region.id == "confirm"));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::FillRect { color, .. } if *color == Color::rgba(0, 0, 0, 180)
        )));
        assert_eq!(tree.focused_id(), Some("cancel:close"));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("cancel"));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("confirm"));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: true,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("cancel"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "cancel".into()
            }]
        );
        tree.set_root(base());
        tree.layout(Size::new(400.0, 300.0));
        assert_eq!(tree.focused_id(), Some("underlying"));
    }

    #[test]
    fn close_button_has_own_focus_but_dispatches_cancel_action() {
        let mut tree = UiTree::new(
            AlertDialog::new(
                Element::button("underlying", "Fondo"),
                true,
                "¿Salir?",
                "Se cancelarán los cambios pendientes.",
                "confirm",
                "cancel",
                ComponentPalette::neutral_dark(),
            )
            .render(),
        );
        let frame = tree.layout(Size::new(700.0, 500.0));
        let close = frame
            .regions
            .iter()
            .find(|region| region.id == "cancel:close")
            .expect("separate close target")
            .rect;
        let center = Point::new(close.x + close.width / 2.0, close.y + close.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(center));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(center)),
            vec![UiAction::Clicked {
                id: "cancel".into()
            }]
        );
        assert!(tree.focus("cancel:close"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "cancel".into()
            }]
        );
    }

    #[test]
    fn narrow_dialog_keeps_both_actions_inside_its_card() {
        let frame = UiTree::new(
            AlertDialog::new(
                Element::spacer(),
                true,
                "¿Eliminar?",
                "Esta acción no se puede deshacer.",
                "confirm",
                "cancel",
                ComponentPalette::neutral_dark(),
            )
            .width(256.0)
            .render(),
        )
        .layout(Size::new(280.0, 320.0));
        let card = frame
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:cancel")
            .unwrap()
            .rect;
        let confirm = frame
            .regions
            .iter()
            .find(|region| region.id == "confirm")
            .unwrap();
        assert!(confirm.rect.x >= card.x);
        assert!(confirm.rect.x + confirm.rect.width <= card.x + card.width);
    }

    #[test]
    fn dialog_actions_stay_within_a_narrow_viewport() {
        let frame = UiTree::new(
            AlertDialog::new(
                Element::spacer(),
                true,
                "¿Eliminar?",
                "Esta acción no se puede deshacer.",
                "confirm",
                "cancel",
                ComponentPalette::neutral_dark(),
            )
            .render(),
        )
        .layout(Size::new(320.0, 420.0));
        let card = frame
            .regions
            .iter()
            .find(|region| region.id == "popover_surface:cancel")
            .unwrap()
            .rect;
        for id in ["cancel:close", "cancel", "confirm"] {
            let action = frame
                .regions
                .iter()
                .find(|region| region.id == id && region.kind == HitKind::Button)
                .unwrap()
                .rect;
            assert!(
                action.x >= card.x && action.x + action.width <= card.x + card.width,
                "{id}: action={action:?}, card={card:?}"
            );
            assert!(
                action.y >= card.y && action.y + action.height <= card.y + card.height,
                "{id}: action={action:?}, card={card:?}"
            );
        }
    }

    #[test]
    fn backdrop_blocks_pointer_without_dismissing_alert() {
        let mut tree = UiTree::new(
            AlertDialog::new(
                Element::button("underlying", "Fondo"),
                true,
                "Renombrar",
                "Nuevo nombre",
                "save",
                "cancel",
                ComponentPalette::neutral_dark(),
            )
            .body(Element::text_input("name", "Anterior", "Nombre"))
            .render(),
        );
        tree.layout(Size::new(800.0, 600.0));
        assert!(tree.focus("name"));
        let outside = Point::new(4.0, 4.0);
        assert!(tree.handle_event(UiEvent::PointerDown(outside)).is_empty());
        assert!(tree.handle_event(UiEvent::PointerUp(outside)).is_empty());
        assert_eq!(tree.focused_id(), Some("name"));
    }
}
