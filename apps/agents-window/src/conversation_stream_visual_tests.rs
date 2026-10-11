//! Synthetic UI regression fixtures; no live provider, journal or process access.

use super::*;
use openide_core::ModelTokenUsage;
use openide_ui::{Color, DrawCommand, HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};
use serde_json::json;
use std::time::{Duration, Instant};

const OWNER: &str = "visual-stream-fixture";
const TOOL_ID: &str = "call-alpha";
const PUBLIC_REASONING: &str = "RESUMEN_PUBLICO_DE_PRUEBA: comprobar la estructura del proyecto.";

fn state() -> ViewState {
    let mut state = super::tests::state();
    state.selected_session = Some(OWNER.into());
    state.selected_session_title = Some("Visual del streaming".into());
    state.show_side_pane = false;
    state.running = false;
    state.background_run_selected = false;
    state.conversation_details.sync(Some(OWNER));
    state.transcript = vec![user("Revisá la estructura de este proyecto.")];
    state
}

fn user(content: &str) -> TranscriptItem {
    TranscriptItem::User {
        content: content.into(),
        images: vec![],
    }
}

fn assistant(content: &str) -> TranscriptItem {
    TranscriptItem::Assistant {
        content: content.into(),
        streaming: false,
    }
}

fn model_activity(step: usize, status: ActivityStatus, has_text: bool) -> TranscriptItem {
    TranscriptItem::Activity(TranscriptActivity::Model {
        id: format!("model-request:{OWNER}:{step}"),
        step,
        model: "model-id-fixture".into(),
        has_text,
        status,
    })
}

fn tool(activity: ActivityStatus) -> TranscriptItem {
    TranscriptItem::Tool {
        id: TOOL_ID.into(),
        name: "list_files".into(),
        arguments: json!({"path":"src"}),
        output: match activity {
            ActivityStatus::Failed => Some("ERROR_DE_PRUEBA: directorio no disponible".into()),
            ActivityStatus::Completed => Some("SALIDA_DE_PRUEBA: main.rs".into()),
            _ => None,
        },
        is_error: activity == ActivityStatus::Failed,
        activity,
    }
}

fn usage(id: &str, step: usize, status: ModelStreamStatus) -> TranscriptItem {
    let (input, output, reasoning, cached) = if step == 1 {
        (100, 23, 5, 11)
    } else {
        (400, 56, 7, 13)
    };
    TranscriptItem::ModelUsage {
        id: id.into(),
        step,
        usage: ModelTokenUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
            reasoning_tokens: Some(reasoning),
            cached_input_tokens: Some(cached),
        },
        status,
    }
}

fn action(detail: &str) -> String {
    format!("conversation_detail:{OWNER}:{detail}")
}

fn text(frame: &openide_ui::Frame) -> String {
    let layouts = frame.text_layouts();
    let sources = layouts
        .iter()
        .map(|layout| layout.source.as_str())
        .collect::<Vec<_>>();
    // Truncated controls and tooltip titles paint directly without a paragraph
    // layout. Preserve their multiplicity, while not counting ordinary text
    // twice when it already has an exact source layout.
    let mut text = sources.clone();
    text.extend(frame.commands.iter().filter_map(|command| {
        if let DrawCommand::Text { text, .. } = command {
            (!sources.contains(&text.as_str())).then_some(text.as_str())
        } else {
            None
        }
    }));
    text.join(" ")
}

fn surface(state: &ViewState, width: f32, reduced_motion: bool) -> UiTree {
    let mut tree = UiTree::new(build_session_surface(state, width, false, false));
    tree.set_reduced_motion(reduced_motion);
    tree
}

fn click_detail(tree: &mut UiTree, state: &mut ViewState, viewport: Size, detail: &str) {
    let id = action(detail);
    let frame = tree.layout(viewport);
    let target = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing disclosure {id}"))
        .rect;
    assert!(target.x >= 0.0 && target.x + target.width <= viewport.width);
    let point = Point::new(
        target.x + target.width / 2.0,
        target.y + target.height / 2.0,
    );
    assert_eq!(frame.hit_test(point), Some(id.as_str()));
    tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(
        tree.handle_event(UiEvent::PointerUp(point)),
        vec![UiAction::Clicked { id }]
    );
    let default = conversation_detail_default_open(state, detail);
    state.conversation_details.toggle(detail, default);
    tree.set_root(build_session_surface(state, viewport.width, false, false));
}

#[test]
fn lone_exploration_call_is_one_flat_line_and_keeps_errors_and_pending_work_visible() {
    for width in [360.0, 900.0] {
        for status in [
            ActivityStatus::Running,
            ActivityStatus::WaitingPermission,
            ActivityStatus::PermissionInterrupted,
            ActivityStatus::Completed,
            ActivityStatus::Failed,
            ActivityStatus::Cancelled,
            ActivityStatus::Interrupted,
        ] {
            let mut state = state();
            state.transcript.push(tool(status));
            // Inspector selection never takes over the inline line.
            state.selected_tool_id = Some(TOOL_ID.into());
            let viewport = Size::new(width, 900.0);
            let mut tree = surface(&state, width, true);
            let frame = tree.layout(viewport);
            // A lone exploration call is one flat line, as inside a group: no
            // card, no disclosure; clicking it opens its recorded activity.
            assert!(text(&frame).contains("src"));
            for disclosure in ["tool:call-alpha", "work:call-alpha"] {
                assert!(!frame
                    .regions
                    .iter()
                    .any(|region| region.id == action(disclosure)));
            }
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .filter(|region| {
                        region.id == "tool:call-alpha" && region.kind == HitKind::Button
                    })
                    .count(),
                1
            );
            assert!(!text(&frame).contains("SALIDA_DE_PRUEBA"));
            if status == ActivityStatus::Completed {
                assert!(text(&frame).contains("Listó"));
            }
            if status == ActivityStatus::Failed {
                assert!(text(&frame).contains("ERROR_DE_PRUEBA"));
            }
            if status == ActivityStatus::WaitingPermission {
                assert!(text(&frame).contains("autorización"));
            }
            if status == ActivityStatus::PermissionInterrupted {
                assert!(text(&frame).contains("permiso pendiente"));
            }
            if status == ActivityStatus::Interrupted {
                assert!(text(&frame).contains("Resultado incierto"));
            }
            assert!(!tree.is_animating());
            assert!(!state.show_side_pane);
        }
    }
}

fn exploration_fixture(id: &str, name: &str, activity: ActivityStatus) -> TranscriptItem {
    TranscriptItem::Tool {
        id: id.into(),
        name: name.into(),
        arguments: json!({"path":format!("src/{id}.rs"), "query":"GROUP_QUERY"}),
        output: (activity == ActivityStatus::Completed).then(|| format!("GROUP_RESULT_{id}")),
        is_error: activity == ActivityStatus::Failed,
        activity,
    }
}

fn toggle_group_detail(tree: &mut UiTree, state: &mut ViewState, width: f32, detail: &str) {
    let id = action(detail);
    assert!(tree.focus(&id), "missing keyboard target {id}");
    assert_eq!(
        tree.handle_event(UiEvent::Key {
            key: Key::Enter,
            ctrl: false,
            shift: false
        }),
        vec![UiAction::Clicked { id }]
    );
    let default = conversation_detail_default_open(state, detail);
    state.conversation_details.toggle(detail, default);
    tree.set_root(build_session_surface(state, width, false, false));
}

fn open_file(call: &str) -> String {
    tool_presentation::navigation_action(OWNER, call)
}

#[test]
fn consecutive_exploration_is_one_flat_line_that_opens_to_one_navigable_line_per_call() {
    for width in [360.0, 900.0] {
        let mut state = state();
        for index in 0..12 {
            state.transcript.push(exploration_fixture(
                &format!("read-{index}"),
                "read_file",
                ActivityStatus::Completed,
            ));
        }
        let viewport = Size::new(width, 1800.0);
        let mut tree = surface(&state, width, true);
        let closed = tree.layout(viewport);
        assert!(text(&closed).contains("Exploró"));
        assert!(text(&closed).contains("12 archivos"));
        assert_eq!(
            closed
                .regions
                .iter()
                .filter(|r| r.id == action("work:read-0"))
                .count(),
            1
        );
        assert!(!closed.regions.iter().any(|r| r.id == open_file("read-0")));
        assert!(!text(&closed).contains("Leyó"));
        // Text only: no spinner or icon column, just the chevron.
        let icons = UiTree::new(timeline(&state, width))
            .layout(viewport)
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Icon { icon, .. } => Some(*icon),
                DrawCommand::Spinner { .. } => Some(Icon::Refresh),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(icons, [Icon::ChevronRight]);
        assert!(!tree.is_animating());

        toggle_group_detail(&mut tree, &mut state, width, "work:read-0");
        let opened = tree.layout(viewport);
        assert_eq!(text(&opened).matches("Leyó").count(), 12);
        for index in 0..12 {
            let call = format!("read-{index}");
            assert!(text(&opened).contains(&format!("{call}.rs")));
            assert_eq!(
                opened
                    .regions
                    .iter()
                    .filter(|r| r.id == open_file(&call) && r.kind == HitKind::Button)
                    .count(),
                1,
                "each line opens its own file: {call}"
            );
        }
        assert!(!text(&opened).contains("GROUP_RESULT"));
        toggle_group_detail(&mut tree, &mut state, width, "work:read-0");
        let collapsed = tree.layout(viewport);
        assert!(!collapsed.regions.iter().any(|r| r.id == open_file("read-0")));
        assert!(!text(&collapsed).contains("Leyó"));
    }
}

#[test]
fn exploration_group_identity_survives_preview_execution_completion_and_new_calls() {
    let mut state = state();
    state.running = true;
    state.transcript.extend([
        TranscriptItem::ToolPreview {
            id: "preview-a".into(),
            step: 1,
            item_id: "item-a".into(),
            call_id: Some("a".into()),
            name: Some("read_file".into()),
            arguments: r#"{"path":"src/a.rs"}"#.into(),
            input_complete: true,
            status: ModelStreamStatus::Receiving,
            truncated: false,
        },
        exploration_fixture("b", "search_files", ActivityStatus::Running),
    ]);
    let width = 900.0;
    let viewport = Size::new(width, 1200.0);
    let mut tree = surface(&state, width, false);
    let preview = tree.layout(viewport);
    assert_eq!(
        preview
            .regions
            .iter()
            .filter(|r| r.id == action("work:a"))
            .count(),
        1
    );
    // Live: the header counts what has arrived and the ticker names the
    // step under way.
    assert!(text(&preview).contains("Explorando"));
    assert!(text(&preview).contains("1 archivo, 1 búsqueda"));
    assert!(text(&preview).contains("Buscando GROUP_QUERY"));
    assert!(!text(&preview).contains("Leyendo"));
    assert!(tree.is_animating());
    state.transcript[1] = exploration_fixture("a", "read_file", ActivityStatus::Running);
    state.transcript[2] = exploration_fixture("b", "search_files", ActivityStatus::Completed);
    tree.set_root(build_session_surface(&state, width, false, false));
    let started = tree.layout(viewport);
    assert!(started.regions.iter().any(|r| r.id == action("work:a")));
    assert!(text(&started).contains("a.rs, 1 búsqueda"));
    assert!(text(&started).contains("Leyendo a.rs"));
    // Between steps the group stays live and waits in its own ticker; the
    // transcript does not add a second waiting line under it.
    state.transcript[1] = exploration_fixture("a", "read_file", ActivityStatus::Completed);
    state.transcript.push(exploration_fixture(
        "c",
        "list_files",
        ActivityStatus::Completed,
    ));
    tree.set_root(build_session_surface(&state, width, false, false));
    // Still: a shimmer paints its text in several passes.
    let waiting = surface(&state, width, true).layout(viewport);
    assert!(text(&waiting).contains("Explorando"));
    assert_eq!(text(&waiting).matches(WAITING_LABEL).count(), 1);
    // The turn ends: the same group settles and nothing keeps moving.
    state.running = false;
    tree.set_root(build_session_surface(&state, width, false, false));
    let completed = tree.layout_at(viewport, Instant::now() + Duration::from_secs(1));
    assert!(completed.regions.iter().any(|r| r.id == action("work:a")));
    assert!(text(&completed).contains("Exploró"));
    assert!(text(&completed).contains("a.rs, 1 búsqueda, 1 listado"));
    assert!(!text(&completed).contains(WAITING_LABEL));
    assert!(!text(&completed).contains("Explorando"));
    assert!(
        !tree.is_animating(),
        "completed historical work has nothing in motion"
    );
}

