//! A phone driving the real window over the remote server: it pairs,
//! starts a task, reads the answer the model gave and answers what a task
//! off screen waits on.

use super::background_resume_app_tests::{app_at, Reply, ScriptedProvider};
use super::*;
use crate::app::remote_app::RemoteSettings;
use openide_harness::LocalTool;

/// The window's loop while a phone's request is out: serve it, run turns.
pub(crate) fn serve_until<T>(app: &mut AgentsWindow, phone: std::thread::JoinHandle<T>) -> T {
    let deadline = Instant::now() + DEADLINE;
    while !phone.is_finished() {
        assert!(Instant::now() < deadline, "the phone's request timed out: {}", app.view.status);
        app.serve_remote();
        app.poll_run();
        app.poll_parked_runs();
        std::thread::sleep(Duration::from_millis(3));
    }
    phone.join().unwrap()
}

/// The window's loop until the task's journal has an event `done` accepts.
pub(crate) fn run_until(app: &mut AgentsWindow, task: &str, done: impl Fn(&AgentEventKind) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        app.poll_run();
        app.poll_parked_runs();
        app.serve_remote();
        let events = app.store.as_ref().unwrap().load(task).unwrap().unwrap().events;
        if events.iter().any(|event| done(&event.kind)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the task never got there: {} · parked {} · active {} · journal {:?}",
            app.view.status,
            app.parked_runs.len(),
            app.active_run.is_some(),
            events.iter().map(|event| format!("{:?}", event.kind).chars().take(60).collect::<String>()).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(3));
    }
}

pub(crate) fn phone() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// The server's address and a paired phone's token.
pub(crate) fn paired(app: &mut AgentsWindow) -> (String, String) {
    app.set_remote_settings(RemoteSettings { enabled: true, port: 0, ..RemoteSettings::default() });
    let server = app.remote.server.as_ref().unwrap_or_else(|| panic!("{:?}", app.remote.error));
    let base = format!("https://127.0.0.1:{}", server.port());
    let invitation = server.invite("PC").unwrap();
    app.serve_remote();
    let address = base.clone();
    let token = serve_until(
        app,
        std::thread::spawn(move || {
            let paired: Value = phone()
                .post(format!("{address}/v2/pair"))
                .json(&json!({
                    "pairing_id": invitation.pairing_id,
                    "secret": invitation.secret,
                    "request_id": uuid::Uuid::new_v4().to_string(),
                    "device_name": "iPhone",
                }))
                .send()
                .unwrap()
                .json()
                .unwrap();
            paired["token"].as_str().unwrap().to_owned()
        }),
    );
    (base, token)
}

/// A native task the phone starts in the window's project.
fn start_task(app: &mut AgentsWindow, base: &str, token: &str, prompt: &'static str) -> String {
    let (base, token) = (base.to_owned(), token.to_owned());
    serve_until(
        app,
        std::thread::spawn(move || {
            let overview: Value = phone().get(format!("{base}/v2/overview")).bearer_auth(&token).send().unwrap().json().unwrap();
            let project = overview["projects"][0]["id"].as_str().unwrap().to_owned();
            let created: Value = phone()
                .post(format!("{base}/v2/tasks"))
                .bearer_auth(&token)
                .json(&json!({"request_id": uuid::Uuid::new_v4().to_string(), "project": project, "backend": "native", "prompt": prompt}))
                .send()
                .unwrap()
                .json()
                .unwrap();
            created["task"].as_str().unwrap_or_else(|| panic!("{created}")).to_owned()
        }),
    )
}

pub(crate) fn history(app: &mut AgentsWindow, base: &str, token: &str, task: &str) -> Value {
    let url = format!("{base}/v2/tasks/{task}/history");
    let token = token.to_owned();
    serve_until(app, std::thread::spawn(move || phone().get(url).bearer_auth(&token).send().unwrap().json().unwrap()))
}

#[test]
fn a_paired_phone_starts_a_task_in_the_window_and_reads_its_answer() {
    let root = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(|_| Reply::Text("RESPUESTA_PARA_EL_TELÉFONO"));
    let mut app = app_at(root.path(), &provider.endpoint);
    let (base, token) = paired(&mut app);
    let task = start_task(&mut app, &base, &token, "Hola desde el teléfono");
    // What the person had open stays open; the task goes on off screen.
    assert_ne!(app.view.selected_session.as_deref(), Some(task.as_str()));
    run_until(&mut app, &task, |kind| matches!(kind, AgentEventKind::TurnCompleted { .. }));
    let history = history(&mut app, &base, &token, &task).to_string();
    assert!(history.contains("Hola desde el teléfono"), "{history}");
    assert!(history.contains("RESPUESTA_PARA_EL_TELÉFONO"), "{history}");
    assert_eq!(provider.requests().len(), 1);
}

#[test]
fn a_phone_allows_what_a_task_off_screen_waits_on() {
    let root = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(|request| {
        if request["messages"].as_array().unwrap().iter().any(|message| message["role"] == "tool") {
            Reply::Text("ARCHIVO_ESCRITO")
        } else {
            Reply::Tool("write_file", json!({"path": "nuevo.txt", "content": "x"}))
        }
    });
    let mut app = app_at(root.path(), &provider.endpoint);
    let mut permissions = AgentToolSettings::default();
    permissions.set(LocalTool::WriteFile, true);
    assert!(app.commit_agent_tool_settings(permissions, "Writing asks first"));
    let (base, token) = paired(&mut app);
    let on_screen = app.view.selected_session.clone();
    let task = start_task(&mut app, &base, &token, "Escribí un archivo");
    run_until(&mut app, &task, |kind| matches!(kind, AgentEventKind::PermissionRequested { .. }));
    let waiting = history(&mut app, &base, &token, &task);
    let request = waiting["pending"][0]["request_id"].as_str().unwrap().to_owned();
    assert_eq!(waiting["pending"][0]["tool_name"], "write_file");

    let url = format!("{base}/v2/tasks/{task}/permissions/{request}");
    let phone_token = token.clone();
    let answer: Value = serve_until(
        &mut app,
        std::thread::spawn(move || {
            phone().post(url).bearer_auth(&phone_token).json(&json!({"decision": "allow"})).send().unwrap().json().unwrap()
        }),
    );
    assert_eq!(answer["decision"], "allow_once", "{answer}");
    run_until(&mut app, &task, |kind| matches!(kind, AgentEventKind::TurnCompleted { .. }));
    assert!(root.path().join("nuevo.txt").exists());
    assert!(history(&mut app, &base, &token, &task).to_string().contains("ARCHIVO_ESCRITO"));
    // The person's screen was left as it was.
    assert_eq!(app.view.selected_session, on_screen);

    // A second answer finds nothing waiting.
    let url = format!("{base}/v2/tasks/{task}/permissions/{request}");
    let late = serve_until(
        &mut app,
        std::thread::spawn(move || phone().post(url).bearer_auth(&token).json(&json!({"decision": "deny"})).send().unwrap().status()),
    );
    assert_eq!(late, 409);
}
