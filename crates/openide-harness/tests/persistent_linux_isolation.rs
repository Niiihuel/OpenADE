//! Real Linux sandbox acceptance. Explicitly run ignored tests with Bubblewrap
//! installed and working user/mount/network/PID namespaces; never silently skip.
#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use openide_harness::{
    AgentError, CommandIsolation, PersistentCommandEvent, PersistentCommandExit,
    PersistentCommandLimits, PersistentCommandManager,
};
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::{Path, PathBuf},
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
fn probe(workspace: &Path) -> PathBuf {
    let source = workspace.join("probe.rs");
    let binary = workspace.join("probe");
    std::fs::write(&source, include_str!("fixtures/linux_sandbox_probe.rs")).unwrap();
    let compile =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2021")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
    assert!(
        compile.status.success(),
        "probe compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    binary
}
fn manager(workspace: &Path, lifetime: Duration) -> Arc<PersistentCommandManager> {
    Arc::new(
        PersistentCommandManager::new(
            workspace.into(),
            CommandIsolation::WorkspaceWriteOffline,
            PersistentCommandLimits {
                max_lifetime: lifetime,
                ..PersistentCommandLimits::default()
            },
        )
        .unwrap()
        .with_native_session_owner("linux-isolated-owner")
        .unwrap(),
    )
}
fn copied_bytes(manager: &PersistentCommandManager, id: &str) -> Vec<u8> {
    manager
        .inspect_pending_output(id)
        .unwrap()
        .events
        .iter()
        .flat_map(|event| match event {
            PersistentCommandEvent::Output { bytes, .. } => bytes.clone(),
            PersistentCommandEvent::Omitted { .. } => panic!("unexpected probe output omission"),
        })
        .collect()
}
async fn terminal(manager: &PersistentCommandManager, id: &str) -> PersistentCommandExit {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let copy = manager.inspect_pending_output(id).unwrap();
        if let Some(exit) = copy.exit {
            assert!(
                copy.reader_error.is_none(),
                "{}",
                copy.reader_error.unwrap_or_default()
            );
            return exit;
        }
        assert!(
            Instant::now() < deadline,
            "isolated process never produced terminal receipt; output={}",
            String::from_utf8_lossy(&copied_bytes(manager, id))
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
async fn heartbeat(workspace: &Path) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = std::fs::read_to_string(workspace.join("heartbeat"))
            .unwrap_or_default()
            .parse::<u64>()
            .unwrap_or_default();
        if value >= 2 && workspace.join("detached-started").exists() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "setsid descendant never confirmed heartbeat"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn assert_heartbeat_stopped(workspace: &Path) {
    std::thread::sleep(Duration::from_millis(80));
    let before = std::fs::read(workspace.join("heartbeat")).unwrap();
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(
        std::fs::read(workspace.join("heartbeat")).unwrap(),
        before,
        "escaped descendant still writes after namespace termination"
    );
}

#[test]
fn cancelled_and_pty_requests_fail_before_any_isolated_payload_or_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path(), Duration::from_secs(10));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        manager.start("/bin/sh", &["-c".into(), "touch payload".into()], &cancel),
        Err(AgentError::Cancelled)
    ));
    assert!(matches!(
        manager.start_pty(
            "/bin/sh",
            &["-c".into(), "touch payload".into()],
            80,
            24,
            &CancellationToken::new()
        ),
        Err(AgentError::Tool(_))
    ));
    assert!(!dir.path().join("payload").exists());
    assert!(manager.list_sessions().unwrap().is_empty());
}

