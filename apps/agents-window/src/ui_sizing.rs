//! openADE's sizing profiles for limn components, kept in one place.
//!
//! A limn component takes its density (padding, gap, font size, icon size,
//! row height) from a const profile rather than from numbers at each call.
//! The app's own profiles live here so a screen never restates them.

use openide_ui::{
    ActionRowSizing, FormFieldSizing, HeaderSizing, Modal, NoticeSizing, PageHeaderSizing,
    PagerSizing, RowSizing, StatusBarItemSizing, TextRole,
};

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

/// A line of the Problems panel: a file or one of its diagnostics.
pub const PROBLEM_ROW: RowSizing = RowSizing {
    height: Some(26.0),
    padding: 5.0,
    gap: 6.0,
    icon_size: 14.0,
    leading_width: 14.0,
    leading_height: Some(16.0),
    meta_gap: 6.0,
    ..RowSizing::COMPACT
};

/// A tool call in the dock's Activity list: one line of 24 in a row of 38.
pub const DOCK_TOOL: RowSizing = RowSizing {
    height: Some(38.0),
    padding: 7.0,
    leading_height: Some(24.0),
    title_height: Some(24.0),
    ..RowSizing::COMPACT
};

/// The heading of a workspace sidebar. Its tools hang from the top of the
/// 26 px row, a pixel above its center.
pub const SIDEBAR_HEADER: HeaderSizing = HeaderSizing {
    center_tools: false,
    ..HeaderSizing::SIDEBAR
};

/// The bar over a pane. Its tools fill the 24 px between the insets, and
/// one that sets no height of its own keeps that height.
pub const PANE_HEADER: HeaderSizing = HeaderSizing {
    center_tools: false,
    ..HeaderSizing::PANE
};

/// The heading of the notifications popover, whose text action is as tall
/// as the row.
pub const NOTIFICATIONS_HEADER: HeaderSizing = HeaderSizing {
    center_tools: false,
    ..HeaderSizing::POPOVER
};

/// The files panel's heading on the IDE sidebar: tools 4 apart.
pub const FILES_HEADER_COMPACT: HeaderSizing = HeaderSizing {
    gap: 4.0,
    ..SIDEBAR_HEADER
};

/// The files panel's heading in the side pane: a row as tall as its tool.
pub const FILES_HEADER: HeaderSizing = HeaderSizing {
    height: 24.0,
    gap: 4.0,
    ..HeaderSizing::SIDEBAR
};

/// A 32 px heading without an inset, its tools at the top: the project
/// actions, the chat's changes and the usage popup.
pub const TALL_HEADER: HeaderSizing = HeaderSizing {
    height: 32.0,
    gap: 6.0,
    title_role: TextRole::Body,
    center_tools: false,
    ..HeaderSizing::SIDEBAR
};

/// The commit view's heading: tools centered in 28, a 13 px title.
pub const COMMIT_HEADER: HeaderSizing = HeaderSizing {
    height: 28.0,
    gap: 4.0,
    title_role: TextRole::Label,
    ..HeaderSizing::SIDEBAR
};

/// The review strip's pagers: the arrows touch the counter's slot.
pub const REVIEW_PAGER: PagerSizing = PagerSizing { gap: 0.0 };

/// What a modal's form leaves free beside the width limn promises its body:
/// the forms have always been laid out 54 px narrower than their card.
pub const FORM_SLACK: f32 = 12.0;

/// The width a modal `modal_width` wide lays its form out in.
pub fn form_width(modal_width: f32) -> f32 {
    Modal::body_width(modal_width) - FORM_SLACK
}

/// A stacked editor inside a settings group: its lines sit 8 px apart.
pub const GROUP_FIELD: FormFieldSizing = FormFieldSizing {
    gap: 8.0,
    ..FormFieldSizing::DEFAULT
};

/// The command a rule preview evaluates, tight over its actions.
pub const RULE_PREVIEW_FIELD: FormFieldSizing = FormFieldSizing {
    gap: 6.0,
    ..FormFieldSizing::DEFAULT
};

/// A field of the task editor: a quiet label on its own 18 px line.
pub const AUTOMATION_FIELD: FormFieldSizing = FormFieldSizing {
    label_muted: true,
    label_height: 18.0,
    hint_role: TextRole::Caption,
    gap: 8.0,
    ..FormFieldSizing::DEFAULT
};

/// The same field on the schedule step, whose rows sit a pixel further apart.
pub const AUTOMATION_SCHEDULE_FIELD: FormFieldSizing = FormFieldSizing {
    gap: 9.0,
    ..AUTOMATION_FIELD
};

/// The path of a new file: a 13 px label and a 12 px error.
pub const NEW_FILE_FIELD: FormFieldSizing = FormFieldSizing {
    label_role: TextRole::Label,
    hint_role: TextRole::Caption,
    gap: 8.0,
    ..FormFieldSizing::DEFAULT
};

/// The notifications bell of the status bar: a wider glyph beside its count.
pub const STATUS_BELL: StatusBarItemSizing = StatusBarItemSizing {
    icon_width: 15.0,
    ..StatusBarItemSizing::COUNTS
};

/// A shortcut of the empty editor: a larger title far from its keycaps.
pub const EMPTY_EDITOR_ROW: RowSizing = RowSizing {
    height: Some(32.0),
    gap: 20.0,
    title_size: 13.0,
    ..RowSizing::COMPACT
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modal_form_stays_54_px_narrower_than_its_card() {
        for width in 240..=860 {
            let width = width as f32;
            assert_eq!(form_width(width), width - 54.0);
            assert_eq!(form_width(width + 0.5), width + 0.5 - 54.0);
        }
        assert_eq!(FORM_SLACK, 54.0 - (560.0 - Modal::body_width(560.0)));
    }
}
