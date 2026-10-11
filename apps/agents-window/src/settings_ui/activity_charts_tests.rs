use super::*;
use eframe::egui;
use openide_ui::{DrawCommand, FontFamily, Frame, HitKind, Point, Rect, Size, UiEvent, UiTree};
use std::time::{Duration, Instant};

fn session(id: usize, backend: &str) -> SessionView {
    SessionView {
        id: id.to_string(),
        title: format!("Tarea {id}"),
        project: "/fixture".into(),
        backend: backend.into(),
        native_automation: None,
        pinned: false,
        archived: false,
        unread: false,
    }
}

fn fixture(backends: &[&str]) -> ViewState {
    let mut state = crate::view::tests::empty_state();
    state.sessions = backends
        .iter()
        .enumerate()
        .map(|(id, backend)| session(id, backend))
        .collect();
    state.backend_options = vec![
        ("native".into(), "Nativo".into()),
        ("codex".into(), "Codex CLI".into()),
        ("custom".into(), "Agente personalizado".into()),
    ];
    state
}

fn text_in(frame: &Frame) -> String {
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

fn bars(frame: &Frame) -> Vec<Rect> {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::FillRect { rect, color, .. }
            | DrawCommand::FillCornerRect { rect, color, .. }
                if *color == TASK_BAR_COLOR =>
            {
                Some(*rect)
            }
            _ => None,
        })
        .collect()
}

fn native_tree(state: &ViewState, width: f32) -> (egui::Context, UiTree) {
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, 520.0),
        )),
        ..Default::default()
    });
    let measure = ctx.clone();
    let mut tree = UiTree::new(tasks_card(state, width));
    tree.set_reduced_motion(true);
    tree.set_text_measurer(move |value, size, family| {
        measure.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    value.into(),
                    egui::FontId::new(
                        size,
                        match family {
                            FontFamily::Sans | FontFamily::SansItalic => egui::FontFamily::Proportional,
                            FontFamily::Monospace => egui::FontFamily::Monospace,
                        },
                    ),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
    });
    (ctx, tree)
}

#[test]
fn task_counts_preserve_recorded_agents_and_sort_ties_independently_of_history_order() {
    let mut sessions = vec![
        session(1, "zeta"),
        session(2, "alpha"),
        session(3, "native"),
        session(4, "native"),
    ];
    sessions[0].archived = true; // Count the supplied view, without silently filtering its history again.
    let names = vec![
        ("native".into(), "Agente Rust".into()),
        ("alpha".into(), String::new()),
    ];
    let data = task_data(&sessions, &names);
    sessions.reverse();
    assert_eq!(data, task_data(&sessions, &names));
    assert_eq!(
        data.iter()
            .map(|datum| datum.id.as_str())
            .collect::<Vec<_>>(),
        ["native", "alpha", "zeta"]
    );
    assert_eq!(data.iter().map(|datum| datum.value).sum::<f64>(), 4.0);
    assert_eq!(data[0].label, "Agente Rust");
    assert_eq!(
        data[1].label, "alpha",
        "an empty configured display name cannot hide an observed agent"
    );
    assert!(data[0].tooltip.as_ref().unwrap().contains("2 tareas"));
    assert!(data[1].tooltip.as_ref().unwrap().contains("1 tarea en"));
}

