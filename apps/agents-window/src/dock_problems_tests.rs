//! The lower panel must reclaim space from the workspace, not insert a tool
//! result between the document breadcrumb and its first line.
use super::*;
use crate::ide_workspace::{IdeBottomPanel, WindowMode};
use openide_ui::{Frame, Rect};

fn fixture() -> ViewState {
    let mut state = crate::view::ide_visual_tests::fixture();
    state.ide.mode = WindowMode::Agents;
    state.sidebar_collapsed = true;
    state.show_side_pane = true;
    state.right_dock_width = Some(520.0);
    state.show_terminal_panel = false;
    state.ide.bottom_panel = IdeBottomPanel::Problems;
    state.ide.bottom_open = false;
    state.terminal_tabs.push(DockTabView {
        id: 82,
        title: "fish".into(),
        full_path: Some(state.workspace.clone()),
        active: false,
        kind: SideTab::Terminal,
        closable: true,
    });
    state
}

fn region(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
        .rect
}

#[test]
fn agents_problems_dock_preserves_editor_origin_and_reserves_only_the_bottom_panel() {
    for width in [1100.0, 1440.0] {
        let mut state = fixture();
        let size = Size::new(width, 800.0);
        let (ctx, mut tree) = crate::view::ide_visual_tests::native_tree(build_window_view(
            &state,
            size.width,
            size.height,
        ));
        let closed = tree.layout(size);
        let editor = region(&closed, "code_editor:71");
        state.ide.bottom_open = true;
        tree.set_root(build_window_view(&state, size.width, size.height));
        let opened = tree.layout(size);
        let next_editor = region(&opened, "code_editor:71");
        let footer = region(&opened, "ide_editor_diagnostics");
        let problems = region(&opened, "ide_problems");
        assert_eq!(editor.y, next_editor.y, "no diagnostics strip above code");
        assert!(
            (editor.height - next_editor.height - terminal_panel_height(&state, size.height)).abs()
                < 1.0
        );
        assert!(problems.y > footer.y + footer.height);
        assert!(next_editor.y + next_editor.height <= footer.y);
        assert!(
            !opened
                .regions
                .iter()
                .any(|r| r.id.starts_with("terminal_screen"))
        );
        assert!(opened.regions.iter().any(|r| r.id == "ide_bottom:terminal"));
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
fn agents_shared_bottom_header_retains_its_sessions_and_all_terminal_actions() {
    for width in [320.0, 700.0, 1100.0] {
        let mut state = fixture();
        state.show_terminal_panel = true;
        state.ide.bottom_panel = IdeBottomPanel::Terminal;
        let (ctx, mut tree) =
            crate::view::ide_visual_tests::native_tree(ide_ui::bottom::build(&state, width, 240.0));
        let frame = tree.layout(Size::new(width, 240.0));
        for id in [
            "ide_bottom:problems",
            "terminal_sessions_toggle",
            "terminal_new",
            "terminal_panel_hide",
        ] {
            let rect = region(&frame, id);
            assert!(
                rect.width > 0.0 && rect.x >= 0.0 && rect.x + rect.width <= width + 0.01,
                "{width}: {id}: {rect:?}"
            );
            assert!(rect.y + rect.height <= crate::dock_tab_ui::HEADER_HEIGHT);
        }
        // The sessions are flat tabs where they fit, and a named list otherwise.
        assert_eq!(
            frame.regions.iter().any(|r| r.id.starts_with("terminal_tab:")),
            width >= 700.0
        );
        assert!(!frame.regions.iter().any(|r| r.id.starts_with("terminal_tab_extent:")));
        assert!(region(&frame, "terminal_screen").y >= crate::dock_tab_ui::HEADER_HEIGHT);
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
#[ignore = "native Agents workspace with shared Problems dock"]
fn export_agents_problems_workspace_visual_review() {
    let mut state = fixture();
    state.sidebar_collapsed = false;
    state.ide.bottom_open = true;
    export(
        build_window_view(&state, 1440.0, 900.0),
        1440.0,
        900.0,
        "agents-problems-workspace",
    );
    state.ide.bottom_open = false;
    state.show_terminal_panel = true;
    state.ide.bottom_panel = IdeBottomPanel::Terminal;
    export(
        build_window_view(&state, 1440.0, 900.0),
        1440.0,
        900.0,
        "agents-terminal-workspace",
    );
}
