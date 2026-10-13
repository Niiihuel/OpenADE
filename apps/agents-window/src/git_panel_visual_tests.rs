//! Native fixtures for the source control sidebar. Every snapshot is in
//! memory: painting and clicking the UiTree never invokes Git or a host action.
use super::*;
use crate::environment_git::GitSnapshot;
use crate::git_review::{GitTracking, ReviewSection};
use openide_ui::{DrawCommand, Frame, HitKind, Key, Point, Rect, Size, UiAction, UiEvent};
use std::time::{Duration, Instant};

const BRANCH: &str = "codex/ui-component-quality";
const UPSTREAM: &str = "origin/codex/ui-component-quality";
const WIDTHS: [f32; 5] = [240.0, 280.0, 320.0, 420.0, 480.0];

fn file(path: &str, status: &str, additions: usize, deletions: usize) -> ReviewFile {
    ReviewFile {
        path: path.into(),
        status: status.into(),
        additions,
        deletions,
        ..Default::default()
    }
}

fn review() -> GitReview {
    let mut review = GitReview {
        branch: BRANCH.into(),
        tracking: Some(GitTracking {
            upstream: UPSTREAM.into(),
            ahead: 2,
            behind: 0,
        }),
        sections: vec![
            ReviewSection {
                title: "CAMBIOS PREPARADOS".into(),
                files: vec![
                    // A partially prepared file occurs in both groups. Keep their
                    // actions distinct instead of deduplicating the row identity.
                    file("README.md", "Modificado", 18, 3),
                    file("crates/openide-ui/src/tree.rs", "Modificado", 120, 30),
                    file("scripts/build.py", "Nuevo", 40, 0),
                ],
                additions: 178,
                deletions: 33,
            },
            ReviewSection {
                title: "CAMBIOS SIN PREPARAR".into(),
                files: vec![
                    file("Cargo.toml", "Modificado", 2, 1),
                    file("README.md", "Modificado", 11000, 3200),
                    file(
                        "apps/agents-window/src/workspace/editor/very_long_editor_layout_name.rs",
                        "Renombrado",
                        400,
                        200,
                    ),
                    file(
                        "assets/design/system/preview-for-narrow-workspaces.png",
                        "Binario",
                        0,
                        0,
                    ),
                    file("docs/obsolete.md", "Eliminado", 0, 100),
                    file("src/merge.rs", "Conflicto", 5, 5),
                ],
                additions: 11407,
                deletions: 3506,
            },
        ],
        untracked: vec![
            "docs/proposals/streaming-ui-review.md".into(),
            "docs/diseño/distribución-compacta-del-editor.md".into(),
            ".openide/skills/workspace-review/SKILL.md".into(),
        ],
        files: 9,
        additions: 11585,
        deletions: 3539,
        last_commit: Some(("56badd9".into(), "Usage cards sit in two independent columns".into())),
        ..Default::default()
    };
    review.group_conflicts();
    review
}

/// The IDE on its source control activity, over a repository that has
/// something of every group and two commits to send.
fn fixture() -> ViewState {
    let mut state = crate::view::ide_visual_tests::fixture();
    state.ide.activity = crate::ide_workspace::IdeActivity::Git;
    state.project_branch = Some(BRANCH.into());
    state.git_review = Some(Ok(Arc::new(review())));
    remote(&mut state, Some(BRANCH), Some(UPSTREAM), &["origin"], 2, 0);
    state
}

/// The repository snapshot the commit and sync controls read.
fn remote(
    state: &mut ViewState,
    branch: Option<&str>,
    upstream: Option<&str>,
    remotes: &[&str],
    ahead: usize,
    behind: usize,
) {
    let workflow = &mut state.environment.workflow;
    workflow.snapshot = Some(GitSnapshot::fixture(branch, upstream, remotes, ahead, behind));
    workflow.push_remote = remotes.first().copied().unwrap_or_default().into();
    workflow.push_branch = branch.unwrap_or_default().into();
    if let Some(Ok(review)) = &mut state.git_review {
        let review = Arc::make_mut(review);
        review.branch = branch.unwrap_or("HEAD separado").into();
        review.tracking = upstream.map(|upstream| GitTracking {
            upstream: upstream.into(),
            ahead,
            behind,
        });
    }
}

