use super::*;
use crate::{AgentEvent, AgentEventKind, SessionStore, StoreError, UserImage, UserImageMime};
use serde_json::{json, Value};
use std::fs;
use std::sync::{Arc, Barrier};

const AUTOMATION_ID: &str = "ae1e4b1e-f4bb-4d6e-8c56-7be02127c9ad";
const SCHEDULED_AT: i64 = 1_700_000_000_000;
const PROMPT: &str = "Inspeccioná el proyecto sin modificarlo 🦀\r\nConservá este pedido exacto.";
const MODEL: &str = "provider-model/exact";
const SNAPSHOT: &str = "{\"version\":1,\"instructions\":\"private-seed-秘密\\n🦀\"}";

fn origin() -> NativeAutomationOrigin {
    NativeAutomationOrigin::new(
        AUTOMATION_ID,
        format!("{AUTOMATION_ID}:{SCHEDULED_AT}"),
        SCHEDULED_AT,
        false,
        SNAPSHOT,
        PROMPT,
        MODEL,
    )
    .unwrap()
}

#[test]
fn readonly_origin_rejects_manual_command_intentions_and_capture_registration_after_reopen() {
    let (_root, _workspace, store_root, store, session) = setup();
    let metadata_before = fs::read(metadata_path(&store_root, &session)).unwrap();
    let reopened = Arc::new(SessionStore::open(&store_root).unwrap());
    let binding = crate::NativeCommandOutputBinding {
        session_id: session.id.clone(),
        workspace: session.workspace.clone(),
        manager_generation: uuid::Uuid::new_v4().to_string(),
        command_id: uuid::Uuid::new_v4().to_string(),
    };
    let outputs = crate::NativeCommandOutputStore::new(reopened.clone());
    assert!(matches!(
        outputs.register(
            &binding,
            &["must-not-run".into()],
            "host-unrestricted",
            false
        ),
        Err(crate::NativeCommandOutputError::AutomationReadOnly)
    ));
    assert!(outputs
        .list(&session.id, &session.workspace)
        .unwrap()
        .is_empty());
    let operations = crate::NativeCommandOperationStore::new(reopened.clone());
    let request = crate::NativeCommandOperationRequest {
        operation_id: uuid::Uuid::new_v4().to_string(),
        session_id: session.id.clone(),
        workspace: session.workspace.clone(),
        manager_generation: binding.manager_generation,
        command_id: binding.command_id,
        action: crate::NativeCommandOperationAction::Stdin {
            input: "must-not-send".into(),
            close_after: true,
        },
        policy_digest: "a".repeat(64),
        hooks_digest: "b".repeat(64),
    };
    assert!(matches!(
        operations.enqueue_requested(request),
        Err(crate::NativeCommandOperationError::AutomationReadOnly)
    ));
    assert!(operations.list(&session.id).unwrap().is_empty());
    assert_eq!(
        fs::read(metadata_path(&store_root, &session)).unwrap(),
        metadata_before
    );
    assert!(store.load(&session.id).unwrap().unwrap().events.is_empty());
}

fn setup() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    SessionStore,
    Session,
) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    let store_root = root.path().join("sessions");
    fs::create_dir(&workspace).unwrap();
    let store = SessionStore::open(&store_root).unwrap();
    let session = store
        .create_native_automation(&workspace, origin())
        .unwrap();
    (root, workspace, store_root, store, session)
}

fn metadata_path(store_root: &Path, session: &Session) -> std::path::PathBuf {
    store_root.join(format!("{}.session.json", session.id))
}

