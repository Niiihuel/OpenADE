//! Actual Runner/HTTP/process acceptance at the destructive poll publication boundary.
#![cfg(unix)]

use async_trait::async_trait;
use openide_core::{
    AgentEvent, AgentEventKind, AgentRunRequest, CheckpointCompatibility, CheckpointFileStore,
    NativeCommandOutputStore, NativeCommandOutputStream, NativeEventJournal, ReplayRecovery,
    SessionStore, StoreError, ToolCall, TurnMode,
};
use openide_harness::{
    AgentError, AgentRunner, CommandIsolation, LocalTool, LocalTools, ModelBackend,
    ModelRequestCapabilities, OpenAiCompatibleBackend, PermissionChoice, PermissionMode,
    PermissionPolicy, PersistentCommandLimits, PersistentCommandManager, ResumeDisposition,
    ResumeEngine, ToolCatalog, ToolContext, ToolDefinition, ToolExecutor, ToolOutput,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "exact-local-output-recovery";
const PROMPT: &str = "Run the approved command once, then collect its final output";
const POLL_ID: &str = "drained-final-poll";
const SCRIPT: &str = "printf 'one\n' >> effects.log; printf 'stdout:caf\\303\\251:\\000\\377:end\n'; printf 'stderr:recovery\n' >&2";
const STDOUT: &[u8] = b"stdout:caf\xc3\xa9:\x00\xff:end\n";
const STDERR: &[u8] = b"stderr:recovery\n";

fn manager(
    sessions: Arc<SessionStore>,
    owner: &str,
    workspace: &Path,
) -> Arc<PersistentCommandManager> {
    Arc::new(
        PersistentCommandManager::new(
            workspace.to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_native_session_owner(owner.to_owned())
        .unwrap()
        .with_native_output_store(Arc::new(NativeCommandOutputStore::new(sessions)))
        .unwrap(),
    )
}

fn local(manager: Arc<PersistentCommandManager>) -> LocalTools {
    LocalTools::new()
        .with_tool_permission(LocalTool::RunCommand, true)
        .with_persistent_commands(manager)
}

// Wait for the genuine process terminal before calling the production adapter.
// This makes its first poll the destructive final drain rather than a timing race.
struct FinalPollTools {
    local: LocalTools,
    manager: Arc<PersistentCommandManager>,
}
#[async_trait]
impl ToolExecutor for FinalPollTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.local.definitions()
    }
    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        if call.id == POLL_ID {
            let id = call.arguments["session_id"].as_str().unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if self.manager.inspect_pending_output(id)?.exit.is_some() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "payload did not reach its terminal"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        self.local.execute(call, context).await
    }
}

struct CutJournal {
    sessions: Arc<SessionStore>,
    manager: Arc<PersistentCommandManager>,
    root: PathBuf,
    after_commit: bool,
}
impl NativeEventJournal for CutJournal {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        if let AgentEventKind::ToolFinished {
            tool_call_id,
            output,
            is_error,
        } = &event.kind
        {
            if tool_call_id == POLL_ID {
                assert!(!is_error);
                let delivered: Value = serde_json::from_str(output).unwrap();
                assert_eq!(delivered["finished"], true);
                assert!(
                    self.manager.list_sessions().unwrap().is_empty(),
                    "final drain must already have removed the live command"
                );
                if self.after_commit {
                    self.sessions.append_native_event(event)?;
                }
                let mut marker = std::fs::File::create(self.root.join("poll-drained")).unwrap();
                marker.write_all(output.as_bytes()).unwrap();
                marker.sync_all().unwrap();
                // Parent SIGKILLs this test process. No runtime or lease Drop runs.
                loop {
                    std::thread::park();
                }
            }
        }
        self.sessions.append_native_event(event)
    }
}

fn backend(endpoint: &str) -> Arc<dyn ModelBackend> {
    Arc::new(
        OpenAiCompatibleBackend::new(endpoint, None, MODEL)
            .with_capabilities(ModelRequestCapabilities {
                tool_calls: true,
                ..Default::default()
            })
            .unwrap()
            .with_timeout(Duration::from_secs(12)),
    )
}
fn policy() -> PermissionPolicy {
    PermissionPolicy::new()
        .with_tool("start_command", PermissionMode::Ask)
        .with_tool("poll_command", PermissionMode::Allow)
}
fn compatibility() -> CheckpointCompatibility {
    CheckpointCompatibility {
        provider: "local-output".into(),
        model: MODEL.into(),
        context_format_key: "chat-v1".into(),
    }
}