fn clean(state: &mut ViewState) {
    if let Some(Ok(review)) = &mut state.git_review {
        let review = Arc::make_mut(review);
        review.sections.clear();
        review.untracked.clear();
        review.files = 0;
        review.conflicts = 0;
    }
}

fn sidebar(state: &ViewState, width: f32, height: f32) -> Element {
    super::super::ide_activity_ui::git_sidebar(state, width, height)
}

fn frame_of(state: &ViewState, width: f32) -> Frame {
    let (context, mut tree) =
        crate::view::ide_visual_tests::native_tree(sidebar(state, width, 760.0));
    let frame = tree.layout(Size::new(width, 760.0));
    context.end_pass().textures_delta.clear();
    frame
}

fn bounds(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
        .rect
}

fn is_button(frame: &Frame, id: &str) -> bool {
    frame
        .regions
        .iter()
        .any(|region| region.id == id && region.kind == HitKind::Button)
}

/// Everything the frame says: wrapped blocks whole, and each painted line.
fn copy(frame: &Frame) -> String {
    frame
        .text_layouts()
        .iter()
        .map(|block| block.source.clone())
        .chain(frame.commands.iter().filter_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.clone()),
            _ => None,
        }))
        .collect::<Vec<_>>()
        .join("\n")
}

fn center(rect: Rect) -> Point {
    Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

#[test]
fn the_sidebar_keeps_every_control_inside_its_width_from_240_to_480() {
    for width in WIDTHS {
        let mut state = fixture();
        state.environment.workflow.commit_message = "Dock chrome follows the sidebar".into();
        if let Some(Ok(review)) = &mut state.git_review {
            // Without the conflict the commit can go ahead.
            let review = Arc::make_mut(review);
            review.sections[1].files.remove(0);
            review.conflicts = 0;
        }
        state.git_panel.revealed = [Some("1:1".into()), Some("g:u".into())];
        let viewport = Size::new(width, 760.0);
        let (context, mut tree) =
            crate::view::ide_visual_tests::native_tree(sidebar(&state, width, viewport.height));
        let frame = tree.layout(viewport);
        for region in &frame.regions {
            assert!(
                region.rect.x >= -0.01 && region.rect.x + region.rect.width <= width + 0.01,
                "{} escapes {width}px: {:?}",
                region.id,
                region.rect
            );
        }
        // Top to bottom: branch and sync, the message, its button, the list.
        let branch = bounds(&frame, "ide_git_branch_toggle");
        let sync = bounds(&frame, "ide_git_sync");
        let message = bounds(&frame, "environment_git_commit_message");
        let commit = bounds(&frame, "ide_git_commit");
        let list = bounds(&frame, "git_change_files");
        assert!(branch.x + branch.width <= sync.x);
        assert!((center(branch).y - center(sync).y).abs() < 0.01);
        assert!(branch.width >= 96.0, "the branch keeps room at {width}: {branch:?}");
        assert!(branch.y + branch.height <= message.y);
        assert!(message.y + message.height <= commit.y);
        assert!(commit.y + commit.height <= list.y);
        assert!((message.width - (width - 12.0)).abs() < 0.01);
        assert!((commit.x + commit.width - (width - 6.0)).abs() < 0.01);

        // A revealed row keeps its tools clear of the reserved rail.
        let stage = bounds(&frame, "review_stage:1:1");
        let row = bounds(&frame, "review_file_open:1:1");
        assert_eq!(row.height, 26.0);
        assert_eq!((stage.width, stage.height), (24.0, 24.0));
        assert!((center(row).y - center(stage).y).abs() < 0.01);
        assert!(stage.x + stage.width <= list.x + list.width - 14.0 + 0.01);
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text == "README.md"
        )));

        for (id, action) in [
            ("review_stage:1:1", "review_stage:1:1"),
            ("review_file_open:1:1", "review_file_open:1:1"),
            ("review_stage_all:u", "review_stage_all:u"),
            ("review_group_toggle:s", "review_group_toggle:s"),
            ("ide_git_commit", "environment_git_commit"),
            ("ide_git_sync", "environment_git_push"),
            ("ide_git_branch_toggle", "ide_git_branch_toggle"),
        ] {
            let rect = bounds(&frame, id);
            let point = Point::new(rect.x + rect.width.min(16.0) / 2.0, center(rect).y);
            assert_eq!(frame.hit_test(point), Some(id), "{id} at {width}");
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked { id: action.into() }],
                "{id} at {width}"
            );
        }
        assert!(tree.focus("review_stage:1:1"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "review_stage:1:1".into()
            }]
        );
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn a_revealed_row_hovers_as_one_surface_across_its_target_and_its_tools() {
    for width in [240.0, 320.0, 420.0] {
        let mut state = fixture();
        state.git_panel.revealed = [Some("1:1".into()), None];
        let viewport = Size::new(width, 760.0);
        let (context, mut tree) =
            crate::view::ide_visual_tests::native_tree(sidebar(&state, width, viewport.height));
        let now = Instant::now();
        let initial = tree.layout_at(viewport, now);
        let row = bounds(&initial, "review_file_open:1:1");
        let stage = bounds(&initial, "review_stage:1:1");
        let targets = initial.regions.clone();
        let expected = theme::action_palette(theme::SIDEBAR, theme::FOREGROUND).hover;
        for (index, id, point) in [
            (0, "review_file_open:1:1", Point::new(row.x + 8.0, center(row).y)),
            (1, "review_stage:1:1", center(stage)),
        ] {
            let at = now + Duration::from_millis(index * 220);
            tree.handle_event_at(UiEvent::PointerMove(point), at);
            // Inspect settled paint before a tooltip can overlay the row.
            let hovered = tree.layout_at(viewport, at + Duration::from_millis(1));
            assert_eq!(hovered.regions, targets, "hover must not alter the targets");
            assert_eq!(hovered.hit_test(point), Some(id));
            // Either side of the seam, away from text and the 24 px glyph.
            for x in [row.x + 6.0, row.x + row.width - 0.5, stage.x + 0.5] {
                for y in [row.y + 0.5, row.y + row.height - 0.5] {
                    let probe = Point::new(x, y);
                    let paint = hovered
                        .commands
                        .iter()
                        .rev()
                        .find_map(|command| match command {
                            DrawCommand::FillRect { rect, color, .. }
                                if color.a == 255 && rect.contains(probe) =>
                            {
                                Some(*color)
                            }
                            _ => None,
                        })
                        .expect("the row has an opaque background");
                    assert_eq!(paint, expected, "{id} at {width}px breaks at {probe:?}");
                }
            }
        }
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn a_running_operation_keeps_navigation_and_holds_what_changes_files() {
    let mut state = fixture();
    state.git_action_pending = true;
    state.environment.workflow.commit_message = "Message".into();
    state.git_panel.revealed = [Some("1:1".into()), Some("g:n".into())];
    let frame = frame_of(&state, 320.0);
    for id in [
        "review_stage:1:1",
        "review_discard:1:1",
        "review_stage_all:n",
        "review_discard_all:n",
        "ide_git_commit",
        "ide_git_sync",
    ] {
        assert!(!is_button(&frame, id), "{id}");
        // It stays where it was, with its hint.
        assert!(frame.regions.iter().any(|region| region.id == id), "{id}");
    }
    for id in [
        "review_file_open:1:1",
        "review_file_open:0:0",
        "review_untracked_open:0",
        "review_group_toggle:u",
        "review_refresh",
        "ide_git_branch_toggle",
    ] {
        assert!(is_button(&frame, id), "{id}");
    }
    // Said once, in the status line; the composer keeps what will go in.
    assert!(copy(&frame).contains("Actualizando archivos…"));
    assert!(!copy(&frame).contains("Operación de Git en curso"));
    assert!(copy(&frame).contains("3 archivos preparados"));
}

#[test]
fn the_commit_button_says_why_it_is_off() {
    let mut state = fixture();
    let review = |state: &ViewState| state.git_review.clone().unwrap().unwrap();
    let blocked = |state: &ViewState| commit_blocked(state, &review(state));
    // The fixture has a conflict.
    assert_eq!(blocked(&state).as_deref(), Some("Resolvé 1 conflicto"));
    if let Some(Ok(review)) = &mut state.git_review {
        let review = Arc::make_mut(review);
        review.sections[1].files.remove(0);
        review.conflicts = 0;
    }
    assert_eq!(blocked(&state).as_deref(), Some("Falta el mensaje"));
    let frame = frame_of(&state, 280.0);
    assert!(!is_button(&frame, "ide_git_commit"));
    assert!(copy(&frame).contains("Falta el mensaje"));

    state.environment.workflow.commit_message = "  \n".into();
    assert_eq!(blocked(&state).as_deref(), Some("Falta el mensaje"));
    state.environment.workflow.commit_message = "Dock chrome follows the sidebar".into();
    assert_eq!(blocked(&state), None);
    let frame = frame_of(&state, 280.0);
    assert!(is_button(&frame, "ide_git_commit"));
    assert!(copy(&frame).contains("3 archivos preparados"));

    let snapshot = state.environment.workflow.snapshot.take();
    assert_eq!(blocked(&state).as_deref(), Some("Leyendo el repositorio…"));
    state.environment.workflow.snapshot = snapshot;

    state.running = true;
    assert_eq!(blocked(&state).as_deref(), Some("El agente está trabajando"));
    state.running = false;
    state.environment.workflow.busy = Some("Creando commit…".into());
    assert_eq!(blocked(&state).as_deref(), Some("Operación de Git en curso"));
    state.environment.workflow.busy = None;

    if let Some(Ok(review)) = &mut state.git_review {
        Arc::make_mut(review).sections.remove(0);
    }
    assert_eq!(blocked(&state).as_deref(), Some("Sin archivos preparados"));
    let frame = frame_of(&state, 280.0);
    assert!(!is_button(&frame, "ide_git_commit"));
    assert!(copy(&frame).contains("Sin archivos preparados"));
}

#[test]
fn the_sync_control_follows_what_the_branch_can_exchange() {
    let step = |state: &ViewState| sync_step(state, &state.git_review.clone().unwrap().unwrap());
    let click = |state: &ViewState| {
        let (context, mut tree) =
            crate::view::ide_visual_tests::native_tree(sidebar(state, 320.0, 760.0));
        let frame = tree.layout(Size::new(320.0, 760.0));
        let point = center(bounds(&frame, "ide_git_sync"));
        tree.handle_event(UiEvent::PointerDown(point));
        let actions = tree.handle_event(UiEvent::PointerUp(point));
        context.end_pass().textures_delta.clear();
        (copy(&frame), actions)
    };
    let clicked = |id: &str| vec![UiAction::Clicked { id: id.into() }];
    let mut state = fixture();

    assert_eq!(step(&state), Some(SyncStep::Push { ahead: 2 }));
    let (text, actions) = click(&state);
    assert!(text.contains("Enviar 2"), "{text}");
    assert_eq!(actions, clicked("environment_git_push"));

    remote(&mut state, Some(BRANCH), Some(UPSTREAM), &["origin"], 2, 3);
    assert_eq!(step(&state), Some(SyncStep::Pull { behind: 3, ahead: 2 }));
    let (text, actions) = click(&state);
    assert!(text.contains("Recibir 3"), "{text}");
    assert_eq!(actions, clicked("environment_git_pull"));

    remote(&mut state, Some(BRANCH), Some(UPSTREAM), &["origin"], 0, 0);
    assert_eq!(step(&state), Some(SyncStep::Fetch));
    assert_eq!(click(&state).1, clicked("environment_git_fetch"));

    remote(&mut state, Some(BRANCH), None, &["origin"], 0, 0);
    assert_eq!(step(&state), Some(SyncStep::Publish));
    let (text, actions) = click(&state);
    assert!(text.contains("Publicar"), "{text}");
    assert_eq!(actions, clicked("environment_git_push"));

    // No remote: nothing to exchange, so no control.
    remote(&mut state, Some(BRANCH), None, &[], 0, 0);
    assert_eq!(step(&state), None);
    assert!(!frame_of(&state, 320.0).regions.iter().any(|r| r.id == "ide_git_sync"));

    // A detached HEAD cannot be published; it can still look for news.
    remote(&mut state, None, None, &["origin"], 0, 0);
    assert_eq!(step(&state), Some(SyncStep::Fetch));

    // Sending waits for the agent; the click does nothing.
    remote(&mut state, Some(BRANCH), Some(UPSTREAM), &["origin"], 2, 0);
    state.running = true;
    let frame = frame_of(&state, 320.0);
    assert!(!is_button(&frame, "ide_git_sync"));
    assert!(frame.regions.iter().any(|region| region.id == "ide_git_sync"));
}

#[test]
fn every_state_of_the_repository_is_said_plainly() {
    let mut state = fixture();
    state.git_review = None;
    let frame = frame_of(&state, 280.0);
    assert!(copy(&frame).contains("Cargando cambios"));
    assert!(!frame.regions.iter().any(|r| r.id == "ide_git_commit"));

    state.git_review = Some(Err(crate::git_review::NOT_A_REPOSITORY.into()));
    let frame = frame_of(&state, 280.0);
    let text = copy(&frame);
    assert!(text.contains("Sin repositorio"), "{text}");
    assert!(text.contains("Esta carpeta no es un repositorio de Git."));
    // Nothing to commit to, nothing to retry.
    for id in ["ide_git_commit", "ide_git_branch_toggle", "environment_git_commit_message"] {
        assert!(!frame.regions.iter().any(|region| region.id == id), "{id}");
    }
    assert_eq!(
        frame.regions.iter().filter(|r| r.id == "review_refresh").count(),
        1,
        "only the header's refresh"
    );

    state.git_review = Some(Err("fatal: unable to read tree".into()));
    let frame = frame_of(&state, 280.0);
    let text = copy(&frame);
    assert!(text.contains("No se pudieron cargar los cambios"));
    assert!(text.contains("fatal: unable to read tree") && text.contains("Reintentar"));

    let mut state = fixture();
    clean(&mut state);
    let frame = frame_of(&state, 280.0);
    let text = copy(&frame);
    assert!(text.contains("Sin cambios"), "{text}");
    assert!(text.contains("Último commit: 56badd9 Usage cards sit in two independent columns"));
    assert!(text.contains("Sin archivos preparados"));
    assert!(!frame.regions.iter().any(|r| r.id == "git_change_files"));
    if let Some(Ok(review)) = &mut state.git_review {
        Arc::make_mut(review).last_commit = None;
    }
    assert!(copy(&frame_of(&state, 280.0)).contains("El árbol de trabajo está limpio."));

    let mut state = fixture();
    remote(&mut state, None, None, &["origin"], 0, 0);
    let frame = frame_of(&state, 280.0);
    let text = copy(&frame);
    assert!(text.contains("HEAD separado"));
    assert!(text.contains("Los commits nuevos no quedan en ninguna rama."));
    assert!(is_button(&frame, "review_detached_branch"));

    let mut state = fixture();
    state.environment.workflow.busy = Some("Enviando commits…".into());
    assert!(copy(&frame_of(&state, 280.0)).contains("Enviando commits…"));
    state.environment.workflow.busy = None;
    state.environment.workflow.error =
        Some("! [rejected] main -> main (fetch first)\nerror: failed to push some refs".into());
    let frame = frame_of(&state, 280.0);
    let text = copy(&frame);
    assert!(text.contains("Git no completó la operación"), "{text}");
    assert!(text.contains("! [rejected] main -> main (fetch first)"));
    assert!(text.contains("error: failed to push some refs"));
    assert!(is_button(&frame, "review_error_dismiss"));
    // What failed while staging is said in the same place.
    state.environment.workflow.error = None;
    state.git_action_error = Some("fatal: pathspec 'x' did not match any files".into());
    assert!(copy(&frame_of(&state, 280.0)).contains("fatal: pathspec 'x'"));
    state.git_action_error = None;
    state.environment.workflow.notice = Some("Commit creado: 0c1d2e3f".into());
    let frame = frame_of(&state, 280.0);
    assert!(copy(&frame).contains("Commit creado: 0c1d2e3f"));
    assert!(!copy(&frame).contains("Git no completó la operación"));
    state.environment.workflow.notice = None;
    assert!(!frame_of(&state, 280.0)
        .regions
        .iter()
        .any(|region| region.id == "review_error_dismiss"));
}

#[test]
fn the_overflow_menu_lists_what_the_worker_can_do_and_turns_off_what_it_cannot_now() {
    let mut state = fixture();
    state.ide.git_actions_open = true;
    let frame = frame_of(&state, 280.0);
    let text = copy(&frame);
    for label in [
        "Buscar cambios (fetch)",
        "Recibir (pull, solo avance rápido)",
        "Enviar (push)",
        "Enviar a otro destino…",
        "Crear rama…",
        "Comparar rama…",
        "Crear worktree…",
    ] {
        assert!(text.contains(label), "{label} in {text}");
    }
    for id in [
        "environment_git_fetch",
        "environment_git_pull",
        "environment_git_push",
        "environment_git_open_commit",
        "environment_git_open_branch",
        "environment_git_open_compare",
        "environment_git_open_worktree",
    ] {
        assert!(is_button(&frame, id), "{id}");
    }
    remote(&mut state, Some(BRANCH), None, &[], 0, 0);
    let frame = frame_of(&state, 280.0);
    for id in [
        "environment_git_fetch",
        "environment_git_pull",
        "environment_git_push",
        "environment_git_open_commit",
    ] {
        assert!(!is_button(&frame, id), "{id} needs a remote");
    }
    assert!(is_button(&frame, "environment_git_open_branch"));
    // Outside a repository there is nothing to branch or compare.
    state.git_review = Some(Err(crate::git_review::NOT_A_REPOSITORY.into()));
    let frame = frame_of(&state, 280.0);
    for id in [
        "environment_git_open_branch",
        "environment_git_open_compare",
        "environment_git_open_worktree",
    ] {
        assert!(!is_button(&frame, id), "{id}");
    }
}

#[test]
fn the_branch_control_opens_the_shared_branch_picker() {
    let mut state = fixture();
    assert!(!frame_of(&state, 280.0).regions.iter().any(|r| r.id == "composer_branch_search"));
    state.composer_branches.open = true;
    state.composer_branches.snapshot = Some(crate::git_branches::BranchSnapshot::fixture(
        Some(BRANCH),
        &["main", "release/1.4"],
    ));
    state.environment.menu = crate::environment_ui::EnvironmentMenu::Branch;
    let frame = frame_of(&state, 280.0);
    assert!(frame.regions.iter().any(|r| r.id == "composer_branch_search"));
    let text = copy(&frame);
    assert!(text.contains("release/1.4") && text.contains("Crear nueva rama…"), "{text}");
    // The composer owns the picker while the menu is not the sidebar's.
    state.environment.menu = crate::environment_ui::EnvironmentMenu::None;
    assert!(!frame_of(&state, 280.0).regions.iter().any(|r| r.id == "composer_branch_search"));
}

#[test]
#[ignore = "exports every state of the source control sidebar as PNG under docs/ui-separation/shots/git-panel"]
fn export_git_sidebar_visual_review() {
    let output =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ui-separation/shots/git-panel");
    std::fs::create_dir_all(&output).unwrap();
    #[derive(Clone, Copy)]
    enum Pointer {
        None,
        Hover(&'static str),
        Menu(&'static str),
    }
    let render = |root: Element, size: Size, name: &str, pointer: Pointer| {
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
        let now = Instant::now();
        let mut frame = tree.layout_at(size, now);
        match pointer {
            Pointer::None => {}
            Pointer::Hover(id) => {
                let target = bounds(&frame, id);
                tree.handle_event_at(
                    UiEvent::PointerMove(Point::new(target.x + 12.0, center(target).y)),
                    now,
                );
                // Before a hint covers what is being shown.
                frame = tree.layout_at(size, now + Duration::from_millis(250));
            }
            Pointer::Menu(id) => {
                let target = bounds(&frame, id);
                tree.handle_event_at(
                    UiEvent::ContextMenu {
                        position: Point::new(target.x + 48.0, center(target).y),
                    },
                    now,
                );
                frame = tree.layout_at(size, now + Duration::from_millis(750));
            }
        }
        let path = output.join(format!("{name}.svg"));
        crate::settings_ui::visual_qa::write_native_frame_svg(
            &frame,
            &path,
            size.width,
            size.height,
        )
        .unwrap();
        let parsed = resvg::usvg::Tree::from_str(
            &std::fs::read_to_string(&path).unwrap(),
            &crate::settings_ui::visual_qa::native_svg_options(),
        )
        .unwrap();
        let mut pixels =
            resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
        resvg::render(
            &parsed,
            resvg::tiny_skia::Transform::identity(),
            &mut pixels.as_mut(),
        );
        pixels.save_png(path.with_extension("png")).unwrap();
        std::fs::remove_file(path).unwrap();
        context.end_pass().textures_delta.clear();
    };
    let edit = |change: &dyn Fn(&mut ViewState)| {
        let mut state = fixture();
        change(&mut state);
        state
    };
    let filled = |state: &mut ViewState| {
        state.environment.workflow.commit_message =
            "Dock chrome follows the sidebar\n\nThe header keeps its tools at 24 px.".into();
        if let Some(Ok(review)) = &mut state.git_review {
            let review = Arc::make_mut(review);
            review.sections[1].files.remove(0);
            review.conflicts = 0;
        }
    };
    let states: Vec<(&str, ViewState, Pointer)> = vec![
        ("changes", fixture(), Pointer::None),
        (
            "row-hover",
            edit(&|state| state.git_panel.revealed = [Some("1:2".into()), None]),
            Pointer::Hover("review_file_open:1:2"),
        ),
        (
            "group-hover",
            edit(&|state| state.git_panel.revealed = [Some("g:u".into()), None]),
            Pointer::Hover("review_group_toggle:u"),
        ),
        ("row-menu", fixture(), Pointer::Menu("review_file_open:1:2")),
        (
            "collapsed",
            edit(&|state| state.git_panel.collapsed = [false, true, false, true]),
            Pointer::None,
        ),
        ("composer-filled", edit(&filled), Pointer::None),
        (
            "composer-disabled",
            edit(&|state| {
                filled(state);
                state.running = true;
            }),
            Pointer::None,
        ),
        (
            "busy",
            edit(&|state| {
                filled(state);
                state.environment.workflow.busy = Some("Enviando commits…".into());
            }),
            Pointer::None,
        ),
        (
            "failed",
            edit(&|state| {
                state.environment.workflow.error = Some(
                    "To github.com:team/project.git\n ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs to 'github.com:team/project.git'"
                        .into(),
                )
            }),
            Pointer::None,
        ),
        (
            "committed",
            edit(&|state| {
                state.environment.workflow.notice = Some("Commit creado: 0c1d2e3f".into())
            }),
            Pointer::None,
        ),
        (
            "behind",
            edit(&|state| remote(state, Some(BRANCH), Some(UPSTREAM), &["origin"], 2, 3)),
            Pointer::None,
        ),
        (
            "unpublished",
            edit(&|state| remote(state, Some(BRANCH), None, &["origin"], 0, 0)),
            Pointer::None,
        ),
        (
            "detached",
            edit(&|state| remote(state, None, None, &["origin"], 0, 0)),
            Pointer::None,
        ),
        (
            "clean",
            edit(&|state| {
                clean(state);
                remote(state, Some(BRANCH), Some(UPSTREAM), &["origin"], 0, 0);
            }),
            Pointer::None,
        ),
        ("loading", edit(&|state| state.git_review = None), Pointer::None),
        (
            "not-a-repository",
            edit(&|state| {
                state.git_review = Some(Err(crate::git_review::NOT_A_REPOSITORY.into()))
            }),
            Pointer::None,
        ),
        (
            "load-failed",
            edit(&|state| {
                state.git_review = Some(Err("Git no pudo leer los cambios del proyecto".into()))
            }),
            Pointer::None,
        ),
        (
            "more-menu",
            edit(&|state| state.ide.git_actions_open = true),
            Pointer::None,
        ),
        (
            "branch-picker",
            edit(&|state| {
                state.composer_branches.open = true;
                state.composer_branches.snapshot =
                    Some(crate::git_branches::BranchSnapshot::fixture(
                        Some(BRANCH),
                        &["main", "release/1.4", "wt/sticky"],
                    ));
                state.environment.menu = crate::environment_ui::EnvironmentMenu::Branch;
            }),
            Pointer::None,
        ),
    ];
    for (name, state, pointer) in &states {
        for width in [280.0, 420.0] {
            // The picker and the menus float past the sidebar's edge.
            let canvas = if matches!(*name, "branch-picker" | "row-menu") {
                f32::max(width, 420.0)
            } else {
                width
            };
            render(
                Element::row(vec![
                    sidebar(state, width, 760.0),
                    Element::spacer().flex(1.0),
                ])
                .background(theme::BACKGROUND),
                Size::new(canvas, 760.0),
                &format!("{name}-{width:.0}"),
                *pointer,
            );
        }
    }
    let size = Size::new(1440.0, 900.0);
    let window = |state: &ViewState| crate::view::build_window_view(state, size.width, size.height);
    let mut state = fixture();
    filled(&mut state);
    render(window(&state), size, "ide-1440", Pointer::None);
    for (name, trash, count) in [
        ("discard-one", false, 1),
        ("discard-many", false, 8),
        ("trash-one", true, 1),
    ] {
        state.git_panel.confirm = Some(GitConfirmView {
            workspace: state.workspace.clone(),
            trash,
            paths: (0..count)
                .map(|index| format!("apps/agents-window/src/dock_chrome_{index}.rs").into())
                .collect(),
            hunk: None,
        });
        render(window(&state), size, &format!("{name}-1440"), Pointer::None);
    }
}
