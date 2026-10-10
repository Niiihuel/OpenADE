use super::*;

fn click(app: &mut AgentsWindow, id: impl Into<String>) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
}

#[test]
fn environment_escape_closes_one_level_before_closing_the_dock() {
    use crate::environment_git_ui::EnvironmentGitPanel;
    use crate::environment_ui::EnvironmentMenu;
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let ctx = egui::Context::default();
    assert!(!app.dismiss_environment_level(&ctx));
    app.view.environment.open = true;
    app.view.environment.workflow.panel = EnvironmentGitPanel::Commit;
    app.view.environment.workflow.remote_menu_open = true;
    assert!(app.dismiss_environment_level(&ctx));
    assert!(!app.view.environment.workflow.remote_menu_open);
    assert_eq!(
        app.view.environment.workflow.panel,
        EnvironmentGitPanel::Commit
    );
    assert!(app.dismiss_environment_level(&ctx));
    assert_eq!(
        app.view.environment.workflow.panel,
        EnvironmentGitPanel::Overview
    );
    assert!(app.view.environment.open);
    app.view.environment.menu = EnvironmentMenu::Project;
    app.view.composer_project_menu_open = true;
    assert!(app.dismiss_environment_level(&ctx));
    assert!(!app.view.composer_project_menu_open);
    assert_eq!(app.view.environment.menu, EnvironmentMenu::None);
    assert!(app.view.environment.open);
    assert!(app.dismiss_environment_level(&ctx));
    assert!(!app.view.environment.open);
}

#[test]
fn unarchived_delete_requires_confirmation_for_the_exact_session_and_can_be_cancelled() {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let store = app.store.clone().unwrap();
    let first = store.create(root.path(), "codex").unwrap();
    let other = store.create(root.path(), "codex").unwrap();
    app.refresh_sessions();
    assert!(!app.view.show_archived);

    click(&mut app, format!("delete_confirm:{}", first.id));
    assert!(store.load(&first.id).unwrap().is_some());
    click(&mut app, format!("session_menu:{}", first.id));
    click(&mut app, format!("session_delete:{}", first.id));
    assert_eq!(
        app.view.pending_delete_session_id.as_deref(),
        Some(first.id.as_str())
    );
    assert!(app.view.session_menu_id.is_none());
    assert!(store.load(&first.id).unwrap().is_some());
    click(&mut app, format!("delete_confirm:{}", other.id));
    assert!(store.load(&other.id).unwrap().is_some());
    click(&mut app, "delete_cancel");
    assert!(app.view.pending_delete_session_id.is_none());
    click(&mut app, format!("delete_confirm:{}", first.id));
    assert!(store.load(&first.id).unwrap().is_some());
    click(&mut app, "session_delete:unknown-session");
    assert!(app.view.pending_delete_session_id.is_none());
}

#[test]
fn deleting_during_branch_switch_keeps_the_selected_chat_and_draft_intact() {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let store = app.store.clone().unwrap();
    let session = store.create(root.path(), "codex").unwrap();
    app.refresh_sessions();
    app.open_session(&session.id);
    app.view.prompt = "keep this draft while changing branches".into();
    app.save_draft();
    let before = store.load(&session.id).unwrap();

    click(&mut app, format!("session_delete:{}", session.id));
    app.view.composer_branches.switching = Some("feature/next".into());
    click(&mut app, format!("delete_confirm:{}", session.id));
    assert_eq!(store.load(&session.id).unwrap(), before);
    assert_eq!(
        app.view.selected_session.as_deref(),
        Some(session.id.as_str())
    );
    assert_eq!(
        app.view.pending_delete_session_id.as_deref(),
        Some(session.id.as_str())
    );
    assert_eq!(app.view.prompt, "keep this draft while changing branches");
    assert!(app.view.status.contains("cambio de rama"));
    click(&mut app, "delete_cancel");
    click(&mut app, format!("session_delete:{}", session.id));
    assert!(app.view.pending_delete_session_id.is_none());
    assert_eq!(store.load(&session.id).unwrap(), before);
    assert_eq!(
        app.view.selected_session.as_deref(),
        Some(session.id.as_str())
    );
}

