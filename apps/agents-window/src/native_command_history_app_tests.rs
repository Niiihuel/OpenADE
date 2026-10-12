use super::native_command_history_app::HistoryResponse;
use super::*;
use crate::native_command_history_ui::{self, PAGE_BYTES};
use openide_core::{
    NativeCommandOutputBinding, NativeCommandOutputExit, NativeCommandOutputPage,
    NativeCommandOutputStore, NativeCommandOutputStream, NativeCommandOutputTerminal,
};
use openide_ui::{HitKind, Key, Point};

fn expected_stdout() -> Vec<u8> {
    let mut bytes = vec![b'A'; 32767];
    bytes.extend_from_slice("ñ🦀\n".as_bytes());
    bytes.extend_from_slice("Salida guardada íntegra ñ\n".repeat(1200).as_bytes());
    bytes.extend_from_slice(b"FIN-OUTPUT-HISTORY\n");
    bytes
}

#[tokio::test]
async fn empty_history_open_refresh_and_close_leave_the_conversation_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let store = app.store.clone().unwrap();
    let session = store.create(&workspace, "native").unwrap();
    let before = store.load(&session.id).unwrap().unwrap();
    app.view.workspace = workspace.to_string_lossy().into_owned();
    app.view.selected_session = Some(session.id.clone());
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.show_side_pane = true;
    app.view.side_tab = SideTab::Activity;
    let ctx = egui::Context::default();
    app.open_native_command_history(None);
    poll(&mut app).await;
    assert!(!app.view.native_command_history.loading);
    assert!(app.view.native_command_history.page.is_none());
    assert!(app.view.native_command_history.error.is_none());
    assert!(app.view.native_command_history.summaries.is_empty());
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_refresh".into(),
        },
        &ctx,
    );
    poll(&mut app).await;
    let mut tree = UiTree::new(native_command_history_ui::modal(
        &app.view,
        Element::spacer(),
        640.0,
    ));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(640.0, 480.0));
    for action in tree.handle_event(UiEvent::Key {
        key: Key::Escape,
        shift: false,
        ctrl: false,
    }) {
        app.apply_action(action, &ctx);
    }
    assert!(app.view.native_command_history.scope.is_none());
    assert!(app.native_command_history_response.is_none());
    assert_eq!(store.load(&session.id).unwrap().unwrap(), before);
    assert!(app.native_command_sessions.manager(&session.id).is_none());
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn history_caches_the_workspace_alias_verified_when_opened() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let alias = root.path().join("project-link");
    std::fs::create_dir(&workspace).unwrap();
    std::os::unix::fs::symlink(&workspace, &alias).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let session = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap();
    app.view.workspace = alias.to_string_lossy().into_owned();
    app.view.selected_session = Some(session.id);
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.show_side_pane = true;
    app.view.side_tab = SideTab::Activity;
    app.open_native_command_history(None);
    poll(&mut app).await;
    assert_eq!(
        app.view
            .native_command_history
            .scope
            .as_ref()
            .unwrap()
            .workspace,
        workspace.canonicalize().unwrap()
    );
    assert_eq!(
        app.view.native_command_history.workspace_alias.as_deref(),
        Some(alias.as_path())
    );
    assert!(
        !app.poll_native_command_history(),
        "an idle alias does not close or reload its inspector"
    );
    app.view.workspace = root.path().to_string_lossy().into_owned();
    assert!(app.poll_native_command_history());
    assert!(app.view.native_command_history.scope.is_none());
}

fn probe(root: &Path) -> PathBuf {
    let source = root.join("history_probe.rs");
    let binary = root.join(if cfg!(windows) {
        "history_probe.exe"
    } else {
        "history_probe"
    });
    std::fs::write(
        &source,
        r#"
use std::io::Write;
fn main() {
    let mut bytes = vec![b'A'; 32767];
    bytes.extend_from_slice("ñ🦀\n".as_bytes());
    bytes.extend_from_slice("Salida guardada íntegra ñ\n".repeat(1200).as_bytes());
    bytes.extend_from_slice(b"FIN-OUTPUT-HISTORY\n");
    std::io::stdout().write_all(&bytes).unwrap();
    std::io::stderr().write_all(&[255, 0, 27, 65]).unwrap();
}
"#,
    )
    .unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let built = std::process::Command::new(compiler)
        .arg("--edition=2021")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    binary
}

