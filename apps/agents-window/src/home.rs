//! The empty-task surface. This is deliberately built from our retained UI tree
//! primitives so it shares keyboard, focus, and icon rules with the workspace.

use openide_ui::{Component, Element, ScrollArea};

use crate::{
    composer_ui::{build_composer_with_height, ComposerProps},
    theme,
    view::ViewState,
};

const MAX_CONTENT_WIDTH: f32 = 800.0;
const MIN_GUTTER: f32 = 24.0;

fn content_width(available: f32) -> f32 {
    (available - MIN_GUTTER * 2.0).clamp(0.0, MAX_CONTENT_WIDTH)
}

#[cfg(test)]
fn build_home_with(inputs: &ComposerProps<'_>, available_width: f32) -> Element {
    build_home_with_height(inputs, available_width, None)
}

fn build_home_with_height(
    inputs: &ComposerProps<'_>,
    available_width: f32,
    available_height: Option<f32>,
) -> Element {
    build_home_layout(inputs, available_width, available_height, None)
}

fn build_home_layout(
    inputs: &ComposerProps<'_>,
    available_width: f32,
    available_height: Option<f32>,
    below: Option<Element>,
) -> Element {
    let width = content_width(available_width);
    let compact = width < 470.0;
    // The window places Environment and panel controls over the home surface's
    // first 48 px. A maximized terminal must not make the editor overlap them.
    let content_height = available_height.map(|height| (height - 48.0).max(0.0));
    let show_title = available_height.is_none_or(|height| height >= 360.0);
    let title_height = if compact { 36.0 } else { 44.0 };
    let gap = if compact { 20.0 } else { 32.0 };
    let below_height = if below.is_some() {
        CLI_ENTRY_HEIGHT + 12.0
    } else {
        0.0
    };
    let card_height = content_height.map(|height| {
        (height - below_height - if show_title { title_height + gap } else { 0.0 }).max(0.0)
    });
    let card = build_composer_with_height(&ComposerProps { width, ..*inputs }, card_height);

    let title = theme::text_px(
        if compact {
            "Nueva tarea"
        } else {
            "¿Qué tenemos en la agenda hoy?"
        },
        if compact { 25.0 } else { 32.0 },
        theme::FOREGROUND,
    );
    let mut centered_title = title.centered_x();
    if available_height.is_some() {
        centered_title = centered_title.size(None, Some(title_height));
    }
    let card = match below {
        Some(below) => Element::column(vec![card, below]).gap(12.0),
        None => card,
    };
    let children = if show_title {
        vec![centered_title, card]
    } else {
        vec![card]
    };
    let content = Element::column(children)
        .gap(if show_title { gap } else { 0.0 })
        .size(Some(width), None);

    let centered_content = content.centered();
    if let Some(height) = content_height {
        // The editor itself shrinks first. Exceptional chrome (attachments or
        // an error notice) can still exceed the minimum: keep it reachable by
        // scrolling and keyboard focus instead of clipping its action row.
        Element::column(vec![
            Element::spacer().size(None, Some(48.0)),
            ScrollArea::new("home_content", centered_content.min_size(0.0, height))
                .render()
                .flex(1.0),
        ])
        .flex(1.0)
    } else {
        centered_content.flex(1.0)
    }
}

#[cfg(test)]
pub fn build_home(state: &ViewState, available_width: f32) -> Element {
    let props = ComposerProps::from_state(
        state,
        content_width(available_width),
        true,
        Some("home_workspace_settings"),
        "Pregúntale a openADE",
        None,
    );
    build_home_with(&props, available_width)
}

pub(crate) fn build_home_sized(
    state: &ViewState,
    available_width: f32,
    available_height: f32,
) -> Element {
    let props = ComposerProps::from_state(
        state,
        content_width(available_width),
        true,
        Some("home_workspace_settings"),
        "Pregúntale a openADE",
        None,
    );
    build_home_layout(
        &props,
        available_width,
        Some(available_height.max(0.0)),
        cli_entry(state),
    )
}

