//! Opt-in visual fixtures painted by the native UI with the application's fonts.
use super::*;
use crate::code_editor_ui::CodeEditorView;
use crate::editor_workbench_ui::EditorWorkbenchView;
use crate::ide_workspace::{IdeActivity, IdeBottomPanel, IdePanel, WindowMode};
use crate::terminal::{TerminalColor, TerminalSpan, TerminalStyle};
use eframe::egui;
use openide_editor::TextBuffer;
use openide_language::{Cancellation, Language, ParserEngine};
use openide_ui::{FontMetrics, Frame, Point, Size, UiEvent, UiTree};

const SOURCE: &str = "use std::path::Path;\n\n/// Los recursos pertenecen al proyecto abierto.\npub struct Workspace {\n    root: String,\n    files: Vec<String>,\n}\n\nimpl Workspace {\n    pub fn new(root: &Path) -> Self {\n        Self {\n            root: root.display().to_string(),\n            files: Vec::new(),\n        }\n    }\n\n    pub fn open_file(&mut self, path: &str) {\n        if !self.files.iter().any(|file| file == path) {\n            self.files.push(path.to_owned());\n        }\n    }\n\n    pub fn project_name(&self) -> &str {\n        &self.root\n    }\n}\n";

pub(crate) fn fixture() -> ViewState {
    let mut state = tests::empty_state();
    state.ide.mode = WindowMode::Ide;
    state.workspace = "/workspace/openADE".into();
    state.workspace_draft = state.workspace.clone();
    state.project_branch = Some("feature/ide-workspace".into());
    let route = crate::composer_ui::tests::configured_effort_route();
    state.model = route.model_id.clone();
    state.composer_model = route.model_id.clone();
    state.native_provider_id = route.provider_id.clone();
    state.composer_model_options = vec![(route.model_id.clone(), route.model_name.clone())];
    state.composer_effort_options = route.effort_options.clone();
    state.composer_effort = "max".into();
    state.composer_routes = vec![route];
    state.prompt = "Agregá un test para abrir el mismo archivo dos veces.".into();
    state.selected_session = Some("ide-review".into());
    state.selected_session_title = Some("Revisar el workspace".into());
    state.sessions = vec![SessionView {
        id: "ide-review".into(),
        title: "Revisar el workspace".into(),
        project: state.workspace.clone(),
        backend: "native".into(),
        pinned: false,
        archived: false,
        unread: false,
        native_automation: None,
    }];
    state.transcript = vec![
        TranscriptItem::User {
            content: "Revisá cómo el workspace conserva los archivos abiertos.".into(),
            images: Vec::new(),
        },
        TranscriptItem::Assistant {
            content: "`Workspace` mantiene los archivos del proyecto. `open_file` evita duplicados antes de guardar una nueva ruta.\n\nPodemos cubrir ese comportamiento con un test pequeño y seguir trabajando desde el editor.".into(),
            streaming: false,
        },
    ];
    state.file_rows = [
        ("apps", 0, true, true),
        ("agents-window", 1, true, true),
        ("src", 2, true, true),
        ("main.rs", 3, false, false),
        ("workspace.rs", 3, false, false),
        ("view.rs", 3, false, false),
        ("crates", 0, true, true),
        ("openide-ui", 1, true, false),
        ("openide-editor", 1, true, false),
        ("openide-harness", 1, true, false),
        ("docs", 0, true, false),
        ("Cargo.toml", 0, false, false),
        ("README.md", 0, false, false),
    ]
    .into_iter()
    .map(|(name, depth, is_dir, expanded)| FileRowView {
        name: name.into(),
        depth,
        is_dir,
        expanded,
        can_load_more: false,
    })
    .collect();

    let controller = openide_ui::EditorController::new(TextBuffer::new(SOURCE).unwrap());
    let snapshot = controller.snapshot().unwrap();
    let analysis = ParserEngine::default()
        .analyze(&snapshot, Language::Rust, &Cancellation::default())
        .unwrap();
    state.editor_workbench = EditorWorkbenchView {
        document_id: Some(snapshot.document_id()),
        highlight_spans: analysis
            .highlights
            .iter()
            .map(|span| openide_ui::CodeHighlight {
                range: span.range.clone(),
                color: match span.kind {
                    openide_language::HighlightKind::Comment => Color::rgb(126, 151, 119),
                    openide_language::HighlightKind::String => Color::rgb(206, 163, 127),
                    openide_language::HighlightKind::Keyword => Color::rgb(184, 143, 216),
                    openide_language::HighlightKind::Number => Color::rgb(151, 193, 169),
                    openide_language::HighlightKind::Type => Color::rgb(225, 196, 131),
                    openide_language::HighlightKind::Function => Color::rgb(143, 185, 218),
                    _ => theme::FOREGROUND,
                },
            })
            .collect::<Vec<_>>()
            .into(),
        analysis: Some(Arc::new(analysis)),
        language_status: "Rust · análisis local".into(),
        ..Default::default()
    };
    state.code_editor = Some(CodeEditorView {
        tab_id: 71,
        path: "apps/agents-window/src/workspace.rs".into(),
        controller: Some(controller),
        loading: false,
        saving: false,
        error: None,
        notice: None,
        conflict: false,
        recovered: false,
        missing: false,
    });
    state.side_tab = SideTab::FilePreview;
    state.dock_tabs = vec![
        DockTabView {
            id: 71,
            title: "workspace.rs".into(),
            full_path: Some("apps/agents-window/src/workspace.rs".into()),
            active: true,
            kind: SideTab::FilePreview,
            closable: true,
        },
        DockTabView {
            id: 72,
            title: "Cargo.toml".into(),
            full_path: Some("Cargo.toml".into()),
            active: false,
            kind: SideTab::FilePreview,
            closable: true,
        },
    ];
    state.show_terminal_panel = true;
    state.terminal_panel_height = 216.0;
    state.terminal_tabs = vec![DockTabView {
        id: 81,
        title: "bash".into(),
        full_path: Some(state.workspace.clone()),
        active: true,
        kind: SideTab::Terminal,
        closable: true,
    }];
    let output = [
        (
            "openADE $ cargo test -p openide-editor",
            TerminalColor::Default,
        ),
        ("   Compiling openide-editor", TerminalColor::Indexed(2)),
        (
            "    Finished test profile in 1.42s",
            TerminalColor::Indexed(2),
        ),
        ("running 24 tests", TerminalColor::Default),
        (
            "test result: ok. 24 passed; 0 failed",
            TerminalColor::Indexed(2),
        ),
        ("openADE $ ", TerminalColor::Default),
    ];
    state.terminal_snapshot = Some(TerminalSnapshot {
        lines: output.iter().map(|(line, _)| line.to_string()).collect(),
        styled_lines: output
            .iter()
            .map(|(line, foreground)| {
                vec![TerminalSpan {
                    text: line.to_string(),
                    style: TerminalStyle {
                        foreground: *foreground,
                        ..Default::default()
                    },
                }]
            })
            .collect(),
        cursor: (5, 10),
        cols: 88,
        rows: 12,
        scrollback_position: 0,
        running: true,
        exit_code: None,
        error: None,
        revision: 1,
        shell: "bash".into(),
        alternate_screen: false,
        selection: None,
    });
    state
}

