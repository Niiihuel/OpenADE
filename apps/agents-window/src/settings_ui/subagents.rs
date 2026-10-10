use super::*;

pub(super) fn page(state: &ViewState, compact: bool) -> Vec<Element> {
    let selected = state.selected_session.as_deref();
    let related = |child: &str, parent: &str| selected == Some(child) || selected == Some(parent);
    let saved = state
        .subagent_sessions
        .iter()
        .filter(|child| related(&child.child_session_id, &child.parent_session_id))
        .collect::<Vec<_>>();
    let live = state
        .active_subagents
        .iter()
        .filter(|child| related(&child.child_session_id, &child.parent_session_id))
        .count();
    let running = saved
        .iter()
        .filter(|child| child.status == openide_harness::SubagentStatus::Running)
        .count();
    let mut content = vec![settings_parts::settings_group(
        "Subagentes nativos de OpenIDE",
        vec![
            settings_parts::settings_row(
                "Conversación seleccionada",
                state
                    .selected_session_title
                    .as_deref()
                    .unwrap_or("Selecciona una conversación para revisar su delegación."),
                value_badge(format!("{} registrados · {running} en ejecución", saved.len())),
                compact,
            ),
            settings_parts::settings_row(
                "Agentes conectados a esta ventana",
                "Subagentes de esta conversación conectados a esta ventana. El estado guardado conserva también los que necesitan recuperación.",
                value_badge(format!("{live} conectados")),
                compact,
            ),
            page_note(
                "Delegación durante Construir",
                "El agente nativo puede crear hijos y enviarles entradas. Cada hijo conserva su conversación, sus permisos y su recuperación. Plan no ofrece herramientas de delegación.",
                SettingsPage::Subagents.icon(),
            ),
        ],
    )];
    if let Some(cards) = crate::session_surface::related_subagent_cards(state, compact) {
        content.push(settings_parts::settings_group(
            "Estado, permisos y entradas",
            vec![cards],
        ));
    } else {
        content.push(settings_parts::empty_state(
            if selected.is_some() {
                "Sin subagentes en esta conversación"
            } else {
                "Selecciona una conversación"
            },
            "Aquí aparecen el registro guardado y los controles de sus subagentes relacionados.",
            None,
        ));
    }
    content.push(settings_parts::settings_group(
        "Permisos y recuperación",
        vec![
            settings_parts::settings_row(
                "Herramientas de los hijos",
                "Los hijos nativos usan la política de herramientas configurada por OpenIDE. Cada aprobación y permiso de sesión conserva su propio alcance.",
                outline("settings_page:agent", "Ver permisos").icon(Icon::ArrowRight).render(),
                compact,
            ),
            page_note(
                "Trabajo interrumpido",
                "Abre el subagente para revisar y reanudar su turno. Los efectos externos inciertos requieren verificación; cambiar modelo, instrucciones o permisos puede impedir la recuperación del perfil original.",
                Icon::History,
            ),
            page_note(
                "Configuración del motor",
                "El motor administra el parentesco y los límites de delegación. Settings todavía no permite editar perfiles especializados ni límites de concurrencia.",
                Icon::Settings,
            ),
        ],
    ));
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{SubagentPermissionView, SubagentRunView, SubagentSessionView};
    use openide_harness::{SubagentInputSnapshot, SubagentInputStatus, SubagentStatus};
    use openide_ui::{DrawCommand, Point, Size, UiAction, UiEvent, UiTree};

    fn child(status: SubagentStatus) -> SubagentSessionView {
        SubagentSessionView {
            child_session_id: "child-1".into(),
            parent_session_id: "parent-1".into(),
            title: "Tarea nativa persistida".into(),
            status,
            cancellation_requested: false,
            open_inputs: 0,
            inputs: vec![],
            dispatch_error: None,
            depth: 1,
        }
    }

    fn frame_text(frame: &openide_ui::Frame) -> String {
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn assert_pointer_action(state: &ViewState, width: f32, id: &str) {
        let mut tree = UiTree::new(Element::column(page(state, width < 640.0)));
        let viewport = Size::new(width, 1500.0);
        tree.layout(viewport);
        assert!(tree.focus(id), "missing focus target {id} at {width}");
        let frame = tree.layout(viewport);
        let region = frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap_or_else(|| panic!("missing reachable {id} at {width}"));
        assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.01);
        let position = Point::new(
            region.rect.x + region.rect.width / 2.0,
            region.rect.y + region.rect.height / 2.0,
        );
        assert_eq!(frame.hit_test(position), Some(id));
        tree.handle_event(UiEvent::PointerDown(position));
        assert!(tree
            .handle_event(UiEvent::PointerUp(position))
            .contains(&UiAction::Clicked {
                id: if id.starts_with("approval_review:") {
                    id.strip_suffix(":inline").unwrap_or(id).into()
                } else {
                    id.into()
                }
            }));
    }

    #[test]
    fn settings_subagents_scope_live_counts_and_navigate_parent_or_child() {
        let mut state = crate::view::tests::empty_state();
        state.subagent_sessions = vec![child(SubagentStatus::Running)];
        state.active_subagents = vec![
            SubagentRunView {
                child_session_id: "child-1".into(),
                parent_session_id: "parent-1".into(),
                title: "Trabajo relacionado".into(),
            },
            SubagentRunView {
                child_session_id: "foreign-child".into(),
                parent_session_id: "foreign-parent".into(),
                title: "Trabajo ajeno".into(),
            },
        ];
        for (selected, target) in [("parent-1", "child-1"), ("child-1", "parent-1")] {
            state.selected_session = Some(selected.into());
            for width in [360.0, 900.0] {
                let frame = UiTree::new(Element::column(page(&state, width < 640.0)))
                    .layout(Size::new(width, 1500.0));
                let text = frame_text(&frame);
                assert!(text.contains("1 registrados · 1 en ejecución"), "{text}");
                assert!(text.contains("1 conectados"), "{text}");
                assert!(!text.contains("Trabajo ajeno"));
                assert_pointer_action(&state, width, &format!("subagent_open:{target}"));
                assert_pointer_action(&state, width, "subagent_cancel:child-1");
            }
        }
        state.selected_session = Some("unrelated".into());
        let frame =
            UiTree::new(Element::column(page(&state, true))).layout(Size::new(360.0, 1500.0));
        let text = frame_text(&frame);
        assert!(text.contains("0 registrados · 0 en ejecución"), "{text}");
        assert!(text.contains("0 conectados"), "{text}");
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("subagent_")));
    }

    #[test]
    fn settings_subagents_reuse_permissions_recovery_and_pending_input_controls() {
        let mut state = crate::view::tests::empty_state();
        state.selected_session = Some("child-1".into());
        state.pending_subagent_permissions = vec![SubagentPermissionView {
            child_session_id: "child-1".into(),
            parent_session_id: "parent-1".into(),
            request_id: "request-1".into(),
            tool_name: "write_file".into(),
            arguments: "{\"path\":\"src/lib.rs\"}".into(),
        }];
        state.recoverable_subagent = Some(SubagentRunView {
            child_session_id: "child-1".into(),
            parent_session_id: "parent-1".into(),
            title: "Trabajo interrumpido".into(),
        });
        state.expanded_subagent_inputs = Some("child-1".into());
        let mut saved = child(SubagentStatus::Interrupted);
        saved.open_inputs = 1;
        saved.inputs = vec![SubagentInputSnapshot {
            id: "input:1".into(),
            prompt: "Revisar recuperación\nGuardar evidencia exacta".into(),
            mode: openide_core::TurnMode::Build,
            status: SubagentInputStatus::Pending,
            revision: 4,
        }];
        state.subagent_sessions = vec![saved];
        let review = format!(
            "{}:inline",
            crate::approval_ui::ApprovalTarget {
                owner_session_id: "child-1".into(),
                workspace: state.workspace.clone(),
                child_session_id: Some("child-1".into()),
                request_id: "request-1".into(),
            }
            .review_action()
        );
        for width in [360.0, 900.0] {
            for id in [
                review.as_str(),
                "subagent_resume:child-1",
                "subagent_inputs:child-1",
                "subagent_input_edit:child-1:input:1",
                "subagent_input_skip:child-1:4:input:1",
            ] {
                assert_pointer_action(&state, width, id);
            }
            let mut tree = UiTree::new(Element::column(page(&state, width < 640.0)));
            let viewport = Size::new(width, 1500.0);
            tree.layout(viewport);
            assert!(tree.focus("subagent_input_edit:child-1:input:1"));
            let frame = tree.layout(viewport);
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "related_subagents"));
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "subagent_mailbox:child-1"));
        }
    }

    #[test]
    fn settings_subagents_project_saved_lifecycle_and_offer_only_supported_dispatch() {
        for (status, label, inputs, dispatch) in [
            (SubagentStatus::Ready, "Pendiente", 0, true),
            (SubagentStatus::Running, "En ejecución", 0, false),
            (SubagentStatus::Interrupted, "Interrumpido", 1, false),
            (SubagentStatus::Completed, "Completado", 0, false),
            (SubagentStatus::Completed, "Completado", 1, true),
            (SubagentStatus::Failed, "Fallido", 1, false),
            (SubagentStatus::Cancelled, "Cancelado", 1, false),
        ] {
            let mut state = crate::view::tests::empty_state();
            state.selected_session = Some("parent-1".into());
            let mut saved = child(status);
            saved.open_inputs = inputs;
            state.subagent_sessions = vec![saved];
            for width in [360.0, 900.0] {
                let frame = UiTree::new(Element::column(page(&state, width < 640.0)))
                    .layout(Size::new(width, 1500.0));
                let text = frame_text(&frame);
                assert!(text.contains(label), "{status:?}, {width}: {text}");
                assert_eq!(
                    frame
                        .regions
                        .iter()
                        .any(|region| region.id == "subagent_dispatch:child-1"),
                    dispatch,
                    "{status:?}, {width}"
                );
                if dispatch {
                    assert_pointer_action(&state, width, "subagent_dispatch:child-1");
                }
            }
            state.subagent_sessions[0].cancellation_requested = true;
            let frame =
                UiTree::new(Element::column(page(&state, true))).layout(Size::new(360.0, 1500.0));
            let text = frame_text(&frame);
            assert!(text.contains(label), "{text}");
            assert!(text.contains("Cancelación solicitada"), "{text}");
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "subagent_dispatch:child-1"));
        }
    }
}