/// Starts a task that runs an installed agent CLI, or an agent over ACP,
/// instead of the native agent.
fn cli_entry(state: &ViewState) -> Option<Element> {
    let installed: Vec<_> = state
        .agent_clis
        .iter()
        .filter(|agent| agent.resolved.is_some())
        .collect();
    if installed.is_empty() && state.acp_agents.is_empty() {
        return None;
    }
    let mut row = vec![
        Element::spacer().flex(1.0),
        Element::truncated_text("O trabaja con")
            .font_family(openide_ui::FontFamily::Sans)
            .font_size(12.0)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(None, Some(CLI_ENTRY_HEIGHT)),
    ];
    row.extend(installed.into_iter().map(|agent| {
        let button = openide_ui::Button::new(
            format!("cli_task:{}", agent.id),
            &agent.label,
            theme::component_palette(),
        )
        .icon(crate::provider_icons::icon_for_agent_cli(&agent.id))
        .variant(openide_ui::ButtonVariant::Ghost)
        .size(openide_ui::ControlSize::Compact)
        .tooltip(format!("Nueva tarea con {} en una terminal", agent.label));
        openide_ui::Component::render(button)
    }));
    row.extend(state.acp_agents.iter().map(|agent| {
        let button = openide_ui::Button::new(format!("acp_task:{}", agent.id), &agent.name, theme::component_palette())
            .icon(crate::provider_icons::icon_for_acp_agent(&agent.name))
            .variant(openide_ui::ButtonVariant::Ghost)
            .size(openide_ui::ControlSize::Compact)
            .tooltip(format!("Nueva tarea con {} en la conversación", agent.name));
        openide_ui::Component::render(button)
    }));
    row.push(Element::spacer().flex(1.0));
    Some(Element::row(row).gap(8.0).size(None, Some(CLI_ENTRY_HEIGHT)))
}

const CLI_ENTRY_HEIGHT: f32 = 28.0;

