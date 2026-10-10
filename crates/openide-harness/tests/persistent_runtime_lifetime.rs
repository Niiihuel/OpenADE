//! Persistent processes must outlive a model turn's temporary Tokio runtime.
#![cfg(unix)]

use openide_harness::{
    CommandIsolation, PersistentCommandEvent, PersistentCommandLimits, PersistentCommandManager,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn output(manager: &PersistentCommandManager, id: &str) -> Vec<u8> {
    manager
        .inspect_pending_output(id)
        .unwrap()
        .events
        .iter()
        .flat_map(|event| match event {
            PersistentCommandEvent::Output { bytes, .. } => bytes.clone(),
            PersistentCommandEvent::Omitted { .. } => panic!("unexpected fixture omission"),
        })
        .collect()
}

#[test]
fn retained_manager_keeps_real_pipe_alive_after_first_turn_runtime_and_serves_second_turn() {
    let root = tempfile::tempdir().unwrap();
    let manager = Arc::new(
        PersistentCommandManager::new(
            root.path().into(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_native_session_owner("retained-native-owner")
        .unwrap(),
    );
    let first = runtime();
    let command=first.block_on(async {
        let command=manager.start("/bin/sh",&["-c".into(),"printf 'READY\n'; while IFS= read -r line; do printf 'ECHO:%s\n' \"$line\"; done; printf 'EOF\n'".into()],&CancellationToken::new()).unwrap();
        let deadline=Instant::now()+Duration::from_secs(5);
        while !output(&manager,&command.id).starts_with(b"READY\n") {
            assert!(Instant::now()<deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        command
    });
    drop(first);
    // A heartbeat/input reaction from a second independent turn is required;
    // a stale exit=None or cached READY copy does not establish persistence.
    std::thread::sleep(Duration::from_millis(50));
    let second = runtime();
    second.block_on(async {
        manager
            .write_stdin(
                &command.id,
                "second turn café 🌱\n".as_bytes(),
                true,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let copy = manager.inspect_pending_output(&command.id).unwrap();
            if copy.exit.is_some() {
                assert_eq!(
                    output(&manager, &command.id),
                    "READY\nECHO:second turn café 🌱\nEOF\n".as_bytes()
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "retained command never produced its terminal receipt"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let final_poll = manager
            .poll(&command.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap();
        assert!(final_poll.finished);
        assert!(manager.list_sessions().unwrap().is_empty());
    });
}

#[test]
fn synchronous_start_uses_its_own_reactor_and_revoke_finishes_after_caller_runtime_closes() {
    let root = tempfile::tempdir().unwrap();
    let manager = PersistentCommandManager::new(
        root.path().into(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap();
    // No caller runtime exists when the child/pipes are registered.
    let command = manager
        .start(
            "/bin/sh",
            &["-c".into(), "printf READY; read line".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while output(&manager, &command.id) != b"READY" {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    manager.stop_all();
    let runtime = runtime();
    runtime.block_on(async {
        loop {
            let copy = manager.inspect_pending_output(&command.id).unwrap();
            if copy.exit.is_some() {
                assert!(copy.reader_error.is_none());
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    assert!(manager.control_snapshot(&command.id).unwrap().revoked);
    assert!(manager
        .start(
            "/bin/sh",
            &["-c".into(), "exit 99".into()],
            &CancellationToken::new()
        )
        .is_err());
}

#[test]
fn last_manager_drop_inside_async_task_does_not_drop_a_tokio_runtime_in_that_context() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime();
    runtime.block_on(async {
        let manager = PersistentCommandManager::new(
            root.path().into(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        let command = manager
            .start(
                "/bin/sh",
                &["-c".into(), "printf READY; read line".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while output(&manager, &command.id) != b"READY" {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let dropping = Instant::now();
        drop(manager);
        assert!(
            dropping.elapsed() < Duration::from_secs(6),
            "manager shutdown exceeded its bounded monitor grace"
        );
        // Panic here used to be possible if an owned Runtime was dropped
        // directly inside the host async task instead of on a dedicated thread.
        tokio::task::yield_now().await;
    });
}

#[test]
#[cfg(target_os = "linux")]
fn last_manager_drop_closes_the_actual_descendant_group_after_turn_runtime_was_dropped() {
    let root = tempfile::tempdir().unwrap();
    let manager = Arc::new(
        PersistentCommandManager::new(
            root.path().into(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap(),
    );
    let first = runtime();
    let command = first.block_on(async {
        let command = manager
            .start(
                "/bin/sh",
                &[
                    "-c".into(),
                    "sleep 30 & printf \"%s\n\" \"$!\"; read line".into(),
                ],
                &CancellationToken::new(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !output(&manager, &command.id).ends_with(b"\n") {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        command
    });
    let descendant = String::from_utf8(output(&manager, &command.id))
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    let stat_path = format!("/proc/{descendant}/stat");
    assert!(std::fs::read_to_string(&stat_path).is_ok());
    drop(first);
    let second = runtime();
    second.block_on(async {
        drop(manager);
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::read_to_string(&stat_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Ok(stat) if stat.rsplit_once(") ").unwrap().1.starts_with("Z ") => break,
            Ok(_) => {}
            Err(error) => panic!("cannot inspect fixture descendant: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "descendant kept running after final manager drop"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "subprocess helper for forced reader-drain grace, invoked by parent test"]
fn escaped_reader_fixture_child() {
    use std::os::unix::process::CommandExt;
    assert_eq!(
        std::env::var("OPENIDE_ESCAPE_READER_FIXTURE").unwrap(),
        "confirmed"
    );
    let mut child = std::process::Command::new("/bin/sh");
    child.args(["-c", "printf \"ESCAPED:%s\n\" \"$$\"; exec sleep 30"]);
    child.stdin(std::process::Stdio::null());
    // Isolated test fixture only: intentionally exercise the documented host
    // process-group escape limit and hold inherited stdout after parent kill.
    unsafe {
        child.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut escaped = child.spawn().unwrap();
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    let _ = escaped.kill();
    let _ = escaped.wait();
}

#[test]
#[cfg(target_os = "linux")]
fn last_async_manager_drop_returns_promptly_even_when_escaped_descendant_holds_reader_open() {
    struct FixtureCleanup(i32);
    impl Drop for FixtureCleanup {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let manager = PersistentCommandManager::new(
        root.path().into(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap();
    // Bootstrap variables are intentionally not passed. A shell literal sets
    // this fixture-only flag for the test binary; argv remains explicit.
    let binary = std::env::current_exe().unwrap();
    let command = manager.start("/bin/sh", &["-c".into(), "OPENIDE_ESCAPE_READER_FIXTURE=confirmed exec \"$1\" --exact escaped_reader_fixture_child --ignored --nocapture".into(), "fixture".into(), binary.to_string_lossy().into_owned()], &CancellationToken::new()).unwrap();
    let runtime = runtime();
    runtime.block_on(async {
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            let bytes = output(&manager, &command.id);
            let text = String::from_utf8_lossy(&bytes);
            if let Some(pid) = text.lines().find_map(|line| {
                line.strip_prefix("ESCAPED:")
                    .and_then(|pid| pid.parse::<i32>().ok())
            }) {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "escaped fixture never confirmed its inherited pipe"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let cleanup = FixtureCleanup(pid);
        assert!(unsafe { libc::kill(pid, 0) } == 0);
        let dropping = Instant::now();
        drop(manager);
        assert!(
            dropping.elapsed() < Duration::from_millis(250),
            "Drop blocked on escaped reader grace"
        );
        // The escaped process is still alive, proving the latency assertion
        // exercised a pending reader rather than an already-finished monitor.
        assert!(unsafe { libc::kill(pid, 0) } == 0);
        drop(cleanup);
        tokio::task::yield_now().await;
    });
}
