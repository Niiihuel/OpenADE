//! The dock's tab overview: every open tab with its controls, the tabs
//! closed recently, and the surfaces a new tab can open.
use super::*;

pub(super) fn content(state: &ViewState, width: f32) -> Element {
    let query = state.dock_overview_query.trim().to_lowercase();
    let matches = |title: &str, kind: SideTab| {
        let kind_name = match kind {
            SideTab::Context => "entorno",
            SideTab::Files => "archivos",
            SideTab::Activity => "actividad",
            SideTab::CodeReview => "repositorio git",
            SideTab::SessionChanges => "cambios chat conversación",
            SideTab::Browser => "navegador web",
            SideTab::Terminal => "terminal",
            SideTab::FilePreview => "vista de archivo",
            SideTab::CodeSnapshot => "vista previa capturada",
        };
        query.is_empty() || title.to_lowercase().contains(&query) || kind_name.contains(&query)
    };
    let filtered_tabs: Vec<_> = state
        .dock_tabs
        .iter()
        .filter(|tab| matches(&tab.title, tab.kind))
        .collect();
    let filtered_recent: Vec<_> = state
        .dock_recently_closed
        .iter()
        .filter(|tab| matches(&tab.title, tab.kind))
        .collect();
    let mut tabs = vec![
        Search::new(
            "dock_overview_query",
            &state.dock_overview_query,
            "Buscar pestañas…",
            theme::component_palette(),
        )
        .render()
        .size(None, Some(34.0)),
        Element::row(vec![
            UiText::eyebrow("PESTAÑAS ABIERTAS", theme::component_palette()).render(),
            Element::spacer().flex(1.0),
            if state.dock_tabs.is_empty() {
                Element::spacer().size(Some(86.0), Some(24.0))
            } else {
                button(
                    "dock_close_all",
                    "Cerrar todas",
                    theme::HOVER,
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(11.0)
                .min_size(0.0, 0.0)
                .size(Some(86.0), Some(24.0))
            },
        ])
        .size(None, Some(24.0)),
    ];
    if filtered_tabs.is_empty() {
        tabs.push(
            UiText::new(
                if state.dock_tabs.is_empty() {
                    "No hay pestañas abiertas"
                } else {
                    "Sin coincidencias abiertas"
                },
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render()
            .padding(8.0),
        );
    }
    let first_id = state.dock_tabs.first().map(|tab| tab.id);
    let last_id = state.dock_tabs.last().map(|tab| tab.id);
    for tab in filtered_tabs {
        tabs.push(
            Element::row(vec![
                button(
                    format!("dock_tab:{}", tab.id),
                    shorten(&tab.title, 25),
                    if tab.active {
                        theme::SELECTED
                    } else {
                        theme::BACKGROUND
                    },
                    theme::FOREGROUND,
                )
                .font_size(12.0)
                .min_size(0.0, 0.0)
                .flex(1.0)
                .size(None, Some(28.0)),
                button(
                    format!("dock_close_others:{}", tab.id),
                    "Solo",
                    theme::BACKGROUND,
                    theme::FOREGROUND_SUBTLE,
                )
                .font_size(10.0)
                .min_size(0.0, 0.0)
                .size(Some(34.0), Some(28.0)),
                if first_id == Some(tab.id) {
                    Element::icon(Icon::ArrowUp)
                        .foreground(theme::BORDER)
                        .size(Some(24.0), Some(24.0))
                } else {
                    UiIconButton::new(
                        format!("dock_move_up:{}", tab.id),
                        Icon::ArrowUp,
                        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
                    )
                    .variant(ButtonVariant::Subtle)
                    .size(ControlSize::Dense)
                    .tooltip_with(TooltipSpec::new("Subir").placement(TooltipPlacement::Above))
                    .render()
                },
                if last_id == Some(tab.id) {
                    Element::icon(Icon::ArrowDown)
                        .foreground(theme::BORDER)
                        .size(Some(24.0), Some(24.0))
                } else {
                    UiIconButton::new(
                        format!("dock_move_down:{}", tab.id),
                        Icon::ArrowDown,
                        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
                    )
                    .variant(ButtonVariant::Subtle)
                    .size(ControlSize::Dense)
                    .tooltip_with(TooltipSpec::new("Bajar").placement(TooltipPlacement::Above))
                    .render()
                },
                if tab.closable {
                    UiIconButton::new(
                        format!("dock_close:{}", tab.id),
                        Icon::Close,
                        theme::action_palette(theme::BACKGROUND, theme::FOREGROUND_SUBTLE),
                    )
                    .variant(ButtonVariant::Subtle)
                    .size(ControlSize::Compact)
                    .tooltip_with(
                        TooltipSpec::new("Cerrar pestaña").placement(TooltipPlacement::Above),
                    )
                    .render()
                } else {
                    Element::spacer().size(Some(28.0), Some(28.0))
                },
            ])
            .gap(4.0)
            .size(None, Some(28.0)),
        );
    }
    tabs.push(Element::spacer().size(None, Some(8.0)));
    tabs.push(UiText::eyebrow("RECIENTES (MÁX. 8)", theme::component_palette()).render());
    if filtered_recent.is_empty() {
        tabs.push(
            UiText::new(
                if state.dock_recently_closed.is_empty() {
                    "No hay pestañas recientes"
                } else {
                    "Sin coincidencias recientes"
                },
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render()
            .padding(8.0),
        );
    }
    for tab in filtered_recent {
        let row = if tab.restorable {
            Button::new(
                format!("dock_reopen:{}", tab.id),
                shorten(&tab.title, 25),
                theme::action_palette(theme::BACKGROUND, theme::FOREGROUND),
            )
            .icon(Icon::History)
            .variant(ButtonVariant::Subtle)
            .render()
            .font_size(12.0)
            .min_size(0.0, 0.0)
            .size(None, Some(28.0))
        } else {
            UiText::new(
                format!("{} · sesión cerrada", shorten(&tab.title, 25)),
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render()
            .size(None, Some(28.0))
        };
        tabs.push(row);
    }
    tabs.push(Element::spacer().size(None, Some(8.0)));
    tabs.push(UiText::eyebrow("ABRIR PESTAÑA", theme::component_palette()).render());
    tabs.push(open_actions((width - 24.0).max(150.0)));
    Element::column(tabs).gap(5.0).padding(12.0)
}

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
