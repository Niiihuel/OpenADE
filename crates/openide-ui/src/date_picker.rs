//! A controlled, keyboard-accessible calendar for local calendar dates.
//!
//! The caller owns both the selected ISO date and the displayed month. Text
//! edits emit `InputChanged` with the base ID; month and day buttons emit
//! `Clicked` with IDs described on [`DatePicker`]. No date is silently chosen
//! while the user navigates the calendar.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::components::{Component, ComponentPalette, ControlSize, IconButton, TextField};
use crate::{Color, Element, FontFamily, Icon};

const WEEKDAYS: [&str; 7] = ["Lu", "Ma", "Mi", "Ju", "Vi", "Sa", "Do"];
const MONTHS: [&str; 12] = [
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "octubre",
    "noviembre",
    "diciembre",
];
// Seven columns plus gaps and panel padding stay within a 248 px modal body.
const CELL_WIDTH: f32 = 30.0;
const CELL_HEIGHT: f32 = 28.0;
const CELL_GAP: f32 = 2.0;

/// Inline date entry and month calendar. Buttons are tabbable and activate
/// with Enter or Space through the ordinary [`crate::UiTree`] keyboard path.
///
/// Action IDs are `{id}:month_prev`, `{id}:month_next`, and
/// `{id}:day:YYYY-MM-DD`. The editable field uses `{id}` itself.
pub struct DatePicker {
    id: String,
    value: String,
    display_year: i32,
    display_month: u32,
    palette: ComponentPalette,
}

impl DatePicker {
    pub fn new(id: impl Into<String>, value: impl Into<String>, palette: ComponentPalette) -> Self {
        let value = value.into();
        let (display_year, display_month) = parse_date(&value)
            .map(|(year, month, _)| (year, month))
            .unwrap_or_else(current_utc_month);
        Self {
            id: id.into(),
            value,
            display_year,
            display_month,
            palette,
        }
    }

    /// Keep the visible month separate from the selected date so navigation
    /// never changes a scheduled date until a day is chosen.
    pub fn display_month(mut self, year: i32, month: u32) -> Self {
        if (1..=9999).contains(&year) && (1..=12).contains(&month) {
            self.display_year = year;
            self.display_month = month;
        }
        self
    }

    pub fn is_valid_date(value: &str) -> bool {
        parse_date(value).is_some()
    }

    /// Decode only a valid day action belonging to this picker.
    pub fn selected_date<'a>(action_id: &'a str, id: &str) -> Option<&'a str> {
        let value = action_id.strip_prefix(id)?.strip_prefix(":day:")?;
        Self::is_valid_date(value).then_some(value)
    }

    /// Shift a displayed month, clamping to the supported Gregorian years.
    pub fn shift_month(year: i32, month: u32, delta: i32) -> (i32, u32) {
        let year = year.clamp(1, 9999);
        let month = month.clamp(1, 12);
        let index = (i64::from(year) - 1) * 12 + i64::from(month - 1);
        let shifted = (index + i64::from(delta)).clamp(0, 9999 * 12 - 1);
        ((shifted / 12 + 1) as i32, (shifted % 12 + 1) as u32)
    }

    fn day_cell(&self, index: u32, first_weekday: u32, month_days: u32) -> Element {
        let offset = index as i32 - first_weekday as i32;
        let (year, month, day, in_month) = if offset < 0 {
            let (year, month) = Self::shift_month(self.display_year, self.display_month, -1);
            if year == self.display_year && month == self.display_month {
                return blank_cell();
            }
            (
                year,
                month,
                (days_in_month(year, month) as i32 + offset + 1) as u32,
                false,
            )
        } else if offset >= month_days as i32 {
            let (year, month) = Self::shift_month(self.display_year, self.display_month, 1);
            if year == self.display_year && month == self.display_month {
                return blank_cell();
            }
            (year, month, (offset - month_days as i32 + 1) as u32, false)
        } else {
            (
                self.display_year,
                self.display_month,
                offset as u32 + 1,
                true,
            )
        };
        let date = format!("{year:04}-{month:02}-{day:02}");
        let selected = self.value == date;
        let ink = if selected || in_month {
            self.palette.text
        } else {
            self.palette.muted
        };
        Element::clickable(
            format!("{}:day:{date}", self.id),
            Element::text(day.to_string())
                .foreground(ink)
                .font_family(FontFamily::Sans)
                .font_size(12.0),
        )
        .size(Some(CELL_WIDTH), Some(CELL_HEIGHT))
        .center_content()
        .background(if selected {
            self.palette.selected
        } else {
            Color::TRANSPARENT
        })
        .hover_background(self.palette.hover)
        .pressed_background(self.palette.selected)
        .focus_border(self.palette.border, 1.0)
        .radius(6.0)
    }
}