struct CaptureFixture {
    _root: tempfile::TempDir,
    app: AgentsWindow,
    data: PathBuf,
    session_id: String,
    binding: NativeCommandOutputBinding,
    manager: Arc<PersistentCommandManager>,
}

async fn capture_fixture() -> CaptureFixture {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let data = root.path().join("data");
    let binary = probe(root.path());
    let mut app = AgentsWindow::with_data_root(data.clone());
    let session = app
        .store
        .as_ref()
        .unwrap()
        .create(&workspace, "native")
        .unwrap();
    app.view.workspace = workspace.display().to_string();
    app.view.selected_session = Some(session.id.clone());
    app.view.selected_backend = "native".into();
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.show_side_pane = true;
    app.view.side_tab = SideTab::Activity;
    let manager = app
        .native_command_sessions
        .for_session(
            &session.id,
            &workspace,
            CommandIsolation::HostUnrestricted,
            true,
        )
        .unwrap()
        .unwrap();
    let command = manager
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap();
    let binding = NativeCommandOutputBinding {
        session_id: session.id.clone(),
        workspace: workspace.canonicalize().unwrap(),
        manager_generation: manager.generation().into(),
        command_id: command.id,
    };
    let output = NativeCommandOutputStore::new(app.store.as_ref().unwrap().clone());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if output
            .list(&session.id, &binding.workspace)
            .unwrap()
            .last()
            .is_some_and(|summary| summary.terminal.is_some())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real process capture did not close"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    CaptureFixture {
        _root: root,
        app,
        data,
        session_id: session.id,
        binding,
        manager,
    }
}

async fn poll(app: &mut AgentsWindow) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.view.native_command_history.loading {
        app.poll_native_command_history();
        assert!(Instant::now() < deadline, "history reader did not answer");
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    assert!(
        app.view.native_command_history.error.is_none(),
        "{:?}",
        app.view.native_command_history.error
    );
}

fn click(tree: &mut UiTree, frame: &UiFrame, id: &str) -> Vec<UiAction> {
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing button {id}"))
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    assert!(rect.x >= 0.0 && rect.y >= 0.0);
    tree.handle_event(UiEvent::PointerDown(point));
    let actions = tree.handle_event(UiEvent::PointerUp(point));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, UiAction::Clicked { id: clicked } if clicked == id)),
        "{id}: {actions:?}"
    );
    actions
}

fn capture_bytes(page: &NativeCommandOutputPage, stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) {
    let encoded: serde_json::Value =
        serde_json::from_str(&native_command_history_ui::page_json(page)).unwrap();
    for (chunk, rendered) in page
        .chunks
        .iter()
        .zip(encoded["chunks"].as_array().unwrap())
    {
        use base64::Engine;
        let decoded = match rendered["encoding"].as_str().unwrap() {
            "utf8" => rendered["text"].as_str().unwrap().as_bytes().to_vec(),
            "base64" => base64::engine::general_purpose::STANDARD
                .decode(rendered["data"].as_str().unwrap())
                .unwrap(),
            other => panic!("invented encoding {other}"),
        };
        assert_eq!(decoded, chunk.bytes);
        assert_eq!(rendered["start"], chunk.start);
        assert_eq!(rendered["end"], chunk.end);
        match chunk.stream {
            NativeCommandOutputStream::Stdout => stdout.extend_from_slice(&decoded),
            NativeCommandOutputStream::Stderr => stderr.extend_from_slice(&decoded),
            NativeCommandOutputStream::Terminal => panic!("pipe became PTY"),
        }
    }
}

