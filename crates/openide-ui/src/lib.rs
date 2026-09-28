//! A small, original UI engine for OpenIDE.
//!
//! Application code constructs an [`Element`] tree. [`UiTree`] lays it out,
//! retains interaction state by stable IDs, and emits backend-neutral drawing
//! commands. A host such as eframe translates pointer and keyboard events to
//! [`UiEvent`] and paints the resulting [`Frame`].

mod components;
mod composer;
mod dropdown_menu;
mod element;
mod geometry;
mod menu;
mod motion;
mod scroll_area;
mod tree;
mod tree_view;
mod typography;

pub use components::{
    Button, ButtonContent, ButtonSizing, ButtonVariant, Component, ComponentPalette, ControlSize,
    ControlState, DropdownList, DropdownOption, IconButton, Popover, SearchField, SegmentedControl,
    Switch, Tab, Tabs, Tooltip,
};
pub use composer::Composer;
pub use dropdown_menu::{DropdownMenu, DropdownMenuEntry, DropdownMenuItem};
pub use element::{
    Border, BoxShadow, Element, ElementKind, FontFamily, Icon, PopoverPlacement, Style,
    TabSelection, TextSpan, TooltipPlacement, TooltipStyle, UniformList, DEFAULT_FONT_SIZE,
    ICON_SIZE,
};
pub use geometry::{Color, Point, Rect, Size};
pub use menu::{MenuItem, MenuSeparator};
pub use motion::{Easing, MenuMotion, ScalarTransition};
pub use scroll_area::{ScrollArea, ScrollbarAppearance, ScrollbarPlacement};
pub use tree::{
    DrawCommand, FontMetrics, Frame, HitKind, HitRegion, Key, UiAction, UiEvent, UiTree,
};
pub use tree_view::{guides_for_depths, TreeGuides, TreeRow};
pub use typography::{truncate_measured, wrap_measured, TextRole};
