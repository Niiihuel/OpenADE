//! Home layout inspection from the actual retained tree and bundled fonts.
//! All drafts and route metadata are local fixtures; no account is accessed.

use super::build_home_with_height;
use crate::composer_selection::{ExecutionSupport, ModelRouteOption};
use crate::composer_ui::{
    tests::{configured_effort_route, props},
    ComposerProps,
};
use eframe::egui;
use openide_ui::{
    DrawCommand, Element, FontFamily, FontMetrics, Frame, HitKind, Rect, Size, UiTree,
};
use std::path::Path;

const MULTILINE: &str =
    "Revisá el proyecto.\nExplicá cómo funciona el streaming.\nConservá los cambios existentes.";
const VIEWPORTS: [Size; 2] = [Size::new(900.0, 700.0), Size::new(360.0, 660.0)];

fn routes() -> [ModelRouteOption; 1] {
    let mut route = configured_effort_route();
    route.execution = ExecutionSupport::Verified;
    route.effort_options = vec![
        ("default".into(), "Automático".into()),
        ("low".into(), "Bajo".into()),
        ("medium".into(), "Medio".into()),
        ("high".into(), "Alto".into()),
        ("max".into(), "Máximo".into()),
    ];
    [route]
}

fn fixture<'a>(prompt: &'a str, width: f32, routes: &'a [ModelRouteOption]) -> ComposerProps<'a> {
    let mut state = props(prompt, width);
    state.home = true;
    state.placeholder = "Pregúntale a openADE";
    state.workspace = "/projects/openADE";
    state.project_action = Some("home_workspace_settings");
    state.project_branch = Some("main");
    state.native_provider_id = "codex";
    state.selected_model = "gpt-5.6-luna";
    state.model_label = "GPT-5.6 Luna";
    state.model_routes = routes;
    state.effort_options = &routes[0].effort_options;
    state.selected_effort = "max";
    state.effort_label = "Máximo";
    state.full_access = true;
    state.permission_controls = Some((
        openide_harness::ToolPermissions::default(),
        openide_harness::CommandIsolation::HostUnrestricted,
        false,
    ));
    state
}

fn native_tree(state: &ComposerProps<'_>, viewport: Size) -> (egui::Context, UiTree) {
    let context = egui::Context::default();
    crate::fonts::install(&context);
    context.begin_pass(egui::RawInput::default());
    let measure = context.clone();
    let root = Element::panel(build_home_with_height(
        state,
        viewport.width,
        Some(viewport.height),
    ))
    .background(crate::theme::BACKGROUND);
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    let mono_font = egui::FontId::monospace(14.0);
    tree.set_metrics(context.fonts_mut(|fonts| FontMetrics {
        char_width: fonts.glyph_width(&mono_font, 'M'),
        line_height: fonts.row_height(&mono_font),
    }));
    let sans_font = egui::FontId::proportional(14.0);
    tree.set_sans_metrics(context.fonts_mut(|fonts| {
        FontMetrics {
            char_width: "abcdefghijklmnopqrstuvwxyz"
                .chars()
                .map(|character| fonts.glyph_width(&sans_font, character))
                .sum::<f32>()
                / 26.0,
            line_height: fonts.row_height(&sans_font),
        }
    }));
    tree.set_text_measurer(move |text, size, family| {
        measure.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::new(
                        size,
                        match family {
                            FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontFamily::Proportional,
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

fn rect(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind != HitKind::Hover)
        .unwrap_or_else(|| panic!("missing home control {id}"))
        .rect
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x - 0.1
        && inner.y >= outer.y - 0.1
        && inner.x + inner.width <= outer.x + outer.width + 0.1
        && inner.y + inner.height <= outer.y + outer.height + 0.1
}

fn card(frame: &Frame) -> Rect {
    let editor = rect(frame, "prompt");
    frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::FillRect {
                rect,
                color,
                radius,
            } if *radius == 10.0
                && *color == openide_ui::Color::rgb(38, 38, 38)
                && rect.width >= 220.0
                && contains(*rect, editor) =>
            {
                Some(*rect)
            }
            _ => None,
        })
        .expect("actual home composer card")
}

fn assert_layout(frame: &Frame, viewport: Size, state: &ComposerProps<'_>) {
    let bounds = Rect::new(0.0, 0.0, viewport.width, viewport.height);
    let card = card(frame);
    let editor = rect(frame, "prompt");
    let plus = rect(frame, "composer_add_context");
    let model = rect(frame, "composer_model_toggle");
    let send = rect(frame, "run_toggle");
    let permission = rect(frame, "composer:permissions");
    let project = rect(frame, "home_workspace_settings");
    assert!(
        contains(bounds, card),
        "home card stays in viewport: {card:?}"
    );
    assert!(card.x >= 24.0 - 0.1);
    assert!(card.x + card.width <= viewport.width - 24.0 + 0.1);
    for id in [
        "prompt",
        "composer_add_context",
        "composer_model_toggle",
        "run_toggle",
    ] {
        assert!(contains(card, rect(frame, id)), "{id} stays in card");
    }
    assert!(contains(bounds, project));
    assert!(project.y + project.height <= card.y);
    assert!(model.x + model.width <= send.x);
    assert_eq!(plus.y, send.y);
    assert!(editor.y + editor.height <= plus.y);
    assert!(editor.width >= card.width - 18.0);
    assert!(contains(card, permission));
    assert!(plus.x + plus.width <= permission.x);
    assert!(permission.x + permission.width <= model.x);
    if state.environment.is_some() {
        let environment = rect(frame, "composer_environment_toggle");
        let branch = rect(frame, "composer_branch_toggle");
        let context = rect(frame, "composer_context_toggle");
        for target in [environment, branch, context] {
            assert!(
                contains(bounds, target),
                "strip target in bounds: {target:?}"
            );
            assert!(
                target.y + target.height <= card.y,
                "strip is above the editor"
            );
        }
        assert!(project.x + project.width <= environment.x);
        assert!(environment.x + environment.width <= branch.x);
        assert!(branch.x + branch.width <= context.x);
    }
    if viewport.width >= 688.0 {
        let microphone = rect(frame, "composer_microphone");
        assert!(contains(card, microphone));
        assert_eq!(microphone.y, send.y);
        assert!(model.x + model.width <= microphone.x);
        assert!(microphone.x + microphone.width <= send.x);
    }
    if state.prompt.is_empty() {
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, font_size, .. }
                    if text == state.placeholder && *font_size == 13.0
            )),
            "empty home paints the actual placeholder"
        );
    }
}

