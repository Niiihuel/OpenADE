//! File operations keep one disclosure through input, execution and review.
//! Previews come only from the recorded call; confirmed diffs come from the worker.

use crate::{
    chat_diff_ui, file_activity_preview, theme, tool_presentation,
    transcript::{ActivityStatus, ModelStreamStatus, TranscriptItem},
    view::ViewState,
};
use openide_ui::{
    ButtonSize, ButtonVariant, CodeDiff, CodeLanguage, Component, Element, FileActivity,
    FontFamily, Icon, IconButton, Skeleton, Tooltip, TooltipPlacement,
};
use std::path::Path;

pub(crate) fn default_expanded_calls(state: &ViewState) -> Vec<&str> {
    // A bounded live tail opens naturally. Earlier history stays compact, and
    // explicit choices in ConversationDetails always take precedence.
    state
        .transcript
        .iter()
        .rev()
        .take(64)
        .take_while(|item| !matches!(item, TranscriptItem::User { .. }))
        .filter_map(|item| match item {
            TranscriptItem::Tool { id, name, .. }
                if matches!(name.as_str(), "write_file" | "edit_file" | "apply_patch") =>
            {
                Some(id.as_str())
            }
            TranscriptItem::ToolPreview {
                id,
                call_id,
                name: Some(name),
                ..
            } if matches!(name.as_str(), "write_file" | "edit_file" | "apply_patch") => {
                Some(call_id.as_deref().unwrap_or(id))
            }
            _ => None,
        })
        .take(2)
        .collect()
}

pub(crate) fn default_expanded(state: &ViewState, call: &str) -> bool {
    default_expanded_calls(state).contains(&call)
}

fn label(value: impl Into<String>) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(11.0)
        .foreground(theme::FOREGROUND_SUBTLE)
        .padding(8.0)
}

pub(crate) fn icon_action(id: String, icon: Icon, hint: &str) -> Element {
    icon_action_alias(id.clone(), id, icon, hint)
}

fn icon_action_alias(id: String, action: String, icon: Icon, hint: &str) -> Element {
    Tooltip::new(
        IconButton::new(&id, icon, theme::interactive_palette())
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::IconExtraSmall)
            .render()
            .dispatch_as(action),
        id,
        hint,
        theme::component_palette(),
    )
    .render()
}

/// Resolve paths against this captured row at click time, never from an action
/// carrying a path. Preview IDs remain distinct from later tool call IDs.
pub(crate) fn open_action(owner: &str, row_id: &str, file_index: usize) -> String {
    format!(
        "file_activity_open:{}",
        serde_json::to_string(&(owner, row_id, file_index)).expect("file action identity")
    )
}

pub(crate) fn inspect_action(owner: &str, row_id: &str) -> String {
    format!(
        "file_activity_inspect:{}",
        serde_json::to_string(&(owner, row_id)).expect("file activity identity")
    )
}

pub(crate) fn directory_action(owner: &str, row_id: &str) -> String {
    format!(
        "file_activity_directory:{}",
        serde_json::to_string(&(owner, row_id)).expect("directory activity identity")
    )
}

fn file_open_heading(owner: &str, row_id: &str, index: usize, path: &str) -> Element {
    let action = open_action(owner, row_id, index);
    let title = Element::icon_button(&action, openide_ui::file_icon(path), path)
        .underline_on_hover()
        .font_family(FontFamily::Sans)
        .font_size(11.5)
        .foreground(theme::FOREGROUND)
        .gap(6.0)
        .padding(6.0)
        .size(None, Some(32.0));
    Element::row(vec![
        Tooltip::new(title, &action, "", theme::component_palette())
            .description(path)
            .placement(TooltipPlacement::Right)
            .render(),
        Element::spacer().flex(1.0),
    ])
    .padding(4.0)
    .size(None, Some(40.0))
    .clip_children()
}

fn generation_placeholder(owner: &str, call: &str) -> Element {
    // Initial loading only. Received code replaces this placeholder entirely.
    // Skeleton's stable IDs preserve pulse phase through token-driven rebuilds;
    // the shared painter suspends animation offscreen and for reduced motion.
    Element::column(
        [0.68, 0.43]
            .into_iter()
            .enumerate()
            .map(|(index, fraction)| {
                Element::row(vec![
                    Skeleton::new(
                        format!("file-generation:{owner}:{call}:{index}"),
                        theme::component_palette(),
                    )
                    .height(6.0)
                    .radius(3.0)
                    .render()
                    .flex(fraction),
                    Element::spacer().flex(1.0 - fraction),
                ])
                .size(None, Some(6.0))
            })
            .collect(),
    )
    .gap(5.0)
    .padding(8.0)
    .size(None, Some(33.0))
}

