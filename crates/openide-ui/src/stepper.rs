//! A controlled navigation strip for short, ordered forms.

use crate::{Component, ComponentPalette, Element, FontFamily};

pub struct Step {
    id: String,
    label: String,
    description: Option<String>,
}

impl Step {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

pub struct Stepper {
    steps: Vec<Step>,
    active_index: usize,
    palette: ComponentPalette,
    width: Option<f32>,
}

impl Stepper {
    pub fn new(steps: Vec<Step>, active_index: usize, palette: ComponentPalette) -> Self {
        Self {
            steps,
            active_index,
            palette,
            width: None,
        }
    }

    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = Some(width.max(0.0));
        }
        self
    }

    pub fn render(self) -> Element {
        if self.steps.is_empty() {
            return Element::spacer();
        }
        let active_index = self.active_index.min(self.steps.len() - 1);
        let mut items = Vec::with_capacity(self.steps.len() * 2 - 1);
        for (index, step) in self.steps.into_iter().enumerate() {
            if index > 0 {
                items.push(
                    Element::panel(Element::spacer())
                        .background(self.palette.border)
                        .size(Some(16.0), Some(1.0)),
                );
            }
            let active = index == active_index;
            let ink = if active {
                self.palette.text
            } else {
                self.palette.muted
            };
            let badge = Element::panel(
                Element::text((index + 1).to_string())
                    .foreground(if active { self.palette.surface } else { ink })
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .center_content(),
            )
            .background(if active {
                self.palette.text
            } else {
                self.palette.raised
            })
            .border(
                if active {
                    self.palette.text
                } else {
                    self.palette.border
                },
                1.0,
            )
            .radius(12.0)
            .size(Some(24.0), Some(24.0));
            let mut copy = vec![Element::text(step.label)
                .foreground(ink)
                .font_family(FontFamily::Sans)
                .font_size(12.0)];
            if let Some(description) = step.description {
                copy.push(
                    Element::text(description)
                        .foreground(self.palette.muted)
                        .font_family(FontFamily::Sans)
                        .font_size(10.0),
                );
            }
            items.push(
                Element::clickable(
                    step.id,
                    Element::row(vec![badge, Element::column(copy).gap(2.0)])
                        .gap(7.0)
                        .center_content(),
                )
                .flex(1.0)
                .size(None, Some(42.0)),
            );
        }
        Element::row(items).gap(6.0).size(self.width, Some(42.0))
    }
}

impl Component for Stepper {
    fn render(self) -> Element {
        Stepper::render(self)
    }
}