#[tokio::test]
async fn durable_output_pages_survive_consumption_and_reopen_without_manager_with_native_pointer_and_keyboard_actions(
) {
    let mut fixture = capture_fixture().await;
    let record_before = fixture
        .app
        .store
        .as_ref()
        .unwrap()
        .load(&fixture.session_id)
        .unwrap()
        .unwrap();
    let policy_before = fixture.app.native_permission_policy().unwrap().digest_hex();
    let inspected = fixture
        .manager
        .inspect_pending_output(&fixture.binding.command_id)
        .unwrap();
    fixture.app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_open".into(),
        },
        &egui::Context::default(),
    );
    poll(&mut fixture.app).await;
    assert_eq!(
        fixture
            .manager
            .inspect_pending_output(&fixture.binding.command_id)
            .unwrap(),
        inspected,
        "history consumed pending output"
    );
    let first = fixture
        .app
        .view
        .native_command_history
        .page
        .clone()
        .unwrap();
    assert_eq!(
        first.summary.observed_end,
        (expected_stdout().len() + 4) as u64
    );
    assert!(
        first.summary.observed_end > PAGE_BYTES as u64,
        "UI page size cannot stand in for the total"
    );
    assert!(first.summary.terminal.as_ref().unwrap().exit_observed);
    assert!(
        first
            .chunks
            .iter()
            .map(|chunk| chunk.bytes.len())
            .sum::<usize>()
            <= PAGE_BYTES
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let consumed = fixture
            .manager
            .poll(
                &fixture.binding.command_id,
                Duration::from_millis(10),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        if consumed.finished {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "live output never delivered its final receipt"
        );
    }
    assert!(fixture
        .manager
        .inspect_pending_output(&fixture.binding.command_id)
        .is_err());
    fixture.app.native_command_sessions.revoke_all();
    assert!(fixture
        .app
        .native_command_sessions
        .manager(&fixture.session_id)
        .is_none());
    drop(fixture.app);
    let mut app = AgentsWindow::with_data_root(fixture.data.clone());
    app.open_session(&fixture.session_id);
    app.view.show_side_pane = true;
    app.view.side_tab = SideTab::Activity;
    assert!(app
        .native_command_sessions
        .manager(&fixture.session_id)
        .is_none());
    assert!(app.view.native_commands.is_empty());
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_open".into(),
        },
        &egui::Context::default(),
    );
    poll(&mut app).await;
    assert_eq!(
        app.view.native_command_history.page.as_ref().unwrap(),
        &first
    );
    let ctx = egui::Context::default();
    for width in [360.0, 900.0] {
        app.view.native_command_history.cursor = 0;
        app.view.native_command_history.previous_cursors.clear();
        app.apply_action(
            UiAction::Clicked {
                id: "native_command_history_refresh".into(),
            },
            &ctx,
        );
        poll(&mut app).await;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        loop {
            capture_bytes(
                app.view.native_command_history.page.as_ref().unwrap(),
                &mut stdout,
                &mut stderr,
            );
            let mut tree = UiTree::new(native_command_history_ui::modal(
                &app.view,
                Element::button("behind", "Detrás"),
                width,
            ));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 720.0));
            assert_eq!(
                tree.focused_id(),
                Some("native_command_history_dismiss:close")
            );
            let page = app.view.native_command_history.page.as_ref().unwrap();
            assert!(
                !frame
                    .text_layouts()
                    .iter()
                    .any(|text| text.source == app.view.native_command_history.page_json),
                "JSON is not the default product presentation"
            );
            let mut range_seen = vec![false; page.chunks.len()];
            let mut data_seen = vec![false; page.chunks.len()];
            let body = frame
                .regions
                .iter()
                .find(|region| {
                    region.id == "modal_body:native_command_history_dismiss"
                        && region.kind == HitKind::Scroll
                })
                .unwrap()
                .rect;
            for _ in 0..70 {
                let visible_frame = tree.layout(Size::new(width, 720.0));
                for (index, chunk) in page.chunks.iter().enumerate() {
                    range_seen[index] |= visible_frame.text_layouts().iter().any(|text| {
                        text.source
                            .contains(&format!("bytes {}–{}", chunk.start, chunk.end))
                    });
                    let expected = match std::str::from_utf8(&chunk.bytes) {
                        Ok(value) => value.to_owned(),
                        Err(_) => {
                            use base64::Engine;
                            base64::engine::general_purpose::STANDARD.encode(&chunk.bytes)
                        }
                    };
                    data_seen[index] |= visible_frame
                        .text_layouts()
                        .iter()
                        .any(|text| text.source == expected);
                }
                if range_seen.iter().all(|seen| *seen) && data_seen.iter().all(|seen| *seen) {
                    break;
                }
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(body.x + body.width / 2.0, body.y + body.height / 2.0),
                    delta_y: -180.0,
                });
            }
            assert!(
                range_seen.iter().all(|seen| *seen) && data_seen.iter().all(|seen| *seen),
                "page output unreachable at {width}: ranges={range_seen:?}, data={data_seen:?}"
            );
            tree.handle_event(UiEvent::Scroll {
                position: Point::new(body.x + body.width / 2.0, body.y + body.height / 2.0),
                delta_y: 100_000.0,
            });
            let frame = tree.layout(Size::new(width, 720.0));
            if app.view.native_command_history.cursor == 0 {
                for action in click(&mut tree, &frame, "native_command_history_exact_toggle") {
                    app.apply_action(action, &ctx);
                }
                tree.set_root(native_command_history_ui::modal(
                    &app.view,
                    Element::button("behind", "Detrás"),
                    width,
                ));
                let exact_frame = tree.layout(Size::new(width, 720.0));
                assert!(exact_frame
                    .text_layouts()
                    .iter()
                    .any(|text| text.source == app.view.native_command_history.page_json));
                assert!(tree.focus("native_command_history_exact_toggle"));
                for action in tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false,
                }) {
                    app.apply_action(action, &ctx);
                }
                assert!(!app.view.native_command_history.exact_data_open);
                tree.set_root(native_command_history_ui::modal(
                    &app.view,
                    Element::button("behind", "Detrás"),
                    width,
                ));
                tree.layout(Size::new(width, 720.0));
            }
            if !app.view.native_command_history.can_next() {
                break;
            }
            for id in [
                "native_command_history_next",
                "native_command_history_refresh",
                "native_command_history_dismiss:close",
            ] {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect;
                assert!(
                    rect.x + rect.width <= width + 0.1 && rect.y + rect.height <= 720.0,
                    "clipped {id} at {width}"
                );
            }
            let actions = if width == 360.0 {
                click(&mut tree, &frame, "native_command_history_next")
            } else {
                assert!(tree.focus("native_command_history_next"));
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false,
                })
            };
            for action in actions {
                app.apply_action(action, &ctx);
            }
            poll(&mut app).await;
        }
        assert_eq!(stdout, expected_stdout());
        assert_eq!(stderr, [255, 0, 27, 65]);
        assert!(app.view.native_command_history.previous_cursors.len() > 1);
        while !app.view.native_command_history.previous_cursors.is_empty() {
            app.apply_action(
                UiAction::Clicked {
                    id: "native_command_history_previous".into(),
                },
                &ctx,
            );
            poll(&mut app).await;
        }
        assert_eq!(
            app.view.native_command_history.page.as_ref().unwrap(),
            &first
        );
    }
    assert_eq!(
        app.store
            .as_ref()
            .unwrap()
            .load(&fixture.session_id)
            .unwrap()
            .unwrap(),
        record_before
    );
    assert_eq!(
        app.native_permission_policy().unwrap().digest_hex(),
        policy_before
    );
    assert!(app.active_run.is_none() && app.active_resume.is_none());
    assert!(app
        .native_command_sessions
        .manager(&fixture.session_id)
        .is_none());
}