#[test]
fn two_independent_stores_create_one_private_origin_and_preserve_it_through_updates() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    let store_root = root.path().join("sessions");
    fs::create_dir(&workspace).unwrap();
    let first = Arc::new(SessionStore::open(&store_root).unwrap());
    let second = Arc::new(SessionStore::open(&store_root).unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let threads = [first.clone(), second.clone()].map(|store| {
        let barrier = barrier.clone();
        let workspace = workspace.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.create_native_automation(workspace, origin()).unwrap()
        })
    });
    barrier.wait();
    let [left, right] = threads.map(|thread| thread.join().unwrap());
    assert_eq!(left, right);
    assert_eq!(first.list().unwrap().len(), 1);
    origin().validate_session(&left).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(metadata_path(&store_root, &left))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    first
        .rename(&left.id, Some("Inspección registrada"))
        .unwrap();
    first.set_pinned(&left.id, true).unwrap();
    first.set_archived(&left.id, true).unwrap();
    let reopened = SessionStore::open(&store_root).unwrap();
    let saved = reopened.load(&left.id).unwrap().unwrap();
    assert_eq!(saved.session.native_automation.as_ref(), Some(&origin()));
    assert!(saved.session.pinned && saved.session.archived);
    assert_eq!(
        saved.session.title.as_deref(),
        Some("Inspección registrada")
    );
    assert_eq!(
        reopened
            .create_native_automation(&workspace, origin())
            .unwrap(),
        saved.session
    );
    let original_bytes = fs::read(metadata_path(&store_root, &left)).unwrap();
    let mut changed = origin();
    changed.authority_snapshot_json = "{\"version\":1,\"instructions\":\"changed\"}".into();
    changed.authority_snapshot_digest =
        native_automation_snapshot_digest(&changed.authority_snapshot_json).unwrap();
    assert!(matches!(
        reopened.create_native_automation(&workspace, changed),
        Err(StoreError::ConflictingNativeAutomationOrigin(_))
    ));
    let mut changed = origin();
    changed.first_request_digest =
        native_automation_request_digest("different request", MODEL, TurnMode::Build).unwrap();
    assert!(matches!(
        reopened.create_native_automation(&workspace, changed),
        Err(StoreError::ConflictingNativeAutomationOrigin(_))
    ));
    assert_eq!(
        fs::read(metadata_path(&store_root, &left)).unwrap(),
        original_bytes
    );
    // The private host authority is not an implicit transcript/search result.
    let query = crate::SessionQuery {
        search: Some("private-seed".into()),
        kind: crate::SessionListKind::All,
        ..Default::default()
    };
    assert_eq!(reopened.query_sessions(&query).unwrap().total, 0);
    let lease = reopened.try_acquire_native_lease(&left.id).unwrap();
    assert!(matches!(
        second.try_acquire_native_lease(&left.id),
        Err(StoreError::NativeSessionBusy(_))
    ));
    drop(lease);
    assert!(second.try_acquire_native_lease(&left.id).is_ok());
}

