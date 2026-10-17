use super::*;
use crate::acp_run::AcpOffer;
use crate::transcript::TranscriptItem;
use openide_acp::AgentCommand;
use serde_json::json;
use std::io::{BufRead, Write};
use std::path::Path;

const FAKE_AGENT: &str = "OPENADE_FAKE_ACP_AGENT";
/// A file the fake agent writes each message it gets to, one per line.
const FAKE_LOG: &str = "OPENADE_FAKE_ACP_LOG";
/// Set, the fake agent asks to sign in and never finishes signing in.
const FAKE_LOGIN: &str = "OPENADE_FAKE_ACP_LOGIN";
/// Set, the fake agent takes no images in a prompt.
const FAKE_NO_IMAGES: &str = "OPENADE_FAKE_ACP_NO_IMAGES";

/// The fake agent: this test binary, run again on this one test, which then
/// speaks ACP on its stdio. The harness's own lines are not JSON, and the
/// client skips them.
fn fake_agent() -> AgentCommand {
    AgentCommand {
        program: std::env::current_exe().unwrap(),
        args: ["--exact", "app::acp_app_tests::fake_acp_agent", "--ignored", "--nocapture", "--test-threads=1"]
            .map(str::to_owned)
            .to_vec(),
        env: vec![(FAKE_AGENT.into(), "1".into())],
    }
}

#[test]
#[ignore = "run by the other tests as their ACP agent"]
fn fake_acp_agent() {
    if std::env::var_os(FAKE_AGENT).is_none() {
        return;
    }
    let log = std::env::var_os(FAKE_LOG);
    let login = std::env::var_os(FAKE_LOGIN).is_some();
    let images = std::env::var_os(FAKE_NO_IMAGES).is_none();
    let mut lines = std::io::stdin().lock().lines().map_while(Result::ok).filter_map(|line| serde_json::from_str::<Value>(&line).ok()).inspect(|message| {
        if let Some(log) = &log {
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
            writeln!(file, "{message}").unwrap();
        }
    });
    let send = |message: Value| {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{message}").unwrap();
        out.flush().unwrap();
    };
    let update = |update: Value| send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "fake-1", "update": update}}));
    let mut chosen = json!({"model": "fake-a", "effort": "high"});
    let config = |chosen: &Value| {
        json!([
            {"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": chosen["model"],
             "options": [{"value": "fake-a", "name": "Rápido"}, {"value": "fake-b", "name": "Profundo"}]},
            {"id": "effort", "name": "Effort", "category": "thought_level", "type": "select", "currentValue": chosen["effort"],
             "options": [{"value": "low", "name": "Bajo"}, {"value": "high", "name": "Alto"}]}
        ])
    };
    // The harness has just printed "test … ..." with no line break.
    send(Value::String(String::new()));
    while let Some(message) = lines.next() {
        match message["method"].as_str() {
            Some("initialize") => send(json!({"jsonrpc": "2.0", "id": message["id"], "result": {"protocolVersion": 1,
                "agentCapabilities": {"promptCapabilities": {"image": images}}, "authMethods": [{"id": "fake-login", "name": "Cuenta falsa"}]}})),
            // Signing in waits on the user's browser, which never answers here.
            Some("session/new") if login => send(json!({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32000, "message": "Authentication required"}})),
            Some("session/new") => send(json!({"jsonrpc": "2.0", "id": message["id"], "result": {"sessionId": "fake-1", "configOptions": config(&chosen)}})),
            Some("session/set_config_option") => {
                chosen[message["params"]["configId"].as_str().unwrap()] = message["params"]["value"].clone();
                send(json!({"jsonrpc": "2.0", "id": message["id"], "result": {"configOptions": config(&chosen)}}));
            }
            Some("session/prompt") => {
                update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hola desde ACP"}}));
                update(json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "echo hi", "kind": "execute", "status": "pending", "rawInput": {"command": "echo hi"}}));
                send(json!({"jsonrpc": "2.0", "id": 900, "method": "session/request_permission", "params": {"sessionId": "fake-1", "toolCall": {"toolCallId": "t1"},
                    "options": [{"optionId": "ok", "name": "Permitir", "kind": "allow_once"}, {"optionId": "no", "name": "Rechazar", "kind": "reject_once"}]}}));
                let answer = lines.next().unwrap();
                let allowed = answer["result"]["outcome"]["optionId"] == "ok";
                update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": if allowed { "completed" } else { "failed" },
                    "content": [{"type": "content", "content": {"type": "text", "text": if allowed { "hi" } else { "rechazado" }}}]}));
                update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": " y fin"}}));
                send(json!({"jsonrpc": "2.0", "id": message["id"], "result": {"stopReason": "end_turn"}}));
            }
            _ => {}
        }
    }
}

