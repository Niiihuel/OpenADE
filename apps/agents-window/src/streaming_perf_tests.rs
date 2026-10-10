//! Repeatable, synthetic native-font layout measurements. No provider or real journal.
use super::*;
use crate::transcript::TranscriptItem;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn native_measurement_installation_survives_frames_and_invalidates_context_or_scale() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(directory.path().join("data"));
    let context = egui::Context::default();
    assert!(app.ensure_native_text_measurers(&context));
    assert!(!app.ensure_native_text_measurers(&context));
    let replacement = egui::Context::default();
    assert!(app.ensure_native_text_measurers(&replacement));
    assert!(!app.ensure_native_text_measurers(&replacement));
    replacement.set_pixels_per_point(2.0);
    replacement.begin_pass(egui::RawInput::default());
    assert_eq!(replacement.pixels_per_point(), 2.0);
    assert!(app.ensure_native_text_measurers(&replacement));
    assert!(!app.ensure_native_text_measurers(&replacement));
    replacement.end_pass().textures_delta.clear();
}

#[test]
fn pointer_frames_reuse_hit_geometry_and_changes_rebuild_before_input() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(directory.path().join("data"));
    let viewport = Size::new(1280.0, 800.0);
    let (initial, rebuilt) = app.prepare_workspace_frame(viewport, false);
    assert!(rebuilt);
    let prompt = initial
        .regions
        .iter()
        .find(|region| region.id == "prompt")
        .unwrap()
        .rect;
    let (_, rebuilt) = app.prepare_workspace_frame(viewport, false);
    assert!(!rebuilt, "pointer input must not trigger a first layout");
    app.tree
        .handle_event(UiEvent::PointerMove(openide_ui::Point::new(
            prompt.x + 10.0,
            prompt.y + 10.0,
        )));
    let (cached, rebuilt) = app.prepare_workspace_frame(viewport, false);
    assert!(!rebuilt);
    let refreshed = relayout_after_updates(&mut app.tree, cached, viewport, false, true, || {
        panic!("hover must not rebuild the application tree")
    });
    assert!(refreshed.regions.iter().any(|region| region.id == "prompt"));
    app.view.show_side_pane = true;
    let (_, rebuilt) = app.prepare_workspace_frame(viewport, true);
    assert!(
        rebuilt,
        "asynchronous state changes invalidate hit geometry"
    );
    let (_, rebuilt) = app.prepare_workspace_frame(Size::new(980.0, 700.0), false);
    assert!(rebuilt, "resizing invalidates hit geometry");
}

#[test]
fn environment_motion_opens_closes_retargets_and_respects_reduced_motion() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(directory.path().join("data"));
    let start = Instant::now();
    assert!(!app.update_environment_motion(start));
    app.view.environment.open = true;
    assert!(app.update_environment_motion(start));
    app.update_environment_motion(start + Duration::from_millis(80));
    let middle = app.view.environment_presentation_progress.unwrap();
    assert!(middle > 0.0 && middle < 1.0);
    app.view.environment.open = false;
    app.update_environment_motion(start + Duration::from_millis(80));
    assert_eq!(app.view.environment_presentation_progress, Some(middle));
    app.update_environment_motion(start + Duration::from_millis(300));
    assert!(app.view.environment_presentation_progress.is_none());
    app.reduced_motion = true;
    app.view.environment.open = true;
    assert!(!app.update_environment_motion(start + Duration::from_millis(301)));
    assert!(app.view.environment_presentation_progress.is_none());
}

