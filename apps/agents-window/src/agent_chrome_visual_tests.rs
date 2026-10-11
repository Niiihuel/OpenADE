//! Opt-in snapshots of the actual Agents chrome, measured with bundled fonts.
//! Register as a child of `view` to exercise its private sidebar composition.
use std::path::Path;
use std::time::{Duration, Instant};

use openide_ui::{DrawCommand, Element, Frame, Icon, Point, Rect, Size, UiAction, UiEvent};

use crate::{
    github_account::{GithubAccountStatus, GithubIdentity},
    ide_workspace::WindowMode,
    view::{self, ViewState},
};

const DIRECTORY: &str = "/tmp/openade-agent-chrome-review";

pub(super) fn fixture() -> ViewState {
    let mut state = view::tests::empty_state();
    state.ide.mode = WindowMode::Agents;
    state.workspace = "/workspace/openADE".into();
    state.workspace_draft = state.workspace.clone();
    state.project_branch = Some("feature/compact-chrome".into());
    state.sidebar_collapsed = false;
    state.show_side_pane = false;
    state.github_account = GithubAccountStatus::Connected(GithubIdentity {
        login: "nihuel".into(),
        avatar: None,
    });
    state.sessions = vec![view::SessionView {
        id: "workspace-review".into(),
        title: "Revisar el proyecto y la interfaz".into(),
        project: state.workspace.clone(),
        backend: "native".into(),
        pinned: false,
        archived: false,
        unread: false,
        native_automation: None,
    }];
    let route = crate::composer_ui::tests::configured_effort_route();
    state.model = route.model_id.clone();
    state.composer_model = route.model_id.clone();
    state.native_provider_id = route.provider_id.clone();
    state.composer_model_options = vec![(route.model_id.clone(), route.model_name.clone())];
    state.composer_effort_options = route.effort_options.clone();
    state.composer_effort = "max".into();
    state.composer_routes = vec![route];
    state
}

fn gear_geometry(frame: &Frame) -> (Rect, Rect) {
    let target = frame
        .regions
        .iter()
        .find(|region| region.id == "toggle_settings")
        .expect("the connected account footer exposes Settings")
        .rect;
    let icon = frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::Icon {
                rect,
                icon: Icon::Settings,
                ..
            } if target.contains(Point::new(
                rect.x + rect.width / 2.0,
                rect.y + rect.height / 2.0,
            )) =>
            {
                Some(*rect)
            }
            _ => None,
        })
        .expect("the Settings target contains its native gear icon");
    assert_eq!((target.width, target.height), (24.0, 24.0));
    assert_eq!((icon.width, icon.height), (16.0, 16.0));
    assert!((icon.x + icon.width / 2.0 - target.x - target.width / 2.0).abs() < 0.01);
    assert!((icon.y + icon.height / 2.0 - target.y - target.height / 2.0).abs() < 0.01);
    let account = frame
        .regions
        .iter()
        .find(|region| region.id == "github_account_menu_toggle")
        .expect("the scene renders a connected GitHub account")
        .rect;
    assert!(
        account.x + account.width <= target.x,
        "account and Settings remain independent controls"
    );
    (target, icon)
}

fn export(frame: &Frame, size: Size, name: &str) {
    let directory = Path::new(DIRECTORY);
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}-{:.0}x{:.0}.svg", size.width, size.height));
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, size.width, size.height)
        .unwrap();
    let parsed = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixels = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    pixels.save_png(path.with_extension("png")).unwrap();
}

fn review_scene(root: Element, size: Size, name: &str) {
    let (context, mut tree) = view::ide_visual_tests::native_tree(root);
    let now = Instant::now();
    let idle = tree.layout_at(size, now);
    let geometry = gear_geometry(&idle);
    export(&idle, size, &format!("{name}-idle"));
    let point = Point::new(
        geometry.0.x + geometry.0.width / 2.0,
        geometry.0.y + geometry.0.height / 2.0,
    );
    tree.handle_event_at(UiEvent::PointerMove(point), now + Duration::from_millis(16));
    let hovered = tree.layout_at(size, now + Duration::from_millis(32));
    assert_eq!(tree.hovered_id(), Some("toggle_settings"));
    assert_eq!(
        gear_geometry(&hovered),
        geometry,
        "hover must not resize or move the gear"
    );
    export(&hovered, size, &format!("{name}-hover-settings"));
    let tooltip = tree.layout_at(size, now + Duration::from_secs(1));
    assert_eq!(gear_geometry(&tooltip), geometry);
    export(&tooltip, size, &format!("{name}-tooltip-settings"));
    tree.handle_event_at(
        UiEvent::PointerDown(point),
        now + Duration::from_millis(1100),
    );
    let actions =
        tree.handle_event_at(UiEvent::PointerUp(point), now + Duration::from_millis(1120));
    assert_eq!(
        actions,
        vec![UiAction::Clicked {
            id: "toggle_settings".into()
        }]
    );
    context.end_pass().textures_delta.clear();
}

#[test]
#[ignore = "exports real Agents chrome, compact sidebar and Settings hover with native fonts"]
fn export_agent_chrome_visual_review() {
    let state = fixture();
    for size in [Size::new(1440.0, 900.0), Size::new(520.0, 680.0)] {
        review_scene(
            view::build_window_view(&state, size.width, size.height),
            size,
            "agents",
        );
    }
    let size = Size::new(264.0, 320.0);
    review_scene(
        view::sidebar_for_height(&state, size.width, size.height),
        size,
        "sidebar",
    );
}