#[test]
#[ignore = "helper invoked and SIGKILLed only by durable_output_survives_final_poll_publication_cuts"]
fn output_recovery_runner_child() {
    let Ok(root) = std::env::var("OPENIDE_OUTPUT_RECOVERY_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let owner = std::env::var("OPENIDE_OUTPUT_RECOVERY_OWNER").unwrap();
    let endpoint = std::env::var("OPENIDE_OUTPUT_RECOVERY_ENDPOINT").unwrap();
    let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
    let lease = sessions.try_acquire_native_lease(&owner).unwrap();
    let admitted = sessions
        .load(&owner)
        .unwrap()
        .unwrap()
        .events
        .last()
        .unwrap()
        .sequence;
    let manager = manager(sessions.clone(), &owner, &root.join("workspace"));
    let tools = Arc::new(FinalPollTools {
        local: local(manager.clone()),
        manager: manager.clone(),
    });
    let runner = AgentRunner::new(backend(&endpoint), tools)
        .with_permission_policy(policy())
        .with_native_runtime_identity("local-output", &endpoint)
        .with_event_journal(Arc::new(CutJournal {
            sessions,
            manager,
            root: root.clone(),
            after_commit: std::env::var("OPENIDE_OUTPUT_RECOVERY_COMMITTED").unwrap() == "true",
        }));
    let handle = runner.start_with_lease(
        AgentRunRequest {
            session_id: Some(owner),
            sequence_start: admitted,
            workspace: root.join("workspace"),
            history: vec![],
            prompt: PROMPT.into(),
            mode: TurnMode::Build,
        },
        lease,
    );
    while let Ok(event) = handle.events.recv_timeout(Duration::from_secs(15)) {
        if let AgentEventKind::PermissionRequested { request_id, .. } = event.kind {
            assert!(handle.resolve_permission(&request_id, PermissionChoice::Allow));
        }
    }
    panic!(
        "runner failed before final poll cut: {:?}",
        handle.journal_error()
    );
}

fn request(socket: &TcpStream) -> Value {
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut bytes = vec![0; length.unwrap()];
    reader.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn response(socket: &mut TcpStream, delta: Value, reason: &str) {
    let body = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"output-response", "choices":[{"delta":delta,"finish_reason":reason}]})
    );
    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    socket.flush().unwrap();
}
fn tool_delta(id: &str, name: &str, args: Value) -> Value {
    json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
}

