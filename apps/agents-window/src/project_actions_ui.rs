//! Project action controls embedded in the environment card.
use crate::{
    project_actions::{ProjectAction, ProjectActionsState},
    theme, ui_sizing,
};
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, Element, FontFamily, Icon, IconButton, ListRow,
    Text, TextRole, TooltipPlacement, TooltipSpec,
};

fn status_line(saving: bool) -> Element {
    Text::new(
        if saving {
            "Guardando…"
        } else {
            "Cargando…"
        },
        theme::component_palette(),
    )
    .role(TextRole::Micro)
    .muted()
    .render()
}

fn error_line(error: &str) -> Element {
    Text::new(error, theme::component_palette())
        .role(TextRole::Small)
        .ink(theme::DESTRUCTIVE)
        .render()
}

/// The part of an action's row that runs it.
fn launch(action: &ProjectAction, enabled: bool) -> Element {
    let id = format!("project_action_run:{}", action.id);
    // A saving panel keeps its actions in full ink; only the hit target goes.
    ListRow::new(
        &id,
        &action.label,
        theme::action_palette(theme::CARD, theme::FOREGROUND),
    )
    .leading(Element::icon(Icon::Terminal).foreground(theme::FOREGROUND_SUBTLE))
    .subtitle(action.command.lines().next().unwrap_or_default())
    .subtitle_mono(true)
    .title_ink(theme::FOREGROUND)
    .enabled(enabled)
    .tooltip_with(
        TooltipSpec::new(format!(
            "Ejecutar en una terminal nueva\n{}",
            action.command
        ))
        .palette(theme::component_palette()),
    )
    .sizing(ui_sizing::PROJECT_ACTION)
    .render()
}

fn action_row(action: &ProjectAction, width: f32, enabled: bool) -> Element {
    Element::row(vec![
        launch(action, enabled).flex(1.0),
        IconButton::new(
            format!("project_action_edit:{}", action.id),
            Icon::Edit,
            theme::action_palette(theme::CARD, theme::FOREGROUND),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Regular)
        .enabled(enabled)
        .tooltip_with(TooltipSpec::new("Editar acción").placement(TooltipPlacement::Above))
        .render(),
        IconButton::new(
            format!("project_action_delete:{}", action.id),
            Icon::Trash,
            theme::action_palette(theme::CARD, theme::FOREGROUND),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Regular)
        .enabled(enabled)
        .tooltip_with(TooltipSpec::new("Eliminar acción").placement(TooltipPlacement::Above))
        .render(),
    ])
    .gap(2.0)
    .size(Some(width), Some(42.0))
}