/// Writes `frame` as `docs/ui-separation/shots/<folder>/<name>.png` at 2x.
pub(crate) fn export_shot(frame: &Frame, size: Size, folder: &str, name: &str) {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/ui-separation/shots")
        .join(folder);
    std::fs::create_dir_all(&directory).unwrap();
    let svg = std::env::temp_dir().join(format!("openade-shot-{folder}-{name}.svg"));
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &svg, size.width, size.height)
        .unwrap();
    let parsed = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&svg).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixels =
        resvg::tiny_skia::Pixmap::new(size.width as u32 * 2, size.height as u32 * 2).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::from_scale(2.0, 2.0),
        &mut pixels.as_mut(),
    );
    pixels.save_png(directory.join(format!("{name}.png"))).unwrap();
    std::fs::remove_file(svg).unwrap();
}

pub(crate) fn native_tree(root: Element) -> (egui::Context, UiTree) {
    let context = egui::Context::default();
    crate::fonts::install(&context);
    context.set_pixels_per_point(2.0);
    context.begin_pass(egui::RawInput::default());
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    tree.set_context_menu_palette(theme::menu_palette());
    for family in [FontFamily::Monospace, FontFamily::Sans] {
        let font = match family {
            FontFamily::Monospace => egui::FontId::monospace(14.0),
            FontFamily::Sans => egui::FontId::proportional(14.0),
        };
        let metrics = context.fonts_mut(|fonts| FontMetrics {
            char_width: fonts
                .glyph_width(&font, if family == FontFamily::Sans { 'a' } else { 'M' }),
            line_height: fonts.row_height(&font),
        });
        match family {
            FontFamily::Monospace => tree.set_metrics(metrics),
            FontFamily::Sans => tree.set_sans_metrics(metrics),
        }
    }
    let measurer = context.clone();
    tree.set_text_measurer(move |text, size, family| {
        measurer.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::new(
                        size,
                        match family {
                            FontFamily::Sans => egui::FontFamily::Proportional,
                            FontFamily::Monospace => egui::FontFamily::Monospace,
                        },
                    ),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
    });
    (context, tree)
}

