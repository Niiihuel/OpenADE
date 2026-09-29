use openide_ui::{
    ButtonVariant, Color, Component, ControlSize, Element, Empty, FontFamily, Icon, IconButton,
    Kbd, Point, ScrollArea, ScrollbarAppearance, Search, Spinner, Tab, Tabs,
};

use crate::{command_center::Scope, theme};

use super::{divider, eyebrow, label, shorten, ViewState};

pub fn build_command_center(state: &ViewState, width: f32, height: f32) -> Element {
    let tab_widths = if width >= 330.0 {
        [56.0, 78.0, 62.0, 76.0]
    } else {
        [((width - 36.0) / 4.0).max(40.0); 4]
    };
    let tab_strip_width = tab_widths.iter().sum::<f32>() + 10.0;
    let scopes = [
        ("all", "Todo", Scope::All),
        ("commands", "Acciones", Scope::Commands),
        ("sessions", "Tareas", Scope::Sessions),
        ("files", "Archivos", Scope::Files),
    ];
    let scope_tabs = Tabs::new(
        scopes
            .into_iter()
            .zip(tab_widths)
            .map(|((id, title, scope), tab_width)| {
                Tab::new(format!("command_scope:{id}"), title, tab_width)
                    .active(state.command_scope == scope)
            })
            .collect(),
        theme::segmented_palette(),
    )
    .height(26.0)
    .render()
    .size(Some(tab_strip_width), Some(30.0));
    let mut rows = Vec::new();
    let mut section = None;
    for (index, result) in state.command_results.iter().enumerate() {
        if section != Some(result.section) {
            section = Some(result.section);
            rows.push(
                eyebrow(match result.section {
                    Scope::All => "RESULTADOS",
                    Scope::Commands => "ACCIONES",
                    Scope::Sessions => "TAREAS",
                    Scope::Files => "ARCHIVOS",
                })
                .padding(6.0),
            );
        }
        let icon = match result.section {
            Scope::All | Scope::Commands => Icon::Code,
            Scope::Sessions => Icon::Chat,
            Scope::Files => Icon::Folder,
        };
        let shortcut = match &result.target {
            crate::command_center::Target::NewTask => "Ctrl+N",
            crate::command_center::Target::OpenSettings => "",
            _ => "",
        };
        rows.push(
            Element::clickable(
                format!("command_result:{index}"),
                Element::row(vec![
                    Element::icon(icon)
                        .foreground(theme::FOREGROUND_SUBTLE)
                        .size(Some(16.0), Some(16.0)),
                    Element::column(vec![
                        label(shorten(&result.title, 62), theme::FOREGROUND).font_size(13.0),
                        label(shorten(&result.subtitle, 84), theme::FOREGROUND_SUBTLE)
                            .font_size(10.5),
                    ])
                    .gap(2.0)
                    .flex(1.0),
                    if shortcut.is_empty() {
                        Element::spacer()
                    } else {
                        Kbd::new(shortcut, theme::component_palette()).render()
                    },
                ])
                .gap(8.0)
                .padding(7.0)
                .size(None, Some(42.0)),
            )
            .background(if state.command_selected_index == index {
                theme::SELECTED
            } else {
                theme::PANEL
            })
            .hover_background(if state.command_selected_index == index {
                theme::SELECTED
            } else {
                theme::hover_for(theme::PANEL)
            })
            .pressed_background(theme::SELECTED)
            .radius(9.0),
        );
    }
    let spinner = || {
        Spinner::new("command_search_progress", theme::FOREGROUND)
            .size(32.0)
            .render()
    };
    let centered_spinner = || {
        Element::column(vec![
            Element::spacer().flex(1.0),
            Element::row(vec![
                Element::spacer().flex(1.0),
                spinner(),
                Element::spacer().flex(1.0),
            ]),
            Element::spacer().flex(1.0),
        ])
    };
    let results_body = if rows.is_empty() && state.command_search_loading {
        centered_spinner()
    } else if rows.is_empty() {
        let (title, description, icon, ink) = if !state.command_search_errors.is_empty() {
            (
                "No se pudo completar la búsqueda",
                "Revisa el origen de datos o prueba otro filtro.",
                Icon::Close,
                theme::DESTRUCTIVE,
            )
        } else if state.command_search_truncated {
            (
                "Hay más coincidencias",
                "Precisa la búsqueda para ver otros resultados.",
                Icon::Search,
                theme::FOREGROUND,
            )
        } else {
            (
                "No hay resultados",
                "Prueba otro término o cambia el filtro.",
                Icon::Search,
                theme::FOREGROUND,
            )
        };
        let mut palette = theme::component_palette();
        palette.text = ink;
        let extra_status =
            !state.command_search_errors.is_empty() || state.command_search_truncated;
        Empty::new(title, description, palette)
            .icon(icon)
            .compact()
            .height(height - if extra_status { 230.0 } else { 200.0 })
            .render()
    } else if state.command_search_loading {
        Element::column(vec![
            Element::column(rows).gap(2.0),
            Element::row(vec![
                Element::spacer().flex(1.0),
                spinner(),
                Element::spacer().flex(1.0),
            ])
            .padding(8.0),
        ])
        .gap(8.0)
    } else {
        Element::column(rows).gap(2.0)
    };
    let search_status = if state.command_search_loading {
        None
    } else if !state.command_search_errors.is_empty() {
        let label = if state.command_results.is_empty() {
            "Error de búsqueda"
        } else {
            "Resultados parciales"
        };
        Some((
            format!("{label} · {}", state.command_search_errors.join(" · ")),
            true,
        ))
    } else if state.command_search_truncated {
        Some((
            "Se muestran los primeros resultados. Precisa la búsqueda para ver más.".to_owned(),
            false,
        ))
    } else {
        None
    };
    let result_summary = if state.command_search_loading {
        String::new()
    } else {
        match (state.command_results.len(), state.command_search_truncated) {
            (0, _) => "Sin resultados".to_owned(),
            (1, false) => "1 resultado".to_owned(),
            (count, true) => format!("{count}+ resultados"),
            (count, false) => format!("{count} resultados"),
        }
    };
    let mut content = vec![
        Element::row(vec![
            label("Buscar", theme::FOREGROUND).font_size(14.0),
            Element::spacer().flex(1.0),
            IconButton::new(
                "command_close",
                Icon::CloseCircle,
                theme::action_palette(theme::PANEL, theme::FOREGROUND_SUBTLE),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .tooltip("Cerrar búsqueda")
            .tooltip_shortcut("Esc")
            .render(),
        ])
        .padding(2.0),
        Search::new(
            "command_query",
            &state.command_query,
            "Buscar acciones, tareas o archivos…",
            theme::component_palette(),
        )
        .size(ControlSize::Regular)
        .shortcut("Ctrl+K")
        .render()
        .size(None, Some(34.0)),
        scope_tabs,
    ];
    if let Some((message, error)) = search_status {
        content.push(
            Element::truncated_text(message)
                .foreground(if error {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND_SUBTLE
                })
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .padding(6.0)
                .size(None, Some(24.0)),
        );
    }
    content.extend([
        divider(),
        ScrollArea::new("command_results", results_body.padding(6.0))
            .appearance(ScrollbarAppearance::reserved())
            .tint(theme::FOREGROUND_SUBTLE)
            .render()
            .flex(1.0),
        divider(),
        Element::row(vec![
            Element::truncated_text(result_summary)
                .foreground(theme::FOREGROUND_SUBTLE)
                .font_family(FontFamily::Sans)
                .font_size(10.0)
                .flex(1.0),
            Kbd::new("↑↓", theme::component_palette()).render(),
            Element::truncated_text("Navegar")
                .foreground(theme::FOREGROUND_SUBTLE)
                .font_family(FontFamily::Sans)
                .font_size(10.0),
            Kbd::new("Enter", theme::component_palette()).render(),
            Element::truncated_text("Abrir")
                .foreground(theme::FOREGROUND_SUBTLE)
                .font_family(FontFamily::Sans)
                .font_size(10.0),
        ])
        .gap(5.0)
        .padding(3.0),
    ]);
    Element::panel(Element::column(content).gap(6.0))
        .padding(10.0)
        .background(theme::PANEL)
        .border(theme::BORDER, 1.0)
        .radius(12.0)
        .shadow(Color::rgba(0, 0, 0, 155), Point::new(0.0, 16.0), 28.0, 1.0)
        .size(Some(width), Some(height))
}
