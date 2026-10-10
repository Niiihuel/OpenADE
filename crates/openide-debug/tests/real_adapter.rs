//! Explicit opt-in only: these tests launch adapters and fixture programs.
use openide_debug::*;
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};

struct Driver {
    session: DebugSession,
    events: Vec<DebugEvent>,
}
impl Driver {
    fn wait(&mut self, predicate: impl Fn(&[DebugEvent]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.events.extend(self.session.drain());
            if predicate(&self.events) {
                return;
            }
            if self.session.is_closed() {
                // Closure can race the nonblocking drain above. Finish is
                // published after its terminal events, so drain once more.
                self.events.extend(self.session.drain());
                assert!(
                    predicate(&self.events),
                    "Adapter closed early: {:?}",
                    self.events
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Real adapter timed out: {:?}",
                self.events
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn stopped(&self) -> (u64, i64) {
        self.events
            .iter()
            .rev()
            .find_map(|event| match event {
                DebugEvent::Stopped {
                    stop_id,
                    thread_id: Some(thread),
                    ..
                } => Some((*stop_id, *thread)),
                _ => None,
            })
            .expect("Real adapter must identify its stopped thread")
    }
    fn step(&mut self, command: DebugCommand) {
        let (previous, _) = self.stopped();
        self.session.send(command).unwrap();
        self.wait(|events| {
            events.iter().any(
                |event| matches!(event, DebugEvent::Stopped {stop_id,..} if *stop_id > previous),
            )
        });
    }
    fn variables(&mut self) -> Vec<Variable> {
        let (_, thread_id) = self.stopped();
        let request = self
            .session
            .send(DebugCommand::StackTrace {
                thread_id,
                start_frame: 0,
                levels: 20,
            })
            .unwrap();
        self.wait(|events| events.iter().any(|event| matches!(event, DebugEvent::StackTrace {request_id,..} if *request_id == request)));
        let frame = self
            .events
            .iter()
            .find_map(|event| match event {
                DebugEvent::StackTrace {
                    request_id, frames, ..
                } if *request_id == request => frames.first().map(|frame| frame.id),
                _ => None,
            })
            .expect("Stopped fixture has a source frame");
        let request = self
            .session
            .send(DebugCommand::Scopes { frame_id: frame })
            .unwrap();
        self.wait(|events| events.iter().any(|event| matches!(event, DebugEvent::Scopes {request_id,..} if *request_id == request)));
        let references: Vec<_> = self
            .events
            .iter()
            .find_map(|event| match event {
                DebugEvent::Scopes {
                    request_id, scopes, ..
                } if *request_id == request => Some(
                    scopes
                        .iter()
                        .filter(|scope| !scope.expensive && scope.variables_reference > 0)
                        .map(|scope| scope.variables_reference)
                        .collect(),
                ),
                _ => None,
            })
            .unwrap();
        let mut values = Vec::new();
        for variables_reference in references {
            let request = self
                .session
                .send(DebugCommand::Variables {
                    variables_reference,
                    start: 0,
                    count: 100,
                })
                .unwrap();
            self.wait(|events| events.iter().any(|event| matches!(event, DebugEvent::Variables {request_id,..} if *request_id == request)));
            values.extend(
                self.events
                    .iter()
                    .find_map(|event| match event {
                        DebugEvent::Variables {
                            request_id,
                            variables,
                            ..
                        } if *request_id == request => Some(variables.clone()),
                        _ => None,
                    })
                    .unwrap(),
            );
        }
        values
    }
}

fn exercise(mut config: DebugConfig, program: &Path, breakpoint_line: u32) {
    config.breakpoints = vec![SourceBreakpoints {
        path: program.to_owned(),
        lines: vec![breakpoint_line],
    }];
    let mut driver = Driver {
        session: DebugSession::start(config).unwrap(),
        events: Vec::new(),
    };
    driver.wait(|events| {
        events
            .iter()
            .any(|event| matches!(event, DebugEvent::Stopped { .. }))
    });
    assert!(driver.events.iter().any(|event| matches!(event, DebugEvent::Breakpoints {breakpoints,..} if breakpoints.iter().any(|point| point.verified && point.requested_line == Some(breakpoint_line)))));
    let values = driver.variables();
    assert!(
        values
            .iter()
            .any(|value| value.name == "counter" && value.value == "3"),
        "{values:?}"
    );
    let (_, thread_id) = driver.stopped();
    driver.step(DebugCommand::StepIn { thread_id });
    driver.step(DebugCommand::Next { thread_id });
    driver.step(DebugCommand::StepOut { thread_id });
    let mut values = driver.variables();
    if !values
        .iter()
        .any(|value| value.name == "counter" && value.value == "4")
    {
        driver.step(DebugCommand::Next { thread_id });
        values = driver.variables();
    }
    assert!(
        values
            .iter()
            .any(|value| value.name == "counter" && value.value == "4"),
        "{values:?}"
    );
    driver
        .session
        .send(DebugCommand::Continue { thread_id })
        .unwrap();
    driver.wait(|events| {
        events
            .iter()
            .any(|event| matches!(event, DebugEvent::Disconnected { .. }))
    });
    let output: String = driver
        .events
        .iter()
        .filter_map(|event| match event {
            DebugEvent::Output { output, category } if category == "stdout" => {
                Some(output.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(output.contains("RESULT 4"), "{output}");
    assert!(driver
        .events
        .iter()
        .any(|event| matches!(event, DebugEvent::Terminated)));
    assert!(
        !driver
            .events
            .iter()
            .any(|event| matches!(event, DebugEvent::RequestFailed { .. })),
        "{:?}",
        driver.events
    );
}

#[test]
#[ignore = "launches debugpy and a Python fixture; set OPENIDE_DEBUG_ADAPTER_PYTHON"]
fn debugpy_real_breakpoint_locals_steps_output_and_cleanup() {
    let python = std::env::var_os("OPENIDE_DEBUG_ADAPTER_PYTHON")
        .expect("Set OPENIDE_DEBUG_ADAPTER_PYTHON to Python with debugpy installed");
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("main.py");
    std::fs::write(&program, "def compute(value):\n    result = value + 1\n    return result\n\ncounter = 3\ncounter = compute(counter)\nprint('RESULT', counter)\n").unwrap();
    let mut config = DebugConfig::new(
        python,
        root.path(),
        json!({"program":program,"cwd":root.path(),"console":"internalConsole","redirectOutput":true,"subProcess":false,"justMyCode":false}),
    );
    config.adapter_id = "python".into();
    config.args = vec!["-m".into(), "debugpy.adapter".into()];
    exercise(config, &program, 6);
}

#[test]
#[ignore = "launches lldb-dap and compiles a Rust fixture; set OPENIDE_DEBUG_ADAPTER_LLDB"]
fn lldb_real_rust_breakpoint_locals_steps_output_and_cleanup() {
    let adapter = std::env::var_os("OPENIDE_DEBUG_ADAPTER_LLDB")
        .expect("Set OPENIDE_DEBUG_ADAPTER_LLDB to lldb-dap");
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("main.rs");
    let binary = root.path().join("fixture");
    std::fs::write(&program, "fn compute(value: i32) -> i32 {\n    let result = value + 1;\n    result\n}\nfn main() {\n    let mut counter = 3;\n    counter = compute(counter);\n    println!(\"RESULT {}\", counter);\n}\n").unwrap();
    let compiler = std::process::Command::new("rustc")
        .args(["-g", "-C", "opt-level=0"])
        .arg(&program)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );
    let mut config = DebugConfig::new(
        adapter,
        root.path(),
        json!({"program":binary,"cwd":root.path(),"stopOnEntry":false,"disableASLR":false}),
    );
    config.adapter_id = "lldb".into();
    exercise(config, &program, 7);
}