/// Controlled service snapshots exercise the actual native widgets and their
/// scoped actions. These fixtures do not invoke Git, launch a debugger or install
/// a skill while exporting the shell.
fn activity_fixture(phase: &str, width: f32) -> ViewState {
    use crate::debug_ui::*;
    use crate::marketplace::{InstallScope, MarketplaceItem, MarketplaceSource, SourceKind};
    let mut state = fixture();
    state.ide.activity = match phase {
        "activity-search" => IdeActivity::Search,
        "activity-git" => IdeActivity::Git,
        "activity-debug" | "bottom-console" => IdeActivity::Debug,
        "activity-extensions" => IdeActivity::Extensions,
        _ => IdeActivity::Files,
    };
    if phase.starts_with("activity-") && width <= 520.0 {
        state.ide.compact_panel = Some(IdePanel::Explorer);
    }
    state.ide_search.workspace = state.workspace.clone();
    state.ide_search.query = "open_file".into();
    state.ide_search.generation = 1;
    state.ide_search.searched = true;
    state.ide_search.hits = vec![
        crate::ide_search::SearchHit {
            path: "apps/agents-window/src/workspace.rs".into(),
            line: 18,
            character: 11,
            snippet: "pub fn open_file(&mut self, path: &str) {".into(),
        },
        crate::ide_search::SearchHit {
            path: "apps/agents-window/src/workspace_tests.rs".into(),
            line: 42,
            character: 14,
            snippet: "workspace.open_file(\"README.md\");".into(),
        },
    ];
    state.git_review = Some(Ok(Arc::new(GitReview {
        branch: "feature/ide-workspace".into(),
        tracking: Some(crate::git_review::GitTracking {
            upstream: "origin/main".into(),
            ahead: 2,
            behind: 0,
        }),
        sections: vec![
            crate::git_review::ReviewSection {
                title: "CAMBIOS SIN PREPARAR".into(),
                files: vec![ReviewFile {
                    path: "apps/agents-window/src/workspace.rs".into(),
                    status: "Modificado".into(),
                    additions: 18,
                    deletions: 4,
                    ..Default::default()
                }],
                additions: 18,
                deletions: 4,
            },
            crate::git_review::ReviewSection {
                title: "CAMBIOS PREPARADOS".into(),
                files: vec![ReviewFile {
                    path: "Cargo.toml".into(),
                    status: "Modificado".into(),
                    additions: 2,
                    ..Default::default()
                }],
                additions: 2,
                deletions: 0,
            },
        ],
        untracked: vec!["docs/ide-workspace.md".into()],
        files: 2,
        additions: 20,
        deletions: 4,
        ..Default::default()
    })));
    state.debug_open = matches!(state.ide.activity, IdeActivity::Debug);
    state.debug = DebugView {
        scope: "ide-visual-workspace-debug-session-1".into(),
        status: DebugStatus::Stopped,
        configuration: "Workspace · Rust".into(),
        configurations: vec!["Workspace · Rust".into()].into(),
        adapter: Some("CodeLLDB".into()),
        stop_reason: Some("punto de interrupción".into()),
        controls: DebugControls {
            resume: true,
            step_over: true,
            step_into: true,
            step_out: true,
            restart: true,
            stop: true,
            ..Default::default()
        },
        threads: vec![DebugThreadView {
            id: 1,
            name: "main".into(),
            selected: true,
            stopped: true,
        }]
        .into(),
        frames: vec![
            DebugFrameView {
                id: 10,
                name: "Workspace::open_file".into(),
                path: Some("apps/agents-window/src/workspace.rs".into()),
                line: Some(19),
                selected: true,
            },
            DebugFrameView {
                id: 11,
                name: "main".into(),
                path: Some("apps/agents-window/src/main.rs".into()),
                line: Some(34),
                selected: false,
            },
        ]
        .into(),
        variables: vec![
            DebugVariableView {
                key: "scope:1".into(),
                name: "Locales".into(),
                value: "".into(),
                type_name: None,
                depth: 0,
                has_children: true,
                expanded: true,
                ..Default::default()
            },
            DebugVariableView {
                key: "path:1".into(),
                name: "path".into(),
                value: "\"README.md\"".into(),
                type_name: Some("&str".into()),
                depth: 1,
                has_children: false,
                expanded: false,
                ..Default::default()
            },
            DebugVariableView {
                key: "files:1".into(),
                name: "self.files".into(),
                value: "Vec(2)".into(),
                type_name: Some("Vec<String>".into()),
                depth: 1,
                has_children: true,
                expanded: false,
                ..Default::default()
            },
        ]
        .into(),
        output: vec![
            DebugOutputView {
                category: "console".into(),
                text: "Adaptador conectado: CodeLLDB".into(),
            },
            DebugOutputView {
                category: "stdout".into(),
                text: "Proyecto abierto: openADE\nArchivos abiertos: 2".into(),
            },
        ]
        .into(),
        breakpoints: vec![DebugBreakpointView {
            path: "apps/agents-window/src/workspace.rs".into(),
            line: 19,
            enabled: true,
            ..Default::default()
        }]
        .into(),
        ..Default::default()
    };
    state.marketplace.snapshot.sources = vec![MarketplaceSource {
        id: "team".into(),
        name: "Equipo".into(),
        location: "/workspace/team-skills".into(),
        kind: SourceKind::Local,
    }];
    state.marketplace.snapshot.items = [
        (
            "review",
            "Revisión de código",
            "Revisa cambios, pruebas y compatibilidad del proyecto.",
            true,
        ),
        (
            "rust",
            "Desarrollo en Rust",
            "Convenciones del workspace y validación con cargo.",
            true,
        ),
        (
            "docs",
            "Documentación",
            "Redacta documentación siguiendo las guías del equipo.",
            false,
        ),
    ]
    .into_iter()
    .map(|(id, name, description, installed)| MarketplaceItem {
        id: format!("team:{id}"),
        name: name.into(),
        description: description.into(),
        license: Some("MIT".into()),
        source_name: "Equipo".into(),
        source_kind: SourceKind::Local,
        installed_scope: installed.then_some(InstallScope::Workspace),
        enabled: installed,
        source_available: true,
    })
    .collect();
    if phase == "activity-extensions" {
        state.ide.extensions_open = true;
        state.marketplace.selected_item_id = Some("team:review".into());
        state.show_terminal_panel = false;
    }
    if phase.starts_with("bottom-") {
        state.ide.compact_panel = None;
        state.show_terminal_panel = false;
        state.ide.bottom_open = true;
        state.ide.bottom_panel = if phase == "bottom-problems" {
            IdeBottomPanel::Problems
        } else {
            IdeBottomPanel::DebugConsole
        };
        if phase == "bottom-problems" {
            let snapshot = state
                .code_editor
                .as_ref()
                .unwrap()
                .controller
                .as_ref()
                .unwrap()
                .snapshot()
                .unwrap();
            state.editor_workbench.diagnostics_current = true;
            state.editor_workbench.diagnostic_revision =
                Some((snapshot.document_id(), snapshot.revision()));
            state.editor_workbench.diagnostics = vec![openide_language::lsp::Diagnostic {
                range: openide_language::Range {
                    start: openide_language::Position {
                        line: 24,
                        character: 11,
                    },
                    end: openide_language::Position {
                        line: 24,
                        character: 23,
                    },
                },
                severity: Some(2),
                code: Some(serde_json::json!("dead_code")),
                source: Some("rust-analyzer".into()),
                message: "El método project_name todavía no se utiliza".into(),
            }];
        }
    }
    state
}

