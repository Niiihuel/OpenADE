//! Passive plugin-management fixtures. Packages and processes are never accessed.

use super::*;
use eframe::egui;
use openide_ui::{DrawCommand, Frame, HitKind, Key, Point, Rect, Size, UiAction, UiEvent, UiTree};
use std::{fmt::Write as _, fs, path::Path};

fn plugin(id: &str, enabled: bool) -> InstalledPlugin {
    InstalledPlugin {
        manifest: PluginManifest {
            schema_version: 1,
            id: id.into(),
            name: if id == "echo" { "Echo local" } else { "Documentación del proyecto" }.into(),
            version: "1.2.0".into(),
            description: "Consulta información del proyecto mediante herramientas locales. La activación se aplica al proyecto actual y conserva los permisos de cada llamada.".into(),
            publisher: if id == "echo" { "OpenIDE fixtures" } else { "Equipo de documentación" }.into(),
            program: "python3".into(),
            entrypoint: Some("src/server.py".into()),
            args: vec!["--workspace-mode".into(), "read-only".into()],
            credential_env_names: vec!["DOCUMENTATION_API_KEY".into()],
        },
        digest: "a1b2c3d4".repeat(8),
        enabled,
        path: format!("/datos/openide/plugins/packages/{id}/version-1.2.0").into(),
    }
}

fn state(installed: bool) -> ViewState {
    let mut state = crate::view::tests::empty_state();
    state.show_settings = true;
    state.settings_page = SettingsPage::Plugins;
    state.workspace = "/workspace/openADE".into();
    state.workspace_draft = state.workspace.clone();
    state.plugins.workspace = state.workspace.clone();
    if installed {
        state.plugins.installed = vec![plugin("echo", false), plugin("docs", true)];
    }
    state
}

fn preview() -> PluginPreview {
    let installed = plugin("echo", false);
    PluginPreview {
        manifest: installed.manifest,
        digest: installed.digest,
        source: "/workspace/plugins/echo-local".into(),
        file_count: 7,
        total_bytes: 18_432,
    }
}

fn scene(state: &ViewState, viewport: Size) -> Element {
    let sidebar = (viewport.width >= 1000.0).then_some(244.0);
    let page = crate::settings_ui::build_settings_page(
        state,
        SettingsPage::Plugins,
        viewport.width - sidebar.unwrap_or(0.0),
        sidebar.is_some(),
    );
    let base = if let Some(width) = sidebar {
        Element::row(vec![
            crate::settings_ui::build_settings_sidebar(state, width),
            page,
        ])
    } else {
        page
    }
    .background(theme::BACKGROUND)
    .size(Some(viewport.width), Some(viewport.height));
    modal(state, base, viewport.width)
}

struct NativeFixture {
    context: egui::Context,
    tree: UiTree,
}

impl NativeFixture {
    fn new(state: &ViewState, viewport: Size) -> Self {
        let (context, mut tree) =
            crate::view::ide_visual_tests::native_tree(scene(state, viewport));
        tree.set_reduced_motion(true);
        Self { context, tree }
    }
}

impl Drop for NativeFixture {
    fn drop(&mut self) {
        // These fixtures inspect draw commands, without uploading a GPU atlas.
        self.context.end_pass().textures_delta.clear();
    }
}

fn text(frame: &Frame) -> String {
    frame
        .text_layouts()
        .iter()
        .map(|layout| layout.source.clone())
        .chain(frame.commands.iter().filter_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.clone()),
            _ => None,
        }))
        .collect::<Vec<_>>()
        .join(" ")
}

fn button_rect(frame: &Frame, id: &str, viewport: Size) -> Rect {
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing reachable button {id} at {viewport:?}"))
        .rect;
    assert!(
        rect.x >= 0.0
            && rect.y >= 0.0
            && rect.x + rect.width <= viewport.width + 0.5
            && rect.y + rect.height <= viewport.height + 0.5,
        "{id} outside viewport {viewport:?}: {rect:?}"
    );
    assert!(
        rect.width > 10.0 && rect.height >= 24.0,
        "clipped button {id}: {rect:?}"
    );
    assert_eq!(
        frame.hit_test(Point::new(
            rect.x + rect.width / 2.0,
            rect.y + rect.height / 2.0
        )),
        Some(id)
    );
    rect
}

fn click(tree: &mut UiTree, rect: Rect) -> Vec<UiAction> {
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    tree.handle_event(UiEvent::PointerDown(point));
    tree.handle_event(UiEvent::PointerUp(point))
}

