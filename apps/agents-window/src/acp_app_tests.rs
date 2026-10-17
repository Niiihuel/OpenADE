use super::*;
use crate::acp_run::AcpOffer;
use crate::transcript::TranscriptItem;
use openide_acp::AgentCommand;
use serde_json::json;
use std::io::{BufRead, Write};

const FAKE_AGENT: &str = "OPENADE_FAKE_ACP_AGENT";

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
    let mut lines = std::io::stdin().lock().lines().map_while(Result::ok).filter_map(|line| serde_json::from_str::<Value>(&line).ok());
    let send = |message: Value| {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{message}").unwrap();
        out.flush().unwrap();
    };
    let update = |update: Value| send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "fake-1", "update": update}}));
    // The harness has just printed "test … ..." with no line break.
    send(Value::String(String::new()));
    while let Some(message) = lines.next() {
        match message["method"].as_str() {
            Some("initialize") => send(json!({"jsonrpc": "2.0", "id": message["id"], "result": {"protocolVersion": 1}})),
            Some("session/new") => send(json!({"jsonrpc": "2.0", "id": message["id"], "result": {"sessionId": "fake-1"}})),
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
    while !done(app) {
        app.poll_run();
        assert!(Instant::now() < deadline, "timed out: {}", app.view.status);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_acp_agent_answers_in_the_conversation_and_asks_before_acting() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = workspace.display().to_string();
    app.view.acp_agents = vec![AcpOffer { id: "fake".into(), name: "Agente falso".into(), command: fake_agent() }];
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