#[test]
fn first_admission_is_exact_text_build_and_cannot_be_replaced_or_seeded_through_compatibility() {
    let (_root, workspace, store_root, store, session) = setup();
    let origin = origin();
    let metadata_before = fs::read(metadata_path(&store_root, &session)).unwrap();
    for (turn_id, prompt, model, mode) in [
        ("other-turn", PROMPT, MODEL, TurnMode::Build),
        (
            origin.first_turn_id.as_str(),
            "changed prompt",
            MODEL,
            TurnMode::Build,
        ),
        (
            origin.first_turn_id.as_str(),
            PROMPT,
            "other/model",
            TurnMode::Build,
        ),
        (origin.first_turn_id.as_str(), PROMPT, MODEL, TurnMode::Plan),
    ] {
        assert!(matches!(
            store.admit_native_turn(&session.id, turn_id, prompt, model, mode),
            Err(StoreError::NativeAutomationAdmissionBlocked)
        ));
        assert!(store.load(&session.id).unwrap().unwrap().events.is_empty());
    }
    let image = UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nimage").unwrap();
    assert!(matches!(
        store.admit_native_turn_with_images(
            &session.id,
            &origin.first_turn_id,
            PROMPT,
            MODEL,
            TurnMode::Build,
            &[image]
        ),
        Err(StoreError::NativeAutomationAdmissionBlocked)
    ));
    for event in [
        AgentEventKind::TurnAdmitted {
            turn_id: origin.first_turn_id.clone(),
            prompt: PROMPT.into(),
            model: MODEL.into(),
            mode: TurnMode::Build,
        },
        AgentEventKind::TurnStarted {
            prompt: PROMPT.into(),
        },
    ] {
        assert!(matches!(
            store.append_event(&session.id, &AgentEvent::new(&session.id, 1, event)),
            Err(StoreError::NativeAutomationAdmissionBlocked)
        ));
    }
    assert_eq!(
        fs::read(metadata_path(&store_root, &session)).unwrap(),
        metadata_before
    );
    let admission = store
        .admit_native_turn(
            &session.id,
            &origin.first_turn_id,
            PROMPT,
            MODEL,
            TurnMode::Build,
        )
        .unwrap();
    assert_eq!(admission.sequence, 1);
    let journal = store_root.join(format!("{}.jsonl", session.id));
    let journal_before = fs::read(&journal).unwrap();
    let reopened = SessionStore::open(&store_root).unwrap();
    assert_eq!(
        reopened
            .admit_native_turn(
                &session.id,
                &origin.first_turn_id,
                PROMPT,
                MODEL,
                TurnMode::Build
            )
            .unwrap(),
        admission
    );
    assert!(matches!(
        reopened.admit_native_turn(&session.id, "second-turn", PROMPT, MODEL, TurnMode::Build),
        Err(StoreError::NativeAutomationAdmissionBlocked)
    ));
    assert_eq!(fs::read(&journal).unwrap(), journal_before);
    let lease = reopened.try_acquire_native_lease(&session.id).unwrap();
    assert!(matches!(
        reopened.create_native_child(&lease, "spawn", &"a".repeat(64), 3),
        Err(StoreError::NativeAutomationSubagentsDisabled)
    ));
    assert_eq!(reopened.list().unwrap().len(), 1);
    assert_eq!(
        reopened
            .create_native_automation(&workspace, origin)
            .unwrap()
            .id,
        session.id
    );
}

#[test]
fn origin_corruption_blocks_load_lease_and_admission_without_repairing_metadata_or_journal() {
    let (_root, _workspace, store_root, store, session) = setup();
    let origin = origin();
    store
        .admit_native_turn(
            &session.id,
            &origin.first_turn_id,
            PROMPT,
            MODEL,
            TurnMode::Build,
        )
        .unwrap();
    let lease = store.try_acquire_native_lease(&session.id).unwrap();
    let path = metadata_path(&store_root, &session);
    let original_bytes = fs::read(&path).unwrap();
    let journal = store_root.join(format!("{}.jsonl", session.id));
    let journal_before = fs::read(&journal).unwrap();
    let original: Value = serde_json::from_slice(&original_bytes).unwrap();
    let mut mutations = Vec::new();
    for (field, value) in [
        ("version", json!(2)),
        ("authority_snapshot_version", json!(2)),
        ("authority_snapshot_digest", json!("0".repeat(64))),
        ("authority_snapshot_json", json!("{\"changed\":true}")),
        ("first_turn_id", json!("other-turn")),
        ("first_request_digest", json!("invalid")),
        ("scheduled_at_ms", json!(SCHEDULED_AT + 1)),
        ("unknown_field", json!(true)),
    ] {
        let mut changed = original.clone();
        changed["native_automation"][field] = value;
        mutations.push(changed);
    }
    for (field, value) in [
        ("native_automation", Value::Null),
        ("backend", json!("codex")),
        ("workspace", json!(store_root.to_str().unwrap())),
        ("subagent", json!({"parent_session_id":"parent","depth":1})),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        mutations.push(changed);
    }
    for changed in mutations {
        let invalid_bytes = serde_json::to_vec(&changed).unwrap();
        fs::write(&path, &invalid_bytes).unwrap();
        let reopened = SessionStore::open(&store_root).unwrap();
        assert!(reopened.load(&session.id).is_err());
        assert!(reopened.try_acquire_native_lease(&session.id).is_err());
        assert!(reopened.native_session_for_lease(&lease).is_err());
        assert!(reopened
            .admit_native_turn(
                &session.id,
                &origin.first_turn_id,
                PROMPT,
                MODEL,
                TurnMode::Build
            )
            .is_err());
        let listing = reopened.list_with_diagnostics().unwrap();
        assert!(listing.sessions.is_empty());
        assert_eq!(listing.diagnostics.len(), 1);
        assert_eq!(fs::read(&path).unwrap(), invalid_bytes);
        assert_eq!(fs::read(&journal).unwrap(), journal_before);
    }
    fs::write(&path, original_bytes).unwrap();
    assert_eq!(store.load(&session.id).unwrap().unwrap().events.len(), 1);
}

