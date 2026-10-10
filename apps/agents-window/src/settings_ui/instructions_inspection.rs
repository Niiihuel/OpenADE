//! Private journal instruction inspection, scoped to one selected conversation.

use super::*;

pub(crate) fn modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let inspection = &state.native_instructions;
    if !state.show_settings
        || state.settings_page != SettingsPage::Memory
        || !inspection.matches_scope(
            state.selected_session.as_deref(),
            Path::new(&state.workspace),
        )
    {
        return base;
    }
    let width = (viewport_width - 24.0).min(760.0);
    let body_width = (width - 54.0).max(0.0);
    let owner = inspection.session_id.as_deref().expect("scope checked");
    let mut fields = vec![
        text(format!("Conversación: {owner}"), theme::FOREGROUND, 12.0)
            .font_family(FontFamily::Monospace),
        text(
            format!(
                "Proyecto: {}",
                inspection
                    .workspace
                    .as_ref()
                    .expect("scope checked")
                    .display()
            ),
            theme::FOREGROUND,
            12.0,
        )
        .font_family(FontFamily::Monospace),
        subtle(format!(
            "Lectura hasta el evento {} · {} copias adicionales iniciales guardadas",
            inspection.journal_last_sequence,
            inspection.snapshots.len(),
        )),
    ];
    if let Some(error) = &inspection.error {
        fields.push(text(
            "No se pudo validar la conversación",
            theme::FOREGROUND,
            12.0,
        ));
        fields.push(text(error, theme::FOREGROUND, 12.0));
        fields.push(subtle(
            "No se muestran copias de instrucciones sin validar el journal y sus checkpoints.",
        ));
    } else if let Some(snapshot) = inspection.selected_snapshot() {
        let options = inspection
            .snapshots
            .iter()
            .map(|candidate| {
                DropdownOption::new(
                    format!(
                        "settings_native_instructions_select:{}",
                        serde_json::to_string(&(owner, candidate.sequence))
                            .expect("selector serializes"),
                    ),
                    format!(
                        "Evento {} · inicio de turno {}",
                        candidate.sequence, candidate.turn_start_sequence
                    ),
                )
                .selected(candidate.sequence == snapshot.sequence)
            })
            .collect();
        let trigger = outline(
            "settings_native_instructions_selector_toggle",
            format!("Copia del evento {}", snapshot.sequence),
        )
        .trailing_icon(Icon::ChevronDown)
        .width(body_width)
        .render();
        fields.push(
            DropdownList::new(
                trigger,
                options,
                inspection.selector_open,
                "settings_native_instructions_selector_dismiss",
                theme::menu_palette(),
            )
            .width(body_width)
            .placement(PopoverPlacement::BottomStart)
            .render()
            .size(Some(body_width), None),
        );
        fields.push(
            text(snapshot.identity_description(), theme::FOREGROUND, 12.0)
                .font_family(FontFamily::Monospace),
        );
        fields.push(text(
            "Texto completo de esta copia",
            theme::FOREGROUND,
            12.0,
        ));
        if snapshot.content.is_empty() {
            fields.push(subtle("El texto guardado de esta copia está vacío."));
        } else {
            fields.push(
                text(&snapshot.content, theme::FOREGROUND, 12.0).font_family(FontFamily::Monospace),
            );
        }
        fields.push(subtle("Estas copias corresponden a SystemInstructionsCommitted: instrucciones adicionales del host, proyecto, skills y objetivo. El motor agrega su envoltura de workspace, herramientas y modo al preparar la solicitud. Esta consulta no reconstruye esa envoltura ni los ámbitos anidados activados posteriormente por preflight; no representa el contexto completo de cada solicitud."));
    } else {
        fields.push(subtle("Esta conversación todavía no tiene eventos SystemInstructionsCommitted. El journal puede ser anterior a esa capacidad o no haber iniciado una solicitud de modelo. No se inventa una copia a partir de los archivos actuales."));
    }
    fields.push(subtle("Esta consulta muestra copias privadas validadas del journal. Conserva el texto leído al abrir; un turno activo puede registrar eventos después. Cerrar y volver a abrir consulta el journal de nuevo. No carga AGENTS.md ni cambia instrucciones, herramientas o recuperación."));
    let close = outline("settings_native_instructions_dismiss", "Cerrar").render();
    Modal::new(
        base,
        true,
        "Instrucciones guardadas",
        "Instrucciones adicionales iniciales por turno de la conversación seleccionada.",
        "settings_native_instructions_dismiss",
        theme::component_palette(),
    )
    .width(width)
    .height(640.0)
    .body(
        Element::column(fields)
            .gap(10.0)
            .size(Some(body_width), None),
    )
    .footer(Element::row(vec![Element::spacer().flex(1.0), close]).gap(6.0))
    .initial_focus("settings_native_instructions_dismiss")
    .return_focus(format!("settings_native_instructions_open:{owner}"))
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};

    fn inspection_state() -> (ViewState, tempfile::TempDir, String) {
        let root = tempfile::tempdir().unwrap();
        let document = format!(
            "Reglas exactas ñ\n{}FIN-INSTRUCCIONES-終わり",
            "Texto íntegro guardado\n".repeat(100)
        );
        let (store, id) = crate::native_instruction_ui::tests::journal_fixture(
            &root.path().join("data"),
            root.path(),
            &["Copia anterior", &document],
            true,
        );
        let stored = store.load(&id).unwrap().unwrap();
        let mut state = crate::view::tests::empty_state();
        state.workspace = root.path().display().to_string();
        state.selected_session = Some(id);
        state.show_settings = true;
        state.settings_page = SettingsPage::Memory;
        state.native_instructions = crate::native_instruction_ui::tests::validated_state(&stored);
        (state, root, document)
    }

    #[test]
    fn instruction_inspection_selects_one_complete_copy_and_closes_with_scroll_and_focus() {
        for width in [360.0, 900.0] {
            let (mut state, _root, document) = inspection_state();
            let mut tree = UiTree::new(modal(&state, Element::button("behind", "Detrás"), width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 720.0));
            assert_eq!(
                tree.focused_id(),
                Some("settings_native_instructions_dismiss")
            );
            assert!(frame
                .text_layouts()
                .iter()
                .any(|text| text.source == document));
            assert!(!frame
                .text_layouts()
                .iter()
                .any(|text| text.source == "Copia anterior"));
            let close = frame
                .regions
                .iter()
                .find(|region| {
                    region.id == "settings_native_instructions_dismiss"
                        && region.kind == HitKind::Button
                })
                .unwrap()
                .rect;
            let point = Point::new(close.x + close.width / 2.0, close.y + close.height / 2.0);
            assert!(close.x >= 0.0 && close.x + close.width <= width);
            assert!(close.y >= 0.0 && close.y + close.height <= 720.0);
            tree.handle_event(UiEvent::PointerDown(point));
            assert!(tree.handle_event(UiEvent::PointerUp(point)).iter().any(|action| matches!(action, UiAction::Clicked { id } if id == "settings_native_instructions_dismiss")));
            let scroll_id = "modal_body:settings_native_instructions_dismiss";
            let body = frame
                .regions
                .iter()
                .find(|region| region.id == scroll_id && region.kind == HitKind::Scroll)
                .unwrap()
                .rect;
            let mut document_end_visible = false;
            for _ in 0..64 {
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(body.x + body.width / 2.0, body.y + body.height / 2.0),
                    delta_y: -150.0,
                });
                let current = tree.layout(Size::new(width, 720.0));
                document_end_visible |= current.text_layouts().iter().any(|block| {
                    block.source == document
                        && block.runs.iter().any(|run| {
                            run.text.contains("FIN-INSTRUCCIONES") && run.visible.is_some()
                        })
                });
            }
            assert!(tree.scroll_offset(scroll_id) > 0.0);
            assert!(
                document_end_visible,
                "full document end unreachable at {width}px"
            );
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "settings_native_instructions_dismiss".into()
                }]
            );

            state.native_instructions.selected_sequence =
                Some(state.native_instructions.snapshots[0].sequence);
            state.native_instructions.selector_open = true;
            let mut history = UiTree::new(modal(&state, Element::spacer(), width));
            let frame = history.layout(Size::new(width, 720.0));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|text| text.source == "Copia anterior"));
            assert!(!frame
                .text_layouts()
                .iter()
                .any(|text| text.source == document));
            let expected_id = format!(
                "settings_native_instructions_select:{}",
                serde_json::to_string(&(
                    state.selected_session.as_deref().unwrap(),
                    state.native_instructions.snapshots[1].sequence
                ))
                .unwrap()
            );
            let option = frame
                .regions
                .iter()
                .find(|region| region.id == expected_id)
                .unwrap()
                .rect;
            let point = Point::new(
                option.x + option.width / 2.0,
                option.y + option.height / 2.0,
            );
            history.handle_event(UiEvent::PointerDown(point));
            assert!(history
                .handle_event(UiEvent::PointerUp(point))
                .iter()
                .any(|action| matches!(action, UiAction::Clicked { id } if id == &expected_id)));
        }
    }

    #[test]
    fn instruction_inspection_errors_are_visible_and_foreign_scope_is_hidden() {
        let (mut state, _root, _) = inspection_state();
        state.native_instructions.error =
            Some("Digest/version de checkpoint no verificable ñ".into());
        state.native_instructions.snapshots.clear();
        for width in [360.0, 900.0] {
            let frame = UiTree::new(modal(&state, Element::spacer(), width))
                .layout(Size::new(width, 720.0));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|text| text.source == "Digest/version de checkpoint no verificable ñ"));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id == "settings_native_instructions_selector_toggle"));
        }
        state.selected_session = Some("foreign".into());
        let mut foreign = UiTree::new(modal(&state, Element::spacer(), 360.0));
        assert!(!foreign.focus("settings_native_instructions_dismiss"));
        state.selected_session = state.native_instructions.session_id.clone();
        state.workspace = state.workspace.clone() + "/another-project";
        let mut foreign = UiTree::new(modal(&state, Element::spacer(), 360.0));
        assert!(!foreign.focus("settings_native_instructions_dismiss"));
    }
}
