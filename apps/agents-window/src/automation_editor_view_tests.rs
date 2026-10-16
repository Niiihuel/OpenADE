use super::*;
use openide_ui::{Frame, Key, Point, Size, UiAction, UiEvent};

fn state() -> AutomationViewState {
    let mut state = AutomationViewState::new(
        "fixture",
        vec![
            ("native".into(), "Agente nativo".into()),
            ("cli".into(), "CLI local".into()),
        ],
        1_000_000,
    );
    state.open_editor(AutomationForm::new("native"), None);
    state.form.title = "Conservar este borrador".into();
    state.form.prompt = "Revisar cambios sin modificar archivos".into();
    state.form.time = "16:45".into();
    state.models = (0..120)
        .map(|index| {
            (
                "fixture".into(),
                format!("model-{index:03}"),
                format!("Proveedor · Modelo {index:03}"),
            )
        })
        .collect();
    state
}

fn window(state: &AutomationViewState, width: f32) -> Element {
    let base = Element::column(vec![
        Element::button("outside_editor", "Acción del resto de la ventana").size(None, Some(32.0)),
        build_automations(state, width).flex(1.0),
    ]);
    editor_overlay(state, base, width)
}

fn has(frame: &Frame, id: &str) -> bool {
    frame.regions.iter().any(|region| region.id == id)
}

fn key(key: Key) -> UiEvent {
    UiEvent::Key {
        key,
        shift: false,
        ctrl: false,
    }
}

fn export(frame: &Frame, width: f32, name: &str) {
    let Some(directory) = std::env::var_os("OPENIDE_AUTOMATION_VISUAL_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    crate::settings_ui::visual_qa::write_native_frame_svg(
        frame,
        &directory.join(format!("{name}-{width:.0}.svg")),
        width,
        760.0,
    )
    .unwrap();
}