fn pump(app: &mut AgentsWindow, done: impl Fn(&AgentsWindow) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let ctx = egui::Context::default();
    while !done(app) {
        app.poll_run();
        app.poll_parked_runs();
        app.poll_acp_agents(&ctx);
        assert!(Instant::now() < deadline, "timed out: {}", app.view.status);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The messages the fake agent got, as written to its log.
fn received(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// A window that knows the fake agent, in a project of its own.
fn window(root: &Path) -> AgentsWindow {
    let workspace = root.join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.join("data"));
    app.view.workspace = workspace.display().to_string();
    app.view.acp_agents = vec![AcpOffer { id: "fake".into(), name: "Agente falso".into(), command: fake_agent() }];
    app
}

/// Runs one turn, allowing what the agent asks for.
fn turn(app: &mut AgentsWindow, prompt: &str) {
    app.view.prompt = prompt.into();
    app.start_run();
    pump(app, |app| !app.view.pending_permissions.is_empty());
    let request = app.view.pending_permissions[0].request_id.clone();
    app.answer_permission(&request, true);
    pump(app, |app| app.active_run.is_none() && !app.view.running);
}

#[test]
fn an_acp_agent_answers_in_the_conversation_and_asks_before_acting() {
    let root = tempfile::tempdir().unwrap();
    let mut app = window(root.path());
    app.start_acp_task("fake");
    assert_eq!(app.view.selected_backend, "acp:fake", "{}", app.view.status);
    let session = app.view.selected_session.clone().unwrap();

    app.view.prompt = "decí hola".into();
    app.start_run();
    assert!(app.view.running, "{}", app.view.status);
    pump(&mut app, |app| !app.view.pending_permissions.is_empty());
    let request = app.view.pending_permissions[0].request_id.clone();
    assert_eq!(app.view.pending_permissions[0].tool_name, "run_command");
    app.answer_permission(&request, true);
    pump(&mut app, |app| app.active_run.is_none() && !app.view.running);

    let transcript = &app.view.transcript;
    assert!(transcript.iter().any(|item| matches!(item, TranscriptItem::User { content, .. } if content == "decí hola")), "{transcript:?}");
    assert!(transcript.iter().any(|item| matches!(item, TranscriptItem::Assistant { content, streaming: false } if content == "Hola desde ACP y fin")), "{transcript:?}");
    assert!(transcript.iter().any(|item| matches!(item, TranscriptItem::Tool { name, output: Some(output), is_error: false, .. } if name == "run_command" && output.trim() == "hi")), "{transcript:?}");
    let stored = app.store.as_ref().unwrap().load(&session).unwrap().unwrap();
    assert!(stored.events.iter().any(|event| matches!(&event.kind, AgentEventKind::TurnCompleted { response, .. } if response == "Hola desde ACP y fin")));
    assert!(app.view.pending_permissions.is_empty());

    // The agent stays up for the next turn, which a denial ends in an error row.
    app.view.prompt = "otra vez".into();
    app.start_run();
    pump(&mut app, |app| !app.view.pending_permissions.is_empty());
    let request = app.view.pending_permissions[0].request_id.clone();
    app.answer_permission(&request, false);
    pump(&mut app, |app| app.active_run.is_none() && !app.view.running);
    // A denial reads as the native agent's do, on that turn's own row.
    let tools: Vec<_> = app.view.transcript.iter().filter(|item| matches!(item, TranscriptItem::Tool { .. })).collect();
    assert_eq!(tools.len(), 2, "{tools:#?}");
    assert!(matches!(tools[1], TranscriptItem::Tool { output: Some(output), is_error: true, .. } if output == "Permiso denegado"), "{tools:#?}");
    assert_eq!(crate::acp_run::remembered_session(&app.data_root, &session).as_deref(), Some("fake-1"));
}

#[test]
fn an_agent_that_is_not_installed_says_so_instead_of_starting() {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = root.path().display().to_string();
    app.start_acp_task("ghost");
    assert!(app.view.selected_session.is_none());
    assert!(app.view.status.contains("ghost"), "{}", app.view.status);
}

#[test]
fn the_agents_own_models_and_efforts_are_chosen_in_the_composer_and_remembered() {
    let root = tempfile::tempdir().unwrap();
    let mut app = window(root.path());
    let ctx = egui::Context::default();
    app.start_acp_task("fake");
    let session = app.view.selected_session.clone().unwrap();
    // Opening the task starts its agent, whose options reach the composer.
    pump(&mut app, |app| !app.view.composer_model_options.is_empty());
    assert_eq!(app.view.composer_model_options, [("fake-a".into(), "Rápido".into()), ("fake-b".into(), "Profundo".into())]);
    assert_eq!((app.view.composer_model.as_str(), app.view.composer_effort.as_str()), ("fake-a", "high"));
    app.apply_action(UiAction::Clicked { id: "composer_model_toggle".into() }, &ctx);
    assert!(app.view.composer_model_menu_open);
    app.apply_action(UiAction::Clicked { id: "composer_model:fake-b".into() }, &ctx);
    app.apply_action(UiAction::Clicked { id: "composer_effort:low".into() }, &ctx);
    assert_eq!((app.view.composer_model.as_str(), app.view.composer_effort.as_str()), ("fake-b", "low"));
    // Unknown to the agent, so nothing changes.
    app.apply_action(UiAction::Clicked { id: "composer_model:gpt-4.1".into() }, &ctx);
    assert_eq!(app.view.composer_model, "fake-b");

    turn(&mut app, "hola");
    let config = app.acp_slots[&session].config();
    assert_eq!(crate::acp_run::choices(&config, "model").unwrap().0, "fake-b", "{}", app.view.status);
    assert_eq!(crate::acp_run::choices(&config, "thought_level").unwrap().0, "low");
    assert!(app.view.transcript.iter().any(|item| matches!(item, TranscriptItem::Assistant { content, .. } if content == "Hola desde ACP y fin")));

    // The next task of this agent starts with the same choice.
    app.start_acp_task("fake");
    assert_ne!(app.view.selected_session.as_deref(), Some(session.as_str()));
    pump(&mut app, |app| !app.view.composer_model_options.is_empty());
    assert_eq!(app.view.composer_model, "fake-b");
    assert!(!app.acp_slots.contains_key(&session), "the idle agent of the other task stops");
}

#[test]
fn attachments_reach_the_agent_and_images_go_inline_when_it_takes_them() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC").unwrap();
    for takes_images in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let mut app = window(root.path());
        let log = root.path().join("agent.log");
        app.view.acp_agents[0].command.env.push((FAKE_LOG.into(), log.display().to_string()));
        if !takes_images {
            app.view.acp_agents[0].command.env.push((FAKE_NO_IMAGES.into(), "1".into()));
        }
        app.start_acp_task("fake");
        // The started agent has said whether it takes images.
        pump(&mut app, |app| !app.view.composer_model_options.is_empty());
        let workspace = PathBuf::from(&app.view.workspace);
        std::fs::write(workspace.join("screen.png"), &png).unwrap();
        std::fs::write(workspace.join("notes.md"), "notas").unwrap();
        for name in ["screen.png", "notes.md"] {
            app.view.composer_attachments.push(composer_attachment_for_path(&workspace, Path::new(name)).unwrap());
        }

        turn(&mut app, "mirá esto");
        let prompt = received(&log).into_iter().find(|message| message["method"] == "session/prompt").expect("a prompt")["params"]["prompt"].clone();
        let text = prompt[0]["text"].as_str().unwrap();
        assert!(text.starts_with("mirá esto") && text.contains("notes.md") && text.contains("screen.png"), "{text}");
        let shown = if takes_images {
            let image = UserImage::from_bytes(UserImageMime::Png, &png).unwrap();
            assert_eq!(prompt[1], json!({"type": "image", "mimeType": "image/png", "data": image.data_base64}));
            1
        } else {
            assert_eq!(prompt.as_array().unwrap().len(), 1, "the image goes as a path: {prompt}");
            0
        };
        assert!(app.view.composer_attachments.is_empty());
        assert!(
            app.view.transcript.iter().any(|item| matches!(item, TranscriptItem::User { content, images } if content == text && images.len() == shown)),
            "{:?}",
            app.view.transcript
        );
    }
}

