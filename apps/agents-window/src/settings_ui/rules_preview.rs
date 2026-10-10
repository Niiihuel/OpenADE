use super::*;

pub(super) fn group(state: &ViewState, compact: bool) -> Element {
    let preview = &state.native_exec_rules;
    let mut rows = vec![
        page_note(
            "Evaluar un comando",
            "Introduce el ejecutable y sus argumentos exactos como JSON. La vista consulta el archivo de reglas guardado y conserva los cambios sin guardar del editor.",
            Icon::Terminal,
        ),
        Element::column(vec![
            text("Argumentos (JSON)", theme::FOREGROUND, 12.0),
            TextArea::new(
                "exec_rule_preview_argv",
                &preview.preview_argv,
                "[\"git\",\"push\",\"origin\",\"main\"]",
                control_palette(),
            )
            .height(80.0)
            .render(),
            if compact {
                Element::column(vec![
                    mcp_action_button(
                        "exec_rule_preview_evaluate",
                        "Evaluar reglas guardadas",
                        Icon::Search,
                        !preview.editor_open,
                    ),
                    mcp_action_button(
                        "exec_rule_preview_clear",
                        "Limpiar resultado",
                        Icon::Close,
                        preview.preview.is_some(),
                    ),
                ])
                .gap(6.0)
            } else {
                Element::row(vec![
                    mcp_action_button(
                        "exec_rule_preview_evaluate",
                        "Evaluar reglas guardadas",
                        Icon::Search,
                        !preview.editor_open,
                    ),
                    mcp_action_button(
                        "exec_rule_preview_clear",
                        "Limpiar resultado",
                        Icon::Close,
                        preview.preview.is_some(),
                    ),
                ])
                .gap(6.0)
            },
        ])
        .gap(6.0)
        .padding(10.0),
    ];
    if let Some(report) = &preview.preview {
        let (report, color) = match report {
            Ok(report) => (report.description(), theme::FOREGROUND),
            Err(error) => (
                format!("No se pudo evaluar las reglas guardadas: {error}"),
                theme::DESTRUCTIVE,
            ),
        };
        rows.push(
            Element::column(vec![
                text("Resultado de la revisión", theme::FOREGROUND, 12.0),
                ScrollArea::new(
                    "exec_rule_preview_details",
                    text(report, color, 12.0).font_family(FontFamily::Monospace),
                )
                .render()
                .max_height(220.0),
            ])
            .gap(6.0)
            .padding(10.0),
        );
    }
    rows.push(page_note(
        "Alcance de la revisión",
        "La revisión no ejecuta comandos ni otorga permisos. Las aprobaciones y los permisos de sesión vigentes se comprueban durante la ejecución; el resultado mostrado corresponde al archivo guardado revisado.",
        Icon::Check,
    ));
    settings_parts::settings_group("Vista previa de las reglas", rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_policy_settings::ExecPolicySettings;
    use openide_harness::{ExecDecision, ExecPatternToken, ExecRule};
    use openide_ui::{Point, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn rules_preview_complete_report_and_actions_fit_small_and_large_settings() {
        let mut state = crate::view::tests::empty_state();
        let reason = format!(
            "{}FIN DEL MOTIVO",
            "Razón exacta con Unicode üñ\n".repeat(100)
        );
        let settings = ExecPolicySettings {
            rules: vec![ExecRule::new(
                vec![ExecPatternToken::Literal("git".into())],
                ExecDecision::Deny,
                Some(reason.clone()),
            )
            .unwrap()],
        };
        state.native_exec_rules =
            crate::exec_policy_settings::ExecPolicyUiState::from_settings(&settings);
        state
            .native_exec_rules
            .set_preview_argv("[\"git\",\"push\",\"origen\",\"rama üñ\"]".into());
        state.native_exec_rules.preview_saved(Ok(settings));
        let report = state
            .native_exec_rules
            .preview
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .description();
        assert!(report.contains(&reason));
        for width in [360.0, 900.0] {
            let viewport = Size::new(width, 1200.0);
            let mut tree = UiTree::new(group(&state, width < 640.0));
            let frame = tree.layout(viewport);
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "exec_rule_preview_details"));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|layout| layout.source == report));
            for id in ["exec_rule_preview_evaluate", "exec_rule_preview_clear"] {
                assert!(tree.focus(id));
                let frame = tree.layout(viewport);
                let region = frame.regions.iter().find(|region| region.id == id).unwrap();
                assert!(
                    region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.01,
                    "{id}, {width}: {:?}",
                    region.rect
                );
                let point = Point::new(
                    region.rect.x + region.rect.width / 2.0,
                    region.rect.y + region.rect.height / 2.0,
                );
                assert_eq!(frame.hit_test(point), Some(id));
                tree.handle_event(UiEvent::PointerDown(point));
                assert!(tree
                    .handle_event(UiEvent::PointerUp(point))
                    .contains(&UiAction::Clicked { id: id.into() }));
            }
        }
    }

    #[test]
    fn saved_rule_pattern_is_complete_and_scrollable_in_settings() {
        let root = tempfile::tempdir().unwrap();
        let argument = format!("{}FIN_DEL_ARGUMENTO", "argumento_".repeat(80));
        let settings = ExecPolicySettings {
            rules: vec![ExecRule::new(
                vec![
                    ExecPatternToken::Literal("git".into()),
                    ExecPatternToken::Literal(argument),
                ],
                ExecDecision::Ask,
                None,
            )
            .unwrap()],
        };
        settings.save(root.path()).unwrap();
        let saved = ExecPolicySettings::load(root.path()).unwrap();
        let pattern = serde_json::to_string(saved.rules[0].pattern()).unwrap();
        let mut state = crate::view::tests::empty_state();
        state.native_exec_rules =
            crate::exec_policy_settings::ExecPolicyUiState::from_settings(&saved);
        for width in [360.0, 900.0] {
            let viewport = Size::new(width, 1800.0);
            let mut tree = UiTree::new(Element::column(super::super::exec_rules_page(
                &state,
                width < 640.0,
            )));
            let frame = tree.layout(viewport);
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "exec_rule_pattern:0"));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|block| block.source == pattern));
            tree.scroll_to_end("exec_rule_pattern:0");
            let frame = tree.layout(viewport);
            assert!(tree.is_scroll_at_end("exec_rule_pattern:0", 1.0));
            let texts = frame.text_layouts();
            let block = texts.iter().find(|block| block.source == pattern).unwrap();
            assert!(block
                .runs
                .iter()
                .filter(|run| run.visible.is_some())
                .map(|run| run.text.as_str())
                .collect::<String>()
                .contains("FIN_DEL_ARGUMENTO"));
        }
    }
}
