use super::*;
use std::process::Command;

fn git(workspace: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(workspace)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn finish_review(app: &mut AgentsWindow, workspace: &Path) {
    app.view.side_tab = SideTab::CodeReview;
    app.request_git_review(workspace.to_owned());
    let context = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(8);
    while app.review_in_flight {
        assert!(Instant::now() < deadline, "{}", app.view.status);
        app.update_git_review(&context);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        matches!(app.view.git_review, Some(Ok(_))),
        "{:?}",
        app.view.git_review
    );
}

#[test]
fn markers_compact_terminal_without_durable_checkpoint_never_claims_success() {
    let workspace = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(data.path().to_owned());
    let store = app.store.clone().unwrap();
    let session = store.create(workspace.path(), "native").unwrap();
    app.open_session(&session.id);
    let (sender, receiver) = std::sync::mpsc::channel();
    app.active_compact = Some(ActiveCompact {
        session_id: session.id.clone(),
        receiver,
        cancel: CancellationToken::new(),
    });
    app.view
        .conversation_activity
        .begin_compaction(&session.id, workspace.path());
    // The host must verify the durable marker even if a worker channel reports
    // a success sequence. This is a UI boundary test, not a provider fixture.
    sender.send(Ok(99)).unwrap();
    assert!(app.poll_compact());
    assert!(app.active_compact.is_none());
    assert!(app.view.conversation_activity.compacting_session.is_none());
    assert_eq!(
        app.view.status,
        "No se pudo confirmar el checkpoint guardado después de compactar"
    );
    assert!(!app.view.transcript.iter().any(|item| matches!(
        item,
        crate::transcript::TranscriptItem::Activity(
            crate::transcript::TranscriptActivity::Checkpoint { .. }
        )
    )));
    assert!(store.load(&session.id).unwrap().unwrap().events.is_empty());
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(data.path().to_owned());
    reopened.open_session(&session.id);
    assert!(crate::session_surface::host_activity_markers(&reopened.view).is_empty());
    assert!(!reopened.view.transcript.iter().any(|item| matches!(
        item,
        crate::transcript::TranscriptItem::Activity(
            crate::transcript::TranscriptActivity::Checkpoint { .. }
        )
    )));
}

#[test]
fn markers_git_review_callback_observes_external_switch_without_journaling_checkout_and_reopen_resets(
) {
    let workspace = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    git(workspace.path(), &["init", "--initial-branch=before"]);
    git(
        workspace.path(),
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "baseline",
        ],
    );
    let mut app = AgentsWindow::with_data_root(data.path().to_owned());
    let store = app.store.clone().unwrap();
    let session = store.create(workspace.path(), "native").unwrap();
    app.open_session(&session.id);
    finish_review(&mut app, workspace.path());
    assert!(app
        .view
        .conversation_activity
        .branch_change(workspace.path(), Some(&session.id))
        .is_none());
    git(workspace.path(), &["switch", "-c", "after"]);
    finish_review(&mut app, workspace.path());
    let change = app
        .view
        .conversation_activity
        .branch_change(workspace.path(), Some(&session.id))
        .unwrap();
    assert_eq!(
        (change.previous.as_str(), change.current.as_str()),
        ("before", "after")
    );
    let markers = crate::session_surface::host_activity_markers(&app.view);
    let mut tree = UiTree::new(Element::column(markers));
    let frame = tree.layout(Size::new(420.0, 200.0));
    assert!(frame.text_layouts().iter().any(|text| text
        .source
        .contains("Rama cambiada: before → after · observada en Git")));
    assert!(!tree.is_animating());
    assert!(frame.regions.is_empty());
    assert!(
        store.load(&session.id).unwrap().unwrap().events.is_empty(),
        "a branch observation must not invent a journal tool effect"
    );
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(data.path().to_owned());
    reopened.open_session(&session.id);
    finish_review(&mut reopened, workspace.path());
    assert!(reopened
        .view
        .conversation_activity
        .branch_change(workspace.path(), Some(&session.id))
        .is_none());
    assert!(crate::session_surface::host_activity_markers(&reopened.view).is_empty());
}
