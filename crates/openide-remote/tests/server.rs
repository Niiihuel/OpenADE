//! The server over real TLS: pairing, the task list, a command answered by
//! a stand-in window, the stream reading a journal as it grows, revocation.

use openide_core::{AgentEvent, AgentEventKind, SessionStore, ToolCall};
use openide_remote::bridge::{Agent, Bridge, Command, Delivery, Live, TaskRow};
use openide_remote::server::{Options, Server};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

fn client() -> reqwest::Client {
    // The phone pins the fingerprint; here only the server's own answers count.
    reqwest::Client::builder().danger_accept_invalid_certs(true).timeout(Duration::from_secs(10)).build().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_phone_reads_tasks_sends_a_message_follows_the_journal_and_loses_access_when_revoked() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("sessions");
    let store = SessionStore::open(&sessions).unwrap();
    let workspace = root.path().join("aurora");
    std::fs::create_dir_all(&workspace).unwrap();
    let session = store.create(&workspace, "native").unwrap();
    store
        .append_event(&session.id, &AgentEvent::new(&session.id, 1, AgentEventKind::TurnStarted { prompt: "PRIMER_EVENTO".into() }))
        .unwrap();

    let bridge = Bridge::new(|| {});
    bridge.publish(Live {
        tasks: vec![TaskRow {
            id: session.id.clone(),
            title: "hola".into(),
            project: workspace.clone(),
            backend: "native".into(),
            running: true,
            ..TaskRow::default()
        }],
        agents: vec![Agent { backend: "native".into(), name: "openADE".into(), startable: true }],
        projects: Vec::new(),
        desktop_focused: false,
    });
    // The window: answers every command and keeps the last one.
    let window = bridge.clone();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = seen.clone();
    std::thread::spawn(move || loop {
        for request in window.take_commands() {
            kept.lock().unwrap().push(request.command.clone());
            request.answer(Ok(json!({"delivered": "started"})));
        }
        std::thread::sleep(Duration::from_millis(5));
    });

    let server = Server::start(
        Options { root: root.path().join("remote"), sessions: sessions.clone(), port: 0, name: "PC de prueba".into(), public_address: None, upnp: false },
        bridge.clone(),
    )
    .unwrap();
    let base = format!("https://127.0.0.1:{}", server.port());
    let http = client();

    let identity: Value = http.get(format!("{base}/.well-known/openade")).send().await.unwrap().json().await.unwrap();
    assert_eq!(identity["fingerprint"], server.fingerprint());
    assert!(identity.get("name").is_none(), "the host name waits for a token: {identity}");
    assert_eq!(http.get(format!("{base}/v2/overview")).send().await.unwrap().status(), 401);

    // A code shown before stops working once another is made.
    let replaced = server.invite("PC de prueba").unwrap();
    let invitation = server.invite("PC de prueba").unwrap();
    assert_eq!(invitation.fingerprint, server.fingerprint());
    let stale = http
        .post(format!("{base}/v2/pair"))
        .json(&json!({"pairing_id": replaced.pairing_id, "secret": replaced.secret, "request_id": uuid::Uuid::new_v4().to_string(), "device_name": "Otro"}))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 401);
    let paired: Value = http
        .post(format!("{base}/v2/pair"))
        .json(&json!({
            "pairing_id": invitation.pairing_id,
            "secret": invitation.secret,
            "request_id": uuid::Uuid::new_v4().to_string(),
            "device_name": "iPhone de prueba",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = paired["token"].as_str().unwrap().to_owned();
    assert_eq!(server.devices().unwrap().len(), 1);

    let overview: Value =
        http.get(format!("{base}/v2/overview")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(overview["tasks"][0]["id"], session.id.as_str());
    assert_eq!(overview["tasks"][0]["state"], "running");
    assert_eq!(overview["projects"][0]["name"], "aurora");

    let sent: Value = http
        .post(format!("{base}/v2/tasks/{}/messages", session.id))
        .bearer_auth(&token)
        .json(&json!({"request_id": "m1", "prompt": "seguí con los tests"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sent["delivered"], "started");
    assert_eq!(
        seen.lock().unwrap().last(),
        Some(&Command::Send {
            task: session.id.clone(),
            prompt: "seguí con los tests".into(),
            files: Vec::new(),
            delivery: Delivery::Auto,
        })
    );

    let history: Value = http
        .get(format!("{base}/v2/tasks/{}/history", session.id))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(history["cursor"], 1);

    // The stream goes on after the last event the phone has.
    let mut stream = http
        .get(format!("{base}/v2/stream?task={}&after=1", session.id))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), 200);
    let call = ToolCall { id: "call-1".into(), name: "run_command".into(), arguments: json!({"command": "cargo test"}) };
    for (sequence, kind) in [
        (2, AgentEventKind::AssistantDelta { content: "Corro los tests".into() }),
        (3, AgentEventKind::PermissionRequested { request_id: "p1".into(), tool_call: call }),
    ] {
        store.append_event(&session.id, &AgentEvent::new(&session.id, sequence, kind)).unwrap();
    }
    let mut received = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(received.contains("Corro los tests") && received.contains("\"always\"")) {
        let chunk = tokio::time::timeout_at(deadline, stream.chunk()).await.expect("stream stalled").unwrap().unwrap();
        received.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(received.contains("event: overview"));
    assert!(received.contains("\"run_command\""));
    assert!(received.contains("\"always\":\"cargo test\""), "the phone sees what always allowing would allow: {received}");
    assert!(!received.contains("PRIMER_EVENTO"), "the phone already had the first event");
    assert_eq!(bridge.take_paired(), ["iPhone de prueba"]);

    // Losing access ends the stream that is open, not only new requests.
    let device = server.devices().unwrap()[0].id.clone();
    server.revoke(&device).unwrap();
    assert_eq!(http.get(format!("{base}/v2/overview")).bearer_auth(&token).send().await.unwrap().status(), 401);
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match stream.chunk().await {
                Ok(Some(_)) => continue,
                _ => return,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the revoked phone's stream kept going");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_address_gets_a_few_connections_and_a_few_pairing_attempts() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(
        Options { root: root.path().join("remote"), sessions: root.path().join("sessions"), port: 0, name: "PC".into(), public_address: None, upnp: false },
        Bridge::new(|| {}),
    )
    .unwrap();
    let address = format!("127.0.0.1:{}", server.port());
    // Connections that never finish TLS hold their place for a while.
    let held: Vec<_> = (0..8).map(|_| std::net::TcpStream::connect(&address).unwrap()).collect();
    let mut extra = std::net::TcpStream::connect(&address).unwrap();
    extra.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(std::io::Read::read(&mut extra, &mut byte).unwrap_or(0), 0, "a ninth connection is closed at once");
    drop(held);

    tokio::time::sleep(Duration::from_millis(200)).await;
    let http = client();
    let attempt = || {
        http.post(format!("https://{address}/v2/pair")).json(&json!({
            "pairing_id": uuid::Uuid::new_v4().to_string(),
            "secret": "0".repeat(64),
            "request_id": uuid::Uuid::new_v4().to_string(),
            "device_name": "x",
        }))
    };
    for _ in 0..10 {
        assert_eq!(attempt().send().await.unwrap().status(), 401);
    }
    assert_eq!(attempt().send().await.unwrap().status(), 429);
}
