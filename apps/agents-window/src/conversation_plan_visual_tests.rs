//! Plan and context layout from the native tree, with bundled font metrics.
//! Fixtures are local view projections; these tests never start an engine.

use super::{build_session_surface, plan_snapshot_item, session_plan_card, tests::state};
use crate::{
    transcript::TranscriptItem,
    view::{
        PermissionView, SessionPlanView, SessionRecoveryView, SubagentPermissionView,
        SubagentRunView, ViewState,
    },
};
use eframe::egui;
use openide_core::{MAX_PLAN_STEPS, PlanSnapshot, PlanStep, PlanStepStatus};
use openide_ui::{
    DrawCommand, Element, FontFamily, FontMetrics, Frame, HitKind, Icon, Key, Point, Rect, Size,
    UiAction, UiEvent, UiTree,
};
use std::path::Path;

const OWNER: &str = "inline-plan-fixture";
const DRAFT: &str = "Conservá mi borrador y revisá el siguiente paso.\nTodavía no lo envíes.";
const VIEWPORTS: [Size; 3] = [
    Size::new(360.0, 700.0),
    Size::new(900.0, 900.0),
    Size::new(1700.0, 1000.0),
];

pub(super) fn fixture(long_transcript: bool) -> ViewState {
    let mut state = state();
    state.workspace = "/projects/openADE".into();
    state.selected_session = Some(OWNER.into());
    state.selected_session_title = Some("Plan de trabajo del proyecto".into());
    state.prompt = DRAFT.into();
    state.native_key_available = true;
    let mut route = crate::composer_ui::tests::configured_effort_route();
    route.execution = crate::composer_selection::ExecutionSupport::Verified;
    state.native_provider_id = route.provider_id.clone();
    state.model = route.model_id.clone();
    state.composer_model = route.model_id.clone();
    state.composer_effort_options.clear();
    route.effort_options.clear();
    state.composer_routes = vec![route];
    state.show_side_pane = false;
    state.conversation_details.sync(Some(OWNER));
    for index in 1..=if long_transcript { 12 } else { 1 } {
        state.transcript.push(TranscriptItem::User {
            content: format!(
                "Revisá el área {index} del proyecto y explicá los cambios necesarios."
            ),
            images: vec![],
        });
        state.transcript.push(TranscriptItem::Assistant {
            content: format!("Revisión {index}: el proyecto conserva sus cambios existentes. La implementación debe mantener el borrador, mostrar el plan junto a la conversación y dejar accesibles las acciones al desplazarse por el historial."),
            streaming: false,
        });
    }
    state
}

fn plan(long: bool, can_build: bool) -> SessionPlanView {
    let count = if long { MAX_PLAN_STEPS } else { 3 };
    let snapshot = PlanSnapshot {
        explanation: None,
        plan: (0..count)
            .map(|index| PlanStep {
                step: if long {
                    format!("Revisión detallada del área {}: comprobar los cambios existentes, mantener el borrador del usuario y documentar el comportamiento de la conversación. Verificar que las acciones finales sigan accesibles al desplazarse, incluso en una ventana angosta, antes de implementar el siguiente cambio.", index + 1)
                } else {
                    ["Inspeccionar el proyecto", "Validar el plan en la conversación", "Implementar los cambios acordados"][index].into()
                },
                status: if long {
                    PlanStepStatus::Completed
                } else {
                    [PlanStepStatus::Completed, PlanStepStatus::InProgress, PlanStepStatus::Pending][index]
                },
            })
            .collect(),
    };
    assert!(
        snapshot.is_valid(),
        "fixture respects the durable plan bounds"
    );
    SessionPlanView {
        snapshot,
        can_build,
    }
}

fn append_plan(state: &mut ViewState, plan: SessionPlanView, sequence: u64, is_proposal: bool) {
    state.transcript.push(TranscriptItem::PlanSnapshot {
        id: format!("plan:{OWNER}:{sequence}"),
        sequence: Some(sequence),
        plan: plan.snapshot.clone(),
        is_proposal,
        tool_call_id: None,
    });
    state.session_plans.insert(OWNER.into(), plan);
}

const HISTORY_BEFORE: &str = "Primero voy a revisar el proyecto antes de modificar archivos.";
const HISTORY_BETWEEN: &str = "La inspección terminó. Actualizo el plan con lo que encontré.";
const HISTORY_AFTER: &str =
    "Este es el resultado posterior al último plan. Los archivos existentes se conservaron.";