fn editor(state: &ProjectActionsState, width: f32) -> Element {
    Element::column(vec![
        Text::new(
            if state.editing_id.is_some() {
                "Editar acción"
            } else {
                "Nueva acción"
            },
            theme::component_palette(),
        )
        .render(),
        theme::text_px("Nombre", 10.5, theme::FOREGROUND_SUBTLE),
        Element::text_input(
            "project_action_label",
            &state.label_draft,
            "Ejecutar proyecto",
        )
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .background(theme::INPUT)
        .foreground(theme::FOREGROUND)
        .border(theme::BORDER, 1.0)
        .radius(6.0)
        .padding(7.0)
        .size(Some(width), Some(32.0)),
        theme::text_px("Comando", 10.5, theme::FOREGROUND_SUBTLE),
        Element::text_input(
            "project_action_command",
            &state.command_draft,
            "npm run dev",
        )
        .font_family(FontFamily::Monospace)
        .font_size(11.0)
        .background(theme::INPUT)
        .foreground(theme::FOREGROUND)
        .border(theme::BORDER, 1.0)
        .radius(6.0)
        .padding(7.0)
        .size(Some(width), Some(34.0)),
        theme::text_px(
            "Una sola línea; puedes usar &&. Se abre una terminal nueva en el proyecto.",
            10.5,
            theme::FOREGROUND_SUBTLE,
        ),
        Element::row(vec![
            Button::new("project_action_save", "Guardar", theme::component_palette())
                .variant(ButtonVariant::Secondary)
                .size(ControlSize::Compact)
                .enabled(
                    !state.saving
                        && !state.loading
                        && !state.label_draft.trim().is_empty()
                        && !state.command_draft.trim().is_empty(),
                )
                .render(),
            crate::environment_ui::interactive_surface(
                Button::new(
                    "project_action_cancel",
                    "Cancelar",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .render(),
            ),
        ])
        .gap(6.0),
    ])
    .gap(5.0)
    .size(Some(width), None)
}

pub(crate) fn build(state: &ProjectActionsState, width: f32) -> Element {
    let width = width.max(160.0);
    let mut rows = Vec::new();
    if state.saving || state.loading {
        rows.push(status_line(state.saving).size(Some(width), Some(20.0)));
    }
    if state.editor_open {
        rows.push(editor(state, width));
    } else {
        for action in &state.actions {
            rows.push(action_row(action, width, !state.saving && !state.loading));
        }
        if state.actions.is_empty() && !state.loading {
            rows.push(
                Text::new(
                    "Guarda los comandos que usas para ejecutar, compilar o probar este proyecto.",
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .muted()
                .render(),
            );
            rows.push(crate::environment_ui::interactive_surface(
                Button::new(
                    "project_action_new",
                    "Crear acción",
                    theme::component_palette(),
                )
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Compact)
                .enabled(!state.saving)
                .render(),
            ));
        }
        let suggestions: Vec<_> = state
            .presets
            .iter()
            .filter(|preset| {
                !state
                    .actions
                    .iter()
                    .any(|action| action.command == preset.command)
            })
            .collect();
        if !suggestions.is_empty() {
            rows.push(
                Text::new("Sugeridas por el proyecto", theme::component_palette())
                    .role(TextRole::Micro)
                    .muted()
                    .render()
                    .padding(3.0),
            );
            for preset in suggestions {
                rows.push(
                    Element::row(vec![
                        Element::column(vec![
                            Text::new(&preset.label, theme::component_palette())
                                .role(TextRole::Small)
                                .render(),
                            Element::truncated_text(&preset.command)
                                .font_family(FontFamily::Monospace)
                                .font_size(10.0)
                                .foreground(theme::FOREGROUND_SUBTLE),
                        ])
                        .gap(1.0)
                        .flex(1.0),
                        crate::environment_ui::interactive_surface(
                            Button::new(
                                format!("project_action_preset:{}", preset.id),
                                "Añadir",
                                theme::component_palette(),
                            )
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Compact)
                            .enabled(!state.saving && !state.loading)
                            .render()
                            .font_size(11.0),
                        ),
                    ])
                    .gap(5.0)
                    .padding(3.0)
                    .size(Some(width), Some(40.0)),
                );
            }
        }
    }
    if let Some(error) = &state.error {
        rows.push(error_line(error));
    }
    Element::column(rows).gap(5.0).size(Some(width), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Size, UiTree};

    #[test]
    fn saved_actions_have_explicit_launch_edit_delete_targets_and_presets_only_add() {
        let mut state = ProjectActionsState::default();
        state.actions.push(ProjectAction {
            id: "mine".into(),
            label: "Build".into(),
            command: "cargo build".into(),
        });
        state.presets.push(ProjectAction {
            id: "cargo-test".into(),
            label: "Test".into(),
            command: "cargo test".into(),
        });
        let frame = UiTree::new(build(&state, 320.0)).layout(Size::new(320.0, 500.0));
        for id in [
            "project_action_run:mine",
            "project_action_edit:mine",
            "project_action_delete:mine",
            "project_action_preset:cargo-test",
        ] {
            assert!(
                frame.regions.iter().any(|region| region.id == id),
                "missing {id}"
            );
        }
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "project_action_run:cargo-test"));
        for region in frame.regions {
            assert!(
                region.rect.x >= 0.0 && region.rect.x + region.rect.width <= 321.0,
                "{} {:?}",
                region.id,
                region.rect
            );
        }
    }

    #[test]
    fn action_editor_exposes_name_and_command_without_launching() {
        let mut state = ProjectActionsState::default();
        state.begin_new();
        let frame = UiTree::new(build(&state, 300.0)).layout(Size::new(300.0, 600.0));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "project_action_label"));
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "project_action_command"));
        assert!(frame
            .regions
            .iter()
            .all(|region| !region.id.starts_with("project_action_run:")));
    }
}
