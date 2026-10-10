use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    sessions: Arc<SessionStore>,
    store: NativeCommandOutputStore,
    binding: NativeCommandOutputBinding,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let workspace = fs::canonicalize(workspace).unwrap();
        let sessions = Arc::new(SessionStore::open(directory.path().join("sessions")).unwrap());
        let session = sessions.create(&workspace, "native").unwrap();
        let binding = NativeCommandOutputBinding {
            session_id: session.id,
            workspace,
            manager_generation: Uuid::new_v4().to_string(),
            command_id: Uuid::new_v4().to_string(),
        };
        let store = NativeCommandOutputStore::new(sessions.clone());
        Self {
            _directory: directory,
            sessions,
            store,
            binding,
        }
    }
    fn register(&self) -> NativeCommandOutputRegistration {
        self.store
            .register(
                &self.binding,
                &["probe".into(), "exact\r\nargument".into()],
                "host_unrestricted",
                false,
            )
            .unwrap()
    }
    fn reopen(&self) -> NativeCommandOutputStore {
        NativeCommandOutputStore::new(Arc::new(
            SessionStore::open(self.sessions.storage_root()).unwrap(),
        ))
    }
}
fn exited() -> NativeCommandOutputTerminal {
    NativeCommandOutputTerminal {
        exit_observed: true,
        exit_kind: Some(NativeCommandOutputExit::Exited),
        exit_code: Some(0),
        reader_error: None,
    }
}

#[test]
fn durable_command_output_binary_pages_terminal_and_exact_retry_survive_reopen() {
    let fixture = Fixture::new();
    let registration = fixture.register();
    assert!(registration.newly_registered);
    assert!(registration.summary.terminal.is_none());
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.binding.session_id)
        .unwrap();
    let first = fixture
        .store
        .append(
            &fixture.binding,
            0,
            NativeCommandOutputStream::Stdout,
            &[0, 255, 0xf0, 0x9f],
        )
        .unwrap();
    assert_eq!(
        (
            first.observed_end,
            first.retained_bytes,
            first.omitted_bytes
        ),
        (4, 4, 0)
    );
    let reopened = fixture.reopen();
    let repeated = reopened
        .append(
            &fixture.binding,
            0,
            NativeCommandOutputStream::Stdout,
            &[0, 255, 0xf0, 0x9f],
        )
        .unwrap();
    assert!(!repeated.newly_appended);
    assert_eq!(repeated.revision, 1);
    assert!(matches!(
        reopened.append(
            &fixture.binding,
            0,
            NativeCommandOutputStream::Stderr,
            &[0, 255, 0xf0, 0x9f]
        ),
        Err(NativeCommandOutputError::Conflict)
    ));
    fixture
        .store
        .append(
            &fixture.binding,
            4,
            NativeCommandOutputStream::Stderr,
            &[0x98, 0x80, b'\r', b'\n'],
        )
        .unwrap();
    let page = reopened.read(&fixture.binding, 1, 4, 2).unwrap();
    assert_eq!(page.next_cursor, 5);
    assert_eq!(
        page.chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.clone())
            .collect::<Vec<_>>(),
        [255, 0xf0, 0x9f, 0x98]
    );
    assert_eq!(page.chunks[0].stream, NativeCommandOutputStream::Stdout);
    assert_eq!(page.chunks[1].stream, NativeCommandOutputStream::Stderr);
    let terminal = fixture
        .store
        .finish(&fixture.binding, 8, &exited())
        .unwrap();
    assert_eq!((terminal.revision, terminal.observed_end), (3, 8));
    assert_eq!(
        reopened.finish(&fixture.binding, 8, &exited()).unwrap(),
        terminal
    );
    let last = reopened.read(&fixture.binding, 8, 16, 4).unwrap();
    assert!(last.chunks.is_empty() && last.omitted_ranges.is_empty());
    assert_eq!(last.summary, terminal);
    assert_eq!(last.next_cursor, 8);
    assert!(
        !reopened
            .register(
                &fixture.binding,
                &["probe".into(), "exact\r\nargument".into()],
                "host_unrestricted",
                false
            )
            .unwrap()
            .newly_registered
    );
    assert!(matches!(
        fixture.store.append(
            &fixture.binding,
            8,
            NativeCommandOutputStream::Stdout,
            b"late"
        ),
        Err(NativeCommandOutputError::Conflict)
    ));
    assert_eq!(
        reopened
            .list(&fixture.binding.session_id, &fixture.binding.workspace)
            .unwrap(),
        vec![terminal]
    );
    drop(lease);
}

