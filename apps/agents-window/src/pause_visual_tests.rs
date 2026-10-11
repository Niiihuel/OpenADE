//! Synthetic fixtures for provider retries and limit pauses; no provider,
//! journal or process access.

use super::*;
use openide_ui::{Size, UiTree};

const OWNER: &str = "visual-pause-fixture";
/// A fixed wall clock, so countdowns render the same on every run.
const NOW_MS: u64 = 1_800_000_000_000;

fn state() -> ViewState {
    let mut state = super::tests::state();
    state.selected_session = Some(OWNER.into());
    state.selected_session_title = Some("Pausa por límite".into());
    state.show_side_pane = false;
    state.running = false;
    state.background_run_selected = false;
    state.conversation_details.sync(Some(OWNER));
    state.transcript = vec![TranscriptItem::User {
        content: "Migrá los tests del módulo de cobros.".into(),
        images: vec![],
    }];
    state.pause.clock_ms = NOW_MS;
    state
}

fn retrying() -> ViewState {
    let mut state = state();
    state.running = true;
    state.pause.retry = Some(crate::pause::ModelRetry {
        attempt: 4,
        max_attempts: 6,
        until_ms: NOW_MS + 7_400,
    });
    state
}

fn texts(state: &ViewState, width: f32) -> String {
    let (context, mut tree) = super::file_activity_visual_tests::native_tree(
        build_session_surface(state, width, false, false),
    );
    let frame = tree.layout(Size::new(width, 600.0));
    let mut output = context.end_pass();
    output.textures_delta.clear();
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            openide_ui::DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_visible_retry_replaces_the_waiting_line_with_its_countdown() {
    let text = texts(&retrying(), 900.0);
    assert!(text.contains("Reintentando en 8s (4/6)"), "{text}");
    let mut waiting = retrying();
    waiting.pause.retry = None;
    assert!(texts(&waiting, 900.0).contains(WAITING_LABEL));
}

#[test]
#[ignore = "exports the retry line and the pause states as PNG under docs/ui-separation/shots/pause"]
fn export_pause_visual_review() {
    let directory = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/ui-separation/shots/pause"
    ));
    std::fs::create_dir_all(&directory).unwrap();
    for width in [360.0, 900.0] {
        for (name, state) in [("retry-line", retrying())] {
            let (context, mut tree) = super::file_activity_visual_tests::native_tree(
                build_session_surface(&state, width, false, false),
            );
            tree.set_reduced_motion(true);
            save(&mut tree, &directory, name, width);
            let mut output = context.end_pass();
            output.textures_delta.clear();
        }
    }
}

fn save(tree: &mut UiTree, directory: &std::path::Path, name: &str, width: f32) {
    let frame = tree.layout(Size::new(width, 600.0));
    let path = directory.join(format!("{name}-{width:.0}.svg"));
    crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 600.0).unwrap();
    let options = crate::settings_ui::visual_qa::native_svg_options();
    let svg = std::fs::read_to_string(&path).unwrap();
    let svg_tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 600).unwrap();
    resvg::render(
        &svg_tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(path.with_extension("png")).unwrap();
    std::fs::remove_file(&path).unwrap();
}
