//! Read-only review of an exact local skill catalog observation.

use super::*;

fn provenance(selection: &openide_harness::SkillSelection, label: &str) -> String {
    let provenance = &selection.provenance;
    let origin = match provenance.origin {
        SkillOrigin::Project => "Proyecto",
        SkillOrigin::User => "Usuario",
    };
    format!(
        "{label}\nNombre: {}\nOrigen: {origin}\nRaíz: {}\nDirectorio: {}\nDocumento: {}\nSHA-256: {}",
        selection.name,
        provenance.root.display(),
        provenance.skill_directory.display(),
        provenance.document.display(),
        provenance.sha256_hex,
    )
}

pub(crate) fn modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    if !state.show_settings || state.settings_page != SettingsPage::Skills {
        return base;
    }
    let local = &state.native_skills;
    let Some(entry) = local.preview_entry() else {
        return base;
    };
    let selected = local
        .selected
        .iter()
        .find(|selection| selection.name.eq_ignore_ascii_case(&entry.name));
    let (action_label, status) = match selected {
        Some(selection) if *selection == entry.selection => {
            ("Desactivar", "Selección guardada coincidente")
        }
        Some(_) => (
            "Aceptar cambios",
            "La selección guardada difiere del documento observado",
        ),
        None => ("Seleccionar", "Documento sin seleccionar"),
    };
    let width = (viewport_width - 24.0).min(760.0);
    let mut fields = vec![
        text(&entry.name, theme::FOREGROUND, 12.0),
        subtle(status),
        subtle(
            "Esta vista muestra la lectura realizada al actualizar el catálogo. Revisar no activa instrucciones ni guarda el contenido. Al guardar una selección, el motor vuelve a comprobar documento y hash.",
        ),
        text(
            provenance(&entry.selection, "Documento observado"),
            theme::FOREGROUND,
            12.0,
        )
        .font_family(FontFamily::Monospace),
    ];
    if let Some(selection) = selected {
        fields.push(
            text(
                provenance(selection, "Selección guardada"),
                theme::SETTINGS_FOREGROUND_SUBTLE,
                11.0,
            )
            .font_family(FontFamily::Monospace),
        );
    }
    fields.push(text("SKILL.md completo", theme::FOREGROUND, 12.0));
    fields.push(
        text(&entry.instructions, theme::FOREGROUND, 12.0).font_family(FontFamily::Monospace),
    );
    let close = Button::new("native_skill_preview_close", "Cerrar", control_palette())
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .width(80.0)
        .render();
    let change = Button::new(
        "native_skill_preview_accept",
        action_label,
        control_palette(),
    )
    .icon(Icon::Check)
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .width(160.0)
    .enabled(
        local.load_error.is_none()
            && local.catalog_error.is_none()
            && local.toggle_preview().is_ok(),
    )
    .render();
    let footer = if width < 350.0 {
        Element::column(vec![change, close]).gap(6.0)
    } else {
        Element::row(vec![Element::spacer().flex(1.0), close, change]).gap(6.0)
    };
    Modal::new(
        base,
        true,
        "Revisar skill local",
        "Documento e identidad observados para este proyecto.",
        "native_skill_preview_close",
        theme::component_palette(),
    )
    .width(width)
    .height(640.0)
    .body(
        Element::column(fields)
            .gap(10.0)
            .size(Some((width - 54.0).max(0.0)), None),
    )
    .footer(footer)
    .initial_focus("native_skill_preview_close")
    .return_focus(
        local
            .preview_return_focus
            .as_deref()
            .unwrap_or("native_skill_refresh"),
    )
    .render()
}