impl Component for DatePicker {
    fn render(self) -> Element {
        let first_weekday = weekday_monday_zero(self.display_year, self.display_month, 1);
        let month_days = days_in_month(self.display_year, self.display_month);
        let previous = IconButton::new(
            format!("{}:month_prev", self.id),
            Icon::ArrowLeft,
            self.palette,
        )
        .size(ControlSize::Compact)
        .variant(crate::ButtonVariant::Ghost)
        .enabled(!(self.display_year == 1 && self.display_month == 1))
        .render();
        let next = IconButton::new(
            format!("{}:month_next", self.id),
            Icon::ArrowRight,
            self.palette,
        )
        .size(ControlSize::Compact)
        .variant(crate::ButtonVariant::Ghost)
        .enabled(!(self.display_year == 9999 && self.display_month == 12))
        .render();
        let title = Element::text(format!(
            "{} {}",
            MONTHS[self.display_month as usize - 1],
            self.display_year
        ))
        .foreground(self.palette.text)
        .font_family(FontFamily::Sans)
        .font_size(13.0)
        .flex(1.0)
        .center_content();
        let header = Element::row(vec![previous, title, next]).gap(3.0);
        let weekdays = Element::row(
            WEEKDAYS
                .iter()
                .map(|day| {
                    Element::text(*day)
                        .foreground(self.palette.muted)
                        .font_family(FontFamily::Sans)
                        .font_size(11.0)
                        .size(Some(CELL_WIDTH), Some(22.0))
                        .center_content()
                })
                .collect(),
        )
        .gap(CELL_GAP);
        let week_count = (first_weekday + month_days).div_ceil(7).max(5);
        let weeks = Element::column(
            (0..week_count)
                .map(|week| {
                    Element::row(
                        (0..7)
                            .map(|weekday| {
                                self.day_cell(week * 7 + weekday, first_weekday, month_days)
                            })
                            .collect(),
                    )
                    .gap(CELL_GAP)
                })
                .collect(),
        )
        .gap(CELL_GAP);
        let input = TextField::new(&self.id, &self.value, "AAAA-MM-DD", self.palette)
            .size(ControlSize::Compact)
            .width(126.0)
            .render();
        Element::column(vec![
            input,
            Element::panel(Element::column(vec![header, weekdays, weeks]).gap(5.0))
                .padding(8.0)
                .background(self.palette.surface)
                .border(self.palette.border, 1.0)
                .radius(9.0),
        ])
        .gap(7.0)
    }
}

fn blank_cell() -> Element {
    Element::spacer().size(Some(CELL_WIDTH), Some(CELL_HEIGHT))
}

fn parse_date(value: &str) -> Option<(i32, u32, u32)> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        return None;
    }
    let year = value[..4].parse::<i32>().ok()?;
    let month = value[5..7].parse::<u32>().ok()?;
    let day = value[8..10].parse::<u32>().ok()?;
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
    {
        return None;
    }
    Some((year, month, day))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

fn weekday_monday_zero(year: i32, month: u32, day: u32) -> u32 {
    // Sakamoto's Gregorian weekday, converted from Sunday-zero.
    const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let adjusted_year = year - i32::from(month < 3);
    let sunday_zero = (adjusted_year + adjusted_year / 4 - adjusted_year / 100
        + adjusted_year / 400
        + OFFSETS[month as usize - 1]
        + day as i32)
        .rem_euclid(7);
    ((sunday_zero + 6) % 7) as u32
}

fn current_utc_month() -> (i32, u32) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds / 86_400;
    // Civil date from days since 1970-01-01 (proleptic Gregorian calendar).
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_piece = (5 * day_of_year + 2) / 153;
    let month = month_piece + if month_piece < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    (year.clamp(1, 9999) as i32, month as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Key, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn validates_gregorian_dates_and_strict_iso_text() {
        assert!(DatePicker::is_valid_date("2024-02-29"));
        assert!(DatePicker::is_valid_date("2000-02-29"));
        for value in [
            "1900-02-29",
            "2025-02-29",
            "2025-13-01",
            "2025-00-10",
            "0000-01-01",
            "2025-01-32",
            "2025-1-1",
            "2025-01-01x",
        ] {
            assert!(!DatePicker::is_valid_date(value), "{value}");
        }
        assert_eq!(weekday_monday_zero(2025, 5, 1), 3);
        assert_eq!(weekday_monday_zero(2024, 1, 1), 0);
    }

    #[test]
    fn navigation_crosses_year_and_stops_at_supported_bounds() {
        assert_eq!(DatePicker::shift_month(2025, 1, -1), (2024, 12));
        assert_eq!(DatePicker::shift_month(2025, 12, 1), (2026, 1));
        assert_eq!(DatePicker::shift_month(1, 1, -1), (1, 1));
        assert_eq!(DatePicker::shift_month(9999, 12, 1), (9999, 12));
    }

    #[test]
    fn day_action_is_scoped_and_validated() {
        assert_eq!(
            DatePicker::selected_date("schedule:day:2025-05-14", "schedule"),
            Some("2025-05-14")
        );
        assert_eq!(
            DatePicker::selected_date("other:day:2025-05-14", "schedule"),
            None
        );
        assert_eq!(
            DatePicker::selected_date("schedule:day:2025-02-29", "schedule"),
            None
        );
    }

    #[test]
    fn calendar_exposes_day_buttons_and_keyboard_activation() {
        let picker = DatePicker::new("date", "2025-05-14", ComponentPalette::neutral_dark())
            .display_month(2025, 5)
            .render();
        let mut tree = UiTree::new(picker);
        let frame = tree.layout(Size::new(340.0, 330.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "date:day:2025-05-14"));
        assert!(tree.focus("date:day:2025-05-14"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "date:day:2025-05-14".into()
            }]
        );
        assert!(tree.focus("date"));
        assert_eq!(
            tree.handle_event(UiEvent::Text("6".into())),
            vec![UiAction::InputChanged {
                id: "date".into(),
                value: "2025-05-146".into(),
            }]
        );
    }
}
