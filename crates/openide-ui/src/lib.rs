//! A small, original UI engine for OpenIDE.
//!
//! Application code constructs an [`Element`] tree. [`UiTree`] lays it out,
//! retains interaction state by stable IDs, and emits backend-neutral drawing
//! commands. A host such as eframe translates pointer and keyboard events to
//! [`UiEvent`] and paints the resulting [`Frame`].

mod alert_dialog;
mod components;
mod composer;
mod date_picker;
mod date_time_picker;
mod dropdown_menu;
mod element;
mod empty;
mod geometry;
mod menu;
mod modal;
mod motion;
mod raster_image;
mod scroll_area;
mod shortcut;
mod skeleton;
mod spinner;
mod stepper;
mod time_picker;
mod tree;
mod tree_view;
mod typography;

pub use alert_dialog::AlertDialog;
pub use components::{
    Button, ButtonContent, ButtonSize, ButtonSizing, ButtonVariant, Component, ComponentPalette,
    ControlSize, ControlState, DropdownList, DropdownOption, IconButton, Kbd, Popover, Search,
    SearchField, SegmentedControl, Switch, Tab, Tabs, TextArea, TextField, Tooltip,
};
pub use composer::Composer;
pub use date_picker::DatePicker;
pub use date_time_picker::DateTimePicker;
pub use dropdown_menu::{DropdownMenu, DropdownMenuEntry, DropdownMenuItem};
pub use element::{
    Border, BoxShadow, Element, ElementKind, FontFamily, Icon, PopoverPlacement, SpinnerVariant,
    Style, TabSelection, TextSpan, TooltipPlacement, TooltipStyle, UniformList, DEFAULT_FONT_SIZE,
    ICON_SIZE,
};
pub use empty::Empty;
pub use geometry::{Color, Point, Rect, Size};
pub use menu::{MenuItem, MenuSeparator};
pub use modal::Modal;
pub use motion::{Easing, MenuMotion, ScalarTransition};
pub use raster_image::{RasterImage, RasterImageFormat};
pub use scroll_area::{ScrollArea, ScrollbarAppearance, ScrollbarPlacement};
pub use skeleton::Skeleton;
pub use spinner::Spinner;
pub use stepper::{Step, Stepper};
pub use time_picker::TimePicker;
pub use tree::{
    DrawCommand, FontMetrics, Frame, HitKind, HitRegion, Key, UiAction, UiEvent, UiTree,
};
pub use tree_view::{guides_for_depths, TreeGuides, TreeRow};
pub use typography::{truncate_measured, wrap_measured, TextRole};