#[tokio::test]
async fn durable_output_host_rejects_stale_scope_request_binding_cursor_revision_and_foreign_store()
{
    let mut fixture = capture_fixture().await;
    let app = &mut fixture.app;
    let ctx = egui::Context::default();
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_open".into(),
        },
        &ctx,
    );
    poll(app).await;
    let scope = app.view.native_command_history.scope.clone().unwrap();
    let known = app.view.native_command_history.page.clone().unwrap();
    let store = app.store.as_ref().unwrap().clone();
    for variant in 1..6 {
        app.apply_action(
            UiAction::Clicked {
                id: "native_command_history_refresh".into(),
            },
            &ctx,
        );
        let mut response = HistoryResponse {
            scope: scope.clone(),
            request_id: app.view.native_command_history.request_id.clone().unwrap(),
            store: store.clone(),
            requested_binding: Some(fixture.binding.clone()),
            cursor: 0,
            result: Ok((
                vec![known.summary.clone()],
                Some(known.clone()),
                native_command_history_ui::page_json(&known),
            )),
        };
        match variant {
            1 => response.scope.session_id = Uuid::new_v4().to_string(),
            2 => response.scope.workspace = fixture._root.path().canonicalize().unwrap(),
            3 => {
                response
                    .requested_binding
                    .as_mut()
                    .unwrap()
                    .manager_generation = Uuid::new_v4().to_string()
            }
            4 => response.cursor = 1,
            5 => {
                response.store = Arc::new(
                    SessionStore::open(fixture._root.path().join("foreign-store")).unwrap(),
                )
            }
            _ => unreachable!(),
        }
        let (sender, receiver) = mpsc::channel();
        sender.send(response).unwrap();
        app.native_command_history_response = Some(receiver);
        app.poll_native_command_history();
        assert!(app.view.native_command_history.page.is_none());
        assert!(app.view.native_command_history.page_json.is_empty());
        assert!(app.view.native_command_history.error.is_some());
    }
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_refresh".into(),
        },
        &ctx,
    );
    let mut old_page = known.clone();
    old_page.summary.revision -= 1;
    let (sender, receiver) = mpsc::channel();
    sender
        .send(HistoryResponse {
            scope,
            request_id: app.view.native_command_history.request_id.clone().unwrap(),
            store,
            requested_binding: Some(fixture.binding.clone()),
            cursor: 0,
            result: Ok((
                vec![old_page.summary.clone()],
                Some(old_page.clone()),
                native_command_history_ui::page_json(&old_page),
            )),
        })
        .unwrap();
    app.native_command_history_response = Some(receiver);
    app.poll_native_command_history();
    assert!(app.view.native_command_history.page.is_none());
    assert!(app
        .view
        .native_command_history
        .error
        .as_deref()
        .unwrap()
        .contains("revisión"));
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_refresh".into(),
        },
        &ctx,
    );
    poll(app).await;
    let accepted = app.view.native_command_history.page.clone();
    let accepted_json = app.view.native_command_history.page_json.clone();
    let (sender, receiver) = mpsc::channel();
    sender
        .send(HistoryResponse {
            scope: app.view.native_command_history.scope.clone().unwrap(),
            request_id: "older-request".into(),
            store: app.store.as_ref().unwrap().clone(),
            requested_binding: Some(fixture.binding.clone()),
            cursor: 0,
            result: Ok((
                vec![old_page.summary.clone()],
                Some(old_page.clone()),
                native_command_history_ui::page_json(&old_page),
            )),
        })
        .unwrap();
    app.native_command_history_response = Some(receiver);
    assert!(!app.poll_native_command_history());
    assert_eq!(app.view.native_command_history.page, accepted);
    assert_eq!(app.view.native_command_history.page_json, accepted_json);
    assert!(!app.view.native_command_history.loading);
    assert!(app.view.native_command_history.error.is_none());
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_refresh".into(),
        },
        &ctx,
    );
    app.view.selected_session = Some("different-owner".into());
    app.poll_native_command_history();
    assert!(app.view.native_command_history.scope.is_none());
    assert!(app.native_command_history_response.is_none());
    assert_eq!(
        app.store
            .as_ref()
            .unwrap()
            .load(&fixture.session_id)
            .unwrap()
            .unwrap()
            .events
            .len(),
        0
    );
}

