//! Shared empty state for pages, panes and filtered results.

use crate::{
    Button, ButtonSize, ButtonVariant, Component, ComponentPalette, Element, FontFamily, Icon,
};

pub struct Empty {
    title: String,
    description: String,
    icon: Option<Icon>,
    actions: Vec<(String, String, ButtonVariant)>,
    action_element: Option<Element>,
    palette: ComponentPalette,
    width: Option<f32>,
    requested_height: Option<f32>,
    compact: bool,
}

impl Empty {
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            icon: None,
            actions: Vec::new(),
            action_element: None,
            palette,
            width: None,
            requested_height: None,
            compact: false,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn action(
        mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        variant: ButtonVariant,
    ) -> Self {
        self.actions.push((id.into(), label.into(), variant));
        self
    }

    /// Place a caller-owned control in the centered action row.
    /// This is useful when an empty state's action is more than a button.
    pub fn action_element(mut self, element: Element) -> Self {
        self.action_element = Some(element);
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width.max(0.0));
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        if height.is_finite() {
            self.requested_height = Some(height.max(0.0));
        }
        self
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }
}

impl Component for Empty {
    fn render(self) -> Element {
        // Empty states stay legible even when a caller requests a short pane.
        // The caller may always make the surface taller to occupy a viewport.
        let minimum_height = if self.compact { 228.0 } else { 256.0 };
        let height = self
            .requested_height
            .unwrap_or(minimum_height)
            .max(minimum_height);
        let center = |element: Element| {
            Element::row(vec![
                Element::spacer().flex(1.0),
                element,
                Element::spacer().flex(1.0),
            ])
        };
        let mut content = Vec::new();
        if let Some(icon) = self.icon {
            let side = if self.compact { 46.0 } else { 52.0 };
            content.push(center(
                Element::panel(Element::icon(icon).foreground(self.palette.text))
                    .background(self.palette.raised)
                    .radius(13.0)
                    .size(Some(side), Some(side)),
            ));
        }
        content.push(
            Element::text(self.title)
                .foreground(self.palette.text)
                .font_family(FontFamily::Sans)
                .font_size(if self.compact { 18.0 } else { 21.0 })
                .center_content(),
        );
        if !self.description.is_empty() {
            content.push(
                Element::text(self.description)
                    .foreground(self.palette.muted)
                    .font_family(FontFamily::Sans)
                    .font_size(if self.compact { 13.5 } else { 15.0 })
                    .center_content(),
            );
        }
        if !self.actions.is_empty() || self.action_element.is_some() {
            let mut actions: Vec<Element> = self
                .actions
                .into_iter()
                .map(|(id, label, variant)| {
                    Button::new(id, label, self.palette)
                        .variant(variant)
                        .button_size(if self.compact {
                            ButtonSize::Small
                        } else {
                            ButtonSize::Default
                        })
                        .render()
                })
                .collect();
            if let Some(element) = self.action_element {
                actions.push(element);
            }
            content.push(center(Element::row(actions).gap(8.0)));
        }
        let body = Element::column(content).gap(if self.compact { 10.0 } else { 12.0 });
        let centered = Element::column(vec![
            Element::spacer().flex(1.0),
            body,
            Element::spacer().flex(1.0),
        ]);
        let border = emphasized_border(self.palette.border, self.palette.text);
        Element::panel(centered)
            .padding(if self.compact { 18.0 } else { 24.0 })
            .background(self.palette.surface)
            .dashed_border(border, 1.0)
            .radius(14.0)
            .size(self.width, Some(height))
    }
}

fn emphasized_border(border: crate::Color, text: crate::Color) -> crate::Color {
    let blend = |base: u8, ink: u8| ((u16::from(base) * 4 + u16::from(ink)) / 5) as u8;
    crate::Color::rgba(
        blend(border.r, text.r),
        blend(border.g, text.g),
        blend(border.b, text.b),
        border.a,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Size, UiTree};

    #[test]
    fn empty_keeps_actions_and_dashed_frame() {
        let palette = ComponentPalette::neutral_dark();
        let empty = Empty::new("Sin proyectos", "Crea tu primer proyecto", palette)
            .icon(Icon::Folder)
            .action("create", "Crear proyecto", ButtonVariant::Default)
            .action("import", "Importar", ButtonVariant::Outline)
            .width(420.0)
            .render();
        assert!(empty.style.dashed_border);
        let frame = UiTree::new(empty).layout(Size::new(420.0, 256.0));
        assert!(frame.regions.iter().any(|region| region.id == "create"));
        assert!(frame.regions.iter().any(|region| region.id == "import"));
    }

    #[test]
    fn compact_empty_keeps_its_action_inside_a_flexible_pane() {
        let palette = ComponentPalette::neutral_dark();
        let empty = Empty::new(
            "Sin archivos",
            "Abre un proyecto para ver sus archivos.",
            palette,
        )
        .compact()
        .icon(Icon::Folder)
        .action("open", "Abrir proyecto", ButtonVariant::Default)
        .render();
        assert_eq!(empty.style.width, None);
        assert_eq!(empty.style.height, Some(228.0));
        let frame = UiTree::new(empty).layout(Size::new(280.0, 228.0));
        let action = frame
            .regions
            .iter()
            .find(|region| region.id == "open")
            .unwrap();
        assert!(action.rect.y >= 0.0);
        assert!(action.rect.y + action.rect.height <= 228.0);
    }

    #[test]
    fn empty_centers_content_and_honors_minimum_height() {
        let palette = ComponentPalette::neutral_dark();
        let empty = Empty::new("No hay resultados", "Prueba otra búsqueda", palette)
            .icon(Icon::Search)
            .compact()
            .height(150.0)
            .render();
        assert_eq!(empty.style.height, Some(228.0));
        let frame = UiTree::new(empty).layout(Size::new(420.0, 228.0));
        let icon = frame.commands.iter().find_map(|command| match command {
            crate::DrawCommand::Icon {
                icon: Icon::Search,
                rect,
                ..
            } => Some(rect),
            _ => None,
        });
        let icon = icon.expect("empty icon");
        assert!((icon.x + icon.width / 2.0 - 210.0).abs() < 1.0);
        let title = frame.commands.iter().find_map(|command| match command {
            crate::DrawCommand::Text { position, text, .. } if text == "No hay resultados" => {
                Some(position)
            }
            _ => None,
        });
        let title = title.expect("empty title");
        assert!(title.x > 80.0 && title.x < 210.0);
        assert!(title.y > 80.0 && title.y < 160.0);
    }

    #[test]
    fn empty_keeps_caller_owned_action_in_centered_row() {
        let palette = ComponentPalette::neutral_dark();
        let empty = Empty::new("Sin conexión", "Vuelve a intentarlo", palette)
            .action_element(
                Button::new("retry", "Reintentar", palette)
                    .variant(ButtonVariant::Secondary)
                    .render(),
            )
            .width(420.0)
            .render();
        let frame = UiTree::new(empty).layout(Size::new(420.0, 256.0));
        let retry = frame
            .regions
            .iter()
            .find(|region| region.id == "retry")
            .expect("caller-owned empty action");
        assert!((retry.rect.x + retry.rect.width / 2.0 - 210.0).abs() < 1.0);
    }
}
