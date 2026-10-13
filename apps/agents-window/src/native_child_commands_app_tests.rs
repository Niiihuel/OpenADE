use super::native_command_control_app_tests::{
    acknowledge, apply, click, open, paste, probe, tree, wait_bytes,
};
use super::*;
use openide_core::{NativeCommandOutputStore, NativeCommandOutputStream};
use openide_harness::{LocalTool, ToolContext};
use serde_json::{json, Value};
use std::net::{TcpListener, TcpStream};

pub(super) fn request(socket: &mut TcpStream) -> Value {
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut bytes = Vec::new();
    let (end, length) = loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() <= 2 * 1024 * 1024);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
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
    while bytes.len() < end + length {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&bytes[end..end + length]).unwrap()
}

pub(super) fn response(socket: &mut TcpStream, delta: Value, finish: &str) {
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"id":"child-command-response","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
        json!({"id":"child-command-response","choices":[{"index":0,"delta":{},"finish_reason":finish}]})
    );
    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
}

pub(super) fn tool(id: &str, name: &str, arguments: Value) -> Value {
    json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]})
}

async fn next_request(receiver: &Receiver<Value>) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match receiver.try_recv() {
            Ok(request) => return request,
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "No native child HTTP request");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(error) => panic!("Child HTTP fixture disconnected: {error}"),
        }
    }
}

