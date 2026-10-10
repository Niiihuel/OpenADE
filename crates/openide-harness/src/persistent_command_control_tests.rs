use super::*;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

fn probe() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("control_probe.rs");
    let binary = directory.path().join(if cfg!(windows) {
        "control_probe.exe"
    } else {
        "control_probe"
    });
    std::fs::write(
        &source,
        r#"
use std::io::{BufRead, Write};
fn main() {
    println!("READY");
    std::io::stdout().flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        #[cfg(windows)]
        if line == "size" {
            #[repr(C)] struct Coord { x: i16, y: i16 }
            #[repr(C)] struct SmallRect { left:i16, top:i16, right:i16, bottom:i16 }
            #[repr(C)] struct Info { size:Coord, cursor:Coord, attributes:u16, window:SmallRect, maximum:Coord }
            #[link(name="kernel32")] extern "system" {
                fn GetStdHandle(kind:u32)->*mut std::ffi::c_void;
                fn GetConsoleScreenBufferInfo(handle:*mut std::ffi::c_void, info:*mut Info)->i32;
            }
            let mut info:Info=unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { GetConsoleScreenBufferInfo(GetStdHandle((-11i32) as u32), &mut info) },0);
            println!("SIZE:{}:{}",info.size.x,info.size.y);
        } else { println!("ECHO:{line}"); }
        #[cfg(not(windows))]
        println!("ECHO:{line}");
        std::io::stdout().flush().unwrap();
    }
    println!("EOF");
    std::io::stdout().flush().unwrap();
}
"#,
    )
    .unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let compiled = std::process::Command::new(compiler)
        .args(["--edition=2021"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    (directory, binary)
}

fn output_bytes(copy: &PersistentCommandInspection) -> Vec<u8> {
    copy.events
        .iter()
        .flat_map(|event| match event {
            PersistentCommandEvent::Output { bytes, .. } => bytes.clone(),
            PersistentCommandEvent::Omitted { .. } => {
                panic!("unexpected omission in control probe")
            }
        })
        .collect()
}

async fn wait_output(manager: &PersistentCommandManager, id: &str, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let copy = manager.inspect_pending_output(id).unwrap();
        if String::from_utf8(output_bytes(&copy))
            .unwrap()
            .contains(text)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "control probe did not publish expected output"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_exit(manager: &PersistentCommandManager, id: &str) -> PersistentCommandInspection {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let copy = manager.inspect_pending_output(id).unwrap();
        if copy.exit.is_some() {
            return copy;
        }
        assert!(Instant::now() < deadline, "control probe did not exit");
        sleep(Duration::from_millis(10)).await;
    }
}

fn manager(workspace: &std::path::Path, owner: &str) -> PersistentCommandManager {
    PersistentCommandManager::new(
        workspace.to_path_buf(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap()
    .with_native_session_owner(owner)
    .unwrap()
}

#[tokio::test]
async fn command_control_snapshot_observes_real_echo_eof_without_consuming_output() {
    let (_probe, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let manager = manager(workspace.path(), "native-session-1");
    let args = vec!["literal argument: café 🌱".into(), "--kept-separate".into()];
    let started = manager
        .start(binary.to_str().unwrap(), &args, &CancellationToken::new())
        .unwrap();
    wait_output(&manager, &started.id, "READY\n").await;
    let before_output = manager.inspect_pending_output(&started.id).unwrap();
    let before = manager.control_snapshot(&started.id).unwrap();
    assert_eq!(before.id, started.id);
    assert_eq!(before.generation, manager.generation());
    assert_eq!(before.owner_session_id.as_deref(), Some("native-session-1"));
    assert_eq!(before.workspace, workspace.path().canonicalize().unwrap());
    assert_eq!(before.isolation, CommandIsolation::HostUnrestricted);
    assert_eq!(
        before.command_argv,
        std::iter::once(binary.to_str().unwrap().to_owned())
            .chain(args)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        manager.command_argv(&started.id).unwrap(),
        before.command_argv
    );
    assert_eq!(before.stdin, PersistentCommandStdinState::Open);
    assert!(before.stdin_open());
    assert!(!before.revoked);
    assert!(!before.stop_requested);
    assert!(!before.pty);
    assert_eq!(before.dimensions, None);
    assert!(!before.dimensions_uncertain);
    assert!(!before.resize_enabled);
    assert_eq!(
        before.max_stdin_bytes,
        PersistentCommandLimits::default().max_stdin_bytes
    );
    assert_eq!(
        manager.inspect_pending_output(&started.id).unwrap(),
        before_output
    );

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        manager
            .write_stdin(&started.id, b"not delivered\n", true, &cancelled)
            .await,
        Err(AgentError::Cancelled)
    ));
    assert_eq!(manager.control_snapshot(&started.id).unwrap(), before);
    assert!(manager
        .write_stdin(
            &started.id,
            &vec![0; before.max_stdin_bytes + 1],
            true,
            &CancellationToken::new()
        )
        .await
        .is_err());
    assert_eq!(manager.control_snapshot(&started.id).unwrap(), before);
    assert_eq!(
        manager.inspect_pending_output(&started.id).unwrap(),
        before_output
    );

    manager
        .write_stdin(
            &started.id,
            "Hola café 🌱\n".as_bytes(),
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    wait_output(&manager, &started.id, "ECHO:Hola café 🌱\n").await;
    assert!(manager.control_snapshot(&started.id).unwrap().stdin_open());
    manager
        .close_stdin(&started.id, &CancellationToken::new())
        .await
        .unwrap();
    let closed = manager.control_snapshot(&started.id).unwrap();
    assert_eq!(closed.stdin, PersistentCommandStdinState::Closed);
    assert!(!closed.stdin_open());
    let terminal = wait_exit(&manager, &started.id).await;
    assert_eq!(
        output_bytes(&terminal),
        "READY\nECHO:Hola café 🌱\nEOF\n".as_bytes()
    );
    let final_controls = manager.control_snapshot(&started.id).unwrap();
    assert_eq!(
        final_controls.exit,
        Some(PersistentCommandExit::Exited { code: Some(0) })
    );
    assert_eq!(final_controls.stdin, PersistentCommandStdinState::Closed);
    assert_eq!(
        manager.inspect_pending_output(&started.id).unwrap(),
        terminal
    );
    let polled = manager
        .poll(&started.id, Duration::ZERO, &CancellationToken::new())
        .await
        .unwrap();
    assert!(polled.finished);
    assert_eq!(polled.events, terminal.events);
    assert_eq!(polled.exit, terminal.exit);
    assert!(manager.control_snapshot(&started.id).is_err());
    assert!(manager.command_argv(&started.id).is_err());
}

#[tokio::test]
async fn command_control_generation_owner_and_revocation_reject_foreign_or_reopened_ids() {
    let (_probe, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let first = manager(workspace.path(), "owner-first");
    let second = manager(workspace.path(), "owner-second");
    assert_eq!(
        Uuid::parse_str(first.generation())
            .unwrap()
            .get_version_num(),
        4
    );
    assert_ne!(first.generation(), second.generation());
    let command = first
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap();
    wait_output(&first, &command.id, "READY\n").await;
    assert!(second.control_snapshot(&command.id).is_err());
    assert!(second.command_argv(&command.id).is_err());
    let second_command = second
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap();
    wait_output(&second, &second_command.id, "READY\n").await;
    let output = first.inspect_pending_output(&command.id).unwrap();
    first.stop_all();
    let revoked = first.control_snapshot(&command.id).unwrap();
    assert_eq!(revoked.generation, first.generation());
    assert!(revoked.revoked);
    assert!(revoked.stop_requested);
    assert!(!revoked.stdin_open());
    assert!(!revoked.resize_enabled);
    assert!(first
        .write_stdin(
            &command.id,
            b"must not run\n",
            false,
            &CancellationToken::new()
        )
        .await
        .is_err());
    assert!(first
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .is_err());
    let terminal = wait_exit(&first, &command.id).await;
    assert_eq!(terminal.events, output.events);
    assert_eq!(terminal.exit, Some(PersistentCommandExit::Terminated));
    assert_eq!(first.list_sessions().unwrap().len(), 1);
    let replacement = manager(workspace.path(), "owner-first");
    assert_ne!(replacement.generation(), first.generation());
    assert_eq!(
        replacement.native_session_owner(),
        first.native_session_owner()
    );
    assert!(replacement.control_snapshot(&command.id).is_err());
    assert!(replacement.list_sessions().unwrap().is_empty());
    assert!(second
        .control_snapshot(&second_command.id)
        .unwrap()
        .stdin_open());
    second
        .write_stdin(
            &second_command.id,
            b"other owner still live\n",
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let second_terminal = wait_exit(&second, &second_command.id).await;
    assert!(String::from_utf8(output_bytes(&second_terminal))
        .unwrap()
        .contains("ECHO:other owner still live\nEOF\n"));
    assert!(
        first
            .poll(&command.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap()
            .finished
    );
    let unbound = PersistentCommandManager::new(
        workspace.path().to_path_buf(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap();
    let unowned_command = unbound
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap();
    wait_output(&unbound, &unowned_command.id, "READY\n").await;
    assert_eq!(
        unbound
            .control_snapshot(&unowned_command.id)
            .unwrap()
            .owner_session_id,
        None
    );
    assert!(unbound
        .with_native_session_owner("owner-after-start")
        .is_err());
}

#[test]
fn command_control_owner_binding_is_optional_once_only_and_validated_before_effects() {
    let workspace = tempfile::tempdir().unwrap();
    for owner in ["", "../other", "ñ", &"x".repeat(257)] {
        let unbound = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap();
        assert!(unbound.with_native_session_owner(owner).is_err());
    }
    let unbound = PersistentCommandManager::new(
        workspace.path().to_path_buf(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap();
    assert_eq!(unbound.native_session_owner(), None);
    let bound = unbound.with_native_session_owner("native-owner").unwrap();
    assert_eq!(bound.native_session_owner(), Some("native-owner"));
    assert!(bound.with_native_session_owner("other-owner").is_err());
    let revoked = PersistentCommandManager::new(
        workspace.path().to_path_buf(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap();
    revoked.stop_all();
    assert!(revoked.with_native_session_owner("native-owner").is_err());
}

struct PendingInput {
    dispatched: Arc<AtomicBool>,
}

impl AsyncWrite for PendingInput {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.dispatched.store(true, Ordering::SeqCst);
        Poll::Pending
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn command_control_abandoned_input_marks_uncertainty_and_stops_real_process() {
    let (_probe, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let manager = Arc::new(manager(workspace.path(), "owner-input"));
    let started = manager
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap();
    wait_output(&manager, &started.id, "READY\n").await;
    let session = manager.session(&started.id).unwrap();
    let dispatched = Arc::new(AtomicBool::new(false));
    // The process/manager are real. A deterministic pending writer injects the
    // transport boundary; retain the real handle so it does not signal EOF.
    let original = session.stdin.lock().await.replace(Box::new(PendingInput {
        dispatched: dispatched.clone(),
    }));
    let writer = manager.clone();
    let id = started.id.clone();
    let operation = tokio::spawn(async move {
        writer
            .write_stdin(&id, b"uncertain input\n", true, &CancellationToken::new())
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dispatched.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "injected transport was never dispatched"
        );
        sleep(Duration::from_millis(10)).await;
    }
    let busy = manager.control_snapshot(&started.id).unwrap();
    assert_eq!(busy.stdin, PersistentCommandStdinState::Busy);
    assert!(!busy.stdin_open());
    operation.abort();
    assert!(operation.await.unwrap_err().is_cancelled());
    let uncertain = manager.control_snapshot(&started.id).unwrap();
    assert_eq!(uncertain.stdin, PersistentCommandStdinState::Uncertain);
    assert!(!uncertain.stdin_open());
    assert!(uncertain.stop_requested);
    assert!(!uncertain.revoked);
    drop(original);
    let terminal = wait_exit(&manager, &started.id).await;
    assert_eq!(terminal.exit, Some(PersistentCommandExit::Terminated));
    assert_eq!(output_bytes(&terminal), b"READY\n");
    assert_eq!(
        manager.control_snapshot(&started.id).unwrap().stdin,
        PersistentCommandStdinState::Uncertain
    );
    assert_eq!(
        manager.inspect_pending_output(&started.id).unwrap(),
        terminal
    );
    let polled = manager
        .poll(&started.id, Duration::ZERO, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(polled.events, terminal.events);
    assert!(polled.finished);
}

#[cfg(not(windows))]
#[tokio::test]
async fn command_control_pipe_snapshot_never_invents_pty_dimensions_or_resize() {
    let (_probe, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let manager = manager(workspace.path(), "owner-pipe")
        .with_pty_tools()
        .with_pty_resize_tools();
    let started = manager
        .start(binary.to_str().unwrap(), &[], &CancellationToken::new())
        .unwrap();
    wait_output(&manager, &started.id, "READY\n").await;
    let before = manager.control_snapshot(&started.id).unwrap();
    assert!(!before.pty);
    assert!(!before.resize_enabled);
    assert_eq!(before.dimensions, None);
    assert!(manager
        .resize_pty(&started.id, 120, 40, &CancellationToken::new())
        .await
        .is_err());
    assert_eq!(manager.control_snapshot(&started.id).unwrap(), before);
    assert!(manager
        .start_pty(
            binary.to_str().unwrap(),
            &[],
            80,
            24,
            &CancellationToken::new()
        )
        .is_err());
    assert_eq!(manager.list_sessions().unwrap().len(), 1);
    manager
        .close_stdin(&started.id, &CancellationToken::new())
        .await
        .unwrap();
    wait_exit(&manager, &started.id).await;
}

#[cfg(windows)]
#[tokio::test]
async fn command_control_conpty_dimensions_follow_only_confirmed_resize() {
    let (_probe, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let manager = manager(workspace.path(), "owner-pty")
        .with_pty_tools()
        .with_pty_resize_tools();
    let started = manager
        .start_pty(
            binary.to_str().unwrap(),
            &[],
            80,
            24,
            &CancellationToken::new(),
        )
        .unwrap();
    wait_output(&manager, &started.id, "READY").await;
    let initial = manager.control_snapshot(&started.id).unwrap();
    assert!(initial.pty);
    assert!(initial.resize_enabled);
    assert_eq!(
        initial.dimensions,
        Some(PersistentCommandDimensions {
            columns: 80,
            rows: 24
        })
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        manager.resize_pty(&started.id, 120, 40, &cancelled).await,
        Err(AgentError::Cancelled)
    ));
    assert!(manager
        .resize_pty(&started.id, 19, 40, &CancellationToken::new())
        .await
        .is_err());
    assert_eq!(manager.control_snapshot(&started.id).unwrap(), initial);
    manager
        .resize_pty(&started.id, 120, 40, &CancellationToken::new())
        .await
        .unwrap();
    let resized = manager.control_snapshot(&started.id).unwrap();
    assert_eq!(
        resized.dimensions,
        Some(PersistentCommandDimensions {
            columns: 120,
            rows: 40
        })
    );
    assert!(!resized.dimensions_uncertain);
    manager
        .write_stdin(&started.id, b"size\r\n", false, &CancellationToken::new())
        .await
        .unwrap();
    wait_output(&manager, &started.id, "SIZE:120:40").await;
    manager.terminate(&started.id).await.unwrap();
    let stopped = manager.control_snapshot(&started.id).unwrap();
    assert!(!stopped.stdin_open());
    assert!(!stopped.resize_enabled);
    assert_eq!(stopped.dimensions, resized.dimensions);
}

#[cfg(windows)]
#[tokio::test]
async fn command_control_cancelled_conpty_resize_retains_last_confirmed_dimensions_as_uncertain() {
    let (_probe, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let manager = Arc::new(
        manager(workspace.path(), "owner-pty-cancel")
            .with_pty_tools()
            .with_pty_resize_tools(),
    );
    let started = manager
        .start_pty(
            binary.to_str().unwrap(),
            &[],
            80,
            24,
            &CancellationToken::new(),
        )
        .unwrap();
    wait_output(&manager, &started.id, "READY").await;
    let session = manager.session(&started.id).unwrap();
    let console = session.pty_console.as_ref().unwrap().clone();
    let (locked, observed_lock) = std::sync::mpsc::channel();
    let (release, observed_release) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _console_guard = console.lock().unwrap();
        locked.send(()).unwrap();
        observed_release
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
    });
    observed_lock.recv_timeout(Duration::from_secs(5)).unwrap();
    let cancellation = CancellationToken::new();
    let resize_manager = manager.clone();
    let resize_id = started.id.clone();
    let resize_cancel = cancellation.clone();
    let operation = tokio::spawn(async move {
        resize_manager
            .resize_pty(&resize_id, 120, 40, &resize_cancel)
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !manager
        .control_snapshot(&started.id)
        .unwrap()
        .dimensions_uncertain
    {
        assert!(
            Instant::now() < deadline,
            "resize operation was not dispatched"
        );
        sleep(Duration::from_millis(10)).await;
    }
    cancellation.cancel();
    assert!(matches!(
        operation.await.unwrap(),
        Err(AgentError::EffectUncertain(_))
    ));
    let uncertain = manager.control_snapshot(&started.id).unwrap();
    assert_eq!(
        uncertain.dimensions,
        Some(PersistentCommandDimensions {
            columns: 80,
            rows: 24
        })
    );
    assert!(uncertain.dimensions_uncertain);
    assert!(uncertain.stop_requested);
    assert!(!uncertain.resize_enabled);
    release.send(()).unwrap();
    holder.join().unwrap();
    // The blocking Win32 operation can finish after cancellation. It must not
    // retroactively publish 120x40 as a successful observed control result.
    wait_exit(&manager, &started.id).await;
    assert_eq!(
        manager.control_snapshot(&started.id).unwrap().dimensions,
        uncertain.dimensions
    );
    assert!(
        manager
            .control_snapshot(&started.id)
            .unwrap()
            .dimensions_uncertain
    );
}