#[test]
fn home_native_layout_keeps_editor_selectors_and_expanded_action_in_bounds() {
    let routes = routes();
    let environment = crate::environment_ui::EnvironmentUiState::default();
    for viewport in VIEWPORTS {
        for prompt in ["", MULTILINE] {
            let mut state = fixture(prompt, viewport.width, &routes);
            state.environment = Some(&environment);
            let (context, mut tree) = native_tree(&state, viewport);
            let frame = tree.layout(viewport);
            assert_layout(&frame, viewport, &state);
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn home_permissions_and_model_effort_popovers_fit_the_viewport() {
    let routes = routes();
    let environment = crate::environment_ui::EnvironmentUiState::default();
    for viewport in VIEWPORTS {
        for menu in ["permissions", "model", "model-list", "effort"] {
            let mut state = fixture("", viewport.width, &routes);
            state.environment = Some(&environment);
            if menu == "permissions" {
                state.permission_controls.as_mut().unwrap().2 = true;
            } else {
                state.model_menu_open = true;
                state.model_list_open = menu == "model-list";
                state.advanced_open = menu == "effort";
            }
            let (context, mut tree) = native_tree(&state, viewport);
            let frame = tree.layout(viewport);
            assert_layout(&frame, viewport, &state);
            let viewport_bounds = Rect::new(0.0, 0.0, viewport.width, viewport.height);
            let ids: &[&str] = match menu {
                "permissions" => &[
                    "composer_mode:plan",
                    "composer_permissions:read_only",
                    "composer_permissions:ask",
                    "composer_permissions:full_access",
                ],
                "model" => &["composer_model_list_toggle", "composer_advanced_toggle"],
                "model-list" => &[
                    "composer_model_search",
                    "composer_route:native:codex:gpt-5.6-luna",
                    "composer_connect_provider",
                ],
                _ => &["composer_effort_slider"],
            };
            for id in ids {
                assert!(
                    contains(viewport_bounds, rect(&frame, id)),
                    "{menu}/{id} stays in viewport"
                );
            }
            if menu == "effort" {
                let slider = rect(&frame, "composer_effort_slider");
                assert!(slider.y + slider.height <= rect(&frame, "composer_model_toggle").y);
                assert!(frame
                    .commands
                    .iter()
                    .any(|command| matches!(command, DrawCommand::EffortSlider { .. })));
                // The SVG backend cannot paint the native animated shader.
                // Its genuine paint command and geometry are checked here.
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

fn export(frame: &Frame, directory: &Path, name: &str, viewport: Size) {
    let svg_path = directory.join(format!("{name}-{:.0}.svg", viewport.width));
    crate::settings_ui::visual_qa::write_native_frame_svg(
        frame,
        &svg_path,
        viewport.width,
        viewport.height,
    )
    .unwrap();
    let svg = std::fs::read_to_string(&svg_path).unwrap();
    let parsed =
        resvg::usvg::Tree::from_str(&svg, &crate::settings_ui::visual_qa::native_svg_options())
            .unwrap();
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(viewport.width as u32, viewport.height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(svg_path.with_extension("png")).unwrap();
}

fn review_environment_selectors(directory: Option<&Path>) {
    let temporary = tempfile::tempdir().unwrap();
    let project = temporary.path().join("openADE");
    std::fs::create_dir(&project).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "Fixture",
        ],
        vec!["branch", "feature/composer"],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&project)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let workspace = project.to_string_lossy().into_owned();
    let projects = vec![
        (workspace.clone(), "openADE".into()),
        ("/projects/website".into(), "website".into()),
        ("/projects/design-system".into(), "design-system".into()),
    ];
    let routes = routes();
    for viewport in VIEWPORTS {
        for menu in ["project", "local", "branch"] {
            let mut environment = crate::environment_ui::EnvironmentUiState::default();
            environment.workflow.snapshot =
                Some(crate::environment_git::load_snapshot(&project).unwrap());
            environment.workflow.snapshot_revision = 1;
            environment.composer_local_open = menu == "local";
            let picker = crate::git_branches::BranchPickerView {
                workspace: Some(project.clone()),
                revision: 1,
                open: menu == "branch",
                snapshot: Some(crate::git_branches::load_branches(&project).unwrap()),
                ..Default::default()
            };
            let mut state = fixture("", viewport.width, &routes);
            state.workspace = &workspace;
            state.project_options = &projects;
            state.environment = Some(&environment);
            state.branch_picker = Some(&picker);
            state.project_menu_open = menu == "project";
            state.branch_menu_open = menu == "branch";
            let (context, mut tree) = native_tree(&state, viewport);
            let frame = tree.layout(viewport);
            assert_layout(&frame, viewport, &state);
            let bounds = Rect::new(0.0, 0.0, viewport.width, viewport.height);
            let ids: &[&str] = match menu {
                "project" => &[
                    "composer_project_search",
                    "composer_project_select:0",
                    "composer_project_browse",
                ],
                "local" => &[
                    "environment_worktree_select:1:0",
                    "environment_git_open_worktree",
                ],
                _ => &["composer_branch_search", "environment_git_open_branch"],
            };
            for id in ids {
                assert!(
                    contains(bounds, rect(&frame, id)),
                    "{menu}/{id} stays in viewport at {viewport:?}"
                );
            }
            if let Some(directory) = directory {
                export(&frame, directory, &format!("home-{menu}-open"), viewport);
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn environment_selectors_fit_the_home_with_native_fonts() {
    review_environment_selectors(None);
}

#[test]
#[ignore = "opt-in home SVG/PNG inspection with native fonts"]
fn export_home_layout_visual_review() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("OPENIDE_HOME_VISUAL_DIR")
            .unwrap_or_else(|| "/tmp/openade-home-layout-review".into()),
    );
    std::fs::create_dir_all(&directory).unwrap();
    review_environment_selectors(Some(&directory));
    let routes = routes();
    let environment = crate::environment_ui::EnvironmentUiState::default();
    for viewport in VIEWPORTS {
        for (name, prompt) in [("home-empty", ""), ("home-multiline", MULTILINE)] {
            let mut state = fixture(prompt, viewport.width, &routes);
            state.environment = Some(&environment);
            let (context, mut tree) = native_tree(&state, viewport);
            let frame = tree.layout(viewport);
            assert_layout(&frame, viewport, &state);
            export(&frame, &directory, name, viewport);
            context.end_pass().textures_delta.clear();
        }
        let mut state = fixture("", viewport.width, &routes);
        state.environment = Some(&environment);
        state.permission_controls.as_mut().unwrap().2 = true;
        let (context, mut tree) = native_tree(&state, viewport);
        let frame = tree.layout(viewport);
        assert_layout(&frame, viewport, &state);
        export(&frame, &directory, "home-permissions-open", viewport);
        context.end_pass().textures_delta.clear();
    }
}
