use super::*;
use crate::transcript::ActivityStatus;
use openide_ui::{DrawCommand, Size, UiTree};
use serde_json::json;

const OWNER: &str = "chat-diff-fixture";

#[test]
fn every_unadmitted_diff_reports_capacity_instead_of_promising_a_load() {
    let (_temp, mut app) = fixture();
    app.sync_chat_diff_scope();
    app.view.chat_diffs.open = (0..16).map(|index| format!("open-{index}")).collect();
    app.view.chat_diffs.limit_notice = Some("extra-first".into());
    for tool in ["extra-first", "extra-second", "extra-third"] {
        let mut tree = UiTree::new(chat_diff_ui::body(&app.view, tool));
        let frame = tree.layout(Size::new(900.0, 300.0));
        let text = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("límite de 16"), "{text}");
        assert!(!text.contains("Preparando"));
    }
}

fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("main.rs"), "fn original() {}\n").unwrap();
    let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
    app.view.workspace = workspace
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.view.selected_session = Some(OWNER.into());
    app.view.show_side_pane = false;
    app.view.sidebar_collapsed = true;
    let store = app.store.as_ref().unwrap().storage_root();
    let mut events = vec![AgentEvent::new(
        OWNER,
        1,
        AgentEventKind::SessionStarted {
            workspace: workspace.clone(),
        },
    )];
    crate::session_changes::capture_baseline(store, OWNER, &workspace, &events).unwrap();
    for (id, content) in [
        ("edit-first", "fn middle() {}\n"),
        ("edit-second", "fn final_version() {}\n"),
    ] {
        events.push(AgentEvent::new(
            OWNER,
            events.len() as u64 + 1,
            AgentEventKind::ToolStarted {
                tool_call: ToolCall {
                    id: id.into(),
                    name: "write_file".into(),
                    arguments: json!({"path":"main.rs","content":content}),
                },
            },
        ));
        events.push(AgentEvent::new(
            OWNER,
            events.len() as u64 + 1,
            AgentEventKind::ToolFinished {
                tool_call_id: id.into(),
                output: "File written".into(),
                is_error: false,
            },
        ));
    }
    let mut journal = Vec::new();
    for event in &events {
        serde_json::to_writer(&mut journal, event).unwrap();
        journal.push(b'\n');
    }
    std::fs::write(store.join(format!("{OWNER}.jsonl")), journal).unwrap();
    app.view.transcript = vec![TranscriptItem::User {
        content: "Edita este archivo".into(),
        images: Vec::new(),
    }];
    for id in ["edit-first", "edit-second"] {
        app.view
            .transcript
            .push(tool(id, ActivityStatus::Completed, false));
    }
    // Legacy disclosure tests start with the user's explicit closed choice.
    // Separate tests below cover the file-card's automatic live defaults.
    app.sync_chat_diff_scope();
    app.set_chat_diff_disclosure("edit-first", false);
    app.set_chat_diff_disclosure("edit-second", false);
    (temp, app)
}
fn tool(id: &str, activity: ActivityStatus, is_error: bool) -> TranscriptItem {
    TranscriptItem::Tool {
        id: id.into(),
        name: "write_file".into(),
        arguments: json!({"path":"main.rs","content":"not trusted presentation data"}),
        output: (activity == ActivityStatus::Completed || is_error).then(|| "ok".into()),
        is_error,
        activity,
    }
}
fn toggle(app: &mut AgentsWindow, owner: &str, id: &str) {
    app.apply_action(
        UiAction::Clicked {
            id: chat_diff_ui::action(owner, id, false),
        },
        &egui::Context::default(),
    );
}
fn wait(app: &mut AgentsWindow) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.view.chat_diffs.loading.is_empty() {
        app.poll_chat_diffs(&egui::Context::default());
        assert!(
            Instant::now() < deadline,
            "chat diff worker did not complete"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn texts(entry: &ToolDiffEntry) -> Vec<&str> {
    entry
        .files
        .iter()
        .flat_map(|file| file.lines.iter())
        .map(|line| line.text.as_str())
        .collect()
}

#[test]
fn inline_diff_is_closed_until_requested_and_uses_confirmed_history() {
    let (_temp, mut app) = fixture();
    let mut tree = UiTree::new(chat_diff_ui::build(&app.view, "edit-first"));
    let frame = tree.layout(Size::new(360.0, 320.0));
    assert!(frame
        .regions
        .iter()
        .all(|r| !r.id.starts_with("chat-diff:")));
    assert!(app.chat_diffs.pending.is_none());
    std::fs::write(
        Path::new(&app.view.workspace).join("main.rs"),
        "manual version unrelated\n",
    )
    .unwrap();
    toggle(&mut app, OWNER, "edit-second");
    wait(&mut app);
    let entry = &app.view.chat_diffs.entries["edit-second"];
    assert!(entry.unavailable.is_none());
    assert!(texts(entry).contains(&"fn middle() {}"));
    assert!(texts(entry).contains(&"fn final_version() {}"));
    assert!(!texts(entry)
        .iter()
        .any(|x| x.contains("manual version") || x.contains("not trusted")));
    assert!(!app.view.show_side_pane);
    let mut tree = UiTree::new(chat_diff_ui::build(&app.view, "edit-second"));
    let frame = tree.layout(Size::new(360.0, 420.0));
    assert!(frame
        .regions
        .iter()
        .any(|r| r.id == chat_diff_ui::view_id(OWNER, "edit-second", 0)));
    assert!(frame
        .commands
        .iter()
        .any(|c| matches!(c, DrawCommand::Text { text, .. } if text.contains("final_version"))));
}

#[test]
fn cache_reuses_arc_and_does_not_repeat_io_on_reopen_or_manual_edits() {
    let (_temp, mut app) = fixture();
    toggle(&mut app, OWNER, "edit-first");
    wait(&mut app);
    let generation = app.chat_diffs.generation;
    let lines = app.view.chat_diffs.entries["edit-first"].files[0]
        .lines
        .clone();
    toggle(&mut app, OWNER, "edit-first");
    std::fs::write(
        Path::new(&app.view.workspace).join("main.rs"),
        "changed again\n",
    )
    .unwrap();
    toggle(&mut app, OWNER, "edit-first");
    assert_eq!(app.chat_diffs.generation, generation);
    assert!(Arc::ptr_eq(
        &lines,
        &app.view.chat_diffs.entries["edit-first"].files[0].lines
    ));
    assert!(app.view.chat_diffs.loading.is_empty());
}

#[test]
fn two_quick_disclosures_queue_bounded_work_and_keep_both_results() {
    let (_temp, mut app) = fixture();
    toggle(&mut app, OWNER, "edit-first");
    toggle(&mut app, OWNER, "edit-second");
    assert_eq!(app.view.chat_diffs.loading.len(), 2);
    assert_eq!(app.chat_diffs.waiting.len(), 1);
    wait(&mut app);
    assert_eq!(app.view.chat_diffs.entries.len(), 2);
    assert!(texts(&app.view.chat_diffs.entries["edit-first"]).contains(&"fn original() {}"));
    assert!(texts(&app.view.chat_diffs.entries["edit-second"]).contains(&"fn middle() {}"));
    assert!(!app.view.show_side_pane);
}

#[test]
fn scope_change_drops_pending_results_and_stale_actions() {
    let (_temp, mut app) = fixture();
    toggle(&mut app, OWNER, "edit-first");
    app.view.selected_session = Some("different-owner".into());
    app.view.transcript.clear();
    assert!(app.poll_chat_diffs(&egui::Context::default()));
    assert!(app.view.chat_diffs.entries.is_empty());
    assert!(app.view.chat_diffs.loading.is_empty());
    assert!(app.chat_diffs.pending.is_none());
    assert!(!app.poll_chat_diffs(&egui::Context::default()));
    toggle(&mut app, OWNER, "edit-first");
    assert!(app.view.chat_diffs.open.is_empty());
    assert!(!app.view.show_side_pane);
}

fn reset_card_choices(app: &mut AgentsWindow) {
    app.view.conversation_details = Default::default();
    app.view
        .conversation_details
        .sync(app.view.selected_session.as_deref());
}

fn toggle_card(app: &mut AgentsWindow, tool: &str) {
    app.apply_action(
        UiAction::Clicked {
            id: format!("conversation_detail:{OWNER}:tool:{tool}"),
        },
        &egui::Context::default(),
    );
}

#[test]
fn latest_confirmed_cards_load_automatically_and_manual_collapse_survives_polling() {
    let (_temp, mut app) = fixture();
    reset_card_choices(&mut app);
    assert!(app.poll_chat_diffs(&egui::Context::default()));
    assert_eq!(app.view.chat_diffs.open.len(), 2);
    assert_eq!(app.view.chat_diffs.loading.len(), 2);
    assert!(!app.view.show_side_pane);
    wait(&mut app);
    assert_eq!(app.view.chat_diffs.entries.len(), 2);
    let generation = app.chat_diffs.generation;
    let lines = app.view.chat_diffs.entries["edit-second"].files[0]
        .lines
        .clone();
    toggle_card(&mut app, "edit-second");
    assert!(app.poll_chat_diffs(&egui::Context::default()));
    assert!(!app.view.chat_diffs.open.contains("edit-second"));
    for _ in 0..3 {
        assert!(!app.poll_chat_diffs(&egui::Context::default()));
    }
    toggle_card(&mut app, "edit-second");
    assert!(app.poll_chat_diffs(&egui::Context::default()));
    assert!(app.view.chat_diffs.open.contains("edit-second"));
    assert!(app.view.chat_diffs.loading.is_empty());
    assert_eq!(app.chat_diffs.generation, generation);
    assert!(Arc::ptr_eq(
        &lines,
        &app.view.chat_diffs.entries["edit-second"].files[0].lines
    ));
    assert!(!app.view.show_side_pane);
}

#[test]
fn closing_a_default_card_cancels_queued_reconstruction_and_next_turn_stays_closed() {
    let (_temp, mut app) = fixture();
    reset_card_choices(&mut app);
    assert!(app.sync_expanded_chat_diffs());
    assert_eq!(
        app.chat_diffs.pending.as_ref().unwrap().tool_call_id,
        "edit-second"
    );
    assert_eq!(
        app.chat_diffs.waiting.front().map(String::as_str),
        Some("edit-first")
    );
    toggle_card(&mut app, "edit-first");
    assert!(app.sync_expanded_chat_diffs());
    assert!(app.chat_diffs.waiting.is_empty());
    assert!(!app.view.chat_diffs.loading.contains("edit-first"));
    wait(&mut app);
    assert!(!app.view.chat_diffs.entries.contains_key("edit-first"));
    app.view.transcript.push(TranscriptItem::User {
        content: "Next turn".into(),
        images: vec![],
    });
    app.poll_chat_diffs(&egui::Context::default());
    assert!(app.view.chat_diffs.open.is_empty());
    assert!(!app.view.show_side_pane);
}

#[test]
fn cards_bound_automatic_work_and_scope_reopen_reloads_only_current_history() {
    let (_temp, mut app) = fixture();
    reset_card_choices(&mut app);
    for number in 0..40 {
        let id = format!("extra-{number}");
        app.view
            .transcript
            .push(tool(&id, ActivityStatus::Completed, false));
        app.set_chat_diff_disclosure(&id, true);
    }
    app.poll_chat_diffs(&egui::Context::default());
    assert_eq!(app.view.chat_diffs.open.len(), MAX_CACHED_DIFFS);
    assert_eq!(app.view.chat_diffs.loading.len(), MAX_CACHED_DIFFS);
    assert_eq!(app.chat_diffs.waiting.len(), MAX_CACHED_DIFFS - 1);
    assert!(app.view.chat_diffs.limit_notice.is_some());
    let transcript = std::mem::take(&mut app.view.transcript);
    app.view.selected_session = Some("different-owner".into());
    assert!(app.poll_chat_diffs(&egui::Context::default()));
    assert!(app.view.chat_diffs.entries.is_empty());
    assert!(app.view.chat_diffs.loading.is_empty());
    assert!(app.chat_diffs.pending.is_none());
    assert!(app.chat_diffs.waiting.is_empty());
    app.view.selected_session = Some(OWNER.into());
    app.view.transcript = transcript.into_iter().take(3).collect();
    assert!(app.poll_chat_diffs(&egui::Context::default()));
    assert_eq!(app.view.chat_diffs.open.len(), 2);
    wait(&mut app);
    assert_eq!(app.view.chat_diffs.entries.len(), 2);
    assert!(texts(&app.view.chat_diffs.entries["edit-second"]).contains(&"fn final_version() {}"));
    assert!(!app.view.show_side_pane);
}

#[test]
fn automatic_cards_never_load_failed_unfinished_or_duplicate_results() {
    let (_temp, mut app) = fixture();
    reset_card_choices(&mut app);
    for items in [
        vec![tool("edit-first", ActivityStatus::Running, false)],
        vec![tool("edit-first", ActivityStatus::Failed, true)],
        vec![tool("edit-first", ActivityStatus::Interrupted, false)],
        vec![tool("edit-first", ActivityStatus::Cancelled, false)],
        vec![
            tool("edit-first", ActivityStatus::Completed, false),
            tool("edit-first", ActivityStatus::Completed, false),
        ],
    ] {
        app.view.transcript = items;
        app.poll_chat_diffs(&egui::Context::default());
        assert!(app.view.chat_diffs.open.is_empty());
        assert!(app.chat_diffs.pending.is_none());
        assert!(app.view.chat_diffs.entries.is_empty());
    }
}

#[test]
fn confirmation_without_a_new_row_invalidates_the_expansion_cache_but_idle_frames_do_not() {
    let (_temp, mut app) = fixture();
    reset_card_choices(&mut app);
    app.view.transcript = vec![tool("edit-first", ActivityStatus::Running, false)];
    app.last_sequence = 2;
    app.poll_chat_diffs(&egui::Context::default());
    assert!(app.chat_diffs.pending.is_none());
    let before = app.chat_diffs.expansion_revision;
    let generation = app.chat_diffs.generation;
    for _ in 0..120 {
        assert!(!app.poll_chat_diffs(&egui::Context::default()));
    }
    assert_eq!(app.chat_diffs.expansion_revision, before);
    assert_eq!(app.chat_diffs.generation, generation);
    // ToolFinished replaces a row rather than appending one. Its durable
    // sequence must invalidate the cached disclosure/confirmation decision.
    app.view.transcript[0] = tool("edit-first", ActivityStatus::Completed, false);
    app.last_sequence = 3;
    assert!(app.poll_chat_diffs(&egui::Context::default()));
    wait(&mut app);
    assert!(app.view.chat_diffs.entries.contains_key("edit-first"));
    toggle_card(&mut app, "edit-first");
    assert!(app.poll_chat_diffs(&egui::Context::default()));
    assert!(app.view.chat_diffs.open.is_empty());
    assert!(!app.poll_chat_diffs(&egui::Context::default()));
}

#[test]
fn confirmed_diff_file_actions_validate_owner_index_workspace_and_current_file_boundary() {
    let (temp, mut app) = fixture();
    toggle(&mut app, OWNER, "edit-first");
    wait(&mut app);
    let click = |app: &mut AgentsWindow, owner: &str, index| {
        assert!(app.apply_tool_navigation_action(
            &UiAction::Clicked {
                id: chat_diff_ui::file_action(owner, "edit-first", index)
            },
            &egui::Context::default(),
        ));
    };
    click(&mut app, "old-owner", 0);
    click(&mut app, OWNER, 99);
    assert!(!app.view.show_side_pane);
    let original = app.view.workspace.clone();
    app.view.workspace = temp.path().to_string_lossy().into_owned();
    click(&mut app, OWNER, 0);
    assert!(!app.view.show_side_pane);
    app.view.workspace = original;
    let outside = temp.path().join("outside.rs");
    std::fs::write(&outside, "outside\n").unwrap();
    app.view
        .chat_diffs
        .entries
        .get_mut("edit-first")
        .unwrap()
        .files[0]
        .path = outside.to_string_lossy().into_owned();
    click(&mut app, OWNER, 0);
    assert!(!app.view.show_side_pane);
    assert!(app.view.status.contains("dentro del proyecto"));
    let file = &mut app
        .view
        .chat_diffs
        .entries
        .get_mut("edit-first")
        .unwrap()
        .files[0];
    file.path = "main.rs".into();
    file.can_open = false;
    click(&mut app, OWNER, 0);
    assert!(!app.view.show_side_pane);
    app.view
        .chat_diffs
        .entries
        .get_mut("edit-first")
        .unwrap()
        .files[0]
        .can_open = true;
    click(&mut app, OWNER, 0);
    assert!(app.view.show_side_pane);
    assert_eq!(app.view.file_preview.as_ref().unwrap().path, "main.rs");
}

#[test]
fn failed_running_uncertain_unknown_and_duplicate_calls_cannot_open_diffs() {
    let (_temp, mut app) = fixture();
    for (status, is_error) in [
        (ActivityStatus::Running, false),
        (ActivityStatus::Failed, true),
        (ActivityStatus::Interrupted, false),
        (ActivityStatus::Cancelled, false),
    ] {
        app.view.transcript = vec![tool("edit-first", status, is_error)];
        toggle(&mut app, OWNER, "edit-first");
        assert!(app.view.chat_diffs.open.is_empty());
        assert!(app.chat_diffs.pending.is_none());
    }
    app.view.transcript = vec![
        tool("edit-first", ActivityStatus::Completed, false),
        tool("edit-first", ActivityStatus::Completed, false),
    ];
    toggle(&mut app, OWNER, "edit-first");
    toggle(&mut app, OWNER, "unknown");
    assert!(app.view.chat_diffs.open.is_empty());
}

#[test]
fn unavailable_history_is_visible_and_retry_requires_explicit_action() {
    let (_temp, mut app) = fixture();
    let journal = app
        .store
        .as_ref()
        .unwrap()
        .storage_root()
        .join(format!("{OWNER}.jsonl"));
    std::fs::write(&journal, b"incomplete").unwrap();
    toggle(&mut app, OWNER, "edit-first");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.view.chat_diffs.loading.is_empty() {
        let changed = app.poll_chat_diffs(&egui::Context::default());
        if app.view.chat_diffs.entries.contains_key("edit-first") {
            assert!(
                changed,
                "a failed history read must invalidate the visible frame"
            );
        }
        assert!(Instant::now() < deadline, "chat diff worker did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.view.chat_diffs.entries["edit-first"]
        .unavailable
        .is_some());
    assert!(app.view.chat_diffs.entries["edit-first"].files.is_empty());
    let mut tree = UiTree::new(chat_diff_ui::build(&app.view, "edit-first"));
    let frame = tree.layout(Size::new(360.0, 500.0));
    assert!(frame
        .regions
        .iter()
        .any(|r| r.id == chat_diff_ui::action(OWNER, "edit-first", true)));
    assert!(frame
        .regions
        .iter()
        .all(|r| !r.id.starts_with("chat-diff:")));
}

#[test]
fn cache_and_expansion_limits_do_not_grow_with_transcript_length() {
    let (_temp, mut app) = fixture();
    app.sync_chat_diff_scope();
    for id in 0..MAX_CACHED_DIFFS {
        let key = format!("cached-{id}");
        app.view.chat_diffs.open.insert(key.clone());
        app.cache_chat_diff(key, ToolDiffEntry::from_result(Err("not available".into())));
    }
    toggle(&mut app, OWNER, "edit-first");
    assert_eq!(app.view.chat_diffs.entries.len(), MAX_CACHED_DIFFS);
    assert_eq!(app.view.chat_diffs.open.len(), MAX_CACHED_DIFFS);
    assert_eq!(
        app.view.chat_diffs.limit_notice.as_deref(),
        Some("edit-first")
    );
    assert!(app.chat_diffs.pending.is_none());
    app.view.chat_diffs.open.remove("cached-0");
    toggle(&mut app, OWNER, "edit-first");
    wait(&mut app);
    assert_eq!(app.view.chat_diffs.entries.len(), MAX_CACHED_DIFFS);
    assert!(app.view.chat_diffs.entries.contains_key("edit-first"));
}

#[test]
fn closing_queued_diffs_cancels_waiting_work_and_keeps_the_queue_bounded() {
    let (_temp, mut app) = fixture();
    toggle(&mut app, OWNER, "edit-first");
    for number in 0..100 {
        let id = format!("queued-{number}");
        app.view
            .transcript
            .push(tool(&id, ActivityStatus::Completed, false));
        toggle(&mut app, OWNER, &id);
        assert_eq!(app.chat_diffs.waiting.len(), 1);
        toggle(&mut app, OWNER, &id);
        assert!(app.chat_diffs.waiting.is_empty());
        assert_eq!(app.view.chat_diffs.loading.len(), 1);
    }
    // An admitted read may finish, but closing it does not let new waiting
    // work exceed the independent pending+waiting budget.
    toggle(&mut app, OWNER, "edit-first");
    for number in 0..MAX_CACHED_DIFFS - 1 {
        let id = format!("bounded-{number}");
        app.view
            .transcript
            .push(tool(&id, ActivityStatus::Completed, false));
        toggle(&mut app, OWNER, &id);
    }
    app.view
        .transcript
        .push(tool("over-budget", ActivityStatus::Completed, false));
    toggle(&mut app, OWNER, "over-budget");
    assert_eq!(app.chat_diffs.waiting.len(), MAX_CACHED_DIFFS - 1);
    assert_eq!(app.view.chat_diffs.loading.len(), MAX_CACHED_DIFFS);
    assert!(!app.view.chat_diffs.open.contains("over-budget"));
    assert_eq!(
        app.view.chat_diffs.limit_notice.as_deref(),
        Some("over-budget")
    );
}

#[test]
fn diff_conversion_preserves_missing_line_ending_evidence() {
    use crate::git_review::{ReviewFile, ReviewLine, ReviewLineKind};
    let metadata = |text: &str| ReviewLine {
        kind: ReviewLineKind::Meta,
        text: text.into(),
        old_number: None,
        new_number: None,
    };
    let entry = ToolDiffEntry::from_result(Ok(crate::session_tool_diff::SessionToolDiff {
        session_id: OWNER.into(),
        workspace: PathBuf::from("fixture"),
        tool_call_id: "edit".into(),
        sequence: 3,
        limited: false,
        unavailable: None,
        coverage_notes: Vec::new(),
        files: vec![crate::session_changes::SessionFileChange {
            file: ReviewFile {
                path: "main.rs".into(),
                lines: vec![
                    metadata("index old..new 100644"),
                    metadata("\\ No newline at end of file"),
                ],
                ..Default::default()
            },
            last_tool_sequence: 3,
            unavailable: None,
            truncated: false,
        }],
    }));
    assert_eq!(entry.files[0].lines.len(), 1);
    assert_eq!(entry.files[0].lines[0].text, "\\ No newline at end of file");
    assert_eq!(entry.files[0].lines[0].old_line, None);
    assert_eq!(entry.files[0].lines[0].new_line, None);
}

#[test]
fn inline_editor_diff_keeps_the_chat_surface_and_sidebar_under_user_control() {
    let (_temp, mut app) = fixture();
    for width in [360.0, 900.0] {
        app.view.conversation_details.sync(Some(OWNER));
        if app.view.chat_diffs.open.contains("edit-first") {
            toggle(&mut app, OWNER, "edit-first");
        }
        let mut tree = UiTree::new(crate::session_surface::build_session_surface(
            &app.view, width, false, false,
        ));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(width, 720.0));
        assert!(frame
            .regions
            .iter()
            .all(|r| !r.id.starts_with("chat-diff:")));
        if !app.view.chat_diffs.open.contains("edit-first") {
            toggle(&mut app, OWNER, "edit-first");
            wait(&mut app);
        }
        tree.set_root(crate::session_surface::build_session_surface(
            &app.view, width, false, false,
        ));
        let frame = tree.layout(Size::new(width, 720.0));
        let editor = frame
            .regions
            .iter()
            .find(|r| r.id == chat_diff_ui::view_id(OWNER, "edit-first", 0))
            .expect("inline editor diff must be inside the chat");
        assert!(editor.rect.x >= 0.0);
        assert!(editor.rect.x + editor.rect.width <= width);
        assert!(!app.view.show_side_pane);
    }
}

#[test]
fn inline_file_cards_preserve_basename_native_font_budget_and_full_path_tooltip() {
    let (_temp, mut app) = fixture();
    app.view.conversation_details.sync(Some(OWNER));
    toggle(&mut app, OWNER, "edit-first");
    wait(&mut app);
    for (width, long_path) in [(360.0, false), (360.0, true), (900.0, true)] {
        let path = if long_path {
            format!("src/{}main.rs", "nested-module/".repeat(4))
        } else {
            "main.rs".to_owned()
        };
        for item in &mut app.view.transcript {
            if let TranscriptItem::Tool { arguments, .. } = item {
                arguments["path"] = path.clone().into();
            }
        }
        for entry in app.view.chat_diffs.entries.values_mut() {
            entry.files[0].path = path.clone();
        }
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        ctx.begin_pass(egui::RawInput::default());
        let measure = |text: &str, size, family| {
            ctx.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        text.to_owned(),
                        egui::FontId::new(size, family),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
            })
        };
        let mut tree = UiTree::new(crate::session_surface::build_session_surface(
            &app.view, width, false, false,
        ));
        tree.set_reduced_motion(true);
        let native_fonts = ctx.clone();
        tree.set_text_measurer(move |text, size, family| {
            native_fonts.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        text.to_owned(),
                        egui::FontId::new(
                            size,
                            match family {
                                openide_ui::FontFamily::Sans => egui::FontFamily::Proportional,
                                openide_ui::FontFamily::Monospace => egui::FontFamily::Monospace,
                            },
                        ),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
            })
        });
        let now = Instant::now();
        let frame = tree.layout_at(Size::new(width, 720.0), now);
        for id in ["edit-first", "edit-second"] {
            let open_id = crate::file_activity_ui::open_action(OWNER, id, 0);
            let row = frame
                .regions
                .iter()
                .find(|region| region.id == open_id && region.kind == openide_ui::HitKind::Button)
                .unwrap();
            let (text, position, font_size) = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text {
                        text,
                        position,
                        font_size,
                        ..
                    } if text == "main.rs"
                        && position.y >= row.rect.y
                        && position.y < row.rect.y + row.rect.height =>
                    {
                        Some((text, *position, *font_size))
                    }
                    _ => None,
                })
                .unwrap();
            let painted_width = measure(text, font_size, egui::FontFamily::Proportional);
            assert!(
                position.x + painted_width
                    <= row.rect.x + row.rect.width - (position.x - row.rect.x) + 0.1,
                "native caption escapes its row: {text:?}; x={} width={painted_width} row={:?}",
                position.x,
                row.rect
            );
            if width == 360.0 && !long_path && id == "edit-first" {
                assert_eq!(text, "main.rs");
            }
        }
        if width == 360.0 && long_path {
            let open_id = crate::file_activity_ui::open_action(OWNER, "edit-first", 0);
            let row = frame
                .regions
                .iter()
                .find(|region| region.id == open_id && region.kind == openide_ui::HitKind::Button)
                .unwrap();
            let point = openide_ui::Point::new(
                row.rect.x + row.rect.width / 2.0,
                row.rect.y + row.rect.height / 2.0,
            );
            assert_eq!(frame.hit_test(point), Some(open_id.as_str()));
            tree.handle_event_at(openide_ui::UiEvent::PointerMove(point), now);
            let hovered = tree.layout_at(Size::new(width, 720.0), now + Duration::from_millis(400));
            let full_caption = path.clone();
            let visible_text: String = hovered
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            assert!(!visible_text.contains("Abrir en el editor"));
            assert!(
                visible_text
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect::<String>()
                    .contains(
                        &full_caption
                            .chars()
                            .filter(|ch| !ch.is_whitespace())
                            .collect::<String>()
                    ),
                "tooltip must retain the full caption: {visible_text}"
            );
        }
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
#[ignore = "export confirmed inline chat diffs at compact and wide sizes"]
fn export_inline_chat_editor_diff_visual_review() {
    let directory = PathBuf::from("/tmp/openide-inline-diff-visual-qa");
    std::fs::create_dir_all(&directory).unwrap();
    let (_temp, mut app) = fixture();
    app.view.selected_session_title = Some("Ediciones de esta conversación".into());
    app.view.conversation_details.sync(Some(OWNER));
    toggle(&mut app, OWNER, "edit-first");
    toggle(&mut app, OWNER, "edit-second");
    wait(&mut app);
    for width in [360.0, 900.0] {
        for opened in [false, true] {
            if opened {
                app.view
                    .conversation_details
                    .toggle("work:edit-first", false);
            }
            let ctx = egui::Context::default();
            crate::fonts::install(&ctx);
            ctx.begin_pass(egui::RawInput::default());
            let mut tree = UiTree::new(crate::session_surface::build_session_surface(
                &app.view, width, false, false,
            ));
            tree.set_reduced_motion(true);
            let measure = ctx.clone();
            tree.set_text_measurer(move |text, size, family| {
                measure.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(
                            text.to_owned(),
                            egui::FontId::new(
                                size,
                                match family {
                                    openide_ui::FontFamily::Sans => egui::FontFamily::Proportional,
                                    openide_ui::FontFamily::Monospace => {
                                        egui::FontFamily::Monospace
                                    }
                                },
                            ),
                            egui::Color32::WHITE,
                        )
                        .size()
                        .x
                })
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            let frame = loop {
                let frame = tree.layout(Size::new(width, 720.0));
                if !tree.is_animating() {
                    break frame;
                }
                assert!(Instant::now() < deadline, "syntax did not settle");
                std::thread::sleep(Duration::from_millis(1));
            };
            let path = directory.join(format!(
                "chat-diff-{}-{width:.0}.svg",
                if opened { "open" } else { "closed" }
            ));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 720.0)
                .unwrap();
            let options = crate::settings_ui::visual_qa::native_svg_options();
            let parsed =
                resvg::usvg::Tree::from_str(&std::fs::read_to_string(&path).unwrap(), &options)
                    .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 720).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            ctx.end_pass().textures_delta.clear();
            if opened {
                app.view
                    .conversation_details
                    .toggle("work:edit-first", false);
            }
        }
    }
}