#[test]
fn every_width_uses_one_modal_with_one_step_and_blocks_the_whole_window() {
    for width in [320.0, 360.0, 900.0, 1400.0] {
        let mut state = state();
        for (step, first, absent) in [
            (
                AutomationEditorStep::Content,
                "automation_title",
                "automation_time",
            ),
            (
                AutomationEditorStep::Schedule,
                "automation_time",
                "automation_prompt",
            ),
            (
                AutomationEditorStep::Agent,
                "automation_choice_toggle:model",
                "automation_time",
            ),
        ] {
            state.editor_step = step;
            let base = openide_ui::UiTree::new(build_automations(&state, width))
                .layout(Size::new(width, 760.0));
            assert!(
                !has(&base, "automation_title")
                    && !has(&base, "automation_time")
                    && !has(&base, "automation_choice_toggle:model"),
                "base must never embed fields"
            );
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(window(&state, width));
            let frame = tree.layout(Size::new(width, 760.0));
            assert!(has(&frame, first), "{step:?} at {width}");
            assert!(!has(&frame, absent) && !has(&frame, "automation_save_all"));
            let modal = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:automation_cancel")
                .unwrap()
                .rect;
            assert!(modal.width <= 620.1 && modal.x >= 0.0 && modal.x + modal.width <= width + 0.1);
            let footer = if step == AutomationEditorStep::Agent {
                "automation_save"
            } else {
                "automation_next"
            };
            let action = frame
                .regions
                .iter()
                .find(|region| region.id == footer)
                .unwrap()
                .rect;
            assert!(
                action.y >= modal.y && action.y + action.height <= modal.y + modal.height + 0.1
            );
            for _ in 0..30 {
                tree.handle_event(key(Key::Tab));
                assert_ne!(tree.focused_id(), Some("outside_editor"));
            }
            tree.handle_event(UiEvent::PointerDown(Point::new(8.0, 8.0)));
            assert!(!tree
                .handle_event(UiEvent::PointerUp(Point::new(8.0, 8.0)))
                .iter()
                .any(
                    |action| matches!(action, UiAction::Clicked { id } if id == "outside_editor")
                ));
            assert_eq!(
                tree.handle_event(key(Key::Escape)),
                vec![UiAction::Clicked {
                    id: "automation_cancel".into()
                }]
            );
            if width == 360.0 || width == 900.0 {
                export(&frame, width, &format!("editor-{}", step.index()));
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn schedule_time_is_a_compact_editable_field_and_the_native_calendar_opens_on_demand() {
    for width in [320.0, 900.0] {
        let mut state = state();
        state.editor_step = AutomationEditorStep::Schedule;
        let viewport = Size::new(width, 760.0);
        for frequency in [
            AutomationFrequency::Daily,
            AutomationFrequency::Weekly,
            AutomationFrequency::Monthly,
            AutomationFrequency::Once,
        ] {
            state.form.frequency = frequency;
            state.form.set_once_date("2035-05-17");
            state.calendar_year = 2035;
            state.calendar_month = 5;
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(window(&state, width));
            let frame = tree.layout(viewport);
            if frequency == AutomationFrequency::Weekly {
                export(&frame, width, "weekly");
            }
            let id = if frequency == AutomationFrequency::Once {
                "automation_once_time"
            } else {
                "automation_time"
            };
            let control = frame
                .regions
                .iter()
                .find(|region| region.id == id && region.kind == openide_ui::HitKind::TextInput)
                .expect("editable HH:MM field");
            assert!((control.rect.height - ControlSize::Regular.height()).abs() < 0.1);
            assert!((control.rect.width - 144.0).abs() < 0.1);
            assert!(tree.focus(id));
            let events = tree.handle_event(UiEvent::Text("0".into()));
            assert!(events.iter().any(
                |event| matches!(event, UiAction::InputChanged { id: target, .. } if target == id)
            ));
            assert!(!frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("automation_time:")
                    || region.id.starts_with("automation_once_time:")
                    || region.id.starts_with("automation_once_date:day:")));
            if frequency == AutomationFrequency::Once {
                assert!(has(&frame, "automation_choice_toggle:date"));
                state.editor_menu = Some("date".into());
                tree.set_root(window(&state, width));
                let opened = tree.layout(viewport);
                export(&opened, width, "calendar");
                assert!(has(&opened, "automation_once_date"));
                assert!(opened
                    .regions
                    .iter()
                    .any(|region| region.id.starts_with("automation_once_date:day:")));
                assert_eq!(tree.focused_id(), Some("automation_once_date"));
                assert_eq!(
                    tree.handle_event(key(Key::Escape)),
                    vec![UiAction::Clicked {
                        id: "automation_choice_dismiss".into()
                    }]
                );
            }
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn model_picker_searches_in_a_menu_preserves_indices_and_escapes_before_the_modal() {
    for width in [360.0, 900.0] {
        let mut state = state();
        state.editor_step = AutomationEditorStep::Agent;
        let viewport = Size::new(width, 760.0);
        let (context, mut tree) = crate::view::ide_visual_tests::native_tree(window(&state, width));
        let frame = tree.layout(viewport);
        for id in [
            "automation_choice_toggle:backend",
            "automation_choice_toggle:model",
        ] {
            assert_eq!(
                frame.access.iter().find(|node| node.id == id).unwrap().role,
                openide_ui::Role::ComboBox
            );
        }
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("automation_model:")
                || region.id == "automation_models"));
        assert!(tree.focus("automation_choice_toggle:model"));
        assert_eq!(
            tree.handle_event(key(Key::Enter)),
            vec![UiAction::Clicked {
                id: "automation_choice_toggle:model".into()
            }]
        );
        state.editor_menu = Some("model".into());
        tree.set_root(window(&state, width));
        let opened = tree.layout(viewport);
        export(&opened, width, "models-menu");
        assert_eq!(tree.focused_id(), Some("automation_model_query"));
        state.model_query = "model-119".into();
        tree.set_input_value("automation_model_query", &state.model_query);
        tree.set_root(window(&state, width));
        let filtered = tree.layout(viewport);
        export(&filtered, width, "models-search");
        assert!(has(&filtered, "automation_model:119"));
        assert!(!has(&filtered, "automation_model:0"));
        assert!(tree.focus("automation_model:119"));
        assert_eq!(
            tree.handle_event(key(Key::Enter)),
            vec![UiAction::Clicked {
                id: "automation_model:119".into()
            }]
        );
        assert_eq!(
            tree.handle_event(key(Key::Escape)),
            vec![UiAction::Clicked {
                id: "automation_choice_dismiss".into()
            }]
        );
        state.editor_menu = None;
        tree.set_root(window(&state, width));
        tree.layout(viewport);
        assert_eq!(tree.focused_id(), Some("automation_choice_toggle:model"));
        assert_eq!(
            tree.handle_event(key(Key::Escape)),
            vec![UiAction::Clicked {
                id: "automation_cancel".into()
            }]
        );
        assert_eq!(state.form.title, "Conservar este borrador");
        assert_eq!(state.form.time, "16:45");
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn unchanged_one_off_fields_preserve_the_original_completed_timestamp_exactly() {
    let mut form = AutomationForm::new("codex");
    form.title = "Renombrada".into();
    form.prompt = "Sin cambiar cuándo ocurrió".into();
    let at_ms = 1_000_123;
    form.frequency = AutomationFrequency::Once;
    form.kept_schedule = Some(AutomationSchedule::Once { at_ms });
    form.once_at = Local
        .timestamp_millis_opt(at_ms)
        .single()
        .unwrap()
        .format("%Y-%m-%d %H:%M")
        .to_string();
    assert_eq!(
        form.to_draft(9_000_000).unwrap().schedule,
        AutomationSchedule::Once { at_ms }
    );
    form.set_once_time("00:00");
    assert!(
        form.to_draft(9_000_000).is_err(),
        "a changed past time must still fail validation"
    );
}
