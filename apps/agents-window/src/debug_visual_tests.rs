//! Opt-in, font-accurate debugger review. Fixtures are presentation snapshots:
//! exporting them never launches an adapter, program, terminal or provider.
use super::*;
use openide_ui::{Frame, HitKind, Point, Size, UiAction, UiEvent};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const DIRECTORY: &str = "/tmp/openade-debug-sidebar-review";

fn fixture(status: DebugStatus) -> DebugView {
    let configured = status != DebugStatus::Unconfigured;
    let stopped = status == DebugStatus::Stopped;
    DebugView {
        scope: "debug-visual:workspace:session-7".into(),
        status,
        configuration: if configured { "Rust · aplicación local".into() } else { String::new() },
        configurations: if configured { vec!["Rust · aplicación local".into(), "Pruebas de integración".into()].into() } else { Arc::from([]) },
        adapter: configured.then(|| "CodeLLDB".into()),
        message: (status == DebugStatus::Failed).then(|| "No se encontró el programa configurado. Revisá su ruta en .openide/debug.json y volvé a iniciar. Podés seguir trabajando en el editor mientras corregís la configuración.".into()),
        controls: DebugControls {
            start: matches!(status, DebugStatus::Idle | DebugStatus::Ended | DebugStatus::Failed),
            resume: stopped,
            pause: status == DebugStatus::Running,
            step_over: stopped,
            step_into: stopped,
            step_out: stopped,
            stop: matches!(status, DebugStatus::Starting | DebugStatus::Running | DebugStatus::Stopped),
        },
        frames: if stopped { vec![
            DebugFrameView { id: 17, name: "Workspace::open_file".into(), path: Some("apps/agents-window/src/workspace.rs".into()), line: Some(18), selected: true },
            DebugFrameView { id: 18, name: "main".into(), path: Some("apps/agents-window/src/main.rs".into()), line: Some(42), selected: false },
        ].into() } else { Arc::from([]) },
        variables: if stopped { vec![
            DebugVariableView { key: "locals".into(), name: "Locales".into(), value: "3 variables".into(), type_name: None, depth: 0, has_children: true, expanded: true, loading: false, error: None },
            DebugVariableView { key: "locals:path".into(), name: "path".into(), value: "README.md".into(), type_name: Some("&str".into()), depth: 1, has_children: false, expanded: false, loading: false, error: None },
            DebugVariableView { key: "locals:files".into(), name: "files".into(), value: "Vec(2)".into(), type_name: Some("Vec<String>".into()), depth: 1, has_children: true, expanded: false, loading: false, error: None },
            DebugVariableView { key: "locals:is_open".into(), name: "is_open".into(), value: "false".into(), type_name: Some("bool".into()), depth: 1, has_children: false, expanded: false, loading: false, error: None },
        ].into() } else { Arc::from([]) },
        breakpoints: if configured { vec![
            DebugBreakpointView { path: "apps/agents-window/src/workspace.rs".into(), line: 18, dirty: false },
            DebugBreakpointView { path: "apps/agents-window/src/main.rs".into(), line: 42, dirty: true },
        ].into() } else { Arc::from([]) },
        output: vec![DebugOutputView { category: "console".into(), text: "Programa detenido en workspace.rs:18\nInspeccioná las variables o continuá con F5.\n".into() }].into(),
        ..Default::default()
    }
}

fn export(frame: &Frame, size: Size, name: &str) {
    let directory = Path::new(DIRECTORY);
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}-{:.0}x{:.0}.svg", size.width, size.height));
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
}

#[test]
#[ignore = "exports real native debugger states to /tmp/openade-debug-sidebar-review"]
fn export_debug_sidebar_visual_review() {
    for (name, status) in [
        ("setup", DebugStatus::Unconfigured),
        ("ready", DebugStatus::Idle),
        ("running", DebugStatus::Running),
        ("paused", DebugStatus::Stopped),
        ("error", DebugStatus::Failed),
    ] {
        let state = fixture(status);
        for width in [240.0, 320.0, 420.0] {
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
            export(&frame, size, name);
            if status == DebugStatus::Stopped && width == 320.0 {
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
            if status == DebugStatus::Failed {
                tree.scroll_to_end(&format!("debug_sidebar_message:{}", state.scope));
                let frame = tree.layout_at(size, now + Duration::from_millis(1300));
                export(&frame, size, "error-detail-end");
            }
            context.end_pass().textures_delta.clear();
        }
    }
    for (name, width, height, sidebar_width) in [
        ("workspace", 1440.0, 900.0, 320.0),
        ("workspace-compact", 1040.0, 720.0, 240.0),
    ] {
        let mut state = crate::view::ide_visual_tests::fixture();
        state.ide.activity = crate::ide_workspace::IdeActivity::Debug;
        state.ide.explorer_width = sidebar_width;
        state.ide.bottom_panel = crate::ide_workspace::IdeBottomPanel::DebugConsole;
        state.ide.bottom_open = true;
        state.show_terminal_panel = false;
        state.debug = fixture(DebugStatus::Stopped);
        let size = Size::new(width, height);
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
            crate::view::build_window_view(&state, width, height),
        );
        export(&tree.layout(size), size, name);
        context.end_pass().textures_delta.clear();
    }
}
