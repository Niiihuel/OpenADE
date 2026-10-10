use super::*;

fn request(session: &Session) -> NativeCommandOperationRequest {
    NativeCommandOperationRequest {
        operation_id: Uuid::new_v4().to_string(),
        session_id: session.id.clone(),
        workspace: fs::canonicalize(&session.workspace).unwrap(),
        manager_generation: Uuid::new_v4().to_string(),
        command_id: Uuid::new_v4().to_string(),
        action: NativeCommandOperationAction::Stdin {
            input: "hola 🦀\r\nexacto\0".into(),
            close_after: true,
        },
        policy_digest: "a".repeat(64),
        hooks_digest: "b".repeat(64),
    }
}

fn fixture() -> (
    tempfile::TempDir,
    Arc<SessionStore>,
    Session,
    NativeSessionLease,
    NativeCommandOperationStore,
    NativeCommandOperationRequest,
) {
    let root = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
    let session = sessions.create(root.path(), "native").unwrap();
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let store = NativeCommandOperationStore::new(sessions.clone());
    let req = request(&session);
    (root, sessions, session, lease, store, req)
}

fn started(
    store: &NativeCommandOperationStore,
    lease: &NativeSessionLease,
    req: NativeCommandOperationRequest,
) -> NativeCommandOperationSnapshot {
    store.enqueue_requested(req.clone()).unwrap();
    store
        .preflight_capacity(lease, &req.operation_id, 1024 * 1024)
        .unwrap();
    let receipt = store.begin_started(lease, &req.operation_id, 0).unwrap();
    assert!(receipt.newly_started);
    receipt.snapshot
}