fn counting_measurer(
    tree: &mut UiTree,
    context: &egui::Context,
    calls: &Arc<AtomicUsize>,
    bytes: &Arc<AtomicUsize>,
) {
    let context = context.clone();
    let calls = calls.clone();
    let bytes = bytes.clone();
    tree.set_text_measurer(move |text, size, family| {
        calls.fetch_add(1, Ordering::Relaxed);
        bytes.fetch_add(text.len(), Ordering::Relaxed);
        context.fonts_mut(|fonts| {
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
}

fn streaming_fixture(history: usize) -> (tempfile::TempDir, AgentsWindow) {
    let directory = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(directory.path().join("data"));
    app.view.selected_session = Some("synthetic-perf".into());
    app.view.selected_session_title = Some("Medición sintética del streaming".into());
    app.view.show_side_pane = false;
    app.view.running = true;
    app.view.background_run_selected = false;
    app.view.transcript.clear();
    app.tree.set_reduced_motion(true);
    for index in 0..history {
        app.view.transcript.push(TranscriptItem::User {
            content: format!("Paso {index}: revisá el flujo de la interfaz y su resultado."),
            images: vec![],
        });
        app.view.transcript.push(TranscriptItem::Assistant { content: format!("### Resultado {index}\n\nEl proyecto presenta una interfaz **nativa**. La revisión comprueba el procesamiento de eventos, conserva el orden de la respuesta y mide el tiempo necesario para presentar cada actualización.\n\n- Procesar la entrada {index}.\n- Conservar el estado del usuario.\n- Verificar que el resultado sea correcto.\n\n```rust\nlet value_{index} = input.len();\nassert!(value_{index} > 0);\n```\n\nLa comprobación terminó correctamente para este paso y sus archivos asociados."), streaming: false });
    }
    app.view.transcript.push(TranscriptItem::Assistant {
        content: "### Respuesta activa\n\n".into(),
        streaming: true,
    });
    (directory, app)
}

#[test]
#[ignore = "manual native-font performance probe; prints measurements rather than timing assertions"]
fn streaming_layout_benchmark() {
    const STEPS: usize = 24;
    const HISTORY: usize = 48;
    for reinstall in [true, false] {
        let (_directory, mut app) = streaming_fixture(HISTORY);
        let context = egui::Context::default();
        crate::fonts::install(&context);
        context.begin_pass(egui::RawInput::default());
        let mono = egui::FontId::monospace(14.0);
        app.tree.set_metrics(context.fonts_mut(|fonts| FontMetrics {
            char_width: fonts.glyph_width(&mono, 'M'),
            line_height: fonts.row_height(&mono),
        }));
        let sans = egui::FontId::proportional(14.0);
        app.tree
            .set_sans_metrics(context.fonts_mut(|fonts| FontMetrics {
                char_width: fonts.glyph_width(&sans, 'a'),
                line_height: fonts.row_height(&sans),
            }));
        let calls = Arc::new(AtomicUsize::new(0));
        let bytes = Arc::new(AtomicUsize::new(0));
        counting_measurer(&mut app.tree, &context, &calls, &bytes);
        let viewport = Size::new(1280.0, 800.0);
        app.layout_workspace_frame(viewport);
        calls.store(0, Ordering::Relaxed);
        bytes.store(0, Ordering::Relaxed);
        let mut build_time = Duration::ZERO;
        let mut layout_time = Duration::ZERO;
        let mut frame_times = Vec::with_capacity(STEPS);
        for step in 0..STEPS {
            if let Some(TranscriptItem::Assistant { content, .. }) = app.view.transcript.last_mut()
            {
                content.push_str(&format!(
                    "Fragmento {step}: respuesta incremental con acentos y 🦀. "
                ));
            }
            if reinstall {
                counting_measurer(&mut app.tree, &context, &calls, &bytes);
            }
            let start = Instant::now();
            app.tree.set_root(crate::view::build_window_view(
                &app.view,
                viewport.width,
                viewport.height,
            ));
            let build_elapsed = start.elapsed();
            build_time += build_elapsed;
            let start = Instant::now();
            app.tree.layout(viewport);
            let layout_elapsed = start.elapsed();
            layout_time += layout_elapsed;
            frame_times.push(build_elapsed + layout_elapsed);
        }
        context.end_pass().textures_delta.clear();
        eprintln!("STREAM_PERF mode={} history={} frames={} build_ms={:.2} layout_ms={:.2} mean_frame_ms={:.2} native_calls={} native_bytes={}", if reinstall { "reinstall" } else { "stable" }, HISTORY, STEPS, build_time.as_secs_f64()*1000.0, layout_time.as_secs_f64()*1000.0, (build_time+layout_time).as_secs_f64()*1000.0/STEPS as f64, calls.load(Ordering::Relaxed), bytes.load(Ordering::Relaxed));
        frame_times.sort_unstable();
        eprintln!("STREAM_FRAME_BUDGET mode={} p50_ms={:.2} p95_ms={:.2} worst_ms={:.2} budget_120hz_ms=8.33", if reinstall { "reinstall" } else { "stable" }, frame_times[STEPS / 2].as_secs_f64() * 1000.0, frame_times[(STEPS * 95).div_ceil(100) - 1].as_secs_f64() * 1000.0, frame_times[STEPS - 1].as_secs_f64() * 1000.0);
    }
}

#[path = "native_frame_perf_tests.rs"]
mod native_frame_perf_tests;
