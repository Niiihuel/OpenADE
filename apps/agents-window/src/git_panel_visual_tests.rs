//! Native visual fixtures for the shared Git sidebar. Every snapshot is in
//! memory: painting and clicking the UiTree never invokes Git or a host action.
use super::*;
use crate::git_review::{GitTracking, ReviewSection};
use openide_ui::{DrawCommand, Frame, HitKind, Key, Point, Rect, Size, UiAction, UiEvent};
use std::time::{Duration, Instant};

fn fixture() -> ViewState {
    let mut state = crate::view::ide_visual_tests::fixture();
    state.ide.activity = crate::ide_workspace::IdeActivity::Git;
    state.project_branch = Some("codex/ui-component-quality".into());
    let file = |path: &str, status: &str, additions, deletions| ReviewFile {
        path: path.into(),
        status: status.into(),
        additions,
        deletions,
        ..Default::default()
    };
    let sections = vec![
        ReviewSection {
            title: "CAMBIOS SIN PREPARAR".into(),
            files: vec![
                file("README.md", "Modificado", 11000, 3200),
                file("Cargo.toml", "Modificado", 2200, 1300),
                file(
                    "apps/agents-window/src/workspace/editor/very_long_editor_layout_name.rs",
                    "Renombrado",
                    400,
                    200,
                ),
                file("docs/obsolete.md", "Eliminado", 0, 100),
                file(
                    "assets/design/system/preview-for-narrow-workspaces.png",
                    "Binario",
                    0,
                    0,
                ),
                file("src/merge.rs", "Conflicto", 5, 5),
                file("docs/guides/copied-overview.md", "Copiado", 0, 0),
            ],
            additions: 13605,
            deletions: 4805,
        },
        ReviewSection {
            title: "CAMBIOS PREPARADOS".into(),
            files: vec![
                // A partially prepared file occurs in both groups. Keep their
                // actions distinct instead of deduplicating the row identity.
                file("README.md", "Modificado", 1800, 30),
                file("crates/openide-ui/src/tree.rs", "Modificado", 120, 30),
                file("scripts/build.py", "Modificado", 40, 6),
            ],
            additions: 1960,
            deletions: 66,
        },
    ];
    state.git_review = Some(Ok(Arc::new(GitReview {
        branch: "codex/ui-component-quality".into(),
        tracking: Some(GitTracking {
            upstream: "origin/codex/ui-component-quality".into(),
            ahead: 2,
            behind: 1,
        }),
        sections,
        untracked: vec![
            "docs/proposals/streaming-ui-review.md".into(),
            "docs/diseño/distribución-compacta-del-editor.md".into(),
            ".openide/skills/workspace-review/SKILL.md".into(),
        ],
        files: 10,
        additions: 15565,
        deletions: 4871,
        ..Default::default()
    })));
    state
}

fn sidebar(state: &ViewState, width: f32, height: f32) -> Element {
    super::super::ide_activity_ui::git_sidebar(state, width, height)
}

fn bounds(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
        .rect
}

