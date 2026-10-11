use super::*;
use openide_ui::{Frame, HitKind, Point, Rect, Size, UiEvent, UiTree};

fn state(session: bool) -> ViewState {
    let mut state = crate::view::tests::empty_state();
    state.sidebar_collapsed = true;
    state.show_side_pane = false;
    state.environment.open = true;
    if session {
        state.selected_session = Some("environment-layout".into());
        state.selected_session_title = Some("Revisar el entorno del proyecto".into());
        state.transcript = vec![TranscriptItem::Assistant {
            content: "El chat comparte el espacio con el entorno sin quedar debajo de la tarjeta."
                .into(),
            streaming: false,
        }];
    }
    state
}

fn frame(state: &ViewState, width: f32, height: f32) -> Frame {
    let mut tree = UiTree::new(crate::view::build_window_view(state, width, height));
    tree.set_reduced_motion(true);
    tree.layout(Size::new(width, height))
}

fn rect(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
        .rect
}

#[test]
fn environment_column_reflows_home_and_session_composer_at_every_animation_step() {
    for session in [false, true] {
        let mut state = state(session);
        state.environment.open = false;
        let closed = frame(&state, 1600.0, 900.0);
        let closed_prompt = rect(&closed, "prompt");
        let header_button = rect(&closed, "environment_open");
        let closed_center = closed_prompt.x + closed_prompt.width / 2.0;
        assert!(!closed
            .regions
            .iter()
            .any(|region| region.id == "chat_environment"));
        state.environment.open = true;
        let mut previous_center = closed_center;
        for progress in [0.1, 0.25, 0.5, 0.75, 1.0] {
            state.environment_presentation_progress = Some(progress);
            let open = frame(&state, 1600.0, 900.0);
            let prompt = rect(&open, "prompt");
            assert_eq!(
                rect(&open, "environment_open"),
                header_button,
                "header must retain its full-width position"
            );
            let panel = rect(&open, "chat_environment");
            let center = prompt.x + prompt.width / 2.0;
            assert!(
                center < previous_center,
                "composer must recenter continuously"
            );
            previous_center = center;
            assert!(
                prompt.x + prompt.width <= panel.x,
                "prompt overlaps panel at {progress}"
            );
            assert!(panel.x + panel.width <= 1600.0);
            if session {
                let transcript = rect(&open, "transcript");
                assert!(transcript.x + transcript.width <= panel.x);
                assert!(transcript.height > 0.0);
            }
            assert!(!open
                .regions
                .iter()
                .any(|region| region.id.starts_with("popover_surface:")));
        }
        state.environment.open = false;
        state.environment_presentation_progress = Some(0.5);
        assert!(frame(&state, 1600.0, 900.0)
            .regions
            .iter()
            .any(|region| region.id == "chat_environment"));
        state.environment_presentation_progress = None;
        assert_eq!(rect(&frame(&state, 1600.0, 900.0), "prompt"), closed_prompt);
    }
}

#[test]
fn narrow_environment_uses_bounded_vertical_space_and_leaves_composer_visible() {
    for (width, height) in [(360.0, 540.0), (520.0, 700.0), (620.0, 700.0)] {
        for session in [false, true] {
            let state = state(session);
            let frame = frame(&state, width, height);
            let panel = rect(&frame, "chat_environment");
            let prompt = rect(&frame, "prompt");
            assert!(panel.height <= height * 0.35, "panel is bounded at {width}");
            assert!(
                panel.y + panel.height < prompt.y,
                "panel precedes chat at {width}"
            );
            assert!(
                prompt.y + prompt.height <= height,
                "composer remains visible at {width}"
            );
            assert!(prompt.width > 120.0);
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "environment_dismiss"));
        }
    }
}

#[test]
fn environment_selectors_float_over_the_card_without_resizing_it() {
    let closed = {
        let state = state(true);
        let mut tree = UiTree::new(crate::view::build_window_view(&state, 900.0, 480.0));
        tree.set_reduced_motion(true);
        rect(&tree.layout(Size::new(900.0, 480.0)), "chat_environment")
    };
    for menu in [
        EnvironmentMenu::Local,
        EnvironmentMenu::Branch,
        EnvironmentMenu::Project,
    ] {
        let mut state = state(true);
        state.environment.menu = menu;
        state.composer_branches.open = menu == EnvironmentMenu::Branch;
        state.composer_project_menu_open = menu == EnvironmentMenu::Project;
        state.composer_project_options = (0..30)
            .map(|index| (format!("/project/{index}"), format!("Proyecto {index}")))
            .collect();
        let mut tree = UiTree::new(crate::view::build_window_view(&state, 900.0, 480.0));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(900.0, 480.0));
        let panel = rect(&frame, "chat_environment");
        let transcript = rect(&frame, "transcript");
        assert!(transcript.x + transcript.width <= panel.x);
        // An open menu is a surface over the card: the card keeps its box.
        assert_eq!(panel, closed, "{menu:?} resized the card");
        let surface = frame
            .regions
            .iter()
            .find(|region| region.id.starts_with("popover_surface:"))
            .unwrap_or_else(|| panic!("{menu:?} opened no floating menu"))
            .rect;
        assert!(surface.x >= 0.0 && surface.x + surface.width <= 900.0, "{menu:?}: {surface:?}");
        assert!(surface.y >= 0.0 && surface.y + surface.height <= 480.0, "{menu:?}: {surface:?}");
    }
}

#[test]
#[ignore = "native compact environment visual review; synthetic state, no commands executed"]
fn export_environment_visual_review() {
    let output = std::path::Path::new("/tmp/openade-environment-review");
    std::fs::create_dir_all(output).unwrap();
    let render = |root: Element, size: Size, name: &str| {
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(root);
        let frame = tree.layout(size);
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
        pixels.save_png(output.join(format!("{name}.png"))).unwrap();
        context.end_pass().textures_delta.clear();
    };
    for session in [false, true] {
        let mut state = state(session);
        state.workspace = "/home/openide/projects/openADE".into();
        state.project_branch = Some("feature/environment-layout".into());
        state.git_review = Some(Ok(crate::git_review::GitReview {
            additions: 248,
            deletions: 31,
            files: 8,
            ..Default::default()
        }
        .into()));
        state.project_actions.workspace = Some(state.workspace.clone().into());
        state.project_actions.actions = vec![crate::project_actions::ProjectAction {
            id: "check".into(),
            label: "Verificar proyecto".into(),
            command: "cargo check --workspace".into(),
        }];
        let mode = if session { "session" } else { "home" };
        for width in [1200.0, 1600.0] {
            render(
                crate::view::build_window_view(&state, width, 900.0),
                Size::new(width, 900.0),
                &format!("{mode}-{width}"),
            );
        }
        if session {
            for width in [280.0, 380.0] {
                render(
                    panel(&state, width, 344.0),
                    Size::new(width, 344.0),
                    &format!("card-{width}"),
                );
            }
            state.environment.workflow.panel =
                crate::environment_git_ui::EnvironmentGitPanel::Actions;
            render(
                panel(&state, 380.0, 520.0),
                Size::new(380.0, 520.0),
                "actions-380",
            );
        }
    }
}