#[test]
fn exploration_membership_does_not_change_when_diagnostics_or_batch_reads_finish() {
    for name in [
        "get_diagnostics",
        "batch_read",
        "codebase_search",
        "codebase_explore",
    ] {
        let mut state = state();
        state.transcript.extend([
            exploration_fixture("a", "read_file", ActivityStatus::Completed),
            exploration_fixture("b", name, ActivityStatus::Running),
            exploration_fixture("c", "read_file", ActivityStatus::Completed),
            exploration_fixture("d", "search_files", ActivityStatus::Completed),
        ]);
        let viewport = Size::new(900.0, 1200.0);
        let mut tree = surface(&state, viewport.width, true);
        let running = tree.layout(viewport);
        assert_eq!(exploration_run(&state.transcript, 1), (5, 4));
        let group_ids = |frame: &openide_ui::Frame| {
            frame
                .regions
                .iter()
                .filter(|region| region.id.starts_with(&action("work:")))
                .map(|region| region.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(group_ids(&running), vec![action("work:a")], "{name}");
        assert!(text(&running).contains("Explorando"), "{name}");
        toggle_group_detail(&mut tree, &mut state, viewport.width, "work:a");
        tree.layout(viewport);
        assert_eq!(tree.focused_id(), Some(action("work:a").as_str()));
        state.transcript[2] = exploration_fixture("b", name, ActivityStatus::Completed);
        tree.set_root(build_session_surface(&state, viewport.width, false, false));
        let finished = tree.layout(viewport);
        assert_eq!(group_ids(&finished), group_ids(&running), "{name}");
        assert_eq!(tree.focused_id(), Some(action("work:a").as_str()), "{name}");
        for call in ["a", "b", "c", "d"] {
            assert!(
                finished
                    .regions
                    .iter()
                    .any(|r| r.id == open_file(call) || r.id == format!("tool:{call}")),
                "open group keeps {call} visible after {name} finishes"
            );
        }
        assert!(text(&finished).contains("Exploró"), "{name}");
        assert!(
            text(&finished).contains(if name.starts_with("codebase") {
                "2 archivos, 2 búsquedas"
            } else {
                "3 archivos, 1 búsqueda"
            }),
            "{name}: {}",
            text(&finished)
        );
    }
}

#[test]
fn exploration_groups_never_cross_prose_turns_edits_or_foreign_captured_output() {
    let boundaries = vec![
        assistant("Una conclusión entre exploraciones"),
        user("Otro turno"),
        TranscriptItem::Reasoning {
            id: "r".into(),
            step: 1,
            content: "Analizando".into(),
            status: ModelStreamStatus::Committed,
            truncated: false,
        },
        exploration_fixture("write", "write_file", ActivityStatus::Completed),
        exploration_fixture("command", "run_command", ActivityStatus::Completed),
        TranscriptItem::Notice {
            message: "Aviso durable".into(),
            is_error: true,
        },
        TranscriptItem::ToolOutput {
            tool_call_id: "foreign".into(),
            text: "Ajeno".into(),
            truncated: false,
        },
        TranscriptItem::ToolPreview {
            id: "unknown".into(),
            step: 1,
            item_id: "u".into(),
            call_id: None,
            name: Some("read_file".into()),
            arguments: String::new(),
            input_complete: false,
            status: ModelStreamStatus::Receiving,
            truncated: false,
        },
    ];
    for boundary in boundaries {
        let mut state = state();
        state.transcript.extend([
            exploration_fixture("a", "read_file", ActivityStatus::Completed),
            exploration_fixture("b", "find_files", ActivityStatus::Completed),
            boundary,
            exploration_fixture("c", "list_files", ActivityStatus::Completed),
            exploration_fixture("d", "search_text", ActivityStatus::Completed),
        ]);
        assert_eq!(exploration_run(&state.transcript, 1), (3, 2));
        assert_eq!(exploration_run(&state.transcript, 4), (6, 2));
        let frame = surface(&state, 900.0, true).layout(Size::new(900.0, 1800.0));
        let groups = frame
            .regions
            .iter()
            .filter(|r| r.id.starts_with(&action("work:")))
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            groups,
            [action("work:a").as_str(), action("work:c").as_str()]
        );
    }
}

#[test]
fn exploration_keeps_errors_permissions_and_uncertain_outcomes_visible_open_or_collapsed() {
    for status in [
        ActivityStatus::Failed,
        ActivityStatus::WaitingPermission,
        ActivityStatus::PermissionInterrupted,
        ActivityStatus::Interrupted,
        ActivityStatus::Cancelled,
        ActivityStatus::Discarded,
    ] {
        for width in [360.0, 900.0] {
            let mut state = state();
            state.transcript.push(exploration_fixture(
                "ok",
                "read_file",
                ActivityStatus::Completed,
            ));
            let mut attention = exploration_fixture("attention", "list_files", status);
            if let TranscriptItem::Tool { output, .. } = &mut attention {
                *output = Some("GROUP_OUTCOME_TO_KEEP".into());
            }
            state.transcript.push(attention);
            let expected = match status {
                ActivityStatus::WaitingPermission => "autorización",
                ActivityStatus::PermissionInterrupted => "permiso pendiente",
                ActivityStatus::Failed => "GROUP_OUTCOME_TO_KEEP",
                ActivityStatus::Interrupted => "Resultado incierto",
                ActivityStatus::Cancelled => "Operación cancelada",
                ActivityStatus::Discarded => "Operación descartada",
                _ => unreachable!(),
            };
            // A call that needs attention keeps the group open by default,
            // and stays on screen when the reader collapses the rest.
            assert!(conversation_detail_default_open(&state, "work:ok"));
            for collapsed in [false, true] {
                if collapsed {
                    state.conversation_details.toggle("work:ok", true);
                }
                let frame = surface(&state, width, true).layout(Size::new(width, 1100.0));
                assert!(frame.regions.iter().any(|r| r.id == action("work:ok")));
                assert_eq!(
                    frame.regions.iter().any(|r| r.id == open_file("ok")),
                    !collapsed
                );
                assert!(frame.regions.iter().any(|r| r.id == "tool:attention"));
                assert!(
                    text(&frame).contains(expected),
                    "{status:?}: {}",
                    text(&frame)
                );
                let failed_ink = frame.commands.iter().any(|command| {
                    matches!(command, DrawCommand::Text { color, .. } if *color == theme::DESTRUCTIVE)
                });
                assert_eq!(failed_ink, status == ActivityStatus::Failed, "{status:?}");
            }
        }
    }
}

#[test]
fn exploration_keeps_a_calls_auxiliary_output_inside_its_run_and_out_of_the_lines() {
    let mut state = state();
    state.transcript.extend([
        exploration_fixture("a", "read_file", ActivityStatus::Running),
        TranscriptItem::ToolOutput {
            tool_call_id: "a".into(),
            text: "CAPTURE_FROM_A".into(),
            truncated: false,
        },
        model_activity(2, ActivityStatus::Completed, false),
        usage("usage-group", 2, ModelStreamStatus::Committed),
        exploration_fixture("b", "read_file", ActivityStatus::Completed),
    ]);
    assert_eq!(exploration_run(&state.transcript, 1), (6, 2));
    assert!(!conversation_detail_default_open(&state, "work:a"));
    let width = 900.0;
    let viewport = Size::new(width, 1500.0);
    let mut tree = surface(&state, width, true);
    let live = tree.layout(viewport);
    assert!(text(&live).contains("Explorando"));
    assert!(text(&live).contains("2 archivos"));
    assert!(text(&live).contains("Leyendo a.rs"));
    toggle_group_detail(&mut tree, &mut state, width, "work:a");
    let opened = tree.layout(viewport);
    assert!(opened.regions.iter().any(|r| r.id == open_file("a")));
    assert!(opened.regions.iter().any(|r| r.id == open_file("b")));
    assert!(!text(&opened).contains("CAPTURE_FROM_A"));
    assert!(!text(&opened).contains("GROUP_RESULT_b"));
}

#[test]
fn streamed_tool_arguments_keep_target_and_disclosure_after_confirmation() {
    let mut state = state();
    let preview = TranscriptItem::ToolPreview {
        id: "stream-preview".into(),
        step: 1,
        item_id: "item-1".into(),
        call_id: Some(TOOL_ID.into()),
        name: Some("write_file".into()),
        arguments: r#"{"path":"src/ni\u00f1o.rs","content":"partial"#.into(),
        input_complete: false,
        status: ModelStreamStatus::Receiving,
        truncated: false,
    };
    state.transcript.push(preview.clone());
    let mut tree = UiTree::new(model_stream_item(&preview, 1, &state, 780.0));
    let frame = tree.layout(Size::new(780.0, 400.0));
    assert!(text(&frame).contains("niño.rs"));
    assert!(!text(&frame).contains("src"));
    let title = crate::file_activity_ui::open_action(OWNER, "stream-preview", 0);
    assert!(tree.focus(&title));
    let hinted = tree.layout_at(
        Size::new(780.0, 400.0),
        Instant::now() + Duration::from_secs(1),
    );
    assert!(text(&hinted).contains("src/niño.rs"));
    assert!(text(&frame).contains("Preparando"));
    assert!(!text(&frame).contains("Escrito"));
    assert!(
        text(&frame).contains("partial"),
        "the live edit shows only proposed content"
    );
    assert!(frame
        .regions
        .iter()
        .any(|region| region.id == action("tool:call-alpha")));
    state.conversation_details.toggle("tool:call-alpha", false);
    assert!(tree.focus(&action("tool:call-alpha")));
    let tool = TranscriptItem::Tool {
        id: TOOL_ID.into(),
        name: "write_file".into(),
        arguments: json!({"path":"src/niño.rs","content":"confirmed"}),
        output: Some("Archivo escrito".into()),
        is_error: false,
        activity: ActivityStatus::Completed,
    };
    state.transcript[1] = tool.clone();
    state.chat_diffs.workspace = Some(std::path::PathBuf::from(&state.workspace));
    state.chat_diffs.session_id = Some(OWNER.into());
    state.chat_diffs.entries.insert(
        TOOL_ID.into(),
        crate::chat_diff_ui::ToolDiffEntry {
            files: vec![crate::chat_diff_ui::DiffFileView {
                path: "src/niño.rs".into(),
                old_path: None,
                additions: 1,
                deletions: 1,
                truncated: false,
                unavailable: None,
                can_open: true,
                lines: vec![
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Removed,
                        old_line: Some(1),
                        new_line: None,
                        text: "before".into(),
                    },
                    openide_ui::CodeDiffLine {
                        kind: openide_ui::CodeDiffLineKind::Added,
                        old_line: None,
                        new_line: Some(1),
                        text: "confirmed".into(),
                    },
                ]
                .into(),
            }],
            unavailable: None,
            notes: Vec::new(),
            limited: false,
        },
    );
    tree.set_root(tool_line(&tool, &state, false));
    let frame = tree.layout(Size::new(780.0, 600.0));
    assert!(text(&frame).contains("niño.rs"));
    assert!(!text(&frame).contains("Escrito"));
    assert!(text(&frame).contains("+1") && text(&frame).contains("−1"));
    assert!(text(&frame).contains("confirmed"));
    assert!(text(&frame).contains("before"));
    assert!(!text(&frame).contains("partial"));
    assert!(
        !text(&frame).contains("Archivo escrito"),
        "the raw result is not a second activity row"
    );
    assert_eq!(tree.focused_id(), Some(action("tool:call-alpha").as_str()));
    assert_eq!(
        frame
            .regions
            .iter()
            .filter(
                |region| region.id == action("tool:call-alpha") && region.kind == HitKind::Button
            )
            .count(),
        1
    );
    assert!(frame
        .regions
        .iter()
        .any(|region| region.id == crate::chat_diff_ui::view_id(OWNER, TOOL_ID, 0)));
    assert!(
        !frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("chat_diff_toggle:")),
        "one card owns the integrated diff disclosure"
    );
}

