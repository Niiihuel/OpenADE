//! Shared integrated resource tabs. limn's `TabStrip` draws them; this module
//! keeps the names the docks read and the width estimate, which is the app's.
use openide_ui::TabStrip;

// The active tab reaches the content below. Its 24px contents and the fixed
// toolbar share a center at y=16; the last 4px form the connection to the body.
pub(crate) const HEADER_HEIGHT: f32 = TabStrip::HEADER_HEIGHT;
// Resource strips share the editor's inset, including lower panel sections
// and terminal sessions, so their first tab and trailing tools line up.
pub(crate) const HEADER_SIDE_INSET: f32 = TabStrip::HEADER_SIDE_INSET;
pub(crate) const TOOL_SIZE: f32 = TabStrip::TOOL_SIZE;
pub(crate) const TAB_HEIGHT: f32 = TabStrip::TAB_HEIGHT;
#[cfg(test)]
pub(crate) const CLOSE_SIZE: f32 = TabStrip::CLOSE_SIZE;
#[cfg(test)]
pub(crate) const TAB_GAP: f32 = 0.0;
#[cfg(test)]
pub(crate) const STRIP_BACKGROUND: openide_ui::Color = crate::theme::PANEL;
/// Widest tab outside the IDE's editor group, which allows longer file names.
pub(crate) const TAB_MAX_WIDTH: f32 = 208.0;

pub(crate) fn tab_width(title: &str, available: f32, max: f32) -> f32 {
    // This estimate reserves ordinary Sans13 names. The native measurer still
    // owns text paint; long labels fade and the strip scrolls continuously.
    (68.0 + title.chars().count() as f32 * 7.8)
        .clamp(112.0, max)
        .min((available - 32.0).max(0.0))
}