#[test]
fn durable_command_output_two_stores_serialize_cursor_and_keep_capture_after_archive() {
    use std::sync::Barrier;
    let fixture = Fixture::new();
    fixture.register();
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for bytes in [b"first".to_vec(), b"other".to_vec()] {
        let store = fixture.reopen();
        let binding = fixture.binding.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            (
                bytes.clone(),
                store.append(&binding, 0, NativeCommandOutputStream::Stdout, &bytes),
            )
        }));
    }
    barrier.wait();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|(_, result)| matches!(result, Err(NativeCommandOutputError::Conflict)))
            .count(),
        1
    );
    let (winner, ack) = results.iter().find(|(_, result)| result.is_ok()).unwrap();
    assert!(ack.as_ref().unwrap().newly_appended);
    let page = fixture.reopen().read(&fixture.binding, 0, 32, 8).unwrap();
    assert_eq!(page.chunks[0].bytes, *winner);
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.binding.session_id)
        .unwrap();
    fixture
        .sessions
        .set_archived(&fixture.binding.session_id, true)
        .unwrap();
    assert!(
        !fixture
            .reopen()
            .register(
                &fixture.binding,
                &["probe".into(), "exact\r\nargument".into()],
                "host_unrestricted",
                false
            )
            .unwrap()
            .newly_registered
    );
    assert!(matches!(
        fixture.store.register(
            &fixture.binding,
            &["new".into()],
            "host_unrestricted",
            false
        ),
        Err(NativeCommandOutputError::Conflict)
    ));
    // Neither the turn lease nor archive blocks closure of an existing capture.
    fixture
        .reopen()
        .append(
            &fixture.binding,
            5,
            NativeCommandOutputStream::Stderr,
            b"after archive",
        )
        .unwrap();
    let terminal = fixture
        .store
        .finish(
            &fixture.binding,
            18,
            &NativeCommandOutputTerminal {
                exit_observed: true,
                exit_kind: Some(NativeCommandOutputExit::Terminated),
                exit_code: None,
                reader_error: Some("capture reader failed after its confirmed prefix".into()),
            },
        )
        .unwrap();
    assert_eq!(terminal.observed_end, 18);
    assert_eq!(
        fixture
            .reopen()
            .list(&fixture.binding.session_id, &fixture.binding.workspace)
            .unwrap(),
        [terminal]
    );
    assert!(matches!(
        fixture.sessions.delete(&fixture.binding.session_id),
        Err(StoreError::NativeSessionBusy(_))
    ));
    drop(lease);
    assert!(fixture
        .sessions
        .delete(&fixture.binding.session_id)
        .unwrap());
    assert!(matches!(
        fixture.reopen().append(
            &fixture.binding,
            18,
            NativeCommandOutputStream::Stdout,
            b"late"
        ),
        Err(NativeCommandOutputError::Store(StoreError::UnknownSession(
            _
        )))
    ));
    assert!(!ledger_path(fixture.sessions.storage_root(), &fixture.binding.session_id).exists());
}

#[test]
fn durable_command_output_writer_delete_race_cannot_recreate_sidecar() {
    use std::sync::Barrier;
    for _ in 0..8 {
        let fixture = Fixture::new();
        fixture.register();
        let barrier = Arc::new(Barrier::new(2));
        let store = fixture.reopen();
        let binding = fixture.binding.clone();
        let thread_barrier = barrier.clone();
        let writer = std::thread::spawn(move || {
            thread_barrier.wait();
            store.append(
                &binding,
                0,
                NativeCommandOutputStream::Stdout,
                b"captured before deletion only",
            )
        });
        barrier.wait();
        assert!(fixture
            .sessions
            .delete(&fixture.binding.session_id)
            .unwrap());
        match writer.join().unwrap() {
            Ok(ack) => assert_eq!(ack.observed_end, 29),
            Err(NativeCommandOutputError::Store(StoreError::UnknownSession(_))) => {}
            Err(error) => panic!("unexpected race result: {error}"),
        }
        assert!(
            !ledger_path(fixture.sessions.storage_root(), &fixture.binding.session_id).exists()
        );
        assert!(fixture
            .reopen()
            .list(&fixture.binding.session_id, &fixture.binding.workspace)
            .is_err());
    }
}