const FIRST_STEP: &str = "Inspeccionar documentos antes de implementar";
const LATEST_STEP: &str = "Documentos inspeccionados y resultado confirmado";

fn history_fixture() -> ViewState {
    let mut state = fixture(false);
    state.transcript = vec![TranscriptItem::Assistant {
        content: HISTORY_BEFORE.into(),
        streaming: false,
    }];
    let mut initial = plan(false, false);
    initial.snapshot.plan[0].step = FIRST_STEP.into();
    initial.snapshot.plan[0].status = PlanStepStatus::InProgress;
    initial.snapshot.plan[1].status = PlanStepStatus::Pending;
    let frozen = initial.snapshot.clone();
    append_plan(&mut state, initial, 10, false);
    state.transcript.push(TranscriptItem::Assistant {
        content: HISTORY_BETWEEN.into(),
        streaming: false,
    });
    let mut updated = plan(false, true);
    updated.snapshot.plan[0].step = LATEST_STEP.into();
    append_plan(&mut state, updated, 20, true);
    state.transcript.push(TranscriptItem::Assistant {
        content: HISTORY_AFTER.into(),
        streaming: false,
    });
    state.transcript.push(TranscriptItem::ModelUsage {
        id: "history-usage".into(),
        step: 1,
        usage: openide_core::ModelTokenUsage {
            input_tokens: 400,
            output_tokens: 80,
            total_tokens: 480,
            reasoning_tokens: Some(20),
            cached_input_tokens: Some(100),
        },
        status: crate::transcript::ModelStreamStatus::Committed,
    });
    assert!(
        matches!(&state.transcript[1], TranscriptItem::PlanSnapshot { plan, .. } if *plan == frozen)
    );
    state
}

pub(super) fn root(state: &ViewState, viewport: Size) -> Element {
    Element::panel(build_session_surface(state, viewport.width, false, false))
        .background(crate::theme::BACKGROUND)
}

pub(super) fn native_tree(root: Element) -> (egui::Context, UiTree) {
    let context = egui::Context::default();
    crate::fonts::install(&context);
    context.begin_pass(egui::RawInput::default());
    let measure = context.clone();
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    let mono_font = egui::FontId::monospace(14.0);
    tree.set_metrics(context.fonts_mut(|fonts| FontMetrics {
        char_width: fonts.glyph_width(&mono_font, 'M'),
        line_height: fonts.row_height(&mono_font),
    }));
    let sans_font = egui::FontId::proportional(14.0);
    tree.set_sans_metrics(context.fonts_mut(|fonts| {
        FontMetrics {
            char_width: "abcdefghijklmnopqrstuvwxyz"
                .chars()
                .map(|character| fonts.glyph_width(&sans_font, character))
                .sum::<f32>()
                / 26.0,
            line_height: fonts.row_height(&sans_font),
        }
    }));
    tree.set_text_measurer(move |text, size, family| {
        measure.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::new(
                        size,
                        match family {
                            FontFamily::Sans => egui::FontFamily::Proportional,
                            FontFamily::Monospace => egui::FontFamily::Monospace,
                        },
                    ),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
    });
    (context, tree)
}

fn rect(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind != HitKind::Hover)
        .unwrap_or_else(|| panic!("missing conversation control {id}"))
        .rect
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x - 0.1
        && inner.y >= outer.y - 0.1
        && inner.x + inner.width <= outer.x + outer.width + 0.1
        && inner.y + inner.height <= outer.y + outer.height + 0.1
}

fn composer_card(frame: &Frame) -> Rect {
    let editor = rect(frame, "prompt");
    frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::FillRect { rect, radius, .. }
                if *radius == 10.0 && contains(*rect, editor) =>
            {
                Some(*rect)
            }
            _ => None,
        })
        .expect("native composer card")
}

fn plan_card(frame: &Frame) -> Rect {
    frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::StrokeRect { rect, radius, .. }
                if *radius == 8.0 && rect.width > 200.0 =>
            {
                Some(*rect)
            }
            _ => None,
        })
        .expect("inline plan panel")
}

fn assert_same_dock(before: &Frame, after: &Frame) {
    assert_eq!(rect(before, "transcript"), rect(after, "transcript"));
    assert_eq!(composer_card(before), composer_card(after));
    for id in [
        "prompt",
        "run_toggle",
        "composer_add_context",
        "composer_model_toggle",
    ] {
        assert_eq!(rect(before, id), rect(after, id), "{id} geometry");
    }
}

