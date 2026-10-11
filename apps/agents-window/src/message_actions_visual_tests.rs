//! The action rows of the transcript, from the native tree with bundled
//! font metrics. Fixtures are local view projections; no engine starts.

use super::plan_visual_tests::{export, fixture, native_tree, root};
use crate::{
    message_actions::{apply_action, turn_items, turn_markdown, Effect},
    message_edit::{EditableTurn, EditingMessage},
    provider_usage_ui::now_ms,
    transcript::{turn_time_id, ActivityStatus, TranscriptActivity, TranscriptItem},
    view::ViewState,
};
use openide_ui::{DrawCommand, Frame, HitKind, Icon, Point, Rect, Size, UiAction, UiEvent, UiTree};
use serde_json::json;
use std::path::Path;

const WIDE: Size = Size::new(900.0, 700.0);
const NARROW: Size = Size::new(360.0, 700.0);
const SHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/ui-separation/shots/message-actions");
const ANSWER: &str = "El margen se calcula dos veces.\n\n```rust\nlet margen = alto - 24.0;\n```";

fn prose(content: &str) -> TranscriptItem {
    TranscriptItem::Assistant { content: content.into(), streaming: false }
}

fn model(status: ActivityStatus) -> TranscriptItem {
    TranscriptItem::Activity(TranscriptActivity::Model {
        id: "model-1".into(),
        step: 1,
        model: "opus-5.5".into(),
        has_text: true,
        status,
    })
}

/// Two finished native turns. The first has prose around a command; its
/// rows start at transcript index 1 and the second turn's at 7.
fn two_turns() -> ViewState {
    let mut state = fixture(false);
    state.selected_backend = "native".into();
    state.transcript = vec![
        TranscriptItem::User { content: "Primer pedido: ¿por qué salta el contenido?".into(), images: vec![] },
        model(ActivityStatus::Completed),
        prose("Voy a mirar el cálculo."),
        TranscriptItem::Tool {
            id: "cmd-1".into(),
            name: "run_command".into(),
            arguments: json!({"command": "cargo test margen"}),
            output: Some("SALIDA_DE_LA_HERRAMIENTA".into()),
            is_error: false,
            activity: ActivityStatus::Completed,
        },
        TranscriptItem::Reasoning {
            id: "reasoning-1".into(),
            step: 1,
            content: "RAZONAMIENTO".into(),
            status: crate::transcript::ModelStreamStatus::Committed,
            truncated: false,
        },
        prose(ANSWER),
        TranscriptItem::User { content: "Segundo pedido: corregilo.".into(), images: vec![] },
        model(ActivityStatus::Completed),
        prose("Listo: el margen se calcula una sola vez."),
    ];
    state.message_edit.turns = (0..2)
        .map(|user_row| EditableTurn {
            user_row,
            sequence: 1 + user_row as u64 * 30,
            text: Some(if user_row == 0 { "Lo que escribí" } else { "Segundo pedido: corregilo." }.into()),
            drops_attachments: false,
        })
        .collect();
    let now = now_ms();
    state.item_times.record(&turn_time_id(0), now - 314_000, Some(now - 300_000));
    state.item_times.record(&turn_time_id(1), now - 62_000, Some(now - 60_000));
    state
}

fn button(frame: &Frame, id: &str) -> Option<Rect> {
    frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .map(|region| region.rect)
}