#[test]
fn exact_intention_reopens_utf8_and_owner_without_model_events_or_automatic_effects() {
    let (root, sessions, session, lease, store, req) = fixture();
    let queued = store.enqueue_requested(req.clone()).unwrap();
    assert_eq!(queued.request, req);
    assert_eq!(queued.revision, 0);
    let path = ledger_path(root.path(), &session.id);
    let bytes = fs::read(&path).unwrap();
    assert_eq!(store.enqueue_requested(req.clone()).unwrap(), queued);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let mut conflicting = req.clone();
    conflicting.action = NativeCommandOperationAction::Stdin {
        input: "changed".into(),
        close_after: true,
    };
    assert!(matches!(
        store.enqueue_requested(conflicting),
        Err(NativeCommandOperationError::Conflict)
    ));
    let reopened =
        NativeCommandOperationStore::new(Arc::new(SessionStore::open(root.path()).unwrap()));
    assert_eq!(
        reopened.snapshot(&session.id, &req.operation_id).unwrap(),
        Some(queued.clone())
    );
    assert_eq!(reopened.list_pending(&session.id).unwrap(), vec![queued]);
    assert!(reopened.require_no_uncertain(&lease).is_ok());
    assert!(matches!(
        reopened.require_no_pending(&lease),
        Err(NativeCommandOperationError::Pending(_))
    ));
    assert!(matches!(
        store.begin_started(&lease, &req.operation_id, 0),
        Err(NativeCommandOperationError::Conflict)
    ));
    assert!(sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn started_retry_never_grants_a_second_effect_and_reopen_requires_acknowledgment() {
    let (root, _, session, lease, store, req) = fixture();
    let snapshot = started(&store, &lease, req.clone());
    let reopened =
        NativeCommandOperationStore::new(Arc::new(SessionStore::open(root.path()).unwrap()));
    let repeated = reopened
        .begin_started(&lease, &req.operation_id, 0)
        .unwrap();
    assert!(!repeated.newly_started);
    assert_eq!(repeated.snapshot, snapshot);
    assert!(reopened.require_no_uncertain(&lease).is_err());
    assert!(matches!(
        reopened.reject_requested(&lease, &req.operation_id, 1, "manager gone"),
        Err(NativeCommandOperationError::Conflict)
    ));
    let acknowledged = reopened
        .resolve_started(
            &lease,
            &req.operation_id,
            1,
            "Human inspected process and pre-hook effects; no process restoration",
        )
        .unwrap();
    assert!(matches!(
        acknowledged.state,
        NativeCommandOperationState::Resolved { .. }
    ));
    assert!(acknowledged.effect_result.is_none());
    assert_eq!(
        reopened
            .resolve_started(
                &lease,
                &req.operation_id,
                1,
                "Human inspected process and pre-hook effects; no process restoration"
            )
            .unwrap(),
        acknowledged
    );
    assert_eq!(reopened.list_pending(&session.id).unwrap(), vec![]);
    assert!(reopened.require_no_pending(&lease).is_ok());
    assert!(
        !reopened
            .begin_started(&lease, &req.operation_id, 0)
            .unwrap()
            .newly_started
    );
}

#[test]
fn known_effect_survives_post_hook_failure_and_uncertain_result_is_never_reexecuted() {
    let (root, sessions, session, lease, store, req) = fixture();
    started(&store, &lease, req.clone());
    assert!(matches!(
        store.finish(
            &lease,
            &req.operation_id,
            1,
            NativeCommandOperationOutcome::Succeeded {
                diagnostic: "Must not claim command success before a known effect receipt".into()
            }
        ),
        Err(NativeCommandOperationError::Conflict)
    ));
    assert!(matches!(
        store.record_phase(
            &lease,
            &req.operation_id,
            1,
            NativeCommandOperationPhase::BeforePostHooks,
            "skip effect receipt"
        ),
        Err(NativeCommandOperationError::Conflict)
    ));
    store
        .record_phase(
            &lease,
            &req.operation_id,
            1,
            NativeCommandOperationPhase::BeforeCommand,
            "pre-hook approved; before exact stdin",
        )
        .unwrap();
    let effect = NativeCommandOperationOutcome::Succeeded {
        diagnostic: "Exact UTF8 input written; EOF closed".into(),
    };
    store
        .record_effect_result(&lease, &req.operation_id, 2, effect.clone())
        .unwrap();
    let diagnostic = serde_json::json!({"hook_id":"post-check", "phase":"post", "decision":"deny", "reason":"known failure", "stdout":"🦀".repeat(32 * 1024), "stderr":"full captured output", "error":null}).to_string();
    let phase = store
        .record_phase(
            &lease,
            &req.operation_id,
            3,
            NativeCommandOperationPhase::BeforePostHooks,
            &diagnostic,
        )
        .unwrap();
    assert!(
        matches!(&phase.state, NativeCommandOperationState::Started { diagnostic: saved, .. } if saved == &diagnostic)
    );
    let finished = store
        .finish(
            &lease,
            &req.operation_id,
            4,
            NativeCommandOperationOutcome::Failed {
                diagnostic:
                    "Post hook denied after successful stdin; inspect separate effect receipt"
                        .into(),
            },
        )
        .unwrap();
    assert_eq!(finished.effect_result, Some(effect));
    let reopened =
        NativeCommandOperationStore::new(Arc::new(SessionStore::open(root.path()).unwrap()));
    assert_eq!(
        reopened.snapshot(&session.id, &req.operation_id).unwrap(),
        Some(finished)
    );
    let mut next = request(&session);
    next.action = NativeCommandOperationAction::Terminate;
    started(&store, &lease, next.clone());
    let uncertain = store
        .finish(
            &lease,
            &next.operation_id,
            1,
            NativeCommandOperationOutcome::EffectUncertain {
                diagnostic: "Termination result was not observed".into(),
            },
        )
        .unwrap();
    assert!(uncertain.state.requires_verification());
    assert!(store.require_no_uncertain(&lease).is_err());
    store
        .resolve_started(
            &lease,
            &next.operation_id,
            2,
            "Verified externally; acknowledgment only",
        )
        .unwrap();
    assert!(store.require_no_pending(&lease).is_ok());
    assert!(sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
}

#[test]
fn requested_rejection_cancellation_and_capacity_reservations_never_start_effects() {
    let (root, _, session, lease, store, req) = fixture();
    store.enqueue_requested(req.clone()).unwrap();
    let path = ledger_path(root.path(), &session.id);
    let before = fs::read(&path).unwrap();
    assert!(store
        .preflight_capacity(&lease, &req.operation_id, MAX_FILE_BYTES)
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(store.begin_started(&lease, &req.operation_id, 0).is_err());
    let rejected = store
        .reject_requested(
            &lease,
            &req.operation_id,
            0,
            "Fresh policy denies stdin; no hooks or command attempted",
        )
        .unwrap();
    assert!(matches!(
        rejected.state,
        NativeCommandOperationState::Rejected { .. }
    ));
    assert!(rejected.effect_result.is_none());
    let mut req = request(&session);
    req.action = NativeCommandOperationAction::Resize {
        columns: 80,
        rows: 24,
    };
    store.enqueue_requested(req.clone()).unwrap();
    store
        .preflight_capacity(&lease, &req.operation_id, MAX_FILE_BYTES - 400 * 1024)
        .unwrap();
    // A separate queue writer cannot steal the pending actor's reserved bytes.
    let new_req = request(&session);
    assert!(store.enqueue_requested(new_req).is_err());
    let cancelled = store
        .cancel_requested(
            &lease,
            &req.operation_id,
            0,
            "Permission revoked before effect",
        )
        .unwrap();
    assert!(matches!(
        cancelled.state,
        NativeCommandOperationState::Cancelled { .. }
    ));
    assert!(store.list_pending(&session.id).unwrap().is_empty());
    store.enqueue_requested(request(&session)).unwrap();
}

#[test]
fn exact_revision_race_and_foreign_lease_or_workspace_are_rejected() {
    let (root, _, session, lease, store, req) = fixture();
    store.enqueue_requested(req.clone()).unwrap();
    store
        .preflight_capacity(&lease, &req.operation_id, 1024 * 1024)
        .unwrap();
    let other =
        NativeCommandOperationStore::new(Arc::new(SessionStore::open(root.path()).unwrap()));
    let barrier = std::sync::Barrier::new(2);
    let receipts = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            store.begin_started(&lease, &req.operation_id, 0)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            other.begin_started(&lease, &req.operation_id, 0)
        });
        [a.join().unwrap().unwrap(), b.join().unwrap().unwrap()]
    });
    assert_eq!(receipts.iter().filter(|v| v.newly_started).count(), 1);
    let foreign_root = tempfile::tempdir().unwrap();
    let foreign = NativeCommandOperationStore::new(Arc::new(
        SessionStore::open(foreign_root.path()).unwrap(),
    ));
    assert!(matches!(
        foreign.finish(
            &lease,
            &req.operation_id,
            1,
            NativeCommandOperationOutcome::Failed {
                diagnostic: "Not executed".into()
            }
        ),
        Err(NativeCommandOperationError::Store(
            StoreError::NativeLeaseStoreMismatch
        ))
    ));
    let mut wrong = request(&session);
    wrong.workspace = foreign_root.path().into();
    assert!(matches!(
        other.enqueue_requested(wrong),
        Err(NativeCommandOperationError::Conflict)
    ));
}

