use super::*;
use std::time::Instant as StdInstant;

fn manager(root: &std::path::Path, terminal: bool) -> PersistentCommandManager {
    let manager = PersistentCommandManager::new(
        root.to_path_buf(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap();
    if terminal {
        manager.with_host_terminal()
    } else {
        manager
    }
}

fn sh(script: &str) -> Vec<String> {
    vec!["-c".into(), script.into()]
}

/// Collect the host tab's copy until `done` or the deadline.
fn tab_output(
    terminal: &PersistentTerminal,
    done: impl Fn(&str, &Option<PersistentCommandExit>) -> bool,
) -> (String, Option<PersistentCommandExit>) {
    let deadline = StdInstant::now() + Duration::from_secs(10);
    let (mut text, mut exit) = (String::new(), None);
    while !done(&text, &exit) {
        assert!(StdInstant::now() < deadline, "tab saw only: {text:?}");
        match terminal.try_event() {
            Some(PersistentTerminalEvent::Output(bytes)) => {
                text.push_str(&String::from_utf8_lossy(&bytes))
            }
            Some(PersistentTerminalEvent::Exited(status)) => exit = Some(status),
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    (text, exit)
}

#[tokio::test]
async fn host_terminal_is_a_tty_with_the_same_cleared_environment_and_shared_with_the_agent() {
    let root = tempfile::tempdir().unwrap();
    let manager = manager(root.path(), true);
    let cancel = CancellationToken::new();
    let started = manager
        .start(
            "/bin/sh",
            &sh("test -t 0 && test -t 1 && test -t 2 && echo TTY=$TERM; echo HOME=[$HOME]; read line; echo GOT:$line; read again; echo AGENT:$again"),
            &cancel,
        )
        .unwrap();
    assert!(manager.list_sessions().unwrap()[0].pty);
    let terminal = manager.attach_terminal(&started.id).unwrap();
    assert!(terminal.interactive());
    let (text, _) = tab_output(&terminal, |text, _| text.contains("HOME=["));
    // A controlling terminal and TERM are the only additions.
    assert!(text.contains("TTY=xterm-256color"), "{text:?}");
    assert!(text.contains("HOME=[]"), "{text:?}");
    terminal.resize(100, 40).unwrap();
    // The user types in the tab; the agent's own tool writes to the same process.
    terminal.write(b"hola\n").unwrap();
    tab_output(&terminal, |text, _| text.contains("GOT:hola"));
    manager
        .write_stdin(&started.id, b"mundo\n", false, &cancel)
        .await
        .unwrap();
    let (_, exit) = tab_output(&terminal, |_, exit| exit.is_some());
    assert_eq!(exit, Some(PersistentCommandExit::Exited { code: Some(0) }));
    // The tab never consumed what the agent polls.
    let mut polled = String::new();
    loop {
        let snapshot = manager
            .poll(&started.id, Duration::from_secs(5), &cancel)
            .await
            .unwrap();
        for event in &snapshot.events {
            if let PersistentCommandEvent::Output { stream, bytes } = event {
                assert_eq!(*stream, PersistentCommandStream::Terminal);
                polled.push_str(&String::from_utf8_lossy(bytes));
            }
        }
        if snapshot.finished {
            break;
        }
    }
    for expected in ["TTY=xterm-256color", "GOT:hola", "AGENT:mundo"] {
        assert!(polled.contains(expected), "{polled:?}");
    }
    // A late tab still gets the retained tail and the exit, if the session
    // has not been retired by the agent's final poll.
    assert!(manager.attach_terminal(&started.id).is_err());
}

#[tokio::test]
async fn interrupt_from_the_tab_and_termination_end_the_whole_tree() {
    let root = tempfile::tempdir().unwrap();
    let manager = manager(root.path(), true);
    let cancel = CancellationToken::new();
    let alive = |pid: i32| unsafe { libc::kill(pid, 0) == 0 };
    let child_pid = |text: &str| -> i32 {
        text.lines()
            .find_map(|line| line.trim().strip_prefix("CHILD=")?.parse().ok())
            .unwrap()
    };
    // Ctrl+C typed by the user reaches the foreground process group.
    let started = manager
        .start("/bin/sh", &sh("echo READY; sleep 300"), &cancel)
        .unwrap();
    let terminal = manager.attach_terminal(&started.id).unwrap();
    tab_output(&terminal, |text, _| text.contains("READY"));
    terminal.write(&[0x03]).unwrap();
    let (_, exit) = tab_output(&terminal, |_, exit| exit.is_some());
    assert!(matches!(exit, Some(PersistentCommandExit::Exited { .. })));
    // Stopping from the host ends descendants too.
    let started = manager
        .start(
            "/bin/sh",
            &sh("sleep 300 & echo CHILD=$!; wait"),
            &cancel,
        )
        .unwrap();
    let terminal = manager.attach_terminal(&started.id).unwrap();
    let (text, _) = tab_output(&terminal, |text, _| text.contains("CHILD=") && text.ends_with('\n'));
    let pid = child_pid(&text);
    assert!(alive(pid));
    manager.terminate(&started.id).await.unwrap();
    let (_, exit) = tab_output(&terminal, |_, exit| exit.is_some());
    assert_eq!(exit, Some(PersistentCommandExit::Terminated));
    let deadline = StdInstant::now() + Duration::from_secs(5);
    while alive(pid) {
        assert!(StdInstant::now() < deadline, "descendant survived");
        std::thread::sleep(Duration::from_millis(20));
    }
    // An agent's close_after is end of input on a terminal.
    let started = manager.start("/bin/sh", &sh("cat"), &cancel).unwrap();
    let terminal = manager.attach_terminal(&started.id).unwrap();
    manager
        .write_stdin(&started.id, b"last\n", true, &cancel)
        .await
        .unwrap();
    let (_, exit) = tab_output(&terminal, |_, exit| exit.is_some());
    assert_eq!(exit, Some(PersistentCommandExit::Exited { code: Some(0) }));
    manager.stop_all();
}

#[tokio::test]
async fn pipe_commands_are_a_read_only_mirror_with_the_retained_tail() {
    let root = tempfile::tempdir().unwrap();
    let manager = manager(root.path(), false);
    let cancel = CancellationToken::new();
    let started = manager
        .start(
            "/bin/sh",
            &sh("test -t 1 || echo PIPE; echo ERR >&2; sleep 300"),
            &cancel,
        )
        .unwrap();
    assert!(!manager.list_sessions().unwrap()[0].pty);
    // Attached after the output was produced: the tail is replayed.
    std::thread::sleep(Duration::from_millis(300));
    let terminal = manager.attach_terminal(&started.id).unwrap();
    assert!(!terminal.interactive());
    assert!(terminal.write(b"x").is_err());
    assert!(terminal.resize(80, 24).is_err());
    tab_output(&terminal, |text, _| {
        text.contains("PIPE") && text.contains("ERR")
    });
    manager.stop_all();
    let (_, exit) = tab_output(&terminal, |_, exit| exit.is_some());
    assert_eq!(exit, Some(PersistentCommandExit::Terminated));
}