fn export(frame: &Frame, size: Size, name: &str) {
    export_to("/tmp/openade-ide-workspace-review", frame, size, name);
}

fn export_to(directory: &str, frame: &Frame, size: Size, name: &str) {
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
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

#[test]
#[ignore = "export native IDE workspace, compact layouts and shared panels for visual inspection"]
fn export_ide_workspace_visual_review() {
    for size in [
        Size::new(1440.0, 900.0),
        Size::new(1024.0, 768.0),
        Size::new(800.0, 680.0),
        Size::new(520.0, 680.0),
        Size::new(320.0, 680.0),
    ] {
        for phase in [
            "workspace",
            "empty",
            "agent-hidden",
            "mode-hover",
            "drawer-agent",
            "drawer-explorer",
            "menu-file",
            "menu-view",
            "mode-reveal",
            "new-file",
            "activity-search",
            "activity-git",
            "activity-debug",
            "activity-extensions",
            "bottom-problems",
            "bottom-console",
        ] {
            let mut state = fixture();
            if phase.starts_with("activity-") || phase.starts_with("bottom-") {
                if !matches!(size.width as u32, 1440 | 520) {
                    continue;
                }
                state = activity_fixture(phase, size.width);
            }
            if phase == "new-file" {
                if !matches!(size.width as u32, 1440 | 320) {
                    continue;
                }
                state.code_editor = None;
                state.dock_tabs.clear();
                state.side_tab = SideTab::Files;
                state.show_terminal_panel = false;
                state.ide.new_file = Some(crate::ide_workspace::IdeNewFile {
                    workspace: state.workspace.clone(),
                    path: "src/nuevo.rs".into(),
                    error: None,
                });
            }
            if phase == "menu-file" {
                if !matches!(size.width as u32, 1440 | 520) {
                    continue;
                }
                state.ide.menu_open =
                    Some(if size.width >= 1208.0 { "file" } else { "all" }.into());
            }
            if phase == "menu-view" {
                if size.width != 1440.0 {
                    continue;
                }
                state.ide.menu_open = Some("view".into());
            }
            if phase == "mode-reveal" {
                if size.width != 1440.0 {
                    continue;
                }
                state.ide.mode_reveal = 0.5;
            }
            if phase == "mode-hover" {
                state.ide.mode_reveal = 1.0;
            }
            if phase == "drawer-agent" {
                if size.width > 800.0 {
                    continue;
                }
                state.ide.compact_panel = Some(IdePanel::Agent);
            }
            if phase == "drawer-explorer" {
                if size.width > 320.0 {
                    continue;
                }
                state.ide.compact_panel = Some(IdePanel::Explorer);
            }
            if phase == "empty" {
                state.code_editor = None;
                state.dock_tabs.clear();
                state.side_tab = SideTab::Files;
                state.show_terminal_panel = false;
                state.selected_session = None;
                state.selected_session_title = None;
                state.transcript.clear();
                state.prompt.clear();
            }
            if phase == "agent-hidden" {
                state.ide.agent_open = false;
            }
            let (context, mut tree) =
                native_tree(build_window_view(&state, size.width, size.height));
            let now = std::time::Instant::now();
            let mut frame = tree.layout_at(size, now);
            if phase == "mode-hover" {
                let toggle = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "window_mode_toggle")
                    .expect("IDE mode switch is visible")
                    .rect;
                tree.handle_event_at(
                    UiEvent::PointerMove(Point::new(
                        toggle.x + toggle.width / 2.0,
                        toggle.y + toggle.height / 2.0,
                    )),
                    now,
                );
                tree.layout_at(size, now);
                frame = tree.layout_at(size, now + std::time::Duration::from_millis(700));
            }
            export(&frame, size, phase);
            context.end_pass().textures_delta.clear();
        }
    }
}

