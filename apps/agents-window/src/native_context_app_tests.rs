use super::*;
use serde_json::json;
use std::io::{Read, Write};

fn exact_model_listing() -> Vec<native_provider::ProviderModelDiscovery> {
    vec![native_provider::ProviderModelDiscovery {
        provider_id: "custom".into(),
        models: BTreeMap::from([(
            "context-exact-model".into(),
            native_provider::DiscoveredModel {
                listing_index: None,
                limits_declared: false,
                efforts_declared: false,
                capabilities_declared: false,
                max_input_tokens: None,
                declared_capabilities: Default::default(),
                id: "context-exact-model".into(),
                name: "Context fixture model".into(),
                provider_reports_tools: true,
                tool_call_declared: Some(true),
                text_input_output: true,
                supported_efforts: vec![],
                context_window_tokens: Some(20_000),
                max_output_tokens: Some(4_000),
            },
        )]),
    }]
}

fn all_context_text(app: &AgentsWindow) -> String {
    app.view
        .native_context
        .sections
        .iter()
        .map(|section| {
            format!(
                "{}\n{}",
                section.title,
                section
                    .rows
                    .iter()
                    .map(|row| format!("{}\n{}\n{}", row.label, row.value, row.detail))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn error_context_tree(app: &AgentsWindow, automation: bool) -> UiTree {
    let control = if automation {
        crate::composer_ui::build_native_context_control(&app.view, 620.0)
    } else {
        let props = crate::composer_ui::ComposerProps::from_state(
            &app.view,
            620.0,
            false,
            None,
            "Continuar",
            None,
        );
        crate::composer_ui::build_composer(&props)
    };
    let mut tree = UiTree::new(Element::column(vec![Element::spacer().flex(1.0), control]));
    tree.set_reduced_motion(true);
    tree
}

fn open_rendered_error_details(app: &mut AgentsWindow, automation: bool) -> String {
    use openide_ui::{DrawCommand, Key};
    let text = |frame: &UiFrame| {
        frame
            .commands
            .iter()
            .filter_map(|command| {
                if let DrawCommand::Text { text, .. } = command {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
            // Wrapped text commands retain boundary whitespace. Preserve
            // every word while comparing the same visible sentence.
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let size = Size::new(620.0, 760.0);
    let ctx = egui::Context::default();
    let mut tree = error_context_tree(app, automation);
    tree.layout(size);
    assert!(tree.focus("composer_context_toggle"));
    for action in tree.handle_event(UiEvent::Key {
        key: Key::Enter,
        shift: false,
        ctrl: false,
    }) {
        app.apply_action(action, &ctx);
    }
    let frame = error_context_tree(app, automation).layout(size);
    let details = text(&frame);
    assert!(!details.contains("Aún no hay una respuesta medida"));
    assert!(frame.commands.iter().all(|command| !matches!(
        command,
        DrawCommand::ProgressRing {
            fraction: Some(_),
            ..
        }
    )));
    assert!(details.contains("Consulta no disponible"), "{details}");
    assert!(app.view.native_context.last_usage.is_none());
    assert!(app.view.native_context.conversation_usage.is_none());
    assert!(app.view.native_context.used_fraction().is_none());
    details
}

#[test]
fn native_context_missing_workspace_error_remains_visible_without_measurements() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    NativeProviderSettings {
        provider_id: "custom".into(),
        model_id: "error-scope-model".into(),
        custom_endpoint: "http://127.0.0.1:9/v1".into(),
    }
    .save(&data)
    .unwrap();
    let mut app = AgentsWindow::with_data_root(data);
    app.view.workspace = root.path().join("missing-project").display().to_string();
    app.view.selected_backend = "native".into();
    app.view.selected_session = None;
    app.view.native_provider_id = "custom".into();
    app.view.model = "error-scope-model".into();
    app.view.composer_model = app.view.model.clone();
    app.refresh_native_context();
    let details = open_rendered_error_details(&mut app, false);
    assert!(
        details.contains("No se pudo verificar el proyecto seleccionado"),
        "{details}"
    );
    assert!(app.view.native_context.workspace.is_none());
    assert!(app.view.native_context.capacity().is_none());
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        app.apply_action(
            UiAction::Clicked {
                id: "composer_context_copy_diagnostic".into(),
            },
            ui.ctx(),
        );
    });
    output.textures_delta.clear();
    assert!(output.platform_output.commands.iter().any(|command| matches!(command,
        egui::OutputCommand::CopyText(text)
            if text.starts_with("Diagnóstico del contexto\ncustom · error-scope-model\n")
                && text.contains("Estado: No se pudo verificar el proyecto seleccionado"))));
    // Retaining the failed view while selecting another project must not leak
    // its cause into the next project's summary.
    app.view.workspace = root.path().display().to_string();
    let props = crate::composer_ui::ComposerProps::from_state(
        &app.view,
        620.0,
        false,
        None,
        "Continuar",
        None,
    );
    assert!(props.native_context.is_none());
}

#[test]
fn native_context_automation_load_error_keeps_recorded_route_and_visible_cause() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let settings = NativeProviderSettings {
        provider_id: "custom".into(),
        model_id: "automation-error-model".into(),
        custom_endpoint: "http://127.0.0.1:9/v1".into(),
    };
    settings.save(&data).unwrap();
    let mut app = AgentsWindow::with_data_root(data.clone());
    app.change_workspace(workspace.display().to_string());
    let automation_id = Uuid::new_v4().to_string();
    let run_id = format!("{automation_id}:2000000");
    let frozen = crate::native_automation::FrozenNativeAutomationAuthorityV1::freeze(
        settings.resolve_at(&data).unwrap(),
        "custom",
        &data,
        &workspace,
        None,
        &[],
        None,
        None,
        "Read-only fixture".into(),
        "Inspect the project".into(),
        &run_id,
        false,
    )
    .unwrap();
    let store = app.store.clone().unwrap();
    let session = store
        .create_native_automation(
            &workspace,
            frozen
                .to_origin(&automation_id, &run_id, 2_000_000, false)
                .unwrap(),
        )
        .unwrap();
    app.open_session(&session.id);
    assert!(app
        .view
        .sessions
        .iter()
        .any(|row| row.id == session.id && row.native_automation.is_some()));
    // A retained journal can load while its workspace no longer exists. The
    // projection's validation error must remain available in that case too.
    let retained = root.path().join("retained-project");
    std::fs::rename(&workspace, &retained).unwrap();
    app.refresh_native_context();
    let missing_details = open_rendered_error_details(&mut app, true);
    assert!(missing_details.contains("No se pudo verificar el proyecto de la conversación"));
    std::fs::rename(&retained, &workspace).unwrap();
    app.view.composer_context_open = false;
    let journal = store.storage_root().join(format!("{}.jsonl", session.id));
    std::fs::write(&journal, b"invalid committed journal line\n").unwrap();
    let before = std::fs::read(&journal).unwrap();
    // Changing Settings cannot relabel the failed automation lookup.
    app.view.native_provider_id = "other-provider".into();
    app.view.model = "other-model".into();
    app.view.composer_model = app.view.model.clone();
    app.view.selected_backend = "native".into();
    app.refresh_native_context();
    let error = app
        .view
        .native_context
        .error
        .clone()
        .expect("real journal read fails");
    assert_eq!(app.view.native_context.provider, "custom");
    assert_eq!(app.view.native_context.model, "automation-error-model");
    let details = open_rendered_error_details(&mut app, true);
    let cause_prefix: String = error.chars().take(18).collect();
    assert!(
        details.contains(&cause_prefix),
        "missing {cause_prefix}: {details}"
    );
    assert_eq!(
        std::fs::read(journal).unwrap(),
        before,
        "inspection must not repair or truncate the journal"
    );
}

#[test]
fn native_context_card_compacts_through_the_scoped_guarded_action() {
    const CONTROL: &str = "composer_context:action:conversation";
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let store = app.store.clone().unwrap();
    let session = store.create(root.path(), "native").unwrap();
    app.view.workspace = root.path().display().to_string();
    app.view.selected_backend = "native".into();
    app.view.selected_session = Some(session.id.clone());
    app.view.sessions = vec![crate::view::SessionView {
        id: session.id.clone(),
        title: "Conversación".into(),
        project: app.view.workspace.clone(),
        backend: "native".into(),
        pinned: false,
        archived: false,
        unread: false,
        native_automation: None,
    }];
    app.view.native_context = crate::native_context_ui::NativeContextUiState {
        owner: Some(session.id.clone()),
        workspace: Some(root.path().canonicalize().unwrap()),
        provider: app.view.native_provider_id.clone(),
        model: app.view.model.clone(),
        unreported_responses: 1,
        breakdown: vec![crate::native_context_ui::NativeContextCategory::new(
            "conversation",
            "Conversación",
            "Sin uso",
            "",
        )],
        ..Default::default()
    };
    app.view.composer_context_open = true;
    let size = Size::new(620.0, 760.0);
    let ctx = egui::Context::default();
    let mut tree = error_context_tree(&app, false);
    tree.layout(size);
    assert!(tree.focus(CONTROL));
    let actions = tree.handle_event(UiEvent::Key {
        key: openide_ui::Key::Enter,
        shift: false,
        ctrl: false,
    });
    assert_eq!(
        actions,
        vec![UiAction::Clicked {
            id: format!("composer_context_compact:{}", session.id)
        }]
    );
    // A response in flight withdraws the control, and a click already on its
    // way still meets the host's guard.
    app.view.running = true;
    app.view.status = "unchanged".into();
    let mut running = error_context_tree(&app, false);
    running.layout(size);
    assert!(!running.focus(CONTROL));
    for action in actions.clone() {
        app.apply_action(action, &ctx);
    }
    // A row has nothing to open, so its click changes nothing either.
    app.apply_action(
        UiAction::Clicked {
            id: "composer_context:share:conversation".into(),
        },
        &ctx,
    );
    assert!(app.view.composer_context_open);
    assert_eq!(app.view.status, "unchanged");
    app.view.running = false;
    for action in actions {
        app.apply_action(action, &ctx);
    }
    // The host reaches the real journal guard, not a provider request.
    assert_eq!(app.view.status, "No hay historial nuevo para compactar");
    assert!(app.active_compact.is_none());
}

fn context_category<'a>(
    app: &'a AgentsWindow,
    id: &str,
) -> &'a crate::native_context_ui::NativeContextCategory {
    app.view
        .native_context
        .breakdown
        .iter()
        .find(|category| category.id == id)
        .unwrap()
}

fn export_context_visual(app: &mut AgentsWindow, stage: &str) {
    let Some(directory) = std::env::var_os("OPENIDE_CONTEXT_VISUAL_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let was_open = app.view.composer_context_open;
    app.view.composer_context_open = true;
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    for width in [420.0, 900.0] {
        let export = |frame: &openide_ui::Frame, position: &str| {
            let path = directory.join(format!("context-{stage}-{width:.0}-{position}.svg"));
            crate::settings_ui::visual_qa::write_frame_svg(frame, &path, width, 760.0).unwrap();
            let svg = std::fs::read_to_string(&path).unwrap();
            let svg_tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 760).unwrap();
            resvg::render(
                &svg_tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
        };
        let viewport = openide_ui::Size::new(width, 760.0);
        let props = crate::composer_ui::ComposerProps::from_state(
            &app.view,
            width,
            false,
            None,
            "Continuar con el harness nativo",
            None,
        );
        let mut tree = openide_ui::UiTree::new(crate::composer_ui::build_composer(&props));
        tree.set_reduced_motion(true);
        export(&tree.layout(viewport), "compact");
    }
    app.view.composer_context_open = was_open;
}

#[test]
fn native_context_http_reopens_checkpoint_refills_and_rechecks_exact_route_without_effects() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let project = "Project instructions ñ\nRetain exact context evidence";
    std::fs::write(root.path().join("AGENTS.md"), project).unwrap();
    let skill_dir = root.path().join(".agents/skills/context-test");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let skill = "---\nname: context-test\ndescription: Context projection fixture\n---\nPreserve usage subsets without double counting. 終わり";
    std::fs::write(skill_dir.join("SKILL.md"), skill).unwrap();
    let skill_settings = NativeSkillsSettings::load(&data, root.path()).unwrap();
    let selection = skill_settings.discover(&data).unwrap()[0].selection();
    skill_settings
        .with_selections(vec![selection.clone()])
        .save(&data)
        .unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (requests_tx, requests_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        for index in 0..3 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "missing context fixture HTTP request {index}"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let (header_end, length) = loop {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                assert!(bytes.len() < 1024 * 1024);
                if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while bytes.len() < header_end + length {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            let request: serde_json::Value =
                serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
            let streaming = request["stream"].as_bool() == Some(true);
            assert_eq!(request["model"], "context-exact-model");
            requests_tx.send(request).unwrap();
            let content = if index == 1 {
                json!({"objective":"Continue context acceptance", "decisions":["Preserve journal usage"], "files":[], "current_state":"Plan completed", "constraints":["Do not attribute usage by model-name inference"], "remaining_work":["Refill after checkpoint"]}).to_string()
            } else {
                format!("Native context Plan {index} completed")
            };
            let usage = if index == 0 {
                json!({"prompt_tokens":1000,"completion_tokens":200,"total_tokens":1200,"prompt_tokens_details":{"cached_tokens":300},"completion_tokens_details":{"reasoning_tokens":50}})
            } else if index == 1 {
                json!({"prompt_tokens":777,"completion_tokens":88,"total_tokens":865})
            } else {
                json!({"prompt_tokens":400,"completion_tokens":100,"total_tokens":500})
            };
            let (body, content_type) = if streaming {
                let chunk = json!({"id":format!("context-{index}"),"choices":[{"index":0,"delta":{"content":content},"finish_reason":null}]});
                let terminal = json!({"id":format!("context-{index}"),"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":usage});
                (
                    format!("data: {chunk}\n\ndata: {terminal}\n\ndata: [DONE]\n\n"),
                    "text/event-stream",
                )
            } else {
                (json!({"id":format!("context-{index}"),"choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}],"usage":usage}).to_string(), "application/json")
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
    });
    let mut app = AgentsWindow::with_data_root(data.clone());
    app.view.workspace = root.path().display().to_string();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = "context-exact-model".into();
    app.view.composer_model = app.view.model.clone();
    app.view.composer_effort = "default".into();
    app.view.endpoint = endpoint.clone();
    app.model_discoveries = exact_model_listing();
    assert!(app.persist_native_settings());
    let store = app.store.clone().unwrap();
    let session = store.create(root.path(), "native").unwrap();
    app.open_session(&session.id);
    app.view.turn_mode = TurnMode::Plan;
    app.view.prompt = "Observe actual HTTP usage".into();
    app.start_run();
    assert!(app.active_run.is_some(), "{}", app.view.status);
    let deadline = Instant::now() + Duration::from_secs(15);
    while app.active_run.is_some() {
        assert!(Instant::now() < deadline, "{}", app.view.status);
        app.poll_run();
        std::thread::sleep(Duration::from_millis(5));
    }
    app.refresh_composer_context_percent();
    assert!(
        app.view.native_context.error.is_none(),
        "{:?}",
        app.view.native_context.error
    );
    assert_eq!(
        app.view.native_context.last_usage,
        Some(openide_core::ModelTokenUsage {
            input_tokens: 1000,
            output_tokens: 200,
            total_tokens: 1200,
            cached_input_tokens: Some(300),
            reasoning_tokens: Some(50)
        })
    );
    assert_eq!(app.view.native_context.remaining_percent, Some(94));
    assert_eq!(app.view.native_context.used_fraction(), Some(0.06));
    assert_eq!(
        app.view.native_context.measurement_status,
        crate::native_context_ui::NativeContextMeasurementStatus::Measured
    );
    assert_eq!(app.view.native_context.window, Some(20_000));
    assert_eq!(app.view.native_context.max_output, Some(4_000));
    assert_eq!(app.view.native_context.output_reserve, Some(4_000));
    assert_eq!(
        app.view
            .native_context
            .breakdown
            .iter()
            .map(|category| category.id.as_str())
            .collect::<Vec<_>>(),
        [
            "system",
            "tools",
            "project",
            "skills",
            "mcp",
            "subagents",
            "conversation"
        ]
    );
    assert_eq!(
        context_category(&app, "project").value,
        format!("1 docs · {} bytes", project.len())
    );
    assert!(context_category(&app, "skills")
        .value
        .starts_with("1 locales · "));
    assert!(context_category(&app, "skills")
        .detail
        .contains("Índice de skills instaladas: 0 bytes de metadatos."));
    assert_eq!(
        context_category(&app, "tools").value,
        "Cantidad desconocida"
    );
    assert!(context_category(&app, "tools").detail.contains("SHA-256"));
    assert_eq!(
        context_category(&app, "mcp").value,
        "0 servidores · herramientas no observadas"
    );
    assert_eq!(context_category(&app, "subagents").value, "Sin desglose");
    assert_eq!(
        context_category(&app, "conversation").value,
        "1000 entrada · 200 salida"
    );
    export_context_visual(&mut app, "latest-usage");
    let first = requests_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(first["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains(project));
    assert!(first["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("Preserve usage subsets"));
    let stored = store.load(&session.id).unwrap().unwrap();
    let saved_digest = stored
        .events
        .iter()
        .find_map(|event| {
            if let AgentEventKind::SystemInstructionsCommitted { digest, .. } = &event.kind {
                Some(digest.clone())
            } else {
                None
            }
        })
        .unwrap();
    let saved_bytes = stored
        .events
        .iter()
        .find_map(|event| {
            if let AgentEventKind::SystemInstructionsCommitted { content, .. } = &event.kind {
                Some(content.len())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        context_category(&app, "system").value,
        format!("{saved_bytes} bytes")
    );
    assert!(context_category(&app, "system")
        .detail
        .contains(&saved_digest));
    let journal_path = store.storage_root().join(format!("{}.jsonl", session.id));
    let before = std::fs::read(&journal_path).unwrap();
    std::fs::write(
        root.path().join("AGENTS.md"),
        "ChangedProjectDocumentsForNextTurnOnly",
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        skill.replace("without double counting", "changed without accepting hash"),
    )
    .unwrap();
    app.refresh_native_context();
    let details = all_context_text(&app);
    assert!(details.contains(&saved_digest));
    assert!(details.contains("Activación no disponible"));
    assert!(details.contains("próximo turno"));
    assert_eq!(
        context_category(&app, "skills").value,
        "1 seleccionadas · no activadas"
    );
    assert_eq!(
        context_category(&app, "system").value,
        format!("{saved_bytes} bytes")
    );
    assert_eq!(
        app.view
            .native_context
            .last_usage
            .as_ref()
            .unwrap()
            .total_tokens,
        1200
    );
    assert_eq!(std::fs::read(&journal_path).unwrap(), before);
    std::fs::write(skill_dir.join("SKILL.md"), skill).unwrap();

    app.start_compact();
    assert!(app.active_compact.is_some(), "{}", app.view.status);
    let deadline = Instant::now() + Duration::from_secs(15);
    while app.active_compact.is_some() {
        assert!(Instant::now() < deadline, "{}", app.view.status);
        app.poll_compact();
        std::thread::sleep(Duration::from_millis(5));
    }
    app.refresh_composer_context_percent();
    assert!(
        app.view.native_context.error.is_none(),
        "{:?}",
        app.view.native_context.error
    );
    assert!(app.view.native_context.checkpoint_sequence.is_some());
    assert_eq!(
        app.view.native_context.measurement_status,
        crate::native_context_ui::NativeContextMeasurementStatus::AfterCheckpoint
    );
    assert!(app.view.native_context.used_fraction().is_none());
    assert!(
        app.view.native_context.last_usage.is_none()
            && app.view.native_context.remaining_percent.is_none()
    );
    assert_eq!(
        app.view
            .native_context
            .conversation_usage
            .as_ref()
            .unwrap()
            .total_tokens,
        1200
    );
    export_context_visual(&mut app, "checkpoint-awaiting-refill");
    let compact = requests_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(compact["tools"]
        .as_array()
        .is_none_or(|tools| tools.is_empty()));
    drop(app);

    let mut reopened = AgentsWindow::with_data_root(data.clone());
    reopened.model_discoveries = exact_model_listing();
    reopened.open_session(&session.id);
    reopened.refresh_composer_context_percent();
    assert!(
        reopened.view.native_context.error.is_none(),
        "{:?}",
        reopened.view.native_context.error
    );
    assert!(reopened.view.native_context.last_usage.is_none());
    assert_eq!(
        reopened
            .view
            .native_context
            .conversation_usage
            .as_ref()
            .unwrap()
            .total_tokens,
        1200
    );
    reopened.view.turn_mode = TurnMode::Plan;
    reopened.view.prompt = "Refill actual context after checkpoint".into();
    reopened.start_run();
    assert!(reopened.active_run.is_some(), "{}", reopened.view.status);
    let deadline = Instant::now() + Duration::from_secs(15);
    while reopened.active_run.is_some() {
        assert!(Instant::now() < deadline, "{}", reopened.view.status);
        reopened.poll_run();
        std::thread::sleep(Duration::from_millis(5));
    }
    reopened.refresh_composer_context_percent();
    assert!(
        reopened.view.native_context.error.is_none(),
        "{:?}",
        reopened.view.native_context.error
    );
    assert_eq!(
        reopened
            .view
            .native_context
            .last_usage
            .as_ref()
            .unwrap()
            .total_tokens,
        500
    );
    assert_eq!(reopened.view.native_context.remaining_percent, Some(98));
    assert_eq!(reopened.view.native_context.used_fraction(), Some(0.025));
    assert_eq!(
        reopened.view.native_context.measurement_status,
        crate::native_context_ui::NativeContextMeasurementStatus::Measured
    );
    assert_eq!(reopened.view.native_context.reported_responses, 2);
    assert_eq!(
        context_category(&reopened, "conversation").value,
        "1400 entrada · 300 salida"
    );
    assert_eq!(
        reopened
            .view
            .native_context
            .conversation_usage
            .as_ref()
            .unwrap()
            .total_tokens,
        1700
    );
    assert_eq!(
        reopened
            .view
            .native_context
            .conversation_usage
            .as_ref()
            .unwrap()
            .cached_input_tokens,
        Some(300)
    );
    export_context_visual(&mut reopened, "reopened-refill");
    let refill = requests_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(refill["messages"]
        .to_string()
        .contains("Refill after checkpoint"));
    server.join().unwrap();
    let final_journal = std::fs::read(&journal_path).unwrap();
    let mut changed_limits = exact_model_listing().remove(0);
    let model = changed_limits
        .models
        .get_mut("context-exact-model")
        .unwrap();
    model.context_window_tokens = Some(10_000);
    model.max_output_tokens = Some(2_000);
    for (listing, expected_window, expected_reserve, expected_remaining) in [
        (changed_limits, 10_000, 2_000, 95),
        (exact_model_listing().remove(0), 20_000, 4_000, 98),
    ] {
        let (sender, receiver) = std::sync::mpsc::channel();
        reopened.model_discovery_response = Some(receiver);
        reopened.model_discovery_pending = 1;
        sender.send(("custom".into(), Ok(listing))).unwrap();
        assert!(reopened.poll_model_discovery());
        assert_eq!(reopened.view.native_context.window, Some(expected_window));
        assert_eq!(
            reopened.view.native_context.output_reserve,
            Some(expected_reserve)
        );
        assert_eq!(
            reopened.view.native_context.remaining_percent,
            Some(expected_remaining)
        );
    }
    assert_eq!(std::fs::read(&journal_path).unwrap(), final_journal);
    let ctx = egui::Context::default();
    reopened.apply_action(
        UiAction::InputChanged {
            id: "endpoint".into(),
            value: "invalid endpoint draft".into(),
        },
        &ctx,
    );
    assert!(
        reopened.view.native_context.error.is_some()
            && reopened.view.native_context.last_usage.is_none()
            && reopened.view.native_context.remaining_percent.is_none()
    );
    assert_eq!(
        NativeProviderSettings::load(&data).custom_endpoint,
        endpoint
    );
    reopened.apply_action(
        UiAction::InputChanged {
            id: "endpoint".into(),
            value: "http://127.0.0.1:1/v1".into(),
        },
        &ctx,
    );
    assert!(
        reopened.view.native_context.last_usage.is_none()
            && reopened.view.native_context.conversation_usage.is_none()
    );
    reopened.apply_action(
        UiAction::InputChanged {
            id: "endpoint".into(),
            value: endpoint.clone(),
        },
        &ctx,
    );
    assert_eq!(
        reopened
            .view
            .native_context
            .last_usage
            .as_ref()
            .unwrap()
            .total_tokens,
        500
    );
    native_provider::save_output_limit(
        &data,
        "custom",
        "context-exact-model",
        Some(native_provider::NativeOutputLimit {
            provider_id: "custom".into(),
            model_id: "context-exact-model".into(),
            wire_api: NativeWireApi::ChatCompletions,
            field: "max_tokens".into(),
            tokens: 1000,
        }),
    )
    .unwrap();
    reopened.refresh_native_context();
    assert!(
        reopened.view.native_context.last_usage.is_none()
            && reopened.view.native_context.conversation_usage.is_none()
    );
    assert_eq!(reopened.view.native_context.output_reserve, Some(1000));
    assert_eq!(reopened.view.native_context.max_output, Some(4000));
    native_provider::save_output_limit(&data, "custom", "context-exact-model", None).unwrap();
    reopened.view.native_provider_id = "openai".into();
    reopened.refresh_native_context();
    assert!(reopened.view.native_context.last_usage.is_none());
    reopened.view.native_provider_id = "custom".into();
    reopened.view.model = "other-model".into();
    reopened.refresh_native_context();
    assert!(reopened.view.native_context.last_usage.is_none());
    reopened.view.model = "context-exact-model".into();
    let foreign = tempfile::tempdir().unwrap();
    reopened.view.workspace = foreign.path().display().to_string();
    reopened.refresh_native_context();
    assert!(
        reopened.view.native_context.error.is_some()
            && reopened.view.native_context.last_usage.is_none()
    );
    reopened.view.workspace = root.path().display().to_string();
    reopened.view.selected_backend = "codex".into();
    reopened.refresh_native_context();
    assert!(
        reopened.view.native_context.owner.is_none()
            && reopened.view.native_context.sections.is_empty()
    );
    reopened.view.selected_backend = "native".into();
    reopened.refresh_native_context();
    assert_eq!(
        reopened
            .view
            .native_context
            .last_usage
            .as_ref()
            .unwrap()
            .total_tokens,
        500
    );
    assert_eq!(std::fs::read(&journal_path).unwrap(), final_journal);
    let checkpoint = store
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .iter()
        .find_map(|event| {
            if let AgentEventKind::CheckpointCommitted { reference } = &event.kind {
                Some(reference.clone())
            } else {
                None
            }
        })
        .unwrap();
    std::fs::write(
        store
            .storage_root()
            .join("checkpoints")
            .join(&checkpoint.session_id)
            .join(format!("{}.json", checkpoint.checkpoint_id)),
        b"corrupt checkpoint",
    )
    .unwrap();
    reopened.refresh_native_context();
    assert!(
        reopened.view.native_context.error.is_some()
            && reopened.view.native_context.last_usage.is_none()
    );
    assert_eq!(std::fs::read(journal_path).unwrap(), final_journal);
}

#[test]
fn native_context_hosted_web_search_refreshes_exact_request_profile_at_commit() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let mut app = AgentsWindow::with_data_root(data.clone());
    app.view.workspace = root.path().display().to_string();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "openai".into();
    app.view.model = "fixture-model".into();
    app.view.composer_model = "fixture-model".into();
    app.view.composer_effort = "default".into();
    let mut discovery = exact_model_listing().remove(0);
    discovery.provider_id = "openai".into();
    let mut model = discovery.models.remove("context-exact-model").unwrap();
    model.id = "fixture-model".into();
    model.supported_efforts = vec!["high".into()];
    discovery.models.insert(model.id.clone(), model);
    app.model_discoveries = vec![discovery];
    assert!(app.persist_native_settings());
    let connection = app.native_settings().resolve_at(&data).unwrap();
    let endpoint = connection.endpoint.clone();
    let backend = native_model_backend(
        connection,
        "openai",
        Some("default"),
        None,
        &data,
        app.view.catalog.as_deref(),
        &app.model_discoveries,
    )
    .unwrap();
    let route = openide_core::ModelRouteSnapshot::new(
        "openai",
        "fixture-model",
        &endpoint,
        backend.request_profile_fingerprint().unwrap(),
    )
    .unwrap();
    let (store, id) = crate::native_context_ui::tests::journal_fixture(
        &data,
        root.path(),
        Some(&route),
        &[Some(openide_core::ModelTokenUsage {
            input_tokens: 1000,
            output_tokens: 200,
            total_tokens: 1200,
            cached_input_tokens: Some(300),
            reasoning_tokens: Some(50),
        })],
    );
    app.open_session(&id);
    app.refresh_composer_context_percent();
    assert_eq!(app.view.native_context.remaining_percent, Some(94));
    let journal_path = store.storage_root().join(format!("{id}.jsonl"));
    let before = std::fs::read(&journal_path).unwrap();
    let ctx = egui::Context::default();
    app.apply_action(
        UiAction::Clicked {
            id: "native_web_search:indexed".into(),
        },
        &ctx,
    );
    assert_eq!(
        app.view.hosted_web_search_mode,
        Some(HostedWebSearchMode::Indexed)
    );
    assert!(
        app.view.native_context.last_usage.is_none()
            && app.view.native_context.remaining_percent.is_none()
            && app.view.native_context.conversation_usage.is_none()
    );
    assert_eq!(app.view.native_context.unowned_responses, 1);
    assert_eq!(
        app.view.native_context.measurement_status,
        crate::native_context_ui::NativeContextMeasurementStatus::RouteMismatch
    );
    assert!(app.view.native_context.used_fraction().is_none());
    app.apply_action(
        UiAction::Clicked {
            id: "native_web_search:off".into(),
        },
        &ctx,
    );
    assert_eq!(app.view.native_context.remaining_percent, Some(94));
    app.select_composer_effort("high");
    assert_eq!(app.view.composer_effort, "high");
    assert!(
        app.view.native_context.last_usage.is_none()
            && app.view.native_context.conversation_usage.is_none()
    );
    app.select_composer_effort("default");
    assert_eq!(app.view.native_context.remaining_percent, Some(94));
    assert_eq!(std::fs::read(&journal_path).unwrap(), before);
}