#[test]
fn queued_cancellation_works_while_native_worker_owns_lease_and_unknown_input_changes_nothing() {
    let (root, sessions, session, lease, store, req) = fixture();
    store.enqueue_requested(req.clone()).unwrap();
    let second_sessions = Arc::new(SessionStore::open(root.path()).unwrap());
    assert!(matches!(
        second_sessions.try_acquire_native_lease(&session.id),
        Err(StoreError::NativeSessionBusy(_))
    ));
    let ui_store = NativeCommandOperationStore::new(second_sessions);
    let path = ledger_path(root.path(), &session.id);
    let before = fs::read(&path).unwrap();
    assert!(matches!(
        ui_store.cancel_queued_requested(
            &session.id,
            &Uuid::new_v4().to_string(),
            0,
            "No such queued operation"
        ),
        Err(NativeCommandOperationError::Conflict)
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    let cancelled = ui_store
        .cancel_queued_requested(
            &session.id,
            &req.operation_id,
            0,
            "Human withdrew queued input while model worker remains active",
        )
        .unwrap();
    assert!(matches!(
        cancelled.state,
        NativeCommandOperationState::Cancelled { .. }
    ));
    assert!(cancelled.effect_result.is_none());
    let bytes = fs::read(&path).unwrap();
    assert_eq!(
        ui_store
            .cancel_queued_requested(
                &session.id,
                &req.operation_id,
                0,
                "Human withdrew queued input while model worker remains active"
            )
            .unwrap(),
        cancelled
    );
    assert!(matches!(
        ui_store.cancel_queued_requested(
            &session.id,
            &req.operation_id,
            0,
            "Changed reason cannot replace receipt"
        ),
        Err(NativeCommandOperationError::Conflict)
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(store.list_pending(&session.id).unwrap().is_empty());
    store.require_no_pending(&lease).unwrap();
    assert!(sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
}

#[test]
fn queued_cancel_races_started_cas_with_one_winner_and_never_cancels_an_external_effect() {
    let (root, _, session, lease, store, _) = fixture();
    let ui_store =
        NativeCommandOperationStore::new(Arc::new(SessionStore::open(root.path()).unwrap()));
    for _ in 0..24 {
        let req = request(&session);
        store.enqueue_requested(req.clone()).unwrap();
        store
            .preflight_capacity(&lease, &req.operation_id, 512 * 1024)
            .unwrap();
        let barrier = std::sync::Barrier::new(2);
        let (started, cancelled) = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                store.begin_started(&lease, &req.operation_id, 0)
            });
            let b = scope.spawn(|| {
                barrier.wait();
                ui_store.cancel_queued_requested(
                    &session.id,
                    &req.operation_id,
                    0,
                    "Withdraw exact queued input",
                )
            });
            (a.join().unwrap(), b.join().unwrap())
        });
        match (started, cancelled) {
            (Ok(receipt), Err(NativeCommandOperationError::Conflict)) => {
                assert!(receipt.newly_started);
                assert!(matches!(
                    receipt.snapshot.state,
                    NativeCommandOperationState::Started { .. }
                ));
                let bytes = fs::read(ledger_path(root.path(), &session.id)).unwrap();
                assert!(matches!(
                    ui_store.cancel_queued_requested(
                        &session.id,
                        &req.operation_id,
                        1,
                        "Cannot withdraw Started effects"
                    ),
                    Err(NativeCommandOperationError::Conflict)
                ));
                assert_eq!(
                    fs::read(ledger_path(root.path(), &session.id)).unwrap(),
                    bytes
                );
                assert!(store.require_no_uncertain(&lease).is_err());
                store
                    .resolve_started(
                        &lease,
                        &req.operation_id,
                        1,
                        "Verified this core fixture did not perform a process effect",
                    )
                    .unwrap();
            }
            (Err(NativeCommandOperationError::Conflict), Ok(snapshot)) => {
                assert!(matches!(
                    snapshot.state,
                    NativeCommandOperationState::Cancelled { .. }
                ));
                assert!(snapshot.effect_result.is_none());
                assert!(matches!(
                    store.begin_started(&lease, &req.operation_id, 0),
                    Err(NativeCommandOperationError::Conflict)
                ));
            }
            (started, cancelled) => panic!("CAS must have one winner: {started:?} / {cancelled:?}"),
        }
        store.require_no_pending(&lease).unwrap();
    }
}

#[test]
fn admission_cannot_ignore_pending_intentions_or_started_effects_and_corruption_fails_closed() {
    let (root, sessions, session, lease, store, req) = fixture();
    store.enqueue_requested(req.clone()).unwrap();
    assert!(matches!(
        sessions.admit_native_turn(
            &session.id,
            "turn",
            "Prompt",
            "model",
            crate::TurnMode::Build
        ),
        Err(StoreError::NativeCommandOperationAdmissionBlocked(_))
    ));
    store
        .cancel_requested(
            &lease,
            &req.operation_id,
            0,
            "No process restored after manager disappeared",
        )
        .unwrap();
    let admitted = sessions
        .admit_native_turn(
            &session.id,
            "turn",
            "Prompt",
            "model",
            crate::TurnMode::Build,
        )
        .unwrap();
    let req = request(&session);
    started(&store, &lease, req.clone());
    assert_eq!(
        sessions
            .admit_native_turn(
                &session.id,
                "turn",
                "Prompt",
                "model",
                crate::TurnMode::Build
            )
            .unwrap(),
        admitted
    );
    assert!(sessions
        .admit_native_turn(
            &session.id,
            "new-turn",
            "Prompt",
            "model",
            crate::TurnMode::Build
        )
        .is_err());
    let path = ledger_path(root.path(), &session.id);
    let good = fs::read(&path).unwrap();
    let mut ledger: serde_json::Value = serde_json::from_slice(&good).unwrap();
    ledger["operations"][1]["state"] =
        serde_json::json!({"type":"resolved","acknowledgment":"forged"});
    let corrupt = serde_json::to_vec(&ledger).unwrap();
    fs::write(&path, &corrupt).unwrap();
    assert!(store.list(&session.id).is_err());
    assert!(store.require_no_uncertain(&lease).is_err());
    assert!(store
        .resolve_started(
            &lease,
            &req.operation_id,
            1,
            "Must not overwrite corruption"
        )
        .is_err());
    assert!(sessions
        .admit_native_turn(
            &session.id,
            "new-turn",
            "Prompt",
            "model",
            crate::TurnMode::Build
        )
        .is_err());
    assert_eq!(fs::read(path).unwrap(), corrupt);
}

#[test]
fn model_and_terminal_publication_fences_hold_pending_intentions_until_actor_consumes_them() {
    let (_root, sessions, session, lease, store, req) = fixture();
    let admitted = sessions
        .admit_native_turn(
            &session.id,
            "turn",
            "Prompt",
            "model",
            crate::TurnMode::Build,
        )
        .unwrap();
    sessions
        .append_native_event(&crate::AgentEvent::new(
            &session.id,
            admitted.sequence + 1,
            crate::AgentEventKind::TurnStarted {
                prompt: "Prompt".into(),
            },
        ))
        .unwrap();
    let request_event = crate::AgentEvent::new(
        &session.id,
        admitted.sequence + 2,
        crate::AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "model".into(),
        },
    );
    store.enqueue_requested(req.clone()).unwrap();
    assert!(matches!(
        sessions.append_native_event(&request_event),
        Err(StoreError::NativeCommandOperationAdmissionBlocked(_))
    ));
    assert_eq!(sessions.load(&session.id).unwrap().unwrap().events.len(), 2);
    store
        .reject_requested(
            &lease,
            &req.operation_id,
            0,
            "Old process manager was not restored",
        )
        .unwrap();
    sessions.append_native_event(&request_event).unwrap();
    let req = request(&session);
    store.enqueue_requested(req.clone()).unwrap();
    let terminal = crate::AgentEvent::new(
        &session.id,
        admitted.sequence + 3,
        crate::AgentEventKind::TurnCompleted {
            response: "Done".into(),
            steps: 1,
        },
    );
    assert!(matches!(
        sessions.append_native_event(&terminal),
        Err(StoreError::NativeCommandOperationAdmissionBlocked(_))
    ));
    store
        .cancel_requested(
            &lease,
            &req.operation_id,
            0,
            "Cancelled explicitly before hooks/process effect",
        )
        .unwrap();
    sessions.append_native_event(&terminal).unwrap();
    // Byte-equivalent publication retries still return the prior durable
    // result even if the next human operation was queued after completion.
    store.enqueue_requested(request(&session)).unwrap();
    sessions.append_native_event(&terminal).unwrap();
    assert_eq!(sessions.load(&session.id).unwrap().unwrap().events.len(), 4);
}

