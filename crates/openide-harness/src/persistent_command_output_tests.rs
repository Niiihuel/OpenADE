use super::*;
use openide_core::SessionStore;

#[test]
fn long_unicode_diagnostics_do_not_erase_the_real_durable_terminal_outcome() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let owner = sessions.create(&workspace, "native").unwrap().id;
    let store = Arc::new(NativeCommandOutputStore::new(sessions));
    let manager = PersistentCommandManager::new(
        workspace.clone(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap()
    .with_native_session_owner(owner.clone())
    .unwrap()
    .with_native_output_store(store.clone())
    .unwrap();
    let id = Uuid::new_v4().to_string();
    let capture = manager
        .register_output(&id, "unspawned-diagnostic-fixture", &[], false)
        .unwrap();
    let output = OutputBuffer::new(4096).with_capture(capture);
    output
        .push(PersistentCommandStream::Stdout, b"prefix")
        .unwrap();
    let wait_error = "🦀".repeat(2048);
    let reader_error = "é".repeat(4096);
    output.finish(
        PersistentCommandExit::WaitFailed(wait_error.clone()),
        Some(reader_error),
    );
    let summary = store.list(&owner, &workspace).unwrap().remove(0);
    assert_eq!(summary.observed_end, 6);
    assert_eq!(summary.retained_bytes, 6);
    let terminal = summary.terminal.unwrap();
    assert!(terminal.exit_observed);
    let Some(NativeCommandOutputExit::WaitFailed { error }) = terminal.exit_kind else {
        panic!("terminal lost the actual wait failure")
    };
    assert!(error.len() <= MAX_NATIVE_OUTPUT_ERROR_BYTES);
    assert!(error.ends_with("[diagnostic truncated to 4096 bytes]"));
    let reader = terminal.reader_error.unwrap();
    assert!(reader.len() <= MAX_NATIVE_OUTPUT_ERROR_BYTES);
    assert!(reader.ends_with("[diagnostic truncated to 4096 bytes]"));
    assert_eq!(
        output.state.lock().unwrap().exit,
        Some(PersistentCommandExit::WaitFailed(wait_error))
    );
    assert!(!output
        .capture_failed
        .load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn registered_command_id_cannot_grant_a_second_process_launch() {
    let root = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let owner = sessions.create(root.path(), "native").unwrap().id;
    let store = Arc::new(NativeCommandOutputStore::new(sessions));
    let manager = PersistentCommandManager::new(
        root.path().to_path_buf(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap()
    .with_native_session_owner(owner.clone())
    .unwrap()
    .with_native_output_store(store.clone())
    .unwrap();
    let id = Uuid::new_v4().to_string();
    assert!(manager
        .register_output(&id, "never-execute-fixture", &[], false)
        .unwrap()
        .is_some());
    assert!(manager
        .register_output(&id, "never-execute-fixture", &[], false)
        .err()
        .unwrap()
        .to_string()
        .contains("must not be replayed"));
    assert_eq!(store.list(&owner, root.path()).unwrap().len(), 1);
    assert!(manager.list_sessions().unwrap().is_empty());
}
