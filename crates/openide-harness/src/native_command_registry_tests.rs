use super::*;
use crate::{CommandHooks, LocalTool, LocalTools, NativeManualCommandPolicy, PermissionPolicy};
#[cfg(unix)]
use openide_core::{
    NativeCommandOperationAction, NativeCommandOperationState, NativeCommandOutputBinding,
};
use serde_json::{json, Value};
#[cfg(unix)]
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    sessions: Arc<SessionStore>,
    owner: String,
    second_owner: String,
    registry: Arc<NativeCommandRegistry>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let owner = sessions.create(&workspace, "native").unwrap().id;
        let second_owner = sessions.create(&workspace, "native").unwrap().id;
        let registry = Arc::new(
            NativeCommandRegistry::new(sessions.clone()).with_manual_policy_source(Arc::new(
                || {
                    Ok(NativeManualCommandPolicy {
                        permissions: PermissionPolicy::new(),
                        run_command_enabled: true,
                        isolation: CommandIsolation::HostUnrestricted,
                        hooks: Arc::new(CommandHooks::default()),
                    })
                },
            )),
        );
        Self {
            _root: root,
            workspace,
            sessions,
            owner,
            second_owner,
            registry,
        }
    }

    fn profile(&self) -> NativeCommandProfile {
        self.registry
            .profile(NativeCommandProfileOptions::default())
            .unwrap()
    }

    fn context(&self) -> ToolContext {
        ToolContext {
            workspace: self.workspace.clone(),
            cancel: CancellationToken::new(),
        }
    }

    fn base(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(LocalTools::new().with_tool_permission(LocalTool::RunCommand, true))
    }

    #[cfg(unix)]
    fn binding(&self, manager: &PersistentCommandManager, id: &str) -> NativeCommandOutputBinding {
        NativeCommandOutputBinding {
            session_id: manager.native_session_owner().unwrap().into(),
            workspace: self.workspace.clone(),
            manager_generation: manager.generation().into(),
            command_id: id.into(),
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn readonly_automation_origin_blocks_managers_overlays_and_manual_actors_without_capture() {
    let fixture = Fixture::new();
    let automation_id = uuid::Uuid::new_v4().to_string();
    let origin = openide_core::NativeAutomationOrigin::new(
        &automation_id,
        format!("{automation_id}:123456"),
        123456,
        false,
        "{\"profile\":\"read_only_v1\"}",
        "Read only",
        "exact-automation-model",
    )
    .unwrap();
    let session = fixture
        .sessions
        .create_native_automation(&fixture.workspace, origin)
        .unwrap();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&session.id)
        .unwrap();
    let profile = fixture.profile();
    assert!(profile.manager(&session.id, &fixture.workspace).is_err());
    assert!(profile.actor(fixture.sessions.clone(), &lease).is_err());
    assert!(profile
        .overlay(fixture.base(), fixture.sessions.clone(), &lease)
        .is_err());
    assert!(fixture.registry.manager(&session.id).unwrap().is_none());
    assert!(fixture
        .registry
        .manual_actor(&session.id)
        .unwrap()
        .is_none());
    assert!(
        openide_core::NativeCommandOutputStore::new(fixture.sessions.clone())
            .list(&session.id, &fixture.workspace)
            .unwrap()
            .is_empty()
    );
    assert!(fixture
        .sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        arguments,
    }
}

#[test]
fn profile_catalog_is_pure_identical_to_manager_and_identity_survives_epoch_changes() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let identity = profile.identity_digest();
    let definitions = profile.definitions();
    assert_eq!(definitions.len(), 4);
    assert_eq!(
        definitions[0],
        persistent_command_tools::legacy_start_command_definition()
    );
    assert!(fixture.registry.manager(&fixture.owner).unwrap().is_none());
    assert!(fixture
        .registry
        .manual_actor(&fixture.owner)
        .unwrap()
        .is_none());
    assert!(fixture.registry.managers().unwrap().is_empty());
    assert!(fixture
        .registry
        .output
        .list(&fixture.owner, &fixture.workspace)
        .unwrap()
        .is_empty());
    let manager = profile.manager(&fixture.owner, &fixture.workspace).unwrap();
    assert_eq!(definitions, persistent_command_tools::definitions(&manager));
    let options = NativeCommandProfileOptions {
        pty: true,
        resize: true,
        ..NativeCommandProfileOptions::default()
    };
    let terminal_profile = fixture.registry.profile(options).unwrap();
    let terminal_manager = terminal_profile
        .manager(&fixture.second_owner, &fixture.workspace)
        .unwrap();
    assert_eq!(
        terminal_profile.definitions(),
        persistent_command_tools::definitions(&terminal_manager)
    );
    assert_eq!(
        terminal_profile
            .definitions()
            .iter()
            .any(|d| d.name == "resize_command"),
        cfg!(windows)
    );
    fixture.registry.revoke_all().unwrap();
    assert_eq!(identity, fixture.profile().identity_digest());
    assert_ne!(identity, terminal_profile.identity_digest());
    let changed = NativeCommandProfileOptions {
        limits: PersistentCommandLimits {
            max_stdin_bytes: 512,
            ..PersistentCommandLimits::default()
        },
        ..NativeCommandProfileOptions::default()
    };
    assert_ne!(
        identity,
        fixture.registry.profile(changed).unwrap().identity_digest()
    );
    assert!(fixture
        .registry
        .profile(NativeCommandProfileOptions {
            limits: PersistentCommandLimits {
                max_sessions: 0,
                ..PersistentCommandLimits::default()
            },
            ..NativeCommandProfileOptions::default()
        })
        .is_err());
}

