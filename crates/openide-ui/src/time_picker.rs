//! Compact controlled selector for a local 24-hour time of day.

use crate::components::{Component, ComponentPalette, ControlSize, IconButton, TextField};
use crate::{Element, FontFamily, Icon};

/// An editable `HH:MM` field plus keyboard-reachable hour and minute controls.
///
/// The field emits `InputChanged` using `{id}`. Adjustment buttons emit
/// `{id}:hour_up`, `{id}:hour_down`, `{id}:minute_up`, or `{id}:minute_down`.
/// Use [`TimePicker::adjusted_value`] to turn those actions into a validated
/// new value. Invalid partially typed text remains editable and is never
/// silently replaced by an adjustment.
pub struct TimePicker {
    id: String,
    value: String,
    palette: ComponentPalette,
}

impl TimePicker {
    pub fn new(id: impl Into<String>, value: impl Into<String>, palette: ComponentPalette) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            palette,
        }
    }

    pub fn is_valid_time(value: &str) -> bool {
        parse_time(value).is_some()
    }

    /// Applies a single button action to a valid 24-hour time. Hour adjustment
    /// wraps across midnight; minute adjustment carries into the hour.
    pub fn adjusted_value(action_id: &str, id: &str, current: &str) -> Option<String> {
        let (hour, minute) = parse_time(current)?;
        let action = action_id.strip_prefix(id)?.strip_prefix(':')?;
        let total = i32::from(hour) * 60 + i32::from(minute);
        let delta = match action {
            "hour_up" => 60,
            "hour_down" => -60,
            "minute_up" => 1,
            "minute_down" => -1,
            _ => return None,
        };
        let adjusted = (total + delta).rem_euclid(24 * 60);
        Some(format!("{:02}:{:02}", adjusted / 60, adjusted % 60))
    }

    fn adjustment(&self, suffix: &str, icon: Icon) -> Element {
        IconButton::new(format!("{}:{suffix}", self.id), icon, self.palette)
            .size(ControlSize::Compact)
            .variant(crate::ButtonVariant::Ghost)
            .render()
    }

    fn unit(&self, label: &str, display: String, down: &str, up: &str) -> Element {
        let ink = self.palette.text;
        Element::column(vec![
            Element::text(label)
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(self.palette.muted),
            Element::row(vec![
                self.adjustment(down, Icon::ArrowDown),
                Element::text(display)
                    .font_family(FontFamily::Sans)
                    .font_size(13.0)
                    .foreground(ink)
                    .size(Some(28.0), Some(28.0))
                    .center_content(),
                self.adjustment(up, Icon::ArrowUp),
            ])
            .gap(2.0),
        ])
        .gap(3.0)
    }
}

impl Component for TimePicker {
    fn render(self) -> Element {
        let parsed = parse_time(&self.value);
        let hour = parsed
            .map(|(hour, _)| format!("{hour:02}"))
            .unwrap_or_else(|| "--".into());
        let minute = parsed
            .map(|(_, minute)| format!("{minute:02}"))
            .unwrap_or_else(|| "--".into());
        let input = TextField::new(&self.id, &self.value, "HH:MM", self.palette)
            .size(ControlSize::Compact)
            .width(92.0)
            .render();
        let controls = Element::row(vec![
            self.unit("Hora", hour, "hour_down", "hour_up"),
            self.unit("Minuto", minute, "minute_down", "minute_up"),
        ])
        .gap(10.0);
        Element::column(vec![input, controls]).gap(6.0)
    }
}

fn parse_time(value: &str) -> Option<(u8, u8)> {
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 2 || byte.is_ascii_digit())
    {
        return None;
    }
    let hour = value[..2].parse::<u8>().ok()?;
    let minute = value[3..5].parse::<u8>().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Key, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn validates_exact_24_hour_times() {
        assert!(TimePicker::is_valid_time("00:00"));
        assert!(TimePicker::is_valid_time("23:59"));
        for value in ["24:00", "23:60", "9:30", "09:3", "09.30", "-1:30", "09:30x"] {
            assert!(!TimePicker::is_valid_time(value), "{value}");
        }
    }

    #[test]
    fn minute_adjustment_carries_and_hour_adjustment_wraps() {
        assert_eq!(
            TimePicker::adjusted_value("time:minute_up", "time", "23:59"),
            Some("00:00".into())
        );
        assert_eq!(
            TimePicker::adjusted_value("time:minute_down", "time", "00:00"),
            Some("23:59".into())
        );
        assert_eq!(
            TimePicker::adjusted_value("time:hour_up", "time", "23:15"),
            Some("00:15".into())
        );
        assert_eq!(
            TimePicker::adjusted_value("time:hour_down", "time", "00:15"),
            Some("23:15".into())
        );
        assert_eq!(
            TimePicker::adjusted_value("other:minute_up", "time", "23:59"),
            None
        );
        assert_eq!(
            TimePicker::adjusted_value("time:minute_up", "time", "invalid"),
            None
        );
    }

    #[test]
    fn adjustment_buttons_work_with_keyboard() {
        let picker = TimePicker::new("time", "09:00", ComponentPalette::neutral_dark()).render();
        let mut tree = UiTree::new(picker);
        let frame = tree.layout(Size::new(300.0, 100.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "time:minute_up"));
        assert!(tree.focus("time:minute_up"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "time:minute_up".into()
            }]
        );
    }
}