#[test]
fn original_admission_is_revalidated_after_reopen_even_if_journal_json_is_well_formed() {
    let (_root, _workspace, store_root, store, session) = setup();
    let origin = origin();
    let original = store
        .admit_native_turn(
            &session.id,
            &origin.first_turn_id,
            PROMPT,
            MODEL,
            TurnMode::Build,
        )
        .unwrap();
    let journal = store_root.join(format!("{}.jsonl", session.id));
    let mut changed = original.clone();
    if let AgentEventKind::TurnAdmitted { prompt, .. } = &mut changed.kind {
        *prompt = "Journal changed while origin remained intact".into();
    }
    let bytes = format!("{}\n", serde_json::to_string(&changed).unwrap());
    fs::write(&journal, bytes.as_bytes()).unwrap();
    let reopened = SessionStore::open(&store_root).unwrap();
    assert!(matches!(
        reopened.load(&session.id),
        Err(StoreError::NativeAutomationAdmissionBlocked)
    ));
    assert!(matches!(
        reopened.admit_native_turn(
            &session.id,
            &origin.first_turn_id,
            PROMPT,
            MODEL,
            TurnMode::Build
        ),
        Err(StoreError::NativeAutomationAdmissionBlocked)
    ));
    assert_eq!(fs::read(&journal).unwrap(), bytes.as_bytes());
    let duplicate = AgentEvent::new(&session.id, 2, original.kind.clone());
    let bytes = format!(
        "{}\n{}\n",
        serde_json::to_string(&original).unwrap(),
        serde_json::to_string(&duplicate).unwrap()
    );
    fs::write(&journal, bytes.as_bytes()).unwrap();
    assert!(matches!(
        reopened.load(&session.id),
        Err(StoreError::NativeAutomationAdmissionBlocked)
    ));
    assert_eq!(fs::read(&journal).unwrap(), bytes.as_bytes());
}