#[test]
fn durable_output_survives_final_poll_publication_cuts_without_reexecution_or_synthesized_tool_result(
) {
    use std::os::unix::process::ExitStatusExt;
    for after_commit in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sessions_root = root.path().join("sessions");
        let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
        let owner = sessions.create(&workspace, "native").unwrap().id;
        sessions
            .admit_native_turn(&owner, "one-command", PROMPT, MODEL, TurnMode::Build)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let server_count = count.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !server_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(12)))
                            .unwrap();
                        let body = request(&socket);
                        assert_eq!(body["model"], MODEL);
                        assert_eq!(body["stream"], true);
                        match server_count.fetch_add(1, Ordering::SeqCst) {
                            0 => response(&mut socket, tool_delta("start-once", "start_command", json!({"command":"/bin/sh", "args":["-c",SCRIPT]})), "tool_calls"),
                            1 => {
                                let start = body["messages"].as_array().unwrap().iter().find(|message| message["tool_call_id"] == "start-once").unwrap();
                                let result: Value = serde_json::from_str(start["content"].as_str().unwrap()).unwrap();
                                response(&mut socket, tool_delta(POLL_ID, "poll_command", json!({"session_id":result["session_id"],"wait_ms":0})), "tool_calls");
                            },
                            2 if after_commit => {
                                let saved = body["messages"].as_array().unwrap().iter().find(|message| message["tool_call_id"] == POLL_ID).unwrap();
                                let result: Value = serde_json::from_str(saved["content"].as_str().unwrap()).unwrap(); assert_eq!(result["finished"], true);
                                response(&mut socket, json!({"content":"The single process completed; retained output is confirmed."}), "stop");
                            },
                            _ => panic!("recovery contacted the provider or repeated a command unexpectedly"),
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "provider fixture timeout");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("provider accept: {error}"),
                }
            }
        });
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "output_recovery_runner_child",
                "--ignored",
                "--nocapture",
            ])
            .env("OPENIDE_OUTPUT_RECOVERY_ROOT", root.path())
            .env("OPENIDE_OUTPUT_RECOVERY_OWNER", &owner)
            .env("OPENIDE_OUTPUT_RECOVERY_ENDPOINT", &endpoint)
            .env(
                "OPENIDE_OUTPUT_RECOVERY_COMMITTED",
                after_commit.to_string(),
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if std::fs::read(root.path().join("poll-drained")).is_ok_and(|bytes| !bytes.is_empty())
            {
                break;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before poll drain"
            );
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("poll cut timeout");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        child.kill().unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
        let reopened = Arc::new(SessionStore::open(&sessions_root).unwrap());
        let output = NativeCommandOutputStore::new(reopened.clone());
        let summaries = output.list(&owner, &workspace).unwrap();
        assert_eq!(summaries.len(), 1);
        let summary = &summaries[0];
        let binding = &summary.header.binding;
        let mut cursor = 0;
        let mut stdout = Vec::<u8>::new();
        let mut stderr = Vec::<u8>::new();
        loop {
            let page = output.read(binding, cursor, 3, 2).unwrap();
            assert!(page.omitted_ranges.is_empty());
            assert!(page.summary.terminal.is_some());
            for chunk in &page.chunks {
                assert!(chunk.start >= cursor && chunk.end > chunk.start);
                match chunk.stream {
                    NativeCommandOutputStream::Stdout => stdout.extend(&chunk.bytes),
                    NativeCommandOutputStream::Stderr => stderr.extend(&chunk.bytes),
                    NativeCommandOutputStream::Terminal => panic!("pipe became terminal"),
                }
            }
            if page.next_cursor == page.summary.observed_end {
                break;
            }
            assert!(page.next_cursor > cursor, "non-progressing output page");
            cursor = page.next_cursor;
        }
        assert_eq!(stdout, STDOUT);
        assert_eq!(stderr, STDERR);
        assert_eq!(
            std::fs::read_to_string(workspace.join("effects.log")).unwrap(),
            "one\n"
        );
        let history_before = reopened.load(&owner).unwrap().unwrap().events;
        assert_eq!(history_before.iter().filter(|e| matches!(&e.kind, AgentEventKind::ToolFinished {tool_call_id,..} if tool_call_id == POLL_ID)).count(), usize::from(after_commit));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        let new_manager = manager(reopened.clone(), &owner, &workspace);
        assert!(new_manager.list_sessions().unwrap().is_empty());
        assert!(new_manager
            .inspect_pending_output(&binding.command_id)
            .is_err());
        let tools = Arc::new(local(new_manager));
        let engine = ResumeEngine::new(
            reopened.clone(),
            Arc::new(CheckpointFileStore::new(root.path().join("checkpoints")).unwrap()),
            compatibility(),
            backend(&endpoint),
            tools.clone(),
            ToolCatalog::new(tools.definitions()).unwrap(),
            policy(),
            None,
        )
        .with_native_runtime_identity("local-output", &endpoint);
        let lease = reopened.try_acquire_native_lease(&owner).unwrap();
        let initial = engine.inspect(&lease).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        if after_commit {
            assert!(matches!(
                initial.recovery,
                ReplayRecovery::ReadyForModel { next_step: 3 }
            ));
            assert_eq!(
                runtime
                    .block_on(engine.run_until_pause(&lease, CancellationToken::new()))
                    .unwrap()
                    .disposition,
                ResumeDisposition::Complete
            );
            assert_eq!(count.load(Ordering::SeqCst), 3);
        } else {
            assert!(
                matches!(&initial.recovery, ReplayRecovery::ToolOutcomeUncertain {tool_call} if tool_call.id == POLL_ID)
            );
            let step = runtime
                .block_on(engine.resume_step(&lease, CancellationToken::new()))
                .unwrap();
            assert!(matches!(
                step.disposition,
                ResumeDisposition::ToolOutcomeUncertain { .. }
            ));
            assert!(step.appended_events.is_empty());
            assert_eq!(
                reopened.load(&owner).unwrap().unwrap().events,
                history_before
            );
            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
        assert_eq!(
            output.list(&owner, &workspace).unwrap(),
            summaries,
            "model recovery must not rewrite capture"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("effects.log")).unwrap(),
            "one\n"
        );
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }
}