#[test]
fn terminal_output_uses_snapshot_events_and_mcp_unknown_name_stays_identifiable() {
    let state = state();
    let tool = TranscriptItem::Tool { id: "poll-1".into(), name: "poll_command".into(),
        arguments: json!({"session_id":"process-1"}),
        output: Some(json!({"session_id":"process-1", "events":[{"stream":"stdout", "text":"compiler output\ntest ok"}],
            "finished":false, "exit":null, "has_more_output":true}).to_string()),
        is_error:false, activity:ActivityStatus::Completed };
    let frame = UiTree::new(tool_line(&tool, &state, false)).layout(Size::new(780.0, 300.0));
    let visible = text(&frame);
    assert!(visible.contains("compiler output"));
    assert!(visible.contains("Proceso en ejecución al consultar"));
    assert!(!visible.contains("has_more_output"));
    let mcp = TranscriptItem::Tool {
        id: "mcp-1".into(),
        name: "mcp__github__create_issue".into(),
        arguments: json!({}),
        output: Some("Issue created".into()),
        is_error: false,
        activity: ActivityStatus::Completed,
    };
    assert!(exploration_call(&mcp).is_none());
    let frame = UiTree::new(tool_line(&mcp, &state, false)).layout(Size::new(780.0, 100.0));
    assert!(text(&frame).contains("mcp__github__create_issue"));
}

#[test]
fn command_stream_remains_visible_before_and_after_its_authoritative_result() {
    let mut state = state();
    state.transcript.push(TranscriptItem::Tool {
        id: "live-command".into(),
        name: "run_command".into(),
        arguments: json!({"command":"cargo test"}),
        output: None,
        is_error: false,
        activity: ActivityStatus::Running,
    });
    state.transcript.push(TranscriptItem::ToolOutput {
        tool_call_id: "live-command".into(),
        text: "STREAMING_BEFORE_EXIT\n".into(),
        truncated: false,
    });
    for width in [360.0, 900.0] {
        let mut tree = surface(&state, width, true);
        let frame = tree.layout(Size::new(width, 900.0));
        let visible = text(&frame);
        assert_eq!(visible.matches("STREAMING_BEFORE_EXIT").count(), 1);
        assert!(frame
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Spinner { .. })));
        assert!(!visible.contains("Salida en curso"));
        assert!(!visible.contains("Ejecutado"));
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.id == action("tool:live-command"))
                .count(),
            1
        );
    }
    if let TranscriptItem::Tool { activity, .. } = &mut state.transcript[1] {
        *activity = ActivityStatus::Interrupted;
    }
    if let TranscriptItem::ToolOutput { truncated, .. } = &mut state.transcript[2] {
        *truncated = true;
    }
    let frame = surface(&state, 900.0, true).layout(Size::new(900.0, 900.0));
    let visible = text(&frame);
    assert!(visible.contains("STREAMING_BEFORE_EXIT"));
    assert!(visible.contains("Sin confirmar"));
    assert!(visible.contains("Resultado incierto"));
    assert!(visible.contains("omitida"));
    assert!(!visible.contains("Salida en curso"));
    if let TranscriptItem::Tool {
        activity, output, ..
    } = &mut state.transcript[1]
    {
        *activity = ActivityStatus::Completed;
        *output = Some("CONFIRMED_RESULT".into());
    }
    // Captured diagnostics stay visible; they never overwrite the final result.
    let frame = surface(&state, 900.0, true).layout(Size::new(900.0, 900.0));
    let visible = text(&frame);
    assert!(visible.contains("STREAMING_BEFORE_EXIT"));
    assert!(visible.contains("Salida capturada"));
    assert!(!visible.contains("Salida parcial"));
    state
        .conversation_details
        .toggle("tool:live-command", false);
    let frame = surface(&state, 900.0, true).layout(Size::new(900.0, 1200.0));
    let visible = text(&frame);
    assert!(visible.contains("CONFIRMED_RESULT"));
    assert!(visible.contains("Resultado final"));
    assert!(visible.contains("STREAMING_BEFORE_EXIT"));
    assert!(visible.contains("Salida capturada"));
    assert!(tool_partial_output(&state, 1, "different-call").is_none());
}

#[test]
fn terminal_title_names_the_executable_without_repeating_the_command_or_parsing_quoted_spaces() {
    for (command, expected) in [
        ("cargo test --workspace", "cargo"),
        (
            "LANG=es_ES.UTF-8 MODE='two words' ./tools/build --verbose",
            "build",
        ),
        ("'./tools/my command' --verbose", "my command"),
        ("(cd workspace && cargo test)", "cd"),
        ("sudo -u root cargo test", "sudo"),
        ("FOO=bar", "Terminal"),
    ] {
        assert_eq!(terminal_command_title(command), expected);
    }
}

#[test]
fn compact_terminal_keeps_command_output_and_independent_actions_without_tool_json() {
    for width in [240.0, 360.0, 780.0] {
        let mut state = state();
        let mut tool = TranscriptItem::Tool {
            id: "compact-terminal".into(),
            name: "run_command".into(),
            arguments: json!({"command":"cargo test --workspace", "cwd":"/private/terminal-fixture"}),
            output: Some(
                (0..60)
                    .map(|line| format!("OUTPUT_LINE_{line:02}\n"))
                    .collect(),
            ),
            is_error: false,
            activity: ActivityStatus::Completed,
        };
        let viewport = Size::new(width, 700.0);
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(Element::column(
            vec![tool_line(&tool, &state, width < 470.0)],
        ));
        let compact = tree.layout(viewport);
        let compact_text = text(&compact);
        assert!(compact_text.contains("OUTPUT_LINE_59"));
        assert!(!compact_text.contains("OUTPUT_LINE_00"));
        for absent in [
            "Ejecutado",
            "Entrada",
            "Resultado final",
            "/private/terminal-fixture",
            "\"cwd\"",
        ] {
            assert!(
                !compact_text.contains(absent),
                "redundant or raw field {absent}"
            );
        }
        let disclosure = action("tool:compact-terminal");
        let header = compact
            .regions
            .iter()
            .find(|region| region.id == disclosure)
            .unwrap()
            .rect;
        let inspect = compact
            .regions
            .iter()
            .find(|region| region.id == "tool:compact-terminal" && region.kind == HitKind::Button)
            .unwrap()
            .rect;
        assert_eq!(header.height, 28.0);
        assert!(header.x + header.width <= inspect.x);
        assert!(!compact.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Icon {
                icon: Icon::ChevronRight | Icon::ChevronDown,
                ..
            }
        )));
        for (id, expected) in [
            (format!("{disclosure}:expand"), disclosure.clone()),
            (
                "tool:compact-terminal".to_owned(),
                "tool:compact-terminal".to_owned(),
            ),
        ] {
            let rect = compact
                .regions
                .iter()
                .find(|region| region.id == id && region.kind == HitKind::Button)
                .unwrap()
                .rect;
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked { id: expected }]
            );
        }
        state
            .conversation_details
            .set_expanded("tool:compact-terminal", true);
        tree.set_root(Element::column(vec![tool_line(
            &tool,
            &state,
            width < 470.0,
        )]));
        let expanded = tree.layout(viewport);
        assert_eq!(
            expanded
                .regions
                .iter()
                .find(|region| region.id == disclosure)
                .unwrap()
                .rect,
            header
        );
        let scroll_id = format!("tool-result:{OWNER}:compact-terminal:final");
        let scroll = expanded
            .regions
            .iter()
            .find(|region| region.id == scroll_id && region.kind == HitKind::Scroll)
            .expect("expanded output has its own bounded viewport")
            .rect;
        assert!(scroll.height <= if width < 470.0 { 240.0 } else { 320.0 });
        assert!(text(&expanded).contains("$ cargo test --workspace"));
        assert!(!text(&expanded).contains("\"cwd\""));
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(
                scroll.x + scroll.width / 2.0,
                scroll.y + scroll.height / 2.0,
            ),
            delta_y: -72.0,
        });
        tree.layout(viewport);
        let offset = tree.scroll_offset(&scroll_id);
        assert!(offset > 0.0);
        if let TranscriptItem::Tool { output, .. } = &mut tool {
            output.as_mut().unwrap().push_str("OUTPUT_APPENDED\n");
        }
        tree.set_root(Element::column(vec![tool_line(
            &tool,
            &state,
            width < 470.0,
        )]));
        tree.layout(viewport);
        assert_eq!(
            tree.scroll_offset(&scroll_id),
            offset,
            "new output never moves a manual reader"
        );
        tree.scroll_to_end(&scroll_id);
        let end = tree.layout(viewport);
        assert!(end.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.contains("OUTPUT_APPENDED")
        )));
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn terminal_keeps_exceptional_states_and_does_not_echo_stdin_or_claim_process_exit() {
    for (activity, expected) in [
        (ActivityStatus::WaitingPermission, "Permiso pendiente"),
        (ActivityStatus::Interrupted, "Sin confirmar"),
        (ActivityStatus::Cancelled, "Cancelado"),
        (ActivityStatus::Discarded, "Descartado"),
        (ActivityStatus::Failed, "Error"),
    ] {
        let state = state();
        let tool = TranscriptItem::Tool {
            id: "terminal-attention".into(),
            name: "run_command".into(),
            arguments: json!({"command":"cargo test"}),
            output: None,
            is_error: activity == ActivityStatus::Failed,
            activity,
        };
        let frame =
            UiTree::new(tool_line(&tool, &state, true)).layout(Size::new(320.0, 400.0));
        assert!(text(&frame).contains(expected));
        assert!(!frame
            .commands
            .iter()
            .any(|command| matches!(command, DrawCommand::Spinner { .. })));
    }
    for expanded in [false, true] {
        let mut state = state();
        state
            .conversation_details
            .set_expanded("tool:stdin", expanded);
        let tool = TranscriptItem::Tool {
            id: "stdin".into(),
            name: "write_stdin".into(),
            arguments: json!({"session_id":"process-1", "chars":"DO_NOT_ECHO_SECRET"}),
            output: Some(json!({"session_id":"process-1", "stdin_closed":false}).to_string()),
            is_error: false,
            activity: ActivityStatus::Completed,
        };
        let frame =
            UiTree::new(tool_line(&tool, &state, true)).layout(Size::new(360.0, 400.0));
        assert!(text(&frame).contains("Entrada enviada"));
        assert!(!text(&frame).contains("DO_NOT_ECHO_SECRET"));
        assert!(!text(&frame).contains("Proceso finalizado"));
    }
}

