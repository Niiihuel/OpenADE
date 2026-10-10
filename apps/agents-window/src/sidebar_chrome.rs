//! Shared density and interaction surfaces for workspace sidebars.
use crate::theme;
use openide_ui::{Component, ControlSize, Element, FontFamily, SearchField};

pub(crate) const INSET: f32 = 6.0;
pub(crate) const GAP: f32 = 6.0;
pub(crate) const HEADER_HEIGHT: f32 = 26.0;
pub(crate) const SEARCH_HEIGHT: f32 = 28.0;
pub(crate) const TOOL_SIZE: f32 = ControlSize::Dense.height();
pub(crate) const NAV_ROW_HEIGHT: f32 = 28.0;
pub(crate) const ACCOUNT_FOOTER_HEIGHT: f32 = TOOL_SIZE + INSET * 2.0;

pub(crate) fn title(text: impl Into<String>) -> Element {
    Element::truncated_text(text)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(theme::FOREGROUND)
        .flex(1.0)
}

pub(crate) fn header(label: Element, actions: Vec<Element>, width: f32) -> Element {
    let mut children = vec![label.flex(1.0)];
    children.extend(actions);
    Element::row(children)
        .gap(2.0)
        .size(Some(width), Some(HEADER_HEIGHT))
}

pub(crate) fn search(id: &str, value: &str, placeholder: &str, width: f32) -> Element {
    // This is the existing Extensions search, including its native input
    // focus/hover treatment and icon inset. All activities use the same field.
    SearchField::new(id, value, placeholder, theme::component_palette())
        .size(ControlSize::Compact)
        .width(width)
        .render()
        .size(Some(width), Some(SEARCH_HEIGHT))
}

pub(crate) fn panel(children: Vec<Element>, width: f32, height: f32) -> Element {
    Element::column(children)
        .gap(GAP)
        .padding(INSET)
        .background(theme::SIDEBAR)
        .size(Some(width), Some(height))
}
