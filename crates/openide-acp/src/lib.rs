//! A client of the Agent Client Protocol (ACP): openADE runs an external
//! coding agent (Grok, Antigravity, Gemini, Copilot, Cursor and every other
//! agent of the ACP registry) as a child process and drives its sessions.

mod connection;
pub mod registry;
pub mod schema;

pub use connection::{AgentCommand, Connection, Error, Event, PermissionAnswer};

#[cfg(test)]
mod tests {
    use super::*;
    use schema::*;
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    /// An agent on the other end of two pipes, playing `script` with what it
    /// reads; it gets each line the client writes and the pipe to answer on.
    fn fake(script: impl FnOnce(&mut dyn Iterator<Item = Value>, &mut dyn FnMut(Value)) + Send + 'static) -> (Connection, Receiver<Event>) {
        let (client_reads, agent_writes) = std::io::pipe().unwrap();
        let (agent_reads, client_writes) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let mut lines = BufReader::new(agent_reads).lines().map_while(Result::ok).filter_map(|line| serde_json::from_str(&line).ok());
            let mut output = agent_writes;
            let mut send = |message: Value| {
                writeln!(output, "{message}").unwrap();
            };
            script(&mut lines, &mut send);
        });
        let (sender, events) = std::sync::mpsc::channel();
        let connection = Connection::over(client_reads, client_writes, move |event| {
            let _ = sender.send(event);
        });
        (connection, events)
    }

    fn update(text: &str) -> Value {
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}}})
    }

    fn next(events: &Receiver<Event>) -> Event {
        events.recv_timeout(Duration::from_secs(5)).expect("an event")
    }

    #[test]
    fn a_turn_streams_updates_asks_permission_and_ends() {
        let (connection, events) = fake(|lines, send| {
            let init = lines.next().unwrap();
            assert_eq!(init["method"], "initialize");
            assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);
            send(json!({"jsonrpc": "2.0", "id": init["id"], "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": true, "promptCapabilities": {"image": true}},
                "authMethods": [{"id": "grok.com", "name": "Grok"}],
            }}));
            let new = lines.next().unwrap();
            assert_eq!(new["method"], "session/new");
            send(json!({"jsonrpc": "2.0", "id": new["id"], "result": {"sessionId": "s1", "configOptions": [
                {"id": "model", "name": "Modelo", "category": "model", "type": "select", "currentValue": "grok-4.6",
                 "options": [{"value": "grok-4.6", "name": "Grok 4.6"}]}
            ]}}));
            let prompt = lines.next().unwrap();
            assert_eq!(prompt["params"]["prompt"][0]["text"], "arreglá el test");
            send(update("Miro "));
            send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {
                "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Editar lib.rs", "kind": "edit", "status": "pending"}}}));
            send(json!({"jsonrpc": "2.0", "id": "p1", "method": "session/request_permission", "params": {
                "sessionId": "s1", "toolCall": {"toolCallId": "t1"},
                "options": [{"optionId": "yes", "name": "Permitir", "kind": "allow_once"}, {"optionId": "no", "name": "Rechazar", "kind": "reject_once"}]}}));
            let answer = lines.next().unwrap();
            assert_eq!(answer["id"], "p1");
            assert_eq!(answer["result"]["outcome"], json!({"outcome": "selected", "optionId": "yes"}));
            send(update("listo"));
            send(json!({"jsonrpc": "2.0", "id": prompt["id"], "result": {"stopReason": "end_turn"}}));
        });
        let init = connection.initialize("openADE", "0.0.0").unwrap();
        assert!(init.agent_capabilities.load_session);
        assert!(init.agent_capabilities.prompt_capabilities.image);
        assert_eq!(init.auth_methods[0].id, "grok.com");
        let session = connection.new_session(std::path::Path::new("/w"), json!([])).unwrap();
        assert_eq!(session.session_id, "s1");
        assert_eq!(session.config_options.unwrap()[0].category.as_deref(), Some("model"));
        let connection = std::sync::Arc::new(connection);
        let turn = {
            let connection = connection.clone();
            std::thread::spawn(move || connection.prompt("s1", &[ContentBlock::Text { text: "arreglá el test".into() }]))
        };
        assert!(matches!(next(&events), Event::Update { update: SessionUpdate::AgentMessageChunk { .. }, .. }));
        assert!(matches!(next(&events), Event::Update { update: SessionUpdate::ToolCall(call), .. } if call.kind == ToolKind::Edit));
        let Event::Permission { request, permission } = next(&events) else {
            panic!("a permission request");
        };
        assert_eq!(permission.options[1].kind, PermissionOptionKind::RejectOnce);
        connection.answer_permission(&request, &PermissionAnswer::Selected("yes".into())).unwrap();
        assert!(matches!(next(&events), Event::Update { .. }));
        assert_eq!(turn.join().unwrap(), Ok(StopReason::EndTurn));
    }

    #[test]
    fn a_changed_option_comes_back_with_the_others() {
        let (connection, _events) = fake(|lines, send| {
            let set = lines.next().unwrap();
            assert_eq!(set["params"], json!({"sessionId": "s1", "configId": "model", "value": "b"}));
            send(json!({"jsonrpc": "2.0", "id": set["id"], "result": {"configOptions": [
                {"id": "model", "name": "Modelo", "category": "model", "type": "select", "currentValue": "b",
                 "options": [{"value": "a", "name": "A"}, {"value": "b", "name": "B"}]}
            ]}}));
        });
        let options = connection.set_config_option("s1", "model", "b").unwrap();
        assert!(matches!(&options[0].value, ConfigValue::Select { current_value, .. } if current_value == "b"), "{options:?}");
    }

    #[test]
    fn cancelling_ends_the_turn_as_cancelled() {
        let (connection, _events) = fake(|lines, send| {
            let prompt = lines.next().unwrap();
            let cancel = lines.next().unwrap();
            assert_eq!(cancel["method"], "session/cancel");
            assert!(cancel.get("id").is_none(), "a notification");
            send(json!({"jsonrpc": "2.0", "id": prompt["id"], "result": {"stopReason": "cancelled"}}));
        });
        let connection = std::sync::Arc::new(connection);
        let turn = {
            let connection = connection.clone();
            std::thread::spawn(move || connection.prompt("s1", &[ContentBlock::Text { text: "x".into() }]))
        };
        std::thread::sleep(Duration::from_millis(50));
        connection.cancel("s1").unwrap();
        assert_eq!(turn.join().unwrap(), Ok(StopReason::Cancelled));
    }

    #[test]
    fn requests_for_services_the_client_did_not_offer_are_refused() {
        let (connection, events) = fake(|lines, send| {
            send(json!({"jsonrpc": "2.0", "id": 77, "method": "fs/read_text_file", "params": {"sessionId": "s1", "path": "/etc/passwd"}}));
            let refusal = lines.next().unwrap();
            assert_eq!(refusal["id"], 77);
            assert_eq!(refusal["error"]["code"], -32601);
            send(update("visto"));
        });
        assert!(matches!(next(&events), Event::Update { .. }));
        drop(connection);
    }

    #[test]
    fn an_agent_that_goes_away_fails_its_requests_and_says_so() {
        let (connection, events) = fake(|lines, _send| {
            lines.next();
        });
        let error = connection.initialize("openADE", "0").unwrap_err();
        assert!(matches!(error, Error::Closed(_)), "{error:?}");
        assert!(matches!(next(&events), Event::Closed { .. }));
    }

    #[test]
    fn errors_from_the_agent_reach_the_caller() {
        let (connection, _events) = fake(|lines, send| {
            let new = lines.next().unwrap();
            send(json!({"jsonrpc": "2.0", "id": new["id"], "error": {"code": -32000, "message": "Authentication required"}}));
        });
        let error = connection.new_session(std::path::Path::new("/w"), json!([])).unwrap_err();
        assert_eq!(error, Error::Rpc { code: -32000, message: "Authentication required".into() });
    }

    #[test]
    fn a_real_process_is_spawned_heard_and_killed_with_its_connection() {
        let command = AgentCommand {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                r#"read line; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'; echo ready >&2; sleep 30"#.into(),
            ],
            env: Vec::new(),
        };
        let connection = Connection::spawn(&command, &std::env::temp_dir(), |_| {}).unwrap();
        assert_eq!(connection.initialize("openADE", "0").unwrap().protocol_version, 1);
        let started = std::time::Instant::now();
        while connection.stderr_tail().is_empty() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(connection.stderr_tail(), "ready");
        drop(connection);
        assert!(started.elapsed() < Duration::from_secs(10), "dropping kills the agent instead of waiting for it");
        let silent = AgentCommand { program: "sh".into(), args: vec!["-c".into(), "exec sleep 30".into()], env: Vec::new() };
        let connection = std::sync::Arc::new(Connection::spawn(&silent, &std::env::temp_dir(), |_| {}).unwrap());
        let asking = {
            let connection = connection.clone();
            std::thread::spawn(move || connection.initialize("openADE", "0"))
        };
        std::thread::sleep(Duration::from_millis(50));
        connection.kill();
        assert!(matches!(asking.join().unwrap(), Err(Error::Closed(_))), "a killed agent's open request fails at once");
        assert!(started.elapsed() < Duration::from_secs(10));
        let missing = AgentCommand { program: "/no/such/agent".into(), args: Vec::new(), env: Vec::new() };
        assert!(matches!(Connection::spawn(&missing, &std::env::temp_dir(), |_| {}), Err(Error::Spawn(_))));
    }
}
