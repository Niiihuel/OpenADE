//! Composition of the shared calendar and 24-hour time controls.

use crate::components::{Component, ComponentPalette};
use crate::date_picker::DatePicker;
use crate::time_picker::TimePicker;
use crate::{Element, FontFamily};

/// A local date and time picker with separate controlled values and action IDs.
/// Keeping the date and time independent allows a form to preserve a partially
/// typed value while the other half changes. The caller combines them when it
/// validates or persists a local timestamp.
pub struct DateTimePicker {
    date_id: String,
    time_id: String,
    date_value: String,
    time_value: String,
    display_month: Option<(i32, u32)>,
    palette: ComponentPalette,
}

impl DateTimePicker {
    pub fn new(
        date_id: impl Into<String>,
        time_id: impl Into<String>,
        date_value: impl Into<String>,
        time_value: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            date_id: date_id.into(),
            time_id: time_id.into(),
            date_value: date_value.into(),
            time_value: time_value.into(),
            display_month: None,
            palette,
        }
    }

    pub fn display_month(mut self, year: i32, month: u32) -> Self {
        self.display_month = Some((year, month));
        self
    }
}

impl Component for DateTimePicker {
    fn render(self) -> Element {
        let mut date = DatePicker::new(self.date_id, self.date_value, self.palette);
        if let Some((year, month)) = self.display_month {
            date = date.display_month(year, month);
        }
        let label = |text: &str| {
            Element::text(text)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(self.palette.muted)
        };
        Element::column(vec![
            label("Fecha"),
            date.render(),
            label("Hora local"),
            TimePicker::new(self.time_id, self.time_value, self.palette).render(),
        ])
        .gap(6.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Size, UiTree};

    #[test]
    fn exposes_date_and_time_as_independent_controls() {
        let picker = DateTimePicker::new(
            "task_date",
            "task_time",
            "2025-05-14",
            "09:30",
            ComponentPalette::neutral_dark(),
        )
        .display_month(2025, 5)
        .render();
        let mut tree = UiTree::new(picker);
        let frame = tree.layout(Size::new(340.0, 480.0));
        for id in [
            "task_date",
            "task_date:day:2025-05-14",
            "task_time",
            "task_time:minute_up",
        ] {
            assert!(frame.regions.iter().any(|region| region.id == id), "{id}");
        }
    }

    #[test]
    fn calendar_and_time_controls_fit_a_narrow_modal_body() {
        let picker = DateTimePicker::new(
            "task_date",
            "task_time",
            "2025-05-14",
            "09:30",
            ComponentPalette::neutral_dark(),
        )
        .display_month(2025, 5)
        .render();
        let mut tree = UiTree::new(picker);
        let frame = tree.layout(Size::new(248.0, 480.0));
        for region in frame.regions.iter().filter(|region| {
            region.id.starts_with("task_date") || region.id.starts_with("task_time")
        }) {
            assert!(
                region.rect.x + region.rect.width <= 248.5,
                "{} overflows: {:?}",
                region.id,
                region.rect
            );
        }
    }
}