fn assert_reading_column(frame: &Frame, viewport: Size) {
    let card = plan_card(frame);
    let composer = composer_card(frame);
    assert!(card.width <= 780.0, "plan follows the chat reading width");
    let scrollbar_width = frame
        .regions
        .iter()
        .find(|region| region.id == "transcript" && region.kind == HitKind::Scrollbar)
        .map_or(0.0, |region| region.rect.width);
    // The transcript centers within its usable width, excluding the rail;
    // the composer centers within the window. Compare their reading widths
    // around the same center, without the old disclosure's extra left inset.
    let shared_axis_x = card.x + scrollbar_width / 2.0;
    assert!(shared_axis_x >= composer.x - 0.1);
    assert!(shared_axis_x + card.width <= composer.x + composer.width + 0.1);
    assert!(
        (card.x - (viewport.width - scrollbar_width - card.x - card.width)).abs() < 1.0,
        "inline plan is centered in the available chat column: {card:?}"
    );
    if viewport.width >= 1700.0 {
        assert!(
            card.width < viewport.width / 2.0,
            "wide windows keep readable lines"
        );
    }
    assert!(
        frame
            .regions
            .iter()
            .all(|region| { region.kind != HitKind::Scroll || region.id == "transcript" }),
        "plan shares transcript scrolling without an inner scroll viewport"
    );
}

fn assert_build_reachable(frame: &Frame) {
    let id = format!("session_build_plan:{OWNER}");
    let controls = frame
        .regions
        .iter()
        .filter(|region| {
            region.kind == HitKind::Button && region.id.starts_with("session_build_plan:")
        })
        .collect::<Vec<_>>();
    assert_eq!(controls.len(), 1, "only the selected latest plan has a CTA");
    assert_eq!(controls[0].id, id);
    let action = controls[0].rect;
    assert!(contains(rect(frame, "transcript"), action));
    assert!(action.y + action.height <= composer_card(frame).y);
    assert_eq!(
        frame.hit_test(Point::new(
            action.x + action.width / 2.0,
            action.y + action.height / 2.0
        )),
        Some(id.as_str())
    );
}