// Large fixtures represent confirmed historical chunks with their real bytes;
// public append/read/finish then test the independent boundary without thousands
// of expensive complete-file publications during setup.
fn seed_retained(fixture: &Fixture, records: usize, chunk_size: usize) {
    fixture.register();
    let session = fixture
        .sessions
        .load(&fixture.binding.session_id)
        .unwrap()
        .unwrap()
        .session;
    let mut payload = read_payload(fixture.sessions.storage_root(), &session).unwrap();
    let capture = &mut payload.commands[0];
    let bytes = vec![255; chunk_size];
    for index in 0..records {
        let start = (index * chunk_size) as u64;
        capture.chunks.push(NativeCommandOutputChunk {
            start,
            end: start + chunk_size as u64,
            stream: NativeCommandOutputStream::Stdout,
            bytes: bytes.clone(),
        });
    }
    capture.observed_end = (records * chunk_size) as u64;
    capture.revision = records as u64;
    capture.last_append = Some(AppendReceipt {
        expected_cursor: capture.observed_end - chunk_size as u64,
        stream: NativeCommandOutputStream::Stdout,
        input_bytes: chunk_size,
        digest: hex_digest(&bytes),
        ack: NativeCommandOutputAppendAck {
            revision: capture.revision,
            observed_end: capture.observed_end,
            retained_bytes: chunk_size,
            omitted_bytes: 0,
            newly_appended: true,
        },
    });
    validate_payload(&payload, &session).unwrap();
    write_payload(fixture.sessions.storage_root(), &payload).unwrap();
}

#[test]
fn durable_command_output_raw_and_record_quotas_keep_prefix_and_exact_omitted_tail() {
    for (records, size) in [
        (MAX_NATIVE_COMMAND_OUTPUT_RAW_BYTES / 8192, 8192),
        (MAX_NATIVE_COMMAND_OUTPUT_RECORDS, 1),
    ] {
        let fixture = Fixture::new();
        seed_retained(&fixture, records, size);
        let cursor = (records * size) as u64;
        let ack = fixture
            .store
            .append(
                &fixture.binding,
                cursor,
                NativeCommandOutputStream::Stderr,
                b"actually observed but omitted",
            )
            .unwrap();
        assert_eq!((ack.retained_bytes, ack.omitted_bytes), (0, 29));
        let repeated = fixture
            .reopen()
            .append(
                &fixture.binding,
                cursor,
                NativeCommandOutputStream::Stderr,
                b"actually observed but omitted",
            )
            .unwrap();
        assert!(!repeated.newly_appended);
        assert!(matches!(
            fixture.store.append(
                &fixture.binding,
                cursor,
                NativeCommandOutputStream::Stderr,
                b"actually observed and changed"
            ),
            Err(NativeCommandOutputError::Conflict)
        ));
        let next = fixture
            .store
            .append(
                &fixture.binding,
                cursor + 29,
                NativeCommandOutputStream::Stdout,
                b"tail",
            )
            .unwrap();
        assert_eq!((next.retained_bytes, next.omitted_bytes), (0, 4));
        let page = fixture
            .reopen()
            .read(&fixture.binding, cursor - 1, 1, 2)
            .unwrap();
        assert_eq!(page.chunks[0].bytes, [255]);
        assert_eq!(
            page.omitted_ranges,
            [NativeCommandOutputRange {
                start: cursor,
                end: cursor + 33
            }]
        );
        assert_eq!(page.next_cursor, cursor + 33);
        assert_eq!(page.summary.retained_bytes, cursor);
        assert_eq!(page.summary.omitted_bytes, 33);
        let terminal = fixture
            .store
            .finish(&fixture.binding, cursor + 33, &exited())
            .unwrap();
        let empty = fixture
            .reopen()
            .read(&fixture.binding, cursor + 33, 1, 1)
            .unwrap();
        assert_eq!(empty.summary, terminal);
        assert!(empty.chunks.is_empty());
    }
}