#[test]
fn native_sidebar_rows_keep_stage_actions_inside_their_scroll_budget() {
    for width in [240.0, 320.0, 420.0] {
        let state = fixture();
        let viewport = Size::new(width, 760.0);
        let (context, mut tree) =
            crate::view::ide_visual_tests::native_tree(sidebar(&state, width, viewport.height));
        let frame = tree.layout(viewport);
        let scroll = bounds(&frame, "git_change_files");
        let file = bounds(&frame, "review_file_open:0:0");
        let stage = bounds(&frame, "review_stage:0:0");
        assert!((file.height - 28.0).abs() < 0.001);
        assert!((stage.width - 24.0).abs() < 0.001);
        assert!((stage.height - 24.0).abs() < 0.001);
        assert!((file.y + file.height / 2.0 - stage.y - stage.height / 2.0).abs() < 0.001);
        assert!(
            stage.x + stage.width <= scroll.x + scroll.width - 14.0 + 0.01,
            "stage action stays clear of the reserved rail: {stage:?}, {scroll:?}"
        );
        for region in frame
            .regions
            .iter()
            .filter(|region| region.id.starts_with("review_"))
        {
            assert!(
                region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.01,
                "{} escapes {width}px: {:?}",
                region.id,
                region.rect
            );
        }
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "README.md"
            )),
            "the root basename is visible at {width}px"
        );
        let point = Point::new(stage.x + stage.width / 2.0, stage.y + stage.height / 2.0);
        assert_eq!(frame.hit_test(point), Some("review_stage:0:0"));
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "review_stage:0:0".into()
            }]
        );
        assert!(tree.focus("review_stage:0:0"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "review_stage:0:0".into()
            }]
        );
        let point = Point::new(file.x + 8.0, file.y + file.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "review_file_open:0:0".into()
            }]
        );
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn native_git_hover_is_continuous_across_file_and_staging_without_changing_targets() {
    for surface in [theme::SIDEBAR, theme::BACKGROUND] {
        for width in [240.0, 320.0, 420.0] {
            for (action, stage_action, prepared, status) in [
                (
                    "review_file_open:0:0",
                    "review_stage:0:0",
                    false,
                    "Modificado",
                ),
                (
                    "review_file_open:1:0",
                    "review_unstage:1:0",
                    true,
                    "Modificado",
                ),
                (
                    "review_untracked_open:0",
                    "review_stage_untracked:0",
                    false,
                    "Nuevo",
                ),
            ] {
                let row = ChangeRow::File {
                    action: action.into(),
                    stage_action: stage_action.into(),
                    prepared,
                    path: "crates/openide-ui/src/hover_surface.rs".into(),
                    status: status.into(),
                    additions: 12,
                    deletions: 3,
                };
                let viewport = Size::new(width, 96.0);
                let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                    Element::column(vec![
                        Element::spacer().size(None, Some(32.0)),
                        change_row(&row, width, false, surface),
                        Element::spacer().flex(1.0),
                    ])
                    .background(surface),
                );
                let now = Instant::now();
                let initial = tree.layout_at(viewport, now);
                let file = bounds(&initial, action);
                let stage = bounds(&initial, stage_action);
                assert!((file.height - 28.0).abs() < 0.001);
                assert!((stage.width - 24.0).abs() < 0.001 && (stage.height - 24.0).abs() < 0.001);
                assert!((file.x + file.width - stage.x).abs() < 0.001);
                let targets = initial.regions.clone();
                for (index, id, point) in [
                    (
                        0,
                        action,
                        Point::new(file.x + 8.0, file.y + file.height / 2.0),
                    ),
                    (
                        1,
                        stage_action,
                        Point::new(stage.x + stage.width / 2.0, stage.y + stage.height / 2.0),
                    ),
                ] {
                    let at = now + Duration::from_millis(index * 220);
                    tree.handle_event_at(UiEvent::PointerMove(point), at);
                    // Inspect settled paint before a tooltip can overlay the
                    // row. The native fixture also enables reduced motion.
                    let hovered = tree.layout_at(viewport, at + Duration::from_millis(1));
                    assert_eq!(
                        hovered.regions, targets,
                        "hover must not alter click/focus regions"
                    );
                    assert_eq!(hovered.hit_test(point), Some(id));
                    let expected = theme::action_palette(surface, theme::FOREGROUND).hover;
                    let expected_radius = openide_ui::RadiusRole::Control.px();
                    // Probe inside the outer curves and either side of the seam.
                    // Top/bottom strips avoid text and the centered 24px glyph
                    // control, catching a gray cell or rounded hover island.
                    for x in [
                        file.x + expected_radius,
                        file.x + file.width - 0.5,
                        stage.x + 0.5,
                        stage.x + stage.width - expected_radius,
                    ] {
                        for y in [file.y + 0.5, file.y + file.height - 0.5] {
                            let probe = Point::new(x, y);
                            let (paint, radius) = hovered
                                .commands
                                .iter()
                                .rev()
                                .find_map(|command| match command {
                                    DrawCommand::FillRect {
                                        rect,
                                        color,
                                        radius,
                                    } if color.a == 255 && rect.contains(probe) => {
                                        Some((*color, *radius))
                                    }
                                    _ => None,
                                })
                                .expect("the row has an opaque native background");
                            assert_eq!(
                                paint, expected,
                                "{id} at {width}px breaks its hover at {probe:?}"
                            );
                            assert_eq!(
                                radius, expected_radius,
                                "one continuous hover uses the shared control radius"
                            );
                        }
                    }
                    tree.handle_event_at(
                        UiEvent::PointerDown(point),
                        at + Duration::from_millis(2),
                    );
                    assert_eq!(
                        tree.handle_event_at(
                            UiEvent::PointerUp(point),
                            at + Duration::from_millis(3)
                        ),
                        vec![UiAction::Clicked { id: id.into() }],
                        "hover never merges the two actions"
                    );
                }
                context.end_pass().textures_delta.clear();
            }
        }
    }
}