#[test]
fn waiting_row_is_one_header_line_only_while_its_approval_card_is_shown() {
    let arguments = json!({"command":"rm prueba2.md"});
    let tool = TranscriptItem::Tool {
        id: "waiting".into(),
        name: "run_command".into(),
        arguments: arguments.clone(),
        output: None,
        is_error: false,
        activity: ActivityStatus::WaitingPermission,
    };
    let row = |state: &ViewState| {
        text(&UiTree::new(tool_line(&tool, state, true)).layout(Size::new(360.0, 400.0)))
    };
    let repeats = |row: &str| {
        assert!(row.contains("Permiso pendiente"), "{row}");
        row.contains("Esperando autorización") && row.contains("$ rm prueba2.md")
    };
    let pretty = serde_json::to_string_pretty(&arguments).unwrap();
    let permission = |arguments: &str| crate::view::PermissionView {
        request_id: "request".into(),
        tool_name: "run_command".into(),
        arguments: arguments.into(),
    };
    let mut state = state();
    assert!(repeats(&row(&state)), "no card: the row is the only request");
    state.pending_permissions = vec![permission("{}")];
    assert!(repeats(&row(&state)), "a card for another call");
    state.pending_permissions = vec![permission("{}"); 3];
    state.pending_permissions.push(permission(&pretty));
    assert!(repeats(&row(&state)), "only three cards are rendered");
    state.pending_permissions = vec![permission(&pretty)];
    assert!(!repeats(&row(&state)));
    state.conversation_details.set_expanded("tool:waiting", true);
    assert!(row(&state).contains("$ rm prueba2.md"), "opened on purpose");
    state.conversation_details.set_expanded("tool:waiting", false);
    state.pending_permissions.clear();
    state.pending_subagent_permissions = vec![crate::view::SubagentPermissionView {
        child_session_id: OWNER.into(),
        parent_session_id: "parent".into(),
        request_id: "request".into(),
        tool_name: "run_command".into(),
        arguments: pretty,
    }];
    assert!(!repeats(&row(&state)));
    state.pending_subagent_permissions[0].child_session_id = "another".into();
    assert!(repeats(&row(&state)), "that card belongs to another session");

    let arguments =
        json!({"patch":"*** Begin Patch\n*** Delete File: docs/borrador.md\n*** End Patch\n"});
    let file = TranscriptItem::Tool {
        id: "waiting-file".into(),
        name: "apply_patch".into(),
        arguments: arguments.clone(),
        output: None,
        is_error: false,
        activity: ActivityStatus::WaitingPermission,
    };
    let row = |state: &ViewState| {
        text(&UiTree::new(tool_line(&file, state, true)).layout(Size::new(360.0, 400.0)))
    };
    assert!(row(&state).contains("Esperando autorización"));
    state.pending_permissions = vec![crate::view::PermissionView {
        request_id: "request".into(),
        tool_name: "apply_patch".into(),
        arguments: serde_json::to_string_pretty(&arguments).unwrap(),
    }];
    let paired = row(&state);
    assert!(!paired.contains("Esperando autorización"), "{paired}");
    assert!(paired.contains("Permiso pendiente"), "{paired}");
    assert!(paired.contains("borrador.md"), "{paired}");
}

#[test]
#[ignore = "exports compact native terminal timelines with bundled fonts under /tmp"]
fn export_terminal_timeline_native_visual_review() {
    let directory = std::path::Path::new("/tmp/openade-terminal-timeline-review");
    std::fs::create_dir_all(directory).unwrap();
    for width in [360.0, 900.0] {
        for phase in [
            "completed",
            "running",
            "expanded",
            "failed",
            "failed-action-hover",
            "scrolled",
            "end",
        ] {
            let failed = phase.starts_with("failed");
            let mut state = state();
            state.prompt = "Revisá el resultado antes de continuar.".into();
            state.transcript.push(TranscriptItem::Tool {
                id: "terminal-review".into(), name: "run_command".into(),
                arguments: json!({"command":"cargo test -p openide-ui --lib", "cwd":"/projects/openADE"}),
                output: if phase == "running" { None } else { Some(if failed {
                    "error: no se pudo completar la compilación\nRevisa la salida y vuelve a ejecutar las pruebas.\n".into()
                } else {
                    (0..24).map(|index| format!("test editor::selección_unicode_{index:02} ... ok\n")).collect()
                }) },
                is_error: failed,
                activity: if phase == "running" { ActivityStatus::Running } else if failed { ActivityStatus::Failed } else { ActivityStatus::Completed },
            });
            if phase == "running" {
                state.transcript.push(TranscriptItem::ToolOutput {
                    tool_call_id: "terminal-review".into(),
                    text: "Compiling openide-ui\nRunning unit tests\ntest editor::selección_unicode ... ok\n".into(),
                    truncated: false,
                });
            }
            state.conversation_details.set_expanded(
                "tool:terminal-review",
                matches!(phase, "expanded" | "scrolled" | "end"),
            );
            if matches!(phase, "scrolled" | "end") {
                state.transcript.extend((0..8).map(|index| TranscriptItem::User {
                    content: format!("Revisión {index}: conserva el contexto del resultado y comprueba que las últimas líneas sigan siendo legibles al llegar al final de la conversación."),
                    images: Vec::new(),
                }));
            }
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                build_session_surface(&state, width, false, false),
            );
            tree.set_reduced_motion(true);
            if matches!(phase, "scrolled" | "end") {
                let initial = tree.layout(Size::new(width, 900.0));
                let transcript = initial
                    .regions
                    .iter()
                    .find(|region| region.id == "transcript")
                    .unwrap()
                    .rect;
                tree.handle_event(UiEvent::ScrollDirect {
                    position: Point::new(transcript.x + 20.0, transcript.y + 40.0),
                    delta_y: if phase == "end" { -100_000.0 } else { -160.0 },
                });
            }
            if phase == "failed-action-hover" {
                let action = tree
                    .layout(Size::new(width, 900.0))
                    .regions
                    .iter()
                    .find(|region| region.id == "tool:terminal-review")
                    .unwrap()
                    .rect;
                tree.handle_event(UiEvent::PointerMove(Point::new(
                    action.x + action.width / 2.0,
                    action.y + action.height / 2.0,
                )));
            }
            let frame = tree.layout(Size::new(width, 900.0));
            let path = directory.join(format!("terminal-{phase}-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 900.0)
                .unwrap();
            let svg = std::fs::read_to_string(&path).unwrap();
            let parsed = resvg::usvg::Tree::from_str(
                &svg,
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 900).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            context.end_pass().textures_delta.clear();
        }
    }
}


fn mcp_image_result() -> String {
    use base64::Engine as _;
    let bitmap = image::RgbaImage::from_fn(120, 60, |x, y| {
        image::Rgba([30 + x as u8, 60 + y as u8, 120, 255])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(bitmap)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    json!({"content":[{"type":"text", "text":"Captura de referencia"},
        {"type":"image", "mimeType":"image/png", "data":base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())}]}).to_string()
}

#[test]
fn mcp_result_uses_raster_and_explicit_navigation_without_nested_click_targets() {
    let mut state = state();
    let output = mcp_image_result();
    let item = TranscriptItem::Tool {
        id: "image-call".into(),
        name: "mcp__openide_browser__browser_navigate".into(),
        arguments: json!({"url":"https://example.test"}),
        output: Some(output),
        is_error: false,
        activity: ActivityStatus::Completed,
    };
    state.conversation_details.toggle("tool:image-call", false);
    for width in [360.0, 900.0] {
        let mut tree = UiTree::new(tool_line(&item, &state, width < 470.0));
        let frame = tree.layout(Size::new(width, 800.0));
        assert_eq!(
            frame
                .commands
                .iter()
                .filter(|command| matches!(command, DrawCommand::RasterImage { .. }))
                .count(),
            1
        );
        let visible = text(&frame);
        assert!(visible.contains("Captura de referencia"));
        assert!(!visible.contains("iVBOR"));
        let id = tool_presentation::navigation_action(OWNER, "image-call");
        let button = frame.regions.iter().find(|region| region.id == id).unwrap();
        let point = Point::new(
            button.rect.x + button.rect.width / 2.0,
            button.rect.y + button.rect.height / 2.0,
        );
        assert_eq!(frame.hit_test(point), Some(id.as_str()));
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked { id }]
        );
    }
}

