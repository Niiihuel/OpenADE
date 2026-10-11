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

fn paused(class: openide_core::TurnPauseClass, resets_in_ms: Option<u64>) -> crate::pause::PausedTurn {
    crate::pause::PausedTurn {
        class,
        limit_key: "fixture-limit".into(),
        resets_at_ms: resets_in_ms.map(|left| NOW_MS + left),
        detail: "HTTP 429: {\"error\":{\"type\":\"usage_limit_reached\",\"message\":\"The usage limit has been reached\"}}".into(),
        provider_id: "codex".into(),
        parent: None,
    }
}

/// The selected session paused for `pause`, as replay leaves it.
fn paused_state(pause: crate::pause::PausedTurn) -> ViewState {
    let mut state = state();
    state.transcript.push(TranscriptItem::Assistant {
        content: "Empiezo por los tests de facturas.".into(),
        streaming: false,
    });
    state
        .session_recovery
        .insert(OWNER.into(), crate::view::SessionRecoveryView::Resume);
    state.pause.sessions.insert(OWNER.into(), pause);
    state
}

fn limit() -> ViewState {
    paused_state(paused(openide_core::TurnPauseClass::LimitReached, Some(8_040_000)))
}

fn credits() -> ViewState {
    let mut pause = paused(openide_core::TurnPauseClass::OutOfCredits, None);
    pause.provider_id = "openrouter".into();
    let mut state = paused_state(pause);
    state.pause.detail_open = Some(OWNER.into());
    state
}

fn auth() -> ViewState {
    let mut pause = paused(openide_core::TurnPauseClass::Auth, None);
    pause.provider_id = "anthropic".into();
    paused_state(pause)
}

/// A parent waiting on a paused child: both share the limit.
fn waiting_on_child() -> ViewState {
    let mut state = paused_state(paused(openide_core::TurnPauseClass::WaitingOnChild, Some(8_040_000)));
    let mut child = paused(openide_core::TurnPauseClass::LimitReached, Some(8_040_000));
    child.parent = Some(OWNER.into());
    state.pause.sessions.insert("child-1".into(), child);
    state.subagent_sessions = vec![crate::view::SubagentSessionView {
        child_session_id: "child-1".into(),
        parent_session_id: OWNER.into(),
        title: "Migrar los tests de facturas".into(),
        status: openide_harness::SubagentStatus::Interrupted,
        cancellation_requested: false,
        open_inputs: 0,
        inputs: vec![],
        dispatch_error: None,
        depth: 1,
    }];
    state
}

fn actions(state: &ViewState, width: f32) -> Vec<String> {
    let (context, mut tree) = super::file_activity_visual_tests::native_tree(
        build_session_surface(state, width, false, false),
    );
    let frame = tree.layout(Size::new(width, 600.0));
    let mut output = context.end_pass();
    output.textures_delta.clear();
    frame
        .regions
        .iter()
        .map(|region| region.id.clone())
        .filter(|id| id.starts_with("pause_") || id.starts_with("session_recover:"))
        .collect()
}

#[test]
fn a_paused_turn_shows_its_cause_and_actions_instead_of_the_recovery_card() {
    for width in [360.0, 900.0] {
        let text = texts(&limit(), width);
        assert!(text.contains("Límite de uso alcanzado · ChatGPT"), "{text}");
        assert!(text.contains("faltan 2 h 14 min"), "{text}");
        // The provider's raw text stays one click away.
        assert!(!text.contains("usage_limit_reached"), "{text}");
        assert!(!text.contains("Recuperación del turno"), "{text}");
        let ids = actions(&limit(), width);
        for action in ["pause_continue", "pause_model", "pause_cancel", "pause_detail"] {
            assert!(ids.contains(&format!("{action}:{OWNER}")), "{action} at {width}: {ids:?}");
        }
        assert!(!ids.iter().any(|id| id.starts_with("pause_account") || id.starts_with("session_recover")));

        let text = texts(&credits(), width);
        assert!(text.contains("Sin crédito en OpenRouter") && text.contains("usage_limit_reached"), "{text}");
        assert!(actions(&credits(), width).contains(&format!("pause_account:{OWNER}")));
        assert!(texts(&auth(), width).contains("Reconectá Anthropic"));

        let text = texts(&waiting_on_child(), width);
        assert!(text.contains("Esperando a un subagente en pausa"), "{text}");
        assert!(text.contains("2 agentes en pausa por este límite."), "{text}");
        // The child reads as paused, and its actions live on the root card only.
        assert!(text.contains("En pausa · ") && !text.contains("Interrumpido"), "{text}");
        assert!(!actions(&waiting_on_child(), width).iter().any(|id| id.ends_with(":child-1")));
    }
    // A turn that needs a person is never dressed as a pause.
    let mut uncertain = limit();
    uncertain.pause.sessions.clear();
    assert!(texts(&uncertain, 900.0).contains("Recuperación del turno"));
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
        for (name, state) in [
            ("retry-line", retrying()),
            ("card-limit-known-reset", limit()),
            ("card-credits-detail", credits()),
            ("card-auth", auth()),
            ("card-waiting-on-child", waiting_on_child()),
        ] {
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