#[tokio::test]
async fn durable_output_reopened_history_requests_repaint_and_applies_response_through_real_egui_frame(
) {
    let fixture = capture_fixture().await;
    let binding = fixture.binding.clone();
    let session_id = fixture.session_id.clone();
    let data = fixture.data.clone();
    drop(fixture.app);
    let mut app = AgentsWindow::with_data_root(data);
    app.open_session(&session_id);
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.show_side_pane = true;
    app.view.side_tab = SideTab::Activity;
    assert!(app.native_command_sessions.manager(&session_id).is_none());
    assert!(app.active_run.is_none() && app.active_resume.is_none());
    // Quieten unrelated startup workers so their repaint demand cannot hide a
    // missing history-worker demand. The real App::ui path remains unchanged.
    app.marketplace_response = None;
    app.catalog_response = None;
    app.model_discovery_response = None;
    app.codex_model_response = None;
    app.workspace_folder_response = None;
    // Account/workspace discovery also has an 80 ms polling demand. Keep
    // this idle-history test independent of its startup completion timing.
    app.github_account_worker = None;
    app.review_in_flight = false;
    app.github_in_flight = false;
    app.subagent_poll_needed = false;
    app.native_command_poll_owners.clear();
    app.tree.set_reduced_motion(true);
    app.command_tree.set_reduced_motion(true);
    app.settings_preferences.reduced_motion = true;
    let ctx = egui::Context::default();
    let mut native_frame = eframe::Frame::_new_kittest();
    let mut time = 0.0;
    let mut repaint = |app: &mut AgentsWindow| {
        time += 1.0;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 720.0),
                )),
                time: Some(time),
                ..Default::default()
            },
            |ui| eframe::App::ui(app, ui, &mut native_frame),
        );
        let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        output.drop_without_applying_deltas();
        delay
    };
    let mut idle_delay = Duration::ZERO;
    for _ in 0..4 {
        idle_delay = repaint(&mut app);
    }
    assert!(
        idle_delay > Duration::from_millis(150),
        "another worker hid the missing repaint demand: {idle_delay:?}"
    );
    app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_open".into(),
        },
        &ctx,
    );
    let scope = app.view.native_command_history.scope.clone().unwrap();
    let request_id = app.view.native_command_history.request_id.clone().unwrap();
    // Hold the completion barrier: the worker's real source data is read from
    // the reopened store, but the UI receives no response until this sender acts.
    let (sender, receiver) = mpsc::channel();
    app.native_command_history_response = Some(receiver);
    for _ in 0..4 {
        assert!(repaint(&mut app) <= Duration::from_millis(150));
        assert!(app.view.native_command_history.loading);
        assert!(app.view.native_command_history.page.is_none());
    }
    let store = app.store.as_ref().unwrap().clone();
    let output = NativeCommandOutputStore::new(store.clone());
    let summaries = output.list(&session_id, &binding.workspace).unwrap();
    let page = output
        .read(
            &binding,
            0,
            native_command_history_ui::PAGE_BYTES,
            native_command_history_ui::PAGE_RECORDS,
        )
        .unwrap();
    let expected_page = page.clone();
    sender
        .send(HistoryResponse {
            scope,
            request_id,
            store,
            requested_binding: None,
            cursor: 0,
            result: Ok((
                summaries,
                Some(page.clone()),
                native_command_history_ui::page_json(&page),
            )),
        })
        .unwrap();
    repaint(&mut app);
    assert!(!app.view.native_command_history.loading);
    assert!(app.view.native_command_history.error.is_none());
    assert_eq!(
        app.view.native_command_history.page.as_ref(),
        Some(&expected_page)
    );
    assert!(app.native_command_history_response.is_none());
    assert!(app.native_command_sessions.manager(&session_id).is_none());
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[tokio::test]
async fn durable_output_history_reads_capture_without_repairing_torn_model_journal() {
    let mut fixture = capture_fixture().await;
    let root = fixture
        .app
        .store
        .as_ref()
        .unwrap()
        .storage_root()
        .to_path_buf();
    let journal = root.join(format!("{}.jsonl", fixture.session_id));
    let torn = b"{\"session_id\":\"unpublished-torn-record\",\"sequence\":";
    std::fs::write(&journal, torn).unwrap();
    fixture.app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_open".into(),
        },
        &egui::Context::default(),
    );
    poll(&mut fixture.app).await;
    assert_eq!(std::fs::read(&journal).unwrap(), torn);
    let page = fixture
        .app
        .view
        .native_command_history
        .page
        .as_ref()
        .unwrap();
    assert_eq!(page.summary.header.binding, fixture.binding);
    assert_eq!(
        page.summary.observed_end,
        (expected_stdout().len() + 4) as u64
    );
    assert!(page.summary.terminal.is_some());
    assert!(fixture.app.active_run.is_none() && fixture.app.active_resume.is_none());
    let store = fixture.app.store.as_ref().unwrap();
    drop(store.try_acquire_native_lease(&fixture.session_id).unwrap());
    assert_eq!(std::fs::read(&journal).unwrap(), torn);
}