#[test]
fn overlay_requires_existing_run_capability_exact_store_scope_and_unique_reserved_names() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    assert!(profile
        .overlay(
            Arc::new(LocalTools::new()),
            fixture.sessions.clone(),
            &lease
        )
        .is_err());
    let reopened = Arc::new(SessionStore::open(fixture.sessions.storage_root()).unwrap());
    assert!(profile
        .overlay(fixture.base(), reopened.clone(), &lease)
        .is_err());
    assert!(profile.actor(reopened, &lease).is_err());
    let other_root = tempfile::tempdir().unwrap();
    let other_store = Arc::new(SessionStore::open(other_root.path()).unwrap());
    let other = other_store.create(&fixture.workspace, "native").unwrap();
    let other_lease = other_store.try_acquire_native_lease(&other.id).unwrap();
    assert!(profile
        .overlay(fixture.base(), fixture.sessions.clone(), &other_lease)
        .is_err());
    assert!(PersistentCommandToolExecutor::new(
        fixture.base(),
        profile.clone(),
        fixture.sessions.clone(),
        fixture.owner.clone(),
        other_root.path()
    )
    .is_err());
    assert!(profile
        .manager("unknown-session", &fixture.workspace)
        .is_err());
    let cli = fixture
        .sessions
        .create(&fixture.workspace, "codex")
        .unwrap();
    assert!(profile.manager(&cli.id, &fixture.workspace).is_err());
    let manager = profile.manager(&fixture.owner, &fixture.workspace).unwrap();
    let conflicting = Arc::new(
        LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_persistent_commands(manager),
    );
    assert!(profile
        .overlay(conflicting, fixture.sessions.clone(), &lease)
        .is_err());
    let overlay = profile
        .overlay(fixture.base(), fixture.sessions.clone(), &lease)
        .unwrap();
    assert!(overlay.certifies_parallel_read("read_file"));
    for name in [
        "start_command",
        "poll_command",
        "write_stdin",
        "terminate_command",
        "resize_command",
    ] {
        assert!(!overlay.certifies_parallel_read(name));
        assert!(!overlay.certifies_plan_read(name));
    }
    let different_options = fixture
        .registry
        .profile(NativeCommandProfileOptions {
            limits: PersistentCommandLimits {
                max_sessions: 1,
                ..PersistentCommandLimits::default()
            },
            ..NativeCommandProfileOptions::default()
        })
        .unwrap();
    assert!(different_options
        .manager(&fixture.owner, &fixture.workspace)
        .is_err());
    // A delegated read uses the base implementation, not a command manager.
    std::fs::write(fixture.workspace.join("read-proof"), b"base executor read").unwrap();
    let result = runtime()
        .block_on(overlay.execute(
            &call("read_file", json!({"path":"read-proof"})),
            &fixture.context(),
        ))
        .unwrap();
    assert!(result.content.contains("base executor read"));
    assert!(fixture
        .registry
        .output
        .list(&fixture.owner, &fixture.workspace)
        .unwrap()
        .is_empty());
}