#[cfg(test)]
#[path = "home_visual_tests.rs"]
mod visual_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Key, Size, UiAction, UiEvent, UiTree};

    fn inputs() -> ComposerProps<'static> {
        ComposerProps {
            check_failure: None,
            pause_line: None,
            keymap_profile: crate::keymap::KeymapProfile::default(),
            embedded_in_ide: false,
            environment: None,
            workspace: "C:/work/project",
            native_context: None,
            context_compaction: Default::default(),
            context_open: false,
            project_action: Some("home_workspace_settings"),
            project_branch: Some("main"),
            branch_picker: None,
            branch_menu_open: false,
            project_menu_open: false,
            project_query: "",
            project_options: &[],
            prompt: "Tarea",
            attachments: &[],
            attachment_visuals: None,
            attachment_loading: false,
            add_menu_open: false,
            dictation: openide_ui::DictationState::Idle,
            transcript: "",
            placeholder: "Pide al agente que explore, construya o resuelva algo…",
            selected_backend: "native",
            native_provider_id: "openai",
            model_label: "GPT-6 Sol",
            model_options: &[],
            model_routes: &[],
            model_metadata: None,
            provider_status: None,
            selected_model: "gpt-6-sol",
            model_menu_open: false,
            model_list_open: false,
            model_query: "",
            collapsed_model_providers: None,
            search_collapsed_model_providers: None,
            advanced_open: false,
            effort_label: "Ultra",
            effort_options: &[],
            selected_effort: "ultra",
            effort_menu_open: false,
            effort_preview: None,
            turn_mode: openide_core::TurnMode::Build,
            permission_controls: None,
            full_access: false,
            suggestions: None,
            running: false,
            pending: None,
            error: None,
            goal: None,
            goal_editor_open: false,
            goal_draft: "",
            native_goal: None,
            width: MAX_CONTENT_WIDTH,
            home: true,
            transcript_at: 0,
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
        assert!(project.rect.y + project.rect.height < prompt.rect.y);
        assert!(submit.rect.y > prompt.rect.y + prompt.rect.height);
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

    #[test]
    fn empty_home_expands_and_shrinks_without_losing_input_focus() {
        let viewport = Size::new(900.0, 700.0);
        let mut props = inputs();
        props.prompt = "";
        let mut tree = UiTree::new(build_home_with(&props, viewport.width));
        let rect = |frame: &openide_ui::Frame, id: &str| {
            frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect
        };
        let empty = tree.layout(viewport);
        let prompt = rect(&empty, "prompt");
        let send = rect(&empty, "run_toggle");
        assert!(prompt.y + prompt.height < send.y);
        assert!((28.0..36.0).contains(&prompt.height));
        assert!(tree.focus("prompt"));
        let draft = "Revisá mi proyecto\nMejorá el composer\nConservá los controles";
        assert_eq!(
            tree.handle_event(UiEvent::Text(draft.into())),
            vec![UiAction::InputChanged {
                id: "prompt".into(),
                value: draft.into(),
            }]
        );
        props.prompt = draft;
        tree.set_root(build_home_with(&props, viewport.width));
        let written = tree.layout(viewport);
        let editor = rect(&written, "prompt");
        assert_eq!(editor.width, prompt.width);
        assert!(editor.height > prompt.height);
        assert!(rect(&written, "run_toggle").y >= editor.y + editor.height);
        assert_eq!(tree.focused_id(), Some("prompt"));
        assert_eq!(tree.input_value("prompt"), Some(draft));
        assert_eq!(tree.input_cursor("prompt"), Some(draft.chars().count()));
        tree.handle_event(UiEvent::Key {
            key: Key::A,
            shift: false,
            ctrl: true,
        });
        tree.handle_event(UiEvent::Key {
            key: Key::Backspace,
            shift: false,
            ctrl: false,
        });
        props.prompt = "";
        tree.set_root(build_home_with(&props, viewport.width));
        let cleared = tree.layout(viewport);
        assert_eq!(rect(&cleared, "prompt"), prompt);
        assert_eq!(tree.focused_id(), Some("prompt"));
        assert_eq!(tree.input_value("prompt"), Some(""));
    }

    #[test]
    fn short_home_keeps_draft_controls_and_metadata_below_header_and_above_terminal() {
        let draft = "Una instrucción larga que debe conservarse\n".repeat(20);
        for width in [360.0, 720.0, 1000.0] {
            for height in [180.0, 240.0, 360.0] {
                {
                    let mut props = inputs();
                    props.prompt = &draft;
                    let mut tree = UiTree::new(build_home_with_height(&props, width, Some(height)));
                    tree.set_reduced_motion(true);
                    let frame = tree.layout(Size::new(width, height));
                    for id in ["prompt", "run_toggle", "home_workspace_settings"] {
                        let rect = frame
                            .regions
                            .iter()
                            .find(|r| r.id == id)
                            .unwrap_or_else(|| {
                                panic!("missing {id} at {width}×{height}")
                            })
                            .rect;
                        assert!(rect.height >= 24.0, "{id}: {rect:?}");
                        assert!(rect.y >= 48.0 - 0.1, "{id} overlaps header: {rect:?}");
                        assert!(rect.y + rect.height <= height + 0.1, "{id}: {rect:?}");
                    }
                    assert_eq!(tree.input_value("prompt"), Some(draft.as_str()));
                    assert!(tree.is_scroll_at_end("home_content", 0.1));
                }
            }
        }
    }

    #[test]
    fn short_home_with_extra_feedback_keeps_submit_reachable_by_scrolling() {
        let mut props = inputs();
        props.error = Some("No se pudo completar la solicitud. Revisá la conexión y volvé a intentarlo cuando el servicio esté disponible.");
        let viewport = Size::new(360.0, 180.0);
        let mut tree = UiTree::new(build_home_with_height(
            &props,
            viewport.width,
            Some(viewport.height),
        ));
        tree.set_reduced_motion(true);
        tree.layout(viewport);
        tree.scroll_to_end("home_content");
        let frame = tree.layout(viewport);
        assert!(tree.scroll_offset("home_content") > 0.0);
        let submit = frame
            .regions
            .iter()
            .find(|r| r.id == "run_toggle")
            .unwrap()
            .rect;
        assert!(submit.height >= 24.0);
        assert!(submit.y >= 48.0 && submit.y + submit.height <= viewport.height + 0.1);
        assert_eq!(tree.input_value("prompt"), Some(props.prompt));
    }
}