/// Idle, hover and pressed frames of one control, found by its region id.
fn export_states(
    build: impl Fn(Size) -> Element,
    size: Size,
    scene: &str,
    targets: &[(&str, &str)],
) {
    for (label, id, pressed) in std::iter::once(("idle", "", false)).chain(
        targets
            .iter()
            .flat_map(|(label, id)| [(*label, *id, false), (*label, *id, true)]),
    ) {
        let (context, mut tree) = native_tree(build(size));
        let now = std::time::Instant::now();
        let mut frame = tree.layout_at(size, now);
        let mut name = format!("{scene}-idle");
        if !id.is_empty() {
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing {id} in {scene}"))
                .rect;
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event_at(UiEvent::PointerMove(point), now);
            if pressed {
                tree.handle_event_at(UiEvent::PointerDown(point), now);
            }
            tree.layout_at(size, now);
            frame = tree.layout_at(size, now + std::time::Duration::from_millis(700));
            name = format!("{scene}-{label}-{}", if pressed { "pressed" } else { "hover" });
        }
        export_to("/tmp/openade-interaction-states-review", &frame, size, &name);
        context.end_pass().textures_delta.clear();
    }
}

#[test]
#[ignore = "export idle, hover and pressed states of the controls the limn separation migrates"]
fn export_interaction_states_visual_review() {
    let size = Size::new(1024.0, 768.0);

    let mut ide = fixture();
    ide.ide.agent_open = false;
    ide.terminal_tabs.push(DockTabView {
        id: 82,
        title: "zsh".into(),
        full_path: Some(ide.workspace.clone()),
        active: false,
        kind: SideTab::Terminal,
        closable: true,
    });
    let window = |state: &ViewState, size: Size| build_window_view(state, size.width, size.height);
    export_states(
        |size| window(&ide, size),
        size,
        "ide",
        &[
            ("titlebar-toggle-active", "ide_explorer_toggle"),
            ("titlebar-toggle", "ide_agent_toggle"),
            ("activity-button-active", "ide_activity:files"),
            ("activity-button", "ide_activity:search"),
        ],
    );
    export_states(
        |size| {
            Element::panel(crate::terminal_dock_ui::build(&ide, size.width, size.height))
                .background(theme::BACKGROUND)
        },
        Size::new(760.0, 300.0),
        "terminal",
        &[
            ("tab-close-active", "terminal_close:81"),
            ("tab-close", "terminal_close:82"),
        ],
    );

    // The agent panel's tabs beside the editor's: one design for both.
    let mut tabs = fixture();
    let mut second = tabs.sessions[0].clone();
    second.id = "cli-task".into();
    second.title = "Claude Code".into();
    second.backend = "cli:claude".into();
    tabs.sessions.push(second);
    tabs.ide.agent_tabs = vec!["ide-review".into(), "cli-task".into()];
    for size in [Size::new(1440.0, 768.0), size] {
        export_states(
            |size| window(&tabs, size),
            size,
            "ide-agent-tabs",
            &[
                ("tab", "session:cli-task"),
                ("close", "ide_agent_tab_close:cli-task"),
                ("close-active", "ide_agent_tab_close:ide-review"),
            ],
        );
    }

    let mut agents = tests::empty_state();
    agents.selected_session = Some("selected".into());
    agents.sessions = ["idle", "selected"]
        .into_iter()
        .map(|id| SessionView {
            id: id.into(),
            title: format!("Sesión {id}"),
            project: "/project".into(),
            backend: "native".into(),
            pinned: false,
            unread: false,
            archived: false,
            native_automation: None,
        })
        .collect();
    agents.show_side_pane = true;
    agents.side_tab = SideTab::Activity;
    agents.dock_tabs = [("Archivos", SideTab::Files), ("Actividad", SideTab::Activity)]
        .into_iter()
        .enumerate()
        .map(|(index, (title, kind))| DockTabView {
            full_path: None,
            id: index as u64 + 1,
            title: title.into(),
            active: kind == SideTab::Activity,
            kind,
            closable: true,
        })
        .collect();
    // Wide enough for the dock strip to show both tabs whole.
    export_states(
        |size| window(&agents, size),
        Size::new(1440.0, 768.0),
        "agents",
        &[
            ("session-row", "session:idle"),
            ("session-row-selected", "session:selected"),
            ("dock-tab", "dock_tab:1"),
            ("dock-tab-active", "dock_tab:2"),
            ("dock-tab-close", "dock_close:1"),
            ("dock-tab-close-active", "dock_close:2"),
        ],
    );

    let mut settings = tests::empty_state();
    settings.show_settings = true;
    settings.settings_page = SettingsPage::Mcp;
    settings.mcp_editor_open = true;
    export_states(
        |size| window(&settings, size),
        size,
        "settings",
        &[
            ("choice-button-active", "mcp_transport:stdio"),
            ("choice-button", "mcp_transport:http"),
        ],
    );
}