#[test]
fn durable_command_output_descriptor_and_serialized_capacity_are_independent_of_raw_quota() {
    let fixture = Fixture::new();
    let mut binding = fixture.binding.clone();
    for index in 0..MAX_NATIVE_COMMAND_OUTPUT_COMMANDS {
        binding.command_id = Uuid::new_v4().to_string();
        fixture
            .store
            .register(
                &binding,
                &[format!("probe-{index}")],
                "host_unrestricted",
                false,
            )
            .unwrap();
    }
    binding.command_id = Uuid::new_v4().to_string();
    assert!(matches!(
        fixture
            .store
            .register(&binding, &["probe".into()], "host_unrestricted", false),
        Err(NativeCommandOutputError::Quota)
    ));
    assert_eq!(
        fixture
            .reopen()
            .list(&binding.session_id, &binding.workspace)
            .unwrap()
            .len(),
        64
    );

    let fixture = Fixture::new();
    // Escaped control bytes make the serialized budget bind while raw retained
    // bytes and record counts remain zero. The entire original argv is retained.
    let argv = vec!["p".into(), "\u{1}".repeat(MAX_ARGV_BYTES - 1)];
    fixture
        .store
        .register(&fixture.binding, &argv, "host_unrestricted", false)
        .unwrap();
    let session = fixture
        .sessions
        .load(&fixture.binding.session_id)
        .unwrap()
        .unwrap()
        .session;
    let mut payload = read_payload(fixture.sessions.storage_root(), &session).unwrap();
    let template = payload.commands[0].clone();
    // Seed a valid reopened near-capacity descriptor set in one publication.
    for _ in 1..MAX_NATIVE_COMMAND_OUTPUT_COMMANDS {
        let mut capture = template.clone();
        capture.header.binding.command_id = Uuid::new_v4().to_string();
        payload.commands.push(capture);
    }
    while !has_capacity(&payload).unwrap() {
        payload.commands.pop().unwrap();
    }
    validate_payload(&payload, &session).unwrap();
    write_payload(fixture.sessions.storage_root(), &payload).unwrap();
    let accepted: Vec<_> = payload
        .commands
        .iter()
        .map(|c| c.header.binding.clone())
        .collect();
    assert!(!accepted.is_empty() && accepted.len() < MAX_NATIVE_COMMAND_OUTPUT_COMMANDS);
    let mut rejected_binding = fixture.binding.clone();
    rejected_binding.command_id = Uuid::new_v4().to_string();
    assert!(matches!(
        fixture
            .store
            .register(&rejected_binding, &argv, "host_unrestricted", false),
        Err(NativeCommandOutputError::Quota)
    ));
    let binding = &accepted[0];
    let mut cursor = 0;
    loop {
        let ack = fixture
            .store
            .append(
                binding,
                cursor,
                NativeCommandOutputStream::Stdout,
                &vec![255; MAX_NATIVE_COMMAND_OUTPUT_APPEND_BYTES],
            )
            .unwrap();
        cursor = ack.observed_end;
        if ack.omitted_bytes > 0 {
            break;
        }
        assert!(cursor < MAX_NATIVE_COMMAND_OUTPUT_RAW_BYTES as u64);
    }
    let summary = fixture
        .reopen()
        .list(&binding.session_id, &binding.workspace)
        .unwrap()
        .into_iter()
        .find(|summary| summary.header.binding == *binding)
        .unwrap();
    assert!(summary.retained_bytes < MAX_NATIVE_COMMAND_OUTPUT_RAW_BYTES as u64);
    assert!(summary.omitted_bytes > 0);
    assert!(
        fs::metadata(ledger_path(
            fixture.sessions.storage_root(),
            &binding.session_id
        ))
        .unwrap()
        .len()
            <= MAX_NATIVE_COMMAND_OUTPUT_FILE_BYTES as u64
    );
    for binding in [accepted[0].clone(), accepted.last().unwrap().clone()] {
        let observed = fixture
            .store
            .read(&binding, 0, 1, 1)
            .unwrap()
            .summary
            .observed_end;
        fixture
            .store
            .finish(
                &binding,
                observed,
                &NativeCommandOutputTerminal {
                    exit_observed: true,
                    exit_kind: Some(NativeCommandOutputExit::WaitFailed {
                        error: "\u{1}".repeat(MAX_ERROR_BYTES),
                    }),
                    exit_code: None,
                    reader_error: Some("\u{2}".repeat(MAX_ERROR_BYTES)),
                },
            )
            .unwrap();
    }
}

