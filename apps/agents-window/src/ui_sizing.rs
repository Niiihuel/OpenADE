//! openADE's sizing profiles for limn components, kept in one place.
//!
//! A limn component takes its density (padding, gap, font size, icon size,
//! row height) from a const profile rather than from numbers at each call.
//! The app's own profiles live here so a screen never restates them.

use openide_ui::ActionRowSizing;

/// A setting: its label and supporting text beside a caller-owned control.
pub const SETTINGS_ROW: ActionRowSizing = ActionRowSizing {
    gap: 16.0,
    copy_gap: 4.0,
    icon_column: 20.0,
    min_height: 34.0,
    min_height_with_subtitle: 34.0,
};

/// A passive note inside a settings card: a small icon and wrapped copy.
pub const NOTE_ROW: ActionRowSizing = ActionRowSizing {
    gap: 10.0,
    copy_gap: 5.0,
    icon_column: 16.0,
    min_height: 0.0,
    min_height_with_subtitle: 0.0,
};
