//! Opt-in, font-accurate debugger review. Fixtures are presentation snapshots:
//! exporting them never launches an adapter, program, terminal or provider.
use super::*;
use openide_ui::{Frame, HitKind, Point, Size, UiAction, UiEvent};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// Kept in the repository so a change to the panel is reviewed against them.
const DIRECTORY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ui-separation/shots/debug-panel"
);

fn variable(key: &str, name: &str, value: &str, kind: &str, depth: usize) -> DebugVariableView {
    DebugVariableView {
        key: key.into(),
        name: name.into(),
        value: value.into(),
        type_name: (!kind.is_empty()).then(|| kind.into()),
        depth,
        ..Default::default()
    }
}

fn mark(path: &str, line: u32) -> DebugBreakpointView {
    DebugBreakpointView {
        path: path.into(),
        line,
        enabled: true,
        ..Default::default()
    }
}

fn fixture(status: DebugStatus) -> DebugView {
    let configured = status != DebugStatus::Unconfigured;
    let stopped = status == DebugStatus::Stopped;
    let active = matches!(
        status,
        DebugStatus::Starting | DebugStatus::Running | DebugStatus::Stopped
    );
    let frames = [
        ("agents_window::workspace::Workspace::open_file", "workspace.rs", 18),
        ("agents_window::workspace::Workspace::restore_tabs", "workspace.rs", 244),
        ("agents_window::app::AgentsWindow::open_workspace", "app.rs", 5120),
        ("agents_window::app::AgentsWindow::update", "app.rs", 15287),
        ("<agents_window::app::AgentsWindow as eframe::App>::update", "app.rs", 15011),
        ("eframe::native::epi_integration::EpiIntegration::update", "", 0),
        ("eframe::native::glow_integration::GlowWinitRunning::run_ui_and_paint", "", 0),
        ("winit::event_loop::EventLoop::run_app", "", 0),
        ("agents_window::main", "main.rs", 42),
    ];
    DebugView {
        scope: "debug-visual:workspace:session-7".into(),
        status,
        configuration: if configured { "Rust · aplicación local".into() } else { String::new() },
        configurations: if configured {
            vec!["Rust · aplicación local".into(), "Pruebas de integración".into()].into()
        } else {
            Arc::from([])
        },
        adapter: configured.then(|| "lldb-dap".into()),
        message: match status {
            DebugStatus::Failed => Some("Cannot start debug adapter: No such file or directory (os error 2)".into()),
            DebugStatus::Ended => Some("Programa finalizado · código 101".into()),
            _ => None,
        },
        stop_reason: stopped.then(|| "punto de interrupción".into()),
        controls: DebugControls {
            start: matches!(status, DebugStatus::Idle | DebugStatus::Ended | DebugStatus::Failed),
            resume: stopped,
            pause: status == DebugStatus::Running,
            step_over: stopped,
            step_into: stopped,
            step_out: stopped,
            restart: active,
            stop: active,
        },
        threads: if stopped {
            vec![
                DebugThreadView { id: 1, name: "main".into(), selected: true, stopped: true },
                DebugThreadView { id: 2, name: "tokio-runtime-worker".into(), selected: false, stopped: true },
            ]
            .into()
        } else {
            Arc::from([])
        },
        frames: if stopped {
            frames
                .iter()
                .enumerate()
                .map(|(index, (name, file, line))| DebugFrameView {
                    id: 17 + index as i64,
                    name: (*name).into(),
                    path: (!file.is_empty()).then(|| format!("apps/agents-window/src/{file}")),
                    line: (*line > 0).then_some(*line),
                    selected: index == 0,
                })
                .collect::<Vec<_>>()
                .into()
        } else {
            Arc::from([])
        },
        variables: if stopped {
            vec![
                DebugVariableView { has_children: true, expanded: true, ..variable("scope:0", "Locales", "", "", 0) },
                variable("scope:0/0", "path", "\"docs/ui-separation/README.md\"", "&str", 1),
                DebugVariableView { changed: true, ..variable("scope:0/1", "opened", "3", "usize", 1) },
                DebugVariableView { has_children: true, expanded: true, ..variable("scope:0/2", "files", "Vec(2)", "Vec<String>", 1) },
                variable("scope:0/2/0", "[0]", "\"README.md\"", "String", 2),
                DebugVariableView { has_children: true, ..variable("scope:0/2/1", "[1]", "\"Cargo.toml\"", "String", 2) },
                DebugVariableView { has_children: true, expanded: true, ..variable("scope:0/3", "self", "Workspace", "&mut Workspace", 1) },
                DebugVariableView { has_children: true, loading: true, ..variable("scope:0/3/0", "tabs", "", "Vec<Tab>", 2) },
                DebugVariableView { error: Some("variables: optimized out".into()), ..variable("scope:0/3/1", "root", "", "PathBuf", 2) },
                variable("scope:0/3/2", "is_open", "false", "bool", 2),
                DebugVariableView { has_children: true, ..variable("scope:1", "Estáticas", "", "", 0) },
                DebugVariableView { has_children: true, ..variable("scope:2", "Registros", "", "", 0) },
            ]
            .into()
        } else {
            Arc::from([])
        },
        breakpoints: if configured {
            vec![
                DebugBreakpointView { verified: active.then_some(true), ..mark("apps/agents-window/src/workspace.rs", 18) },
                DebugBreakpointView { verified: active.then_some(false), message: active.then(|| "No hay código en esta línea".into()), ..mark("apps/agents-window/src/workspace.rs", 97) },
                DebugBreakpointView { enabled: false, ..mark("apps/agents-window/src/app.rs", 5120) },
                DebugBreakpointView { dirty: true, ..mark("apps/agents-window/src/main.rs", 42) },
            ]
            .into()
        } else {
            Arc::from([])
        },
        output: vec![DebugOutputView {
            category: "console".into(),
            text: "Programa detenido en workspace.rs:18\nInspeccioná las variables o continuá con F5.\n".into(),
        }]
        .into(),
        ..Default::default()
    }
}