#[tokio::test]
async fn durable_output_unfinished_descriptor_is_incomplete_and_wait_failure_full_identity_scrolls_at_narrow_and_wide(
) {
    let mut fixture = capture_fixture().await;
    let output = NativeCommandOutputStore::new(fixture.app.store.as_ref().unwrap().clone());
    let binding = NativeCommandOutputBinding {
        command_id: Uuid::new_v4().to_string(),
        ..fixture.binding.clone()
    };
    let argument = format!(
        "{}FINAL-ARGV-ñ-終わり",
        "Argumento exacto guardado\n".repeat(90)
    );
    output
        .register(
            &binding,
            &["descriptor-no-ejecutado".into(), argument.clone()],
            "host_unrestricted",
            false,
        )
        .unwrap();
    let process_count = fixture.manager.list_sessions().unwrap().len();
    let record_before = fixture
        .app
        .store
        .as_ref()
        .unwrap()
        .load(&fixture.session_id)
        .unwrap()
        .unwrap();
    let ctx = egui::Context::default();
    let open = format!(
        "native_command_history_open:{}",
        serde_json::to_string(&(
            &binding.session_id,
            &binding.manager_generation,
            &binding.command_id
        ))
        .unwrap()
    );
    fixture
        .app
        .apply_action(UiAction::Clicked { id: open }, &ctx);
    poll(&mut fixture.app).await;
    for width in [360.0, 900.0] {
        let frame = UiTree::new(native_command_history_ui::modal(
            &fixture.app.view,
            Element::spacer(),
            width,
        ))
        .layout(Size::new(width, 720.0));
        assert!(frame.text_layouts().iter().any(|text| text
            .source
            .contains("Captura incompleta: no hay cierre guardado")));
        assert!(!frame
            .text_layouts()
            .iter()
            .any(|text| text.source.contains("En ejecución")));
    }
    // A real validated durable failure receipt supplies the presentation. The
    // descriptor has never been dispatched and the inspector never dispatches it.
    let error = format!(
        "{}FINAL-WAIT-FAILURE-ñ-終わり",
        "Diagnóstico íntegro de espera\n".repeat(70)
    );
    output
        .finish(
            &binding,
            0,
            &NativeCommandOutputTerminal {
                exit_observed: true,
                exit_kind: Some(NativeCommandOutputExit::WaitFailed {
                    error: error.clone(),
                }),
                exit_code: None,
                reader_error: None,
            },
        )
        .unwrap();
    fixture.app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_refresh".into(),
        },
        &ctx,
    );
    poll(&mut fixture.app).await;
    for width in [360.0, 900.0] {
        let mut tree = UiTree::new(native_command_history_ui::modal(
            &fixture.app.view,
            Element::button("behind", "Detrás"),
            width,
        ));
        tree.set_reduced_motion(true);
        let viewport = Size::new(width, 720.0);
        let frame = tree.layout(viewport);
        for action in click(&mut tree, &frame, "native_command_history_details_toggle") {
            fixture.app.apply_action(action, &ctx);
        }
        tree.set_root(native_command_history_ui::modal(
            &fixture.app.view,
            Element::button("behind", "Detrás"),
            width,
        ));
        assert_ne!(tree.focused_id(), Some("behind"));
        let mut full_error = false;
        let mut full_identity = false;
        let mut argv_end = false;
        let mut wait_end = false;
        for _ in 0..70 {
            let frame = tree.layout(viewport);
            for text in frame.text_layouts() {
                full_error |= text.source.contains(&error);
                full_identity |= text.source.contains(&binding.session_id)
                    && text.source.contains(&binding.manager_generation)
                    && text.source.contains(&binding.command_id)
                    && text
                        .source
                        .contains(&serde_json::to_string(&argument).unwrap());
                let visible = text
                    .runs
                    .iter()
                    .filter(|run| run.visible.is_some())
                    .map(|run| run.text.as_str())
                    .collect::<String>();
                argv_end |= text
                    .source
                    .contains(&serde_json::to_string(&argument).unwrap())
                    && visible.contains("FINAL-ARGV");
                wait_end |= text.source.contains(&error) && visible.contains("FINAL-WAIT-FAILURE");
            }
            let body = frame
                .regions
                .iter()
                .find(|region| {
                    region.id == "modal_body:native_command_history_dismiss"
                        && region.kind == HitKind::Scroll
                })
                .unwrap()
                .rect;
            tree.handle_event(UiEvent::Scroll {
                position: Point::new(body.x + body.width / 2.0, body.y + body.height / 2.0),
                delta_y: -180.0,
            });
        }
        assert!(full_error && full_identity && argv_end && wait_end, "complete stored details unreachable at {width}: error={full_error}, ids={full_identity}, argv={argv_end}, wait={wait_end}");
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "native_command_history_dismiss".into()
            }]
        );
        fixture.app.view.native_command_history.details_open = false;
    }
    assert_eq!(
        fixture.manager.list_sessions().unwrap().len(),
        process_count
    );
    assert_eq!(
        fixture
            .app
            .store
            .as_ref()
            .unwrap()
            .load(&fixture.session_id)
            .unwrap()
            .unwrap(),
        record_before
    );
}