#[test]
#[ignore = "requires trusted Bubblewrap and working Linux namespaces/seccomp/pidfd/close_range"]
fn actual_linux_persistent_confinement_and_binary_stdin_survive_separate_turn_runtimes() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"safe").unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), workspace.join("escape")).unwrap();
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unix = std::os::unix::net::UnixListener::bind(workspace.join("local.sock")).unwrap();
    let secret = std::fs::File::open(outside.join("secret")).unwrap();
    let canary_fd = unsafe { libc::fcntl(secret.as_raw_fd(), libc::F_DUPFD, 128) };
    assert!(canary_fd >= 128);
    let canary = unsafe { OwnedFd::from_raw_fd(canary_fd) };
    let binary = probe(&workspace);
    let program = binary.to_str().unwrap();
    let manager = manager(&workspace, Duration::from_secs(30));
    let first = runtime();
    first.block_on(async {
        let command = manager
            .start(
                program,
                &[
                    "confinement".into(),
                    outside.to_string_lossy().into_owned(),
                    tcp.local_addr().unwrap().to_string(),
                    canary.as_raw_fd().to_string(),
                ],
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            terminal(&manager, &command.id).await,
            PersistentCommandExit::Exited { code: Some(0) },
            "{}",
            String::from_utf8_lossy(&copied_bytes(&manager, &command.id))
        );
        assert!(
            String::from_utf8_lossy(&copied_bytes(&manager, &command.id))
                .contains("confinement-ok")
        );
        assert!(
            manager
                .poll(&command.id, Duration::ZERO, &CancellationToken::new())
                .await
                .unwrap()
                .finished
        );
    });
    let command = first.block_on(async {
        let command = manager
            .start(program, &["echo".into()], &CancellationToken::new())
            .unwrap();
        manager
            .write_stdin(
                &command.id,
                "first café\n".as_bytes(),
                false,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        command
    });
    let scope = manager.control_snapshot(&command.id).unwrap();
    assert_eq!(scope.command_argv, vec![program.to_owned(), "echo".into()]);
    assert_eq!(scope.isolation, CommandIsolation::WorkspaceWriteOffline);
    assert!(!scope.pty && scope.dimensions.is_none() && !scope.resize_enabled);
    drop(first);
    let second = runtime();
    second.block_on(async {
        let tail = b"second \0binary\n";
        manager
            .write_stdin(&command.id, tail, true, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            terminal(&manager, &command.id).await,
            PersistentCommandExit::Exited { code: Some(0) }
        );
        let mut expected = "first café\n".as_bytes().to_vec();
        expected.extend_from_slice(tail);
        assert_eq!(copied_bytes(&manager, &command.id), expected);
        assert_eq!(std::fs::read(workspace.join("received")).unwrap(), expected);
        assert!(!manager.control_snapshot(&command.id).unwrap().stdin_open());
    });
    assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"safe");
    assert_eq!(
        std::fs::read(workspace.join("inside")).unwrap(),
        b"workspace-ok"
    );
    drop((tcp, unix, canary));
}

#[test]
#[ignore = "requires trusted Bubblewrap and working Linux namespaces/seccomp/pidfd/close_range"]
fn actual_linux_persistent_namespace_closes_setsid_descendants_on_revoke_terminate_timeout_and_drop(
) {
    let dir = tempfile::tempdir().unwrap();
    let binary = probe(dir.path());
    for operation in ["revoke", "terminate", "timeout", "drop"] {
        for marker in ["parent-started", "detached-started", "heartbeat"] {
            let _ = std::fs::remove_file(dir.path().join(marker));
        }
        let lifetime = if operation == "timeout" {
            Duration::from_millis(300)
        } else {
            Duration::from_secs(30)
        };
        let manager = manager(dir.path(), lifetime);
        let first = runtime();
        let command = first.block_on(async {
            let command = manager
                .start(
                    binary.to_str().unwrap(),
                    &["tree".into()],
                    &CancellationToken::new(),
                )
                .unwrap();
            heartbeat(dir.path()).await;
            command
        });
        drop(first);
        let second = runtime();
        match operation {
            "revoke" => {
                manager.stop_all();
                second.block_on(async {
                    assert_eq!(
                        terminal(&manager, &command.id).await,
                        PersistentCommandExit::Terminated
                    );
                });
                assert!(manager.control_snapshot(&command.id).unwrap().revoked);
            }
            "terminate" => second.block_on(async {
                manager.terminate(&command.id).await.unwrap();
                assert_eq!(
                    terminal(&manager, &command.id).await,
                    PersistentCommandExit::Terminated
                );
            }),
            "timeout" => second.block_on(async {
                assert_eq!(
                    terminal(&manager, &command.id).await,
                    PersistentCommandExit::TimedOut
                );
            }),
            "drop" => second.block_on(async {
                let start = Instant::now();
                drop(manager);
                assert!(start.elapsed() < Duration::from_millis(250));
            }),
            _ => unreachable!(),
        }
        assert_heartbeat_stopped(dir.path());
    }
}