#[test]
fn simultaneous_factories_share_one_generation_and_one_manual_actor() {
    let fixture = Fixture::new();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let profile = fixture.profile();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            let profile = profile.clone();
            let registry = fixture.registry.clone();
            let owner = fixture.owner.clone();
            let workspace = fixture.workspace.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let manager = profile.manager(&owner, &workspace).unwrap();
                let actor = registry.manual_actor(&owner).unwrap().unwrap();
                (manager, actor)
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    for (manager, actor) in &results {
        assert!(Arc::ptr_eq(manager, &results[0].0));
        assert!(Arc::ptr_eq(actor, &results[0].1));
        assert_eq!(actor.manager_generation(), Some(manager.generation()));
    }
    assert_eq!(fixture.registry.managers().unwrap().len(), 1);
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let worker_actor = profile
        .actor(fixture.sessions.clone(), &lease)
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&worker_actor, &results[0].1));
    fixture.registry.revoke_session(&fixture.owner).unwrap();
    assert!(fixture
        .registry
        .manual_actor(&fixture.owner)
        .unwrap()
        .is_none());
    let replacement = fixture.profile();
    let replacement_actor = replacement
        .actor(fixture.sessions.clone(), &lease)
        .unwrap()
        .unwrap();
    assert!(!Arc::ptr_eq(&replacement_actor, &worker_actor));
    assert_ne!(
        replacement_actor.manager_generation(),
        worker_actor.manager_generation()
    );
}

