//! Compact project search; uses the same input, tooltip and scroll components
//! as the rest of the workspace. No file reads happen while building the UI.
use super::*;
use crate::sidebar_chrome as chrome;
use openide_ui::TooltipPlacement;

pub(super) fn panel(state: &ViewState, width: f32, height: f32) -> Element {
    let search = &state.ide_search;
    let content_width = (width - chrome::INSET * 2.0).max(0.0);
    let query = chrome::search(
        "ide_search_query",
        &search.query,
        "Buscar en el proyecto…",
        content_width,
    );
    let case = Tooltip::new(
        theme::sidebar_palette()
            .interactive_surface(Element::clickable(
                "ide_search_case",
                label(
                    "Aa",
                    if search.case_sensitive {
                        theme::FOREGROUND
                    } else {
                        theme::FOREGROUND_SUBTLE
                    },
                )
                .font_size(12.0),
            ))
            .background(if search.case_sensitive {
                theme::SELECTED
            } else {
                theme::SIDEBAR
            })
            .radius(4.0)
            .padding(4.0)
            .size(Some(24.0), Some(24.0)),
        "ide_search_case",
        "Distinguir mayúsculas y minúsculas",
        theme::component_palette(),
    )
    .placement(TooltipPlacement::Below)
    .render();
    let submit = Tooltip::new(
        UiIconButton::new("ide_search_submit", Icon::Search, theme::sidebar_palette())
            .variant(ButtonVariant::Ghost)
            .enabled(!search.query.trim().is_empty())
            .render()
            .padding(4.0)
            .radius(4.0)
            .size(Some(24.0), Some(24.0)),
        "ide_search_submit",
        "Buscar en el proyecto",
        theme::component_palette(),
    )
    .shortcut("Enter")
    .placement(TooltipPlacement::Below)
    .render();
    let status = if search.running {
        "Buscando…".to_owned()
    } else if let Some(error) = &search.error {
        error.clone()
    } else if search.searched {
        format!(
            "{}{} líneas encontradas",
            search.hits.len(),
            if search.truncated { "+" } else { "" }
        )
    } else {
        "Enter para buscar en el proyecto".to_owned()
    };
    let mut rows = Vec::new();
    if search.searched && !search.running && search.error.is_none() && search.hits.is_empty() {
        rows.push(
            label("No se encontraron coincidencias.", theme::FOREGROUND_SUBTLE)
                .font_size(12.0)
                .padding(8.0),
        );
    }
    for (index, hit) in search.hits.iter().enumerate() {
        let path = hit.path.to_string_lossy();
        let file = hit
            .path
            .file_name()
            .unwrap_or(hit.path.as_os_str())
            .to_string_lossy();
        let parent = hit.path.parent().map(|parent| parent.to_string_lossy());
        let title_text = match parent.filter(|parent| !parent.is_empty()) {
            Some(parent) => format!("{file} · {parent}"),
            None => file.into_owned(),
        };
        let id = search.result_action(index);
        let title = Element::row(vec![
            Element::icon(openide_ui::file_icon(path.as_ref())).size(Some(16.0), Some(16.0)),
            Element::truncated_text(title_text)
                .foreground(theme::FOREGROUND)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .flex(1.0),
            label(&hit.line.to_string(), theme::FOREGROUND_SUBTLE).font_size(11.0),
        ])
        .gap(5.0)
        .size(None, Some(17.0));
        let snippet = Element::truncated_text(&hit.snippet)
            .foreground(theme::FOREGROUND_SUBTLE)
            .font_family(FontFamily::Monospace)
            .font_size(11.0)
            .size(None, Some(16.0));
        rows.push(
            Tooltip::new(
                theme::sidebar_palette()
                    .interactive_surface(Element::clickable(
                        &id,
                        Element::column(vec![title, snippet]).gap(1.0),
                    ))
                    .radius(4.0)
                    .padding(4.0)
                    .size(Some(content_width), Some(42.0)),
                id,
                format!("{}:{}", path, hit.line),
                theme::component_palette(),
            )
            .description(&hit.snippet)
            .placement(TooltipPlacement::Right)
            .render(),
        );
    }
    if search.truncated {
        rows.push(
            label(
                "Resultados limitados. Afiná el texto de búsqueda.",
                theme::FOREGROUND_SUBTLE,
            )
            .font_size(11.0)
            .padding(4.0),
        );
    }
    if search.skipped_files > 0 {
        rows.push(
            label(
                &format!(
                    "{} archivos grandes o inaccesibles omitidos.",
                    search.skipped_files
                ),
                theme::FOREGROUND_SUBTLE,
            )
            .font_size(11.0)
            .padding(4.0),
        );
    }
    chrome::panel(
        vec![
            chrome::header(chrome::title("Buscar"), vec![case, submit], content_width),
            query,
            label(
                &status,
                if search.error.is_some() {
                    theme::DESTRUCTIVE
                } else {
                    theme::FOREGROUND_SUBTLE
                },
            )
            .font_size(11.0),
            ScrollArea::new(
                format!("ide_search_results:{}", search.generation),
                Element::column(rows).gap(1.0),
            )
            .render()
            .flex(1.0),
        ],
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Key, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn ide_search_sidebar_typing_enter_and_search_button_dispatch_real_actions() {
        for width in [180.0, 264.0, 420.0] {
            let mut state = crate::view::tests::empty_state();
            let viewport = Size::new(width, 600.0);
            let mut tree = UiTree::new(panel(&state, width, viewport.height));
            let frame = tree.layout(viewport);
            for id in ["ide_search_query", "ide_search_case"] {
                let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width);
            }
            assert!(tree.focus("ide_search_query"));
            let typed = tree.handle_event(UiEvent::Text("Workspace".into()));
            assert!(typed.contains(&UiAction::InputChanged {
                id: "ide_search_query".into(),
                value: "Workspace".into(),
            }));
            let submit = tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            });
            assert!(submit.contains(&UiAction::Submitted {
                id: "ide_search_query".into(),
                value: "Workspace".into(),
            }));
            state.ide_search.query = "Workspace".into();
            tree.set_root(panel(&state, width, viewport.height));
            let frame = tree.layout(viewport);
            let button = frame
                .regions
                .iter()
                .find(|region| region.id == "ide_search_submit")
                .unwrap();
            assert!(button.rect.x + button.rect.width <= width);
            assert!(tree.focus("ide_search_submit"));
            assert!(tree
                .handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                })
                .contains(&UiAction::Clicked {
                    id: "ide_search_submit".into()
                }));
        }
    }
}
