//! Producer ACKs precede live output, independently of destructive tool polls.
#![cfg(unix)]

use openide_core::{
    NativeCommandOutputBinding, NativeCommandOutputExit, NativeCommandOutputStore,
    NativeCommandOutputStream, SessionStore,
};
use openide_harness::{
    CommandIsolation, PersistentCommandEvent, PersistentCommandExit, PersistentCommandLimits,
    PersistentCommandManager, PersistentCommandStream,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    owner: String,
    output: Arc<NativeCommandOutputStore>,
    manager: Arc<PersistentCommandManager>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_isolation(CommandIsolation::HostUnrestricted)
    }

    fn with_isolation(isolation: CommandIsolation) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let owner = sessions.create(&workspace, "native").unwrap().id;
        let output = Arc::new(NativeCommandOutputStore::new(sessions));
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.clone(),
                isolation,
                PersistentCommandLimits::default(),
            )
            .unwrap()
            .with_native_session_owner(owner.clone())
            .unwrap()
            .with_native_output_store(output.clone())
            .unwrap(),
        );
        Self {
            root,
            workspace,
            owner,
            output,
            manager,
        }
    }

    fn binding(&self, command_id: &str) -> NativeCommandOutputBinding {
        NativeCommandOutputBinding {
            session_id: self.owner.clone(),
            workspace: self.workspace.clone(),
            manager_generation: self.manager.generation().into(),
            command_id: command_id.into(),
        }
    }

    fn sidecar(&self) -> PathBuf {
        self.root
            .path()
            .join("sessions")
            .join(format!("{}.native-command-output.json", self.owner))
    }

    fn reopen(&self) -> Arc<NativeCommandOutputStore> {
        Arc::new(NativeCommandOutputStore::new(Arc::new(
            SessionStore::open(self.root.path().join("sessions")).unwrap(),
        )))
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn bytes(events: &[PersistentCommandEvent], stream: PersistentCommandStream) -> Vec<u8> {
    events
        .iter()
        .flat_map(|event| match event {
            PersistentCommandEvent::Output {
                stream: found,
                bytes,
            } if *found == stream => bytes.clone(),
            PersistentCommandEvent::Omitted { .. } => panic!("fixture output was omitted"),
            _ => Vec::new(),
        })
        .collect()
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "producer fixture timed out");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[test]
fn binary_pipe_acks_survive_poll_runtime_changes_final_removal_and_store_reopen() {
    let fixture = Fixture::new();
    let script = "printf '\\000\\377\\303'; printf 'ERR0\\n' >&2; IFS= read -r line; printf '\\251\\000end'; printf '\\376' >&2; exit 7";
    let command = fixture
        .manager
        .start(
            "/bin/sh",
            &["-c".into(), script.into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let first = runtime();
    let initial = first.block_on(async {
        wait_until(|| {
            let copy = fixture.manager.inspect_pending_output(&command.id).unwrap();
            bytes(&copy.events, PersistentCommandStream::Stdout) == [0, 255, 195]
                && bytes(&copy.events, PersistentCommandStream::Stderr) == b"ERR0\n"
        })
        .await;
        // These bytes are already ACKed before any drain or tool publication.
        let page = fixture
            .output
            .read(&fixture.binding(&command.id), 0, 256, 64)
            .unwrap();
        assert_eq!(page.summary.observed_end, 8);
        assert!(page.summary.terminal.is_none());
        fixture
            .manager
            .poll(&command.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap()
    });
    assert!(!initial.finished);
    drop(first);
    let second = runtime();
    let final_poll = second.block_on(async {
        fixture
            .manager
            .write_stdin(&command.id, b"continue\n", true, &CancellationToken::new())
            .await
            .unwrap();
        wait_until(|| {
            fixture
                .manager
                .inspect_pending_output(&command.id)
                .unwrap()
                .exit
                .is_some()
        })
        .await;
        // An observable live exit requires the durable terminal ACK first.
        let summary = fixture
            .output
            .list(&fixture.owner, &fixture.workspace)
            .unwrap()
            .remove(0);
        let terminal = summary.terminal.unwrap();
        assert_eq!(terminal.exit_kind, Some(NativeCommandOutputExit::Exited));
        assert_eq!(terminal.exit_code, Some(7));
        assert!(terminal.reader_error.is_none());
        fixture
            .manager
            .poll(&command.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap()
    });
    assert!(final_poll.finished);
    assert_eq!(
        final_poll.exit,
        Some(PersistentCommandExit::Exited { code: Some(7) })
    );
    assert!(fixture.manager.list_sessions().unwrap().is_empty());
    let mut delivered = initial.events;
    delivered.extend(final_poll.events);
    let expected_stdout = [0, 255, 195, 169, 0, b'e', b'n', b'd'];
    let expected_stderr = b"ERR0\n\xfe";
    assert_eq!(
        bytes(&delivered, PersistentCommandStream::Stdout),
        expected_stdout
    );
    assert_eq!(
        bytes(&delivered, PersistentCommandStream::Stderr),
        expected_stderr
    );
    let reopened = fixture.reopen();
    let summaries = reopened.list(&fixture.owner, &fixture.workspace).unwrap();
    let summary = &summaries[0];
    assert_eq!(summary.header.binding, fixture.binding(&command.id));
    assert_eq!(summary.header.argv, ["/bin/sh", "-c", script]);
    assert_eq!(summary.retained_bytes, 14);
    assert_eq!(summary.omitted_bytes, 0);
    let mut cursor = 0;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        // Cursor boundaries deliberately split UTF-8 and binary bytes.
        let page = reopened
            .read(&summary.header.binding, cursor, 2, 1)
            .unwrap();
        assert!(page.omitted_ranges.is_empty());
        for chunk in page.chunks {
            assert_eq!(chunk.start, cursor);
            assert_eq!(chunk.end - chunk.start, chunk.bytes.len() as u64);
            match chunk.stream {
                NativeCommandOutputStream::Stdout => stdout.extend(chunk.bytes),
                NativeCommandOutputStream::Stderr => stderr.extend(chunk.bytes),
                NativeCommandOutputStream::Terminal => panic!("pipe became terminal"),
            }
        }
        assert!(page.next_cursor > cursor);
        cursor = page.next_cursor;
        if cursor == page.summary.observed_end {
            break;
        }
    }
    assert_eq!(stdout, expected_stdout);
    assert_eq!(stderr, expected_stderr);
    let new_manager = PersistentCommandManager::new(
        fixture.workspace.clone(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap()
    .with_native_session_owner(fixture.owner.clone())
    .unwrap()
    .with_native_output_store(reopened)
    .unwrap();
    assert_ne!(new_manager.generation(), fixture.manager.generation());
    assert!(new_manager.list_sessions().unwrap().is_empty());
    assert!(new_manager.inspect_pending_output(&command.id).is_err());
}

#[test]
fn registration_scope_failure_prevents_the_payload_before_any_process_effect() {
    let fixture = Fixture::new();
    let other = fixture.root.path().join("other-workspace");
    std::fs::create_dir(&other).unwrap();
    let manager = PersistentCommandManager::new(
        other.clone(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap()
    .with_native_session_owner(fixture.owner.clone())
    .unwrap()
    .with_native_output_store(fixture.output.clone())
    .unwrap();
    let error = manager
        .start(
            "/bin/sh",
            &["-c".into(), "printf launched > should-not-exist".into()],
            &CancellationToken::new(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("before command launch"));
    assert!(!other.join("should-not-exist").exists());
    assert!(manager.list_sessions().unwrap().is_empty());
    assert!(fixture
        .output
        .list(&fixture.owner, &fixture.workspace)
        .unwrap()
        .is_empty());
    let unbound = PersistentCommandManager::new(
        fixture.workspace.clone(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap()
    .with_native_output_store(fixture.output.clone())
    .err()
    .unwrap();
    assert!(unbound.to_string().contains("owned manager"));
}

#[test]
fn failed_output_ack_stops_the_real_process_without_exposing_unconfirmed_bytes() {
    let fixture = Fixture::new();
    let script = "printf 'ACK-prefix'; printf '%s' \"$$\" > process.pid; IFS= read -r line; printf 'UNACKED-SECRET'; IFS= read -r final; printf effect > should-not-exist";
    let command = fixture
        .manager
        .start(
            "/bin/sh",
            &["-c".into(), script.into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let runtime = runtime();
    runtime.block_on(async {
        wait_until(|| {
            bytes(
                &fixture
                    .manager
                    .inspect_pending_output(&command.id)
                    .unwrap()
                    .events,
                PersistentCommandStream::Stdout,
            ) == b"ACK-prefix"
        })
        .await;
        let committed = std::fs::read(fixture.sidecar()).unwrap();
        // Real malformed store failure, after the first producer ACK.
        std::fs::write(fixture.sidecar(), b"corrupt-output-store").unwrap();
        fixture
            .manager
            .write_stdin(
                &command.id,
                b"emit-unconfirmed\n",
                false,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        wait_until(|| {
            fixture
                .manager
                .inspect_pending_output(&command.id)
                .unwrap()
                .exit
                .is_some()
        })
        .await;
        let copy = fixture.manager.inspect_pending_output(&command.id).unwrap();
        assert_eq!(
            bytes(&copy.events, PersistentCommandStream::Stdout),
            b"ACK-prefix"
        );
        assert_eq!(copy.exit, Some(PersistentCommandExit::Terminated));
        assert!(copy
            .reader_error
            .as_ref()
            .unwrap()
            .contains("append was not confirmed"));
        assert!(
            fixture
                .manager
                .control_snapshot(&command.id)
                .unwrap()
                .stop_requested
        );
        assert!(fixture
            .manager
            .write_stdin(
                &command.id,
                b"execute-effect\n",
                true,
                &CancellationToken::new()
            )
            .await
            .is_err());
        assert!(!fixture.workspace.join("should-not-exist").exists());
        // Restore the last known good artifact to inspect only its ACKed prefix.
        // This models repair for this corruption fixture, not automatic recovery.
        std::fs::write(fixture.sidecar(), committed).unwrap();
        let reopened = fixture.reopen();
        let page = reopened
            .read(&fixture.binding(&command.id), 0, 64, 64)
            .unwrap();
        assert_eq!(
            page.chunks
                .iter()
                .flat_map(|chunk| chunk.bytes.clone())
                .collect::<Vec<_>>(),
            b"ACK-prefix"
        );
        assert_eq!(page.summary.observed_end, 10);
        assert!(
            page.summary.terminal.is_none(),
            "failed terminal write must not be invented on reopen"
        );
    });
}

#[test]
fn failed_terminal_ack_keeps_observed_live_exit_and_explicitly_incomplete_durable_history() {
    let fixture = Fixture::new();
    let command = fixture
        .manager
        .start(
            "/bin/sh",
            &[
                "-c".into(),
                "printf 'ACK-prefix'; IFS= read -r line; exit 9".into(),
            ],
            &CancellationToken::new(),
        )
        .unwrap();
    let runtime = runtime();
    runtime.block_on(async {
        wait_until(|| {
            bytes(
                &fixture
                    .manager
                    .inspect_pending_output(&command.id)
                    .unwrap()
                    .events,
                PersistentCommandStream::Stdout,
            ) == b"ACK-prefix"
        })
        .await;
        let committed = std::fs::read(fixture.sidecar()).unwrap();
        std::fs::write(fixture.sidecar(), b"corrupt-at-terminal").unwrap();
        fixture
            .manager
            .write_stdin(&command.id, b"exit-now\n", true, &CancellationToken::new())
            .await
            .unwrap();
        wait_until(|| {
            fixture
                .manager
                .inspect_pending_output(&command.id)
                .unwrap()
                .exit
                .is_some()
        })
        .await;
        let final_poll = fixture
            .manager
            .poll(&command.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap();
        assert!(final_poll.finished);
        assert_eq!(
            final_poll.exit,
            Some(PersistentCommandExit::Exited { code: Some(9) })
        );
        assert!(final_poll
            .reader_error
            .unwrap()
            .contains("terminal was not confirmed"));
        std::fs::write(fixture.sidecar(), committed).unwrap();
        let summary = fixture
            .reopen()
            .list(&fixture.owner, &fixture.workspace)
            .unwrap()
            .remove(0);
        assert_eq!(summary.observed_end, 10);
        assert!(summary.terminal.is_none());
        assert!(fixture.manager.list_sessions().unwrap().is_empty());
    });
}

#[test]
fn prepublication_failure_keeps_the_acked_artifact_unchanged_without_repair_or_retry() {
    let fixture = Fixture::new();
    let command = fixture.manager.start("/bin/sh", &["-c".into(), "printf 'ACK-prefix'; IFS= read -r line; printf 'UNACKED-SECRET'; IFS= read -r final; printf effect > should-not-exist".into()], &CancellationToken::new()).unwrap();
    let runtime = runtime();
    runtime.block_on(async {
        wait_until(|| {
            bytes(
                &fixture
                    .manager
                    .inspect_pending_output(&command.id)
                    .unwrap()
                    .events,
                PersistentCommandStream::Stdout,
            ) == b"ACK-prefix"
        })
        .await;
        let committed = std::fs::read(fixture.sidecar()).unwrap();
        let canary = fixture.workspace.join("foreign-canary");
        std::fs::write(&canary, b"must-not-change").unwrap();
        let linked_temporary = fixture.root.path().join("sessions").join(format!(
            ".{}.native-command-output.{}.tmp",
            fixture.owner,
            uuid::Uuid::new_v4()
        ));
        std::os::unix::fs::symlink(&canary, &linked_temporary).unwrap();
        fixture
            .manager
            .write_stdin(
                &command.id,
                b"emit-unconfirmed\n",
                false,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        wait_until(|| {
            fixture
                .manager
                .inspect_pending_output(&command.id)
                .unwrap()
                .exit
                .is_some()
        })
        .await;
        let copy = fixture.manager.inspect_pending_output(&command.id).unwrap();
        assert_eq!(
            bytes(&copy.events, PersistentCommandStream::Stdout),
            b"ACK-prefix"
        );
        assert_eq!(copy.exit, Some(PersistentCommandExit::Terminated));
        assert!(copy
            .reader_error
            .unwrap()
            .contains("append was not confirmed"));
        assert!(!fixture.workspace.join("should-not-exist").exists());
        // The publication failed before replacing the original synced artifact.
        assert_eq!(std::fs::read(fixture.sidecar()).unwrap(), committed);
        assert_eq!(std::fs::read(&canary).unwrap(), b"must-not-change");
        let reopened = fixture.reopen();
        let page = reopened
            .read(&fixture.binding(&command.id), 0, 64, 64)
            .unwrap();
        assert_eq!(
            page.chunks
                .iter()
                .flat_map(|chunk| chunk.bytes.clone())
                .collect::<Vec<_>>(),
            b"ACK-prefix"
        );
        assert_eq!(page.summary.observed_end, 10);
        assert!(page.summary.terminal.is_none());
        // Removing the test artifact does not retry a capture, synthesize an
        // exit receipt, or resume the process whose producer already failed.
        std::fs::remove_file(linked_temporary).unwrap();
        assert_eq!(
            reopened
                .read(&fixture.binding(&command.id), 0, 64, 64)
                .unwrap()
                .summary,
            page.summary
        );
        assert!(fixture
            .manager
            .write_stdin(
                &command.id,
                b"execute-effect\n",
                true,
                &CancellationToken::new()
            )
            .await
            .is_err());
    });
}

#[test]
fn durable_record_quota_ack_publishes_exact_omission_instead_of_unretained_raw_output() {
    let fixture = Fixture::new();
    let command = fixture
        .manager
        .start(
            "/bin/sh",
            &[
                "-c".into(),
                "printf P; IFS= read -r line; printf UNRETAINED; printf ERR >&2; exit 0".into(),
            ],
            &CancellationToken::new(),
        )
        .unwrap();
    let runtime = runtime();
    runtime.block_on(async {
        wait_until(|| bytes(&fixture.manager.inspect_pending_output(&command.id).unwrap().events, PersistentCommandStream::Stdout) == b"P").await;
        let quota_binding = fixture.binding(&uuid::Uuid::new_v4().to_string());
        fixture.output.register(&quota_binding, &["prepared-only-quota-fixture".into()], "host_unrestricted", false).unwrap();
        // Versioned on-disk boundary fixture: avoid thousands of full atomic
        // rewrites, while production decoding validates every seeded record.
        // This descriptor only reserves history; no second process was started.
        let mut envelope: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture.sidecar()).unwrap()).unwrap();
        let capture = envelope["payload"]["commands"].as_array_mut().unwrap().iter_mut().find(|capture| capture["header"]["binding"]["command_id"] == quota_binding.command_id).unwrap();
        let count = openide_core::MAX_NATIVE_COMMAND_OUTPUT_RECORDS - 1;
        capture["chunks"] = json!((0..count).map(|i| json!({"start":i,"end":i+1,"stream":"stdout","bytes":"/w=="})).collect::<Vec<_>>());
        capture["revision"] = json!(count);
        capture["observed_end"] = json!(count);
        capture["last_append"] = json!({"expected_cursor":count-1,"stream":"stdout","input_bytes":1,"digest":format!("{:x}",Sha256::digest([255])),"ack":{"revision":count,"observed_end":count,"retained_bytes":1,"omitted_bytes":0,"newly_appended":true}});
        let mut digest = Sha256::new();
        digest.update(b"OpenIDE.NativeCommandOutput.v1\0");
        digest.update(serde_json::to_vec(&envelope["payload"]).unwrap());
        envelope["checksum"] = json!(format!("{:x}", digest.finalize()));
        std::fs::write(fixture.sidecar(), serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert_eq!(fixture.output.list(&fixture.owner, &fixture.workspace).unwrap().len(), 2);
        fixture.manager.write_stdin(&command.id, b"output-after-quota\n", true, &CancellationToken::new()).await.unwrap();
        wait_until(|| fixture.manager.inspect_pending_output(&command.id).unwrap().exit.is_some()).await;
        let copy = fixture.manager.inspect_pending_output(&command.id).unwrap();
        assert_eq!(copy.events, vec![PersistentCommandEvent::Output{stream:PersistentCommandStream::Stdout,bytes:b"P".to_vec()},PersistentCommandEvent::Omitted{bytes:13}]);
        assert!(copy.reader_error.is_none());
        let final_poll = fixture.manager.poll(&command.id, Duration::ZERO, &CancellationToken::new()).await.unwrap();
        assert!(final_poll.finished);
        let page = fixture.reopen().read(&fixture.binding(&command.id), 0, 64, 64).unwrap();
        assert_eq!(page.summary.retained_bytes, 1);
        assert_eq!(page.summary.omitted_bytes, 13);
        assert_eq!(page.summary.observed_end, 14);
        assert_eq!(page.next_cursor, 14);
        assert_eq!(page.chunks.len(), 1);
        assert_eq!(page.chunks[0].bytes, b"P");
        assert_eq!(page.omitted_ranges, [openide_core::NativeCommandOutputRange{start:1,end:14}]);
        assert_eq!(page.summary.terminal.unwrap().exit_code, Some(0));
    });
}

#[test]
fn blocked_store_ack_prevents_real_pipe_bytes_from_becoming_live_output() {
    let fixture = Fixture::new();
    let command = fixture.manager.start("/bin/sh", &["-c".into(), "IFS= read -r line; printf 'wait-for-ACK'; printf emitted > emitted.marker; IFS= read -r final; exit 0".into()], &CancellationToken::new()).unwrap();
    let lock_path = fixture
        .root
        .path()
        .join("sessions")
        .join(format!(".{}.native-command-output.lock", fixture.owner));
    let output_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    output_lock.lock().unwrap();
    let runtime = runtime();
    runtime.block_on(async {
        fixture
            .manager
            .write_stdin(&command.id, b"emit-now\n", false, &CancellationToken::new())
            .await
            .unwrap();
        wait_until(|| fixture.workspace.join("emitted.marker").exists()).await;
        // The actual process wrote its stdout pipe. The producer is waiting
        // for the store lock/ACK, so neither inspector nor poll may see raw data.
        assert!(fixture
            .manager
            .inspect_pending_output(&command.id)
            .unwrap()
            .events
            .is_empty());
        let before_ack = fixture
            .manager
            .poll(&command.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap();
        assert!(before_ack.events.is_empty());
        assert!(!before_ack.finished);
        output_lock.unlock().unwrap();
        wait_until(|| {
            bytes(
                &fixture
                    .manager
                    .inspect_pending_output(&command.id)
                    .unwrap()
                    .events,
                PersistentCommandStream::Stdout,
            ) == b"wait-for-ACK"
        })
        .await;
        let summary = fixture
            .output
            .list(&fixture.owner, &fixture.workspace)
            .unwrap()
            .remove(0);
        assert_eq!(summary.retained_bytes, 12);
        fixture
            .manager
            .write_stdin(&command.id, b"exit-now\n", true, &CancellationToken::new())
            .await
            .unwrap();
        wait_until(|| {
            fixture
                .manager
                .inspect_pending_output(&command.id)
                .unwrap()
                .exit
                .is_some()
        })
        .await;
        assert!(fixture
            .manager
            .inspect_pending_output(&command.id)
            .unwrap()
            .reader_error
            .is_none());
    });
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
#[ignore = "requires trusted Bubblewrap and working Linux namespaces/seccomp/pidfd/close_range"]
fn actual_linux_offline_output_capture_survives_turn_runtimes_and_reopens_without_process() {
    let fixture = Fixture::with_isolation(CommandIsolation::WorkspaceWriteOffline);
    let source = fixture.workspace.join("probe.rs");
    let binary = fixture.workspace.join("probe");
    std::fs::write(&source, include_str!("fixtures/native_output_echo.rs")).unwrap();
    let compile =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2021")
            .arg(source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
    assert!(
        compile.status.success(),
        "probe compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let command = fixture
        .manager
        .start(
            binary.to_str().unwrap(),
            &["echo".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let first = runtime();
    first.block_on(async {
        fixture
            .manager
            .write_stdin(
                &command.id,
                "first café\n".as_bytes(),
                false,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        wait_until(|| {
            bytes(
                &fixture
                    .manager
                    .inspect_pending_output(&command.id)
                    .unwrap()
                    .events,
                PersistentCommandStream::Stdout,
            ) == "first café\n".as_bytes()
        })
        .await;
    });
    drop(first);
    let second = runtime();
    second.block_on(async {
        fixture
            .manager
            .write_stdin(
                &command.id,
                b"second \0\xffbinary\n",
                true,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        wait_until(|| {
            fixture
                .manager
                .inspect_pending_output(&command.id)
                .unwrap()
                .exit
                .is_some()
        })
        .await;
        let poll = fixture
            .manager
            .poll(&command.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap();
        assert!(poll.finished && poll.reader_error.is_none());
        assert_eq!(
            poll.exit,
            Some(PersistentCommandExit::Exited { code: Some(0) })
        );
    });
    let mut expected = "first café\n".as_bytes().to_vec();
    expected.extend_from_slice(b"second \0\xffbinary\n");
    assert_eq!(
        std::fs::read(fixture.workspace.join("received")).unwrap(),
        expected
    );
    assert!(fixture.manager.list_sessions().unwrap().is_empty());
    let reopened = fixture.reopen();
    let page = reopened
        .read(&fixture.binding(&command.id), 0, 1024, 64)
        .unwrap();
    assert_eq!(page.summary.header.isolation, "workspace_write_offline");
    assert_eq!(
        page.chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(page.omitted_ranges.is_empty());
    assert_eq!(page.summary.terminal.unwrap().exit_code, Some(0));
    let new_manager = PersistentCommandManager::new(
        fixture.workspace.clone(),
        CommandIsolation::WorkspaceWriteOffline,
        PersistentCommandLimits::default(),
    )
    .unwrap()
    .with_native_session_owner(fixture.owner.clone())
    .unwrap()
    .with_native_output_store(reopened)
    .unwrap();
    assert_ne!(new_manager.generation(), fixture.manager.generation());
    assert!(new_manager.list_sessions().unwrap().is_empty());
    assert!(new_manager.inspect_pending_output(&command.id).is_err());
}