fn center(rect: Rect) -> Point {
    Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

/// Whether a copy glyph is painted inside `rect`.
fn painted(frame: &Frame, rect: Rect, wanted: Icon) -> bool {
    frame.commands.iter().any(|command| {
        matches!(command, DrawCommand::Icon { rect: glyph, icon, tint }
            if *icon == wanted && tint.a > 0 && rect.contains(center(*glyph)))
    })
}

fn texts(frame: &Frame) -> String {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text { text, color, .. } if color.a > 0 => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn hover(tree: &mut UiTree, viewport: Size, point: Point) -> Frame {
    tree.handle_event(UiEvent::PointerMove(point));
    tree.layout(viewport)
}

fn click(tree: &mut UiTree, rect: Rect) -> Vec<UiAction> {
    tree.handle_event(UiEvent::PointerDown(center(rect)));
    tree.handle_event(UiEvent::PointerUp(center(rect)))
}

#[test]
fn each_message_offers_what_the_app_can_do_at_both_widths() {
    for viewport in [WIDE, NARROW] {
        let state = two_turns();
        let (_context, mut tree) = native_tree(root(&state, viewport));
        let frame = tree.layout(viewport);
        for id in ["user_copy:0", "user_edit:0", "turn_copy:1", "turn_actions:1:more", "user_copy:6", "turn_copy:7"] {
            let rect = button(&frame, id).unwrap_or_else(|| panic!("{id} at {}", viewport.width));
            // The last row may be cut by the end of the viewport.
            assert!(rect.width == 24.0 && rect.height <= 24.0, "{id}");
            assert!(rect.x >= 0.0 && rect.x + rect.width <= viewport.width, "{id}");
        }
        // The bubble's actions sit at its trailing edge, the turn's at its start.
        let (user, turn) = (button(&frame, "user_edit:0").unwrap(), button(&frame, "turn_copy:1").unwrap());
        assert!(user.x > viewport.width / 2.0 && turn.x < viewport.width / 2.0);
        // The pencil is the bubble's own click, not a second mechanism.
        assert_eq!(click(&mut tree, user), vec![UiAction::Clicked { id: "message_edit:0".into() }]);
    }
}

#[test]
fn older_turns_show_their_row_on_hover_and_nothing_moves() {
    for viewport in [WIDE, NARROW] {
        let state = two_turns();
        let (_context, mut tree) = native_tree(root(&state, viewport));
        let rest = tree.layout(viewport);
        let (older, last) = (button(&rest, "turn_copy:1").unwrap(), button(&rest, "turn_copy:7").unwrap());
        assert!(!painted(&rest, older, Icon::Copy), "an older turn is quiet at rest");
        assert!(painted(&rest, last, Icon::Copy), "the last turn always shows its row");
        assert!(texts(&rest).contains("hace 1 min · 2 s · opus-5.5"));
        assert!(!texts(&rest).contains("hace 5 min"));
        let bubble = button(&rest, "user_copy:0").unwrap();
        assert!(!painted(&rest, bubble, Icon::Copy));

        // Anywhere over the turn: here, its first line of prose.
        let prose = rest
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text.contains("Voy a mirar") => Some(*position),
                _ => None,
            })
            .unwrap();
        let hovered = hover(&mut tree, viewport, Point::new(prose.x + 4.0, prose.y + 4.0));
        assert!(painted(&hovered, older, Icon::Copy));
        assert!(texts(&hovered).contains("hace 5 min · 14 s · opus-5.5"));
        assert!(!painted(&hovered, bubble, Icon::Copy), "the bubble is its own region");
        assert_eq!(hovered.regions, rest.regions, "the row was already in its place");

        let over_bubble = hover(&mut tree, viewport, Point::new(bubble.x - 40.0, bubble.y + 12.0));
        assert!(painted(&over_bubble, bubble, Icon::Copy) && !painted(&over_bubble, older, Icon::Copy));
        assert_eq!(over_bubble.regions, rest.regions);
    }
}

#[test]
fn copy_takes_the_prose_of_the_turn_and_shows_a_check() {
    let mut state = two_turns();
    assert_eq!(turn_items(&state.transcript, 1).len(), 5);
    assert_eq!(turn_items(&state.transcript, 7).len(), 2);
    assert!(turn_items(&state.transcript, 99).is_empty());
    let expected = format!("Voy a mirar el cálculo.\n\n{ANSWER}");
    assert_eq!(turn_markdown(turn_items(&state.transcript, 1)), expected);
    assert_eq!(apply_action(&mut state, "turn_copy:1"), Some(Effect::Copy(expected)));
    let (_context, mut tree) = native_tree(root(&state, WIDE));
    let rest = tree.layout(WIDE);
    let copy = button(&rest, "turn_copy:1").unwrap();
    let frame = hover(&mut tree, WIDE, center(copy));
    assert!(painted(&frame, copy, Icon::Check) && !painted(&frame, copy, Icon::Copy));
    assert!(painted(&frame, button(&frame, "turn_copy:7").unwrap(), Icon::Copy), "only the one copied");

    // A user's message copies as it was typed, or as stored when that is unknown.
    assert_eq!(apply_action(&mut state, "user_copy:0"), Some(Effect::Copy("Lo que escribí".into())));
    state.message_edit.turns.clear();
    assert_eq!(apply_action(&mut state, "user_copy:6"), Some(Effect::Copy("Segundo pedido: corregilo.".into())));
    assert_eq!(apply_action(&mut state, "user_copy:1"), Some(Effect::None), "not a user message");
    assert_eq!(apply_action(&mut state, "tool:cmd-1"), None, "not ours");
}

