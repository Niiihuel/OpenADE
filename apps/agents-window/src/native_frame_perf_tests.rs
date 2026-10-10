//! CPU frame probe through the native painter and egui tessellator. It does
//! not submit meshes to a GPU or infer the display's actual refresh rate.
use super::*;

#[derive(Clone, Copy)]
enum Workload {
    Streaming,
    Hover,
    Environment,
}

impl Workload {
    fn name(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Hover => "hover",
            Self::Environment => "environment",
        }
    }
}

fn quantile(values: &mut [Duration], percent: usize) -> f64 {
    values.sort_unstable();
    values[(values.len() * percent).div_ceil(100).saturating_sub(1)].as_secs_f64() * 1000.0
}

#[test]
#[ignore = "manual full CPU frame performance probe, excludes GPU submission"]
fn native_frame_cpu_benchmark() {
    const FRAMES: usize = 96;
    const WARMUP: usize = 8;
    const HISTORY: usize = 48;
    let scale: f32 = std::env::var("OPENIDE_PERF_SCALE")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value: &f32| value.is_finite() && *value >= 1.0 && *value <= 3.0)
        .unwrap_or(1.0);
    let viewport = Size::new(1280.0, 800.0);
    for workload in [Workload::Streaming, Workload::Hover, Workload::Environment] {
        let (_directory, mut app) = streaming_fixture(HISTORY);
        app.reduced_motion = false;
        app.tree.set_reduced_motion(false);
        if !matches!(workload, Workload::Streaming) {
            app.view.running = false;
            if let Some(TranscriptItem::Assistant { streaming, .. }) =
                app.view.transcript.last_mut()
            {
                *streaming = false;
            }
        }
        let context = egui::Context::default();
        context.set_pixels_per_point(scale);
        crate::fonts::install(&context);
        let timeline = Instant::now();
        let mut samples = Vec::with_capacity(FRAMES);
        let mut layouts = Vec::with_capacity(FRAMES);
        let mut paints = Vec::with_capacity(FRAMES);
        let mut tessellations = Vec::with_capacity(FRAMES);
        let mut mesh_count = 0usize;
        for step in 0..FRAMES + WARMUP {
            let measured = step >= WARMUP;
            let index = step.saturating_sub(WARMUP);
            let frame_time = timeline + Duration::from_secs_f64(step as f64 / 120.0);
            let total_start = Instant::now();
            let mut layout_time = Duration::ZERO;
            let mut paint_time = Duration::ZERO;
            let mut output = context.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(viewport.width, viewport.height))),
                time: Some(step as f64 / 120.0),
                predicted_dt: 1.0 / 120.0,
                ..Default::default()
            }, |ui| {
                if step == 0 {
                    let mono = egui::FontId::monospace(14.0);
                    app.tree.set_metrics(ui.ctx().fonts_mut(|fonts| FontMetrics { char_width: fonts.glyph_width(&mono, 'M'), line_height: fonts.row_height(&mono) }));
                    let sans = egui::FontId::proportional(14.0);
                    app.tree.set_sans_metrics(ui.ctx().fonts_mut(|fonts| FontMetrics { char_width: fonts.glyph_width(&sans, 'a'), line_height: fonts.row_height(&sans) }));
                    app.ensure_native_text_measurers(ui.ctx());
                }
                let started = Instant::now();
                // The synthetic fixture has no worker-backed active_run, so
                // explicitly model the live chat's follow-tail intent. Keep
                // hover and Environment at the same visible tail after warmup.
                if !measured || matches!(workload, Workload::Streaming) {
                    app.tree.scroll_to_end("transcript");
                }
                let frame = if !measured {
                    app.layout_workspace_frame(viewport)
                } else {
                    match workload {
                        Workload::Streaming => {
                            if let Some(TranscriptItem::Assistant { content, .. }) = app.view.transcript.last_mut() {
                                content.push_str(&format!("Fragmento {index}: respuesta incremental con acentos y 🦀. "));
                            }
                            app.layout_workspace_frame(viewport)
                        }
                        Workload::Hover => {
                            let (frame, _) = app.prepare_workspace_frame(viewport, false);
                            let targets: Vec<_> = frame.regions.iter().filter(|region| region.kind == openide_ui::HitKind::Button).take(2).map(|region| region.rect).collect();
                            if let Some(target) = targets.get(index % targets.len().max(1)) {
                                app.tree.handle_event_at(UiEvent::PointerMove(openide_ui::Point::new(target.x + target.width / 2.0, target.y + target.height / 2.0)), frame_time);
                            }
                            relayout_after_updates(&mut app.tree, frame, viewport, false, true, || panic!("hover should retain the existing application tree"))
                        }
                        Workload::Environment => {
                            app.view.environment.open = (index / 32) % 2 == 0;
                            app.update_environment_motion(frame_time);
                            app.layout_workspace_frame(viewport)
                        }
                    }
                };
                layout_time = started.elapsed();
                if matches!(workload, Workload::Streaming) {
                    assert!(app.tree.is_scroll_at_end("transcript", 1.0),
                        "the CPU probe must keep the active response visible");
                }
                let started = Instant::now();
                crate::render::paint(ui, &frame);
                paint_time = started.elapsed();
            });
            let tessellate_start = Instant::now();
            let primitives =
                context.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
            let tessellate_time = tessellate_start.elapsed();
            mesh_count += primitives.len();
            std::hint::black_box(&primitives);
            drop(primitives);
            output.textures_delta.clear();
            if measured {
                samples.push(total_start.elapsed());
                layouts.push(layout_time);
                paints.push(paint_time);
                tessellations.push(tessellate_time);
            }
        }
        let mean = samples.iter().sum::<Duration>().as_secs_f64() * 1000.0 / FRAMES as f64;
        let p50 = quantile(&mut samples, 50);
        let p95 = quantile(&mut samples, 95);
        let worst = quantile(&mut samples, 100);
        let layout_p95 = quantile(&mut layouts, 95);
        let paint_p95 = quantile(&mut paints, 95);
        let tessellate_p95 = quantile(&mut tessellations, 95);
        eprintln!("NATIVE_FRAME_CPU workload={} history={} viewport=1280x800 scale={scale} follow_tail=true frames={} mean_ms={mean:.2} p50_ms={p50:.2} p95_ms={p95:.2} worst_ms={worst:.2} layout_p95_ms={layout_p95:.2} paint_p95_ms={paint_p95:.2} tessellate_p95_ms={tessellate_p95:.2} budget_120hz_ms=8.33 primitives={mesh_count}", workload.name(), HISTORY, FRAMES);
    }
}