fn export(frame: &Frame, size: Size, name: &str) {
    let directory = Path::new(DIRECTORY);
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}-{:.0}.svg", size.width));
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, size.width, size.height)
        .unwrap();
    let svg = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixels = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
    resvg::render(
        &svg,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    pixels.save_png(path.with_extension("png")).unwrap();
    // The PNG is what is reviewed; the SVG is only how it is drawn.
    std::fs::remove_file(path).unwrap();
}

#[test]
#[ignore = "exports the debug sidebar's states to docs/ui-separation/shots/debug-panel"]
fn export_debug_sidebar_visual_review() {
    for (name, status) in [
        ("no-configuration", DebugStatus::Unconfigured),
        ("idle", DebugStatus::Idle),
        ("launching", DebugStatus::Starting),
        ("running", DebugStatus::Running),
        ("paused", DebugStatus::Stopped),
        ("error", DebugStatus::Failed),
        ("terminated", DebugStatus::Ended),
    ] {
        let state = fixture(status);
        for width in [240.0, 280.0, 420.0, 480.0] {
            let size = Size::new(width, 720.0);
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(build_sidebar(
                &state,
                size.width,
                size.height,
            ));
            let now = Instant::now();
            let frame = tree.layout_at(size, now);
            for region in frame
                .regions
                .iter()
                .filter(|region| region.kind == HitKind::Button)
            {
                assert!(
                    region.rect.x >= 0.0 && region.rect.x + region.rect.width <= size.width,
                    "{} stays inside the sidebar at {width}",
                    region.id
                );
            }
            if width == 280.0 || width == 420.0 {
                export(&frame, size, name);
            }
            if status == DebugStatus::Stopped && width == 420.0 {
                let id = action(&state.scope, "step_over");
                let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                let point = Point::new(
                    region.rect.x + region.rect.width / 2.0,
                    region.rect.y + region.rect.height / 2.0,
                );
                tree.handle_event_at(UiEvent::PointerMove(point), now);
                let hovered = tree.layout_at(size, now + Duration::from_millis(1100));
                export(&hovered, size, "paused-step-tooltip");
                tree.handle_event_at(
                    UiEvent::PointerDown(point),
                    now + Duration::from_millis(1150),
                );
                assert_eq!(
                    tree.handle_event_at(
                        UiEvent::PointerUp(point),
                        now + Duration::from_millis(1200)
                    ),
                    vec![UiAction::Clicked { id }]
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }
    // The configuration list, and the breakpoints alone with their sections folded.
    let mut menu = fixture(DebugStatus::Idle);
    menu.configuration_menu = true;
    let mut folded = fixture(DebugStatus::Stopped);
    folded.sections.variables = false;
    folded.sections.stack = false;
    for (name, state) in [("idle-configurations", menu), ("breakpoints", folded)] {
        for width in [280.0, 420.0] {
            let size = Size::new(width, 720.0);
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(build_sidebar(
                &state,
                size.width,
                size.height,
            ));
            export(&tree.layout(size), size, name);
            context.end_pass().textures_delta.clear();
        }
    }
    for (name, width, height, sidebar_width, confirm) in [
        ("workspace-paused", 1440.0, 900.0, 320.0, false),
        ("workspace-remove-all", 1440.0, 900.0, 320.0, true),
        ("workspace-compact", 1040.0, 720.0, 240.0, false),
    ] {
        let mut state = crate::view::ide_visual_tests::fixture();
        state.ide.activity = crate::ide_workspace::IdeActivity::Debug;
        state.ide.explorer_width = sidebar_width;
        state.ide.bottom_panel = crate::ide_workspace::IdeBottomPanel::DebugConsole;
        state.ide.bottom_open = true;
        state.show_terminal_panel = false;
        state.debug = fixture(DebugStatus::Stopped);
        state.debug.confirm_clear_breakpoints = confirm;
        let size = Size::new(width, height);
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
            crate::view::build_window_view(&state, width, height),
        );
        export(&tree.layout(size), size, name);
        context.end_pass().textures_delta.clear();
    }
}