#[test]
fn task_columns_keep_real_totals_zero_baseline_and_proportions_at_native_font_widths() {
    let state = fixture(&["native", "native", "native", "codex", "codex", "custom"]);
    for width in [280.0, 360.0, 900.0] {
        let (ctx, mut tree) = native_tree(&state, width);
        let frame = tree.layout(Size::new(width, 520.0));
        let text = text_in(&frame);
        assert!(text.contains("Tareas por agente"));
        assert!(text.contains("6 tareas visibles"));
        assert!(text.contains("3 agentes"));
        assert!(
            frame
                .commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Text { text, .. } if text == "0")),
            "the numeric scale starts at zero"
        );
        let painted = bars(&frame);
        assert_eq!(painted.len(), 3);
        for bar in &painted {
            assert!(bar.width > 0.0 && bar.width <= 48.0);
            assert!(bar.height > 0.0 && bar.x >= 16.0 && bar.x + bar.width <= width - 16.0 + 0.1);
            assert!((bar.y + bar.height - painted[0].y - painted[0].height).abs() < 0.01);
        }
        assert!((painted[0].height - painted[2].height * 3.0).abs() < 0.01);
        assert!((painted[1].height - painted[2].height * 2.0).abs() < 0.01);
        let observations = frame
            .regions
            .iter()
            .filter(|region| region.kind == HitKind::Hover && region.id.starts_with("usage-tasks:"))
            .collect::<Vec<_>>();
        assert_eq!(observations.len(), 3);
        assert!(
            observations
                .iter()
                .all(|region| region.kind == HitKind::Hover)
        );
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
fn one_observed_agent_stays_centered_with_a_narrow_bar_and_singular_count() {
    let state = fixture(&["native"]);
    for width in [280.0, 900.0] {
        let (ctx, mut tree) = native_tree(&state, width);
        let frame = tree.layout(Size::new(width, 520.0));
        let text = text_in(&frame);
        assert!(text.contains("1 tarea visible") && text.contains("1 agente"));
        assert!(!text.contains("1 tareas") && !text.contains("1 agentes"));
        let painted = bars(&frame);
        assert_eq!(painted.len(), 1);
        assert!(painted[0].width <= 48.0);
        let category = frame
            .regions
            .iter()
            .find(|region| region.id == "usage-tasks:native")
            .unwrap()
            .rect;
        assert!(
            (painted[0].x + painted[0].width / 2.0 - category.x - category.width / 2.0).abs() < 0.1
        );
        ctx.end_pass().textures_delta.clear();
    }
}

#[test]
fn task_column_hover_exposes_complete_agent_and_count_without_activating() {
    let mut state = fixture(&["native", "native", "codex"]);
    let full_name = "Agente nativo para proyectos de investigación";
    state.backend_options[0].1 = full_name.into();
    let (ctx, mut tree) = native_tree(&state, 320.0);
    let now = Instant::now();
    let frame = tree.layout_at(Size::new(320.0, 520.0), now);
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == "usage-tasks:native")
        .unwrap()
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    assert!(tree.handle_event(UiEvent::PointerDown(point)).is_empty());
    assert!(tree.handle_event(UiEvent::PointerUp(point)).is_empty());
    tree.handle_event(UiEvent::PointerMove(point));
    let hovered = tree.layout_at(Size::new(320.0, 520.0), now + Duration::from_secs(2));
    let text = text_in(&hovered);
    assert!(
        text.contains(full_name),
        "full agent name must remain available: {text}"
    );
    assert!(
        text.contains("2 tareas en el historial visible"),
        "exact count and scope must remain available: {text}"
    );
    assert_eq!(
        bars(&frame),
        bars(&hovered),
        "hover cannot change the observations"
    );
    ctx.end_pass().textures_delta.clear();
}

#[test]
fn empty_task_history_retains_the_card_identity_and_never_draws_sample_data() {
    let state = fixture(&[]);
    for width in [280.0, 900.0] {
        let frame = UiTree::new(tasks_card(&state, width)).layout(Size::new(width, 520.0));
        let text = text_in(&frame);
        assert!(text.contains("Tareas por agente"));
        assert!(text.contains("0 tareas visibles"));
        assert!(text.contains("No hay tareas en esta vista"));
        assert!(bars(&frame).is_empty());
        assert!(
            !frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("usage-tasks:"))
        );
    }
    assert_eq!(execution_count_label(1), "1 ejecución");
    assert_eq!(execution_count_label(0), "0 ejecuciones");
    assert_eq!(execution_count_label(2), "2 ejecuciones");
}
