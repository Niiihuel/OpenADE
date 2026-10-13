//! What a tab can be opened on, as the empty dock offers it.
use super::*;

pub(super) fn open_actions(width: f32) -> Element {
    let action_width = width.max(150.0);
    let palette = theme::action_palette(theme::CARD, theme::FOREGROUND);
    Element::column(
        [
            ("dock_open:files", Icon::Folder, "Archivos"),
            ("dock_open:activity", Icon::History, "Actividad"),
            ("dock_open:review", Icon::Code, "Cambios de Git"),
            ("dock_open:terminal", Icon::Terminal, "Terminal"),
            ("dock_open:browser", Icon::Globe, "Navegador"),
            ("dock_open:changes", Icon::Code, "Cambios del chat"),
        ]
        .into_iter()
        .map(|(id, icon, title)| {
            ListRow::new(id, title, palette)
                .icon(icon)
                .icon_ink(theme::FOREGROUND)
                .filled()
                .sizing(crate::ui_sizing::DOCK_OPEN)
                .render()
                .size(Some(action_width), crate::ui_sizing::DOCK_OPEN.height)
        })
        .collect(),
    )
    .gap(6.0)
}