#[test]
fn typed_tool_media_survives_truncated_text_without_reparsing_or_crossing_calls() {
    let mut state = state();
    let parsed = crate::tool_result_media::preview("mcp__browser", &mcp_image_result()).unwrap();
    assert_eq!(parsed.images.len(), 1, "fixture supplies one media image");
    state.transcript.push(TranscriptItem::Tool {
        id: "typed-media".into(),
        name: "mcp__browser".into(),
        arguments: json!({}),
        output: Some("{\"content\":[\n[tool output truncated]".into()),
        is_error: false,
        activity: ActivityStatus::Completed,
    });
    state.transcript.push(TranscriptItem::ToolOutput {
        tool_call_id: "typed-media".into(),
        text: "diagnostic".into(),
        truncated: false,
    });
    state.transcript.push(TranscriptItem::ToolMedia {
        tool_call_id: "typed-media".into(),
        images: parsed
            .images
            .iter()
            .map(|image| TranscriptImage {
                raster: Some(image.clone()),
                alt: "Captura del navegador".into(),
            })
            .collect(),
        text: "CAPTION_FROM_MEDIA".into(),
        notices: vec!["Aviso multimedia confirmado".into()],
    });
    state.conversation_details.toggle("tool:typed-media", false);
    for width in [360.0, 900.0] {
        let mut tree = surface(&state, width, true);
        let frame = tree.layout(Size::new(width, 1200.0));
        let rasters = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::RasterImage { image, rect, .. } => Some((image.sha256(), *rect)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rasters
                .iter()
                .filter(|(sha256, _)| *sha256 == parsed.images[0].sha256())
                .count(),
            1,
            "the call's media must paint exactly once at {width}px: {rasters:?}"
        );
        let visible = text(&frame);
        assert!(visible.contains("CAPTION_FROM_MEDIA"));
        assert!(visible.contains("Aviso multimedia confirmado"));
        assert!(!visible.contains("multimedia no está disponible"));
        let id =
            tool_presentation::image_action(OWNER, "typed-media", 0, parsed.images[0].sha256());
        assert_eq!(
            frame
                .regions
                .iter()
                .filter(|region| region.id == id)
                .count(),
            1,
            "typed media keeps one call-scoped interaction target"
        );
        let image = frame.regions.iter().find(|region| region.id == id).unwrap();
        let point = Point::new(
            image.rect.x + image.rect.width / 2.0,
            image.rect.y + image.rect.height / 2.0,
        );
        assert_eq!(frame.hit_test(point), Some(id.as_str()));
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked { id }]
        );
    }
    assert!(tool_media(&state, 1, "other-call").is_none());
    if let TranscriptItem::ToolOutput { tool_call_id, .. } = &mut state.transcript[2] {
        *tool_call_id = "other-call".into();
    }
    assert!(
        tool_media(&state, 1, "typed-media").is_none(),
        "must not cross another tool's output"
    );
}

#[test]
fn reasoning_is_one_muted_line_whose_text_opens_only_by_click() {
    for width in [360.0, 900.0] {
        let mut state = state();
        let reason = TranscriptItem::Reasoning {
            id: "reason-public".into(),
            step: 1,
            content: PUBLIC_REASONING.into(),
            status: ModelStreamStatus::Receiving,
            truncated: false,
        };
        state.transcript.push(reason.clone());
        let mut header = UiTree::new(model_stream_item(&reason, 1, &state, width));
        let frame = header.layout(Size::new(width, 300.0));
        assert!(text(&frame).contains("Pensando…"));
        assert!(!text(&frame).contains("RESUMEN_PUBLICO_DE_PRUEBA"));
        assert!(
            header.is_animating(),
            "the reasoning in flight owns the thinking shimmer"
        );
        // A flat line: no brain, spinner or icon column.
        assert!(!frame.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Spinner { .. }
                | DrawCommand::Icon {
                    icon: Icon::Brain | Icon::Code,
                    ..
                }
        )));
        let viewport = Size::new(width, 900.0);
        let mut tree = surface(&state, width, true);
        assert!(!conversation_detail_default_open(&state, "reason-public"));
        click_detail(&mut tree, &mut state, viewport, "reason-public");
        assert!(text(&tree.layout(viewport)).contains("RESUMEN_PUBLICO_DE_PRUEBA"));
        if let TranscriptItem::Reasoning { status, .. } = &mut state.transcript[1] {
            *status = ModelStreamStatus::Committed;
        }
        tree.set_root(build_session_surface(&state, width, false, false));
        let done = tree.layout(viewport);
        // The reader's choice survives the commit; an untimed row is brief.
        assert!(text(&done).contains("RESUMEN_PUBLICO_DE_PRUEBA"));
        assert!(text(&done).contains("Pensó un momento"));
        assert!(!text(&done).contains("Pensando"));
        click_detail(&mut tree, &mut state, viewport, "reason-public");
        assert!(!text(&tree.layout(viewport)).contains("RESUMEN_PUBLICO_DE_PRUEBA"));

        // The duration is the one the events recorded.
        let mut projection = crate::transcript::TranscriptProjection::new();
        for (sequence, timestamp_ms) in [(1, 10_000), (2, 13_400)] {
            projection.apply_event(&openide_core::AgentEvent {
                session_id: OWNER.into(),
                sequence,
                timestamp_ms,
                kind: if sequence == 1 {
                    openide_core::AgentEventKind::ModelRequestStarted {
                        step: 1,
                        model: "model".into(),
                    }
                } else {
                    openide_core::AgentEventKind::ModelStreamUpdate {
                        step: 1,
                        update: openide_core::ModelStreamEvent::ReasoningDelta {
                            item_id: "r".into(),
                            content: PUBLIC_REASONING.into(),
                        },
                    }
                },
            });
        }
        let mut timed = self::state();
        projection.sync_into(&mut timed.transcript);
        projection.sync_times_into(&mut timed.item_times);
        timed.transcript.push(assistant("La respuesta empieza"));
        let mut tree = surface(&timed, width, true);
        assert!(text(&tree.layout(viewport)).contains("Pensó 3 s"));
    }
}

#[test]
fn thinking_has_one_active_line_and_reduced_motion_stops_animation() {
    for width in [360.0, 900.0] {
        let mut state = state();
        state.running = true;
        state
            .transcript
            .push(model_activity(1, ActivityStatus::Running, false));
        state.transcript.push(TranscriptItem::Reasoning {
            id: "reason-public".into(),
            step: 1,
            content: PUBLIC_REASONING.into(),
            status: ModelStreamStatus::Receiving,
            truncated: false,
        });
        let clock = Instant::now();
        let viewport = Size::new(width, 900.0);
        let mut tree = UiTree::new(timeline(&state, width));
        let first = tree.layout_at(viewport, clock);
        // The reasoning in flight owns progress: no waiting line under it.
        assert!(text(&first).contains("Pensando…"));
        assert!(!text(&first).contains(WAITING_LABEL));
        assert!(tree.is_animating());
        assert!(!first.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Spinner { .. }
                | DrawCommand::Icon {
                    icon: Icon::Brain | Icon::Code,
                    ..
                }
        )));
        tree.set_reduced_motion(true);
        let reduced = tree.layout_at(viewport, clock + Duration::from_millis(700));
        assert!(!tree.is_animating());
        assert_eq!(text(&reduced).matches("Pensando").count(), 1);
    }
}

#[test]
fn visible_response_has_no_redundant_progress_row_and_keeps_stop_available() {
    for width in [360.0, 900.0] {
        // Native model events and legacy CLI prose share the same presentation.
        // A lagging has_text flag must not reintroduce the thinking row either.
        for has_text in [None, Some(false), Some(true)] {
            for streaming in [true, false] {
                let mut state = state();
                state.running = true;
                if let Some(has_text) = has_text {
                    state
                        .transcript
                        .push(model_activity(1, ActivityStatus::Running, has_text));
                }
                state.transcript.push(TranscriptItem::Assistant {
                    content: "RESPUESTA_VISIBLE: la estructura ya se está mostrando.".into(),
                    streaming,
                });
                let mut tree = UiTree::new(timeline(&state, width));
                let frame = tree.layout(Size::new(width, 900.0));
                let visible = text(&frame);
                assert!(visible.contains("RESPUESTA_VISIBLE"));
                for redundant in [
                    "Respondiendo",
                    "Pensando",
                    "Generando respuesta",
                    "Respuesta recibida",
                ] {
                    assert!(!visible.contains(redundant), "{redundant}: {visible}");
                }
                assert!(!frame
                    .commands
                    .iter()
                    .any(|command| matches!(command, DrawCommand::Spinner { .. })));
                assert!(
                    !tree.is_animating(),
                    "no orphan loading animation below the prose"
                );

                let composer = UiTree::new(composer(&state, width)).layout(Size::new(width, 400.0));
                assert!(composer
                    .regions
                    .iter()
                    .any(|region| region.id == "run_toggle"));
                assert!(
                    composer.commands.iter().any(|command| matches!(
                        command,
                        DrawCommand::Icon {
                            icon: Icon::Stop,
                            ..
                        }
                    )),
                    "hiding the prose marker must not imply that the turn completed"
                );
            }
        }
    }
}

#[test]
fn waiting_remains_visible_until_first_painted_prose_and_restarts_for_a_new_request() {
    for width in [360.0, 900.0] {
        for native in [false, true] {
            let mut state = state();
            state.running = true;
            if native {
                state
                    .transcript
                    .push(model_activity(1, ActivityStatus::Running, true));
            }
            let now = Instant::now();
            state
                .stream_presentation
                .sync(Some(OWNER), &state.transcript, now);
            state.transcript.push(TranscriptItem::Assistant {
                content: String::new(),
                streaming: true,
            });
            state
                .stream_presentation
                .sync(Some(OWNER), &state.transcript, now);
            let viewport = Size::new(width, 900.0);
            let mut tree = UiTree::new(timeline(&state, width));
            let waiting = tree.layout(viewport);
            assert!(text(&waiting).contains(WAITING_LABEL));
            assert!(!text(&waiting).contains("PRIMER_TEXTO_VISIBLE"));
            assert!(tree.is_animating());

            let index = state.transcript.len() - 1;
            let TranscriptItem::Assistant { content, .. } = &mut state.transcript[index] else {
                unreachable!();
            };
            *content = "PRIMER_TEXTO_VISIBLE".into();
            state
                .stream_presentation
                .sync(Some(OWNER), &state.transcript, now);
            let visible_bytes = state
                .stream_presentation
                .visible_bytes(index, "PRIMER_TEXTO_VISIBLE");
            assert!(
                visible_bytes > 0,
                "the first received fragment is painted immediately"
            );
            tree.set_root(timeline(&state, width));
            let responding = tree.layout(viewport);
            assert!(text(&responding).contains(&"PRIMER_TEXTO_VISIBLE"[..visible_bytes]));
            assert!(!text(&responding).contains("Pensando"));
            assert!(!text(&responding).contains("Respondiendo"));
            assert!(!tree.is_animating());

            if native {
                state.stream_presentation.flush();
                state
                    .transcript
                    .push(model_activity(2, ActivityStatus::Running, false));
                tree.set_root(timeline(&state, width));
                let next_request = tree.layout(viewport);
                assert!(text(&next_request).contains("PRIMER_TEXTO_VISIBLE"));
                assert!(text(&next_request).contains("Pensando"));
                assert!(
                    tree.is_animating(),
                    "a new model request still owns its waiting state"
                );
            }
        }
    }
}

#[test]
fn partial_response_does_not_hide_a_durable_model_failure() {
    for width in [360.0, 900.0] {
        let mut state = state();
        state.transcript.extend([
            model_activity(1, ActivityStatus::Failed, true),
            assistant("RESPUESTA_PARCIAL_CONSERVADA"),
            TranscriptItem::Notice {
                message: "ERROR_DURABLE_DEL_MODELO".into(),
                is_error: true,
            },
        ]);
        let mut tree = UiTree::new(timeline(&state, width));
        let frame = tree.layout(Size::new(width, 900.0));
        let visible = text(&frame);
        assert!(visible.contains("RESPUESTA_PARCIAL_CONSERVADA"));
        assert!(visible.contains("Error en la conversación"));
        assert_eq!(visible.matches("ERROR_DURABLE_DEL_MODELO").count(), 1);
        assert!(!visible.contains("Respondiendo"));
        assert!(!visible.contains("Pensando"));
        assert!(!tree.is_animating());
    }
}

