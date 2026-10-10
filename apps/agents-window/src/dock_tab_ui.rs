//! Shared integrated resource tabs. Hosts own identity/actions; this module only
//! supplies the same geometry, focus surface and scrolling treatment to docks.
use crate::theme;
use openide_ui::{
    Color, Component, CornerRadii, Element, FontFamily, Icon, Tooltip, TooltipPlacement,
};

// The active tab reaches the content below. Its 24px contents and the fixed
// toolbar share a center at y=16; the last 4px form the connection to the body.
pub(crate) const HEADER_HEIGHT: f32 = 32.0;
// Resource strips share the editor's inset, including lower panel sections
// and terminal sessions, so their first tab and trailing tools line up.
pub(crate) const HEADER_SIDE_INSET: f32 = 6.0;
pub(crate) const TOOL_SIZE: f32 = 24.0;
pub(crate) const TAB_HEIGHT: f32 = 28.0;
pub(crate) const CLOSE_SIZE: f32 = 24.0;
pub(crate) const TAB_GAP: f32 = 0.0;
pub(crate) const STRIP_BACKGROUND: Color = theme::PANEL;

pub(crate) fn file_action(state: &crate::view::ViewState, tab: u64, operation: &str) -> String {
    format!(
        "dock_file_action:{}",
        serde_json::to_string(&(
            &state.workspace,
            state.selected_session.as_deref().unwrap_or(""),
            tab,
            operation,
        ))
        .expect("file tab action is serializable")
    )
}

pub(crate) fn tab_width(title: &str, available: f32) -> f32 {
    // This estimate reserves ordinary Sans13 names. The native measurer still
    // owns text paint; long labels fade and the strip scrolls continuously.
    (68.0 + title.chars().count() as f32 * 7.8)
        .clamp(112.0, 208.0)
        .min((available - 32.0).max(0.0))
}

pub(crate) struct RoundedTab<'a> {
    pub id: String,
    pub extent_id: String,
    pub title: &'a str,
    pub help: &'a str,
    pub icon: Icon,
    pub active: bool,
    pub width: f32,
    pub close: Element,
    pub tooltip_placement: TooltipPlacement,
}

pub(crate) fn rounded_tab(tab: RoundedTab<'_>) -> Element {
    let ink = if tab.active {
        theme::FOREGROUND
    } else {
        theme::FOREGROUND_SUBTLE
    };
    let content = Element::row(vec![
        Element::icon(tab.icon)
            .foreground(ink)
            .size(Some(16.0), Some(CLOSE_SIZE)),
        Element::faded_text(tab.title, 14.0)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .foreground(ink)
            .flex(1.0)
            .size(None, Some(CLOSE_SIZE)),
        tab.close,
    ])
    .gap(6.0)
    .size(None, Some(TOOL_SIZE))
    .flex(1.0);
    let content = Element::row(vec![
        Element::spacer().size(Some(8.0), None),
        content,
        Element::spacer().size(Some(2.0), None),
    ])
    .size(None, Some(TOOL_SIZE));
    let select = tab_surface(&tab.id, content, tab.active, tab.width);
    let select = Tooltip::new(select, tab.id, tab.help, theme::component_palette())
        .placement(tab.tooltip_placement)
        .render();
    // Passive full-width extent includes the independently clickable close.
    Tooltip::new(select, tab.extent_id, tab.help, theme::component_palette())
        .placement(tab.tooltip_placement)
        .render()
        .size(Some(tab.width), Some(TAB_HEIGHT))
}

pub(crate) fn tab_surface(id: &str, content: Element, active: bool, width: f32) -> Element {
    let connection = if active {
        Element::spacer()
    } else {
        // Hovering an inactive tab must not erase the boundary with the body.
        Element::column(vec![
            Element::spacer().flex(1.0),
            Element::spacer()
                .background(theme::BORDER)
                .size(None, Some(1.0)),
        ])
    }
    .size(None, Some(TAB_HEIGHT - TOOL_SIZE));
    Element::clickable(
        id,
        Element::column(vec![content.size(None, Some(TOOL_SIZE)), connection]),
    )
    .background(if active {
        theme::BACKGROUND
    } else {
        Color::TRANSPARENT
    })
    .hover_background(if active {
        theme::BACKGROUND
    } else {
        theme::HOVER
    })
    .pressed_background(theme::BACKGROUND)
    .transition_ms(120)
    .corner_radii(CornerRadii::top(8.0))
    .size(Some(width), Some(TAB_HEIGHT))
}

pub(crate) fn align_tabs(tabs: Element) -> Element {
    let width = tabs.style.width;
    Element::column(vec![
        Element::spacer().size(None, Some(HEADER_HEIGHT - TAB_HEIGHT)),
        tabs,
    ])
    .size(width, Some(HEADER_HEIGHT))
}

/// The baseline is behind the tabs, so only the active tab opens onto the body.
/// Tools stay centered and outside the scrollable portion of the header.
pub(crate) fn header(
    leading: Element,
    tools: Element,
    width: f32,
    side_inset: f32,
    gap: f32,
) -> Element {
    let tools_width = tools.style.width;
    let tools = Element::column(vec![
        Element::spacer().flex(1.0),
        tools,
        Element::spacer().flex(1.0),
    ])
    .size(tools_width, Some(HEADER_HEIGHT));
    let content = Element::row(vec![
        Element::spacer().size(Some(side_inset), None),
        Element::row(vec![leading, tools]).gap(gap).flex(1.0),
        Element::spacer().size(Some(side_inset), None),
    ])
    .size(Some(width), Some(HEADER_HEIGHT));
    Element::overlay(
        Element::column(vec![
            Element::spacer().flex(1.0),
            Element::spacer()
                .background(theme::BORDER)
                .size(None, Some(1.0)),
        ])
        .background(STRIP_BACKGROUND)
        .size(Some(width), Some(HEADER_HEIGHT)),
        content,
    )
    .size(Some(width), Some(HEADER_HEIGHT))
    .clip_children()
}

pub(crate) fn scrolling_tabs(
    id: &str,
    tabs: Vec<Element>,
    action_ids: Vec<String>,
    widths: &[f32],
    reveal: Option<String>,
    active_index: Option<usize>,
) -> Element {
    let content_width =
        widths.iter().sum::<f32>() + TAB_GAP * widths.len().saturating_sub(1) as f32;
    let count = tabs.len();
    let tabs = tabs
        .into_iter()
        .enumerate()
        .map(|(index, tab)| {
            if index + 1 == count || active_index == Some(index) || active_index == Some(index + 1)
            {
                return tab;
            }
            let separator = Element::column(vec![
                Element::spacer().size(None, Some(5.0)),
                Element::row(vec![
                    Element::spacer().flex(1.0),
                    Element::spacer()
                        .background(theme::BORDER)
                        .size(Some(1.0), Some(14.0)),
                ])
                .size(None, Some(14.0)),
            ])
            .size(Some(widths[index]), Some(TAB_HEIGHT));
            Element::overlay(tab, separator)
                .size(Some(widths[index]), Some(TAB_HEIGHT))
                .clip_children()
        })
        .collect();
    let row = Element::tab_strip(tabs, action_ids)
        .gap(TAB_GAP)
        .size(Some(content_width), Some(TAB_HEIGHT));
    let strip = Element::horizontal_scroll(id, row)
        .horizontal_edge_fade(STRIP_BACKGROUND, 16.0)
        .size(None, Some(TAB_HEIGHT));
    let strip = if let Some(reveal) = reveal {
        strip.reveal_horizontal_item(reveal)
    } else {
        strip
    };
    align_tabs(strip).flex(1.0)
}