#[test]
fn steering_text_images_ack_and_compatibility_cannot_enlarge_original_request() {
    let (_root, _workspace, store_root, store, session) = setup();
    let origin = origin();
    let admission = store
        .admit_native_turn(
            &session.id,
            &origin.first_turn_id,
            PROMPT,
            MODEL,
            TurnMode::Build,
        )
        .unwrap();
    let lease = store.try_acquire_native_lease(&session.id).unwrap();
    let journal = store_root.join(format!("{}.jsonl", session.id));
    let journal_before = fs::read(&journal).unwrap();
    let metadata = metadata_path(&store_root, &session);
    let metadata_before = fs::read(&metadata).unwrap();
    let inbox = store_root.join(format!("{}.native-steer.json", session.id));
    assert_eq!(
        store.active_native_turn_id(&session.id).unwrap().as_deref(),
        Some(origin.first_turn_id.as_str())
    );
    assert!(store
        .pending_native_steers(&session.id, &origin.first_turn_id)
        .unwrap()
        .is_empty());
    assert!(matches!(
        store.enqueue_native_steer(
            &session.id,
            &origin.first_turn_id,
            "new-input",
            "An unreserved continuation"
        ),
        Err(StoreError::NativeAutomationSteeringDisabled)
    ));
    let image = UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nimage").unwrap();
    assert!(matches!(
        store.enqueue_native_steer_with_images(
            &session.id,
            &origin.first_turn_id,
            "new-image",
            "",
            std::slice::from_ref(&image)
        ),
        Err(StoreError::NativeAutomationSteeringDisabled)
    ));
    assert!(matches!(
        store.ack_native_steer(&lease, &origin.first_turn_id, "new-input", 2),
        Err(StoreError::NativeAutomationSteeringDisabled)
    ));
    let steered = AgentEvent::new(
        &session.id,
        2,
        AgentEventKind::TurnSteered {
            turn_id: origin.first_turn_id.clone(),
            input_id: "new-input".into(),
            text: "An unreserved continuation".into(),
        },
    );
    let image_steered = AgentEvent::new(
        &session.id,
        2,
        AgentEventKind::TurnSteeredWithImages {
            turn_id: origin.first_turn_id.clone(),
            input_id: "new-image".into(),
            text: String::new(),
            images: vec![image],
        },
    );
    for event in [&steered, &image_steered] {
        assert!(store.append_event(&session.id, event).is_err());
        assert!(store.append_native_event(event).is_err());
    }
    assert!(!inbox.exists());
    assert_eq!(fs::read(&journal).unwrap(), journal_before);
    assert_eq!(fs::read(&metadata).unwrap(), metadata_before);

    // An otherwise well-formed foreign inbox is not ignored on exact retry,
    // read-only inspection, or an already leased worker's next event.
    let foreign_inbox = serde_json::to_vec(&json!({
        "version": 1,
        "session_id": session.id,
        "entries": [{
            "turn_id": origin.first_turn_id,
            "input_id": "foreign-input",
            "text": "Unreserved",
            "images": []
        }]
    }))
    .unwrap();
    fs::write(&inbox, &foreign_inbox).unwrap();
    let reopened = SessionStore::open(&store_root).unwrap();
    assert!(matches!(
        reopened.load(&session.id),
        Err(StoreError::NativeAutomationSteeringDisabled)
    ));
    assert!(matches!(
        reopened.pending_native_steers(&session.id, &origin.first_turn_id),
        Err(StoreError::NativeAutomationSteeringDisabled)
    ));
    assert!(matches!(
        reopened.admit_native_turn(
            &session.id,
            &origin.first_turn_id,
            PROMPT,
            MODEL,
            TurnMode::Build
        ),
        Err(StoreError::NativeAutomationSteeringDisabled)
    ));
    assert!(matches!(
        reopened.append_native_event(&AgentEvent::new(
            &session.id,
            2,
            AgentEventKind::TurnStarted {
                prompt: PROMPT.into()
            }
        )),
        Err(StoreError::NativeAutomationSteeringDisabled)
    ));
    assert_eq!(fs::read(&inbox).unwrap(), foreign_inbox);
    assert_eq!(fs::read(&journal).unwrap(), journal_before);
    assert_eq!(fs::read(&metadata).unwrap(), metadata_before);
    fs::remove_file(&inbox).unwrap();
    let foreign_journal = format!(
        "{}\n{}\n",
        serde_json::to_string(&admission).unwrap(),
        serde_json::to_string(&steered).unwrap()
    );
    fs::write(&journal, foreign_journal.as_bytes()).unwrap();
    assert!(matches!(
        reopened.load(&session.id),
        Err(StoreError::NativeAutomationSteeringDisabled)
    ));
    assert_eq!(fs::read(&journal).unwrap(), foreign_journal.as_bytes());
}

