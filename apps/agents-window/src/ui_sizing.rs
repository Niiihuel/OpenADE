//! openADE's sizing profiles for limn components, kept in one place.
//!
//! A limn component takes its density (padding, gap, font size, icon size,
//! row height) from a const profile rather than from numbers at each call.
//! The app's own profiles live here so a screen never restates them.

use openide_ui::{ActionRowSizing, NoticeSizing, PageHeaderSizing, RowSizing, TextRole};

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

/// The Marketplace's heading: one cut line each, on the row of its actions.
pub const MARKETPLACE_HEADER: PageHeaderSizing = PageHeaderSizing {
    title_size: 20.0,
    description_size: 11.0,
    copy_gap: 2.0,
    row_gap: 6.0,
    stacked_gap: 8.0,
    height: 40.0,
};

/// The same heading beside the host's sidebar control on a narrow page.
pub const MARKETPLACE_HEADER_NARROW: PageHeaderSizing = PageHeaderSizing {
    title_size: 18.0,
    ..MARKETPLACE_HEADER
};

/// A status line of the Problems panel: one cut row of fixed height.
pub const PROBLEM_STATUS: NoticeSizing = NoticeSizing {
    height: Some(51.0),
    title_height: Some(17.0),
    description_height: Some(16.0),
    ..NoticeSizing::COMPACT
};

/// The files panel with nothing to list: two lines of the same size.
pub const FILES_EMPTY: NoticeSizing = NoticeSizing {
    icon_size: 0.0,
    title_role: TextRole::Caption,
    description_role: TextRole::Caption,
    gap: 0.0,
    copy_gap: 8.0,
    padding: 12.0,
    height: None,
    title_height: None,
    description_height: None,
};

/// The debug sidebar before a program is configured.
pub const DEBUG_SETUP: NoticeSizing = NoticeSizing {
    title_role: TextRole::Label,
    copy_gap: 10.0,
    padding: 8.0,
    ..FILES_EMPTY
};

/// The bar over a terminal whose process ended: one cut line and an action.
pub const TERMINAL_EXIT: NoticeSizing = NoticeSizing {
    icon_size: 0.0,
    title_role: TextRole::Small,
    description_role: TextRole::Small,
    gap: 8.0,
    copy_gap: 0.0,
    padding: 4.0,
    height: Some(40.0),
    title_height: Some(32.0),
    description_height: None,
};

/// A saved project action: its label over the first line of its command.
pub const PROJECT_ACTION: RowSizing = RowSizing {
    height: Some(42.0),
    two_line_height: Some(42.0),
    padding: 5.0,
    gap: 7.0,
    leading_width: 16.0,
    subtitle_size: 10.0,
    ..RowSizing::COMPACT
};

/// A notification: as tall as its two lines, with its count and age beside
/// the title.
pub const NOTIFICATION: RowSizing = RowSizing {
    height: None,
    two_line_height: None,
    padding: 6.0,
    subtitle_size: 11.5,
    ..RowSizing::COMPACT
};

/// A web citation in the transcript: a title of 18 over an address of 16.
pub const WEB_CITATION: RowSizing = RowSizing {
    height: None,
    two_line_height: None,
    padding: 9.0,
    subtitle_size: 10.0,
    title_height: Some(18.0),
    subtitle_height: Some(16.0),
    ..RowSizing::COMPACT
};

/// A task in the sidebar: a status slot, a faded title and its menu, edge to
/// edge in a row of the navigation height.
pub const SIDEBAR_TASK: RowSizing = RowSizing {
    two_line_height: Some(28.0),
    padding: 0.0,
    gap: 4.0,
    leading_height: Some(28.0),
    title_height: Some(28.0),
    ..RowSizing::COMPACT
};

/// A destination of the sidebar: its icon, its name and its shortcut.
pub const SIDEBAR_NAV: RowSizing = RowSizing {
    title_size: 13.0,
    ..RowSizing::COMPACT
};

/// A file the chat changed: its path and its counts on one line of 22.
pub const CHANGES_FILE: RowSizing = RowSizing {
    height: Some(38.0),
    padding: 8.0,
    gap: 6.0,
    meta_gap: 6.0,
    title_height: Some(22.0),
    ..RowSizing::COMPACT
};

/// A task in the IDE's task list.
pub const AGENT_LIST: RowSizing = RowSizing {
    height: Some(26.0),
    icon_size: 14.0,
    ..RowSizing::COMPACT
};

/// An entry of the extensions sidebar: its name over its state or source.
pub const EXTENSION: RowSizing = RowSizing {
    height: Some(42.0),
    two_line_height: Some(42.0),
    padding: 5.0,
    gap: 7.0,
    leading_height: Some(16.0),
    subtitle_size: 10.0,
    ..RowSizing::COMPACT
};

/// A changed file of the repository: its name, its folder and its status
/// letter on one line, beside the control that stages it.
pub const CHANGE_ROW: RowSizing = RowSizing {
    padding: 2.0,
    gap: 4.0,
    meta_gap: 4.0,
    ..RowSizing::COMPACT
};

/// A surface the dock can open, in the list of an empty dock.
pub const DOCK_OPEN: RowSizing = RowSizing {
    height: Some(38.0),
    padding: 5.0,
    gap: 6.0,
    title_size: crate::theme::UI_FONT_SIZE,
    ..RowSizing::COMPACT
};