#[test]
fn deleting_a_cli_automation_waits_for_its_handle_to_be_retired_even_when_not_selected() {
    for selected_target in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let store = app.store.clone().unwrap();
        let target = store.create(root.path(), "codex").unwrap();
        let other = store.create(root.path(), "native").unwrap();
        app.refresh_sessions();
        let selected_id = if selected_target {
            &target.id
        } else {
            &other.id
        };
        app.open_session(selected_id);
        app.view.prompt = "preserve the selected draft".into();
        app.save_draft();
        let target_before = store.load(&target.id).unwrap();
        let other_before = store.load(&other.id).unwrap();
        click(&mut app, format!("session_delete:{}", target.id));

        // A real CLI worker queues its startup failure. Until the host consumes
        // that tail, the handle still owns the target journal for persistence.
        let mut profile = CliProfile::builtins().remove(0);
        profile.executable = root.path().join("missing-fixture-executable");
        let run = CliRunner::start(
            profile,
            AgentRunRequest {
                session_id: Some(target.id.clone()),
                sequence_start: 0,
                workspace: root.path().to_path_buf(),
                history: Vec::new(),
                prompt: "test pending automation".into(),
                mode: TurnMode::Build,
            },
        );
        app.background_automation_runs
            .push(BackgroundAutomationRun {
                automation_id: "delete-guard-fixture".into(),
                session_id: target.id.clone(),
                run: ActiveRun::Cli(run),
            });
        click(&mut app, format!("delete_confirm:{}", target.id));
        assert_eq!(store.load(&target.id).unwrap(), target_before);
        assert_eq!(store.load(&other.id).unwrap(), other_before);
        assert_eq!(
            app.view.selected_session.as_deref(),
            Some(selected_id.as_str())
        );
        assert_eq!(
            app.view.pending_delete_session_id.as_deref(),
            Some(target.id.as_str())
        );
        assert_eq!(app.view.prompt, "preserve the selected draft");
        assert_eq!(app.background_automation_runs.len(), 1);
        assert!(app.view.status.contains("automatización"));
        click(&mut app, "delete_cancel");
        click(&mut app, format!("session_delete:{}", target.id));
        assert!(app.view.pending_delete_session_id.is_none());
        assert_eq!(store.load(&target.id).unwrap(), target_before);
        assert_eq!(
            app.view.selected_session.as_deref(),
            Some(selected_id.as_str())
        );
    }
}

#[test]
fn deleting_selected_unarchived_chat_cleans_history_and_draft_without_touching_project_or_other_chat(
) {
    for backend in ["native", "codex"] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let project_file = workspace.join("keep.txt");
        std::fs::write(&project_file, "project changes must survive").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let store = app.store.clone().unwrap();
        let current = store.create(&workspace, backend).unwrap();
        let other = store.create(&workspace, "codex").unwrap();
        store
            .save_draft(&workspace, Some(&other.id), "other draft")
            .unwrap();
        store
            .save_draft(&workspace, None, "new conversation draft")
            .unwrap();
        crate::session_changes::capture_baseline(
            store.storage_root(),
            &current.id,
            &workspace,
            &[],
        )
        .unwrap();
        crate::session_changes_external::mark_external_runtime(
            store.storage_root(),
            &current.id,
            &workspace,
            0,
        )
        .unwrap();
        let baseline = store
            .storage_root()
            .join("session-changes")
            .join(format!("{}.json", current.id));
        assert!(baseline.exists());
        app.refresh_sessions();
        app.open_session(&current.id);
        app.view.prompt = "unsent text belonging only to the deleted chat".into();
        app.save_draft();
        let other_before = store.load(&other.id).unwrap().unwrap();

        click(&mut app, format!("session_delete:{}", current.id));
        click(&mut app, format!("delete_confirm:{}", current.id));

        assert!(
            store.load(&current.id).unwrap().is_none(),
            "{}",
            app.view.status
        );
        assert!(app.view.selected_session.is_none());
        assert!(app.view.pending_delete_session_id.is_none());
        assert!(!baseline.exists());
        assert_eq!(
            crate::session_changes_external::external_runtime_was_prepared(
                store.storage_root(),
                &current.id,
                &workspace
            )
            .unwrap(),
            None
        );
        assert_eq!(
            store.load_draft(&workspace, Some(&current.id)).unwrap(),
            None
        );
        assert!(!app
            .drafts
            .keys()
            .any(|(_, id)| id.as_deref() == Some(current.id.as_str())));
        assert_eq!(app.view.prompt, "new conversation draft");
        assert_eq!(store.load(&other.id).unwrap().unwrap(), other_before);
        assert_eq!(
            store
                .load_draft(&workspace, Some(&other.id))
                .unwrap()
                .as_deref(),
            Some("other draft")
        );
        assert_eq!(
            std::fs::read_to_string(&project_file).unwrap(),
            "project changes must survive"
        );
    }
}

