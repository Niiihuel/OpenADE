//! Shared density of workspace sidebars. The header, the panel and the
//! search field are limn's `PanelHeader`, `SidebarPanel` and `SearchField`.
use openide_ui::ControlSize;

pub(crate) const INSET: f32 = 6.0;
pub(crate) const GAP: f32 = 6.0;
pub(crate) const SEARCH_HEIGHT: f32 = 28.0;
pub(crate) const TOOL_SIZE: f32 = ControlSize::Dense.height();
pub(crate) const NAV_ROW_HEIGHT: f32 = 28.0;
pub(crate) const ACCOUNT_FOOTER_HEIGHT: f32 = TOOL_SIZE + INSET * 2.0;