#[test]
fn queue_and_terminal_race_never_publishes_over_a_prior_pending_operation() {
    for _ in 0..12 {
        let (root, sessions, session, lease, store, req) = fixture();
        let admitted = sessions
            .admit_native_turn(
                &session.id,
                "turn",
                "Prompt",
                "model",
                crate::TurnMode::Build,
            )
            .unwrap();
        sessions
            .append_native_event(&crate::AgentEvent::new(
                &session.id,
                admitted.sequence + 1,
                crate::AgentEventKind::TurnStarted {
                    prompt: "Prompt".into(),
                },
            ))
            .unwrap();
        let terminal = crate::AgentEvent::new(
            &session.id,
            admitted.sequence + 2,
            crate::AgentEventKind::TurnCompleted {
                response: "Done".into(),
                steps: 1,
            },
        );
        let other = Arc::new(SessionStore::open(root.path()).unwrap());
        let barrier = std::sync::Barrier::new(2);
        let (queued, finished) = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                store.enqueue_requested(req.clone())
            });
            let b = scope.spawn(|| {
                barrier.wait();
                other.append_native_event(&terminal)
            });
            (a.join().unwrap(), b.join().unwrap())
        });
        assert!(queued.is_ok());
        assert_eq!(store.list_pending(&session.id).unwrap().len(), 1);
        match finished {
            Ok(()) => assert_eq!(
                sessions.load(&session.id).unwrap().unwrap().events.last(),
                Some(&terminal)
            ),
            Err(StoreError::NativeCommandOperationAdmissionBlocked(_)) => {
                assert_eq!(sessions.load(&session.id).unwrap().unwrap().events.len(), 2);
                store
                    .cancel_requested(
                        &lease,
                        &req.operation_id,
                        0,
                        "Actor consumed after fenced terminal retry",
                    )
                    .unwrap();
                other.append_native_event(&terminal).unwrap();
            }
            Err(error) => panic!("unexpected race error: {error}"),
        }
    }
}