#[test]
fn deleting_another_idle_chat_does_not_interrupt_the_selected_running_chat() {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let store = app.store.clone().unwrap();
    let selected = store.create(root.path(), "native").unwrap();
    let target = store.create(root.path(), "codex").unwrap();
    app.refresh_sessions();
    app.open_session(&selected.id);
    app.view.prompt = "continue working".into();
    app.save_draft();
    app.view.running = true;
    let _running_lease = store.try_acquire_native_lease(&selected.id).unwrap();
    click(&mut app, format!("session_delete:{}", target.id));
    click(&mut app, format!("delete_confirm:{}", target.id));
    assert!(store.load(&target.id).unwrap().is_none());
    assert!(store.load(&selected.id).unwrap().is_some());
    assert_eq!(
        app.view.selected_session.as_deref(),
        Some(selected.id.as_str())
    );
    assert_eq!(app.view.prompt, "continue working");
    assert!(app.view.running);
    // The selected running chat itself is protected until its agent stops.
    click(&mut app, format!("session_delete:{}", selected.id));
    click(&mut app, format!("delete_confirm:{}", selected.id));
    assert!(store.load(&selected.id).unwrap().is_some());
    assert!(app.view.status.contains("Detén el agente"));
}

#[test]
fn unarchived_native_tree_deletion_waits_for_children_and_leases_and_never_deletes_child_directly()
{
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    let store = app.store.clone().unwrap();
    let parent = store.create(root.path(), "native").unwrap();
    let unrelated = store.create(root.path(), "native").unwrap();
    let lease = store.try_acquire_native_lease(&parent.id).unwrap();
    let coordinator = SubagentCoordinator::new(store.clone(), SubagentLimits::default()).unwrap();
    let child = coordinator
        .create_child(
            &lease,
            "delete-active-child",
            &openide_harness::SubagentSpec {
                prompt: "Inspect".into(),
                model: "fixture".into(),
                mode: TurnMode::Build,
                context_digest: "a".repeat(64),
            },
        )
        .unwrap();
    drop(lease);
    app.refresh_sessions();
    click(&mut app, format!("session_delete:{}", parent.id));
    click(&mut app, format!("delete_confirm:{}", parent.id));
    assert!(store.load(&parent.id).unwrap().is_some());
    assert!(store.load(&child.session.id).unwrap().is_some());
    assert!(app.view.status.starts_with("No se pudo eliminar"));

    let lease = store.try_acquire_native_lease(&parent.id).unwrap();
    let run = coordinator.start_child(&lease, &child.session.id).unwrap();
    coordinator
        .finish_child(&run, openide_harness::SubagentOutcome::Completed)
        .unwrap();
    drop(run);
    click(&mut app, format!("delete_confirm:{}", parent.id));
    assert!(store.load(&parent.id).unwrap().is_some());
    drop(lease);
    app.refresh_sessions();
    app.open_session(&child.session.id);
    click(&mut app, format!("session_delete:{}", child.session.id));
    click(&mut app, format!("delete_confirm:{}", child.session.id));
    assert!(store.load(&child.session.id).unwrap().is_some());
    assert!(app.view.status.contains("conversación principal"));
    click(&mut app, format!("session_delete:{}", parent.id));
    click(&mut app, format!("delete_confirm:{}", parent.id));
    assert!(
        store.load(&parent.id).unwrap().is_none(),
        "{}",
        app.view.status
    );
    assert!(store.load(&child.session.id).unwrap().is_none());
    assert!(store.load(&unrelated.id).unwrap().is_some());
    assert!(app.view.selected_session.is_none());
}