#[test]
fn an_agent_that_asks_to_sign_in_says_so_and_stop_ends_the_wait() {
    let root = tempfile::tempdir().unwrap();
    let mut app = window(root.path());
    let ctx = egui::Context::default();
    let log = root.path().join("agent.log");
    app.view.acp_agents[0].command.env.extend([(FAKE_LOG.into(), log.display().to_string()), (FAKE_LOGIN.into(), "1".into())]);
    app.start_acp_task("fake");
    let session = app.view.selected_session.clone().unwrap();
    // Opening the task does not open the browser: the first message does.
    pump(&mut app, |app| app.view.composer_error.is_some());
    assert!(app.view.composer_error.as_deref().unwrap().contains("pide iniciar sesión"), "{:?}", app.view.composer_error);
    assert!(!received(&log).iter().any(|message| message["method"] == "authenticate"));

    app.view.prompt = "hola".into();
    app.start_run();
    let told = |app: &AgentsWindow| {
        app.view.transcript.iter().any(|item| matches!(item, TranscriptItem::Notice { message, .. } if message.contains("pidió iniciar sesión")))
    };
    pump(&mut app, told);
    let started = Instant::now();
    app.apply_action(UiAction::Clicked { id: "turn_stop".into() }, &ctx);
    pump(&mut app, |app| app.active_run.is_none() && !app.view.running);
    assert!(started.elapsed() < Duration::from_secs(10), "Stop does not wait for the sign-in to time out");
    assert_eq!(app.view.status, "Turno cancelado");
    assert_eq!(app.view.prompt, "hola", "the message comes back to the composer");
    let stored = app.store.as_ref().unwrap().load(&session).unwrap().unwrap();
    assert!(stored.events.iter().any(|event| matches!(event.kind, AgentEventKind::TurnCancelled)));
    assert_eq!(received(&log).iter().filter(|message| message["method"] == "authenticate").count(), 1);
}