#[test]
fn durable_command_output_semantic_corruption_is_rejected_even_with_recomputed_checksum() {
    let fixture = Fixture::new();
    fixture.register();
    fixture
        .store
        .append(
            &fixture.binding,
            0,
            NativeCommandOutputStream::Stdout,
            b"exact bytes",
        )
        .unwrap();
    let session = fixture
        .sessions
        .load(&fixture.binding.session_id)
        .unwrap()
        .unwrap()
        .session;
    let original = read_payload(fixture.sessions.storage_root(), &session).unwrap();
    let path = ledger_path(fixture.sessions.storage_root(), &fixture.binding.session_id);
    for corruption in 0..6 {
        let mut payload = original.clone();
        let capture = &mut payload.commands[0];
        match corruption {
            0 => {
                let receipt = capture.last_append.as_mut().unwrap();
                receipt.ack.retained_bytes -= 1;
                receipt.ack.omitted_bytes += 1;
            }
            1 => capture.last_append.as_mut().unwrap().digest = "0".repeat(64),
            2 => capture.last_append.as_mut().unwrap().stream = NativeCommandOutputStream::Stderr,
            3 => capture.chunks[0].stream = NativeCommandOutputStream::Terminal,
            4 => capture.header.binding.session_id = "foreign".into(),
            5 => capture.chunks[0].end += 1,
            _ => unreachable!(),
        }
        let bytes = encoded(&payload).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(fixture
            .reopen()
            .list(&fixture.binding.session_id, &fixture.binding.workspace)
            .is_err());
        assert!(fixture
            .sessions
            .delete(&fixture.binding.session_id)
            .is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(fixture
            .sessions
            .load(&fixture.binding.session_id)
            .unwrap()
            .is_some());
    }
    fs::write(&path, encoded(&original).unwrap()).unwrap();
    let mut envelope: Envelope = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    envelope.checksum = "0".repeat(64);
    let bad_checksum = serde_json::to_vec(&envelope).unwrap();
    fs::write(&path, &bad_checksum).unwrap();
    assert!(matches!(
        fixture.reopen().read(&fixture.binding, 0, 32, 4),
        Err(NativeCommandOutputError::Invalid)
    ));
    assert_eq!(fs::read(&path).unwrap(), bad_checksum);
    fs::write(&path, b"{").unwrap();
    assert!(fixture
        .sessions
        .delete(&fixture.binding.session_id)
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{");
}

#[test]
fn durable_command_output_limits_terminal_kinds_and_stream_domains_are_exact() {
    let fixture = Fixture::new();
    fixture.register();
    assert!(matches!(
        fixture.store.append(
            &fixture.binding,
            0,
            NativeCommandOutputStream::Terminal,
            b"wrong pipe stream"
        ),
        Err(NativeCommandOutputError::Invalid)
    ));
    assert!(matches!(
        fixture
            .store
            .append(&fixture.binding, 0, NativeCommandOutputStream::Stdout, &[]),
        Err(NativeCommandOutputError::Invalid)
    ));
    assert!(matches!(
        fixture.store.append(
            &fixture.binding,
            u64::MAX,
            NativeCommandOutputStream::Stdout,
            b"overflow"
        ),
        Err(NativeCommandOutputError::Conflict)
    ));
    assert!(matches!(
        fixture.store.read(
            &fixture.binding,
            0,
            MAX_NATIVE_COMMAND_OUTPUT_PAGE_BYTES + 1,
            1
        ),
        Err(NativeCommandOutputError::Invalid)
    ));
    let invalid = NativeCommandOutputTerminal {
        exit_observed: false,
        exit_kind: None,
        exit_code: None,
        reader_error: None,
    };
    assert!(matches!(
        fixture.store.finish(&fixture.binding, 0, &invalid),
        Err(NativeCommandOutputError::Invalid)
    ));
    let mut invalid = exited();
    invalid.reader_error = Some("x".repeat(MAX_ERROR_BYTES + 1));
    assert!(matches!(
        fixture.store.finish(&fixture.binding, 0, &invalid),
        Err(NativeCommandOutputError::Invalid)
    ));
    for kind in [
        NativeCommandOutputExit::Exited,
        NativeCommandOutputExit::Terminated,
        NativeCommandOutputExit::TimedOut,
        NativeCommandOutputExit::WaitFailed {
            error: "exact wait failure".into(),
        },
    ] {
        let mut binding = fixture.binding.clone();
        binding.command_id = Uuid::new_v4().to_string();
        fixture
            .store
            .register(&binding, &["pty-probe".into()], "host_unrestricted", true)
            .unwrap();
        assert!(matches!(
            fixture.store.append(
                &binding,
                0,
                NativeCommandOutputStream::Stdout,
                b"wrong pty stream"
            ),
            Err(NativeCommandOutputError::Invalid)
        ));
        fixture
            .store
            .append(&binding, 0, NativeCommandOutputStream::Terminal, b"PTY\r\n")
            .unwrap();
        let terminal = NativeCommandOutputTerminal {
            exit_observed: true,
            exit_kind: Some(kind),
            exit_code: None,
            reader_error: None,
        };
        assert_eq!(
            fixture
                .store
                .finish(&binding, 5, &terminal)
                .unwrap()
                .terminal,
            Some(terminal)
        );
    }
    let error = NativeCommandOutputTerminal {
        exit_observed: false,
        exit_kind: None,
        exit_code: None,
        reader_error: Some("reader failed; exit was not observed".into()),
    };
    assert_eq!(
        fixture
            .store
            .finish(&fixture.binding, 0, &error)
            .unwrap()
            .terminal,
        Some(error)
    );
    let argv: Vec<String> = std::iter::once("p".to_owned())
        .chain(std::iter::repeat_n("".to_owned(), 256))
        .collect();
    let mut binding = fixture.binding.clone();
    binding.command_id = Uuid::new_v4().to_string();
    fixture
        .store
        .register(&binding, &argv, "own-provider-isolation-descriptor", false)
        .unwrap();
    assert!(fixture
        .store
        .register(&binding, &vec!["p".into(); 258], "host_unrestricted", false)
        .is_err());
}

#[test]
fn durable_command_output_owned_temporaries_cleanup_and_orphan_evidence_are_preserved() {
    let fixture = Fixture::new();
    fixture.register();
    let root = fixture.sessions.storage_root();
    let owned = root.join(format!(
        ".{}.native-command-output.{}.tmp",
        fixture.binding.session_id,
        Uuid::new_v4()
    ));
    let unrelated = root.join(format!(
        ".{}.native-command-output.not-an-owned-uuid.tmp",
        fixture.binding.session_id
    ));
    fs::write(&owned, b"unpublished partial bytes").unwrap();
    fs::write(&unrelated, b"unrelated evidence").unwrap();
    fixture
        .store
        .append(
            &fixture.binding,
            0,
            NativeCommandOutputStream::Stdout,
            b"confirmed",
        )
        .unwrap();
    assert!(!owned.exists());
    assert_eq!(fs::read(&unrelated).unwrap(), b"unrelated evidence");
    fs::remove_file(root.join(format!("{}.session.json", fixture.binding.session_id))).unwrap();
    let bytes = fs::read(ledger_path(root, &fixture.binding.session_id)).unwrap();
    assert!(matches!(
        fixture.sessions.delete(&fixture.binding.session_id),
        Err(StoreError::NativeCommandOutputBlocked(_))
    ));
    assert_eq!(
        fs::read(ledger_path(root, &fixture.binding.session_id)).unwrap(),
        bytes
    );
    assert!(fixture
        .reopen()
        .append(
            &fixture.binding,
            9,
            NativeCommandOutputStream::Stdout,
            b"late"
        )
        .is_err());
}

#[cfg(unix)]
#[test]
fn durable_command_output_links_are_not_followed_and_new_files_are_private() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    for linked in ["ledger", "lock", "temporary"] {
        let fixture = Fixture::new();
        let root = fixture.sessions.storage_root();
        let external = fixture.binding.workspace.join("external-private-data");
        fs::write(&external, b"external evidence").unwrap();
        let path = match linked {
            "ledger" => ledger_path(root, &fixture.binding.session_id),
            "lock" => root.join(format!(
                ".{}.native-command-output.lock",
                fixture.binding.session_id
            )),
            "temporary" => root.join(format!(
                ".{}.native-command-output.{}.tmp",
                fixture.binding.session_id,
                Uuid::new_v4()
            )),
            _ => unreachable!(),
        };
        symlink(&external, &path).unwrap();
        assert!(fixture
            .store
            .register(
                &fixture.binding,
                &["probe".into()],
                "host_unrestricted",
                false
            )
            .is_err());
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read(external).unwrap(), b"external evidence");
    }
    let fixture = Fixture::new();
    fixture.register();
    assert_eq!(
        fs::metadata(ledger_path(
            fixture.sessions.storage_root(),
            &fixture.binding.session_id
        ))
        .unwrap()
        .permissions()
        .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn durable_command_output_tree_delete_validates_all_captures_and_fences_late_child_writer() {
    use std::sync::Barrier;
    let fixture = Fixture::new();
    fixture.register();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.binding.session_id)
        .unwrap();
    let child = fixture
        .sessions
        .create_native_child(&lease, "output-child", &"a".repeat(64), 3)
        .unwrap();
    drop(lease);
    let child_binding = NativeCommandOutputBinding {
        session_id: child.id.clone(),
        ..fixture.binding.clone()
    };
    fixture
        .store
        .register(
            &child_binding,
            &["child-probe".into()],
            "host_unrestricted",
            false,
        )
        .unwrap();
    fixture
        .store
        .append(
            &child_binding,
            0,
            NativeCommandOutputStream::Stdout,
            b"child confirmed prefix",
        )
        .unwrap();
    fs::write(fixture.sessions.storage_root().join("subagents.registry.json"), serde_json::to_vec(&serde_json::json!({
        "version": 2, "children": { child.id.clone(): {"status":"completed", "cancellation_requested":false} }, "inputs": {}
    })).unwrap()).unwrap();
    let other = fixture
        .sessions
        .create(&fixture.binding.workspace, "native")
        .unwrap();
    let other_binding = NativeCommandOutputBinding {
        session_id: other.id.clone(),
        ..fixture.binding.clone()
    };
    fixture
        .store
        .register(
            &other_binding,
            &["other-probe".into()],
            "host_unrestricted",
            false,
        )
        .unwrap();
    let root = fixture.sessions.storage_root();
    let child_path = ledger_path(root, &child.id);
    let valid_child = fs::read(&child_path).unwrap();
    fs::write(&child_path, b"broken capture").unwrap();
    assert!(fixture
        .sessions
        .delete_native_tree(&fixture.binding.session_id)
        .is_err());
    assert!(fixture
        .sessions
        .pending_native_deletion_roots()
        .unwrap()
        .is_empty());
    assert!(fixture.sessions.load(&child.id).unwrap().is_some());
    assert_eq!(fs::read(&child_path).unwrap(), b"broken capture");
    fs::write(&child_path, valid_child).unwrap();
    let own_temporary = root.join(format!(
        ".{}.native-command-output.{}.tmp",
        child.id,
        Uuid::new_v4()
    ));
    fs::write(&own_temporary, b"unpublished partial capture").unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = barrier.clone();
    let reopened = fixture.reopen();
    let late_binding = child_binding.clone();
    let writer = std::thread::spawn(move || {
        writer_barrier.wait();
        reopened.append(
            &late_binding,
            22,
            NativeCommandOutputStream::Stderr,
            b"late child bytes",
        )
    });
    barrier.wait();
    assert!(fixture
        .sessions
        .delete_native_tree(&fixture.binding.session_id)
        .unwrap());
    match writer.join().unwrap() {
        Ok(ack) => assert_eq!(ack.observed_end, 38),
        Err(NativeCommandOutputError::Store(
            StoreError::UnknownSession(_) | StoreError::NativeDeletionPending(_),
        )) => {}
        Err(error) => panic!("unexpected child deletion race: {error}"),
    }
    assert!(!own_temporary.exists());
    for id in [&fixture.binding.session_id, &child.id] {
        assert!(!ledger_path(root, id).exists());
        assert!(fixture.sessions.load(id).unwrap().is_none());
    }
    assert_eq!(
        fixture
            .reopen()
            .list(&other.id, &fixture.binding.workspace)
            .unwrap()
            .len(),
        1
    );
    assert!(fixture
        .reopen()
        .append(
            &child_binding,
            22,
            NativeCommandOutputStream::Stdout,
            b"resurrection"
        )
        .is_err());
    assert!(!child_path.exists());
}

#[test]
fn durable_command_output_reordered_json_is_valid_and_foreign_envelope_is_preserved() {
    let fixture = Fixture::new();
    fixture.register();
    fixture
        .store
        .append(
            &fixture.binding,
            0,
            NativeCommandOutputStream::Stdout,
            b"scope",
        )
        .unwrap();
    let path = ledger_path(fixture.sessions.storage_root(), &fixture.binding.session_id);
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert_eq!(
        fixture
            .reopen()
            .read(&fixture.binding, 0, 32, 4)
            .unwrap()
            .chunks[0]
            .bytes,
        b"scope"
    );
    let foreign = Fixture::new();
    foreign.register();
    let foreign_bytes = fs::read(ledger_path(
        foreign.sessions.storage_root(),
        &foreign.binding.session_id,
    ))
    .unwrap();
    fs::write(&path, &foreign_bytes).unwrap();
    assert!(fixture
        .reopen()
        .list(&fixture.binding.session_id, &fixture.binding.workspace)
        .is_err());
    assert!(fixture
        .sessions
        .delete(&fixture.binding.session_id)
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), foreign_bytes);
}

#[cfg(unix)]
#[test]
fn durable_command_output_real_process_cuts_preserve_register_append_omission_and_terminal() {
    use std::os::unix::process::ExitStatusExt;
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    for action in ["register", "append", "omission", "finish"] {
        for stage in ["temporary-synced", "renamed", "directory-synced"] {
            let fixture = Fixture::new();
            match action {
                "register" => {}
                "omission" => seed_retained(&fixture, MAX_NATIVE_COMMAND_OUTPUT_RECORDS, 1),
                _ => {
                    fixture.register();
                }
            }
            let ready = fixture._directory.path().join("cut-ready");
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "native_command_output::tests::output_publication_cut_child",
                    "--nocapture",
                ])
                .env(
                    "OPENIDE_TEST_OUTPUT_CUT_ROOT",
                    fixture.sessions.storage_root(),
                )
                .env(
                    "OPENIDE_TEST_OUTPUT_CUT_BINDING",
                    serde_json::to_string(&fixture.binding).unwrap(),
                )
                .env("OPENIDE_TEST_OUTPUT_CUT_ACTION", action)
                .env("OPENIDE_TEST_OUTPUT_CUT_STAGE", stage)
                .env(
                    "OPENIDE_TEST_OUTPUT_CUT_SESSION",
                    &fixture.binding.session_id,
                )
                .env("OPENIDE_TEST_OUTPUT_CUT_READY", &ready)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready.exists() {
                if let Some(status) = child.try_wait().unwrap() {
                    let output = child.wait_with_output().unwrap();
                    panic!(
                        "cut helper exited {status}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("publication barrier not reached: {action}/{stage}");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(fs::read_to_string(&ready).unwrap(), stage);
            child.kill().unwrap();
            assert_eq!(child.wait().unwrap().signal(), Some(9));
            let published = stage != "temporary-synced";
            let reopened = fixture.reopen();
            match action {
                "register" => {
                    let before = reopened
                        .list(&fixture.binding.session_id, &fixture.binding.workspace)
                        .unwrap();
                    assert_eq!(before.len(), usize::from(published));
                    let receipt = reopened
                        .register(
                            &fixture.binding,
                            &["cut-probe".into()],
                            "host_unrestricted",
                            false,
                        )
                        .unwrap();
                    assert_eq!(receipt.newly_registered, !published);
                    assert!(receipt.summary.terminal.is_none());
                }
                "append" | "omission" => {
                    let cursor = if action == "omission" {
                        MAX_NATIVE_COMMAND_OUTPUT_RECORDS as u64
                    } else {
                        0
                    };
                    let before = reopened.read(&fixture.binding, cursor, 64, 4).unwrap();
                    assert_eq!(
                        before.summary.observed_end,
                        cursor + if published { 4 } else { 0 }
                    );
                    let ack = reopened
                        .append(
                            &fixture.binding,
                            cursor,
                            NativeCommandOutputStream::Stdout,
                            &[0, 255, 0xf0, 0x9f],
                        )
                        .unwrap();
                    assert_eq!(ack.newly_appended, !published);
                    let page = reopened.read(&fixture.binding, cursor, 64, 4).unwrap();
                    assert_eq!(page.summary.observed_end, cursor + 4);
                    if action == "omission" {
                        assert!(page.chunks.is_empty());
                        assert_eq!(
                            page.omitted_ranges,
                            [NativeCommandOutputRange {
                                start: cursor,
                                end: cursor + 4
                            }]
                        );
                        assert_eq!((ack.retained_bytes, ack.omitted_bytes), (0, 4));
                    } else {
                        assert_eq!(page.chunks.len(), 1);
                        assert_eq!(page.chunks[0].bytes, [0, 255, 0xf0, 0x9f]);
                    }
                }
                "finish" => {
                    let before = reopened.read(&fixture.binding, 0, 1, 1).unwrap();
                    assert_eq!(before.summary.terminal.is_some(), published);
                    let summary = reopened.finish(&fixture.binding, 0, &exited()).unwrap();
                    assert_eq!(summary.revision, 1);
                    assert_eq!(summary.observed_end, 0);
                    assert_eq!(
                        reopened.read(&fixture.binding, 0, 1, 1).unwrap().summary,
                        summary
                    );
                }
                _ => unreachable!(),
            }
        }
    }
}

#[test]
#[ignore = "subprocess helper for real command-output publication cuts"]
fn output_publication_cut_child() {
    let Some(root) = std::env::var_os("OPENIDE_TEST_OUTPUT_CUT_ROOT") else {
        return;
    };
    let binding: NativeCommandOutputBinding =
        serde_json::from_str(&std::env::var("OPENIDE_TEST_OUTPUT_CUT_BINDING").unwrap()).unwrap();
    let sessions = Arc::new(SessionStore::open(PathBuf::from(root)).unwrap());
    let store = NativeCommandOutputStore::new(sessions);
    match std::env::var("OPENIDE_TEST_OUTPUT_CUT_ACTION")
        .unwrap()
        .as_str()
    {
        "register" => {
            store
                .register(&binding, &["cut-probe".into()], "host_unrestricted", false)
                .unwrap();
        }
        "append" => {
            store
                .append(
                    &binding,
                    0,
                    NativeCommandOutputStream::Stdout,
                    &[0, 255, 0xf0, 0x9f],
                )
                .unwrap();
        }
        "omission" => {
            store
                .append(
                    &binding,
                    MAX_NATIVE_COMMAND_OUTPUT_RECORDS as u64,
                    NativeCommandOutputStream::Stdout,
                    &[0, 255, 0xf0, 0x9f],
                )
                .unwrap();
        }
        "finish" => {
            store.finish(&binding, 0, &exited()).unwrap();
        }
        action => panic!("unknown action: {action}"),
    }
    panic!("cut not reached");
}
