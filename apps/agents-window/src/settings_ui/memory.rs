//! Conversation context is distinct from learned, cross-conversation memory.

use super::*;

fn goal_details(goal: &openide_core::NativeGoal) -> String {
    let mut lines = vec![
        format!("Objetivo: {}", goal.objective),
        format!("ID: {}", goal.id),
    ];
    for criterion in &goal.criteria {
        lines.push(format!(
            "\nCriterio {}: {}",
            criterion.id, criterion.description
        ));
        match &criterion.evidence {
            Some(evidence) => lines.push(format!(
                "Evidencia registrada · turno {}:\n{}",
                evidence.turn_id, evidence.summary
            )),
            None => lines.push("Evidencia pendiente".to_owned()),
        }
    }
    lines.join("\n")
}

pub(super) fn page(state: &ViewState, compact: bool) -> Vec<Element> {
    let selected = state
        .selected_session
        .as_deref()
        .and_then(|id| state.sessions.iter().find(|session| session.id == id));
    let native = selected.filter(|session| session.backend == "native");
    let automation = native.is_some_and(|session| session.native_automation.is_some());
    let native_route = native.is_some() && state.selected_backend == "native";
    let recovery = native.and_then(|session| state.session_recovery.get(&session.id));
    let idle = !state.running && !state.background_run_selected && recovery.is_none();
    let can_operate = native_route && idle;
    let (history_description, history_value) = match selected {
        Some(session) if session.backend == "native" => (
            "Se guardan los turnos y resultados. Los checkpoints conservan un resumen para continuar esta conversación.",
            "Conversación nativa",
        ),
        Some(_) => (
            "Esta conversación usa una CLI externa, que administra su contexto. Los controles nativos requieren una conversación del agente OpenIDE.",
            "Agente externo",
        ),
        None => (
            "Selecciona una conversación para consultar su contexto y continuidad.",
            "Sin conversación",
        ),
    };
    let context_value = if native_route {
        state
            .composer_context_percent
            .map(|percent| format!("{percent}% restante estimado"))
    } else {
        None
    }
    .unwrap_or_else(|| "Sin medición".to_owned());
    let compact_description = if recovery.is_some() {
        "Resuelve la recuperación pendiente antes de resumir el historial de esta conversación."
    } else if !idle {
        "Espera a que termine la ejecución antes de resumir el historial."
    } else {
        "Resume el historial mediante el modelo de la ruta nativa y guarda un checkpoint. Requiere historial y una conexión disponible."
    };
    let mut content = vec![settings_parts::settings_group(
        "Contexto de la conversación",
        vec![
            settings_parts::settings_row(
                "Historial y checkpoints",
                history_description,
                value_badge(history_value),
                compact,
            ),
            settings_parts::settings_row(
                "Contexto restante estimado",
                "Se calcula con el uso de la última respuesta y el límite verificado del modelo exacto. Después de compactar, espera una nueva respuesta para actualizarse.",
                value_badge(context_value),
                compact,
            ),
            settings_parts::settings_row(
                "Modelo y presupuesto",
                "Consulta los límites y el tope de salida de la ruta seleccionada.",
                outline("settings_page:providers", "Ver modelo").icon(Icon::ArrowRight).render(),
                compact,
            ),
            settings_parts::settings_row(
                "Compactar contexto",
                compact_description,
                outline("settings_native_compact", "Compactar")
                    .icon(Icon::ArrowRight)
                    .enabled(can_operate)
                    .render(),
                compact,
            ),
        ],
    )];
    if let Some(recovery) = recovery {
        let description = match recovery {
            view::SessionRecoveryView::InvalidJournal(error) => {
                format!("No se pudo verificar el historial de la conversación: {error}")
            }
            _ => format!("Recuperación de la conversación: {}", recovery.label()),
        };
        content.push(settings_parts::feedback_banner(
            description,
            FeedbackTone::Error,
            native.map(|session| {
                outline(
                    &format!("settings_session_open:{}", session.id),
                    "Ver recuperación",
                )
                .icon(Icon::ArrowRight)
                .enabled(recovery.can_open())
                .render()
            }),
            compact,
        ));
    }
    content.push(settings_parts::settings_group("Instrucciones del proyecto", vec![
        settings_parts::settings_row(
            "Copias guardadas por turno",
            "Revisa las instrucciones adicionales guardadas del proyecto, skills y objetivo. La consulta no vuelve a leer los archivos ni cambia el contexto de un turno.",
            outline(
                &format!("settings_native_instructions_open:{}", state.selected_session.as_deref().unwrap_or("")),
                "Revisar copias",
            )
            .icon(Icon::ArrowRight)
            .enabled(native.is_some())
            .render(),
            compact,
        ),
        page_note("Archivos e instrucciones seleccionadas", "Los nuevos turnos nativos leen AGENTS.md o AGENTS.override.md y las skills activas del proyecto. Antes de consultar al modelo, se guarda una copia exacta de esas instrucciones adicionales; el motor compone también sus propias instrucciones de modo y herramientas.", Icon::Folder),
        page_note("Continuar un turno", "Los turnos con una copia guardada usan esas instrucciones al reanudarse. Cambiar instrucciones o skills afecta los turnos nuevos; no reemplaza las instrucciones guardadas de un turno interrumpido.", Icon::History),
        settings_parts::settings_row("Skills reutilizables", "Administra la selección de instrucciones del proyecto. Esta selección no crea recuerdos aprendidos entre conversaciones.", outline("settings_page:skills", "Ver skills").icon(Icon::ArrowRight).render(), compact),
    ]));

    let goal_scope = native.is_some_and(|session| {
        state.native_goal.session_id.as_deref() == Some(session.id.as_str())
    });
    let goal = goal_scope
        .then_some(state.native_goal.goal.as_ref())
        .flatten();
    let can_open_goal =
        can_operate && !automation && goal_scope && state.native_goal.error.is_none();
    let mut goal_rows = vec![settings_parts::settings_row(
        "Objetivo de esta conversación",
        if automation {
            "Esta automatización conserva su perfil original de sólo lectura. Aquí puedes consultar el estado guardado; configurar o cambiar objetivos requiere una nueva tarea nativa."
        } else {
            "El objetivo nativo conserva criterios, estado y evidencia por conversación. Su editor permite activarlo, pausarlo o reanudarlo explícitamente."
        },
        outline(
            "settings_native_goal_open",
            if goal.is_some() {
                "Ver objetivo"
            } else {
                "Configurar objetivo"
            },
        )
        .icon(Icon::ArrowRight)
        .enabled(can_open_goal)
        .render(),
        compact,
    )];
    if let Some(goal) = goal {
        let status = match goal.status {
            openide_core::NativeGoalStatus::Active => "Activo",
            openide_core::NativeGoalStatus::Paused => "Pausado",
            openide_core::NativeGoalStatus::Complete => "Completado",
        };
        let evidence = goal
            .criteria
            .iter()
            .filter(|criterion| criterion.evidence.is_some())
            .count();
        goal_rows.push(settings_parts::settings_row(
            "Estado guardado",
            format!("Revisión {} · {evidence}/{} criterios con evidencia registrada. La evidencia ligada a un turno completado no verifica por sí sola la satisfacción del criterio.", goal.revision, goal.criteria.len()),
            value_badge(status),
            compact,
        ));
        goal_rows.push(
            ScrollArea::new(
                "settings_native_goal_details",
                text(goal_details(goal), theme::FOREGROUND, 12.0).padding(10.0),
            )
            .appearance(
                ScrollbarAppearance::reserved()
                    .rail_width(14.0)
                    .tint(theme::SETTINGS_FOREGROUND_SUBTLE),
            )
            .render()
            .max_height(if compact { 300.0 } else { 260.0 }),
        );
    } else {
        goal_rows.push(page_note(
            "Objetivo nativo",
            if automation {
                "El perfil original de esta automatización no permite crear, activar, pausar ni reanudar objetivos desde esta conversación."
            } else if goal_scope && state.native_goal.error.is_none() {
                "Esta conversación todavía no tiene un objetivo nativo. Un texto guardado en el composer no se convierte automáticamente en un objetivo con criterios."
            } else {
                "Selecciona una conversación nativa disponible para consultar o configurar su objetivo."
            },
            Icon::Grid,
        ));
    }
    if goal_scope {
        if let Some(error) = &state.native_goal.error {
            goal_rows.push(settings_parts::feedback_banner(
                error,
                FeedbackTone::Error,
                None,
                compact,
            ));
        }
    }
    content.push(settings_parts::settings_group(
        "Objetivos persistentes",
        goal_rows,
    ));
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Point, Size, UiAction, UiEvent, UiTree};

    fn native_state() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.selected_backend = "native".into();
        state.selected_session = Some("selected-native".into());
        state.sessions = vec![crate::view::SessionView {
            id: "selected-native".into(),
            title: "Conversación nativa".into(),
            project: state.workspace.clone(),
            backend: "native".into(),
            native_automation: None,
            pinned: false,
            archived: false,
            unread: false,
        }];
        state.native_goal.session_id = state.selected_session.clone();
        state.composer_context_percent = Some(47);
        state
    }

    fn tree(state: &ViewState, width: f32) -> UiTree {
        UiTree::new(Element::column(page(state, width < 640.0)).gap(16.0))
    }

    fn rendered_texts(tree: &mut UiTree, width: f32) -> Vec<String> {
        tree.layout(Size::new(width, 2200.0))
            .text_layouts()
            .iter()
            .map(|block| block.source.clone())
            .collect()
    }

    fn pointer_action(tree: &mut UiTree, width: f32, id: &str) -> Vec<UiAction> {
        let frame = tree.layout(Size::new(width, 2200.0));
        let Some(region) = frame.regions.iter().find(|region| region.id == id) else {
            return Vec::new();
        };
        let point = Point::new(
            region.rect.x + region.rect.width / 2.0,
            region.rect.y + region.rect.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        tree.handle_event(UiEvent::PointerUp(point))
    }

    #[test]
    fn memory_uses_selected_native_scope_and_disables_unavailable_operations() {
        for width in [360.0, 900.0] {
            let state = native_state();
            let mut native = tree(&state, width);
            assert!(rendered_texts(&mut native, width)
                .iter()
                .any(|text| text == "47% restante estimado"));
            for id in ["settings_native_compact", "settings_native_goal_open"] {
                assert!(native.focus(id));
                assert!(pointer_action(&mut native, width, id).iter().any(|action| matches!(action, UiAction::Clicked { id: action_id } if action_id == id)));
            }
            for scenario in 0..6 {
                let mut unavailable = native_state();
                match scenario {
                    0 => unavailable.selected_session = None,
                    1 => unavailable.sessions[0].backend = "codex".into(),
                    2 => unavailable.selected_backend = "codex".into(),
                    3 => unavailable.running = true,
                    4 => unavailable.background_run_selected = true,
                    _ => {
                        unavailable.session_recovery.insert(
                            "selected-native".into(),
                            crate::view::SessionRecoveryView::Resume,
                        );
                    }
                }
                let mut unavailable_tree = tree(&unavailable, width);
                let texts = rendered_texts(&mut unavailable_tree, width);
                if scenario <= 2 {
                    assert!(!texts.iter().any(|text| text == "47% restante estimado"));
                    assert!(texts.iter().any(|text| text == "Sin medición"));
                }
                for id in ["settings_native_compact", "settings_native_goal_open"] {
                    assert!(!unavailable_tree.focus(id), "{scenario}: {id}");
                    assert!(!pointer_action(&mut unavailable_tree, width, id).iter().any(|action| matches!(action, UiAction::Clicked { id: action_id } if action_id == id)));
                }
            }
            for error in [false, true] {
                let mut state = native_state();
                if error {
                    state.native_goal.error = Some("Ledger ilegible".into());
                } else {
                    state.native_goal.session_id = Some("other-native".into());
                }
                let mut scoped = tree(&state, width);
                rendered_texts(&mut scoped, width);
                assert!(!scoped.focus("settings_native_goal_open"));
                assert!(scoped.focus("settings_native_compact"));
            }
        }
    }

    #[test]
    fn memory_actions_are_reachable_in_real_settings_scroll_at_small_and_large_widths() {
        for width in [360.0, 900.0] {
            let mut state = native_state();
            state.show_settings = true;
            state.settings_page = SettingsPage::Memory;
            let mut tree = UiTree::new(super::super::build_settings_page(
                &state,
                SettingsPage::Memory,
                width,
                width >= 760.0,
            ));
            let mut reached = std::collections::BTreeSet::new();
            // Exercise smooth scrolling on a 120 Hz timeline. A faster layout
            // must not shorten the simulated interaction below its animation.
            let start = std::time::Instant::now();
            for step in 0..120 {
                let now = start + std::time::Duration::from_secs_f64(step as f64 / 120.0);
                let frame = tree.layout_at(Size::new(width, 720.0), now);
                for region in &frame.regions {
                    assert!(
                        region.rect.x >= -0.5 && region.rect.x + region.rect.width <= width + 0.5,
                        "{} at {width}: {:?}",
                        region.id,
                        region.rect
                    );
                    if matches!(
                        region.id.as_str(),
                        "settings_native_goal_open" | "settings_native_compact"
                    ) && region.rect.height > 1.0
                    {
                        let point = Point::new(
                            region.rect.x + region.rect.width / 2.0,
                            region.rect.y + region.rect.height / 2.0,
                        );
                        tree.handle_event_at(UiEvent::PointerDown(point), now);
                        if tree.handle_event_at(UiEvent::PointerUp(point), now).iter().any(
                            |action| matches!(action, UiAction::Clicked { id } if id == &region.id),
                        ) {
                            reached.insert(region.id.clone());
                        }
                    }
                }
                tree.handle_event_at(
                    UiEvent::Scroll {
                        position: Point::new(width * 0.6, 360.0),
                        delta_y: -160.0,
                    },
                    now,
                );
            }
            for id in ["settings_native_compact", "settings_native_goal_open"] {
                assert!(reached.contains(id), "{id} not reachable at {width}px");
            }
        }
    }

    #[test]
    fn memory_recovery_opens_session_without_starting_resume_and_disables_invalid_journal() {
        for width in [360.0, 900.0] {
            for recovery in [
                view::SessionRecoveryView::StartAutomation,
                view::SessionRecoveryView::Resume,
                view::SessionRecoveryView::Permission,
                view::SessionRecoveryView::VerifyTool,
                view::SessionRecoveryView::ReviewBatch,
                view::SessionRecoveryView::InvalidJournal("Diagnóstico completo ñ FIN".into()),
            ] {
                let mut state = native_state();
                state
                    .session_recovery
                    .insert("selected-native".into(), recovery.clone());
                let mut tree = tree(&state, width);
                let texts = rendered_texts(&mut tree, width);
                assert!(!tree.focus("settings_native_compact"));
                let id = "settings_session_open:selected-native";
                assert_eq!(tree.focus(id), recovery.can_open());
                let clicked = pointer_action(&mut tree, width, id);
                assert_eq!(clicked.iter().any(|action| matches!(action, UiAction::Clicked { id: action_id } if action_id == id)), recovery.can_open());
                assert!(!clicked.iter().any(
                    |action| matches!(action, UiAction::Clicked { id } if id == "resume_turn")
                ));
                if let view::SessionRecoveryView::InvalidJournal(error) = recovery {
                    assert!(texts.iter().any(|text| text.contains(&error)));
                }
            }
        }
    }

    #[test]
    fn automation_memory_keeps_goal_history_read_only_without_goal_actions() {
        let goal: openide_core::NativeGoal = serde_json::from_value(serde_json::json!({
            "id": "recorded-goal", "objective": "Objetivo histórico íntegro",
            "status": "paused", "revision": 2, "changes": [],
            "criteria": [{ "id": "recorded-criterion", "description": "Criterio histórico",
                "evidence": { "turn_id": "completed-turn", "summary": "Evidencia histórica íntegra" } }]
        })).unwrap();
        for width in [360.0, 900.0] {
            for saved_goal in [None, Some(goal.clone())] {
                let mut state = native_state();
                state.sessions[0].native_automation =
                    Some(crate::view::NativeAutomationSessionView {
                        automation_id: "987784fa-996e-4f54-83a6-a5c60b652e44".into(),
                        run_id: "987784fa-996e-4f54-83a6-a5c60b652e44:2000000".into(),
                        provider_id: "original-provider".into(),
                        model_id: "original-model".into(),
                        manual: false,
                        scheduled_at_ms: 2_000_000,
                    });
                state.native_goal.goal = saved_goal.clone();
                let mut tree = tree(&state, width);
                let texts = rendered_texts(&mut tree, width);
                assert!(texts
                    .iter()
                    .any(|text| text.contains("perfil original de sólo lectura")));
                assert!(!tree.focus("settings_native_goal_open"));
                assert!(!pointer_action(&mut tree, width, "settings_native_goal_open")
                    .iter().any(|action| matches!(action, UiAction::Clicked { id } if id == "settings_native_goal_open")));
                assert!(tree.focus("settings_native_compact"));
                assert!(tree.focus("settings_native_instructions_open:selected-native"));
                if let Some(goal) = saved_goal {
                    assert!(texts.iter().any(|text| text == &goal_details(&goal)));
                }
                let frame = tree.layout(Size::new(width, 2200.0));
                for region in &frame.regions {
                    assert!(
                        region.rect.x >= -0.5 && region.rect.x + region.rect.width <= width + 0.5,
                        "{} at {width}: {:?}",
                        region.id,
                        region.rect
                    );
                }
                // An unrelated automation must not disable this session's editor.
                state.sessions[0].native_automation = None;
                let mut unrelated = state.sessions[0].clone();
                unrelated.id = "unselected-automation".into();
                unrelated.native_automation = Some(crate::view::NativeAutomationSessionView {
                    automation_id: "unselected-origin".into(),
                    run_id: "unselected-run".into(),
                    provider_id: "other-provider".into(),
                    model_id: "other-model".into(),
                    manual: true,
                    scheduled_at_ms: 2_000_001,
                });
                state.sessions.push(unrelated);
                let mut selected_native = self::tree(&state, width);
                rendered_texts(&mut selected_native, width);
                assert!(selected_native.focus("settings_native_goal_open"));
            }
        }
    }

    #[test]
    fn memory_preserves_complete_goal_criteria_and_evidence_in_scrollable_native_ui() {
        let goal: openide_core::NativeGoal = serde_json::from_value(serde_json::json!({
            "id": "exact-goal-id", "objective": format!("Objetivo ñ\n{}FIN-OBJETIVO", "texto íntegro ".repeat(45)),
            "status": "paused", "revision": 3, "changes": [],
            "criteria": [
                { "id": "criterion-pending", "description": format!("{}FIN-CRITERIO", "criterio completo ".repeat(50)), "evidence": null },
                { "id": "criterion-verified", "description": "Verificar recuperación", "evidence": { "turn_id": "exact-completed-turn", "summary": format!("Evidencia ñ\n{}FIN-EVIDENCIA", "resultado completo ".repeat(60)) } }
            ]
        })).unwrap();
        let expected = goal_details(&goal);
        for width in [360.0, 900.0] {
            let mut state = native_state();
            state.native_goal.goal = Some(goal.clone());
            let mut tree = tree(&state, width);
            let frame = tree.layout(Size::new(width, 2200.0));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|block| block.source == expected));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|block| block.source == "Pausado"));
            for region in &frame.regions {
                assert!(
                    region.rect.x >= -0.5 && region.rect.x + region.rect.width <= width + 0.5,
                    "{} at {width}: {:?}",
                    region.id,
                    region.rect
                );
            }
            let rect = frame
                .regions
                .iter()
                .find(|region| region.id == "settings_native_goal_details")
                .unwrap()
                .rect;
            let start = frame
                .text_layouts()
                .iter()
                .find(|block| block.source == expected)
                .unwrap()
                .runs[0]
                .rect
                .y;
            tree.handle_event(UiEvent::Scroll {
                position: Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0),
                delta_y: -240.0,
            });
            let after = tree.layout(Size::new(width, 2200.0));
            let end = after
                .text_layouts()
                .iter()
                .find(|block| block.source == expected)
                .unwrap()
                .runs[0]
                .rect
                .y;
            assert!(end < start, "details did not scroll at {width}px");
            assert!(pointer_action(&mut tree, width, "settings_native_goal_open").iter().any(|action| matches!(action, UiAction::Clicked { id } if id == "settings_native_goal_open")));
            state.native_goal.session_id = Some("foreign-session".into());
            assert!(!rendered_texts(&mut self::tree(&state, width), width)
                .iter()
                .any(|text| text == &expected));
        }
    }
}