#[test]
fn maximum_snapshot_reopens_and_metadata_limit_rejects_oversize_without_publication() {
    let (_root, workspace, store_root, store, session) = setup();
    let mut maximum = origin();
    let prefix = "{\"payload\":\"";
    let suffix = "\"}";
    maximum.authority_snapshot_json = format!(
        "{prefix}{}{suffix}",
        "x".repeat(MAX_NATIVE_AUTOMATION_AUTHORITY_SNAPSHOT_BYTES - prefix.len() - suffix.len())
    );
    maximum.authority_snapshot_digest =
        native_automation_snapshot_digest(&maximum.authority_snapshot_json).unwrap();
    maximum.run_id = format!("{AUTOMATION_ID}:{}", SCHEDULED_AT + 1);
    maximum.scheduled_at_ms += 1;
    maximum.first_turn_id = native_automation_first_turn_id(&maximum.run_id).unwrap();
    let maximum_session = store
        .create_native_automation(&workspace, maximum.clone())
        .unwrap();
    let reopened = SessionStore::open(&store_root).unwrap();
    assert_eq!(
        reopened
            .load(&maximum_session.id)
            .unwrap()
            .unwrap()
            .session
            .native_automation,
        Some(maximum)
    );
    let path = metadata_path(&store_root, &session);
    let original_bytes = fs::read(&path).unwrap();
    assert!(matches!(
        store.rename(
            &session.id,
            Some(&"x".repeat(crate::MAX_SESSION_METADATA_BYTES as usize))
        ),
        Err(StoreError::SessionMetadataTooLarge)
    ));
    assert_eq!(fs::read(&path).unwrap(), original_bytes);
    let oversized = vec![b' '; crate::MAX_SESSION_METADATA_BYTES as usize + 1];
    fs::write(&path, &oversized).unwrap();
    assert!(matches!(
        reopened.load(&session.id),
        Err(StoreError::SessionMetadataTooLarge)
    ));
    assert_eq!(fs::read(&path).unwrap(), oversized);
}

#[test]
fn deleting_an_occurrence_never_recreates_it_on_exact_retry() {
    let (_root, workspace, store_root, store, session) = setup();
    assert!(store.delete(&session.id).unwrap());
    assert!(store.load(&session.id).unwrap().is_none());
    let reopened = SessionStore::open(&store_root).unwrap();
    assert!(matches!(
        reopened.create_native_automation(&workspace, origin()),
        Err(StoreError::NativeAutomationOccurrenceDeleted(_))
    ));
    assert!(reopened.list().unwrap().is_empty());
}

#[test]
fn real_process_cuts_publish_either_no_origin_or_one_complete_origin_and_retry_same_identity() {
    for stage in ["temporary-synced", "renamed", "directory-synced"] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let store_root = root.path().join("sessions");
        fs::create_dir(&workspace).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "native_automation_origin::tests::metadata_publication_cut_child",
                "--nocapture",
            ])
            .env("OPENIDE_AUTOMATION_METADATA_PUBLICATION_STAGE", stage)
            .env("OPENIDE_AUTOMATION_CUT_WORKSPACE", &workspace)
            .env("OPENIDE_AUTOMATION_CUT_STORE", &store_root)
            .output()
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(87),
            "{stage}: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        let reopened = SessionStore::open(&store_root).unwrap();
        let session_id =
            native_automation_session_id(&fs::canonicalize(&workspace).unwrap(), &origin().run_id)
                .unwrap();
        let before_retry = reopened.load(&session_id).unwrap();
        assert_eq!(before_retry.is_some(), stage != "temporary-synced");
        if let Some(saved) = before_retry {
            assert_eq!(saved.session.native_automation, Some(origin()));
            assert!(saved.events.is_empty());
        }
        let saved = reopened
            .create_native_automation(&workspace, origin())
            .unwrap();
        assert_eq!(saved.id, session_id);
        assert_eq!(saved.native_automation, Some(origin()));
        assert_eq!(reopened.list().unwrap().len(), 1);
        assert!(reopened.load(&saved.id).unwrap().unwrap().events.is_empty());
    }
}

#[test]
#[ignore = "subprocess-only crash helper; invoked by real_process_cuts"]
fn metadata_publication_cut_child() {
    let Some(workspace) = std::env::var_os("OPENIDE_AUTOMATION_CUT_WORKSPACE") else {
        return;
    };
    let store_root = std::env::var_os("OPENIDE_AUTOMATION_CUT_STORE").expect("child store");
    let store = SessionStore::open(std::path::PathBuf::from(store_root)).unwrap();
    store
        .create_native_automation(std::path::PathBuf::from(workspace), origin())
        .unwrap();
    panic!("publication seam did not exit the process");
}