#[test]
fn archive_and_deletion_preserve_pending_recovery_then_remove_only_settled_owned_files() {
    let (root, sessions, session, lease, store, req) = fixture();
    store.enqueue_requested(req.clone()).unwrap();
    assert!(matches!(
        sessions.set_archived(&session.id, true),
        Err(StoreError::NativeCommandOperationAdmissionBlocked(_))
    ));
    store
        .cancel_requested(
            &lease,
            &req.operation_id,
            0,
            "Permission revoked before command started",
        )
        .unwrap();
    sessions.set_archived(&session.id, true).unwrap();
    assert!(store.enqueue_requested(request(&session)).is_err());
    sessions.set_archived(&session.id, false).unwrap();
    let next = request(&session);
    started(&store, &lease, next.clone());
    drop(lease);
    assert!(matches!(
        sessions.delete(&session.id),
        Err(StoreError::NativeCommandOperationAdmissionBlocked(_))
    ));
    assert!(matches!(
        sessions.delete_native_tree(&session.id),
        Err(StoreError::NativeCommandOperationAdmissionBlocked(_))
    ));
    assert!(ledger_path(root.path(), &session.id).exists());
    assert!(sessions.load(&session.id).unwrap().is_some());
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    store
        .resolve_started(
            &lease,
            &next.operation_id,
            1,
            "Inspected old process and hook effects; acknowledged",
        )
        .unwrap();
    let own_temporary = root.path().join(format!(
        ".{}.native-command-operations.{}.tmp",
        session.id,
        Uuid::new_v4()
    ));
    let unrelated = root.path().join(format!(
        ".{}.native-command-operations.notes.tmp",
        session.id
    ));
    fs::write(&own_temporary, b"unpublished").unwrap();
    fs::write(&unrelated, b"keep unrelated").unwrap();
    drop(lease);
    assert!(sessions.delete_native_tree(&session.id).unwrap());
    assert!(!ledger_path(root.path(), &session.id).exists());
    assert!(!own_temporary.exists());
    assert!(unrelated.exists());
}

