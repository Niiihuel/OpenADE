//! Search/fetch presentation from captured tool output. No network or file I/O.
use crate::{
    theme, tool_presentation,
    transcript::{ActivityStatus, ModelStreamStatus, TranscriptItem},
    view::ViewState,
};
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, Element, FileActivity, FontFamily, Icon,
    Tooltip, TooltipPlacement,
};
use serde_json::Value;
use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SearchResult {
    pub title: String,
    pub target: String,
    pub detail: String,
    pub web: bool,
    pub directory: bool,
}

pub(crate) fn supported(name: &str) -> bool {
    matches!(
        name,
        "list_files" | "find_files" | "search_files" | "search_text" | "web_search" | "web_fetch"
    )
}

pub(crate) fn valid_web_url(raw: &str) -> bool {
    raw.len() <= 4096
        && !raw.chars().any(char::is_control)
        && reqwest::Url::parse(raw)
            .is_ok_and(|url| matches!(url.scheme(), "https" | "http") && url.host_str().is_some())
}

fn bounded(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

pub(crate) fn captured_results(name: &str, output: &str, arguments: &Value) -> Vec<SearchResult> {
    let mut end = output.len().min(32 * 1024);
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    let output = &output[..end];
    if name == "web_search" {
        let Ok(value) = serde_json::from_str::<Value>(output) else {
            return Vec::new();
        };
        return value
            .as_array()
            .or_else(|| value.get("results").and_then(Value::as_array))
            .into_iter()
            .flatten()
            .take(12)
            .filter_map(|result| {
                let url = result.get("url")?.as_str()?;
                valid_web_url(url).then(|| SearchResult {
                    title: bounded(
                        result.get("title").and_then(Value::as_str).unwrap_or(url),
                        180,
                    ),
                    target: url.into(),
                    detail: bounded(
                        result
                            .get("snippet")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        320,
                    ),
                    web: true,
                    directory: false,
                })
            })
            .collect();
    }
    if name == "web_fetch" {
        return Vec::new();
    }
    output
        .lines()
        .take(24)
        .filter_map(|line| {
            if name == "list_files" {
                let (kind, filename) = line.split_once('\t')?;
                if !matches!(kind, "file" | "dir") {
                    return None;
                }
                let parent = arguments.get("path").and_then(Value::as_str).unwrap_or(".");
                return Some(SearchResult {
                    title: bounded(filename, 180),
                    target: if parent == "." {
                        filename.into()
                    } else {
                        format!("{}/{filename}", parent.trim_end_matches('/'))
                    },
                    detail: if kind == "dir" { "Carpeta" } else { "Archivo" }.into(),
                    web: false,
                    directory: kind == "dir",
                });
            }
            let (path, rest) = line.split_once(':')?;
            let (number, content) = rest.split_once(':')?;
            number.parse::<usize>().ok()?;
            if path.is_empty() {
                return None;
            }
            Some(SearchResult {
                title: bounded(&format!("{path}:{number}"), 200),
                target: path.to_owned(),
                detail: bounded(content.trim_start(), 320),
                web: false,
                directory: false,
            })
        })
        .collect()
}

pub(crate) fn result_action(owner: &str, row: &str, index: usize, hosted: bool) -> String {
    format!(
        "activity_result:{}",
        serde_json::to_string(&(owner, row, index, hosted)).unwrap()
    )
}

pub(crate) fn page_action(owner: &str, row: &str) -> String {
    format!(
        "activity_page:{}",
        serde_json::to_string(&(owner, row)).unwrap()
    )
}

pub(crate) fn is_page_action(action: &str) -> bool {
    matches!(action, "open_page" | "find_in_page")
}

pub(crate) fn result_row(
    owner: &str,
    row: &str,
    index: usize,
    result: &SearchResult,
    hosted: bool,
) -> Element {
    let directory_entry = !result.web && matches!(result.detail.as_str(), "Archivo" | "Carpeta");
    let title = Element::truncated_text(if directory_entry {
        &result.target
    } else {
        &result.title
    })
    .font_family(FontFamily::Sans)
    .font_size(12.0)
    .foreground(theme::FOREGROUND);
    let mut lines = vec![title];
    if !result.detail.is_empty() && !directory_entry {
        lines.push(
            Element::truncated_text(&result.detail)
                .font_family(if result.web {
                    FontFamily::Sans
                } else {
                    FontFamily::Monospace
                })
                .font_size(11.0)
                .foreground(theme::FOREGROUND_SUBTLE),
        );
    }
    if result.web {
        if let Ok(url) = reqwest::Url::parse(&result.target) {
            lines.push(
                Element::text(url.host_str().unwrap_or_default())
                    .font_family(FontFamily::Sans)
                    .font_size(10.0)
                    .foreground(theme::FOREGROUND_SUBTLE),
            );
        }
    }
    let action = result_action(owner, row, index, hosted);
    let content = Element::row(vec![
        Element::icon(if result.web {
            Icon::Globe
        } else if result.directory {
            openide_ui::folder_icon(&result.target, false)
        } else {
            openide_ui::file_icon(&result.target)
        })
        .size(Some(14.0), Some(14.0))
        .foreground(theme::FOREGROUND_SUBTLE),
        Element::column(lines).gap(3.0).flex(1.0),
    ])
    .gap(8.0);
    let content = if result.directory {
        Element::panel(content)
    } else {
        Element::clickable(&action, content).underline_on_hover()
    };
    Tooltip::new(
        if directory_entry {
            content.padding(5.0).size(None, Some(28.0))
        } else {
            content.padding(8.0)
        },
        action,
        &result.title,
        theme::component_palette(),
    )
    .description(&result.target)
    .placement(TooltipPlacement::Right)
    .render()
}

pub(crate) fn build(item: &TranscriptItem, state: &ViewState) -> Option<Element> {
    let (name, call, disclosure, args, status, running, output, note) = match item {
        TranscriptItem::Tool {
            id,
            name,
            arguments,
            activity,
            output,
            is_error,
        } if supported(name) => {
            let p = tool_presentation::present(
                name,
                arguments,
                *activity,
                *is_error,
                output.as_deref(),
            );
            (
                name.as_str(),
                id.as_str(),
                format!("tool:{id}"),
                Cow::Borrowed(arguments),
                p.title,
                *activity == ActivityStatus::Running,
                (!*is_error).then_some(output.as_deref()).flatten(),
                p.note,
            )
        }
        TranscriptItem::ToolPreview {
            id,
            call_id,
            name: Some(name),
            arguments,
            status,
            ..
        } if supported(name) => {
            let text = match status {
                ModelStreamStatus::Receiving => "Preparando búsqueda",
                ModelStreamStatus::Committed => "Pendiente",
                ModelStreamStatus::Failed => "Falló · sin ejecutar",
                ModelStreamStatus::Cancelled => "Cancelada · sin ejecutar",
                ModelStreamStatus::Interrupted => "Interrumpida · sin ejecutar",
                ModelStreamStatus::Discarded => "Descartada · sin ejecutar",
            };
            (
                name.as_str(),
                call_id.as_deref().unwrap_or(id),
                call_id
                    .as_ref()
                    .map(|id| format!("tool:{id}"))
                    .unwrap_or_else(|| id.clone()),
                Cow::Owned(tool_presentation::preview_arguments(arguments)),
                text.into(),
                *status == ModelStreamStatus::Receiving,
                None,
                None,
            )
        }
        _ => return None,
    };
    let owner = state.selected_session.as_deref().unwrap_or("draft");
    let expanded = state.conversation_details.is_expanded(&disclosure, false);
    let target = ["query", "pattern", "url", "path"]
        .into_iter()
        .find_map(|key| args.get(key).and_then(Value::as_str))
        .unwrap_or("Proyecto");
    let project_name = std::path::Path::new(&state.workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Proyecto");
    let display_target = if name == "list_files" && matches!(target, "." | "./" | "" | "Proyecto") {
        project_name
    } else {
        target.strip_prefix("./").unwrap_or(target)
    };
    let mut body = Vec::new();
    if expanded {
        if let Some(output) = output {
            let results = captured_results(name, output, &args);
            if results.is_empty() {
                body.push(
                    Element::text(if output == "no matches" {
                        "Sin coincidencias".into()
                    } else {
                        bounded(output, 4000)
                    })
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .foreground(theme::FOREGROUND_SUBTLE),
                );
            } else {
                let results_id = format!("results:{call}");
                let show_all = name != "list_files"
                    || state.conversation_details.is_expanded(&results_id, false);
                let visible = if show_all {
                    results.len()
                } else {
                    results.len().min(6)
                };
                body.extend(
                    results
                        .iter()
                        .take(visible)
                        .enumerate()
                        .map(|(index, result)| result_row(owner, call, index, result, false)),
                );
                if name == "list_files" && results.len() > 6 {
                    body.push(
                        Button::new(
                            format!("conversation_detail:{owner}:{results_id}"),
                            if show_all {
                                "Mostrar menos".into()
                            } else {
                                format!("Mostrar {} más", results.len() - visible)
                            },
                            theme::component_palette(),
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Compact)
                        .render(),
                    );
                }
                if output.lines().count() > 24 || output.len() > 32 * 1024 {
                    body.push(
                        Element::text("Vista limitada a los primeros resultados")
                            .font_family(FontFamily::Sans)
                            .font_size(11.0)
                            .foreground(theme::FOREGROUND_SUBTLE)
                            .padding(5.0),
                    );
                }
            }
        } else {
            let empty = note.as_deref().unwrap_or(match item {
                TranscriptItem::Tool {
                    activity: ActivityStatus::Running,
                    ..
                } => "Esperando resultados…",
                TranscriptItem::ToolPreview {
                    status: ModelStreamStatus::Receiving,
                    ..
                } => "Preparando entrada · todavía sin ejecutar",
                TranscriptItem::ToolPreview {
                    status: ModelStreamStatus::Committed,
                    ..
                } => "Entrada confirmada · pendiente de ejecución",
                TranscriptItem::ToolPreview {
                    status: ModelStreamStatus::Discarded,
                    ..
                } => "Llamada descartada · no se ejecutó",
                TranscriptItem::ToolPreview { .. } => "La llamada no se ejecutó",
                _ => "Sin resultado confirmado",
            });
            body.push(
                Element::text(empty)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(theme::FOREGROUND_SUBTLE),
            );
        }
    }
    let mut card = FileActivity::new(
        format!("conversation_detail:{owner}:{disclosure}"),
        bounded(display_target, 300),
        Element::column(body).gap(3.0),
        theme::component_palette(),
    )
    .icon(if name == "web_fetch" {
        Icon::Globe
    } else if name == "list_files" {
        Icon::Folder
    } else {
        Icon::Search
    })
    .status(status)
    .running(running)
    .expanded(expanded)
    .compact(true);
    if let TranscriptItem::Tool { id, arguments, .. } = item {
        if name == "list_files" {
            card = card.open_action(
                crate::file_activity_ui::directory_action(owner, id),
                "Mostrar carpeta en el explorador",
            );
        } else if name == "web_fetch" && valid_web_url(target) {
            card = card.open_action(
                tool_presentation::navigation_action(owner, id),
                "Abrir enlace",
            );
        } else {
            card = card.open_action(
                crate::file_activity_ui::inspect_action(owner, id),
                "Inspeccionar búsqueda",
            );
        }
        if name != "web_fetch"
            && matches!(
                tool_presentation::navigation_target(name, arguments),
                Some(tool_presentation::ToolNavigationTarget::Url(_))
            )
        {
            card = card.actions(vec![crate::file_activity_ui::icon_action(
                tool_presentation::navigation_action(owner, id),
                Icon::Globe,
                "Abrir enlace",
            )]);
        }
    }
    if let Some(note) = note {
        card = card.note(note);
    }
    Some(card.render())
}

pub(crate) fn hosted(item: &TranscriptItem, state: &ViewState) -> Element {
    let TranscriptItem::WebSearch {
        id,
        action,
        query,
        url,
        sources,
        status,
        ..
    } = item
    else {
        return Element::spacer();
    };
    let owner = state.selected_session.as_deref().unwrap_or("draft");
    let expanded = state.conversation_details.is_expanded(id, false);
    let receiving = *status == ModelStreamStatus::Receiving;
    let phase = hosted_phase(action, *status);
    let target = query
        .as_deref()
        .or(url.as_deref())
        .unwrap_or("Buscar en la web");
    let mut rows = Vec::new();
    if expanded {
        for (index, (title, url)) in sources
            .iter()
            .enumerate()
            .filter(|(_, (_, url))| valid_web_url(url))
        {
            rows.push(result_row(
                owner,
                id,
                index,
                &SearchResult {
                    title: title.clone(),
                    target: url.clone(),
                    detail: String::new(),
                    web: true,
                    directory: false,
                },
                true,
            ));
        }
        if rows.is_empty() {
            let message = if is_page_action(action) {
                url.as_deref()
                    .filter(|url| valid_web_url(url))
                    .unwrap_or("La actividad no informó una dirección de página válida.")
            } else if receiving {
                "Consultando fuentes…"
            } else {
                "Esta actividad no informó fuentes consultadas."
            };
            rows.push(
                Element::text(message)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(theme::FOREGROUND_SUBTLE),
            );
        }
    }
    let source_count = sources.iter().filter(|(_, url)| valid_web_url(url)).count();
    let actions = if is_page_action(action) && url.as_deref().is_some_and(valid_web_url) {
        vec![crate::file_activity_ui::icon_action(
            page_action(owner, id),
            Icon::Globe,
            "Abrir página consultada",
        )]
    } else {
        Vec::new()
    };
    FileActivity::new(
        format!("conversation_detail:{owner}:{id}"),
        bounded(target, 300),
        Element::column(rows).gap(3.0),
        theme::component_palette(),
    )
    .icon(if action == "search" {
        Icon::Search
    } else {
        Icon::Globe
    })
    .status(if source_count == 0 {
        phase.into()
    } else {
        format!("{phase} · {source_count} fuentes")
    })
    .expanded(expanded)
    .running(receiving)
    .compact(true)
    .actions(actions)
    .render()
}

fn hosted_phase(action: &str, status: ModelStreamStatus) -> &'static str {
    match (action, status) {
        ("open_page", ModelStreamStatus::Receiving) => "Abriendo",
        ("open_page", ModelStreamStatus::Committed) => "Consultada",
        ("find_in_page" | "search", ModelStreamStatus::Receiving) => "Buscando",
        (_, ModelStreamStatus::Receiving) => "Consultando",
        (_, ModelStreamStatus::Committed) => "Completada",
        (_, ModelStreamStatus::Failed) => "Falló",
        (_, ModelStreamStatus::Cancelled) => "Cancelada",
        (_, ModelStreamStatus::Interrupted) => "Interrumpida",
        (_, ModelStreamStatus::Discarded) => "Descartada",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_and_search_headers_separate_inspection_from_disclosure() {
        use openide_ui::{DrawCommand, Key, Point, Size, UiAction, UiEvent, UiTree};
        for (name, key, target) in [
            ("list_files", "path", "."),
            ("list_files", "path", "apps"),
            ("list_files", "path", "crates"),
            ("search_files", "pattern", "main"),
            ("web_fetch", "url", "https://example.com/docs"),
        ] {
            let item = TranscriptItem::Tool {
                id: "call:1".into(),
                name: name.into(),
                arguments: serde_json::json!({(key):target}),
                output: None,
                is_error: false,
                activity: ActivityStatus::Running,
            };
            let mut state = crate::view::tests::empty_state();
            state.selected_session = Some("owner:1".into());
            let mut tree = UiTree::new(build(&item, &state).unwrap());
            tree.set_reduced_motion(true);
            let size = Size::new(360.0, 200.0);
            let frame = tree.layout(size);
            let inspect = match name {
                "list_files" => crate::file_activity_ui::directory_action("owner:1", "call:1"),
                "web_fetch" => tool_presentation::navigation_action("owner:1", "call:1"),
                _ => crate::file_activity_ui::inspect_action("owner:1", "call:1"),
            };
            let disclosure = "conversation_detail:owner:1:tool:call:1";
            let title = frame
                .regions
                .iter()
                .find(|region| region.id == inspect)
                .unwrap()
                .rect;
            let row = frame
                .regions
                .iter()
                .find(|region| region.id == disclosure)
                .unwrap()
                .rect;
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "tool:call:1"));
            assert!(!frame.commands.iter().any(|command| matches!(
                command,
                DrawCommand::Icon {
                    icon: Icon::Code,
                    ..
                }
            )));
            for (point, action) in [
                (
                    Point::new(title.x + title.width / 2.0, title.y + title.height / 2.0),
                    inspect.as_str(),
                ),
                (Point::new(row.x + 2.0, row.y + 2.0), disclosure),
                (
                    Point::new(row.x + row.width - 16.0, row.y + row.height / 2.0),
                    disclosure,
                ),
            ] {
                assert_eq!(frame.hit_test(point), Some(action));
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id: action.into() }]
                );
                assert!(tree.focus(action));
                for key in [Key::Enter, Key::Space] {
                    assert_eq!(
                        tree.handle_event(UiEvent::Key {
                            key,
                            shift: false,
                            ctrl: false
                        }),
                        vec![UiAction::Clicked { id: action.into() }]
                    );
                }
            }
            if name == "web_fetch" {
                assert!(frame
                    .regions
                    .iter()
                    .any(|region| region.id
                        == tool_presentation::navigation_action("owner:1", "call:1")));
            }
        }
    }

    #[test]
    fn result_links_come_from_typed_fields_with_bounded_titles() {
        let output = r#"{"results":[{"title":"Rust","url":"https://www.rust-lang.org/","snippet":"A language"},{"url":"file:///etc/passwd"},{"url":"javascript:alert(1)"}]}"#;
        let results = captured_results("web_search", output, &Value::Null);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust");
        assert!(captured_results(
            "web_search",
            "https://example.com (unstructured)",
            &Value::Null
        )
        .is_empty());
        let results = captured_results(
            "search_files",
            "src/main.rs:12: fn main() {}\n[results truncated]",
            &Value::Null,
        );
        assert_eq!(results[0].target, "src/main.rs");
        assert_eq!(results[0].detail, "fn main() {}");
    }

    #[test]
    fn listed_directories_are_passive_while_file_indices_stay_stable() {
        use openide_ui::{DrawCommand, HitKind, Size, UiTree};
        let results = captured_results(
            "list_files",
            "dir\tsrc\nfile\tCargo.toml",
            &serde_json::json!({"path":"."}),
        );
        assert!(results[0].directory && !results[1].directory);
        let mut tree = UiTree::new(Element::column(
            results
                .iter()
                .enumerate()
                .map(|(index, result)| result_row("owner", "call", index, result, false))
                .collect(),
        ));
        let frame = tree.layout(Size::new(320.0, 160.0));
        assert!(!frame.regions.iter().any(|region| region.id
            == result_action("owner", "call", 0, false)
            && region.kind == HitKind::Button));
        assert!(frame.regions.iter().any(|region| region.id
            == result_action("owner", "call", 1, false)
            && region.kind == HitKind::Button));
        assert!(frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                icon,
                ..
            } if *icon == openide_ui::folder_icon("src", false)
        )));
    }

    #[test]
    fn directory_activity_names_the_project_and_bounds_dense_file_rows() {
        use openide_ui::{DrawCommand, Size, UiTree};
        let mut state = crate::view::tests::empty_state();
        state.workspace = "/work/openADE".into();
        state.selected_session = Some("owner".into());
        state.conversation_details.set_expanded("tool:list", true);
        let item = TranscriptItem::Tool {
            id: "list".into(),
            name: "list_files".into(),
            arguments: serde_json::json!({"path":"."}),
            output: Some((0..18).map(|i| format!("file\tfile-{i}.rs\n")).collect()),
            is_error: false,
            activity: ActivityStatus::Completed,
        };
        let texts = |frame: &openide_ui::Frame| {
            frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for width in [320.0, 960.0] {
            let mut tree = UiTree::new(build(&item, &state).unwrap());
            let frame = tree.layout(Size::new(width, 1000.0));
            let rendered = texts(&frame);
            assert!(rendered.iter().any(|text| text == "openADE"));
            assert!(!rendered
                .iter()
                .any(|text| matches!(text.as_str(), "." | "Archivo" | "Carpeta")));
            assert!(rendered.iter().any(|text| text == "file-5.rs"));
            assert!(!rendered.iter().any(|text| text == "file-6.rs"));
            assert!(rendered.iter().any(|text| text == "Mostrar 12 más"));
            let first = frame
                .regions
                .iter()
                .find(|region| region.id == result_action("owner", "list", 0, false))
                .unwrap();
            assert_eq!(first.rect.height, 28.0);
        }
        state
            .conversation_details
            .set_expanded("results:list", true);
        let mut tree = UiTree::new(build(&item, &state).unwrap());
        let frame = tree.layout(Size::new(400.0, 1000.0));
        assert!(texts(&frame).iter().any(|text| text == "file-17.rs"));
        assert!(texts(&frame).iter().any(|text| text == "Mostrar menos"));
    }

    #[test]
    #[ignore = "export dense directory activity with native text metrics"]
    fn export_directory_activity_visual_review() {
        use openide_ui::Size;
        let directory = std::path::Path::new("/tmp/openade-file-activity-review");
        std::fs::create_dir_all(directory).unwrap();
        for width in [360.0, 900.0] {
            let mut state = crate::view::tests::empty_state();
            state.workspace = "/work/openADE".into();
            state.selected_session = Some("owner".into());
            state.conversation_details.set_expanded("tool:list", true);
            let item = TranscriptItem::Tool {
                id: "list".into(), name: "list_files".into(),
                arguments: serde_json::json!({"path":"."}),
                output: Some("dir\tapps\ndir\tcrates\ndir\tdocs\ndir\treferences\nfile\tCargo.toml\nfile\tREADME.md\nfile\tCargo.lock\nfile\tLICENSE\n".into()),
                is_error: false, activity: ActivityStatus::Completed,
            };
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                Element::column(vec![build(&item, &state).unwrap()])
                    .padding(16.0)
                    .background(theme::BACKGROUND),
            );
            let frame = tree.layout(Size::new(width, 400.0));
            let path = directory.join(format!("directory-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 400.0)
                .unwrap();
            let parsed = resvg::usvg::Tree::from_str(
                &std::fs::read_to_string(&path).unwrap(),
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 400).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn hosted_states_describe_the_observed_web_operation() {
        assert_eq!(
            hosted_phase("open_page", ModelStreamStatus::Receiving),
            "Abriendo"
        );
        assert_eq!(
            hosted_phase("open_page", ModelStreamStatus::Committed),
            "Consultada"
        );
        assert_eq!(
            hosted_phase("find_in_page", ModelStreamStatus::Interrupted),
            "Interrumpida"
        );
        assert_eq!(hosted_phase("search", ModelStreamStatus::Failed), "Falló");
        assert!(is_page_action("open_page") && is_page_action("find_in_page"));
        assert!(!is_page_action("search") && !is_page_action("unknown"));
    }
}