#[test]
fn existing_actor_lookup_is_pure_owner_bound_and_checks_store_workspace_profile_and_epoch() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let second_lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.second_owner)
        .unwrap();
    let journal = fixture
        .sessions
        .storage_root()
        .join(format!("{}.jsonl", fixture.owner));
    let before = std::fs::read(&journal).unwrap_or_default();
    assert!(profile
        .actor_existing(fixture.sessions.clone(), &lease)
        .unwrap()
        .is_none());
    assert!(fixture.registry.managers().unwrap().is_empty());
    assert!(fixture
        .registry
        .output
        .list(&fixture.owner, &fixture.workspace)
        .unwrap()
        .is_empty());
    assert!(!fixture
        .sessions
        .storage_root()
        .join(format!("{}.native-command-output.json", fixture.owner))
        .exists());
    assert_eq!(std::fs::read(&journal).unwrap_or_default(), before);
    let same_root_different_handle =
        Arc::new(SessionStore::open(fixture.sessions.storage_root()).unwrap());
    assert!(profile
        .actor_existing(same_root_different_handle, &lease)
        .is_err());
    let foreign = Fixture::new();
    let foreign_lease = foreign
        .sessions
        .try_acquire_native_lease(&foreign.owner)
        .unwrap();
    assert!(profile
        .actor_existing(fixture.sessions.clone(), &foreign_lease)
        .is_err());
    let manager = profile.manager(&fixture.owner, &fixture.workspace).unwrap();
    let ui_actor = fixture
        .registry
        .manual_actor(&fixture.owner)
        .unwrap()
        .unwrap();
    let existing = profile
        .actor_existing(fixture.sessions.clone(), &lease)
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&existing, &ui_actor));
    assert!(profile
        .actor_existing(fixture.sessions.clone(), &second_lease)
        .unwrap()
        .is_none());
    let wrong_profile = fixture
        .registry
        .profile(NativeCommandProfileOptions {
            limits: PersistentCommandLimits {
                max_stdin_bytes: 512,
                ..PersistentCommandLimits::default()
            },
            ..NativeCommandProfileOptions::default()
        })
        .unwrap();
    assert!(wrong_profile
        .actor_existing(fixture.sessions.clone(), &lease)
        .is_err());
    // A manager from another canonical workspace must be rejected even when
    // a previous actor is already cached. This is a malformed host binding,
    // not an attempted process restoration or a model-supplied workspace.
    let foreign_manager = Arc::new(
        PersistentCommandManager::new(
            foreign.workspace.clone(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_native_session_owner(fixture.owner.clone())
        .unwrap()
        .with_native_output_store(fixture.registry.output.clone())
        .unwrap(),
    );
    fixture
        .registry
        .state
        .lock()
        .unwrap()
        .managers
        .get_mut(&fixture.owner)
        .unwrap()
        .manager = foreign_manager;
    assert!(profile
        .actor_existing(fixture.sessions.clone(), &lease)
        .is_err());
    fixture
        .registry
        .state
        .lock()
        .unwrap()
        .managers
        .get_mut(&fixture.owner)
        .unwrap()
        .manager = manager;
    fixture.registry.revoke_session(&fixture.owner).unwrap();
    assert!(profile
        .actor_existing(fixture.sessions.clone(), &lease)
        .is_err());
    // A session-specific cutoff does not revoke an unrelated owner's profile.
    assert!(profile
        .actor_existing(fixture.sessions.clone(), &second_lease)
        .unwrap()
        .is_none());
    let current = fixture.profile();
    assert!(current
        .actor_existing(fixture.sessions.clone(), &lease)
        .unwrap()
        .is_none());
    current.manager(&fixture.owner, &fixture.workspace).unwrap();
    let replacement = current
        .actor_existing(fixture.sessions.clone(), &lease)
        .unwrap()
        .unwrap();
    assert!(!Arc::ptr_eq(&replacement, &ui_actor));
    assert_ne!(
        replacement.manager_generation(),
        ui_actor.manager_generation()
    );
    fixture.registry.revoke_all().unwrap();
    assert!(current
        .actor_existing(fixture.sessions.clone(), &lease)
        .is_err());
    assert!(profile
        .actor_existing(fixture.sessions.clone(), &second_lease)
        .is_err());
}

#[cfg(unix)]
async fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "registry process acceptance deadline exceeded"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[cfg(unix)]
fn copy_output(manager: &PersistentCommandManager, id: &str) -> Vec<u8> {
    manager
        .inspect_pending_output(id)
        .unwrap()
        .events
        .into_iter()
        .flat_map(|event| match event {
            crate::PersistentCommandEvent::Output { bytes, .. } => bytes,
            crate::PersistentCommandEvent::Omitted { .. } => panic!("unexpected fixture omission"),
        })
        .collect()
}

#[test]
#[cfg(unix)]
fn owner_bound_commands_survive_two_turn_runtimes_and_ack_output_before_reopened_history() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let lease_a = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let lease_b = fixture
        .sessions
        .try_acquire_native_lease(&fixture.second_owner)
        .unwrap();
    let tools_a = profile
        .overlay(fixture.base(), fixture.sessions.clone(), &lease_a)
        .unwrap();
    let tools_b = profile
        .overlay(fixture.base(), fixture.sessions.clone(), &lease_b)
        .unwrap();
    assert!(fixture.registry.managers().unwrap().is_empty());
    let first = runtime();
    let (a, b) = first.block_on(async {
        let a = tools_a
            .execute(
                &call("start_command", json!({"command":"cat"})),
                &fixture.context(),
            )
            .await
            .unwrap();
        let b = tools_b
            .execute(
                &call("start_command", json!({"command":"cat"})),
                &fixture.context(),
            )
            .await
            .unwrap();
        (
            serde_json::from_str::<Value>(&a.content).unwrap()["session_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            serde_json::from_str::<Value>(&b.content).unwrap()["session_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
    });
    drop(first);
    let manager_a = fixture.registry.manager(&fixture.owner).unwrap().unwrap();
    let manager_b = fixture
        .registry
        .manager(&fixture.second_owner)
        .unwrap()
        .unwrap();
    assert_ne!(manager_a.generation(), manager_b.generation());
    assert_ne!(a, b);
    assert_eq!(
        fixture
            .registry
            .output
            .list(&fixture.owner, &fixture.workspace)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .registry
            .output
            .list(&fixture.second_owner, &fixture.workspace)
            .unwrap()
            .len(),
        1
    );
    let second = runtime();
    let input_a = "child A café 🌱\r\n";
    let input_b = "child B\n";
    second.block_on(async {
        assert!(tools_a
            .execute(
                &call("write_stdin", json!({"session_id":b,"input":"WRONG OWNER"})),
                &fixture.context()
            )
            .await
            .is_err());
        assert!(tools_b
            .execute(
                &call("poll_command", json!({"session_id":a,"wait_ms":0})),
                &fixture.context()
            )
            .await
            .is_err());
        tools_a
            .execute(
                &call(
                    "write_stdin",
                    json!({"session_id":a,"input":input_a,"close_after":true}),
                ),
                &fixture.context(),
            )
            .await
            .unwrap();
        tools_b
            .execute(
                &call(
                    "write_stdin",
                    json!({"session_id":b,"input":input_b,"close_after":true}),
                ),
                &fixture.context(),
            )
            .await
            .unwrap();
        wait_until(|| {
            manager_a.inspect_pending_output(&a).unwrap().exit.is_some()
                && manager_b.inspect_pending_output(&b).unwrap().exit.is_some()
        })
        .await;
        assert_eq!(copy_output(&manager_a, &a), input_a.as_bytes());
        assert_eq!(copy_output(&manager_b, &b), input_b.as_bytes());
        for (manager, id, expected) in [(&manager_a, &a, input_a), (&manager_b, &b, input_b)] {
            let reopened = NativeCommandOutputStore::new(Arc::new(
                SessionStore::open(fixture.sessions.storage_root()).unwrap(),
            ));
            let page = reopened
                .read(&fixture.binding(manager, id), 0, 4096, 16)
                .unwrap();
            assert!(page.summary.terminal.as_ref().unwrap().exit_observed);
            assert_eq!(
                page.chunks
                    .into_iter()
                    .flat_map(|chunk| chunk.bytes)
                    .collect::<Vec<_>>(),
                expected.as_bytes()
            );
        }
        let final_a = tools_a
            .execute(
                &call("poll_command", json!({"session_id":a,"wait_ms":0})),
                &fixture.context(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&final_a.content).unwrap()["finished"],
            true
        );
        assert!(manager_a.list_sessions().unwrap().is_empty());
    });
}

#[test]
#[cfg(unix)]
fn shared_actor_delivers_exact_confirmed_input_and_notification_to_child_lease() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let manager = profile.manager(&fixture.owner, &fixture.workspace).unwrap();
    let command = manager
        .start("cat", &[], &CancellationToken::new())
        .unwrap();
    let ui_actor = fixture
        .registry
        .manual_actor(&fixture.owner)
        .unwrap()
        .unwrap();
    let child_actor = profile
        .actor(fixture.sessions.clone(), &lease)
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&ui_actor, &child_actor));
    let input = "human exact café\r\n🌱\r";
    let request = ui_actor
        .prepare_request(
            &command.id,
            NativeCommandOperationAction::Stdin {
                input: input.into(),
                close_after: true,
            },
        )
        .unwrap();
    let operation_id = request.operation_id.clone();
    ui_actor.enqueue_confirmed(request).unwrap();
    runtime().block_on(async {
        tokio::time::timeout(Duration::from_secs(2), child_actor.notified())
            .await
            .unwrap();
        child_actor
            .process_pending(&lease, &CancellationToken::new())
            .await
            .unwrap();
        wait_until(|| {
            manager
                .inspect_pending_output(&command.id)
                .unwrap()
                .exit
                .is_some()
        })
        .await;
    });
    assert_eq!(copy_output(&manager, &command.id), input.as_bytes());
    assert!(matches!(
        ui_actor
            .operations()
            .snapshot(&fixture.owner, &operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeCommandOperationState::Finished { .. }
    ));
    assert!(ui_actor.active_operation_id().is_none());
}