#[test]
fn settings_list_the_registry_and_install_turn_on_and_remove_its_agents() {
    let root = tempfile::tempdir().unwrap();
    let mut app = window(root.path());
    let ctx = egui::Context::default();
    // A fresh copy of the index keeps the refreshes off the network.
    std::fs::create_dir_all(&app.data_root).unwrap();
    std::fs::write(app.data_root.join("acp-registry.json"), include_str!("../../../crates/openide-acp/testdata/registry.json")).unwrap();
    let registry: openide_acp::registry::Registry =
        serde_json::from_str(include_str!("../../../crates/openide-acp/testdata/registry.json")).unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("npx"), "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("npx"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let installs = app.data_root.join("acp-agents");
    let downloaded = installs.join("antigravity-acp");
    app.view.acp_catalog = crate::acp_run::catalog(&registry, Some(bin.as_os_str()), &installs, &[]);
    let linux = openide_acp::registry::platform() == "linux-x86_64";
    if linux {
        let Some(crate::acp_run::AcpSource::Download { target, dir }) =
            app.view.acp_catalog.iter().find(|entry| entry.id == "antigravity-acp").map(|entry| entry.source.clone())
        else {
            panic!("Antigravity to download");
        };
        std::fs::create_dir_all(dir.join(&target.cmd).parent().unwrap()).unwrap();
        std::fs::write(dir.join(&target.cmd), "").unwrap();
        let found = crate::acp_run::catalog(&registry, Some(bin.as_os_str()), &installs, &[]);
        assert!(matches!(found.iter().find(|entry| entry.id == "antigravity-acp").unwrap().source, crate::acp_run::AcpSource::Downloaded));
        app.view.acp_catalog = found;
    }

    let mut tree = UiTree::new(crate::settings_ui::build_settings_page(&app.view, SettingsPage::ExternalAgents, 900.0, true));
    let frame = tree.layout(openide_ui::Size::new(900.0, 40000.0));
    let ids: Vec<&str> = frame.regions.iter().map(|region| region.id.as_str()).collect();
    assert!(ids.contains(&"acp_package:gemini"), "{ids:?}");
    if linux {
        assert!(ids.contains(&"acp_remove:antigravity-acp"), "{ids:?}");
        assert!(ids.iter().any(|id| id.starts_with("acp_install:")), "{ids:?}");
    }

    // The registry's search keeps only what matches, by name, id or description.
    app.view.acp_query = "GEMINI".into();
    let mut tree = UiTree::new(crate::settings_ui::build_settings_page(&app.view, SettingsPage::ExternalAgents, 900.0, true));
    let frame = tree.layout(openide_ui::Size::new(900.0, 40000.0));
    let ids: Vec<&str> = frame.regions.iter().map(|region| region.id.as_str()).collect();
    assert!(ids.contains(&"acp_package:gemini") && ids.contains(&"acp_query"), "{ids:?}");
    assert!(!ids.iter().any(|id| id.starts_with("acp_install:")), "{ids:?}");
    app.view.acp_query.clear();

    app.apply_action(UiAction::Clicked { id: "acp_package:gemini".into() }, &ctx);
    assert_eq!(crate::acp_run::packages_on(&app.data_root), ["gemini"]);
    if linux {
        app.apply_action(UiAction::Clicked { id: "acp_remove:antigravity-acp".into() }, &ctx);
        assert!(!downloaded.exists());
        assert!(!app.view.settings_notice_error, "{:?}", app.view.settings_notice);
    }
    // Only what openADE downloaded can be removed.
    app.apply_action(UiAction::Clicked { id: "acp_remove:gemini".into() }, &ctx);
    pump(&mut app, |app| app.acp_offers_loading.is_none());
    assert!(!app.view.acp_catalog.is_empty());
}