#[test]
fn plugins_page_is_bounded_and_actions_are_reachable_at_compact_and_desktop_widths() {
    for width in [360.0, 620.0, 1300.0] {
        let viewport = Size::new(width, 840.0);
        for installed in [false, true] {
            let state = state(installed);
            let mut fixture = NativeFixture::new(&state, viewport);
            let frame = fixture.tree.layout(viewport);
            for id in ["plugins_install_open", "plugins_refresh"] {
                let rect = button_rect(&frame, id, viewport);
                assert!(rect.height <= 28.5, "oversized page action {id}: {rect:?}");
                assert_eq!(
                    click(&mut fixture.tree, rect),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
            if installed {
                assert!(text(&frame).contains("Activo en este proyecto"));
                assert!(text(&frame).contains("Inactivo en este proyecto"));
                for id in [
                    "plugins_details:echo",
                    "plugins_toggle:echo",
                    "plugins_details:docs",
                    "plugins_toggle:docs",
                ] {
                    button_rect(&frame, id, viewport);
                }
            } else {
                assert!(text(&frame).contains("Todavía no hay plugins instalados"));
                assert!(!frame
                    .regions
                    .iter()
                    .any(|region| region.id.starts_with("plugins_toggle:")));
            }
        }
    }
}

#[test]
fn plugins_filter_matches_metadata_and_disabled_actions_never_dispatch() {
    for width in [360.0, 620.0, 1300.0] {
        let viewport = Size::new(width, 840.0);
        let mut state = state(true);
        for query in ["echo", "OPENIDE FIXTURES", "   Echo local   "] {
            state.plugins.query = query.into();
            let mut fixture = NativeFixture::new(&state, viewport);
            let frame = fixture.tree.layout(viewport);
            button_rect(&frame, "plugins_details:echo", viewport);
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "plugins_details:docs"));
        }
        state.plugins.query = "sin resultados 123".into();
        let mut fixture = NativeFixture::new(&state, viewport);
        let frame = fixture.tree.layout(viewport);
        assert!(text(&frame).contains("Sin coincidencias"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("plugins_details:")));
        drop(fixture);

        state.plugins.query.clear();
        state.plugins.load_error = true;
        let mut fixture = NativeFixture::new(&state, viewport);
        let frame = fixture.tree.layout(viewport);
        button_rect(&frame, "plugins_refresh", viewport);
        button_rect(&frame, "plugins_details:echo", viewport);
        assert!(!fixture.tree.focus("plugins_toggle:echo"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "plugins_toggle:echo" && region.kind == HitKind::Button));
        drop(fixture);

        state.plugins.busy = true;
        let mut fixture = NativeFixture::new(&state, viewport);
        fixture.tree.layout(viewport);
        for id in [
            "plugins_install_open",
            "plugins_refresh",
            "plugins_details:echo",
            "plugins_toggle:echo",
        ] {
            assert!(
                !fixture.tree.focus(id),
                "busy control still focusable: {id}"
            );
        }
    }
}

#[test]
fn plugin_modals_keep_close_and_footer_visible_while_long_body_scrolls() {
    for viewport in [
        Size::new(360.0, 420.0),
        Size::new(620.0, 620.0),
        Size::new(1300.0, 840.0),
    ] {
        for mode in ["install", "details", "remove"] {
            let mut state = state(true);
            let (close, footer, initial) = match mode {
                "install" => {
                    state.plugins.install_open = true;
                    state.plugins.source = preview().source.display().to_string();
                    state.plugins.preview = Some(preview());
                    (
                        "plugins_dialog_close",
                        "plugins_install_confirm",
                        "plugins_source",
                    )
                }
                "details" => {
                    state.plugins.selected = Some("echo".into());
                    (
                        "plugins_dialog_close",
                        "plugins_remove:echo",
                        "plugins_dialog_close",
                    )
                }
                _ => {
                    state.plugins.pending_remove = Some(plugin("echo", false));
                    (
                        "plugins_remove_cancel",
                        "plugins_remove_confirm",
                        "plugins_remove_cancel",
                    )
                }
            };
            let mut fixture = NativeFixture::new(&state, viewport);
            let frame = fixture.tree.layout(viewport);
            assert_eq!(fixture.tree.focused_id(), Some(initial), "{mode}");
            let x_id = format!("{close}:close");
            let close_rect = button_rect(&frame, &x_id, viewport);
            let footer_rect = button_rect(&frame, footer, viewport);
            button_rect(&frame, close, viewport);
            assert!(
                close_rect.y + close_rect.height < footer_rect.y,
                "{mode}: close and footer overlap"
            );
            // focus(id) is an explicit host API and can target the base tree;
            // users navigate through the modal's trapped keyboard scope.
            for _ in 0..10 {
                fixture.tree.handle_event(UiEvent::Key {
                    key: Key::Tab,
                    shift: false,
                    ctrl: false,
                });
                let focused = fixture.tree.focused_id().expect("modal focus target");
                assert!(
                    focused == x_id
                        || focused == close
                        || focused == footer
                        || (mode == "install"
                            && matches!(
                                focused,
                                "plugins_source" | "plugins_pick_folder" | "plugins_inspect"
                            )),
                    "{mode}: keyboard focus escaped to {focused}"
                );
            }
            fixture
                .tree
                .handle_event(UiEvent::PointerDown(Point::new(2.0, 2.0)));
            assert!(fixture
                .tree
                .handle_event(UiEvent::PointerUp(Point::new(2.0, 2.0)))
                .is_empty());
            let body_id = format!("modal_body:{close}");
            fixture.tree.scroll_to_end(&body_id);
            let scrolled = fixture.tree.layout(viewport);
            assert_eq!(button_rect(&scrolled, &x_id, viewport), close_rect);
            assert_eq!(button_rect(&scrolled, footer, viewport), footer_rect);
            if mode == "install" && viewport.height <= 420.0 {
                assert!(text(&scrolled).contains("Se instalará inactivo"));
            }
            assert_eq!(
                click(&mut fixture.tree, footer_rect),
                vec![UiAction::Clicked { id: footer.into() }]
            );
            assert_eq!(
                click(&mut fixture.tree, close_rect),
                vec![UiAction::Clicked { id: close.into() }]
            );
            assert_eq!(
                fixture.tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked { id: close.into() }]
            );
        }
    }
}