#[test]
fn terminal_model_state_never_fabricates_a_thinking_tail_from_stale_host_flags() {
    for width in [360.0, 900.0] {
        for status in [
            ActivityStatus::Completed,
            ActivityStatus::Failed,
            ActivityStatus::Cancelled,
            ActivityStatus::Interrupted,
        ] {
            let mut state = state();
            state.running = true;
            state.background_run_selected = true;
            state
                .transcript
                .push(model_activity(1, ActivityStatus::Running, false));
            state.transcript.push(model_activity(2, status, false));
            let mut tree = UiTree::new(timeline(&state, width));
            let frame = tree.layout(Size::new(width, 900.0));
            let visible = text(&frame);
            assert!(!visible.contains("Pensando"), "{status:?}: {visible}");
            assert!(!visible.contains("Respondiendo"));
            assert!(!visible.contains("Preparando herramientas"));
            assert!(!tree.is_animating());
        }
    }
}

#[test]
fn recorded_usage_stays_out_of_chat_even_with_a_saved_open_disclosure() {
    for width in [360.0, 900.0] {
        for status in [
            ModelStreamStatus::Receiving,
            ModelStreamStatus::Committed,
            ModelStreamStatus::Failed,
            ModelStreamStatus::Interrupted,
            ModelStreamStatus::Cancelled,
        ] {
            let mut state = state();
            state.transcript.extend([
                usage("usage-first", 1, status),
                assistant("RESPUESTA_DEL_PRIMER_TURNO"),
                user("Continuá con una explicación."),
                usage("usage-next-turn", 2, ModelStreamStatus::Committed),
                assistant("RESPUESTA_DEL_SEGUNDO_TURNO"),
            ]);
            state.conversation_details.toggle("usage-first", false);
            let frame = surface(&state, width, true).layout(Size::new(width, 1_200.0));
            let visible = text(&frame);
            assert!(visible.contains("RESPUESTA_DEL_PRIMER_TURNO"));
            assert!(visible.contains("RESPUESTA_DEL_SEGUNDO_TURNO"));
            assert!(!visible.contains("Uso de tokens"));
            assert!(!visible.contains("Entrada 100"));
            assert!(!frame.regions.iter().any(|region| {
                region.id == action("usage-first") || region.id == action("usage-next-turn")
            }));
            assert_eq!(
                state
                    .transcript
                    .iter()
                    .filter(|item| matches!(item, TranscriptItem::ModelUsage { .. }))
                    .count(),
                2
            );
        }
    }
}

#[test]
fn hidden_usage_does_not_leave_empty_rows_or_change_chat_spacing() {
    for width in [360.0, 900.0] {
        let mut state = state();
        state.transcript.extend([
            assistant("Antes del consumo registrado."),
            usage("usage-first", 1, ModelStreamStatus::Committed),
            assistant("Después del consumo registrado."),
        ]);
        let viewport = Size::new(width, 1_000.0);
        let with_usage = surface(&state, width, true).layout(viewport);
        state
            .transcript
            .retain(|item| !matches!(item, TranscriptItem::ModelUsage { .. }));
        let without_usage = surface(&state, width, true).layout(viewport);
        assert_eq!(with_usage.commands, without_usage.commands);
        assert_eq!(with_usage.regions, without_usage.regions);
    }
}

#[test]
fn composer_footer_keeps_model_step_technical_status_out_of_the_controls() {
    for width in [360.0, 900.0] {
        for running in [false, true] {
            let mut state = state();
            state.running = running;
            state.status = "model-id-fixture · paso 7".into();
            let frame = UiTree::new(composer(&state, width)).layout(Size::new(width, 400.0));
            assert!(!text(&frame).contains("paso 7"));
            assert!(!text(&frame).contains("model-id-fixture"));
        }
    }
}

#[test]
fn jump_to_bottom_is_a_white_circle_with_centered_dark_arrow_and_real_pointer_and_keyboard_actions()
{
    for width in [360.0, 900.0] {
        let mut state = state();
        state.show_jump_to_bottom = true;
        state
            .transcript
            .push(assistant("Respuesta sintética de prueba."));
        let viewport = Size::new(width, 900.0);
        let mut tree = surface(&state, width, true);
        let frame = tree.layout(viewport);
        let target = frame
            .regions
            .iter()
            .find(|region| region.id == "jump_to_bottom" && region.kind == HitKind::Button)
            .expect("shared native jump control")
            .rect;
        assert_eq!(target.width, 36.0);
        assert_eq!(target.height, 36.0);
        assert!(target.x >= 0.0 && target.x + target.width <= width);
        assert!(
            !text(&frame).contains("Volver al final"),
            "title belongs only to the tooltip"
        );
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, radius, color }
                if *rect == target && *radius == 18.0 && *color == Color::rgb(246, 246, 246)
        )));
        let arrow = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Icon {
                    icon: Icon::ArrowDown,
                    rect,
                    tint,
                    ..
                } if rect.x >= target.x && rect.x + rect.width <= target.x + target.width => {
                    Some((*rect, *tint))
                }
                _ => None,
            })
            .expect("down arrow in the control");
        assert_eq!(arrow.1, Color::rgb(32, 32, 32));
        assert!((arrow.0.x + arrow.0.width / 2.0 - target.x - target.width / 2.0).abs() <= 0.1);
        assert!((arrow.0.y + arrow.0.height / 2.0 - target.y - target.height / 2.0).abs() <= 0.1);
        let center = Point::new(
            target.x + target.width / 2.0,
            target.y + target.height / 2.0,
        );
        assert_eq!(frame.hit_test(center), Some("jump_to_bottom"));
        tree.handle_event(UiEvent::PointerDown(center));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(center)),
            vec![UiAction::Clicked {
                id: "jump_to_bottom".into(),
            }]
        );
        let mut keyboard = UiTree::new(jump_to_bottom_control());
        keyboard.layout(Size::new(width, 120.0));
        keyboard.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        assert_eq!(keyboard.focused_id(), Some("jump_to_bottom"));
        assert_eq!(
            keyboard.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "jump_to_bottom".into()
            }]
        );
    }
}

#[test]
fn jump_to_bottom_tooltip_is_delayed_on_hover_and_uses_the_same_compact_control() {
    for width in [360.0, 900.0] {
        let mut tree = UiTree::new(jump_to_bottom_control());
        tree.set_reduced_motion(true);
        let viewport = Size::new(width, 140.0);
        let before = tree.layout(viewport);
        assert!(!text(&before).contains("Volver al final"));
        let target = before
            .regions
            .iter()
            .find(|region| region.id == "jump_to_bottom")
            .unwrap()
            .rect;
        let clock = Instant::now();
        tree.handle_event(UiEvent::PointerMove(Point::new(
            target.x + 18.0,
            target.y + 18.0,
        )));
        let early = tree.layout_at(viewport, clock + Duration::from_millis(100));
        assert!(!text(&early).contains("Volver al final"));
        let late = tree.layout_at(viewport, clock + Duration::from_millis(650));
        assert!(text(&late).contains("Volver al final"));
        assert_eq!(
            late.regions
                .iter()
                .find(|region| region.id == "jump_to_bottom")
                .unwrap()
                .rect,
            target
        );
    }
}