pub(crate) fn build(
    item: &TranscriptItem,
    state: &ViewState,
    compact: bool,
    partial: Option<(&str, bool)>,
    media: Option<&TranscriptItem>,
) -> Option<Element> {
    let (name, call, disclosure, status, running, confirmed_edit, note) = match item {
        TranscriptItem::Tool {
            id,
            name,
            arguments,
            output,
            is_error,
            activity,
        } if file_activity_preview::is_file_tool(name) => {
            let presentation = tool_presentation::present(
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
                presentation.title,
                *activity == ActivityStatus::Running,
                !*is_error
                    && *activity == ActivityStatus::Completed
                    && output.is_some()
                    && matches!(name.as_str(), "write_file" | "apply_patch"),
                presentation.note,
            )
        }
        TranscriptItem::ToolPreview {
            id,
            call_id,
            name: Some(name),
            status,
            input_complete,
            ..
        } if file_activity_preview::is_file_tool(name) => {
            let title = match status {
                ModelStreamStatus::Receiving if *input_complete => "Entrada recibida",
                ModelStreamStatus::Receiving if name == "read_file" => "Preparando lectura",
                ModelStreamStatus::Receiving => "Preparando edición",
                ModelStreamStatus::Committed => "Pendiente de ejecución",
                ModelStreamStatus::Cancelled => "Cancelada · sin ejecutar",
                ModelStreamStatus::Interrupted => "Interrumpida · sin ejecutar",
                ModelStreamStatus::Failed => "Falló · sin ejecutar",
                ModelStreamStatus::Discarded => "Descartada · sin ejecutar",
            };
            (
                name.as_str(),
                call_id.as_deref().unwrap_or(id),
                call_id
                    .as_ref()
                    .map(|call| format!("tool:{call}"))
                    .unwrap_or_else(|| id.clone()),
                title.to_owned(),
                *status == ModelStreamStatus::Receiving,
                false,
                None,
            )
        }
        _ => return None,
    };
    let owner = state.selected_session.as_deref().unwrap_or("draft");
    let (row_id, open_hint) = match item {
        TranscriptItem::Tool { id, .. } => (id.as_str(), "Abrir en el editor"),
        TranscriptItem::ToolPreview { id, .. } => {
            (id.as_str(), "Abrir archivo o contenido provisional")
        }
        _ => unreachable!("file activities are tool rows"),
    };
    let expanded = state
        .conversation_details
        .is_expanded(&disclosure, default_expanded(state, call));
    let preview = (expanded && !confirmed_edit)
        .then(|| file_activity_preview::preview(item, partial))
        .flatten();
    let entry = confirmed_edit
        .then(|| chat_diff_ui::entry(state, call))
        .flatten();
    let fallback_path = match item {
        TranscriptItem::Tool { arguments, .. } => {
            match tool_presentation::navigation_target(name, arguments) {
                Some(tool_presentation::ToolNavigationTarget::File(path)) => Some(path.to_owned()),
                _ if name == "apply_patch" => arguments
                    .get("patch")
                    .and_then(|patch| patch.as_str())
                    .and_then(|patch| {
                        patch.lines().take(128).find_map(|line| {
                            ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                                .iter()
                                .find_map(|prefix| line.strip_prefix(prefix))
                                .filter(|path| {
                                    !path.is_empty()
                                        && path.len() < 4096
                                        && !path.chars().any(char::is_control)
                                })
                                .map(str::to_owned)
                        })
                    }),
                _ => None,
            }
        }
        TranscriptItem::ToolPreview { arguments, .. } => {
            file_activity_preview::preview_path(arguments)
        }
        _ => None,
    };
    let mut paths: Vec<&str> = if let Some(entry) = entry.filter(|entry| !entry.files.is_empty()) {
        entry.files.iter().map(|file| file.path.as_str()).collect()
    } else {
        preview
            .as_ref()
            .map(|preview| {
                preview
                    .files
                    .iter()
                    .map(|file| file.path.as_str())
                    .collect()
            })
            .unwrap_or_default()
    };
    if paths.is_empty() {
        paths.extend(fallback_path.as_deref());
    }
    let (filename, parent) = if paths.len() == 1 {
        let path = Path::new(paths[0]);
        (
            path.file_name()
                .and_then(|part| part.to_str())
                .unwrap_or(paths[0])
                .to_owned(),
            path.parent()
                .and_then(|part| part.to_str())
                .unwrap_or_default()
                .to_owned(),
        )
    } else if !paths.is_empty() {
        (format!("{} archivos", paths.len()), String::new())
    } else {
        (
            if name == "read_file" {
                "Leer archivo"
            } else {
                "Editar archivos"
            }
            .to_owned(),
            String::new(),
        )
    };
    let filename_action = (paths.len() == 1).then(|| open_action(owner, row_id, 0));
    let mut body = Vec::new();
    let generating_edit = matches!(name, "write_file" | "edit_file" | "apply_patch")
        && matches!(
            item,
            TranscriptItem::Tool {
                activity: ActivityStatus::Running,
                is_error: false,
                ..
            } | TranscriptItem::ToolPreview {
                status: ModelStreamStatus::Receiving,
                input_complete: false,
                ..
            }
        );
    let unconfirmed_end = matches!(
        item,
        TranscriptItem::Tool {
            activity: ActivityStatus::Failed
                | ActivityStatus::Cancelled
                | ActivityStatus::Interrupted
                | ActivityStatus::Discarded
                | ActivityStatus::PermissionInterrupted,
            ..
        } | TranscriptItem::Tool { is_error: true, .. }
            | TranscriptItem::ToolPreview {
                status: ModelStreamStatus::Failed
                    | ModelStreamStatus::Cancelled
                    | ModelStreamStatus::Interrupted
                    | ModelStreamStatus::Discarded,
                ..
            }
    );
    if expanded {
        if confirmed_edit {
            body.push(chat_diff_ui::body(state, call));
        } else {
            if name == "read_file" {
                let completed = matches!(
                    item,
                    TranscriptItem::Tool {
                        output: Some(_),
                        is_error: false,
                        activity: ActivityStatus::Completed,
                        ..
                    }
                );
                let has_content = preview
                    .as_ref()
                    .is_some_and(|preview| preview.files.iter().any(|file| !file.lines.is_empty()));
                // The header already communicates success. Keep the captured
                // source immediately beneath it, without a second status row.
                let message = if completed && !has_content {
                    Some("La lectura no devolvió contenido")
                } else if unconfirmed_end {
                    Some("No se obtuvo contenido de esta lectura")
                } else if !completed && !has_content {
                    Some("Esperando el contenido del archivo…")
                } else {
                    None
                };
                if let Some(message) = message {
                    body.push(label(message).padding(0.0));
                }
            } else if matches!(
                item,
                TranscriptItem::Tool {
                    activity: ActivityStatus::Completed,
                    is_error: false,
                    ..
                }
            ) {
                body.push(label("Contenido enviado · vista previa de la edición"));
            } else if unconfirmed_end {
                body.push(label("Contenido enviado · sin confirmación"));
            } else if generating_edit {
                body.push(label("Vista previa · generando contenido"));
            } else {
                body.push(label("Cambios propuestos"));
            }
            if let Some(preview) = preview.as_ref() {
                for (index, file) in preview.files.iter().enumerate() {
                    if preview.files.len() > 1 {
                        body.push(file_open_heading(owner, row_id, index, &file.path));
                    }
                    if file.deleted {
                        body.push(label(format!("Se propone eliminar {}", file.path)));
                        continue;
                    }
                    if !file.lines.is_empty() {
                        body.push(
                            CodeDiff::new(
                                format!("file-preview:{owner}:{call}:{index}"),
                                file.lines.clone(),
                            )
                            .single_gutter(matches!(name, "read_file" | "write_file"))
                            .follow_tail(running)
                            .language(CodeLanguage::from_extension(
                                Path::new(&file.path)
                                    .extension()
                                    .and_then(|ext| ext.to_str())
                                    .unwrap_or_default(),
                            ))
                            .height(
                                (file.lines.len() as f32 * 20.0 + 12.0)
                                    .clamp(48.0, if compact { 180.0 } else { 240.0 }),
                            )
                            .palette(theme::component_palette())
                            .render(),
                        );
                    }
                }
                if preview.limited {
                    let notice = label("Vista parcial · el contenido mostrado está limitado");
                    body.push(if name == "read_file" {
                        notice.padding(0.0)
                    } else {
                        notice
                    });
                }
            }
            let has_content = preview.as_ref().is_some_and(|preview| {
                preview
                    .files
                    .iter()
                    .any(|file| file.deleted || !file.lines.is_empty())
            });
            if generating_edit && !has_content {
                body.push(generation_placeholder(owner, call));
            }
        }
        if let Some(media) = media {
            body.extend(crate::session_surface::tool_media_elements(
                media, compact, owner,
            ));
        }
    }
    let mut palette = theme::interactive_palette();
    palette.surface = theme::CODE_SURFACE;
    let body = Element::column(body).gap(6.0);
    let body = if name == "read_file" {
        Element::column(vec![
            Element::spacer().size(None, Some(8.0)),
            Element::row(vec![
                Element::spacer().size(Some(8.0), None),
                body.flex(1.0),
                Element::spacer().size(Some(8.0), None),
            ]),
            Element::spacer().size(None, Some(2.0)),
        ])
    } else {
        body
    };
    let mut card = FileActivity::new(
        format!("conversation_detail:{owner}:{disclosure}"),
        filename,
        body,
        palette,
    )
    .path(parent)
    .status(if confirmed_edit {
        String::new()
    } else {
        status
    })
    .expanded(expanded)
    .running(running)
    .compact(name == "read_file");
    if let Some(detail) = entry.and_then(chat_diff_ui::evidence_detail) {
        card = card.detail(detail);
    }
    if let Some(action) = filename_action {
        card = card.open_action(action, open_hint);
    }
    if let Some(note) = note {
        card = card.note(note);
    }
    if let Some(entry) = entry.filter(|entry| {
        entry.unavailable.is_none()
            && !entry.limited
            && entry.files.iter().all(|file| file.unavailable.is_none())
    }) {
        card = card.stats(
            entry.files.iter().map(|file| file.additions).sum(),
            entry.files.iter().map(|file| file.deletions).sum(),
        );
    }
    Some(card.render())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;
    use openide_ui::{DrawCommand, Frame as UiFrame, Rect, ScrollArea, Size, UiTree};
    use std::time::{Duration, Instant};

    fn preview_item(
        name: &str,
        status: ModelStreamStatus,
        input_complete: bool,
        content: &str,
    ) -> TranscriptItem {
        TranscriptItem::ToolPreview {
            id: "model:preview".into(),
            step: 1,
            item_id: "item".into(),
            call_id: Some("edit".into()),
            name: Some(name.into()),
            arguments: serde_json::json!({ "path": "src/main.rs", "content": content }).to_string(),
            input_complete,
            status,
            truncated: false,
        }
    }

    fn state_for(item: &TranscriptItem, expanded: bool) -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.selected_session = Some("owner".into());
        state.transcript.push(item.clone());
        state.conversation_details.sync(Some("owner"));
        state
            .conversation_details
            .set_expanded("tool:edit", expanded);
        state
    }

    fn tail_rects(frame: &UiFrame) -> Vec<Rect> {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, radius, .. }
                    if rect.height == 6.0 && *radius == 3.0 =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn preview_open_actions_use_row_identity_and_remain_available_after_cancellation() {
        for status in [
            ModelStreamStatus::Receiving,
            ModelStreamStatus::Cancelled,
            ModelStreamStatus::Interrupted,
        ] {
            let item = preview_item("write_file", status, true, "# Contenido propuesto");
            let state = state_for(&item, false);
            let frame = UiTree::new(build(&item, &state, false, None, None).unwrap())
                .layout(Size::new(360.0, 300.0));
            let action = open_action("owner", "model:preview", 0);
            assert!(frame.regions.iter().any(|region| region.id == action));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == format!("{action}:button")));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == open_action("owner", "edit", 0)));
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|region| region.id.starts_with("file-preview:")),
                "closed previews remain lazy"
            );
            let text: String = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert!(!text.contains("Escrito"));
            if status == ModelStreamStatus::Cancelled {
                assert!(text.contains("Cancelada"));
            }
        }
        let mut no_path = preview_item("write_file", ModelStreamStatus::Receiving, false, "");
        if let TranscriptItem::ToolPreview { arguments, .. } = &mut no_path {
            *arguments = "{\"path\":".into();
        }
        let state = state_for(&no_path, false);
        let frame = UiTree::new(build(&no_path, &state, false, None, None).unwrap())
            .layout(Size::new(360.0, 300.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("file_activity_open:")));
    }

    #[test]
    fn patch_paths_have_independent_navigation_even_without_a_direct_path_argument() {
        let item = TranscriptItem::Tool {
            id: "edit".into(),
            name: "apply_patch".into(),
            arguments: serde_json::json!({ "patch": "*** Begin Patch\n*** Add File: one.md\n+one\n*** Update File: src/two.rs\n@@\n-old\n+new\n*** End Patch\n" }),
            output: None,
            is_error: false,
            activity: ActivityStatus::Running,
        };
        for expanded in [false, true] {
            let state = state_for(&item, expanded);
            let frame = UiTree::new(build(&item, &state, false, None, None).unwrap())
                .layout(Size::new(360.0, 900.0));
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == open_action("owner", "edit", 0)));
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .any(|region| region.id == open_action("owner", "edit", 1)),
                expanded
            );
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "conversation_detail:owner:tool:edit"));
        }
    }

    #[test]
    fn edit_skeleton_only_appears_for_expanded_generating_or_running_edits() {
        let viewport = Size::new(360.0, 480.0);
        for name in ["write_file", "edit_file", "apply_patch", "read_file"] {
            for status in [
                ModelStreamStatus::Receiving,
                ModelStreamStatus::Committed,
                ModelStreamStatus::Cancelled,
                ModelStreamStatus::Interrupted,
                ModelStreamStatus::Failed,
            ] {
                for expanded in [false, true] {
                    let item = preview_item(name, status, false, "");
                    let state = state_for(&item, expanded);
                    let frame = UiTree::new(build(&item, &state, false, None, None).unwrap())
                        .layout(viewport);
                    assert_eq!(
                        tail_rects(&frame).len(),
                        if expanded && status == ModelStreamStatus::Receiving && name != "read_file"
                        {
                            2
                        } else {
                            0
                        },
                        "{name} {status:?} expanded={expanded}"
                    );
                }
            }
            let complete_input = preview_item(name, ModelStreamStatus::Receiving, true, "");
            let state = state_for(&complete_input, true);
            let frame = UiTree::new(build(&complete_input, &state, false, None, None).unwrap())
                .layout(viewport);
            assert!(
                tail_rects(&frame).is_empty(),
                "received input is not still generating"
            );
            for activity in [
                ActivityStatus::Running,
                ActivityStatus::WaitingPermission,
                ActivityStatus::PermissionInterrupted,
                ActivityStatus::Completed,
                ActivityStatus::Failed,
                ActivityStatus::Cancelled,
                ActivityStatus::Interrupted,
            ] {
                let item = TranscriptItem::Tool {
                    id: "edit".into(),
                    name: name.into(),
                    arguments: serde_json::json!({ "path": "src/main.rs" }),
                    output: None,
                    is_error: false,
                    activity,
                };
                let state = state_for(&item, true);
                let frame =
                    UiTree::new(build(&item, &state, false, None, None).unwrap()).layout(viewport);
                assert_eq!(
                    tail_rects(&frame).len(),
                    if activity == ActivityStatus::Running && name != "read_file" {
                        2
                    } else {
                        0
                    },
                    "{name} {activity:?}"
                );
            }
        }
    }

    #[test]
    fn first_received_content_replaces_the_skeleton_for_the_same_call() {
        let viewport = Size::new(360.0, 480.0);
        let mut tree = UiTree::new(Element::spacer());
        tree.set_reduced_motion(true);
        for content in [
            "",
            "l",
            "let generated_value = 1;",
            "let generated_value = 1;\nlet second_line = 2;",
        ] {
            let item = preview_item("write_file", ModelStreamStatus::Receiving, false, content);
            let state = state_for(&item, true);
            tree.set_root(build(&item, &state, false, None, None).unwrap());
            let frame = tree.layout(viewport);
            let code = frame
                .regions
                .iter()
                .any(|region| region.id == "file-preview:owner:edit:0");
            assert_eq!(code, !content.is_empty());
            assert_eq!(
                tail_rects(&frame).len(),
                if content.is_empty() { 2 } else { 0 }
            );
        }
        for (name, arguments, expected_code) in [
            (
                "write_file",
                serde_json::json!({"path":"a.rs", "content":"\n"}),
                true,
            ),
            (
                "write_file",
                serde_json::json!({"path":"a.rs", "content":"  "}),
                true,
            ),
            (
                "edit_file",
                serde_json::json!({"path":"a.rs", "old_string":"before"}),
                true,
            ),
            (
                "apply_patch",
                serde_json::json!({"patch":"*** Begin Patch\n*** Delete File: a.rs\n"}),
                false,
            ),
            (
                "apply_patch",
                serde_json::json!({"patch":"*** Begin Patch\n*** Add File: a.rs\n+\n*** Add File: b.rs\n"}),
                true,
            ),
        ] {
            let mut item = preview_item(name, ModelStreamStatus::Receiving, false, "");
            if let TranscriptItem::ToolPreview { arguments: raw, .. } = &mut item {
                *raw = arguments.to_string();
            }
            let state = state_for(&item, true);
            tree.set_root(build(&item, &state, false, None, None).unwrap());
            let frame = tree.layout(viewport);
            assert!(tail_rects(&frame).is_empty(), "{name}: {arguments}");
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .any(|region| region.kind == openide_ui::HitKind::CodeDiff),
                expected_code
            );
        }
    }

    #[test]
    fn edit_skeleton_animates_only_on_screen_and_respects_reduced_motion() {
        let item = preview_item("write_file", ModelStreamStatus::Receiving, false, "");
        let state = state_for(&item, true);
        let now = Instant::now();
        let viewport = Size::new(360.0, 480.0);
        let mut tree = UiTree::new(build(&item, &state, false, None, None).unwrap());
        let first = tree.layout_at(viewport, now);
        assert!(tree.is_animating());
        let second = tree.layout_at(viewport, now + Duration::from_millis(400));
        let colors = |frame: &UiFrame| {
            frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::FillRect {
                        rect,
                        radius,
                        color,
                    } if rect.height == 6.0 && *radius == 3.0 => Some(*color),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_ne!(colors(&first), colors(&second));
        tree.set_reduced_motion(true);
        tree.layout_at(viewport, now + Duration::from_millis(500));
        assert!(!tree.is_animating());
        tree.set_reduced_motion(false);
        tree.set_root(
            ScrollArea::new(
                "history",
                Element::column(vec![
                    Element::spacer().size(None, Some(900.0)),
                    build(&item, &state, false, None, None).unwrap(),
                ]),
            )
            .render(),
        );
        let frame = tree.layout_at(viewport, now + Duration::from_millis(600));
        assert!(tail_rects(&frame).is_empty());
        assert!(
            !tree.is_animating(),
            "offscreen source cards must not schedule animation"
        );
    }

    #[test]
    #[ignore = "export streaming file edit skeletons for visual review"]
    fn export_streaming_file_skeleton_visual_review() {
        let directory = std::path::PathBuf::from("/tmp/openide-file-skeleton-visual-qa");
        std::fs::create_dir_all(&directory).unwrap();
        for width in [360.0, 720.0] {
            for (phase, content) in [
                ("starting", ""),
                (
                    "receiving",
                    "fn main() {\n    let message = \"Hello\";\n    println!(\"{message}\");\n}",
                ),
            ] {
                let item = preview_item("write_file", ModelStreamStatus::Receiving, false, content);
                let state = state_for(&item, true);
                let ctx = egui::Context::default();
                crate::fonts::install(&ctx);
                ctx.begin_pass(egui::RawInput::default());
                let mut tree = UiTree::new(
                    Element::column(vec![build(&item, &state, false, None, None).unwrap()])
                        .padding(20.0)
                        .background(theme::WINDOW),
                );
                tree.set_reduced_motion(true);
                let measure = ctx.clone();
                tree.set_text_measurer(move |text, size, family| {
                    measure.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                text.into(),
                                egui::FontId::new(
                                    size,
                                    match family {
                                        FontFamily::Sans => egui::FontFamily::Proportional,
                                        FontFamily::Monospace => egui::FontFamily::Monospace,
                                    },
                                ),
                                egui::Color32::WHITE,
                            )
                            .size()
                            .x
                    })
                });
                let frame = tree.layout(Size::new(width, 400.0));
                let path = directory.join(format!("file-edit-{phase}-{width:.0}.svg"));
                crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 400.0)
                    .unwrap();
                let options = crate::settings_ui::visual_qa::native_svg_options();
                let parsed =
                    resvg::usvg::Tree::from_str(&std::fs::read_to_string(&path).unwrap(), &options)
                        .unwrap();
                let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 400).unwrap();
                resvg::render(
                    &parsed,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                pixmap.save_png(path.with_extension("png")).unwrap();
                ctx.end_pass().textures_delta.clear();
            }
        }
    }
}