#[tokio::test]
#[ignore = "export actual durable command inspector for manual visual review"]
async fn export_native_command_history_visual_review() {
    let mut fixture = capture_fixture().await;
    fixture.app.apply_action(
        UiAction::Clicked {
            id: "native_command_history_open".into(),
        },
        &egui::Context::default(),
    );
    poll(&mut fixture.app).await;
    let output = PathBuf::from("/tmp/openide-command-history-visual-qa");
    std::fs::create_dir_all(&output).unwrap();
    let loaded = fixture.app.view.native_command_history.clone();
    for scenario in [
        "command-history",
        "command-history-empty",
        "command-palette-close-hover",
    ] {
        for width in [360.0_f32, 900.0] {
            let height = 720.0;
            let base = || Element::spacer().background(crate::theme::WINDOW);
            let root = if scenario == "command-palette-close-hover" {
                fixture.app.view.command_results = crate::command_center::search(
                    "",
                    crate::command_center::Scope::All,
                    &[],
                    &[],
                    crate::command_center::DEFAULT_RESULT_LIMIT,
                );
                Element::popover(
                    base(),
                    crate::view::build_command_center(
                        &fixture.app.view,
                        (width - 24.0).min(560.0),
                        420.0,
                    ),
                    "search_preview_dismiss",
                    openide_ui::PopoverPlacement::Center,
                )
                .popover_animation_ms(0)
            } else {
                fixture.app.view.native_command_history = if scenario == "command-history-empty" {
                    native_command_history_ui::NativeCommandHistoryUiState {
                        scope: loaded.scope.clone(),
                        workspace_alias: loaded.workspace_alias.clone(),
                        ..Default::default()
                    }
                } else {
                    loaded.clone()
                };
                native_command_history_ui::modal(&fixture.app.view, base(), width)
            };
            let ctx = egui::Context::default();
            crate::fonts::install(&ctx);
            ctx.begin_pass(egui::RawInput::default());
            let mut tree = UiTree::new(root);
            tree.set_reduced_motion(true);
            let fonts = ctx.clone();
            tree.set_text_measurer(move |text, size, family| {
                fonts.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(
                            text.into(),
                            egui::FontId::new(
                                size,
                                match family {
                                    openide_ui::FontFamily::Sans | openide_ui::FontFamily::SansItalic | openide_ui::FontFamily::SansMedium | openide_ui::FontFamily::SansBold => egui::FontFamily::Proportional,
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
            let now = Instant::now();
            let mut frame = tree.layout_at(Size::new(width, height), now);
            if scenario == "command-palette-close-hover" {
                let close = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "command_close")
                    .unwrap()
                    .rect;
                tree.handle_event_at(
                    UiEvent::PointerMove(Point::new(
                        close.x + close.width / 2.0,
                        close.y + close.height / 2.0,
                    )),
                    now,
                );
                frame = tree.layout_at(Size::new(width, height), now + Duration::from_millis(600));
            }
            let path = output.join(format!("{scenario}-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
                .unwrap();
            let options = crate::settings_ui::visual_qa::native_svg_options();
            let parsed =
                resvg::usvg::Tree::from_str(&std::fs::read_to_string(&path).unwrap(), &options)
                    .unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            eprintln!("{}", path.with_extension("png").display());
            ctx.end_pass().textures_delta.clear();
        }
    }
}