/// The presentation limn's `TabStrip` replaced, kept so the replacement stays
/// provably identical. Delete with the final audit (S18).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use std::time::{Duration, Instant};
    use openide_ui::{
        ButtonVariant, Color, Component, ControlSize, CornerRadii, Element, FontFamily, Icon,
        IconButton, Point, Separator, Size, TabItem, Tooltip, TooltipPlacement, UiEvent, UiTree,
    };

    struct RoundedTab<'a> {
        id: String,
        extent_id: String,
        title: &'a str,
        help: &'a str,
        icon: Icon,
        active: bool,
        width: f32,
        close: Element,
        tooltip_placement: TooltipPlacement,
    }

    fn rounded_tab(tab: RoundedTab<'_>) -> Element {
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

    fn tab_surface(id: &str, content: Element, active: bool, width: f32) -> Element {
        let connection = if active {
            Element::spacer()
        } else {
            // Hovering an inactive tab must not erase the boundary with the body.
            Element::column(vec![
                Element::spacer().flex(1.0),
                Separator::horizontal(theme::component_palette()).render(),
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

    fn align_tabs(tabs: Element) -> Element {
        let width = tabs.style.width;
        Element::column(vec![
            Element::spacer().size(None, Some(HEADER_HEIGHT - TAB_HEIGHT)),
            tabs,
        ])
        .size(width, Some(HEADER_HEIGHT))
    }

    /// The baseline is behind the tabs, so only the active tab opens onto the body.
    /// Tools stay centered and outside the scrollable portion of the header.
    fn header(
        leading: Element,
        tools: Element,
        width: f32,
        side_inset: f32,
        gap: f32,
    ) -> Element {
        let tools_width = tools.style.width;
        let tools = tools.centered_y().size(tools_width, Some(HEADER_HEIGHT));
        let content = Element::row(vec![
            Element::spacer().size(Some(side_inset), None),
            Element::row(vec![leading, tools]).gap(gap).flex(1.0),
            Element::spacer().size(Some(side_inset), None),
        ])
        .size(Some(width), Some(HEADER_HEIGHT));
        Element::overlay(
            Element::column(vec![
                Element::spacer().flex(1.0),
                Separator::horizontal(theme::component_palette()).render(),
            ])
            .background(STRIP_BACKGROUND)
            .size(Some(width), Some(HEADER_HEIGHT)),
            content,
        )
        .size(Some(width), Some(HEADER_HEIGHT))
        .clip_children()
    }

    fn scrolling_tabs(
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
                        Separator::vertical(theme::component_palette())
                            .length(14.0)
                            .render(),
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

    /// Equal frames idle and, for each region, hovered, with its hint open
    /// and pressed.
    fn assert_same(name: &str, legacy: Element, current: Element) {
        for width in [360.0, 900.0] {
            let size = Size::new(width, 120.0);
            let frames = |element: Element| {
                let mut tree = UiTree::new(Element::column(vec![
                    Element::row(vec![element, Element::spacer().flex(1.0)]),
                    Element::spacer().flex(1.0),
                ]));
                tree.set_reduced_motion(true);
                let idle = tree.layout(size);
                let mut frames = Vec::new();
                for region in idle.regions.clone() {
                    let point = Point::new(region.rect.x + 4.0, region.rect.y + 4.0);
                    tree.handle_event(UiEvent::PointerMove(point));
                    frames.push(tree.layout(size));
                    frames.push(tree.layout_at(size, Instant::now() + Duration::from_secs(2)));
                    tree.handle_event(UiEvent::PointerDown(point));
                    frames.push(tree.layout(size));
                    tree.handle_event(UiEvent::PointerUp(point));
                }
                frames.insert(0, idle);
                frames
            };
            assert_eq!(frames(current.clone()), frames(legacy.clone()), "{name} width={width}");
        }
    }

    fn close(id: &str) -> Element {
        IconButton::new(id, Icon::Close, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .render()
    }

    const TITLES: [&str; 4] = ["commands.rs", "Terminal", "native_streaming_app_tests.rs", "a"];

    fn legacy_tab(index: usize, active: bool, width: f32) -> Element {
        rounded_tab(RoundedTab {
            id: format!("tab:{index}"),
            extent_id: format!("extent:{index}"),
            title: TITLES[index % 4],
            help: "/work/src/commands.rs",
            icon: Icon::Code,
            active,
            width,
            close: close(&format!("close:{index}")),
            tooltip_placement: TooltipPlacement::Below,
        })
    }

    fn item(index: usize, active: bool, width: f32) -> TabItem {
        TabItem::new(
            format!("tab:{index}"),
            format!("extent:{index}"),
            Icon::Code,
            TITLES[index % 4],
            width,
        )
        .tooltip("/work/src/commands.rs")
        .tooltip_placement(TooltipPlacement::Below)
        .active(active)
        .close(close(&format!("close:{index}")))
    }

    #[test]
    fn the_shim_palette_holds_the_colors_the_strip_painted() {
        let palette = theme::tab_strip_palette();
        assert_eq!(
            (palette.surface, palette.raised, palette.hover),
            (theme::BACKGROUND, STRIP_BACKGROUND, theme::HOVER)
        );
        assert_eq!(
            (palette.text, palette.muted, palette.border),
            (theme::FOREGROUND, theme::FOREGROUND_SUBTLE, theme::component_palette().border)
        );
    }

    #[test]
    fn limn_tab_item_matches_rounded_tab() {
        let palette = theme::tab_strip_palette();
        for active in [false, true] {
            for width in [112.0, 208.0] {
                assert_same(
                    &format!("tab active={active} width={width}"),
                    legacy_tab(0, active, width),
                    item(0, active, width).render(palette),
                );
            }
        }
        // Without a close control the slot stays reserved, as the dock drew it.
        let legacy = rounded_tab(RoundedTab {
            id: "tab:0".into(),
            extent_id: "extent:0".into(),
            title: "Contexto",
            help: "Contexto",
            icon: Icon::PanelRight,
            active: false,
            width: 140.0,
            close: Element::spacer().size(Some(CLOSE_SIZE), Some(CLOSE_SIZE)),
            tooltip_placement: TooltipPlacement::Below,
        });
        let current = TabItem::new("tab:0", "extent:0", Icon::PanelRight, "Contexto", 140.0)
            .tooltip_placement(TooltipPlacement::Below)
            .render(palette);
        assert_same("tab without close", legacy, current);
    }

    #[test]
    fn limn_tab_surface_and_align_match_the_section_tabs() {
        let palette = theme::tab_strip_palette();
        let content = || Element::icon(Icon::Terminal).foreground(theme::FOREGROUND);
        for active in [false, true] {
            for width in [28.0, 82.0] {
                assert_same(
                    &format!("surface active={active} width={width}"),
                    align_tabs(tab_surface("section", content(), active, width)),
                    TabStrip::align(TabStrip::surface("section", content(), active, width, palette)),
                );
            }
        }
    }

    #[test]
    fn limn_header_matches_the_dock_header_at_every_inset_and_gap() {
        let palette = theme::tab_strip_palette();
        let leading = || legacy_tab(0, true, 160.0).flex(1.0);
        let tools = || Element::row(vec![close("tool")]).size(Some(TOOL_SIZE), Some(TOOL_SIZE));
        for width in [360.0, 900.0] {
            for (inset, gap) in [(HEADER_SIDE_INSET, 8.0), (HEADER_SIDE_INSET, 0.0), (2.0, 2.0)] {
                assert_same(
                    &format!("header width={width} inset={inset} gap={gap}"),
                    header(leading(), tools(), width, inset, gap),
                    TabStrip::header(leading(), tools(), width, palette)
                        .side_inset(inset)
                        .gap(gap)
                        .render(),
                );
            }
        }
    }

    #[test]
    fn limn_tab_strip_matches_scrolling_tabs() {
        let palette = theme::tab_strip_palette();
        for count in [1, 3, 12] {
            for active in [None, Some(0), Some(count / 2), Some(count - 1)] {
                let widths: Vec<_> = (0..count)
                    .map(|index| tab_width(TITLES[index % 4], 600.0, TAB_MAX_WIDTH))
                    .collect();
                let legacy = scrolling_tabs(
                    "tabs",
                    (0..count)
                        .map(|index| legacy_tab(index, active == Some(index), widths[index]))
                        .collect(),
                    (0..count).map(|index| format!("tab:{index}")).collect(),
                    &widths,
                    active.map(|index| format!("extent:{index}")),
                    active,
                );
                let current = TabStrip::new(
                    "tabs",
                    (0..count)
                        .map(|index| item(index, active == Some(index), widths[index]))
                        .collect(),
                    palette,
                )
                .render();
                assert_same(&format!("strip count={count} active={active:?}"), legacy, current);
            }
        }
    }
}
