//! Shared density of workspace sidebars. The header, the panel and the
//! search field are limn's `PanelHeader`, `SidebarPanel` and `SearchField`.
use openide_ui::ControlSize;

pub(crate) const INSET: f32 = 6.0;
pub(crate) const GAP: f32 = 6.0;
pub(crate) const SEARCH_HEIGHT: f32 = 28.0;
pub(crate) const TOOL_SIZE: f32 = ControlSize::Dense.height();
pub(crate) const NAV_ROW_HEIGHT: f32 = 28.0;
pub(crate) const ACCOUNT_FOOTER_HEIGHT: f32 = TOOL_SIZE + INSET * 2.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_panel_parity_tests::assert_same;
    use crate::{theme, ui_sizing};
    use openide_ui::{
        ButtonVariant, Component, Element, FontFamily, HeaderSizing, Icon, IconButton, PanelHeader,
        SearchField, SidebarPanel, TooltipPlacement, TooltipSpec,
    };

    const HEADER_HEIGHT: f32 = 26.0;

    fn legacy_title(text: impl Into<String>) -> Element {
        Element::truncated_text(text)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::FOREGROUND)
            .flex(1.0)
    }

    fn legacy_header(label: Element, actions: Vec<Element>, width: f32) -> Element {
        let mut children = vec![label.flex(1.0)];
        children.extend(actions);
        Element::row(children)
            .gap(2.0)
            .size(Some(width), Some(HEADER_HEIGHT))
    }

    fn legacy_search(id: &str, value: &str, placeholder: &str, width: f32) -> Element {
        SearchField::new(id, value, placeholder, theme::component_palette())
            .size(ControlSize::Compact)
            .width(width)
            .render()
            .size(Some(width), Some(SEARCH_HEIGHT))
    }

    fn legacy_panel(children: Vec<Element>, width: f32, height: f32) -> Element {
        Element::column(children)
            .gap(GAP)
            .padding(INSET)
            .background(theme::SIDEBAR)
            .size(Some(width), Some(height))
    }

    fn tools() -> Vec<Element> {
        ["refresh", "more"]
            .into_iter()
            .map(|id| {
                IconButton::new(id, Icon::Refresh, theme::sidebar_palette())
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Dense)
                    .tooltip_with(TooltipSpec::new("Actualizar").placement(TooltipPlacement::Below))
                    .render()
            })
            .collect()
    }

    #[test]
    fn the_sidebar_header_matches_its_legacy_body() {
        for title in [
            "Cambios",
            "Un título de barra lateral demasiado largo para su ancho",
        ] {
            for width in [168.0, 252.0, 408.0] {
                for actions in [Vec::new(), tools()] {
                    assert_same(
                        "sidebar header",
                        legacy_header(legacy_title(title), actions.clone(), width),
                        PanelHeader::new(title, theme::sidebar_palette())
                            .trailing(actions)
                            .width(width)
                            .sizing(ui_sizing::SIDEBAR_HEADER)
                            .render(),
                    );
                }
            }
        }
    }

    #[test]
    fn the_sidebar_panel_and_search_match_their_legacy_bodies() {
        for width in [180.0, 264.0, 420.0] {
            let inner = (width - INSET * 2.0).max(0.0);
            let blocks = || {
                vec![
                    legacy_header(legacy_title("Buscar"), tools(), inner),
                    legacy_search("query", "limn", "Buscar en el proyecto…", inner),
                ]
            };
            assert_same(
                "sidebar panel",
                legacy_panel(blocks(), width, 320.0),
                SidebarPanel::new(blocks(), theme::sidebar_palette())
                    .size(width, 320.0)
                    .render(),
            );
            for value in ["", "limn"] {
                assert_same(
                    "sidebar search",
                    legacy_search("query", value, "Buscar en el proyecto…", inner),
                    SearchField::new(
                        "query",
                        value,
                        "Buscar en el proyecto…",
                        theme::component_palette(),
                    )
                    .size(ControlSize::Compact)
                    .width(inner)
                    .render(),
                );
            }
        }
    }

    /// Every header the app moved onto `PanelHeader` was this row: the
    /// element that takes the width left, then its tools, in one profile.
    #[test]
    fn a_panel_header_is_the_row_each_screen_built_by_hand() {
        let legacy = |sizing: HeaderSizing, width: Option<f32>| {
            let mut children = vec![legacy_title("Entorno").flex(1.0)];
            children.extend(tools());
            Element::row(children)
                .gap(sizing.gap)
                .padding(sizing.padding)
                .size(width, Some(sizing.height))
        };
        for sizing in [
            ui_sizing::SIDEBAR_HEADER,
            ui_sizing::FILES_HEADER_COMPACT,
            ui_sizing::FILES_HEADER,
            ui_sizing::TALL_HEADER,
            ui_sizing::PANE_HEADER,
            ui_sizing::NOTIFICATIONS_HEADER,
            HeaderSizing::CARD,
            HeaderSizing::POPOVER,
        ] {
            for width in [None, Some(200.0), Some(520.0)] {
                let mut header = PanelHeader::new("", theme::sidebar_palette())
                    .leading(legacy_title("Entorno"))
                    .trailing(tools())
                    .sizing(sizing);
                if let Some(width) = width {
                    header = header.width(width);
                }
                assert_same("panel header", legacy(sizing, width), header.render());
            }
        }
    }
}
