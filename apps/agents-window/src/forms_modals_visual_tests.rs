//! Review captures of the forms, selects and modals, built from synthetic
//! state with the native fonts. Layout exports, not host pixel screenshots.

use std::{fs, path::Path};

use openide_ui::{Element, Size};

use crate::{
    automation_view::{
        build_automations, AutomationEditorStep, AutomationFrequency, AutomationScreen,
        AutomationViewState,
    },
    marketplace::SourceKind,
    marketplace_view::{marketplace_page, MarketplaceViewState},
    settings_ui::{self, visual_qa, SettingsPage},
    theme,
    view::{tests::empty_state, MainPage, ViewState},
};

const HEIGHT: f32 = 760.0;

type Scene = (String, Box<dyn Fn(f32) -> Element>);

fn behind() -> Element {
    Element::spacer().background(theme::BACKGROUND)
}

fn settings(page: SettingsPage, edit: impl Fn(&mut ViewState)) -> ViewState {
    let mut state = empty_state();
    state.show_settings = true;
    state.settings_page = page;
    state.workspace = "/workspace/openADE".into();
    state.workspace_draft = state.workspace.clone();
    edit(&mut state);
    state
}

fn modal_scene(
    name: &str,
    state: ViewState,
    modal: fn(&ViewState, Element, f32) -> Element,
) -> Scene {
    (name.to_owned(), Box::new(move |width| modal(&state, behind(), width)))
}

fn page_scene(name: &str, state: ViewState) -> Scene {
    (
        name.to_owned(),
        Box::new(move |width| {
            settings_ui::build_settings_page(&state, state.settings_page, width, false)
                .background(theme::BACKGROUND)
        }),
    )
}

fn automation(step: AutomationEditorStep, frequency: AutomationFrequency) -> AutomationViewState {
    let backends = vec![
        ("native".to_owned(), "OpenIDE".to_owned()),
        ("codex".to_owned(), "Codex".to_owned()),
        ("claude".to_owned(), "Claude Code".to_owned()),
    ];
    let mut state = AutomationViewState::new("/workspace/openADE", backends, 0);
    state.screen = AutomationScreen::Edit;
    state.editor_step = step;
    state.calendar_year = 2026;
    state.calendar_month = 3;
    state.form.title = "Resumen diario".into();
    state.form.frequency = frequency;
    state.form.once_at = "2026-03-14T09:30".into();
    state.form.weekdays = 1 << 2;
    state
}