#[test]
#[ignore = "opt-in synthetic native streaming SVG/PNG exports; no provider or live journal access"]
fn export_conversation_stream_visual_review() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("OPENIDE_STREAM_VISUAL_DIR")
            .unwrap_or_else(|| "/tmp/openide-conversation-stream-visual-qa".into()),
    );
    std::fs::create_dir_all(&directory).unwrap();
    // The compaction marker validates workspace identity. Use a real, empty
    // fixture directory so this review never inspects the user's project.
    let fixture_workspace = tempfile::tempdir().unwrap();
    for phase in [
        "user-message",
        "plan-current",
        "plan-history-open",
        "compaction-running",
        "compaction-complete",
        "file-footer",
        "reasoning-footer",
        "exploration-footer",
        "thinking",
        "response-streaming",
        "awaiting-commit",
        "prose-reading",
        "tools-running",
        "tools-error",
        "tools-error-open",
        "tool-catalog",
        "exploration-group",
        "exploration-group-open",
        "exploration-group-attention",
        "tool-details",
        "command-live",
        "tool-media",
        "tool-media-viewer",
        "usage",
        "usage-open",
        "usage-partial",
        "jump-control",
    ] {
        let mut state = state();
        match phase {
            "user-message" => {
                state.transcript = vec![
                    user(
                        "Revisá el editor y el panel de agentes.\n\nQuiero que los archivos, las terminales y las opciones conserven el mismo diseño compacto. Mantené el borrador y la selección al abrir @archivos o /comandos.\n\nReferencia: crates/openide-ui/src/components.rs · Español, Unicode y texto literal <sin formato>.",
                    ),
                    assistant(
                        "Voy a comprobar la navegación y el espaciado con el mismo contenido en ventanas angostas y amplias.",
                    ),
                ];
            }
            "plan-current" | "plan-history-open" => {
                let plan = openide_core::PlanSnapshot {
                    explanation: Some("Unificar la presentación sin alterar los datos ni las acciones de la conversación.".into()),
                    plan: [
                        ("Revisar las medidas de los controles y las etiquetas del editor.", openide_core::PlanStepStatus::Completed),
                        ("Conservar foco, borrador y posición al abrir o contraer detalles.", openide_core::PlanStepStatus::InProgress),
                        ("Validar el resultado con fuentes nativas y ventanas compactas.", openide_core::PlanStepStatus::Pending),
                    ].into_iter().map(|(step, status)| openide_core::PlanStep {
                        step: step.into(), status,
                    }).collect(),
                };
                let id = format!("plan:{OWNER}:7");
                state.transcript.push(TranscriptItem::PlanSnapshot {
                    id: id.clone(),
                    sequence: Some(7),
                    plan: plan.clone(),
                    is_proposal: true,
                    tool_call_id: None,
                });
                state.session_plans.insert(
                    OWNER.into(),
                    SessionPlanView {
                        snapshot: plan,
                        can_build: phase == "plan-current",
                    },
                );
                state.conversation_details.set_expanded(&id, true);
            }
            "compaction-running" | "compaction-complete" => {
                state.workspace = fixture_workspace.path().to_string_lossy().into_owned();
                state.transcript.push(assistant(
                    "La conversación mantiene sus archivos y decisiones al resumir el contexto.",
                ));
                if phase == "compaction-running" {
                    state
                        .conversation_activity
                        .begin_compaction(OWNER, fixture_workspace.path());
                } else {
                    state.transcript.push(TranscriptItem::Activity(
                        TranscriptActivity::Checkpoint {
                            id: "compact-density-review".into(),
                            covered_event_sequence: 42,
                        },
                    ));
                    state.transcript.push(assistant(
                        "El resumen está guardado y la conversación puede continuar.",
                    ));
                }
            }
            "file-footer" => {
                state.transcript.push(TranscriptItem::Tool {
                    id: "footer-file".into(), name: "read_file".into(),
                    arguments: json!({"path":"src/components.rs"}),
                    output: Some("pub struct Component {\n    pub title: String,\n    pub expanded: bool,\n}\n\nimpl Component {\n    pub fn collapse(&mut self) {\n        self.expanded = false;\n    }\n}".into()),
                    is_error: false, activity: ActivityStatus::Completed,
                });
                state
                    .conversation_details
                    .set_expanded("tool:footer-file", true);
            }
            "reasoning-footer" => {
                state.transcript.push(TranscriptItem::Reasoning {
                    id: "public-summary-footer".into(), step: 1,
                    content: "Resumen público de prueba: revisar las medidas compartidas y conservar las acciones existentes.\n\nEl control inferior vuelve al encabezado de esta misma sección. No abre el archivo ni cambia el estado de otra actividad.".into(),
                    status: ModelStreamStatus::Committed, truncated: false,
                });
                state
                    .conversation_details
                    .set_expanded("public-summary-footer", true);
            }
            "exploration-footer" => {
                for index in 0..3 {
                    state.transcript.push(exploration_fixture(
                        &format!("footer-read-{index}"),
                        "read_file",
                        ActivityStatus::Completed,
                    ));
                }
                state
                    .conversation_details
                    .set_expanded("work:footer-read-0", true);
            }
            "prose-reading" => {
                state.transcript.push(assistant("## Cómo está construido\n\nEl workspace está dividido en crates Rust: `openide-core` gestiona sesiones y persistencia; `openide-harness`, la ejecución de agentes; y `openide-ui`, los componentes de interfaz. Otros módulos se ocupan del editor, los lenguajes y las vistas previas.\n\nUna característica importante es que el IDE y el espacio de agentes comparten el mismo proyecto y sus archivos. Puedes abrirlos desde la conversación y volver al texto sin perder el lugar.\n\n## En qué estado está\n\nEl proyecto combina herramientas propias para trabajar con el código y continuar las tareas.\n\n- Las lecturas y búsquedas se agrupan.\n- Los archivos se abren desde su nombre.\n- Los errores siguen visibles."));
            }
            "exploration-group" | "exploration-group-open" | "exploration-group-attention" => {
                for index in 0..14 {
                    state.transcript.push(exploration_fixture(
                        &format!("scan-{index}"),
                        if index % 3 == 0 {
                            "list_files"
                        } else {
                            "read_file"
                        },
                        if index == 13 {
                            ActivityStatus::Running
                        } else {
                            ActivityStatus::Completed
                        },
                    ));
                }
                if phase == "exploration-group-open" {
                    state.conversation_details.set_expanded("work:scan-0", true);
                }
                if phase == "exploration-group-attention" {
                    state.transcript.push(exploration_fixture(
                        "needs-permission",
                        "read_file",
                        ActivityStatus::WaitingPermission,
                    ));
                    let mut failed =
                        exploration_fixture("failed-path", "read_file", ActivityStatus::Failed);
                    if let TranscriptItem::Tool { output, .. } = &mut failed {
                        *output = Some("No se pudo leer este archivo: permiso denegado".into());
                    }
                    state.transcript.push(failed);
                }
            }
            "response-streaming" | "awaiting-commit" => {
                state.running = true;
                state
                    .transcript
                    .push(model_activity(1, ActivityStatus::Running, true));
                state.transcript.push(TranscriptItem::Assistant {
                    content: "## Estructura\n\nEl proyecto combina un motor de Rust y componentes propios. La respuesta aparece aquí mientras el turno continúa activo.".into(),
                    streaming: phase == "response-streaming",
                });
            }
            "tool-media" | "tool-media-viewer" => {
                let output = mcp_image_result();
                let parsed = crate::tool_result_media::preview("mcp__browser", &output).unwrap();
                state.transcript.push(TranscriptItem::Tool {
                    id: "image-call".into(),
                    name: "mcp__openide_browser__browser_navigate".into(),
                    arguments: json!({"url":"https://example.test/reference"}),
                    output: Some(output),
                    is_error: false,
                    activity: ActivityStatus::Completed,
                });
                state.transcript.push(TranscriptItem::ToolMedia {
                    tool_call_id: "image-call".into(),
                    images: parsed
                        .images
                        .iter()
                        .map(|raster| TranscriptImage {
                            raster: Some(raster.clone()),
                            alt: raster.alt().into(),
                        })
                        .collect(),
                    text: parsed.text.clone(),
                    notices: Vec::new(),
                });
                state.conversation_details.toggle("tool:image-call", false);
                if phase == "tool-media-viewer" {
                    state.composer_image_viewer =
                        Some(crate::composer_attachments::ImageGalleryView {
                            workspace: state.workspace.clone(),
                            attachments: Vec::new(),
                            selected: 0,
                            generation: 1,
                            image: Some(parsed.images[0].clone()),
                            error: None,
                            loading: false,
                            tool_source: Some(crate::composer_attachments::ToolImageGallery {
                                owner: OWNER.into(),
                                call_id: "image-call".into(),
                                images: parsed.images.clone(),
                            }),
                        });
                }
            }
            "command-live" => {
                state.transcript.push(TranscriptItem::Tool {
                    id: "live-command".into(),
                    name: "run_command".into(),
                    arguments: json!({"command":"cargo test --workspace"}),
                    output: None,
                    is_error: false,
                    activity: ActivityStatus::Running,
                });
                state.transcript.push(TranscriptItem::ToolOutput { tool_call_id: "live-command".into(),
                    text: "Compiling openide-ui\nCompiling openide-harness\nCompiling agents-window\nrunning 14 tests\ntest streaming_order ... ok\ntest unicode_fragments ... ok\n".into(), truncated: false });
            }
            "tool-catalog" | "tool-details" => {
                state.transcript = vec![user("Revisar y mejorar el streaming de la aplicación.")];
                for (id, name, arguments, output, activity) in [
                    (
                        "read",
                        "read_file",
                        json!({"path":"src/main.rs"}),
                        "fn main() {}",
                        ActivityStatus::Completed,
                    ),
                    (
                        "search",
                        "search_files",
                        json!({"query":"stream"}),
                        "src/main.rs:12: stream",
                        ActivityStatus::Completed,
                    ),
                    (
                        "write",
                        "write_file",
                        json!({"path":"src/stream.rs","content":"pub fn stream() {}"}),
                        "Archivo actualizado",
                        ActivityStatus::Completed,
                    ),
                    (
                        "shell",
                        "run_command",
                        json!({"command":"cargo test streaming"}),
                        "running 4 tests\ntest incremental ... ok\ntest utf8 ... ok\ntest order ... ok\ntest usage ... ok\nresult: 4 passed",
                        ActivityStatus::Completed,
                    ),
                    (
                        "web",
                        "web_search",
                        json!({"query":"streaming API Rust"}),
                        "Referencia encontrada",
                        ActivityStatus::Running,
                    ),
                    (
                        "agent",
                        "spawn_agent",
                        json!({"title":"Revisar parser incremental"}),
                        "child-session-1",
                        ActivityStatus::Completed,
                    ),
                    (
                        "mcp",
                        "mcp__github__get_issue",
                        json!({"issue_number":42}),
                        "No se encontró el issue #42",
                        ActivityStatus::Failed,
                    ),
                ] {
                    state.transcript.push(TranscriptItem::Tool {
                        id: id.into(),
                        name: name.into(),
                        arguments,
                        output: (activity != ActivityStatus::Running).then(|| output.into()),
                        is_error: activity == ActivityStatus::Failed,
                        activity,
                    });
                }
                if phase == "tool-details" {
                    state.transcript.retain(|item| matches!(item, TranscriptItem::User { .. }) || matches!(item, TranscriptItem::Tool { id, .. } if id == "shell" || id == "mcp"));
                    state.conversation_details.toggle("tool:shell", false);
                    state.conversation_details.toggle("tool:mcp", false);
                }
            }
            "thinking" => {
                state.running = true;
                state
                    .transcript
                    .push(model_activity(1, ActivityStatus::Running, false));
                state.transcript.push(TranscriptItem::Reasoning {
                    id: "reason-public".into(),
                    step: 1,
                    content: PUBLIC_REASONING.into(),
                    status: ModelStreamStatus::Receiving,
                    truncated: false,
                });
            }
            "tools-running" => {
                state.running = true;
                state
                    .transcript
                    .push(model_activity(1, ActivityStatus::Completed, false));
                state.transcript.push(tool(ActivityStatus::Running));
            }
            "tools-error" | "tools-error-open" => {
                state.transcript.push(tool(ActivityStatus::Failed));
                state.transcript.push(assistant(
                    "El argumento no era válido; continúo con una ruta relativa.",
                ));
                if phase.ends_with("-open") {
                    state.selected_tool_id = Some(TOOL_ID.into());
                    state.conversation_details.toggle("tool:call-alpha", false);
                }
            }
            _ => {
                state.transcript.extend([
                    usage("usage-first", 1, ModelStreamStatus::Committed),
                    tool(ActivityStatus::Completed),
                    usage("usage-second", 2, if phase == "usage-partial" {
                        ModelStreamStatus::Failed
                    } else {
                        ModelStreamStatus::Committed
                    }),
                    assistant("## Estructura\n\nEl proyecto combina un motor de Rust y componentes propios."),
                ]);
                if phase.ends_with("-open") {
                    state.conversation_details.toggle("usage-first", false);
                }
                if phase == "jump-control" {
                    state.show_jump_to_bottom = true;
                }
            }
        }
        for width in [360.0, 620.0, 900.0] {
            let root = if phase == "tool-media-viewer" {
                crate::view::build_window_view(&state, width, 900.0)
            } else {
                build_session_surface(&state, width, false, false)
            };
            let (context, mut tree) = super::file_activity_visual_tests::native_tree(root);
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 900.0));
            let path = directory.join(format!("stream-{phase}-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 900.0)
                .unwrap();
            let options = crate::settings_ui::visual_qa::native_svg_options();
            let svg = std::fs::read_to_string(&path).unwrap();
            let svg_tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 900).unwrap();
            resvg::render(
                &svg_tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            let mut output = context.end_pass();
            // This exporter consumes draw commands, not a GPU texture atlas.
            output.textures_delta.clear();
        }
    }
}

