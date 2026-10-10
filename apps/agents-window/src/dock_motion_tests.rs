use super::*;
use crate::ide_workspace::{IdeBottomPanel, WindowMode};

fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
    app.view = crate::view::ide_visual_tests::fixture();
    app.viewport_width = 1440.0;
    app.viewport_height = 900.0;
    app.view.ide.bottom_panel = IdeBottomPanel::Terminal;
    (root, app)
}

fn set_panels(app: &mut AgentsWindow, open: bool) {
    app.view.ide.explorer_open = open;
    app.view.ide.agent_open = open;
    app.view.show_terminal_panel = open;
    app.view.ide.bottom_open = open;
}

#[test]
fn ide_dock_motion_reflows_all_panels_and_reverses_without_replacing_content() {
    let (_root, mut app) = fixture();
    let start = Instant::now();
    assert!(!app.update_dock_motion(1440.0, start));
    let full = dock_layout(&app.view, 1440.0);
    let full_bottom = crate::view::terminal_panel_height(&app.view, 900.0);
    let original = app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap();
    let terminal_ids = app
        .view
        .terminal_tabs
        .iter()
        .map(|tab| tab.id)
        .collect::<Vec<_>>();
    let preferred = (
        app.view.ide.explorer_width,
        app.view.ide.agent_width,
        app.view.terminal_panel_height,
    );
    set_panels(&mut app, false);
    assert!(app.update_dock_motion(1440.0, start));
    assert_eq!(dock_layout(&app.view, 1440.0), full);
    assert_eq!(
        crate::view::terminal_panel_height(&app.view, 900.0),
        full_bottom
    );
    let midway = start + Duration::from_millis(80);
    assert!(app.update_dock_motion(1440.0, midway));
    let closing = dock_layout(&app.view, 1440.0);
    let bottom = crate::view::terminal_panel_height(&app.view, 900.0);
    assert!(closing.left > 0.0 && closing.left < full.left);
    assert!(closing.right.unwrap() > 0.0 && closing.right < full.right);
    assert!(closing.center > full.center);
    assert!(bottom > 0.0 && bottom < full_bottom);
    set_panels(&mut app, true);
    app.update_dock_motion(1440.0, midway);
    assert_eq!(dock_layout(&app.view, 1440.0), closing);
    assert_eq!(crate::view::terminal_panel_height(&app.view, 900.0), bottom);
    assert!(!app.update_dock_motion(1440.0, midway + Duration::from_millis(220)));
    assert_eq!(dock_layout(&app.view, 1440.0), full);
    assert_eq!(
        crate::view::terminal_panel_height(&app.view, 900.0),
        full_bottom
    );

    let close = midway + Duration::from_millis(240);
    set_panels(&mut app, false);
    app.update_dock_motion(1440.0, close);
    app.update_dock_motion(1440.0, close + Duration::from_millis(220));
    let hidden = dock_layout(&app.view, 1440.0);
    assert_eq!((hidden.left, hidden.right), (0.0, None));
    assert_eq!(crate::view::terminal_panel_height(&app.view, 900.0), 0.0);
    assert_eq!(
        (
            app.view.left_presentation_width,
            app.view.right_presentation_width,
            app.view.bottom_presentation_height
        ),
        (None, None, None)
    );
    assert_eq!(
        (
            app.view.ide.explorer_width,
            app.view.ide.agent_width,
            app.view.terminal_panel_height
        ),
        preferred
    );
    assert_eq!(
        app.view
            .terminal_tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        terminal_ids
    );
    let current = app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(
        (
            current.document_id(),
            current.revision(),
            current.selection()
        ),
        (
            original.document_id(),
            original.revision(),
            original.selection()
        )
    );
}