#[test]
#[cfg(unix)]
fn owner_and_global_revoke_fence_stale_profiles_without_payload_and_new_epoch_is_explicit() {
    let fixture = Fixture::new();
    let old = fixture.profile();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let stale_overlay = old
        .overlay(fixture.base(), fixture.sessions.clone(), &lease)
        .unwrap();
    let manager_a = old.manager(&fixture.owner, &fixture.workspace).unwrap();
    let manager_b = old
        .manager(&fixture.second_owner, &fixture.workspace)
        .unwrap();
    let a = manager_a
        .start("cat", &[], &CancellationToken::new())
        .unwrap();
    let b = manager_b
        .start("cat", &[], &CancellationToken::new())
        .unwrap();
    fixture.registry.revoke_session(&fixture.owner).unwrap();
    assert!(old.ensure_live(&fixture.owner).is_err());
    assert!(old.manager(&fixture.owner, &fixture.workspace).is_err());
    assert!(old
        .manager(&fixture.second_owner, &fixture.workspace)
        .is_ok());
    let payload = ["-c".into(), "printf EFFECT > should-not-exist".into()];
    assert!(manager_a
        .start("/bin/sh", &payload, &CancellationToken::new())
        .is_err());
    assert!(runtime()
        .block_on(stale_overlay.execute(
            &call("start_command", json!({"command":"/bin/sh","args":payload})),
            &fixture.context()
        ))
        .is_err());
    assert!(!fixture.workspace.join("should-not-exist").exists());
    runtime().block_on(wait_until(|| {
        manager_a
            .inspect_pending_output(&a.id)
            .unwrap()
            .exit
            .is_some()
    }));
    assert!(manager_a.control_snapshot(&a.id).unwrap().revoked);
    assert!(manager_b
        .inspect_pending_output(&b.id)
        .unwrap()
        .exit
        .is_none());
    let replacement = fixture.profile();
    let manager_new = replacement
        .manager(&fixture.owner, &fixture.workspace)
        .unwrap();
    assert_ne!(manager_new.generation(), manager_a.generation());
    let new_command = manager_new
        .start(
            "/bin/sh",
            &["-c".into(), "printf NEW-EPOCH".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    assert_ne!(new_command.id, a.id);
    runtime().block_on(wait_until(|| {
        manager_new
            .inspect_pending_output(&new_command.id)
            .unwrap()
            .exit
            .is_some()
    }));
    assert_eq!(copy_output(&manager_new, &new_command.id), b"NEW-EPOCH");
    fixture.registry.revoke_all().unwrap();
    assert!(old.ensure_live(&fixture.second_owner).is_err());
    assert!(replacement
        .manager(&fixture.owner, &fixture.workspace)
        .is_err());
    assert!(manager_b
        .start("/bin/sh", &payload, &CancellationToken::new())
        .is_err());
    assert!(fixture.registry.managers().unwrap().is_empty());
    assert!(!fixture.workspace.join("should-not-exist").exists());
    assert_eq!(old.identity_digest(), fixture.profile().identity_digest());
}

#[test]
#[cfg(unix)]
fn registry_drop_contains_real_descendant_tree_even_with_retained_manager_and_worker_profile() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let owner = sessions.create(&workspace, "native").unwrap().id;
    let registry = Arc::new(NativeCommandRegistry::new(sessions.clone()));
    let profile = registry
        .profile(NativeCommandProfileOptions::default())
        .unwrap();
    let manager = profile.manager(&owner, &workspace).unwrap();
    let command = manager
        .start(
            "/bin/sh",
            &[
                "-c".into(),
                "(while :; do printf . >> heartbeat; sleep 0.03; done) & printf READY; wait".into(),
            ],
            &CancellationToken::new(),
        )
        .unwrap();
    runtime().block_on(wait_until(|| {
        copy_output(&manager, &command.id) == b"READY"
            && std::fs::read(workspace.join("heartbeat"))
                .unwrap_or_default()
                .len()
                >= 2
    }));
    drop(registry);
    assert!(profile.ensure_live(&owner).is_err());
    assert!(profile.manager(&owner, &workspace).is_err());
    profile.revoke_owner(&owner).unwrap(); // dropped pool is already contained
    runtime().block_on(wait_until(|| {
        manager
            .inspect_pending_output(&command.id)
            .unwrap()
            .exit
            .is_some()
    }));
    std::thread::sleep(Duration::from_millis(100));
    let heartbeat = std::fs::read(workspace.join("heartbeat")).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        std::fs::read(workspace.join("heartbeat")).unwrap(),
        heartbeat
    );
    assert!(manager.control_snapshot(&command.id).unwrap().revoked);
    let reopened = NativeCommandOutputStore::new(Arc::new(
        SessionStore::open(sessions.storage_root()).unwrap(),
    ));
    let binding = NativeCommandOutputBinding {
        session_id: owner,
        workspace,
        manager_generation: manager.generation().into(),
        command_id: command.id,
    };
    assert!(reopened
        .read(&binding, 0, 1024, 16)
        .unwrap()
        .summary
        .terminal
        .is_some());
}