#[test]
fn invalid_uuid_dimensions_digest_utf8_byte_bounds_and_unknown_fields_are_rejected() {
    let (root, _, session, _, store, req) = fixture();
    for field in [
        "operation",
        "manager",
        "command",
        "digest",
        "resize",
        "input",
    ] {
        let mut bad = req.clone();
        match field {
            "operation" => bad.operation_id = "../../foreign".into(),
            "manager" => bad.manager_generation = Uuid::nil().to_string(),
            "command" => bad.command_id = "AAAAAAAA-AAAA-4AAA-AAAA-AAAAAAAAAAAA".into(),
            "digest" => bad.policy_digest = "A".repeat(64),
            "resize" => {
                bad.action = NativeCommandOperationAction::Resize {
                    columns: 501,
                    rows: 24,
                }
            }
            "input" => {
                bad.action = NativeCommandOperationAction::Stdin {
                    input: "🦀".repeat(2049),
                    close_after: false,
                }
            }
            _ => unreachable!(),
        }
        assert!(store.enqueue_requested(bad).is_err(), "{field}");
    }
    store.enqueue_requested(req).unwrap();
    let path = ledger_path(root.path(), &session.id);
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    json["operations"][0]["request"]["credentials"] = serde_json::json!("unexpected");
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(store.list(&session.id).is_err());
}