#[test]
fn dock_motion_reduced_motion_drag_and_resizes_settle_immediately() {
    for mode in [WindowMode::Ide, WindowMode::Agents] {
        for operation in ["reduced", "drag", "resize"] {
            let (_root, mut app) = fixture();
            app.view.ide.mode = mode;
            let start = Instant::now();
            app.update_dock_motion(1440.0, start);
            set_panels(&mut app, false);
            app.view.sidebar_collapsed = true;
            app.view.show_side_pane = false;
            app.update_dock_motion(1440.0, start);
            let midway = start + Duration::from_millis(80);
            assert!(app.update_dock_motion(1440.0, midway));
            let width = match operation {
                "reduced" => {
                    app.reduced_motion = true;
                    1440.0
                }
                "drag" => {
                    app.dock_drag = Some(DockDrag::Bottom);
                    1440.0
                }
                _ => {
                    app.viewport_height = 720.0;
                    1040.0
                }
            };
            assert!(
                !app.update_dock_motion(width, midway),
                "{mode:?}: {operation}"
            );
            assert_eq!(
                (
                    app.view.left_presentation_width,
                    app.view.right_presentation_width,
                    app.view.bottom_presentation_height
                ),
                (None, None, None)
            );
            assert_eq!(
                (
                    app.view.ide.explorer_content_width,
                    app.view.ide.agent_content_width
                ),
                (None, None)
            );
        }
    }
}

#[test]
fn responsive_toggles_animate_neighbor_allocation_and_retain_hidden_content_width() {
    for width in [900.0, 1200.0] {
        let (_root, mut app) = fixture();
        app.viewport_width = width;
        app.view.ide.explorer_open = false;
        app.view.ide.agent_open = true;
        if width == 1200.0 {
            app.view.ide.explorer_width = 420.0;
            app.view.ide.agent_width = 720.0;
        }
        let start = Instant::now();
        app.update_dock_motion(width, start);
        let original = dock_layout(&app.view, width);
        app.view.ide.explorer_open = true;
        app.update_dock_motion(width, start);
        assert_eq!(
            dock_layout(&app.view, width),
            original,
            "no initial layout jump"
        );
        let target = crate::ide_workspace::nominal_layout(&app.view, width);
        assert!(target.right.unwrap_or(0.0) < original.right.unwrap());
        let midway = start + Duration::from_millis(80);
        app.update_dock_motion(width, midway);
        let presented = dock_layout(&app.view, width);
        assert!(presented.right.unwrap() < original.right.unwrap());
        assert!(presented.right.unwrap() > target.right.unwrap_or(0.0));
        assert_eq!(app.view.ide.agent_content_width, original.right);
        assert!(presented.center >= original.center.min(target.center) - 4.0);
        assert!(presented.center <= original.center.max(target.center) + 4.0);
        app.view.ide.explorer_open = false;
        app.update_dock_motion(width, midway);
        assert_eq!(
            dock_layout(&app.view, width),
            presented,
            "reverse without a jump"
        );
        assert_eq!(app.view.ide.agent_content_width, original.right);
        app.update_dock_motion(width, midway + Duration::from_millis(220));
        assert_eq!(dock_layout(&app.view, width), original);
        assert!(app.view.ide.agent_content_width.is_none());
        assert!(app.view.ide.explorer_content_width.is_none());
    }
}

#[test]
fn bottom_motion_expands_to_maximized_height_and_settings_do_not_keep_hidden_geometry() {
    let (_root, mut app) = fixture();
    let start = Instant::now();
    app.update_dock_motion(1440.0, start);
    let initial = crate::view::terminal_panel_height(&app.view, 900.0);
    app.view.terminal_maximized = true;
    app.update_dock_motion(1440.0, start);
    let target = crate::view::nominal_terminal_panel_height(&app.view, 900.0);
    assert!(target > initial);
    app.update_dock_motion(1440.0, start + Duration::from_millis(80));
    let midway = crate::view::terminal_panel_height(&app.view, 900.0);
    assert!(midway > initial && midway < target);
    app.view.show_settings = true;
    app.update_dock_motion(1440.0, start + Duration::from_millis(90));
    assert_eq!(crate::view::terminal_panel_height(&app.view, 900.0), 0.0);
    assert!(app.view.bottom_presentation_height.is_none());
    app.view.show_settings = false;
    app.update_dock_motion(1440.0, start + Duration::from_millis(100));
    assert_eq!(crate::view::terminal_panel_height(&app.view, 900.0), target);
    assert!(app.view.bottom_presentation_height.is_none());
}