#[test]
#[cfg(unix)]
fn poisoned_registry_revocation_still_stops_process_and_corrupt_capture_blocks_new_payload() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let manager = profile.manager(&fixture.owner, &fixture.workspace).unwrap();
    let command = manager
        .start("cat", &[], &CancellationToken::new())
        .unwrap();
    let poison = fixture.registry.clone();
    assert!(std::thread::spawn(move || {
        let _guard = poison.state.lock().unwrap();
        panic!("intentional registry poison acceptance");
    })
    .join()
    .is_err());
    assert!(fixture.registry.revoke_all().is_err());
    runtime().block_on(wait_until(|| {
        manager
            .inspect_pending_output(&command.id)
            .unwrap()
            .exit
            .is_some()
    }));
    assert!(manager.control_snapshot(&command.id).unwrap().revoked);
    assert!(profile
        .manager(&fixture.second_owner, &fixture.workspace)
        .is_err());
    let clean = Fixture::new();
    let manager = clean
        .profile()
        .manager(&clean.owner, &clean.workspace)
        .unwrap();
    let sidecar = clean
        .sessions
        .storage_root()
        .join(format!("{}.native-command-output.json", clean.owner));
    std::fs::write(&sidecar, b"not a capture").unwrap();
    assert!(manager
        .start(
            "/bin/sh",
            &["-c".into(), "printf EFFECT > should-not-exist".into()],
            &CancellationToken::new()
        )
        .is_err());
    assert!(!clean.workspace.join("should-not-exist").exists());
    assert_eq!(std::fs::read(sidecar).unwrap(), b"not a capture");
}

