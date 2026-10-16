use super::*;
use crate::subagent_settings::{SubagentLimit, SubagentSettings};
use openide_ui::{NumberInput, NumberInputButtonPlacement};

fn configuration(state: &ViewState, compact: bool) -> Vec<Element> {
    let enabled = state.subagent_settings_error.is_none();
    let mut groups = Vec::new();
    if let Some(error) = &state.subagent_settings_error {
        groups.push(settings_parts::feedback_banner(
            error,
            FeedbackTone::Error,
            None,
            compact,
        ));
        let controls = if state.subagent_settings_reset_pending {
            ButtonGroup::new(vec![
                outline("subagent_settings_reset_cancel", "Cancelar").render(),
                outline(
                    "subagent_settings_reset_confirm",
                    "Guardar copia y restablecer",
                )
                .render(),
            ])
            .stacked(compact)
            .render()
        } else {
            ButtonGroup::new(vec![
                outline("subagent_settings_reload", "Recargar archivo").render(),
                outline("subagent_settings_reset", "Restablecer configuración").render(),
            ])
            .stacked(compact)
            .render()
        };
        groups.push(settings_parts::settings_group("Recuperar configuración", vec![
            settings_parts::settings_row("Conservar la configuración original",
                "Los nuevos turnos quedan bloqueados hasta corregir el archivo. Restablecer guarda una copia del original y vuelve a los límites predeterminados.", controls, compact),
        ]));
    }
    let mut rows = vec![settings_parts::switch_row(
        "subagent_settings_enabled", "Permitir delegación",
        "Permite crear subagentes y enviarles tareas en los próximos turnos de Construir. Los hijos que ya están ejecutando conservan su perfil. Puedes consultar y cancelar su trabajo; recuperarlo requiere un perfil compatible.",
        state.subagent_settings.enabled, enabled, compact,
    )];
    for (limit, title, description) in [
        (SubagentLimit::Concurrent, "Subagentes en ejecución", "Máximo de hijos ejecutando al mismo tiempo en todas las conversaciones de este perfil."),
        (SubagentLimit::Children, "Hijos activos por agente", "Cuántos hijos puede mantener activos cada padre, incluidos los que esperan recuperación."),
        (SubagentLimit::Depth, "Profundidad de delegación", "Cuántas generaciones de hijos puede crear una tarea."),
        (SubagentLimit::Descendants, "Subagentes por tarea raíz", "Máximo total de descendientes registrados para una tarea. Incluye los que ya terminaron."),
        (SubagentLimit::Tasks, "Tareas por agente", "Máximo de hijos que puede crear cada padre. Incluye sus hijos completados."),
    ] {
        let id = format!("subagent_setting:{}", limit.key());
        let control = Element::row(vec![
            NumberInput::new(&id, limit.value(&state.subagent_settings).to_string(), control_palette())
                .min(1.0).max(f64::from(limit.max()))
                .button_placement(NumberInputButtonPlacement::Split)
                .size(ControlSize::Compact).width(132.0)
                .accessibility_label(title)
                .step_labels(format!("Reducir {title}"), format!("Aumentar {title}"))
                .enabled(enabled).render(),
            outline(format!("{id}:reset"), "Restablecer")
                .enabled(enabled && limit.value(&state.subagent_settings) != limit.value(&SubagentSettings::default())).render(),
        ]).gap(6.0).centered_y();
        rows.push(settings_parts::settings_row(title, description, control, compact));
    }
    rows.push(page_note("Aplicación de los cambios",
        "Se guardan para nuevos turnos. Los workers activos conservan sus límites. Recuperar un hijo con otro perfil requiere la revisión de identidad existente; no amplía sus permisos automáticamente.", Icon::InfoCircle));
    groups.push(settings_parts::settings_group("Delegación y límites", rows));
    groups
}