#[test]
fn command_operation_publication_process_cuts_reopen_without_automatically_repeating_effects() {
    for revision in [0, 1, 2, 3, 4, 5] {
        for stage in ["temporary-synced", "renamed", "directory-synced"] {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "native_command_operation::tests::publication_cut_child",
                    "--nocapture",
                ])
                .env("OPENIDE_COMMAND_OPERATION_CUT_ROOT", root.path())
                .env(
                    "OPENIDE_COMMAND_OPERATION_CUT_REVISION",
                    revision.to_string(),
                )
                .env("OPENIDE_COMMAND_OPERATION_CUT_STAGE", stage)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(86),
                "revision{revision}/{stage}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
            let listing = sessions.list_with_diagnostics().unwrap();
            assert_eq!(listing.sessions.len(), 1);
            let session = &listing.sessions[0].session;
            let store = NativeCommandOperationStore::new(sessions.clone());
            let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
            let published = stage != "temporary-synced";
            let before = store.list(&session.id).unwrap();
            if revision == 0 {
                assert_eq!(before.len(), usize::from(published));
            } else {
                assert_eq!(before[0].revision, revision - u64::from(!published));
                assert_eq!(
                    before[0].state.requires_verification(),
                    (published || revision > 1) && !(revision == 5 && published)
                );
                if before[0].revision >= 1 {
                    assert!(
                        !store
                            .begin_started(&lease, &before[0].request.operation_id, 0)
                            .unwrap()
                            .newly_started
                    );
                    assert_eq!(
                        store.require_no_uncertain(&lease).is_err(),
                        before[0].state.requires_verification()
                    );
                }
            }
            let bytes = fs::read(ledger_path(root.path(), &session.id)).ok();
            assert_eq!(
                store.recover_temporaries(&lease).unwrap(),
                usize::from(!published)
            );
            assert_eq!(store.recover_temporaries(&lease).unwrap(), 0);
            assert_eq!(fs::read(ledger_path(root.path(), &session.id)).ok(), bytes);
            assert!(sessions
                .load(&session.id)
                .unwrap()
                .unwrap()
                .events
                .is_empty());
        }
    }
}