#[test]
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[ignore = "requires trusted Bubblewrap and working Linux namespaces/seccomp/pidfd/close_range"]
fn offline_registry_uses_real_confinement_and_durable_capture_without_host_fallback() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let fixture = Fixture::new();
    let outside = fixture._root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"safe").unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), fixture.workspace.join("escape")).unwrap();
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let _unix =
        std::os::unix::net::UnixListener::bind(fixture.workspace.join("local.sock")).unwrap();
    let secret = std::fs::File::open(outside.join("secret")).unwrap();
    let fd = unsafe { libc::fcntl(secret.as_raw_fd(), libc::F_DUPFD, 128) };
    assert!(fd >= 128);
    let canary = unsafe { OwnedFd::from_raw_fd(fd) };
    let source = fixture.workspace.join("probe.rs");
    let binary = fixture.workspace.join("probe");
    std::fs::write(
        &source,
        include_str!("../tests/fixtures/linux_sandbox_probe.rs"),
    )
    .unwrap();
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
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let profile = fixture
        .registry
        .profile(NativeCommandProfileOptions {
            isolation: CommandIsolation::WorkspaceWriteOffline,
            ..NativeCommandProfileOptions::default()
        })
        .unwrap();
    let manager = profile.manager(&fixture.owner, &fixture.workspace).unwrap();
    assert_eq!(manager.isolation(), CommandIsolation::WorkspaceWriteOffline);
    assert!(!manager.pty_tools_enabled());
    assert_eq!(
        profile.definitions(),
        persistent_command_tools::definitions(&manager)
    );
    let command = manager
        .start(
            binary.to_str().unwrap(),
            &[
                "confinement".into(),
                outside.to_str().unwrap().into(),
                tcp.local_addr().unwrap().to_string(),
                canary.as_raw_fd().to_string(),
            ],
            &CancellationToken::new(),
        )
        .unwrap();
    runtime().block_on(wait_until(|| {
        manager
            .inspect_pending_output(&command.id)
            .unwrap()
            .exit
            .is_some()
    }));
    let final_copy = manager.inspect_pending_output(&command.id).unwrap();
    assert!(final_copy.reader_error.is_none());
    assert_eq!(
        final_copy.exit,
        Some(crate::PersistentCommandExit::Exited { code: Some(0) })
    );
    assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"safe");
    assert_eq!(
        std::fs::read(fixture.workspace.join("inside")).unwrap(),
        b"workspace-ok"
    );
    let page = fixture
        .registry
        .output
        .read(&fixture.binding(&manager, &command.id), 0, 4096, 16)
        .unwrap();
    assert_eq!(
        page.chunks
            .into_iter()
            .flat_map(|chunk| chunk.bytes)
            .collect::<Vec<_>>(),
        b"confinement-ok\n"
    );
    assert!(page.summary.terminal.unwrap().exit_observed);
}