#[test]
fn pending_git_work_keeps_navigation_and_disables_both_staging_directions() {
    let mut state = fixture();
    state.git_action_pending = true;
    let viewport = Size::new(320.0, 900.0);
    let (context, mut tree) = crate::view::ide_visual_tests::native_tree(sidebar(
        &state,
        viewport.width,
        viewport.height,
    ));
    let frame = tree.layout(viewport);
    for id in [
        "review_stage:0:0",
        "review_unstage:1:0",
        "review_stage_untracked:0",
    ] {
        assert!(
            !frame
                .regions
                .iter()
                .any(|region| region.id == id && region.kind == HitKind::Button),
            "{id}"
        );
    }
    for id in [
        "review_file_open:0:0",
        "review_file_open:1:0",
        "review_untracked_open:0",
    ] {
        assert!(
            frame
                .regions
                .iter()
                .any(|region| region.id == id && region.kind == HitKind::Button),
            "{id}"
        );
        let file = bounds(&frame, id);
        let disabled_action = Point::new(file.x + file.width + 12.0, file.y + file.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(disabled_action));
        assert!(
            tree.handle_event(UiEvent::PointerUp(disabled_action))
                .is_empty(),
            "the disabled staging target must not open the diff: {id}"
        );
    }
    context.end_pass().textures_delta.clear();
}

#[test]
#[ignore = "native compact Git sidebar review; synthetic repository snapshots only"]
fn export_git_sidebar_visual_review() {
    let output = Path::new("/tmp/openade-git-sidebar-review");
    std::fs::create_dir_all(output).unwrap();
    let render = |root: Element, size: Size, name: &str, hover: Option<&str>| {
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
        let now = Instant::now();
        let mut frame = tree.layout_at(size, now);
        if let Some(id) = hover {
            let target = bounds(&frame, id);
            tree.handle_event_at(
                UiEvent::PointerMove(Point::new(
                    target.x + target.width / 2.0,
                    target.y + target.height / 2.0,
                )),
                now,
            );
            frame = tree.layout_at(size, now + Duration::from_millis(750));
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
        context.end_pass().textures_delta.clear();
    };
    let state = fixture();
    for width in [240.0, 320.0, 420.0] {
        for (name, hover) in [
            ("idle", None),
            ("file-hover", Some("review_file_open:0:2")),
            ("stage-hover", Some("review_stage:0:2")),
        ] {
            render(
                sidebar(&state, width, 760.0),
                Size::new(width, 760.0),
                &format!("git-sidebar-{width:.0}-{name}"),
                hover,
            );
        }
    }
    let size = Size::new(1440.0, 900.0);
    render(
        crate::view::build_window_view(&state, size.width, size.height),
        size,
        "git-workspace-1440",
        None,
    );
    let mut pending = fixture();
    pending.git_action_pending = true;
    render(
        sidebar(&pending, 320.0, 760.0),
        Size::new(320.0, 760.0),
        "git-sidebar-320-pending",
        None,
    );
}
