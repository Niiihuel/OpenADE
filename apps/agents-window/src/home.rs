//! The empty-task surface. This is deliberately built from our retained UI tree
//! primitives so it shares keyboard, focus, and icon rules with the workspace.

use openide_ui::{Color, Element, FontFamily};

use crate::{
    composer_ui::{build_composer, ComposerProps},
    theme,
    view::ViewState,
};

const MAX_CONTENT_WIDTH: f32 = 1120.0;
const MIN_GUTTER: f32 = 24.0;

fn text(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}

fn content_width(available: f32) -> f32 {
    (available - MIN_GUTTER * 2.0).clamp(220.0, MAX_CONTENT_WIDTH)
}

fn build_home_with(inputs: &ComposerProps<'_>, available_width: f32) -> Element {
    let width = content_width(available_width);
    let compact = width < 470.0;
    let card = build_composer(&ComposerProps { width, ..*inputs });

    let title = text(
        if compact {
            "Nueva tarea"
        } else {
            "¿En qué trabajamos hoy?"
        },
        theme::FOREGROUND,
        if compact { 25.0 } else { 34.0 },
    );
    let centered_title = Element::row(vec![
        Element::spacer().flex(1.0),
        title,
        Element::spacer().flex(1.0),
    ]);
    let content = Element::column(vec![centered_title, card])
        .gap(if compact { 16.0 } else { 20.0 })
        .size(Some(width), None);

    Element::column(vec![
        Element::spacer().flex(1.0),
        Element::row(vec![
            Element::spacer().flex(1.0),
            content,
            Element::spacer().flex(1.0),
        ]),
        Element::spacer().flex(1.0),
    ])
    .flex(1.0)
}

pub fn build_home(state: &ViewState, available_width: f32) -> Element {
    let props = ComposerProps::from_state(
        state,
        content_width(available_width),
        true,
        Some("home_workspace_settings"),
        "Ask me anything",
        None,
    );
    build_home_with(&props, available_width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Size, UiTree};

    fn inputs() -> ComposerProps<'static> {
        ComposerProps {
            workspace: "C:/work/project",
            project_action: Some("home_workspace_settings"),
            project_branch: Some("main"),
            project_menu_open: false,
            project_options: &[],
            prompt: "Tarea",
            attachments: &[],
            add_menu_open: false,
            listening: false,
            placeholder: "Pide al agente que explore, construya o resuelva algo…",
            selected_backend: "codex",
            native_provider_id: "openai",
            backend_options: &[],
            backend_menu_open: false,
            model: "",
            model_label: "GPT-6 Sol",
            model_options: &[],
            model_routes: &[],
            provider_status: None,
            selected_model: "gpt-6-sol",
            model_menu_open: false,
            model_list_open: false,
            model_query: "",
            advanced_open: false,
            effort_label: "Ultra",
            effort_options: &[],
            selected_effort: "ultra",
            effort_menu_open: false,
            effort_preview: None,
            turn_mode: openide_core::TurnMode::Build,
            running: false,
            workspace_writes_enabled: false,
            status: None,
            context_percent: None,
            goal: None,
            goal_editor_open: false,
            goal_draft: "",
            width: MAX_CONTENT_WIDTH,
            home: true,
        }
    }

    #[test]
    fn desktop_home_centers_composer_and_keeps_actions() {
        let frame = UiTree::new(build_home_with(&inputs(), 900.0)).layout(Size::new(900.0, 700.0));
        let region = |id: &str| frame.regions.iter().find(|region| region.id == id).unwrap();
        let project = region("home_workspace_settings");
        let prompt = region("prompt");
        let submit = region("run_toggle");
        assert!(prompt.rect.x >= MIN_GUTTER);
        assert!(prompt.rect.x + prompt.rect.width <= 900.0 - MIN_GUTTER);
        assert!(prompt.rect.width <= MAX_CONTENT_WIDTH);
        assert!(project.rect.y > prompt.rect.y);
        assert_eq!(submit.rect.y, prompt.rect.y);
        assert!(prompt.rect.y > 150.0 && prompt.rect.y < 460.0);
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("starter:")));
    }

    #[test]
    fn narrow_home_keeps_submit_visible_without_shortcuts() {
        let frame = UiTree::new(build_home_with(&inputs(), 360.0)).layout(Size::new(360.0, 660.0));
        let region = |id: &str| frame.regions.iter().find(|region| region.id == id).unwrap();
        let submit = region("run_toggle");
        assert!(submit.rect.x + submit.rect.width <= 360.0);
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("starter:")));
    }
}