#[test]
fn snapshots_keep_steps_visible_and_a_passive_aligned_header_in_every_run_state() {
    for width in [280.0, 360.0, 780.0] {
        for (is_proposal, running, background, can_build) in [
            (true, false, false, true),
            (true, true, false, false),
            (true, false, true, false),
            (false, false, false, false),
        ] {
            let mut state = fixture(false);
            state.transcript.clear();
            append_plan(&mut state, plan(false, true), 10, is_proposal);
            state.running = running;
            state.background_run_selected = background;
            let id = format!("plan:{OWNER}:10");
            // A persisted choice from the former collapsible component must
            // never hide steps after this presentation change.
            state.conversation_details.toggle(&id, true);
            assert!(!state.conversation_details.is_expanded(&id, true));
            let (context, mut tree) = native_tree(Element::column(vec![plan_snapshot_item(
                &state.transcript[0],
                0,
                &state,
            )]));
            let frame = tree.layout(Size::new(width, 900.0));
            let card = plan_card(&frame);
            let blocks = frame.text_layouts();
            for (index, step) in state.session_plans[OWNER].snapshot.plan.iter().enumerate() {
                let source = format!("{}. {}", index + 1, step.step);
                let block = blocks
                    .iter()
                    .find(|block| block.source == source)
                    .expect("each plan step is painted without expanding the snapshot");
                assert!(block.runs.iter().all(|run| {
                    run.visible.is_some_and(|visible| {
                        contains(visible, run.rect) && contains(card, visible)
                    })
                }));
            }
            let title = if is_proposal {
                "Plan propuesto"
            } else {
                "Plan actualizado"
            };
            let title_rect = blocks
                .iter()
                .find(|block| block.source == title)
                .unwrap()
                .runs[0]
                .rect;
            let count = blocks
                .iter()
                .find(|block| block.source == "1 de 3")
                .expect("progress is shown alongside the title");
            assert_eq!(count.runs.len(), 1, "progress never wraps");
            let count_rect = count.runs[0].rect;
            assert!(
                (card.x + card.width - 10.0 - count_rect.x - count_rect.width).abs() < 0.5,
                "progress aligns with the card's right content edge: {card:?} {count_rect:?}"
            );
            assert!(title_rect.x + title_rect.width + 4.0 <= count_rect.x);
            assert!(
                (title_rect.y + title_rect.height / 2.0 - count_rect.y - count_rect.height / 2.0)
                    .abs()
                    < 1.0,
                "title and progress share one compact header: {title_rect:?} {count_rect:?}"
            );
            assert!(
                !frame
                    .regions
                    .iter()
                    .any(|region| region.id.starts_with("conversation_detail:")),
                "the plan has no collapse header or footer hit targets"
            );
            assert!(
                !frame.commands.iter().any(|command| matches!(
                    command,
                    DrawCommand::Icon {
                        icon: Icon::ChevronRight | Icon::ChevronDown | Icon::ArrowUp,
                        ..
                    }
                )),
                "a permanently visible plan does not advertise a disclosure"
            );
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .filter(|region| region.kind == HitKind::Button
                        && region.id == format!("session_build_plan:{OWNER}"))
                    .count(),
                usize::from(can_build)
            );
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn inline_plan_keeps_chat_viewport_dock_and_unsent_selection() {
    for viewport in VIEWPORTS {
        let mut state = fixture(false);
        let (context, mut tree) = native_tree(root(&state, viewport));
        let before = tree.layout(viewport);
        assert!(tree.focus("prompt"));
        tree.set_input_cursor("prompt", 11);
        tree.handle_event(UiEvent::Key {
            key: Key::ArrowRight,
            shift: true,
            ctrl: false,
        });
        let cursor = tree.input_cursor("prompt");
        let selection = tree.input_selection("prompt");
        append_plan(&mut state, plan(false, true), 10, true);
        state
            .session_plans
            .insert("another-session".into(), plan(false, true));
        tree.set_root(root(&state, viewport));
        let after = tree.layout(viewport);
        assert_same_dock(&before, &after);
        assert_reading_column(&after, viewport);
        assert_eq!(tree.input_value("prompt"), Some(DRAFT));
        assert_eq!(tree.input_cursor("prompt"), cursor);
        assert_eq!(tree.input_selection("prompt"), selection);
        assert_eq!(tree.focused_id(), Some("prompt"));
        tree.scroll_to_end("transcript");
        assert_build_reachable(&tree.layout(viewport));
        state.session_plans.get_mut(OWNER).unwrap().can_build = false;
        tree.set_root(root(&state, viewport));
        let pending = tree.layout(viewport);
        assert!(
            pending
                .regions
                .iter()
                .all(|region| !region.id.starts_with("session_build_plan:"))
        );
        assert_eq!(tree.input_value("prompt"), Some(DRAFT));
        assert_eq!(tree.input_cursor("prompt"), cursor);
        assert_eq!(tree.input_selection("prompt"), selection);
        state.show_jump_to_bottom = true;
        tree.set_root(root(&state, viewport));
        let jump = tree.layout(viewport);
        assert_same_dock(&before, &jump);
        assert!(contains(
            rect(&jump, "transcript"),
            rect(&jump, "jump_to_bottom")
        ));
        assert!(
            rect(&jump, "jump_to_bottom").y + rect(&jump, "jump_to_bottom").height
                <= composer_card(&jump).y
        );
        assert_eq!(tree.input_value("prompt"), Some(DRAFT));
        assert_eq!(tree.input_cursor("prompt"), cursor);
        assert_eq!(tree.input_selection("prompt"), selection);
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn twenty_four_wrapped_steps_use_chat_scroll_and_leave_the_final_action_clickable() {
    for viewport in VIEWPORTS {
        let mut state = fixture(true);
        let (context, mut tree) = native_tree(root(&state, viewport));
        let before = tree.layout(viewport);
        let plan = plan(true, true);
        let (plan_context, mut plan_tree) = native_tree(Element::column(vec![session_plan_card(
            OWNER,
            &plan,
            "Plan propuesto",
        )]));
        let natural = plan_tree.layout(Size::new(viewport.width.min(780.0), 10000.0));
        assert!(
            plan_card(&natural).height > rect(&before, "transcript").height,
            "the full plan has natural height rather than clipping its steps"
        );
        plan_context.end_pass().textures_delta.clear();
        let final_step = format!(
            "{}. {}",
            MAX_PLAN_STEPS,
            plan.snapshot.plan.last().unwrap().step
        );
        append_plan(&mut state, plan, 10, true);
        tree.set_root(root(&state, viewport));
        let start = tree.layout(viewport);
        assert_same_dock(&before, &start);
        assert!(
            start
                .regions
                .iter()
                .all(|region| !region.id.starts_with("session_build_plan:")),
            "plan actions are below the long history before scrolling"
        );
        assert!(!tree.is_scroll_at_end("transcript", 1.0));
        tree.scroll_to_end("transcript");
        let end = tree.layout(viewport);
        assert_same_dock(&before, &end);
        assert!(tree.scroll_offset("transcript") > 0.0);
        assert!(tree.is_scroll_at_end("transcript", 1.0));
        assert_reading_column(&end, viewport);
        assert_build_reachable(&end);
        let block = end
            .text_layouts()
            .into_iter()
            .find(|block| block.source == final_step)
            .expect("last plan step is rendered");
        assert!(
            block.runs.len() >= 2,
            "long steps actually wrap with native metrics"
        );
        assert!(block.runs.iter().all(|run| {
            run.visible
                .is_some_and(|visible| contains(rect(&end, "transcript"), visible))
        }));
        assert!(
            block.runs.iter().all(|run| run
                .visible
                .is_some_and(|visible| contains(visible, run.rect))),
            "all lines of the final step are readable at the end"
        );
        let id = format!("session_build_plan:{OWNER}");
        let action = rect(&end, &id);
        let point = Point::new(
            action.x + action.width / 2.0,
            action.y + action.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked { id }]
        );
        assert_eq!(tree.input_value("prompt"), Some(DRAFT));
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn plan_versions_keep_their_place_and_prior_state_without_a_synthetic_final_card() {
    for viewport in VIEWPORTS {
        let state = history_fixture();
        let (context, mut tree) = native_tree(root(&state, viewport));
        let first_step = format!("1. {FIRST_STEP}");
        let latest_step = format!("1. {LATEST_STEP}");
        let markers = [
            HISTORY_BEFORE,
            first_step.as_str(),
            HISTORY_BETWEEN,
            latest_step.as_str(),
            HISTORY_AFTER,
        ];
        let mut positions = std::collections::BTreeMap::new();
        for _ in 0..80 {
            let frame = tree.layout(viewport);
            for block in frame.text_layouts() {
                if let Some(marker) = markers.iter().find(|marker| **marker == block.source) {
                    if let Some(run) = block.runs.iter().find(|run| run.visible.is_some()) {
                        positions.insert(*marker, run.rect.y + tree.scroll_offset("transcript"));
                    }
                }
            }
            for region in &frame.regions {
                assert!(
                    !region
                        .id
                        .starts_with(&format!("conversation_detail:{OWNER}:plan:")),
                    "historical snapshots never require a disclosure click"
                );
                assert!(!region.id.contains("history-usage"));
                assert!(!region.id.contains("turn-usage"));
            }
            assert!(
                frame
                    .regions
                    .iter()
                    .any(|region| region.id == "composer_context_toggle")
            );
            assert!(!frame.commands.iter().any(|command| matches!(command, DrawCommand::Text { text, .. } if text.to_lowercase().contains("tokens"))));
            if tree.is_scroll_at_end("transcript", 1.0) {
                break;
            }
            let viewport_rect = rect(&frame, "transcript");
            tree.handle_event(UiEvent::Scroll {
                position: Point::new(
                    viewport_rect.x + viewport_rect.width / 2.0,
                    viewport_rect.y + viewport_rect.height / 2.0,
                ),
                delta_y: -viewport_rect.height * 0.6,
            });
        }
        for pair in markers.windows(2) {
            let before = positions.get(pair[0]).unwrap_or_else(|| {
                panic!(
                    "missing historical marker {} at {}px",
                    pair[0], viewport.width
                )
            });
            let after = positions.get(pair[1]).unwrap_or_else(|| {
                panic!(
                    "missing historical marker {} at {}px",
                    pair[1], viewport.width
                )
            });
            assert!(
                before < after,
                "snapshots must stay between their surrounding responses at {}px",
                viewport.width
            );
        }
        assert!(
            matches!(&state.transcript[1], TranscriptItem::PlanSnapshot { plan, .. } if plan.plan[0].step == FIRST_STEP && plan.plan[0].status == PlanStepStatus::InProgress)
        );
        context.end_pass().textures_delta.clear();

        // A historical version is readable but cannot accidentally build the
        // current proposal merely because it has the same session owner.
        for (index, expected_build) in [(1, false), (3, true)] {
            let (context, mut tree) =
                native_tree(plan_snapshot_item(&state.transcript[index], index, &state));
            let frame = tree.layout(Size::new(viewport.width.min(780.0), 1400.0));
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .filter(|region| region.kind == HitKind::Button
                        && region.id == format!("session_build_plan:{OWNER}"))
                    .count(),
                usize::from(expected_build)
            );
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn current_plan_metadata_does_not_append_a_card_without_a_history_event() {
    let mut state = fixture(false);
    state.session_plans.insert(OWNER.into(), plan(false, true));
    let viewport = VIEWPORTS[1];
    let (context, mut tree) = native_tree(root(&state, viewport));
    tree.layout(viewport);
    tree.scroll_to_end("transcript");
    let frame = tree.layout(viewport);
    assert!(!frame.regions.iter().any(|region| {
        region.id.starts_with("session_build_plan:")
            || region
                .id
                .starts_with(&format!("conversation_detail:{OWNER}:plan:"))
    }));
    assert!(!frame.commands.iter().any(|command| matches!(
        command,
        DrawCommand::Icon {
            icon: Icon::Plan,
            ..
        }
    )));
    context.end_pass().textures_delta.clear();
}

#[test]
fn confirmed_plan_tools_do_not_duplicate_snapshots_but_failures_and_pending_tools_remain() {
    use crate::transcript::{ActivityStatus, ModelStreamStatus};
    for (activity, is_error, should_remain) in [
        (ActivityStatus::Completed, false, false),
        (ActivityStatus::Completed, true, true),
        (ActivityStatus::Failed, true, true),
        (ActivityStatus::Running, false, true),
        (ActivityStatus::Interrupted, false, true),
    ] {
        let mut state = fixture(false);
        state.transcript = vec![TranscriptItem::Tool {
            id: "plan-call".into(),
            name: "update_plan".into(),
            arguments: serde_json::json!({"plan": [{"step": "Revisar", "status": "in_progress"}]}),
            output: Some(
                if is_error {
                    "No se pudo confirmar el plan"
                } else {
                    "Plan actualizado"
                }
                .into(),
            ),
            is_error,
            activity,
        }];
        append_plan(&mut state, plan(false, false), 10, false);
        if let TranscriptItem::PlanSnapshot { tool_call_id, .. } = &mut state.transcript[1] {
            *tool_call_id = Some("plan-call".into());
        }
        let viewport = VIEWPORTS[1];
        let (context, mut tree) = native_tree(root(&state, viewport));
        let frame = tree.layout(viewport);
        assert_eq!(
            frame
                .regions
                .iter()
                .any(|region| region.id == format!("conversation_detail:{OWNER}:tool:plan-call")),
            should_remain,
            "tool state {activity:?}, error={is_error}"
        );
        assert_eq!(
            frame
                .text_layouts()
                .iter()
                .filter(|block| block.source == "1. Inspeccionar el proyecto")
                .count(),
            1,
            "the confirmed snapshot appears once with its steps visible"
        );
        assert!(
            !frame
                .regions
                .iter()
                .any(|region| region.id == format!("conversation_detail:{OWNER}:plan:{OWNER}:10"))
        );
        context.end_pass().textures_delta.clear();
    }
    for (status, should_remain) in [
        (ModelStreamStatus::Committed, false),
        (ModelStreamStatus::Receiving, true),
        (ModelStreamStatus::Failed, true),
    ] {
        let mut state = fixture(false);
        state.transcript = vec![TranscriptItem::ToolPreview {
            id: "preview-plan".into(),
            step: 1,
            item_id: "item-plan".into(),
            call_id: Some("plan-call".into()),
            name: Some("update_plan".into()),
            arguments: "{\"plan\":[]}".into(),
            input_complete: true,
            status,
            truncated: false,
        }];
        append_plan(&mut state, plan(false, false), 10, false);
        if let TranscriptItem::PlanSnapshot { tool_call_id, .. } = &mut state.transcript[1] {
            *tool_call_id = Some("plan-call".into());
        }
        let viewport = VIEWPORTS[1];
        let (context, mut tree) = native_tree(root(&state, viewport));
        let frame = tree.layout(viewport);
        assert_eq!(
            frame
                .regions
                .iter()
                .any(|region| region.id == format!("conversation_detail:{OWNER}:tool:plan-call")),
            should_remain,
            "preview state {status:?}"
        );
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn plan_status_marks_are_vectors_with_no_font_dependent_circle_or_check_glyphs() {
    let (context, mut tree) = native_tree(session_plan_card(
        OWNER,
        &plan(false, true),
        "Plan propuesto",
    ));
    let frame = tree.layout(Size::new(360.0, 400.0));
    assert_eq!(frame.commands.iter().filter(|command| matches!(command,
        DrawCommand::Icon { rect, icon: Icon::CheckCircle, .. } if rect.width == 14.0 && rect.height == 14.0
    )).count(), 1);
    assert_eq!(frame.commands.iter().filter(|command| matches!(command,
        DrawCommand::StrokeRect { rect, radius, .. } if rect.width == 14.0 && rect.height == 14.0 && *radius == 7.0
    )).count(), 2);
    assert_eq!(frame.commands.iter().filter(|command| matches!(command,
        DrawCommand::FillRect { rect, radius, .. } if rect.width == 6.0 && rect.height == 6.0 && *radius == 3.0
    )).count(), 1);
    assert!(frame.commands.iter().all(|command| {
        match command {
            DrawCommand::Text { text, .. } => !text
                .chars()
                .any(|character| ['○', '●', '✓', '☐', '�'].contains(&character)),
            _ => true,
        }
    }));
    context.end_pass().textures_delta.clear();
}

#[test]
fn permissions_recovery_and_subagents_share_chat_without_resizing_the_composer() {
    for viewport in VIEWPORTS {
        for kind in ["permission", "recovery", "subagent-permission", "subagent"] {
            let mut state = fixture(false);
            let (context, mut tree) = native_tree(root(&state, viewport));
            let before = tree.layout(viewport);
            let action: String = match kind {
                "permission" => {
                    state.pending_permissions.push(PermissionView {
                        request_id: "request-local".into(),
                        tool_name: "read_file".into(),
                        arguments: "{\"path\":\"README.md\"}".into(),
                    });
                    crate::approval_ui::ApprovalTarget {
                        owner_session_id: OWNER.into(),
                        workspace: state.workspace.clone(),
                        child_session_id: None,
                        request_id: "request-local".into(),
                    }
                    .decision_action("allow_once")
                }
                "recovery" => {
                    state
                        .session_recovery
                        .insert(OWNER.into(), SessionRecoveryView::Resume);
                    format!("session_recover:{OWNER}")
                }
                "subagent-permission" => {
                    state
                        .pending_subagent_permissions
                        .push(SubagentPermissionView {
                            child_session_id: "child-local".into(),
                            parent_session_id: OWNER.into(),
                            request_id: "request-child".into(),
                            tool_name: "read_file".into(),
                            arguments: "{\"path\":\"README.md\"}".into(),
                        });
                    crate::approval_ui::ApprovalTarget {
                        owner_session_id: OWNER.into(),
                        workspace: state.workspace.clone(),
                        child_session_id: Some("child-local".into()),
                        request_id: "request-child".into(),
                    }
                    .decision_action("allow_once")
                }
                _ => {
                    state.active_subagents.push(SubagentRunView {
                        child_session_id: "child-local".into(),
                        parent_session_id: OWNER.into(),
                        title: "Revisar la documentación".into(),
                    });
                    "subagent_cancel:child-local".into()
                }
            };
            tree.set_root(root(&state, viewport));
            tree.scroll_to_end("transcript");
            let after = tree.layout(viewport);
            assert_same_dock(&before, &after);
            assert!(
                contains(rect(&after, "transcript"), rect(&after, &action)),
                "{kind} is in the chat viewport"
            );
            assert!(
                rect(&after, &action).y + rect(&after, &action).height <= composer_card(&after).y
            );
            assert!(
                after
                    .regions
                    .iter()
                    .all(|region| region.id != "related_subagents")
            );
            assert_eq!(tree.input_value("prompt"), Some(DRAFT));
            context.end_pass().textures_delta.clear();
        }
    }
}

pub(super) fn export(frame: &Frame, directory: &Path, name: &str, viewport: Size) {
    let path = directory.join(format!("{name}-{:.0}.svg", viewport.width));
    crate::settings_ui::visual_qa::write_native_frame_svg(
        frame,
        &path,
        viewport.width,
        viewport.height,
    )
    .unwrap();
    let svg = std::fs::read_to_string(&path).unwrap();
    let parsed =
        resvg::usvg::Tree::from_str(&svg, &crate::settings_ui::visual_qa::native_svg_options())
            .unwrap();
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(viewport.width as u32, viewport.height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(path.with_extension("png")).unwrap();
}

#[test]
#[ignore = "writes native plan PNG/SVG previews under /tmp"]
fn export_inline_plan_visual_review() {
    let directory = Path::new("/tmp/openade-conversation-plan-review");
    std::fs::create_dir_all(directory).unwrap();
    for viewport in VIEWPORTS {
        let mut state = fixture(true);
        append_plan(&mut state, plan(true, true), 10, true);
        let (context, mut tree) = native_tree(root(&state, viewport));
        let start = tree.layout(viewport);
        assert!(
            start
                .regions
                .iter()
                .all(|region| !region.id.starts_with("session_build_plan:"))
        );
        export(&start, directory, "chat-plan-start", viewport);
        tree.scroll_to_end("transcript");
        let end = tree.layout(viewport);
        assert_reading_column(&end, viewport);
        assert_build_reachable(&end);
        export(&end, directory, "chat-plan-end", viewport);
        context.end_pass().textures_delta.clear();

        let mut state = fixture(false);
        append_plan(&mut state, plan(false, true), 10, true);
        let (context, mut tree) = native_tree(root(&state, viewport));
        tree.scroll_to_end("transcript");
        let frame = tree.layout(viewport);
        assert_build_reachable(&frame);
        export(&frame, directory, "chat-plan-status", viewport);
        context.end_pass().textures_delta.clear();

        let mut state = history_fixture();
        let (context, mut tree) = native_tree(root(&state, viewport));
        let start = tree.layout(viewport);
        export(&start, directory, "chat-plan-history-start", viewport);
        tree.scroll_to_end("transcript");
        export(
            &tree.layout(viewport),
            directory,
            "chat-plan-history-end",
            viewport,
        );
        context.end_pass().textures_delta.clear();
        for sequence in [10, 20] {
            state
                .conversation_details
                .toggle(&format!("plan:{OWNER}:{sequence}"), true);
        }
        let (context, mut tree) = native_tree(root(&state, viewport));
        export(
            &tree.layout(viewport),
            directory,
            "chat-plan-history-ignores-old-collapse",
            viewport,
        );
        context.end_pass().textures_delta.clear();
    }
}

#[test]
#[ignore = "writes actual confirmation dialogs with bundled native fonts under /tmp"]
fn export_confirmation_dialogs_native_visual_review() {
    let directory = Path::new("/tmp/openade-confirmation-native-review");
    std::fs::create_dir_all(directory).unwrap();
    for width in [360.0, 900.0] {
        for (name, title, cancel, confirm) in [
            (
                "delete-conversation",
                "¿Eliminar conversación?",
                "delete_cancel",
                "delete_confirm:inline-plan-fixture",
            ),
            (
                "discard-reload",
                "¿Recargar el archivo?",
                "code_editor_reload_cancel",
                "code_editor_reload_confirm",
            ),
            (
                "unsaved-files",
                "Hay archivos sin guardar",
                "code_editor_quit_cancel",
                "code_editor_quit_keep",
            ),
        ] {
            let mut state = fixture(false);
            state.sidebar_collapsed = true;
            state.sessions = vec![crate::view::SessionView {
                id: OWNER.into(),
                title:
                    "Revisar los cambios de la conversación y conservar los archivos del proyecto"
                        .into(),
                project: state.workspace.clone(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
            }];
            match name {
                "delete-conversation" => state.pending_delete_session_id = Some(OWNER.into()),
                "discard-reload" => {
                    state.code_editor_reload_confirm =
                        Some(crate::code_editor_files::EditorFileScope {
                            tab_id: 1,
                            generation: 1,
                            workspace: state.workspace.clone().into(),
                            relative_path: "src/main.rs".into(),
                        })
                }
                _ => state.code_editor_quit_confirm = true,
            }
            let viewport = Size::new(width, 720.0);
            let (context, mut tree) = crate::view::ide_visual_tests::native_tree(
                crate::view::build_window_view(&state, viewport.width, viewport.height),
            );
            let frame = tree.layout(viewport);
            assert_eq!(tree.focused_id(), Some(cancel));
            let close = rect(&frame, &format!("{cancel}:close"));
            assert_eq!(close.width, 24.0);
            assert_eq!(close.height, 24.0);
            for id in [cancel, confirm] {
                let button = frame
                    .regions
                    .iter()
                    .find(|region| region.kind == HitKind::Button && region.id == id)
                    .expect("confirmation actions stay present");
                assert_eq!(button.rect.height, 28.0);
                assert!(contains(
                    Rect::new(0.0, 0.0, width, viewport.height),
                    button.rect
                ));
            }
            let layouts = frame.text_layouts();
            let heading = layouts
                .iter()
                .find(|block| block.source == title)
                .expect("the full confirmation title is laid out");
            assert!(heading.runs.iter().all(|run| {
                run.visible
                    .is_some_and(|visible| contains(visible, run.rect))
            }));
            assert!(!frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, .. } if text == "Esc"
            )));
            export(&frame, directory, name, viewport);
            context.end_pass().textures_delta.clear();
        }
    }
}