fn scenes() -> Vec<Scene> {
    let mut scenes = vec![
        modal_scene(
            "hook-editor",
            settings(SettingsPage::Hooks, |state| {
                state.native_hooks.editor_open = true;
                state.native_hooks.draft_id = "revisar-escritura".into();
            }),
            settings_ui::hook_editor_modal,
        ),
        modal_scene(
            "hook-editor-error",
            settings(SettingsPage::Hooks, |state| {
                state.native_hooks.editor_open = true;
                state.settings_notice_error = true;
                state.settings_notice = Some("El ejecutable no existe.".into());
            }),
            settings_ui::hook_editor_modal,
        ),
        modal_scene(
            "rule-editor",
            settings(SettingsPage::Commands, |state| state.native_exec_rules.editor_open = true),
            settings_ui::exec_rule_editor_modal,
        ),
        modal_scene(
            "rule-reset",
            settings(SettingsPage::Commands, |state| {
                state.native_exec_rules.reset_confirm_open = true;
                state.native_exec_rules.load_error = Some("JSON inválido".into());
            }),
            settings_ui::exec_rule_reset_modal,
        ),
        modal_scene(
            "skill-reset",
            settings(SettingsPage::Skills, |state| {
                state.native_skills.reset_confirm_open = true;
                state.native_skills.load_error = Some("JSON inválido".into());
            }),
            settings_ui::native_skill_reset_modal,
        ),
        modal_scene(
            "external-approval",
            settings(SettingsPage::ExternalAgents, |state| {
                state.external_approval = Some(crate::view::ExternalApprovalView {
                    title: "Codex quiere ejecutar un comando".into(),
                    detail: "cargo test -p agents-window --no-fail-fast".into(),
                });
            }),
            crate::approval_ui::external_modal,
        ),
        modal_scene(
            "mcp-editor-stdio",
            settings(SettingsPage::Mcp, |state| state.mcp_editor_open = true),
            settings_ui::mcp_editor_modal,
        ),
        modal_scene(
            "mcp-editor-http",
            settings(SettingsPage::Mcp, |state| {
                state.mcp_editor_open = true;
                state.mcp_draft_http = true;
            }),
            settings_ui::mcp_editor_modal,
        ),
        modal_scene(
            "mcp-editor-oauth",
            settings(SettingsPage::Mcp, |state| {
                state.mcp_editor_open = true;
                state.mcp_draft_http = true;
                state.mcp_draft_oauth_enabled = true;
            }),
            settings_ui::mcp_editor_modal,
        ),
        modal_scene(
            "provider-connect",
            settings(SettingsPage::Providers, |state| {
                state.provider_connect =
                    Some(crate::provider_connections::ConnectView::new("anthropic", "Anthropic"));
            }),
            settings_ui::provider_connect_modal,
        ),
        modal_scene(
            "plugin-install",
            settings(SettingsPage::Plugins, |state| {
                state.plugins.install_open = true;
                state.plugins.source = "/home/me/plugins/demo".into();
            }),
            crate::plugins_ui::modal,
        ),
        modal_scene(
            "github-sign-in",
            settings(SettingsPage::Github, |state| {
                state.github_sign_in = Some(crate::github_account::GithubSignInView::Code {
                    code: "ABCD-1234".into(),
                    continued: false,
                });
            }),
            crate::github_account_ui::sign_in_alert,
        ),
    ];
    for (name, error) in [("new-file", None), ("new-file-error", Some("Ya existe un archivo con esa ruta."))] {
        let mut state = empty_state();
        state.workspace = "/workspace/openADE".into();
        state.main_page = MainPage::Workspace;
        state.ide.mode = crate::ide_workspace::WindowMode::Ide;
        state.ide.new_file = Some(crate::ide_workspace::IdeNewFile {
            workspace: state.workspace.clone(),
            path: "src/nuevo.rs".into(),
            error: error.map(str::to_owned),
        });
        scenes.push(modal_scene(name, state, super::ide_ui::new_file_modal));
    }
    for (name, page, edit) in [
        ("select-isolation", SettingsPage::Agent, (|state| state.settings_choice_menu_open = Some("isolation".into())) as fn(&mut ViewState)),
        ("select-web-search", SettingsPage::Agent, |state| state.settings_choice_menu_open = Some("native_web_search".into())),
        ("select-organization", SettingsPage::Data, |state| state.settings_choice_menu_open = Some("organization".into())),
        ("select-sort", SettingsPage::Data, |state| state.settings_choice_menu_open = Some("sort".into())),
        ("select-review", SettingsPage::ExternalAgents, |state| state.settings_choice_menu_open = Some("review_model".into())),
        ("select-keymap", SettingsPage::Shortcuts, |state| state.settings_keymap_menu_open = true),
        ("select-shell", SettingsPage::Terminal, |state| state.settings_terminal_shell_menu_open = true),
        ("selects-closed-agent", SettingsPage::Agent, |_| {}),
        ("selects-closed-terminal", SettingsPage::Terminal, |_| {}),
    ] {
        scenes.push(page_scene(name, settings(page, edit)));
    }
    for (name, step, frequency) in [
        ("automation-content", AutomationEditorStep::Content, AutomationFrequency::Daily),
        ("automation-daily", AutomationEditorStep::Schedule, AutomationFrequency::Daily),
        ("automation-weekly", AutomationEditorStep::Schedule, AutomationFrequency::Weekly),
        ("automation-minutes", AutomationEditorStep::Schedule, AutomationFrequency::EveryMinutes),
        ("automation-once", AutomationEditorStep::Schedule, AutomationFrequency::Once),
        ("automation-agent", AutomationEditorStep::Agent, AutomationFrequency::Daily),
    ] {
        let state = automation(step, frequency);
        scenes.push((name.to_owned(), Box::new(move |width| build_automations(&state, width))));
    }
    for (name, kind, status) in [
        ("marketplace-source-local", SourceKind::Local, None),
        ("marketplace-source-github", SourceKind::Github, Some("No se pudo leer el repositorio.")),
    ] {
        let state = MarketplaceViewState {
            add_source_kind: Some(kind),
            source_draft: "/home/me/skills".into(),
            status: status.map(str::to_owned),
            status_error: status.is_some(),
            ..Default::default()
        };
        scenes.push((name.to_owned(), Box::new(move |width| marketplace_page(&state, width))));
    }
    scenes
}

/// Writes one SVG and PNG per scene and width. Set `FORMS_MODALS_FRAMES` to
/// a directory to also dump each frame's draw commands, for a before/after
/// comparison of a refactor.
#[test]
#[ignore = "export the forms, selects and modals at compact and desktop widths"]
fn export_forms_modals_visual_review() {
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/forms-modals-visual-qa");
    fs::create_dir_all(&output).unwrap();
    let frames = std::env::var_os("FORMS_MODALS_FRAMES");
    if let Some(frames) = &frames {
        fs::create_dir_all(frames).unwrap();
    }
    let options = visual_qa::native_svg_options();
    for (name, scene) in scenes() {
        for width in [360.0_f32, 900.0] {
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                scene(width).size(Some(width), Some(HEIGHT)),
            );
            let frame = tree.layout(Size::new(width, HEIGHT));
            let name = format!("{name}-{width}");
            if let Some(frames) = &frames {
                fs::write(Path::new(frames).join(format!("{name}.txt")), format!("{frame:#?}")).unwrap();
            }
            let path = output.join(format!("{name}.svg"));
            visual_qa::write_native_frame_svg(&frame, &path, width, HEIGHT).unwrap();
            let parsed =
                resvg::usvg::Tree::from_str(&fs::read_to_string(&path).unwrap(), &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, HEIGHT as u32).unwrap();
            resvg::render(&parsed, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
            pixmap.save_png(path.with_extension("png")).unwrap();
            drop(context);
        }
    }
}