fn catalog(state: &ViewState, compact: bool) -> Vec<Element> {
    let roles = openide_harness::built_in_subagent_roles().iter().map(|role| {
        let (title, description) = match role.name {
            "explorer" => ("Explorador", "Investiga el código y presenta archivos, símbolos y evidencia. Sólo lectura, también al continuarlo."),
            "reviewer" => ("Revisor", "Revisa cambios con contexto propio y busca defectos verificables. Sólo lectura, también al continuarlo."),
            "worker" => ("Ejecutor", "Realiza una tarea concreta y verifica el resultado con las herramientas y permisos del padre."),
            _ => (role.name, role.description),
        };
        settings_parts::value_row(format!("{title} · {}", role.name), description,
            if role.mode() == openide_core::TurnMode::Plan { "Sólo lectura" } else { "Construir" }, compact)
    }).collect();
    let mut groups = vec![settings_parts::settings_group("Roles incorporados", roles)];
    let mut personas = state
        .subagent_personas
        .iter()
        .map(|persona| {
            settings_parts::value_row(
                &persona.name,
                format!("{}\n{}", persona.description, persona.file.display()),
                &persona.origin,
                compact,
            )
        })
        .collect::<Vec<_>>();
    if personas.is_empty() {
        personas.push(page_note("Sin perfiles de plugins",
            "Los plugins activados pueden aportar personas e instrucciones al agente nativo. Sus permisos siguen limitados por el perfil de openADE.", Icon::InfoCircle));
    }
    personas.push(settings_parts::settings_row("Administrar perfiles de plugins",
        "Los perfiles disponibles provienen de los plugins aprobados y activados. El agente puede consultar sus instrucciones; no cambian su modelo ni conceden permisos adicionales.",
        outline("settings_page:plugins", "Ver plugins").icon(Icon::ArrowRight).render(), compact));
    groups.push(settings_parts::settings_group(
        "Perfiles de plugins",
        personas,
    ));
    groups
}

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
    let mut content = configuration(state, compact);
    content.push(settings_parts::settings_group(
        "Subagentes nativos de openADE",
        vec![
            settings_parts::value_row(
                "Conversación seleccionada",
                state
                    .selected_session_title
                    .as_deref()
                    .unwrap_or("Selecciona una conversación para revisar su delegación."),
                format!("{} registrados · {running} en ejecución", saved.len()),
                compact,
            ),
            settings_parts::value_row(
                "Agentes conectados a esta ventana",
                "Subagentes de esta conversación conectados a esta ventana. El estado guardado conserva también los que necesitan recuperación.",
                format!("{live} conectados"),
                compact,
            ),
            page_note(
                "Delegación durante Construir",
                "El agente nativo puede crear hijos y enviarles entradas. Cada hijo conserva su conversación, sus permisos y su recuperación. Plan no ofrece herramientas de delegación.",
                SettingsPage::Subagents.icon(),
            ),
        ],
    ));
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
                "Los hijos nativos usan la política de herramientas configurada por openADE. Cada aprobación y permiso de sesión conserva su propio alcance.",
                outline("settings_page:agent", "Ver permisos").icon(Icon::ArrowRight).render(),
                compact,
            ),
            page_note(
                "Trabajo interrumpido",
                "Abre el subagente para revisar y reanudar su turno. Los efectos externos inciertos requieren verificación; cambiar modelo, instrucciones o permisos puede impedir la recuperación del perfil original.",
                Icon::History,
            ),
        ],
    ));
    content.extend(catalog(state, compact));
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
            final_response: None,
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
        let mut tree = UiTree::new(
            ScrollArea::new(
                "subagent_settings_test_scroll",
                Element::column(page(state, width < 640.0)),
            )
            .render(),
        );
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
            .contains(&UiAction::Clicked { id: id.into() }));
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
            .any(|region| region.id.starts_with("subagent_")
                && !region.id.starts_with("subagent_setting")));
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
        let review = crate::approval_ui::ApprovalTarget {
            owner_session_id: "child-1".into(),
            workspace: state.workspace.clone(),
            child_session_id: Some("child-1".into()),
            request_id: "request-1".into(),
        }
        .decision_action("allow_once");
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
            let mut tree = UiTree::new(
                ScrollArea::new(
                    "subagent_settings_test_scroll",
                    Element::column(page(&state, width < 640.0)),
                )
                .render(),
            );
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

    #[test]
    fn delegation_controls_use_split_inputs_and_keep_roles_and_errors_accessible() {
        let mut state = crate::view::tests::empty_state();
        for width in [360.0, 900.0] {
            let frame = UiTree::new(Element::column(page(&state, width < 640.0)))
                .layout(Size::new(width, 3200.0));
            let text = frame_text(&frame);
            assert!(
                text.contains("explorer") && text.contains("reviewer") && text.contains("worker")
            );
            for limit in SubagentLimit::ALL {
                let id = format!("subagent_setting:{}", limit.key());
                let rect = |suffix: &str| {
                    frame
                        .regions
                        .iter()
                        .find(|r| r.id == format!("{id}{suffix}"))
                        .unwrap()
                        .rect
                };
                let minus = rect(":decrement");
                let value = rect("");
                let plus = rect(":increment");
                assert!(minus.x + minus.width <= value.x + 0.01);
                assert!(value.x + value.width <= plus.x + 0.01);
                assert!(minus.x >= 0.0 && plus.x + plus.width <= width + 0.01);
            }
        }
        state.subagent_settings_error = Some("Configuración dañada".into());
        let frame =
            UiTree::new(Element::column(page(&state, true))).layout(Size::new(360.0, 3200.0));
        assert!(
            frame
                .access
                .iter()
                .find(|node| node.id == "subagent_settings_enabled")
                .unwrap()
                .disabled
        );
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "subagent_settings_reset"));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "subagent_settings_reset_confirm"));
        state.subagent_settings_reset_pending = true;
        let frame =
            UiTree::new(Element::column(page(&state, true))).layout(Size::new(360.0, 3200.0));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "subagent_settings_reset_confirm"));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "subagent_settings_reset_cancel"));
    }
}