#[test]
fn plugin_install_requires_review_and_active_work_blocks_only_destructive_removal() {
    let viewport = Size::new(360.0, 620.0);
    let mut state = state(true);
    state.plugins.install_open = true;
    let mut fixture = NativeFixture::new(&state, viewport);
    fixture.tree.layout(viewport);
    assert!(!fixture.tree.focus("plugins_install_confirm"));
    assert!(!fixture.tree.focus("plugins_inspect"));
    assert!(fixture.tree.focus("plugins_pick_folder"));
    drop(fixture);

    state.plugins.close_dialogs();
    state.plugins.selected = Some("echo".into());
    state.plugins.removal_blocked = true;
    let mut fixture = NativeFixture::new(&state, viewport);
    let frame = fixture.tree.layout(viewport);
    assert!(!fixture.tree.focus("plugins_remove:echo"));
    button_rect(&frame, "plugins_dialog_close", viewport);
    drop(fixture);

    state.plugins.pending_remove = Some(plugin("echo", false));
    let mut fixture = NativeFixture::new(&state, viewport);
    let frame = fixture.tree.layout(viewport);
    assert!(!fixture.tree.focus("plugins_remove_confirm"));
    button_rect(&frame, "plugins_remove_cancel", viewport);
    button_rect(&frame, "plugins_remove_cancel:close", viewport);
}

#[test]
#[ignore = "export passive plugin-management states with native font metrics"]
fn export_plugins_visual_review() {
    let output = Path::new("/tmp/openide-plugins-review");
    fs::create_dir_all(output).unwrap();
    let options = crate::settings_ui::visual_qa::native_svg_options();
    let mut index = String::from("<!doctype html><meta charset=\"utf-8\"><title>Plugins review</title><style>body{font:14px system-ui;background:#171717;color:#ddd;padding:24px}a{color:#a8d4ff}li{margin:8px}</style><h1>Plugins · native font layout review</h1><p>Synthetic data; no package reads or execution. SVG/PNG layout exports, not host pixel screenshots.</p><ul>");
    for width in [360.0, 620.0, 1300.0] {
        let viewport = Size::new(width, 840.0);
        for variant in [
            "empty",
            "installed",
            "filtered",
            "no-results",
            "install-preview",
            "details",
            "remove",
            "removal-blocked",
        ] {
            let mut state = state(variant != "empty");
            match variant {
                "filtered" => state.plugins.query = "documentación".into(),
                "no-results" => state.plugins.query = "sin resultados 123".into(),
                "install-preview" => {
                    state.plugins.install_open = true;
                    state.plugins.source = preview().source.display().to_string();
                    state.plugins.preview = Some(preview());
                }
                "details" | "removal-blocked" => {
                    state.plugins.selected = Some("echo".into());
                    state.plugins.removal_blocked = variant == "removal-blocked";
                }
                "remove" => state.plugins.pending_remove = Some(plugin("echo", false)),
                _ => {}
            }
            let mut fixture = NativeFixture::new(&state, viewport);
            let frame = fixture.tree.layout(viewport);
            let name = format!("plugins-{variant}-{width:.0}");
            let path = output.join(format!("{name}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(
                &frame,
                &path,
                width,
                viewport.height,
            )
            .unwrap();
            let parsed =
                resvg::usvg::Tree::from_str(&fs::read_to_string(&path).unwrap(), &options).unwrap();
            let mut pixmap =
                resvg::tiny_skia::Pixmap::new(width as u32, viewport.height as u32).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            writeln!(
                index,
                "<li><a href=\"{name}.png\">{name}</a> · <a href=\"{name}.svg\">SVG</a></li>"
            )
            .unwrap();
        }
    }
    index.push_str("</ul>");
    fs::write(output.join("index.html"), index).unwrap();
    eprintln!("{}", output.join("index.html").display());
}