#[test]
fn agents_problems_bottom_animates_without_a_terminal_and_can_reverse_midway() {
    let (_root, mut app) = fixture();
    app.view.ide.mode = WindowMode::Agents;
    app.view.show_terminal_panel = false;
    app.view.ide.bottom_open = false;
    app.view.ide.bottom_panel = IdeBottomPanel::Problems;
    app.view.terminal_tabs.clear();
    let start = Instant::now();
    app.update_dock_motion(1440.0, start);
    assert_eq!(crate::view::terminal_panel_height(&app.view, 900.0), 0.0);
    app.view.ide.bottom_open = true;
    app.update_dock_motion(1440.0, start);
    let target = crate::view::nominal_terminal_panel_height(&app.view, 900.0);
    app.update_dock_motion(1440.0, start + Duration::from_millis(80));
    let midway = crate::view::terminal_panel_height(&app.view, 900.0);
    assert!(midway > 0.0 && midway < target);
    app.view.ide.bottom_open = false;
    app.update_dock_motion(1440.0, start + Duration::from_millis(80));
    assert_eq!(crate::view::terminal_panel_height(&app.view, 900.0), midway);
    app.update_dock_motion(1440.0, start + Duration::from_millis(300));
    assert_eq!(crate::view::terminal_panel_height(&app.view, 900.0), 0.0);
    assert!(app.terminals.is_empty());
    assert!(!app.view.show_terminal_panel);
}

#[test]
#[ignore = "exports native IDE dock motion keyframes with the shared OpenIDE mode control"]
fn export_ide_dock_motion_visual_review() {
    let (_root, mut app) = fixture();
    let size = Size::new(1440.0, 900.0);
    let directory = Path::new("/tmp/openade-dock-motion-review");
    std::fs::create_dir_all(directory).unwrap();
    let start = Instant::now();
    app.update_dock_motion(size.width, start);
    set_panels(&mut app, false);
    for millis in [0, 80, 180, 220] {
        let at = start + Duration::from_millis(millis);
        app.update_dock_motion(size.width, at);
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
            crate::view::build_window_view(&app.view, size.width, size.height),
        );
        let frame = tree.layout_at(size, at);
        let path = directory.join(format!("ide-close-{millis}.svg"));
        crate::settings_ui::visual_qa::write_native_frame_svg(
            &frame,
            &path,
            size.width,
            size.height,
        )
        .unwrap();
        let svg = resvg::usvg::Tree::from_str(
            &std::fs::read_to_string(&path).unwrap(),
            &crate::settings_ui::visual_qa::native_svg_options(),
        )
        .unwrap();
        let mut pixmap =
            resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
        resvg::render(
            &svg,
            resvg::tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        pixmap.save_png(path.with_extension("png")).unwrap();
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn task_list_opens_beside_the_conversation_with_its_own_transition() {
    let (_root, mut app) = fixture();
    let start = Instant::now();
    app.update_dock_motion(1440.0, start);
    let closed = dock_layout(&app.view, 1440.0);
    app.view.ide.agent_list_open = true;
    assert!(app.update_dock_motion(1440.0, start));
    assert_eq!(dock_layout(&app.view, 1440.0).right, closed.right);
    let midway = start + Duration::from_millis(100);
    assert!(app.update_dock_motion(1440.0, midway));
    let partial = crate::ide_workspace::agent_list_width(&app.view, 1440.0);
    assert!(partial > 0.0 && partial < crate::ide_workspace::AGENT_LIST_WIDTH);
    assert!(!app.update_dock_motion(1440.0, start + Duration::from_millis(220)));
    let open = dock_layout(&app.view, 1440.0);
    assert_eq!(
        open.right.unwrap(),
        closed.right.unwrap() + crate::ide_workspace::AGENT_LIST_WIDTH + 1.0
    );
    assert!(app.view.ide.agent_list_progress.is_none());
}