#[test]
#[ignore = "requires trusted Bubblewrap and working Linux namespaces/seccomp/pidfd/close_range"]
fn actual_linux_persistent_hardlink_preflight_rejects_before_payload_without_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = dir.path().join("outside-secret");
    std::fs::write(&outside, b"safe").unwrap();
    std::fs::hard_link(&outside, workspace.join("alias")).unwrap();
    let manager = manager(&workspace, Duration::from_secs(30));
    let error = manager
        .start(
            "/bin/sh",
            &[
                "-c".into(),
                "echo unsafe > payload; echo changed > alias".into(),
            ],
            &CancellationToken::new(),
        )
        .unwrap_err();
    assert!(
        matches!(error,AgentError::Tool(message) if message.contains("hard-linked") && message.contains("not started"))
    );
    assert!(!workspace.join("payload").exists());
    assert_eq!(std::fs::read(&outside).unwrap(), b"safe");
    assert!(manager.list_sessions().unwrap().is_empty());
}

#[test]
#[ignore = "requires trusted Bubblewrap and working Linux namespaces/seccomp/pidfd/close_range"]
fn actual_linux_persistent_normal_parent_exit_closes_ready_setsid_descendant_and_readers() {
    let dir = tempfile::tempdir().unwrap();
    let binary = probe(dir.path());
    let manager = manager(dir.path(), Duration::from_secs(30));
    let runtime = runtime();
    runtime.block_on(async {
        let command = manager
            .start(
                binary.to_str().unwrap(),
                &["tree".into(), "exit".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            terminal(&manager, &command.id).await,
            PersistentCommandExit::Exited { code: Some(0) }
        );
        assert!(
            dir.path().join("detached-started").exists(),
            "child never confirmed setsid before normal parent exit"
        );
        let final_poll = manager
            .poll(&command.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap();
        assert!(final_poll.finished && final_poll.reader_error.is_none());
        assert!(manager.list_sessions().unwrap().is_empty());
    });
    let before = std::fs::read(dir.path().join("heartbeat")).ok();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        std::fs::read(dir.path().join("heartbeat")).ok(),
        before,
        "descendant writes continued after its normal parent exit"
    );
}

#[test]
#[ignore = "subprocess helper for real SIGKILL owner acceptance, invoked by parent test"]
fn linux_persistent_sigkill_owner_helper() {
    let workspace = PathBuf::from(
        std::env::var_os("OPENIDE_LINUX_KILL_WORKSPACE").expect("explicit subprocess workspace"),
    );
    let binary = workspace.join("probe");
    let manager = manager(&workspace, Duration::from_secs(30));
    let command = manager
        .start(
            binary.to_str().unwrap(),
            &["tree".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = std::fs::read_to_string(workspace.join("heartbeat"))
            .unwrap_or_default()
            .parse::<u64>()
            .unwrap_or_default();
        if value >= 2 && workspace.join("detached-started").exists() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real setsid process never became ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::write(
        workspace.join("owner-ready"),
        format!("{}\n{}\n", command.id, manager.generation()),
    )
    .unwrap();
    // The parent sends SIGKILL to this entire Rust process. No Rust destructor,
    // manager.stop_all, Tokio shutdown or orderly target EOF can run.
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
#[ignore = "requires trusted Bubblewrap and working Linux namespaces/seccomp/pidfd/close_range"]
fn actual_linux_sigkill_owner_closes_namespace_without_destructors_or_replaying_old_process() {
    use std::os::unix::process::ExitStatusExt;
    struct HelperCleanup(std::process::Child);
    impl Drop for HelperCleanup {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    probe(dir.path());
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "linux_persistent_sigkill_owner_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("OPENIDE_LINUX_KILL_WORKSPACE", dir.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut helper = HelperCleanup(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    let saved = loop {
        if let Ok(saved) = std::fs::read_to_string(dir.path().join("owner-ready")) {
            break saved;
        }
        if let Some(status) = helper.0.try_wait().unwrap() {
            use std::io::Read;
            let mut stderr = String::new();
            helper
                .0
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("sandbox owner exited before confirmed real process: {status}; {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "sandbox owner never confirmed ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        unsafe { libc::kill(helper.0.id() as i32, libc::SIGKILL) },
        0
    );
    assert_eq!(helper.0.wait().unwrap().signal(), Some(libc::SIGKILL));
    assert_heartbeat_stopped(dir.path());
    let mut lines = saved.lines();
    let old_id = lines.next().unwrap();
    let old_generation = lines.next().unwrap();
    let reopened = manager(dir.path(), Duration::from_secs(30));
    assert_ne!(reopened.generation(), old_generation);
    assert!(reopened.list_sessions().unwrap().is_empty());
    assert!(reopened.inspect_pending_output(old_id).is_err());
    assert_heartbeat_stopped(dir.path());
}