pub(crate) fn clear_workspace_modal(
    state: &ViewState,
    base: Element,
    viewport_width: f32,
) -> Element {
    let local = &state.native_skills;
    if !state.show_settings
        || state.settings_page != SettingsPage::Skills
        || !local.clear_workspace_confirm_open
        || local.load_error.is_some()
    {
        return base;
    }
    let Some(snapshot) = local.clear_workspace_snapshot.as_ref() else {
        return base;
    };
    let width = (viewport_width - 24.0).min(560.0);
    // Reviewing multiple complete identities needs more room than a short
    // confirmation. Modal still clamps the card to the viewport and keeps
    // the cancel/confirm footer outside the scrollable body.
    let height = if snapshot.selections.len() > 1 {
        640.0
    } else {
        360.0
    };
    let cancel = Button::new(
        "native_skill_clear_workspace_cancel",
        "Cancelar",
        control_palette(),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .width(88.0)
    .render();
    let clear = Button::new(
        "native_skill_clear_workspace_confirm",
        "Vaciar selección",
        control_palette(),
    )
    .icon(Icon::Trash)
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .width(160.0)
    .render();
    let footer = if width < 350.0 {
        Element::column(vec![clear, cancel]).gap(6.0)
    } else {
        Element::row(vec![Element::spacer().flex(1.0), cancel, clear]).gap(6.0)
    };
    let mut fields = vec![
        text(
            snapshot.workspace.display().to_string(),
            theme::FOREGROUND,
            12.0,
        )
        .font_family(FontFamily::Monospace),
        subtle(format!(
            "{} skills seleccionadas en este proyecto.",
            snapshot.selections.len()
        )),
        subtle(
            "Se quitarán las selecciones mostradas de este proyecto. Los SKILL.md y la selección de otros proyectos se conservan. Los turnos que ya tienen instrucciones guardadas conservan esa copia; puedes volver a revisar y seleccionar documentos para turnos nuevos.",
        ),
    ];
    fields.extend(snapshot.selections.iter().map(|selection| {
        text(
            provenance(selection, "Selección que se quitará"),
            theme::FOREGROUND,
            12.0,
        )
        .font_family(FontFamily::Monospace)
    }));
    Modal::new(
        base,
        true,
        "Vaciar selección local",
        "Confirma el proyecto cuya selección quieres quitar.",
        "native_skill_clear_workspace_cancel",
        theme::component_palette(),
    )
    .width(width)
    .height(height)
    .body(
        Element::column(fields)
            .gap(10.0)
            .size(Some((width - 54.0).max(0.0)), None),
    )
    .footer(footer)
    .initial_focus("native_skill_clear_workspace_cancel")
    .return_focus("native_skill_clear_workspace")
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};

    fn preview_state() -> (ViewState, tempfile::TempDir, String) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let skill = workspace.join(".agents/skills/review/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        let document = format!(
            "---\nname: review\ndescription: Revisar todo\n---\nObjetivo ñ\n{}FIN-SKILL",
            "Documento completo y verificable\n".repeat(90)
        );
        std::fs::write(&skill, &document).unwrap();
        let data = root.path().join("data");
        let settings =
            crate::native_skills_settings::NativeSkillsSettings::load(&data, &workspace).unwrap();
        let mut state = crate::view::tests::empty_state();
        state.workspace = workspace.display().to_string();
        state.show_settings = true;
        state.settings_page = SettingsPage::Skills;
        state.native_skills =
            crate::native_skills_settings::NativeSkillsUiState::from_settings(&settings, &data);
        state.native_skills.open_preview(0).unwrap();
        (state, root, document)
    }

    struct ScrollSamples {
        sources: std::collections::BTreeSet<String>,
        frame: openide_ui::Frame,
        at: std::time::Instant,
    }

    fn scroll_sources(tree: &mut UiTree, width: f32) -> ScrollSamples {
        scroll_sources_for(tree, width, "native_skill_preview_close")
    }

    fn scroll_sources_for(tree: &mut UiTree, width: f32, close_id: &str) -> ScrollSamples {
        scroll_sources_for_viewport(tree, width, 720.0, close_id)
    }

    fn scroll_sources_for_viewport(
        tree: &mut UiTree,
        width: f32,
        height: f32,
        close_id: &str,
    ) -> ScrollSamples {
        let mut sources = std::collections::BTreeSet::new();
        let scroll_id = format!("modal_body:{close_id}");
        // Keep smooth scroll active on a deterministic 120 Hz timeline;
        // optimized builds must exercise the same elapsed animation time.
        let start = std::time::Instant::now();
        for step in 0..120 {
            let now = start + std::time::Duration::from_secs_f64(step as f64 / 120.0);
            let frame = tree.layout_at(Size::new(width, height), now);
            sources.extend(frame.text_layouts().into_iter().map(|block| block.source));
            let body = frame
                .regions
                .iter()
                .find(|region| region.id == scroll_id && region.kind == HitKind::Scroll)
                .unwrap()
                .rect;
            tree.handle_event_at(
                UiEvent::Scroll {
                    position: Point::new(body.x + body.width / 2.0, body.y + body.height / 2.0),
                    // Overlap actual visible pages so font wrapping or a short
                    // viewport cannot skip an entire provenance block between
                    // samples. All complete-source assertions remain required.
                    delta_y: -(body.height * 0.75).clamp(1.0, 300.0),
                },
                now,
            );
        }
        let at = start + std::time::Duration::from_secs(1);
        let frame = tree.layout_at(Size::new(width, height), at);
        sources.extend(frame.text_layouts().into_iter().map(|block| block.source));
        ScrollSamples { sources, frame, at }
    }

    #[test]
    fn clear_skill_workspace_modal_presents_exact_frozen_selections_with_scroll_and_safe_focus() {
        let (mut state, root, _) = preview_state();
        let workspace = std::path::Path::new(&state.workspace);
        for index in 1..16 {
            let path = workspace
                .join(".agents/skills")
                .join(format!("entry-{index:02}"))
                .join("SKILL.md");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("---\nname: entry-{index:02}\ndescription: Exact entry {index}\n---\nInstructions {index}")).unwrap();
        }
        let data = root.path().join("data");
        let settings =
            crate::native_skills_settings::NativeSkillsSettings::load(&data, workspace).unwrap();
        let selected = settings.with_selections(
            settings
                .discover(&data)
                .unwrap()
                .iter()
                .map(|skill| skill.selection())
                .collect(),
        );
        selected.save(&data).unwrap();
        state.native_skills =
            crate::native_skills_settings::NativeSkillsUiState::from_settings(&selected, &data);
        state.native_skills.clear_workspace_confirm_open = true;
        state.native_skills.clear_workspace_snapshot =
            Some(crate::native_skills_settings::NativeSkillsClearSnapshot {
                workspace: selected.workspace.clone(),
                selections: selected.selections.clone(),
            });
        for (width, height) in [
            (360.0, 720.0),
            (900.0, 720.0),
            (360.0, 320.0),
            (900.0, 320.0),
        ] {
            let mut tree = UiTree::new(clear_workspace_modal(
                &state,
                Element::button("background", "Detrás"),
                width,
            ));
            let frame = tree.layout(Size::new(width, height));
            assert_eq!(
                tree.focused_id(),
                Some("native_skill_clear_workspace_cancel")
            );
            let card = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:native_skill_clear_workspace_cancel")
                .unwrap()
                .rect;
            assert!(card.x >= 0.0 && card.x + card.width <= width + 0.1);
            assert!(card.y >= 0.0 && card.y + card.height <= height + 0.1);
            let body = frame
                .regions
                .iter()
                .find(|region| {
                    region.id == "modal_body:native_skill_clear_workspace_cancel"
                        && region.kind == HitKind::Scroll
                })
                .unwrap()
                .rect;
            assert!(body.height > 0.0);
            if height == 720.0 {
                assert!(
                    body.height >= 300.0,
                    "long selection review should use the available card height"
                );
            }
            for id in [
                "native_skill_clear_workspace_cancel",
                "native_skill_clear_workspace_confirm",
            ] {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect;
                assert!(rect.x >= card.x && rect.x + rect.width <= card.x + card.width + 0.1);
                assert!(rect.y >= card.y && rect.y + rect.height <= card.y + card.height + 0.1);
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert!(tree.handle_event(UiEvent::PointerUp(point)).iter().any(
                    |action| matches!(action, UiAction::Clicked { id: clicked } if clicked == id)
                ));
            }
            let sampled = scroll_sources_for_viewport(
                &mut tree,
                width,
                height,
                "native_skill_clear_workspace_cancel",
            );
            assert!(sampled
                .sources
                .contains(&selected.workspace.display().to_string()));
            for selection in &selected.selections {
                assert!(
                    sampled
                        .sources
                        .contains(&provenance(selection, "Selección que se quitará")),
                    "selection omitted from confirmation at {width}px: {}",
                    selection.name
                );
            }
            assert!(tree.scroll_offset("modal_body:native_skill_clear_workspace_cancel") > 0.0);
            assert_eq!(
                tree.handle_event_at(
                    UiEvent::Key {
                        key: Key::Escape,
                        shift: false,
                        ctrl: false
                    },
                    sampled.at
                ),
                vec![UiAction::Clicked {
                    id: "native_skill_clear_workspace_cancel".into()
                }]
            );
        }
    }

    #[test]
    fn skills_preview_preserves_full_document_hash_provenance_and_footer_on_small_surfaces() {
        for width in [360.0, 900.0] {
            let (state, _root, document) = preview_state();
            let observed = provenance(
                &state.native_skills.available[0].selection,
                "Documento observado",
            );
            let mut tree = UiTree::new(modal(
                &state,
                Element::button("background", "Detrás"),
                width,
            ));
            let frame = tree.layout(Size::new(width, 720.0));
            // A long temporary workspace path can wrap the provenance enough
            // to place the document wholly below the initial viewport. The
            // renderer correctly culls it there; verify the exact observation
            // now and its complete rendered source after actual scrolling below.
            assert_eq!(
                state.native_skills.preview_entry().unwrap().instructions,
                document
            );
            assert!(frame
                .text_layouts()
                .iter()
                .any(|block| block.source == observed));
            assert_eq!(tree.focused_id(), Some("native_skill_preview_close"));
            let card = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:native_skill_preview_close")
                .unwrap()
                .rect;
            for id in ["native_skill_preview_close", "native_skill_preview_accept"] {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect;
                assert!(
                    rect.x >= card.x && rect.x + rect.width <= card.x + card.width + 0.1,
                    "{id} at {width}px: control {rect:?}, card {card:?}"
                );
                assert!(
                    rect.y >= card.y && rect.y + rect.height <= card.y + card.height + 0.1,
                    "{id} at {width}px: control {rect:?}, card {card:?}"
                );
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert!(tree.handle_event(UiEvent::PointerUp(point)).iter().any(
                    |action| matches!(action, UiAction::Clicked { id: clicked } if clicked == id)
                ));
            }
            let sampled = scroll_sources(&mut tree, width);
            assert!(tree.scroll_offset("modal_body:native_skill_preview_close") > 0.0);
            assert!(sampled.sources.contains(&document));
            let end = sampled.frame;
            assert!(
                end.text_layouts()
                    .iter()
                    .any(|block| block.source == document
                        && block
                            .runs
                            .iter()
                            .any(|run| run.text.contains("FIN-SKILL") && run.visible.is_some())),
                "document end cannot be reached at {width}px"
            );
            assert_eq!(
                tree.handle_event_at(
                    UiEvent::Key {
                        key: Key::Escape,
                        shift: false,
                        ctrl: false
                    },
                    sampled.at
                ),
                vec![UiAction::Clicked {
                    id: "native_skill_preview_close".into()
                }]
            );
        }
    }

    #[test]
    fn skills_preview_shows_both_exact_hashes_for_changed_selected_document() {
        let (mut state, root, original) = preview_state();
        let data = root.path().join("data");
        let workspace = std::path::Path::new(&state.workspace);
        let settings = crate::native_skills_settings::NativeSkillsSettings::load(&data, workspace)
            .unwrap()
            .with_selections(state.native_skills.toggle_preview().unwrap());
        settings.save(&data).unwrap();
        let saved = settings.selections[0].clone();
        let updated = original.replace("FIN-SKILL", "FIN-NUEVO");
        std::fs::write(&saved.provenance.document, &updated).unwrap();
        state.native_skills =
            crate::native_skills_settings::NativeSkillsUiState::from_settings(&settings, &data);
        state.native_skills.open_selected_preview(0).unwrap();
        assert_ne!(state.native_skills.preview.as_ref().unwrap(), &saved);
        for width in [360.0, 900.0] {
            let mut tree = UiTree::new(modal(&state, Element::spacer(), width));
            let sampled = scroll_sources(&mut tree, width);
            for source in [
                updated.clone(),
                provenance(&saved, "Selección guardada"),
                provenance(
                    &state.native_skills.available[0].selection,
                    "Documento observado",
                ),
            ] {
                assert!(
                    sampled.sources.contains(&source),
                    "Missing complete {source}"
                );
            }
            let frame = sampled.frame;
            assert!(frame.commands.iter().any(|command| matches!(command, openide_ui::DrawCommand::Text { text, .. } if text == "Aceptar cambios")), "changed-document acceptance label is not visible at {width}px");
            assert!(tree.focus("native_skill_preview_accept"));
            state.native_skills.catalog_error = Some("Catálogo ilegible".into());
            let mut disabled = UiTree::new(modal(&state, Element::spacer(), width));
            disabled.layout(Size::new(width, 720.0));
            assert!(!disabled.focus("native_skill_preview_accept"));
            assert!(disabled.focus("native_skill_preview_close"));
            state.native_skills.catalog_error = None;
        }
        state.show_settings = false;
        let frame = UiTree::new(modal(
            &state,
            Element::button("background", "Detrás"),
            360.0,
        ))
        .layout(Size::new(360.0, 720.0));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "native_skill_preview_accept"));
    }

    #[test]
    fn skills_preview_buttons_are_reachable_from_catalog_and_selection_with_focus_return() {
        for width in [360.0, 900.0] {
            let (mut state, _root, _) = preview_state();
            state.native_skills.selected = vec![state.native_skills.available[0].selection.clone()];
            state.native_skills.close_preview();
            let base = || {
                super::super::build_settings_page(
                    &state,
                    SettingsPage::Skills,
                    width,
                    width >= 760.0,
                )
            };
            let mut tree = UiTree::new(base());
            tree.layout(Size::new(width, 720.0));
            for id in ["native_skill_preview:0", "native_skill_preview_selected:0"] {
                assert!(tree.focus(id));
                let frame = tree.layout(Size::new(width, 720.0));
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect;
                assert!(rect.x >= 0.0 && rect.x + rect.width <= width + 0.1);
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                tree.handle_event(UiEvent::PointerDown(point));
                assert!(tree.handle_event(UiEvent::PointerUp(point)).iter().any(
                    |action| matches!(action, UiAction::Clicked { id: clicked } if clicked == id)
                ));
            }
            state.native_skills.open_selected_preview(0).unwrap();
            tree.set_root(modal(
                &state,
                super::super::build_settings_page(
                    &state,
                    SettingsPage::Skills,
                    width,
                    width >= 760.0,
                ),
                width,
            ));
            tree.layout(Size::new(width, 720.0));
            assert_eq!(tree.focused_id(), Some("native_skill_preview_close"));
            state.native_skills.close_preview();
            tree.set_root(super::super::build_settings_page(
                &state,
                SettingsPage::Skills,
                width,
                width >= 760.0,
            ));
            tree.layout(Size::new(width, 720.0));
            assert_eq!(tree.focused_id(), Some("native_skill_preview_selected:0"));
        }
    }
}