#[test]
#[ignore = "exports the flat streaming lines as PNG under docs/ui-separation/shots/streaming"]
fn export_streaming_lines_visual_review() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("OPENIDE_STREAM_VISUAL_DIR").unwrap_or_else(|| {
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/ui-separation/shots/streaming"
            )
            .into()
        }),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let call = |id: &str, name: &str, arguments: serde_json::Value, activity| TranscriptItem::Tool {
        id: id.into(),
        name: name.into(),
        arguments,
        output: (activity != ActivityStatus::Running).then(|| "ok".into()),
        is_error: activity == ActivityStatus::Failed,
        activity,
    };
    let reasoning = |status| TranscriptItem::Reasoning {
        id: "reason".into(),
        step: 1,
        content: PUBLIC_REASONING.into(),
        status,
        truncated: false,
    };
    let prose = "Voy a mirar la documentación y la estructura del repo para contarte de qué trata el proyecto, sin inventar.";
    let explored = |running: usize| {
        [
            ("search-a", "find_files", json!({"pattern":"**/*.{md,txt}", "path":"presupuestacion"})),
            ("search-b", "search_text", json!({"query":"Editar hormigón|Editar molde"})),
            ("read-a", "read_file", json!({"path":"web/package.json"})),
            ("read-b", "read_file", json!({"path":"docs/README.md", "start_line":130, "end_line":329})),
            ("read-c", "read_file", json!({"path":"docs/funcionalidades.md"})),
            ("list-a", "list_files", json!({"path":"web/src/pantallas"})),
        ]
        .into_iter()
        .enumerate()
        .take(running + 1)
        .map(|(index, (id, name, arguments))| {
            call(
                id,
                name,
                arguments,
                if index == running {
                    ActivityStatus::Running
                } else {
                    ActivityStatus::Completed
                },
            )
        })
        .collect::<Vec<_>>()
    };
    let build = |phase: &str| {
        let mut state = state();
        state.transcript = vec![user("¿De qué trata el proyecto?")];
        state.running = true;
        match phase {
            "waiting" => {}
            "reasoning-live" => {
                state
                    .transcript
                    .push(model_activity(1, ActivityStatus::Running, false));
                state
                    .transcript
                    .push(reasoning(ModelStreamStatus::Receiving));
            }
            "reasoning-done" => {
                state
                    .transcript
                    .push(reasoning(ModelStreamStatus::Committed));
                state.transcript.push(assistant(prose));
                state.running = false;
            }
            "exploration-live-one-file" => {
                state.transcript.push(assistant(prose));
                state.transcript.extend(explored(2));
            }
            "exploration-live-step-a" | "exploration-live-step-mid" => {
                state.transcript.push(assistant(prose));
                state.transcript.extend(explored(3));
            }
            "exploration-live-step-b" => {
                state.transcript.push(assistant(prose));
                state.transcript.extend(explored(4));
            }
            "exploration-live-between-steps" => {
                state.transcript.push(assistant(prose));
                state.transcript.extend(explored(6));
            }
            "exploration-done" | "exploration-done-open" | "exploration-failed" | "full-turn" => {
                state.running = false;
                state.transcript.push(assistant(prose));
                state.transcript.extend(explored(6));
                if phase == "exploration-done-open" {
                    state.conversation_details.set_expanded("work:search-a", true);
                }
                if phase == "exploration-failed" {
                    let mut failed = call(
                        "read-missing",
                        "read_file",
                        json!({"path":"docs/modulos.md"}),
                        ActivityStatus::Failed,
                    );
                    if let TranscriptItem::Tool { output, .. } = &mut failed {
                        *output = Some("No se pudo leer este archivo: no existe".into());
                    }
                    state.transcript.push(failed);
                }
                if phase == "full-turn" {
                    state.transcript.insert(1, reasoning(ModelStreamStatus::Committed));
                    state.transcript.extend([
                        assistant("El constructor decide qué se puede editar. Ajusto la pantalla y corro las pruebas."),
                        call(
                            "edit",
                            "write_file",
                            json!({"path":"web/src/pantallas/PantallaPiezas.tsx","content":"export const editable = Boolean(constructor);\n"}),
                            ActivityStatus::Completed,
                        ),
                        TranscriptItem::Tool {
                            id: "shell".into(),
                            name: "run_command".into(),
                            arguments: json!({"command":"npm test -- piezas"}),
                            output: Some("PASS src/pantallas/PantallaPiezas.test.tsx\nTests: 4 passed, 4 total".into()),
                            is_error: false,
                            activity: ActivityStatus::Completed,
                        },
                        assistant("Listo: sin constructor ya no aparece la edición."),
                    ]);
                }
            }
            _ => unreachable!(),
        }
        state
    };
    for phase in [
        "waiting",
        "reasoning-live",
        "reasoning-done",
        "exploration-live-one-file",
        "exploration-live-step-a",
        "exploration-live-step-mid",
        "exploration-live-step-b",
        "exploration-live-between-steps",
        "exploration-done",
        "exploration-done-open",
        "exploration-failed",
        "full-turn",
    ] {
        // The whole turn needs room for its edit and command cards.
        let height = if phase == "full-turn" { 900.0 } else { 560.0 };
        for width in [360.0, 900.0] {
            let viewport = Size::new(width, height);
            let root = build_session_surface(&build(phase), width, false, false);
            let (context, mut tree) = super::file_activity_visual_tests::native_tree(root);
            let frame = if phase == "exploration-live-step-mid" {
                // The same tree sees the step change and is caught on the way.
                tree.set_reduced_motion(false);
                let clock = Instant::now();
                tree.layout_at(viewport, clock);
                let next = build("exploration-live-step-b");
                tree.set_root(build_session_surface(&next, width, false, false));
                tree.layout_at(viewport, clock);
                tree.layout_at(
                    viewport,
                    clock + Duration::from_millis(u64::from(openide_ui::tokens().motion.move_ms) / 4),
                )
            } else {
                tree.set_reduced_motion(true);
                tree.layout(viewport)
            };
            let path = directory.join(format!("{phase}-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
                .unwrap();
            let options = crate::settings_ui::visual_qa::native_svg_options();
            let svg = std::fs::read_to_string(&path).unwrap();
            let svg_tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &svg_tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            std::fs::remove_file(&path).unwrap();
            let mut output = context.end_pass();
            // This exporter consumes draw commands, not a GPU texture atlas.
            output.textures_delta.clear();
        }
    }
}

/// A long conversation whose calls are far apart: a folded run of
/// exploration at the start, prose in between, a command at the end.
fn reveal_fixture() -> ViewState {
    let mut state = state();
    state.transcript.extend([
        exploration_fixture("reveal-a", "read_file", ActivityStatus::Completed),
        exploration_fixture("reveal-b", "read_file", ActivityStatus::Completed),
        exploration_fixture("reveal-c", "search_text", ActivityStatus::Completed),
    ]);
    for index in 0..30 {
        state.transcript.push(assistant(&format!(
            "Párrafo {index}: sigo revisando la estructura y anoto lo que encuentro en cada módulo."
        )));
    }
    state.transcript.push(TranscriptItem::Tool {
        id: "reveal-shell".into(),
        name: "run_command".into(),
        arguments: json!({"command":"cargo test"}),
        output: Some("ok".into()),
        is_error: false,
        activity: ActivityStatus::Completed,
    });
    for index in 0..30 {
        state
            .transcript
            .push(assistant(&format!("Cierre {index}: queda documentado el resultado.")));
    }
    state
}

fn region_rect(frame: &openide_ui::Frame, id: &str) -> Option<openide_ui::Rect> {
    frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .map(|region| region.rect)
}

#[test]
fn reveal_opens_the_folded_group_and_brings_the_call_into_view() {
    let viewport = Size::new(900.0, 600.0);
    let mut state = reveal_fixture();
    let mut tree = surface(&state, viewport.width, true);
    tree.layout(viewport);
    tree.scroll_to_end("transcript");
    tree.layout(viewport);
    let end = tree.scroll_offset("transcript");
    assert!(end > 600.0);

    // A call inside a folded run: its group opens and its own line shows.
    let line = open_file("reveal-b");
    assert!(region_rect(&tree.layout(viewport), &line).is_none());
    assert_eq!(reveal_target(&mut state, "reveal-b"), Some(line.clone()));
    assert!(state.conversation_details.is_expanded("work:reveal-a", false));
    tree.reveal_with_margin(&line, REVEAL_MARGIN);
    tree.set_root(build_session_surface(&state, viewport.width, false, false));
    let shown = region_rect(&tree.layout(viewport), &line).expect("the line is laid out");
    // Clear of the header and its fade.
    assert!(shown.y >= HEADER_HEIGHT + HEADER_FADE_HEIGHT, "{shown:?}");
    assert!(shown.y + shown.height <= viewport.height, "{shown:?}");
    assert!(tree.scroll_offset("transcript") < end);

    // A card is found by its header, from wherever the transcript stands.
    let card = action("tool:reveal-shell");
    assert_eq!(reveal_target(&mut state, "reveal-shell"), Some(card.clone()));
    tree.scroll_to_start("transcript");
    tree.layout(viewport);
    tree.reveal_with_margin(&card, REVEAL_MARGIN);
    let shown = region_rect(&tree.layout(viewport), &card).expect("the card is laid out");
    assert!(shown.y >= 0.0 && shown.y + shown.height <= viewport.height, "{shown:?}");
    assert_eq!(reveal_target(&mut state, "missing"), None);

    // The call just jumped to is marked, and only that one.
    let marked = |state: &ViewState| {
        UiTree::new(build_session_surface(state, viewport.width, false, false))
            .layout(viewport)
            .commands
            .iter()
            .filter(|command| {
                matches!(command, DrawCommand::StrokeRect { color, .. } if *color == theme::FOREGROUND)
            })
            .count()
    };
    let before = marked(&state);
    state.activity.revealed = Some(("reveal-b".into(), Instant::now()));
    assert_eq!(marked(&state), before + 1);
}

#[test]
#[ignore = "exports the transcript before and after a reveal as PNG under docs/ui-separation/shots/activity"]
fn export_transcript_reveal_visual_review() {
    let directory = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/ui-separation/shots/activity"
    ));
    std::fs::create_dir_all(&directory).unwrap();
    for width in [420.0, 900.0] {
        let viewport = Size::new(width, 600.0);
        let mut state = reveal_fixture();
        let (context, mut tree) = super::file_activity_visual_tests::native_tree(
            build_session_surface(&state, width, false, false),
        );
        tree.set_reduced_motion(true);
        tree.layout(viewport);
        tree.scroll_to_end("transcript");
        let mut save = |tree: &mut UiTree, name: &str| {
            let frame = tree.layout(viewport);
            let path = directory.join(format!("{name}-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 600.0)
                .unwrap();
            let options = crate::settings_ui::visual_qa::native_svg_options();
            let svg = std::fs::read_to_string(&path).unwrap();
            let svg_tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 600).unwrap();
            resvg::render(
                &svg_tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            std::fs::remove_file(&path).unwrap();
        };
        save(&mut tree, "transcript-before-reveal");
        for (call, name) in [
            ("reveal-b", "transcript-after-reveal-line"),
            ("reveal-shell", "transcript-after-reveal-card"),
        ] {
            let target = reveal_target(&mut state, call).unwrap();
            state.activity.revealed = Some((call.into(), Instant::now()));
            tree.set_root(build_session_surface(&state, width, false, false));
            tree.reveal_with_margin(&target, REVEAL_MARGIN);
            save(&mut tree, name);
        }
        let mut output = context.end_pass();
        output.textures_delta.clear();
    }
}