#[test]
fn the_menu_offers_activity_and_a_retry_only_where_they_are_real() {
    let mut state = two_turns();
    assert_eq!(apply_action(&mut state, "turn_actions:1:more"), Some(Effect::None));
    let (_context, mut tree) = native_tree(root(&state, WIDE));
    let frame = tree.layout(WIDE);
    assert!(texts(&frame).contains("Ver actividad del turno") && texts(&frame).contains("Reintentar"));
    assert!(painted(&frame, button(&frame, "turn_copy:1").unwrap(), Icon::Copy), "an open menu holds its row");
    let activity = button(&frame, "turn_activity:cmd-1").unwrap();
    assert_eq!(click(&mut tree, activity), vec![UiAction::Clicked { id: "turn_activity:cmd-1".into() }]);
    assert_eq!(apply_action(&mut state, "turn_activity:cmd-1"), Some(Effect::Activity("cmd-1".into())));
    assert_eq!(state.message_actions.menu, None, "choosing closes the menu");
    assert_eq!(apply_action(&mut state, "turn_retry:0"), Some(Effect::Retry(0)));

    // The second turn ran no tool: its menu has the retry alone.
    apply_action(&mut state, "turn_actions:7:more");
    let (_context, mut tree) = native_tree(root(&state, WIDE));
    let frame = tree.layout(WIDE);
    assert!(button(&frame, "turn_retry:1").is_some() && !texts(&frame).contains("Ver actividad"));
    assert_eq!(apply_action(&mut state, "turn_actions:7:more:dismiss"), Some(Effect::None));
    assert_eq!(state.message_actions.menu, None);

    // A retry that would lose attachments, or that a permission blocks, is passive.
    for block in [0, 1] {
        let mut state = two_turns();
        if block == 0 {
            state.message_edit.turns[1].drops_attachments = true;
        } else {
            state.pending_permissions = fixture(false).pending_permissions;
            state.message_edit.editing = Some(EditingMessage { user_row: 0, sequence: 1, drops_attachments: false, original: String::new() });
        }
        apply_action(&mut state, "turn_actions:7:more");
        let (_context, mut tree) = native_tree(root(&state, WIDE));
        let frame = tree.layout(WIDE);
        assert!(button(&frame, "turn_retry:1").is_none(), "{block}");
    }

    // Sessions whose history is never rewound have nothing to retry or edit.
    let mut state = two_turns();
    state.message_edit.turns.clear();
    let (_context, mut tree) = native_tree(root(&state, WIDE));
    let frame = tree.layout(WIDE);
    assert!(button(&frame, "turn_actions:7:more").is_none() && button(&frame, "user_edit:6").is_none());
    assert!(button(&frame, "turn_copy:7").is_some() && button(&frame, "user_copy:6").is_some());
}

#[test]
fn a_message_that_cannot_be_edited_now_says_why_and_stays_passive() {
    let mut state = two_turns();
    state.background_run_selected = true;
    let (_context, mut tree) = native_tree(root(&state, WIDE));
    let frame = tree.layout(WIDE);
    assert!(button(&frame, "user_edit:0").is_none() && button(&frame, "message_edit:0").is_none());
    let copy = button(&frame, "user_copy:0").unwrap();
    // The passive pencil keeps its place beside copy, and its hint.
    tree.handle_event(UiEvent::PointerMove(Point::new(copy.x + 26.0 + 12.0, copy.y + 12.0)));
    let start = std::time::Instant::now();
    tree.layout_at(WIDE, start);
    let hinted = texts(&tree.layout_at(WIDE, start + std::time::Duration::from_secs(2)));
    assert!(hinted.contains("Editar y reenviar") && hinted.contains("Una automatización está ejecutando esta tarea."), "{hinted}");
}