#[tokio::test]
async fn child_commands_share_host_controls_across_model_wait_followup_cancel_and_reopen() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let executable = probe(root.path());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (request_tx, requests) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let (poll_command_tx, poll_command) = mpsc::channel();
    let start_path = executable.clone();
    let server = std::thread::spawn(move || {
        for step in 0..4 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing child HTTP step {step}");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("child accept: {error}"),
                }
            };
            request_tx.send(request(&mut socket)).unwrap();
            match step {
                0 => response(
                    &mut socket,
                    tool(
                        "start-child",
                        "start_command",
                        json!({"command":start_path}),
                    ),
                    "tool_calls",
                ),
                1 => {
                    // The real Runner is awaiting this model response while
                    // the UI confirms stdin/EOF through the child lease actor.
                    release.recv_timeout(Duration::from_secs(10)).unwrap();
                    response(&mut socket, json!({"content":"first complete"}), "stop");
                }
                2 => {
                    let command: String =
                        poll_command.recv_timeout(Duration::from_secs(10)).unwrap();
                    response(
                        &mut socket,
                        tool(
                            "poll-child",
                            "poll_command",
                            json!({"session_id":command,"wait_ms":0}),
                        ),
                        "tool_calls",
                    );
                }
                3 => response(&mut socket, json!({"content":"followup complete"}), "stop"),
                _ => unreachable!(),
            }
        }
    });
    let data_root = root.path().join("data");
    let mut settings = AgentToolSettings::default();
    settings.set(LocalTool::RunCommand, true);
    settings.set_command_isolation(CommandIsolation::HostUnrestricted);
    settings.save(&data_root).unwrap();
    let mut app = AgentsWindow::with_data_root(data_root.clone());
    // This test is about the pipe contract; see `keep_pipes`.
    app.native_command_sessions.keep_pipes();
    app.view.native_provider_id = "custom".into();
    app.view.model = "fixture-child-commands".into();
    app.view.endpoint = endpoint;
    assert!(app.persist_native_settings());
    let sessions = app.store.clone().unwrap();
    let parent = sessions.create(&workspace, "native").unwrap();
    app.open_session(&parent.id);
    let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
    let coordinator =
        Arc::new(SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap());
    let connection = app.native_settings().resolve_at(&data_root).unwrap();
    let endpoint = connection.endpoint.clone();
    let model = native_model_backend(
        connection,
        "custom",
        Some(&app.view.composer_effort),
        app.view.hosted_web_search_mode,
        &data_root,
        app.view.catalog.as_deref(),
        &app.model_discoveries,
    )
    .unwrap();
    let local: Arc<dyn ToolExecutor> = Arc::new(
        LocalTools::new()
            .with_permissions(settings.permissions())
            .with_command_isolation(settings.command_isolation()),
    );
    let worker = SubagentWorker::new(
        coordinator.clone(),
        sessions.clone(),
        model,
        local.clone(),
        "custom",
        endpoint,
    )
    .with_permission_policy(app.native_permission_policy().unwrap())
    .with_session_approvals(settings.command_isolation())
    .with_event_bridge(app.subagent_bridge.clone())
    .with_nested_subagents(SubagentToolLimits::default())
    .with_native_goals()
    .with_system_instructions("Frozen child command instructions")
    .with_persistent_commands(
        app.native_command_sessions
            .command_profile(settings.command_isolation(), true)
            .unwrap()
            .unwrap(),
    );
    let child = worker
        .prepare_child(
            &parent_lease,
            "child-command-spawn",
            "initial command",
            TurnMode::Build,
        )
        .unwrap();
    let control = coordinator.issue_control(&parent_lease).unwrap();
    drop(parent_lease);
    let child_id = child.session.id.clone();
    let running_worker = worker.clone();
    let running_id = child_id.clone();
    let running = tokio::spawn(async move {
        running_worker
            .run_prepared(
                control,
                running_id,
                "child-command-spawn".into(),
                "initial command".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
    });
    let initial = next_request(&requests).await;
    let advertised = initial["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    for name in [
        "start_command",
        "poll_command",
        "write_stdin",
        "terminate_command",
    ] {
        assert!(advertised.contains(&name));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_subagent_events();
        if let Some(permission) = app
            .view
            .pending_subagent_permissions
            .iter()
            .find(|permission| permission.child_session_id == child_id)
            .cloned()
        {
            apply(
                &mut app,
                &format!(
                    "subagent_permission_allow:{}:{}",
                    child_id, permission.request_id
                ),
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "missing child approval: {}",
            app.view.status
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let second = next_request(&requests).await;
    assert!(second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool"));
    let manager = app.native_command_sessions.manager(&child_id).unwrap();
    assert_eq!(manager.native_session_owner(), Some(child_id.as_str()));
    let command = manager.list_sessions().unwrap()[0].id.clone();
    assert!(app
        .native_command_sessions
        .has_running_in_worktree(&workspace)
        .unwrap());
    assert!(sessions.try_acquire_native_lease(&child_id).is_err());
    app.open_session(&child_id);
    app.view.show_side_pane = true;
    app.view.side_tab = SideTab::Activity;
    open(&mut app, &child_id, &command).await;
    let pooled_actor = app
        .native_command_sessions
        .shared_registry()
        .unwrap()
        .manual_actor(&child_id)
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(
        &pooled_actor,
        &app.native_command_actor_for(&child_id).unwrap()
    ));
    let exact = "  ñ🦀\r\nCR\rTAB\tNUL\0fin  ";
    let mut editor = tree(&app, 360.0);
    paste(
        &mut app,
        &mut editor,
        360.0,
        "native_command_control_input",
        exact,
    );
    let mut preparation = tree(&app, 360.0);
    click(
        &mut app,
        &mut preparation,
        360.0,
        "native_command_control_prepare_input",
    );
    let mut confirmation = tree(&app, 360.0);
    click(
        &mut app,
        &mut confirmation,
        360.0,
        "native_command_control_confirm",
    );
    acknowledge(&mut app, 1).await;
    wait_bytes(&manager, &command, exact.as_bytes()).await;
    assert!(
        sessions.try_acquire_native_lease(&child_id).is_err(),
        "UI must use the current child actor's lease"
    );
    open(&mut app, &child_id, &command).await;
    let mut preparation = tree(&app, 900.0);
    click(
        &mut app,
        &mut preparation,
        900.0,
        "native_command_control_prepare_eof",
    );
    let mut confirmation = tree(&app, 900.0);
    click(
        &mut app,
        &mut confirmation,
        900.0,
        "native_command_control_confirm",
    );
    acknowledge(&mut app, 2).await;
    let expected = format!("{exact}<EOF>").into_bytes();
    wait_bytes(&manager, &command, &expected).await;
    release_tx.send(()).unwrap();
    assert_eq!(
        running.await.unwrap().unwrap().status,
        SubagentStatus::Completed
    );
    let stored = sessions.load(&child_id).unwrap().unwrap();
    // Rebuild the follow-up worker through the production desktop path rather
    // than keeping the original worker's tool executor or temporary runtime.
    let request = app.child_worker_request(&stored, false).unwrap();
    let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
    let sender = SubagentToolExecutor::new(
        local,
        request.worker,
        coordinator.clone(),
        sessions.clone(),
        parent_lease,
        SubagentToolLimits::default(),
    )
    .unwrap();
    poll_command_tx.send(command.clone()).unwrap();
    let queued = sender
        .execute(
            &ToolCall {
                id: "queue-followup".into(),
                name: "send_input".into(),
                arguments: json!({"agent_id":child_id,"prompt":"poll saved child command"}),
            },
            &ToolContext {
                workspace: workspace.clone(),
                cancel: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
    assert!(!queued.is_error, "{}", queued.content);
    sender.shutdown().await.unwrap();
    let followup = next_request(&requests).await;
    assert!(followup["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["content"] == "poll saved child command"));
    let final_request = next_request(&requests).await;
    let result = final_request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    let result: Value = serde_json::from_str(result).unwrap();
    assert_eq!(result["finished"], true);
    let deadline = Instant::now() + Duration::from_secs(5);
    while coordinator
        .list_children_snapshot_for_ui(&parent.id)
        .unwrap()[0]
        .agent
        .status
        != SubagentStatus::Completed
    {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(manager.list_sessions().unwrap().is_empty());
    assert!(Arc::ptr_eq(
        &manager,
        &app.native_command_sessions.manager(&child_id).unwrap()
    ));
    let output = NativeCommandOutputStore::new(sessions.clone());
    let summary = output
        .list(&child_id, &workspace.canonicalize().unwrap())
        .unwrap()
        .pop()
        .unwrap();
    assert!(summary.terminal.as_ref().unwrap().exit_observed);
    let page = output.read(&summary.header.binding, 0, 4096, 32).unwrap();
    let raw = page
        .chunks
        .iter()
        .filter(|chunk| chunk.stream == NativeCommandOutputStream::Stdout)
        .flat_map(|chunk| chunk.bytes.clone())
        .collect::<Vec<_>>();
    assert_eq!(raw, expected);
    // An idle Completed child can still own a live command. UI cancellation
    // must revoke it even though there is no active bridge worker to notify.
    let idle = manager
        .start(executable.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap()
        .id;
    app.open_session(&parent.id);
    app.refresh_sessions();
    app.poll_subagent_sessions();
    app.cancel_subagent(&child_id);
    assert!(
        app.native_command_sessions.manager(&child_id).is_none(),
        "{}",
        app.view.status
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while manager.control_snapshot(&idle).unwrap().exit.is_none() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        coordinator
            .list_children_snapshot_for_ui(&parent.id)
            .unwrap()[0]
            .agent
            .cancellation_requested
    );
    drop(sender);
    server.join().unwrap();
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(data_root);
    reopened.open_session(&child_id);
    reopened.view.show_side_pane = true;
    reopened.view.side_tab = SideTab::Activity;
    assert!(reopened
        .native_command_sessions
        .manager(&child_id)
        .is_none());
    apply(&mut reopened, "native_command_history_open");
    let deadline = Instant::now() + Duration::from_secs(5);
    while reopened.native_command_history_response.is_some() {
        reopened.poll_native_command_history();
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(reopened.view.native_command_history.error.is_none());
    let loaded = NativeCommandOutputStore::new(reopened.store.clone().unwrap())
        .read(&summary.header.binding, 0, 4096, 32)
        .unwrap();
    assert_eq!(loaded, page);
}
