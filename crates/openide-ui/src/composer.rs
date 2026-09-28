//! Reusable composition surface. The host owns text and actions; this
//! component owns the editor card and its responsive slot layout.

use crate::components::{Component, ComponentPalette};
use crate::{Color, Element, FontFamily, Point};

pub struct Composer {
    editor_id: String,
    prompt: String,
    placeholder: String,
    width: f32,
    editor_height: f32,
    compact: bool,
    context: Option<Element>,
    context_height: f32,
    attachments: Vec<Element>,
    footer: Option<Element>,
    palette: ComponentPalette,
    border_color: Color,
    shadow_color: Color,
    card_radius: f32,
}

impl Composer {
    pub fn new(
        editor_id: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        width: f32,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            editor_id: editor_id.into(),
            prompt: prompt.into(),
            placeholder: placeholder.into(),
            width: if width.is_finite() {
                width.max(0.0)
            } else {
                0.0
            },
            editor_height: 64.0,
            compact: false,
            context: None,
            context_height: 44.0,
            attachments: Vec::new(),
            footer: None,
            palette,
            border_color: palette.border,
            shadow_color: palette.shadow,
            card_radius: 25.0,
        }
    }

    pub fn editor_height(mut self, height: f32) -> Self {
        if height.is_finite() {
            self.editor_height = height.max(24.0);
        }
        self
    }

    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    pub fn context(mut self, context: Element) -> Self {
        self.context = Some(context);
        self
    }

    pub fn context_height(mut self, height: f32) -> Self {
        if height.is_finite() {
            self.context_height = height.max(0.0);
        }
        self
    }

    pub fn attachments(mut self, attachments: Vec<Element>) -> Self {
        self.attachments = attachments;
        self
    }

    pub fn footer(mut self, footer: Element) -> Self {
        self.footer = Some(footer);
        self
    }

    pub fn border_color(mut self, color: Color) -> Self {
        self.border_color = color;
        self
    }

    pub fn shadow_color(mut self, color: Color) -> Self {
        self.shadow_color = color;
        self
    }

    pub fn card_radius(mut self, radius: f32) -> Self {
        if radius.is_finite() {
            self.card_radius = radius.max(0.0);
        }
        self
    }
}

impl Component for Composer {
    fn render(self) -> Element {
        let editor = Element::text_area(self.editor_id, self.prompt, self.placeholder)
            .background(self.palette.selected)
            .foreground(self.palette.text)
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .padding(14.0)
            .radius(17.0)
            .size(None, Some(self.editor_height));
        let mut contents = vec![editor];
        contents.extend(self.attachments);
        if let Some(footer) = self.footer {
            contents.push(footer);
        }
        let card = Element::panel(Element::column(contents).gap(5.0))
            .background(self.palette.selected)
            .border(self.border_color, 1.0)
            .radius(self.card_radius)
            .shadow(self.shadow_color, Point::new(0.0, 5.0), 18.0, 0.0)
            .padding(if self.compact { 10.0 } else { 13.0 })
            .size(Some(self.width), None);
        let Some(context) = self.context else {
            return card;
        };
        let context = Element::row(vec![
            Element::spacer().flex(1.0),
            context,
            Element::spacer().flex(1.0),
        ])
        .size(Some(self.width), Some(self.context_height));
        Element::column(vec![context, card])
            .gap(0.0)
            .size(Some(self.width), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Key, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn composer_keeps_context_outside_editor_and_routes_text_to_one_id() {
        let composer = Composer::new(
            "prompt",
            "",
            "Escribe una tarea",
            480.0,
            ComponentPalette::neutral_dark(),
        )
        .context(Element::button("project", "Proyecto"))
        .footer(Element::button("send", "Enviar"))
        .render();
        let mut tree = UiTree::new(composer);
        let frame = tree.layout(Size::new(480.0, 250.0));
        let project = frame
            .regions
            .iter()
            .find(|region| region.id == "project")
            .unwrap();
        let prompt = frame
            .regions
            .iter()
            .find(|region| region.id == "prompt")
            .unwrap();
        assert!(project.rect.y + project.rect.height <= prompt.rect.y);
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.id == "prompt")
                .count(),
            1
        );
        assert!(tree.focus("prompt"));
        assert_eq!(
            tree.handle_event(UiEvent::Text("hola".into())),
            vec![UiAction::InputChanged {
                id: "prompt".into(),
                value: "hola".into(),
            }]
        );
        let enter = tree.handle_event(UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false,
        });
        assert_eq!(
            enter,
            vec![UiAction::InputChanged {
                id: "prompt".into(),
                value: "hola\n".into(),
            }]
        );
    }
}