/// Writes the ACP Settings rows and the composer's menu of an ACP agent's
/// models as PNG files under `OPENADE_VISUAL_DIR`, to look at.
#[test]
#[ignore = "visual review: OPENADE_VISUAL_DIR=<dir> cargo test export_acp_visual_review -- --ignored"]
fn export_acp_visual_review() {
    let out = PathBuf::from(std::env::var_os("OPENADE_VISUAL_DIR").expect("OPENADE_VISUAL_DIR"));
    std::fs::create_dir_all(&out).unwrap();
    let png = |element: Element, name: &str, width: f32, height: f32| {
        let root = Element::column(vec![element]).background(crate::theme::BACKGROUND).size(Some(width), Some(height));
        let (_context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
        let frame = tree.layout(openide_ui::Size::new(width, height));
        let svg = out.join(format!("{name}.svg"));
        crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &svg, width, height).unwrap();
        let parsed = resvg::usvg::Tree::from_str(&std::fs::read_to_string(&svg).unwrap(), &crate::settings_ui::visual_qa::native_svg_options()).unwrap();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
        resvg::render(&parsed, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
        pixmap.save_png(out.join(format!("{name}.png"))).unwrap();
    };
    let root = tempfile::tempdir().unwrap();
    let mut app = window(root.path());
    let registry: openide_acp::registry::Registry =
        serde_json::from_str(include_str!("../../../crates/openide-acp/testdata/registry.json")).unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["npx", "grok"] {
        std::fs::write(bin.join(name), "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    app.view.acp_catalog = crate::acp_run::catalog(&registry, Some(bin.as_os_str()), &root.path().join("installs"), &["gemini".into()]);
    app.view.acp_installing = Some("cursor".into());
    png(crate::settings_ui::build_settings_page(&app.view, SettingsPage::ExternalAgents, 900.0, true), "acp-settings", 900.0, 5200.0);
    app.view.acp_query = "google".into();
    png(crate::settings_ui::build_settings_page(&app.view, SettingsPage::ExternalAgents, 900.0, true), "acp-settings-search", 900.0, 2600.0);
    app.view.acp_query = "zzz".into();
    png(crate::settings_ui::build_settings_page(&app.view, SettingsPage::ExternalAgents, 900.0, true), "acp-settings-none", 900.0, 2600.0);
    app.view.acp_query.clear();

    app.start_acp_task("fake");
    pump(&mut app, |app| !app.view.composer_model_options.is_empty());
    app.view.composer_model_menu_open = true;
    app.view.composer_model_list_open = true;
    // Its menus open upwards, over the conversation.
    let composer = |app: &AgentsWindow| {
        let composer = crate::composer_ui::build_composer(&crate::composer_ui::ComposerProps::from_state(&app.view, 720.0, false, None, "Escribe", None));
        Element::column(vec![Element::spacer().flex(1.0), composer])
    };
    png(composer(&app), "acp-composer-models", 720.0, 520.0);
    app.view.composer_model_list_open = false;
    png(composer(&app), "acp-composer-menu", 720.0, 520.0);
}

#[test]
fn an_acp_turn_goes_on_off_screen_and_what_it_asks_waits_for_the_person() {
    let root = tempfile::tempdir().unwrap();
    let mut app = window(root.path());
    app.start_acp_task("fake");
    let away = app.view.selected_session.clone().unwrap();
    app.view.prompt = "decí hola".into();
    app.start_run();
    assert!(app.view.running, "{}", app.view.status);

    // Another task opens over it: the turn is parked, and what it asks waits
    // for the person as a mark on its row, not as a denial.
    app.start_acp_task("fake");
    let on_screen = app.view.selected_session.clone().unwrap();
    assert_ne!(on_screen, away);
    assert!(!app.view.running && app.parked_runs.len() == 1, "{}", app.view.status);
    pump(&mut app, |app| app.view.off_screen_turns.get(&away) == Some(&true));
    let stored = app.store.as_ref().unwrap().load(&away).unwrap().unwrap();
    assert!(stored.events.iter().any(|event| matches!(&event.kind, AgentEventKind::PermissionRequested { .. })), "its events are in its own journal");
    assert!(app.view.transcript.is_empty() && app.view.pending_permissions.is_empty(), "and not on screen");

    // Back on it, the request is there to answer, and the turn ends on screen
    // without telling the transcript anything twice.
    app.open_session(&away);
    assert!(app.view.running && app.active_run.is_some(), "{}", app.view.status);
    assert_eq!(app.view.pending_permissions.len(), 1);
    assert!(app.view.transcript.iter().any(|item| matches!(item, TranscriptItem::Assistant { content, .. } if content.starts_with("Hola desde ACP"))), "{:?}", app.view.transcript);
    let request = app.view.pending_permissions[0].request_id.clone();
    app.answer_permission(&request, true);
    pump(&mut app, |app| app.active_run.is_none() && !app.view.running);
    assert!(app.view.transcript.iter().any(|item| matches!(item, TranscriptItem::Assistant { content, streaming: false } if content == "Hola desde ACP y fin")), "{:?}", app.view.transcript);
    let tools: Vec<_> = app.view.transcript.iter().filter(|item| matches!(item, TranscriptItem::Tool { .. })).collect();
    assert_eq!(tools.len(), 1, "{tools:#?}");
    assert!(app.parked_runs.is_empty() && app.view.off_screen_turns.is_empty());
}
