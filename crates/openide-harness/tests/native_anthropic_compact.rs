//! Real loopback HTTP acceptance of the summary request's own token preflight.
//! No credentials, external providers, or model-name capacity rules are used.

use openide_core::{
    replay_native, AgentEvent, AgentEventKind, AgentMessage, CheckpointBudgetSnapshot,
    CheckpointCompatibility, CheckpointFileStore, SessionStore, TurnMode,
};
use openide_harness::{
    AgentError, AnthropicMessagesBackend, CheckpointCoordinator, CheckpointCoordinatorError,
    CompactLimits, CompactSummary,
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MODEL: &str = "fixture/exact-summary-model";

#[derive(Clone)]
struct Request {
    target: String,
    body: Value,
}

struct Server {
    endpoint: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    fn new(count: Value, hold_count: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = requests.clone();
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let mut socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let request = read_request(&mut socket);
                let target = request.target.clone();
                seen.lock().unwrap().push(request);
                if target == "/v1/messages/count_tokens" && hold_count {
                    while !stopped.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    return;
                }
                let body = match target.as_str() {
                    "/v1/messages/count_tokens" => count.clone(),
                    "/v1/messages" => json!({
                        "type":"message", "id":"msg_actual_summary", "role":"assistant",
                        "model":MODEL, "content":[{"type":"text","text":summary().to_string()}],
                        "stop_reason":"end_turn", "stop_sequence":null,
                        "usage":{"input_tokens":count["input_tokens"],"output_tokens":30},
                    }),
                    _ => panic!("unexpected target: {target}"),
                }
                .to_string();
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(headers.as_bytes());
                let _ = socket.write_all(body.as_bytes());
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn read_request(socket: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut buffer = [0_u8; 8192];
        let read = socket.read(&mut buffer).unwrap();
        assert!(read > 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(position) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            break position + 4;
        }
        assert!(bytes.len() < 256 * 1024);
    };
    let headers = std::str::from_utf8(&bytes[..boundary]).unwrap();
    let target = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .into();
    let length = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .unwrap()
        .1
        .trim()
        .parse::<usize>()
        .unwrap();
    assert!(length <= 2 * 1024 * 1024);
    while bytes.len() < boundary + length {
        let mut buffer = [0_u8; 8192];
        let read = socket.read(&mut buffer).unwrap();
        assert!(read > 0, "request ended before body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    Request {
        target,
        body: serde_json::from_slice(&bytes[boundary..boundary + length]).unwrap(),
    }
}

fn summary() -> Value {
    json!({
        "objective":"Preserve the confirmed task",
        "decisions":["Read payload.txt once"],
        "files":[{"path":"payload.txt","state":"read; unchanged"}],
        "current_state":"Previous turn completed; summary was requested explicitly",
        "constraints":["No process restoration or tool replay"],
        "remaining_work":["Resume from the published checkpoint"],
    })
}

fn compatibility() -> CheckpointCompatibility {
    CheckpointCompatibility {
        provider: "own-messages-fixture".into(),
        model: MODEL.into(),
        context_format_key: "anthropic-messages-v1".into(),
    }
}

fn budget() -> CheckpointBudgetSnapshot {
    CheckpointBudgetSnapshot {
        input_limit_tokens: Some(200),
        context_window_tokens: 0,
        estimated_input_tokens: 100_000,
        observed_input_tokens: Some(100_000),
        output_reserve_tokens: 4096,
        tool_schema_reserve_tokens: 0,
        limits_unverified: false,
    }
}

fn completed_session(sessions: &SessionStore, workspace: &std::path::Path) -> String {
    let session = sessions.create(workspace, "native").unwrap();
    let _lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "completed-source",
            "Read payload.txt once",
            MODEL,
            TurnMode::Build,
        )
        .unwrap();
    let mut sequence = admission.sequence;
    for kind in [
        AgentEventKind::TurnStarted {
            prompt: "Read payload.txt once".into(),
        },
        AgentEventKind::ModelRequestStarted {
            step: 1,
            model: MODEL.into(),
        },
        AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant("The local file is unchanged", vec![]),
            response_id: Some("previous-confirmed-response".into()),
            usage: None,
        },
        AgentEventKind::AssistantMessage {
            content: "The local file is unchanged".into(),
        },
        AgentEventKind::TurnCompleted {
            response: "The local file is unchanged".into(),
            steps: 1,
        },
    ] {
        sequence += 1;
        sessions
            .append_native_event(&AgentEvent::new(&session.id, sequence, kind))
            .unwrap();
    }
    session.id
}

#[tokio::test]
async fn compact_count_rejects_independent_input_and_combined_window_before_messages_or_candidate()
{
    for (count, budget) in [
        (201, budget()),
        (
            185,
            CheckpointBudgetSnapshot {
                tool_schema_reserve_tokens: 10,
                ..budget()
            },
        ),
        (
            123,
            CheckpointBudgetSnapshot {
                input_limit_tokens: None,
                context_window_tokens: 4300,
                ..budget()
            },
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let id = completed_session(&sessions, root.path());
        let before = sessions.load(&id).unwrap().unwrap().events;
        let checkpoints_root = root.path().join("checkpoints");
        let checkpoints = Arc::new(CheckpointFileStore::new(&checkpoints_root).unwrap());
        let server = Server::new(json!({"input_tokens":count}), false);
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints,
            Arc::new(AnthropicMessagesBackend::new(&server.endpoint, None, MODEL)),
            compatibility(),
            budget,
        );
        let lease = sessions.try_acquire_native_lease(&id).unwrap();
        assert!(matches!(
            coordinator.compact(&lease, CancellationToken::new()).await,
            Err(CheckpointCoordinatorError::Compact(AgentError::Limit(_)))
        ));
        assert_eq!(sessions.load(&id).unwrap().unwrap().events, before);
        assert_eq!(
            std::fs::read_dir(checkpoints_root.join("checkpoints"))
                .unwrap()
                .count(),
            0
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].target, "/v1/messages/count_tokens");
        assert!(requests[0].body["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("faithful checkpoint"));
        assert!(requests[0].body["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("committed_history_json"));
        assert!(requests[0].body.get("tools").is_none());
    }
}

#[tokio::test]
async fn compact_exact_count_allows_independent_input_and_compact_region_then_reopens_checkpoint() {
    // 180 is the automatic compaction region of input 200, but a summary
    // request is allowed until its own hard admission limit of 190.
    for count in [123, 180] {
        let root = tempfile::tempdir().unwrap();
        let sessions_root = root.path().join("sessions");
        let checkpoints_root = root.path().join("checkpoints");
        let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
        let id = completed_session(&sessions, root.path());
        let before_sequence = sessions
            .load(&id)
            .unwrap()
            .unwrap()
            .events
            .last()
            .unwrap()
            .sequence;
        let checkpoints = Arc::new(CheckpointFileStore::new(&checkpoints_root).unwrap());
        let server = Server::new(json!({"input_tokens":count}), false);
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints,
            Arc::new(AnthropicMessagesBackend::new(&server.endpoint, None, MODEL)),
            compatibility(),
            budget(),
        );
        let lease = sessions.try_acquire_native_lease(&id).unwrap();
        let committed = coordinator
            .compact(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(committed.checkpoint.covered_event_sequence, before_sequence);
        assert_eq!(
            serde_json::from_str::<Value>(&committed.checkpoint.summary).unwrap(),
            summary()
        );
        let parsed: CompactSummary = serde_json::from_str(&committed.checkpoint.summary).unwrap();
        assert_eq!(parsed.objective, "Preserve the confirmed task");
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].target, "/v1/messages/count_tokens");
        assert_eq!(requests[1].target, "/v1/messages");
        assert_eq!(requests[1].body["max_tokens"], 4096);
        let mut completion_body = requests[1].body.clone();
        completion_body
            .as_object_mut()
            .unwrap()
            .remove("max_tokens");
        completion_body.as_object_mut().unwrap().remove("stream");
        assert_eq!(
            requests[0].body, completion_body,
            "the preflight counts the exact summary payload"
        );
        drop(lease);
        drop(coordinator);
        drop(sessions);
        let sessions = SessionStore::open(&sessions_root).unwrap();
        let checkpoints = CheckpointFileStore::new(&checkpoints_root).unwrap();
        assert_eq!(
            checkpoints
                .load_after_commit(Some(&committed.reference))
                .unwrap(),
            Some(committed.checkpoint.clone())
        );
        let record = sessions.load(&id).unwrap().unwrap();
        let replay =
            replay_native(&id, &record.events, Some(&checkpoints), &compatibility()).unwrap();
        assert!(replay.can_start_new_turn);
        assert_eq!(replay.checkpoint, Some(committed.checkpoint));
    }
}

#[tokio::test]
async fn compact_invalid_provider_count_never_falls_back_or_publishes_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let id = completed_session(&sessions, root.path());
    let before = sessions.load(&id).unwrap().unwrap().events;
    let checkpoints_root = root.path().join("checkpoints");
    let server = Server::new(json!({"input_tokens":-1}), false);
    let coordinator = CheckpointCoordinator::new(
        sessions.clone(),
        Arc::new(CheckpointFileStore::new(&checkpoints_root).unwrap()),
        Arc::new(AnthropicMessagesBackend::new(&server.endpoint, None, MODEL)),
        compatibility(),
        budget(),
    );
    let lease = sessions.try_acquire_native_lease(&id).unwrap();
    assert!(matches!(
        coordinator.compact(&lease, CancellationToken::new()).await,
        Err(CheckpointCoordinatorError::Compact(AgentError::Model(_)))
    ));
    assert_eq!(sessions.load(&id).unwrap().unwrap().events, before);
    assert_eq!(
        std::fs::read_dir(checkpoints_root.join("checkpoints"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(server.requests().len(), 1);
    assert_eq!(server.requests()[0].target, "/v1/messages/count_tokens");
}

#[tokio::test]
async fn compact_preflight_timeout_and_cancel_block_summary_and_checkpoint() {
    for cancelled in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let id = completed_session(&sessions, root.path());
        let before = sessions.load(&id).unwrap().unwrap().events;
        let checkpoints_root = root.path().join("checkpoints");
        let server = Server::new(json!({"input_tokens":123}), true);
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            Arc::new(CheckpointFileStore::new(&checkpoints_root).unwrap()),
            Arc::new(AnthropicMessagesBackend::new(&server.endpoint, None, MODEL)),
            compatibility(),
            budget(),
        )
        .with_compact_limits(CompactLimits {
            timeout: if cancelled {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(100)
            },
            ..Default::default()
        });
        let lease = sessions.try_acquire_native_lease(&id).unwrap();
        let cancel = CancellationToken::new();
        let count_requests = server.requests.clone();
        let stop = cancel.clone();
        let cancel_task = tokio::spawn(async move {
            if cancelled {
                tokio::time::timeout(Duration::from_secs(1), async {
                    while count_requests.lock().unwrap().is_empty() {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                })
                .await
                .unwrap();
                stop.cancel();
            }
        });
        let result = coordinator.compact(&lease, cancel).await;
        cancel_task.await.unwrap();
        if cancelled {
            assert!(matches!(
                result,
                Err(CheckpointCoordinatorError::Compact(AgentError::Cancelled))
            ));
        } else {
            assert!(matches!(
                result,
                Err(CheckpointCoordinatorError::Compact(AgentError::Limit(_)))
            ));
        }
        assert_eq!(sessions.load(&id).unwrap().unwrap().events, before);
        assert_eq!(
            std::fs::read_dir(checkpoints_root.join("checkpoints"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(server.requests().len(), 1);
        assert_eq!(server.requests()[0].target, "/v1/messages/count_tokens");
    }
}