#[test]
fn a_turn_in_progress_has_no_row_and_one_that_ended_badly_offers_the_retry() {
    // Streaming: the older turn keeps its row, the live one has none.
    let mut state = two_turns();
    state.running = true;
    if let Some(TranscriptItem::Assistant { streaming, .. }) = state.transcript.last_mut() {
        *streaming = true;
    }
    let (_context, mut tree) = native_tree(root(&state, WIDE));
    let frame = tree.layout(WIDE);
    assert!(button(&frame, "turn_copy:1").is_some() && button(&frame, "turn_copy:7").is_none());
    assert!(button(&frame, "turn_stop").is_some() && button(&frame, "user_copy:6").is_some());

    // Failed with nothing written: no copy to make, the retry in plain view.
    let mut state = two_turns();
    state.transcript.truncate(7);
    state.transcript.push(model(ActivityStatus::Failed));
    state.transcript.push(TranscriptItem::Notice { message: "El proveedor cortó la conexión".into(), is_error: true });
    let (_context, mut tree) = native_tree(root(&state, WIDE));
    tree.scroll_to_end("transcript");
    let frame = tree.layout(WIDE);
    assert!(button(&frame, "turn_copy:7").is_none(), "nothing to copy");
    let retry = button(&frame, "turn_retry:1").expect("the retry is a button of the row");
    assert!(painted(&frame, retry, Icon::Refresh));
    assert!(button(&frame, "turn_actions:7:more").is_none());

    // Cancelled half way: what exists can be copied.
    state.transcript.insert(8, prose("Empecé a corregirlo"));
    let (_context, mut tree) = native_tree(root(&state, WIDE));
    tree.scroll_to_end("transcript");
    let frame = tree.layout(WIDE);
    assert!(button(&frame, "turn_copy:7").is_some() && button(&frame, "turn_retry:1").is_some());
}

#[test]
#[ignore = "writes PNG/SVG previews under docs/ui-separation/shots/message-actions"]
fn export_message_actions_visual_review() {
    let directory = Path::new(SHOTS);
    std::fs::create_dir_all(directory).unwrap();
    for viewport in [WIDE, NARROW] {
        let state = two_turns();
        let (context, mut tree) = native_tree(root(&state, viewport));
        let rest = tree.layout(viewport);
        export(&rest, directory, "a1-rest", viewport);
        let older = button(&rest, "turn_copy:1").unwrap();
        export(&hover(&mut tree, viewport, Point::new(older.x + 200.0, older.y - 30.0)), directory, "a2-hover-older-turn", viewport);
        let bubble = button(&rest, "user_copy:0").unwrap();
        export(&hover(&mut tree, viewport, Point::new(bubble.x - 40.0, bubble.y + 12.0)), directory, "a3-hover-user-bubble", viewport);
        context.end_pass().textures_delta.clear();

        let mut state = two_turns();
        apply_action(&mut state, "turn_copy:7");
        let (context, mut tree) = native_tree(root(&state, viewport));
        export(&tree.layout(viewport), directory, "a4-copy-done", viewport);
        context.end_pass().textures_delta.clear();

        let mut state = two_turns();
        apply_action(&mut state, "turn_actions:1:more");
        let (context, mut tree) = native_tree(root(&state, viewport));
        export(&tree.layout(viewport), directory, "a5-menu-open", viewport);
        context.end_pass().textures_delta.clear();

        let mut state = two_turns();
        state.transcript.truncate(7);
        state.transcript.push(model(ActivityStatus::Failed));
        state.transcript.push(prose("Empecé a corregirlo"));
        state.transcript.push(TranscriptItem::Notice { message: "El proveedor cortó la conexión".into(), is_error: true });
        let (context, mut tree) = native_tree(root(&state, viewport));
        tree.scroll_to_end("transcript");
        export(&tree.layout(viewport), directory, "a6-failed-turn", viewport);
        context.end_pass().textures_delta.clear();

        let mut state = two_turns();
        state.running = true;
        if let Some(TranscriptItem::Assistant { streaming, .. }) = state.transcript.last_mut() {
            *streaming = true;
        }
        let (context, mut tree) = native_tree(root(&state, viewport));
        let frame = tree.layout(viewport);
        let bubble = button(&frame, "user_copy:6").unwrap();
        export(&hover(&mut tree, viewport, Point::new(bubble.x - 40.0, bubble.y + 12.0)), directory, "a7-running-hover-bubble", viewport);
        context.end_pass().textures_delta.clear();
    }
}
