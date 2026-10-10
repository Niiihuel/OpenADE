use openide_debug::*;
use serde_json::json;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn fixture(mode: &str) -> (tempfile::TempDir, DebugSession, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("main.py");
    std::fs::write(&program, "counter = 3\nprint(counter)\n").unwrap();
    let log = root.path().join("adapter.jsonl");
    let adapter = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/adapter.py");
    let mut config = DebugConfig::new("python3", root.path(), json!({"program":program}));
    config.args = vec![
        "-u".into(),
        adapter.to_string_lossy().into_owned(),
        mode.into(),
        log.to_string_lossy().into_owned(),
    ];
    config.breakpoints = vec![SourceBreakpoints {
        path: program,
        lines: vec![4, 7, 13],
    }];
    config.request_timeout = Duration::from_millis(700);
    config.shutdown_timeout = Duration::from_millis(350);
    (root, DebugSession::start(config).unwrap(), log)
}
fn wait(
    session: &DebugSession,
    events: &mut Vec<DebugEvent>,
    predicate: impl Fn(&[DebugEvent]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        events.extend(session.drain());
        if predicate(events) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "DAP events timed out: {events:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn stopped(session: &DebugSession, events: &mut Vec<DebugEvent>) {
    wait(session, events, |events| {
        events
            .iter()
            .any(|event| matches!(event, DebugEvent::Stopped { .. }))
    });
}
fn log(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}
fn inspect(session: &DebugSession, events: &mut Vec<DebugEvent>) -> u64 {
    let stack = session
        .send(DebugCommand::StackTrace {
            thread_id: 1,
            start_frame: 0,
            levels: 20,
        })
        .unwrap();
    wait(session, events, |events| {
        events.iter().any(|event| matches!(event, DebugEvent::StackTrace { request_id, .. } if *request_id == stack))
    });
    let scopes = session.send(DebugCommand::Scopes { frame_id: 10 }).unwrap();
    wait(session, events, |events| {
        events.iter().any(
            |event| matches!(event, DebugEvent::Scopes { request_id, .. } if *request_id == scopes),
        )
    });
    session
        .send(DebugCommand::Variables {
            variables_reference: 100,
            start: 0,
            count: 30,
        })
        .unwrap()
}

#[test]
fn real_subprocess_handshake_verified_coordinates_inspection_steps_and_stop() {
    let (_root, session, path) = fixture("normal");
    let mut events = Vec::new();
    stopped(&session, &mut events);
    let points = events
        .iter()
        .find_map(|event| match event {
            DebugEvent::Breakpoints { breakpoints, .. } => Some(breakpoints),
            _ => None,
        })
        .unwrap();
    assert!(points[0].verified);
    assert_eq!(
        (points[1].requested_line, points[1].line),
        (Some(7), Some(8))
    );
    assert!(!points[2].verified);
    let variables_request = inspect(&session, &mut events);
    wait(&session, &mut events, |events| {
        events.iter().any(|event| matches!(event, DebugEvent::Variables { request_id,variables,.. } if *request_id == variables_request && variables.iter().any(|v| v.name == "counter" && v.value == "3")))
    });
    for command in [
        DebugCommand::Next { thread_id: 1 },
        DebugCommand::StepIn { thread_id: 1 },
        DebugCommand::StepOut { thread_id: 1 },
    ] {
        let before = events
            .iter()
            .filter(|event| matches!(event, DebugEvent::Stopped { .. }))
            .count();
        session.send(command).unwrap();
        wait(&session, &mut events, |events| {
            events
                .iter()
                .filter(|event| matches!(event, DebugEvent::Stopped { .. }))
                .count()
                > before
        });
    }
    session
        .send(DebugCommand::Continue { thread_id: 1 })
        .unwrap();
    wait(&session, &mut events, |events| {
        events
            .iter()
            .any(|event| matches!(event, DebugEvent::Disconnected { .. }))
    });
    assert!(events
        .iter()
        .any(|event| matches!(event, DebugEvent::Exited { exit_code: Some(0) })));
    assert!(events
        .iter()
        .any(|event| matches!(event, DebugEvent::Output {output,..} if output == "done\n")));
    let trace = log(&path);
    let commands: Vec<_> = trace
        .iter()
        .filter_map(|line| line["command"].as_str())
        .collect();
    assert_eq!(
        &commands[..4],
        [
            "initialize",
            "launch",
            "setBreakpoints",
            "configurationDone"
        ]
    );
    assert!(commands.contains(&"disconnect"));
    assert!(trace.iter().any(|line| line["stdin_closed"] == true));
}

#[test]
fn superseded_breakpoints_and_old_pause_references_never_reach_the_new_state() {
    let (_root, session, _) = fixture("stale_breakpoints");
    let mut events = Vec::new();
    stopped(&session, &mut events);
    let source = events
        .iter()
        .find_map(|event| match event {
            DebugEvent::Breakpoints { source, .. } => Some(source.clone()),
            _ => None,
        })
        .unwrap();
    let old = session
        .send(DebugCommand::SetBreakpoints(SourceBreakpoints {
            path: source.clone(),
            lines: vec![40],
        }))
        .unwrap();
    let new = session
        .send(DebugCommand::SetBreakpoints(SourceBreakpoints {
            path: source,
            lines: vec![41],
        }))
        .unwrap();
    wait(&session, &mut events, |events| {
        events.iter().any(|event| matches!(event, DebugEvent::RequestFailed { request_id: Some(id), .. } if *id == old))
    });
    assert!(events.iter().any(|event| matches!(event, DebugEvent::Breakpoints { request_id: Some(id), breakpoints, .. } if *id == new && breakpoints[0].line == Some(41))));
    assert!(!events.iter().any(
        |event| matches!(event, DebugEvent::Breakpoints { request_id: Some(id), .. } if *id == old)
    ));
    assert!(!events.iter().any(|event| matches!(event, DebugEvent::BreakpointChanged {breakpoint,..} if breakpoint.id == Some(1040))));
    session.send(DebugCommand::Stop).unwrap();

    let (_root, session, _) = fixture("late_variables");
    let mut events = Vec::new();
    stopped(&session, &mut events);
    let stale = inspect(&session, &mut events);
    session
        .send(DebugCommand::Continue { thread_id: 1 })
        .unwrap();
    wait(&session, &mut events, |events| {
        events.iter().any(|event| matches!(event, DebugEvent::RequestFailed {request_id:Some(id), ..} if *id == stale))
    });
    assert!(!events.iter().any(
        |event| matches!(event, DebugEvent::Variables { request_id, .. } if *request_id == stale)
    ));
    let invalid = session.send(DebugCommand::Scopes { frame_id: 10 }).unwrap();
    wait(&session, &mut events, |events| {
        events.iter().any(|event| matches!(event, DebugEvent::RequestFailed { request_id:Some(id), .. } if *id == invalid))
    });
    session.send(DebugCommand::Stop).unwrap();
}

#[test]
fn pause_threadless_stops_absent_capabilities_and_reverse_requests_are_real() {
    for mode in ["running", "threadless", "no_config", "reverse"] {
        let (_root, session, path) = fixture(mode);
        let mut events = Vec::new();
        stopped(&session, &mut events);
        wait(&session, &mut events, |events| {
            events.iter().any(|event| matches!(event, DebugEvent::Threads { threads, .. } if threads.iter().any(|t| t.id == 1)))
        });
        if mode == "running" {
            session
                .send(DebugCommand::Continue { thread_id: 1 })
                .unwrap();
            wait(&session, &mut events, |events| {
                events
                    .iter()
                    .any(|event| matches!(event, DebugEvent::Continued { .. }))
            });
            session.send(DebugCommand::Pause { thread_id: 1 }).unwrap();
            wait(&session, &mut events, |events| {
                events.iter().any(|event| matches!(event, DebugEvent::Stopped { reason, .. } if reason == "pause"))
            });
        }
        if mode == "threadless" {
            assert!(events.iter().any(|event| matches!(
                event,
                DebugEvent::Stopped {
                    thread_id: None,
                    ..
                }
            )));
        }
        if mode == "no_config" {
            assert!(!log(&path)
                .iter()
                .any(|line| line["command"] == "configurationDone"));
        }
        if mode == "reverse" {
            assert!(events.iter().any(|event| matches!(event, DebugEvent::RequestFailed { command, .. } if command == "runInTerminal")));
            assert!(log(&path)
                .iter()
                .any(|line| line["type"] == "response" && line["success"] == false));
        }
        session.send(DebugCommand::Stop).unwrap();
        wait(&session, &mut events, |events| {
            events
                .iter()
                .any(|event| matches!(event, DebugEvent::Disconnected { .. }))
        });
    }
}

#[test]
fn malformed_timeout_rejection_and_backpressure_close_without_false_running() {
    for mode in [
        "malformed",
        "timeout",
        "initialize_fail",
        "missing_initialized",
    ] {
        let (_root, session, _) = fixture(mode);
        let mut events = Vec::new();
        wait(&session, &mut events, |events| {
            events
                .iter()
                .any(|event| matches!(event, DebugEvent::Disconnected { .. }))
        });
        assert!(!events
            .iter()
            .any(|event| matches!(event, DebugEvent::State(DebugState::Running))));
    }
    let (_root, session, _) = fixture("flood");
    let deadline = Instant::now() + Duration::from_secs(8);
    while !session.is_closed() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let events = session.drain();
    assert!(events.iter().any(
        |event| matches!(event, DebugEvent::Disconnected {reason} if reason.contains("budget"))
    ));
}

#[test]
fn unicode_output_boundaries_and_rejected_inspection_preserve_state() {
    let (_root, session, _) = fixture("unicode");
    let mut events = Vec::new();
    stopped(&session, &mut events);
    let output: String = events
        .iter()
        .filter_map(|event| match event {
            DebugEvent::Output { category, output } if category == "stdout" => {
                Some(output.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(output, format!("{}🦀 café", "a".repeat(16383)));
    session.send(DebugCommand::Stop).unwrap();
    let (_root, session, _) = fixture("scope_fail");
    let mut events = Vec::new();
    stopped(&session, &mut events);
    let stack = session
        .send(DebugCommand::StackTrace {
            thread_id: 1,
            start_frame: 0,
            levels: 20,
        })
        .unwrap();
    wait(&session, &mut events, |events| {
        events.iter().any(
            |event| matches!(event, DebugEvent::StackTrace {request_id,..} if *request_id == stack),
        )
    });
    let scopes = session.send(DebugCommand::Scopes { frame_id: 10 }).unwrap();
    wait(&session, &mut events, |events| {
        events.iter().any(|event| matches!(event, DebugEvent::RequestFailed {request_id:Some(id),..} if *id == scopes))
    });
    assert!(!events
        .iter()
        .any(|event| matches!(event, DebugEvent::Scopes {request_id,..} if *request_id == scopes)));
    assert!(!session.is_closed());
}

#[test]
fn drop_and_stop_do_not_wait_for_a_noncooperative_adapter() {
    let (_root, session, _) = fixture("ignore_disconnect");
    let mut events = Vec::new();
    stopped(&session, &mut events);
    let now = Instant::now();
    session.send(DebugCommand::Stop).unwrap();
    assert!(now.elapsed() < Duration::from_millis(200));
    wait(&session, &mut events, |events| {
        events
            .iter()
            .any(|event| matches!(event, DebugEvent::Disconnected { .. }))
    });
    let (_root, session, path) = fixture("ignore_disconnect");
    let mut events = Vec::new();
    stopped(&session, &mut events);
    let pid = log(&path)[0]["pid"].as_i64().unwrap();
    let now = Instant::now();
    drop(session);
    assert!(now.elapsed() < Duration::from_millis(200));
    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(pid as i32, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "dropped session left adapter alive"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn invalidated_event_retires_pending_responses_and_allows_fresh_inspection() {
    let (_root, session, _) = fixture("invalidated");
    let mut events = Vec::new();
    stopped(&session, &mut events);
    let old = inspect(&session, &mut events);
    wait(&session, &mut events, |events| {
        events.iter().any(|event| matches!(event, DebugEvent::RequestFailed { request_id: Some(id), .. } if *id == old))
    });
    assert!(events.iter().any(|event| matches!(event, DebugEvent::Invalidated { areas, thread_id: Some(1), stack_frame_id: Some(10) } if areas == &["stacks", "variables"])));
    assert!(!events
        .iter()
        .any(|event| matches!(event, DebugEvent::Variables {request_id,..} if *request_id == old)));
    let fresh = inspect(&session, &mut events);
    wait(&session, &mut events, |events| {
        events.iter().any(|event| matches!(event, DebugEvent::Variables {request_id,variables,..} if *request_id == fresh && variables.iter().any(|v| v.name == "counter")))
    });
    session.send(DebugCommand::Stop).unwrap();
    wait(&session, &mut events, |events| {
        events
            .iter()
            .any(|event| matches!(event, DebugEvent::Disconnected { .. }))
    });
}

#[test]
fn maximum_source_configuration_is_sent_in_bounded_batches_before_launch_completes() {
    let root = tempfile::tempdir().unwrap();
    let log_path = root.path().join("adapter.jsonl");
    let adapter = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/adapter.py");
    let mut config = DebugConfig::new(
        "python3",
        root.path(),
        json!({"program":root.path().join("main.py")}),
    );
    config.args = vec![
        "-u".into(),
        adapter.to_string_lossy().into_owned(),
        "normal".into(),
        log_path.to_string_lossy().into_owned(),
    ];
    config.breakpoints = (0..128)
        .map(|index| SourceBreakpoints {
            path: root.path().join(format!("source_{index}.py")),
            lines: vec![4],
        })
        .collect();
    let session = DebugSession::start(config).unwrap();
    let mut events = Vec::new();
    stopped(&session, &mut events);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, DebugEvent::Breakpoints { .. }))
            .count(),
        128
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, DebugEvent::RequestFailed { .. })),
        "{events:?}"
    );
    let trace = log(&log_path);
    let done = trace
        .iter()
        .position(|entry| entry["command"] == "configurationDone")
        .unwrap();
    assert_eq!(
        trace[..done]
            .iter()
            .filter(|entry| entry["command"] == "setBreakpoints")
            .count(),
        128
    );
    session.send(DebugCommand::Stop).unwrap();
    wait(&session, &mut events, |events| {
        events
            .iter()
            .any(|event| matches!(event, DebugEvent::Disconnected { .. }))
    });
}

#[test]
fn final_thread_exit_updates_the_cache_without_querying_a_closed_debuggee() {
    let (_root, session, path) = fixture("thread_exit");
    let mut events = Vec::new();
    stopped(&session, &mut events);
    wait(&session, &mut events, |events| {
        events
            .iter()
            .any(|event| matches!(event, DebugEvent::Threads {threads,..} if !threads.is_empty()))
    });
    session
        .send(DebugCommand::Continue { thread_id: 1 })
        .unwrap();
    wait(&session, &mut events, |events| {
        events
            .iter()
            .any(|event| matches!(event, DebugEvent::Disconnected { .. }))
    });
    assert!(events
        .iter()
        .any(|event| matches!(event, DebugEvent::Threads {threads,..} if threads.is_empty())));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, DebugEvent::RequestFailed { .. })),
        "{events:?}"
    );
    let trace = log(&path);
    let resumed = trace
        .iter()
        .position(|entry| entry["command"] == "continue")
        .unwrap();
    assert!(!trace[resumed + 1..]
        .iter()
        .any(|entry| entry["command"] == "threads"));
}