#[test]
fn capacity_preparation_rejection_and_cancellation_publication_cuts_are_atomic() {
    for action in ["reserve", "reject", "cancel"] {
        for stage in ["temporary-synced", "renamed", "directory-synced"] {
            let root = tempfile::tempdir().unwrap();
            let revision = u64::from(action != "reserve");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "native_command_operation::tests::publication_cut_child",
                    "--nocapture",
                ])
                .env("OPENIDE_COMMAND_OPERATION_CUT_ROOT", root.path())
                .env("OPENIDE_COMMAND_OPERATION_CUT_ACTION", action)
                .env(
                    "OPENIDE_COMMAND_OPERATION_CUT_REVISION",
                    revision.to_string(),
                )
                .env("OPENIDE_COMMAND_OPERATION_CUT_STAGE", stage)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(86),
                "{action}/{stage}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let sessions = Arc::new(SessionStore::open(root.path()).unwrap());
            let session = &sessions.list_with_diagnostics().unwrap().sessions[0].session;
            let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
            let store = NativeCommandOperationStore::new(sessions.clone());
            let before = store.list(&session.id).unwrap();
            assert_eq!(before.len(), 1);
            let published = stage != "temporary-synced";
            if action == "reserve" {
                assert_eq!(before[0].revision, 0);
                assert_eq!(
                    read(root.path(), session).unwrap().reservations[0]
                        .prepared_budget
                        .is_some(),
                    published
                );
                store
                    .preflight_capacity(&lease, &before[0].request.operation_id, 1024 * 1024)
                    .unwrap();
            } else {
                assert_eq!(before[0].revision, u64::from(published));
                assert!(!before[0].state.requires_verification());
                let result = if action == "reject" {
                    store
                        .reject_requested(
                            &lease,
                            &before[0].request.operation_id,
                            0,
                            "Denied before effect",
                        )
                        .unwrap()
                } else {
                    store
                        .cancel_requested(
                            &lease,
                            &before[0].request.operation_id,
                            0,
                            "Cancelled before effect",
                        )
                        .unwrap()
                };
                assert_eq!(result.revision, 1);
                assert!(store.require_no_pending(&lease).is_ok());
            }
            // Retrying cleans the one unpublished temporary but preserves the
            // authoritative state and never obtains a Start/effect receipt.
            assert_eq!(store.recover_temporaries(&lease).unwrap(), 0);
            assert!(sessions
                .load(&session.id)
                .unwrap()
                .unwrap()
                .events
                .is_empty());
        }
    }
}

#[test]
#[ignore = "subprocess helper for actual atomic publication cuts"]
fn publication_cut_child() {
    let Some(root) = std::env::var_os("OPENIDE_COMMAND_OPERATION_CUT_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let sessions = Arc::new(SessionStore::open(&root).unwrap());
    let session = sessions.create(&root, "native").unwrap();
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let store = NativeCommandOperationStore::new(sessions);
    let req = request(&session);
    store.enqueue_requested(req.clone()).unwrap();
    if std::env::var("OPENIDE_COMMAND_OPERATION_CUT_ACTION").as_deref() == Ok("reject") {
        store
            .reject_requested(&lease, &req.operation_id, 0, "Denied before effect")
            .unwrap();
        panic!("publication cut not reached");
    }
    if std::env::var("OPENIDE_COMMAND_OPERATION_CUT_ACTION").as_deref() == Ok("cancel") {
        store
            .cancel_requested(&lease, &req.operation_id, 0, "Cancelled before effect")
            .unwrap();
        panic!("publication cut not reached");
    }
    store
        .preflight_capacity(&lease, &req.operation_id, 1024 * 1024)
        .unwrap();
    store.begin_started(&lease, &req.operation_id, 0).unwrap();
    store
        .record_phase(
            &lease,
            &req.operation_id,
            1,
            NativeCommandOperationPhase::BeforeCommand,
            "After pre-hook, before command",
        )
        .unwrap();
    store
        .record_effect_result(
            &lease,
            &req.operation_id,
            2,
            NativeCommandOperationOutcome::Succeeded {
                diagnostic: "Command observed, before post-hook".into(),
            },
        )
        .unwrap();
    store
        .finish(
            &lease,
            &req.operation_id,
            3,
            NativeCommandOperationOutcome::EffectUncertain {
                diagnostic: "Post-hook result lost; command remains known".into(),
            },
        )
        .unwrap();
    store
        .resolve_started(
            &lease,
            &req.operation_id,
            4,
            "Inspected process and hooks; acknowledgment, not a synthetic outcome",
        )
        .unwrap();
    panic!("publication cut not reached");
}
