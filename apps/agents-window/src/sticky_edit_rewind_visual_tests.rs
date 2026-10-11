//! The pinned user message, from the native tree with bundled font metrics.
//! Fixtures are local view projections; these tests never start an engine.

use super::{
    plan_visual_tests::{export, fixture, native_tree, root},
    HEADER_HEIGHT, USER_PIN_HEIGHT, USER_PIN_INSET,
};
use crate::{
    message_edit::{EditableTurn, EditingMessage, RewindConfirm, RewindUndoView},
    session_changes::{OpaqueEffects, RevertSkip},
    transcript::TranscriptItem,
    view::ViewState,
};
use openide_ui::{DrawCommand, Element, Frame, HitKind, Key, Point, Rect, Size, UiAction, UiEvent, UiTree};
use std::path::Path;

const VIEWPORT: Size = Size::new(900.0, 700.0);
const SHOTS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ui-separation/shots/sticky-edit-rewind"
);

/// Two turns, each long enough to scroll its own message out.
fn two_turns(first_prompt: &str) -> ViewState {
    let mut state = fixture(false);
    state.transcript.clear();
    for (turn, prompt) in [first_prompt, "Segundo pedido: revisá las pruebas."].into_iter().enumerate() {
        state.transcript.push(TranscriptItem::User { content: prompt.into(), images: vec![] });
        for part in 0..24 {
            state.transcript.push(TranscriptItem::Assistant {
                content: format!("Respuesta {turn}.{part}: el proyecto conserva sus cambios existentes y la conversación sigue disponible al desplazarse."),
                streaming: false,
            });
        }
    }
    state
}

fn long_prompt() -> String {
    (1..=14)
        .map(|line| format!("Línea {line} del pedido: revisá este módulo con cuidado."))
        .collect::<Vec<_>>()
        .join("\n")
}

fn transcript(frame: &Frame) -> Rect {
    frame.regions.iter().find(|region| region.id == "transcript").unwrap().rect
}

/// The pinned message blocks the rows under it with a second region of the
/// scroll area.
fn pinned(frame: &Frame) -> Option<Rect> {
    let mut regions = frame
        .regions
        .iter()
        .filter(|region| region.id == "transcript" && region.kind == HitKind::Scroll);
    regions.next();
    regions.next().map(|region| region.rect)
}

fn text_top(frame: &Frame, needle: &str) -> Option<f32> {
    frame.commands.iter().rev().find_map(|command| match command {
        DrawCommand::Text { text, position, .. } if text.contains(needle) => Some(position.y),
        _ => None,
    })
}

fn scroll(tree: &mut UiTree, by: f32) -> Frame {
    let area = transcript(&tree.layout(VIEWPORT));
    tree.handle_event(UiEvent::ScrollDirect {
        position: Point::new(area.x + area.width / 2.0, area.y + 200.0),
        delta_y: -by,
    });
    tree.layout(VIEWPORT)
}

/// Scroll until the second turn's message is `gap` pixels below the pin.
/// Rows outside the viewport are not painted, so this walks there.
fn scroll_second_message_to(tree: &mut UiTree, gap: f32) -> Frame {
    loop {
        let frame = scroll(tree, 4.0);
        let pin = transcript(&frame).y + USER_PIN_INSET;
        if text_top(&frame, "Segundo pedido").is_some_and(|top| top <= pin + gap) {
            return frame;
        }
    }
}

#[test]
fn user_message_stays_pinned_while_its_turn_scrolls_under_it() {
    let state = two_turns("Primer pedido: revisá el proyecto.");
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    assert_eq!(pinned(&tree.layout(VIEWPORT)), None, "nothing is pinned before scrolling");
    let frame = scroll(&mut tree, 300.0);
    let bubble = pinned(&frame).expect("the turn's message is pinned");
    assert_eq!(bubble.y, HEADER_HEIGHT + USER_PIN_INSET);
    let label = text_top(&frame, "Primer pedido").unwrap();
    assert!(label > bubble.y && label < bubble.y + bubble.height);
    // It is painted after the header fade, so the fade does not veil it.
    let fade = frame
        .commands
        .iter()
        .position(|command| matches!(command, DrawCommand::FillVerticalGradient { top, bottom, .. } if top.a > bottom.a))
        .unwrap();
    let text = frame
        .commands
        .iter()
        .rposition(|command| matches!(command, DrawCommand::Text { text, .. } if text.contains("Primer pedido")))
        .unwrap();
    assert!(text > fade);
}

#[test]
fn next_turn_message_pushes_the_pinned_one_out() {
    let state = two_turns("Primer pedido: revisá el proyecto.");
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    // The second message is arriving: the first has given up part of its place.
    let frame = scroll_second_message_to(&mut tree, 40.0);
    let first = pinned(&frame).unwrap();
    assert!(first.y < HEADER_HEIGHT + USER_PIN_INSET);
    assert!(text_top(&frame, "Segundo pedido").unwrap() > first.y + first.height);
    // Past it, the second message holds the pin.
    let frame = scroll(&mut tree, 200.0);
    let second = pinned(&frame).unwrap();
    assert_eq!(second.y, HEADER_HEIGHT + USER_PIN_INSET);
    let label = text_top(&frame, "Segundo pedido").unwrap();
    assert!(label > second.y && label < second.y + second.height);
}

#[test]
fn long_prompt_gives_up_height_while_pinned() {
    let state = two_turns(&long_prompt());
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = scroll(&mut tree, 600.0);
    let bubble = pinned(&frame).unwrap();
    assert_eq!(bubble.height, USER_PIN_HEIGHT);
    assert!(text_top(&frame, "Línea 1 del pedido").is_some());
    // Back at the start the whole prompt is there.
    tree.scroll_to_start("transcript");
    let frame = tree.layout(VIEWPORT);
    assert_eq!(pinned(&frame), None);
    assert!(text_top(&frame, "Línea 14 del pedido").is_some());
}

#[test]
fn running_turn_message_offers_stop_and_an_idle_one_does_not() {
    let mut state = two_turns("Primer pedido: revisá el proyecto.");
    let stop = |frame: &Frame| {
        frame
            .regions
            .iter()
            .filter(|region| region.id == "turn_stop" && region.kind == HitKind::Button)
            .map(|region| region.rect)
            .collect::<Vec<_>>()
    };
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    tree.scroll_to_end("transcript");
    assert!(stop(&tree.layout(VIEWPORT)).is_empty());

    // One running turn, scrolled so that its message is pinned.
    state.transcript.truncate(25);
    state.running = true;
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = scroll(&mut tree, 300.0);
    let bubble = pinned(&frame).unwrap();
    let stops = stop(&frame);
    assert_eq!(stops.len(), 1);
    assert!(stops[0].y >= bubble.y && stops[0].y + stops[0].height <= bubble.y + bubble.height);
    assert_eq!(
        frame.hit_test(Point::new(stops[0].x + stops[0].width / 2.0, stops[0].y + stops[0].height / 2.0)),
        Some("turn_stop")
    );

    // With two turns only the running one, the last, stops it.
    let mut state = two_turns("Primer pedido: revisá el proyecto.");
    state.running = true;
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    assert!(stop(&tree.layout(VIEWPORT)).is_empty());
    tree.scroll_to_end("transcript");
    assert_eq!(stop(&tree.layout(VIEWPORT)).len(), 1);
}

/// Both turns of `two_turns` as the host publishes them for a native session.
fn editable(mut state: ViewState) -> ViewState {
    state.selected_backend = "native".into();
    state.message_edit.turns = (0..2)
        .map(|user_row| EditableTurn {
            user_row,
            sequence: 1 + user_row as u64 * 30,
            text: Some("Texto escrito".into()),
            drops_attachments: false,
        })
        .collect();
    state
}

fn editing(mut state: ViewState, user_row: usize, drops_attachments: bool) -> ViewState {
    state.prompt = "Primer pedido: revisá el proyecto y las pruebas.".into();
    state.message_edit.editing = Some(EditingMessage { user_row, sequence: 1, drops_attachments });
    state
}

fn offers(state: &ViewState) -> Vec<String> {
    let (_context, mut tree) = native_tree(root(state, VIEWPORT));
    let mut ids: Vec<_> = tree
        .layout(VIEWPORT)
        .regions
        .iter()
        .filter(|region| region.id.starts_with("message_edit:"))
        .map(|region| region.id.clone())
        .collect();
    ids.dedup();
    ids
}

#[test]
fn only_messages_that_can_be_rewound_to_offer_an_edit() {
    let state = editable(two_turns("Primer pedido: revisá el proyecto."));
    assert_eq!(offers(&state), ["message_edit:0"], "the second is below the viewport");

    // A steered message is a user row with no turn of its own.
    let mut steered = editable(two_turns("Primer pedido: revisá el proyecto."));
    steered.transcript.insert(1, TranscriptItem::User { content: "Y también esto".into(), images: vec![] });
    steered.message_edit.turns[1].user_row = 2;
    assert_eq!(offers(&steered), ["message_edit:0"]);
    steered.transcript.truncate(2);
    assert_eq!(offers(&steered), ["message_edit:0"], "the steered row itself offers nothing");

    let mut unknown = state_with(&state, |state| state.message_edit.turns[0].text = None);
    assert!(offers(&unknown).is_empty(), "typed text that is not certain");
    unknown.message_edit.turns.clear();
    assert!(offers(&unknown).is_empty(), "an external CLI publishes no turns");

    // A running turn does not close the offer: sending the edit stops it.
    assert_eq!(offers(&state_with(&state, |state| state.running = true)), ["message_edit:0"]);
    assert!(offers(&state_with(&state, |state| state.background_run_selected = true)).is_empty());
    assert!(offers(&state_with(&state, |state| state.selected_backend = "codex".into())).is_empty());
}

fn state_with(base: &ViewState, change: impl FnOnce(&mut ViewState)) -> ViewState {
    let mut state = editable(two_turns("Primer pedido: revisá el proyecto."));
    state.message_edit.turns = base.message_edit.turns.clone();
    change(&mut state);
    state
}

#[test]
fn editing_puts_the_one_composer_over_the_message_and_dims_the_rest() {
    let state = editing(editable(two_turns("Primer pedido: revisá el proyecto.")), 0, true);
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    let prompts: Vec<_> = frame.regions.iter().filter(|region| region.id == "prompt").collect();
    assert_eq!(prompts.len(), 1, "there is never a second composer");
    let area = transcript(&frame);
    let prompt = prompts[0].rect;
    assert!(prompt.y >= area.y && prompt.y + prompt.height < area.y + area.height / 2.0);
    assert_eq!(tree.focused_id(), Some("prompt"));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        DrawCommand::FillRect { rect, color, .. }
            if rect.width >= VIEWPORT.width && rect.height >= VIEWPORT.height && color.a == 170
    )));
    assert!(text_top(&frame, "Los adjuntos del mensaje original no se reenvían").is_some());
    assert!(text_top(&frame, "Editando un mensaje anterior").is_some());
    assert!(offers(&state).is_empty(), "no other message opens meanwhile");

    // Escape and a click outside both cancel.
    let cancel = vec![UiAction::Clicked { id: "message_edit_cancel".into() }];
    assert_eq!(
        tree.handle_event(UiEvent::Key { key: Key::Escape, shift: false, ctrl: false }),
        cancel
    );
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    tree.layout(VIEWPORT);
    let outside = Point::new(VIEWPORT.width / 2.0, area.y + area.height - 40.0);
    tree.handle_event(UiEvent::PointerDown(outside));
    assert_eq!(tree.handle_event(UiEvent::PointerUp(outside)), cancel);
}

#[test]
fn confirmation_offers_cancel_and_keep_files_and_shows_a_refusal() {
    let mut state = editing(editable(two_turns("Primer pedido: revisá el proyecto.")), 0, false);
    state.message_edit.confirm = Some(RewindConfirm {
        later_messages: 1,
        error: Some("Hay un permiso pendiente de respuesta.".into()),
        ..Default::default()
    });
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    for id in ["message_edit_confirm_cancel", "message_edit_keep_files"] {
        assert!(frame.regions.iter().any(|region| region.id == id && region.kind == HitKind::Button), "{id}");
    }
    // Nothing to restore: the only way forward is to send, so "Revertir" is not offered.
    assert!(!frame.regions.iter().any(|region| region.id == "message_edit_revert"));
    assert!(text_top(&frame, "Revertir").is_none());
    assert!(text_top(&frame, "Enviar").is_some());
    assert!(!frame.regions.iter().any(|region| region.id == "prompt"));
    assert!(text_top(&frame, "Hay un permiso pendiente").is_some());
    assert_eq!(
        tree.handle_event(UiEvent::Key { key: Key::Escape, shift: false, ctrl: false }),
        vec![UiAction::Clicked { id: "message_edit_confirm_cancel".into() }]
    );
}

#[test]
fn rewind_notice_belongs_to_its_session_and_waits_for_a_running_turn() {
    let mut state = editable(two_turns("Primer pedido: revisá el proyecto."));
    state.message_edit.undo = Some(RewindUndoView {
        session_id: state.selected_session.clone().unwrap(),
        removed_messages: 2,
        ..Default::default()
    });
    let undo = |state: &ViewState| {
        let (_context, mut tree) = native_tree(root(state, VIEWPORT));
        let frame = tree.layout(VIEWPORT);
        (
            text_top(&frame, "Se rebobinó la conversación").is_some(),
            frame.regions.iter().any(|region| region.id == "message_rewind_undo" && region.kind == HitKind::Button),
        )
    };
    assert_eq!(undo(&state), (true, true));
    state.running = true;
    assert_eq!(undo(&state), (true, false), "the action is passive while a turn runs");
    state.running = false;
    state.message_edit.undo.as_mut().unwrap().session_id = "another".into();
    assert_eq!(undo(&state), (false, false));
}

#[test]
#[ignore = "writes PNG/SVG previews under docs/ui-separation/shots/sticky-edit-rewind"]
fn export_sticky_user_message_visual_review() {
    let directory = Path::new(SHOTS);
    std::fs::create_dir_all(directory).unwrap();
    let state = two_turns("Primer pedido: revisá el proyecto.");
    let (context, mut tree) = native_tree(root(&state, VIEWPORT));
    export(&scroll(&mut tree, 300.0), directory, "s1-pinned-mid-turn", VIEWPORT);
    export(&scroll_second_message_to(&mut tree, 40.0), directory, "s1-hand-off", VIEWPORT);
    context.end_pass().textures_delta.clear();

    let state = two_turns(&long_prompt());
    let (context, mut tree) = native_tree(root(&state, VIEWPORT));
    export(&tree.layout(VIEWPORT), directory, "s1-long-prompt-at-rest", VIEWPORT);
    export(&scroll(&mut tree, 600.0), directory, "s1-long-prompt-pinned", VIEWPORT);
    context.end_pass().textures_delta.clear();

    let mut state = two_turns("Primer pedido: revisá el proyecto.");
    state.running = true;
    let (context, mut tree) = native_tree(root(&state, VIEWPORT));
    tree.scroll_to_end("transcript");
    export(&tree.layout(VIEWPORT), directory, "s1-running-stop", VIEWPORT);
    context.end_pass().textures_delta.clear();
}

#[test]
#[ignore = "writes PNG/SVG previews under docs/ui-separation/shots/sticky-edit-rewind"]
fn export_message_edit_visual_review() {
    let directory = Path::new(SHOTS);
    std::fs::create_dir_all(directory).unwrap();
    for viewport in [Size::new(900.0, 700.0), Size::new(360.0, 700.0)] {
        let state = editing(editable(two_turns("Primer pedido: revisá el proyecto.")), 0, true);
        let (context, mut tree) = native_tree(root(&state, viewport));
        export(&tree.layout(viewport), directory, "s2-edit-mode", viewport);
        context.end_pass().textures_delta.clear();

        let mut state = editing(editable(two_turns("Primer pedido: revisá el proyecto.")), 0, false);
        state.message_edit.confirm = Some(RewindConfirm { later_messages: 1, ..Default::default() });
        let (context, mut tree) = native_tree(root(&state, viewport));
        export(&tree.layout(viewport), directory, "s2-confirm", viewport);
        context.end_pass().textures_delta.clear();

        let mut state = editable(two_turns("Primer pedido: revisá el proyecto."));
        state.message_edit.undo = Some(RewindUndoView {
            session_id: state.selected_session.clone().unwrap(),
            removed_messages: 2,
            ..Default::default()
        });
        let (context, mut tree) = native_tree(root(&state, viewport));
        export(&tree.layout(viewport), directory, "s2-rewind-notice", viewport);
        context.end_pass().textures_delta.clear();
    }
}

fn confirming(confirm: RewindConfirm) -> ViewState {
    let mut state = editing(editable(two_turns("Primer pedido: revisá el proyecto.")), 0, false);
    state.message_edit.confirm = Some(confirm);
    state
}

/// Restorable and skipped files with effects that cannot be taken back.
fn mixed_preflight() -> RewindConfirm {
    RewindConfirm {
        later_messages: 2,
        restorable: (1..=8).map(|index| format!("apps/agents-window/src/modulo_{index}.rs")).collect(),
        skipped: vec![
            ("apps/agents-window/src/session_surface.rs".into(), RevertSkip::ModifiedLater),
            ("docs/notas.md".into(), RevertSkip::ModifiedLater),
            ("assets/captura.png".into(), RevertSkip::IncompleteCapture),
            ("crates/openide-core/src/store.rs".into(), RevertSkip::UnsavedInEditor),
            ("../otro-proyecto/config.toml".into(), RevertSkip::OutsideProject),
        ],
        opaque: OpaqueEffects { commands: 2, mcp: 1, other: 0 },
        ..Default::default()
    }
}

fn buttons(frame: &Frame) -> Vec<&str> {
    let mut ids: Vec<_> = frame
        .regions
        .iter()
        .filter(|region| region.kind == HitKind::Button && region.id.starts_with("message_edit_"))
        .map(|region| region.id.as_str())
        .collect();
    ids.dedup();
    ids
}

#[test]
fn confirmation_is_one_sentence_one_caveat_and_two_buttons_at_any_width() {
    for viewport in [VIEWPORT, Size::new(360.0, 700.0)] {
        let state = confirming(mixed_preflight());
        let (_context, mut tree) = native_tree(root(&state, viewport));
        let frame = tree.layout(viewport);
        for id in ["message_edit_keep_files", "message_edit_revert"] {
            let button = frame.regions.iter().find(|region| region.id == id && region.kind == HitKind::Button);
            let rect = button.unwrap_or_else(|| panic!("{id} at {}", viewport.width)).rect;
            assert!(rect.x >= 0.0 && rect.x + rect.width <= viewport.width, "{id}");
            assert!(rect.y + rect.height <= viewport.height, "{id}");
        }
        // Leaving is the close control and Escape, not a third button in the row.
        assert!(frame.regions.iter().any(|region| region.id == "message_edit_confirm_cancel"));
        assert!(text_top(&frame, "Se quitan los mensajes posteriores a este.").is_some());
        // One caveat only: what cannot be undone outranks the skipped files.
        assert!(text_top(&frame, "Estos turnos también ejecutaron 2 comandos").is_some());
        for absent in ["modulo_1.rs", "no se restauran", "captura incompleta"] {
            assert!(text_top(&frame, absent).is_none(), "{absent} at {}", viewport.width);
        }
    }
}

#[test]
fn effects_that_cannot_be_undone_are_said_even_when_only_keeping_files_is_offered() {
    let warning = "Estos turnos también ejecutaron 2 comandos";
    let state = confirming(RewindConfirm {
        later_messages: 1,
        skipped: vec![("src/main.rs".into(), RevertSkip::ModifiedLater)],
        opaque: OpaqueEffects { commands: 2, mcp: 1, other: 0 },
        ..Default::default()
    });
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    let buttons = buttons(&frame);
    assert!(buttons.contains(&"message_edit_keep_files") && !buttons.contains(&"message_edit_revert"));
    assert!(text_top(&frame, warning).is_some());
    assert!(text_top(&frame, "Enviar").is_some());

    // Without such effects the caveat names why files are left alone.
    let state = confirming(RewindConfirm {
        later_messages: 1,
        skipped: vec![("src/main.rs".into(), RevertSkip::ModifiedLater)],
        ..Default::default()
    });
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    assert!(text_top(&tree.layout(VIEWPORT), "modificado después por ti u otra herramienta").is_some());

    let (_context, mut tree) = native_tree(root(&confirming(mixed_preflight()), VIEWPORT));
    assert!(text_top(&tree.layout(VIEWPORT), warning).is_some());
}

#[test]
fn rewind_notice_reports_restored_and_skipped_files_and_what_an_undo_left() {
    let mut state = editable(two_turns("Primer pedido: revisá el proyecto."));
    state.message_edit.undo = Some(RewindUndoView {
        session_id: state.selected_session.clone().unwrap(),
        removed_messages: 2,
        restored_files: 3,
        skipped_files: vec!["src/main.rs".into(), "docs/notas.md".into()],
        ..Default::default()
    });
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    assert!(text_top(&frame, "Se restauraron 3 archivos. Se omitieron: src/main.rs, docs/notas.md.").is_some());
    assert!(frame.regions.iter().any(|region| region.id == "message_rewind_undo"));

    state.message_edit.undo.as_mut().unwrap().undone = true;
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    assert!(text_top(&frame, "Rebobinado deshecho").is_some());
    assert!(text_top(&frame, "No se volvieron a aplicar porque cambiaron después: src/main.rs").is_some());
    assert!(!frame.regions.iter().any(|region| region.id == "message_rewind_undo"));
    assert!(frame.regions.iter().any(|region| region.id == "message_rewind_undo_dismiss"));
}

#[test]
#[ignore = "writes PNG/SVG previews under docs/ui-separation/shots/sticky-edit-rewind"]
fn export_revert_visual_review() {
    let directory = Path::new(SHOTS);
    std::fs::create_dir_all(directory).unwrap();
    for viewport in [Size::new(900.0, 700.0), Size::new(360.0, 700.0)] {
        let nothing = RewindConfirm {
            later_messages: 1,
            skipped: vec![("src/main.rs".into(), RevertSkip::ModifiedLater)],
            opaque: OpaqueEffects { commands: 1, mcp: 0, other: 0 },
            ..Default::default()
        };
        for (name, confirm) in [("s3-confirm-mixed", mixed_preflight()), ("s3-confirm-nothing-restorable", nothing)] {
            let (context, mut tree) = native_tree(root(&confirming(confirm), viewport));
            export(&tree.layout(viewport), directory, name, viewport);
            context.end_pass().textures_delta.clear();
        }
        let mut state = editable(two_turns("Primer pedido: revisá el proyecto."));
        state.message_edit.undo = Some(RewindUndoView {
            session_id: state.selected_session.clone().unwrap(),
            removed_messages: 2,
            restored_files: 3,
            skipped_files: vec!["apps/agents-window/src/session_surface.rs".into(), "docs/notas.md".into()],
            ..Default::default()
        });
        let (context, mut tree) = native_tree(root(&state, viewport));
        export(&tree.layout(viewport), directory, "s3-revert-notice", viewport);
        context.end_pass().textures_delta.clear();
    }
}

const POLISH_SHOTS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/ui-separation/shots/composer-polish"
);

/// One running turn of `two_turns`, editable, as the host publishes it.
fn running_turn() -> ViewState {
    let mut state = editable(two_turns("Primer pedido: revisá el proyecto."));
    state.transcript.truncate(25);
    state.message_edit.turns.truncate(1);
    state.running = true;
    state
}

#[test]
fn running_turn_message_opens_for_editing_and_its_composer_sends() {
    let state = running_turn();
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    let bubble = frame.regions.iter().find(|region| region.id == "message_edit:0").expect("it is offered").rect;
    let stop = frame.regions.iter().rev().find(|region| region.id == "turn_stop").unwrap().rect;
    // The stop is a circle inside the bubble and keeps its own click.
    assert_eq!((stop.width, stop.height), (24.0, 24.0));
    assert_eq!(frame.hit_test(Point::new(stop.x + 12.0, stop.y + 12.0)), Some("turn_stop"));
    assert_eq!(frame.hit_test(Point::new(bubble.x + 20.0, bubble.y + bubble.height / 2.0)), Some("message_edit:0"));

    // Open: the composer over the message sends, and the stop stays in reach.
    let state = editing(running_turn(), 0, false);
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    assert!(frame.commands.iter().any(|command| matches!(command, DrawCommand::Icon { icon: openide_ui::Icon::ArrowUp, .. })));
    assert!(frame.regions.iter().any(|region| region.id == "run_toggle" && region.kind == HitKind::Button));
    // The stop that comes with the editor is above the veil.
    let stop = frame.regions.iter().rev().find(|region| region.id == "turn_stop" && region.kind == HitKind::Button).unwrap().rect;
    assert_eq!(frame.hit_test(Point::new(stop.x + 12.0, stop.y + 12.0)), Some("turn_stop"));
    assert!(text_top(&frame, "El turno sigue en curso").is_some());
}

#[test]
fn confirmation_while_a_turn_runs_says_it_stops_it_and_then_waits() {
    let mut confirm = mixed_preflight();
    confirm.stops_turn = true;
    let mut state = editing(running_turn(), 0, false);
    state.message_edit.confirm = Some(confirm.clone());
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    assert!(text_top(&frame, "Se detiene el turno en curso").is_some());
    assert!(text_top(&frame, "modulo_1.rs").is_none(), "a provisional list is not shown as final");
    for id in ["message_edit_keep_files", "message_edit_revert"] {
        assert!(buttons(&frame).contains(&id), "{id}");
    }

    confirm.stopping = true;
    state.message_edit.confirm = Some(confirm);
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    assert!(text_top(&frame, "Deteniendo el turno…").is_some());
    let left = buttons(&frame);
    assert!(left.contains(&"message_edit_confirm_cancel"));
    assert!(!left.iter().any(|id| matches!(*id, "message_edit_keep_files" | "message_edit_revert" | "message_edit_stopping")));

    state.message_edit.confirm = Some(RewindConfirm { recomputed: true, ..mixed_preflight() });
    let (_context, mut tree) = native_tree(root(&state, VIEWPORT));
    let frame = tree.layout(VIEWPORT);
    assert!(text_top(&frame, "La lista de archivos cambió al detener el turno").is_some());
}

#[test]
#[ignore = "writes PNG/SVG previews under docs/ui-separation/shots/composer-polish"]
fn export_composer_polish_visual_review() {
    let directory = Path::new(POLISH_SHOTS);
    std::fs::create_dir_all(directory).unwrap();
    let shot = |state: &ViewState, name: &str, viewport: Size, prepare: &dyn Fn(&mut UiTree)| {
        let (context, mut tree) = native_tree(root(state, viewport));
        tree.layout(viewport);
        prepare(&mut tree);
        export(&tree.layout(viewport), directory, name, viewport);
        context.end_pass().textures_delta.clear();
    };
    let none = |_: &mut UiTree| {};

    // The fade under the pinned message, and its hand-off.
    let state = two_turns("Primer pedido: revisá el proyecto.");
    let (context, mut tree) = native_tree(root(&state, VIEWPORT));
    export(&scroll(&mut tree, 306.0), directory, "p1-pinned-fade", VIEWPORT);
    export(&scroll_second_message_to(&mut tree, 70.0), directory, "p1-hand-off-arriving", VIEWPORT);
    export(&scroll_second_message_to(&mut tree, 40.0), directory, "p1-hand-off", VIEWPORT);
    context.end_pass().textures_delta.clear();

    for viewport in [Size::new(900.0, 700.0), Size::new(360.0, 700.0)] {
        // Editing while the turn runs, and its confirmation.
        shot(&running_turn(), "p2-running-bubble", viewport, &|tree| tree.scroll_to_end("transcript"));
        let mut state = editing(running_turn(), 0, false);
        shot(&state, "p2-edit-while-running", viewport, &none);
        let mut confirm = mixed_preflight();
        confirm.later_messages = 0;
        confirm.stops_turn = true;
        state.message_edit.confirm = Some(confirm.clone());
        shot(&state, "p2-confirm-stops-turn", viewport, &none);
        confirm.stopping = true;
        state.message_edit.confirm = Some(confirm);
        shot(&state, "p2-stopping", viewport, &none);
        state.message_edit.confirm = Some(RewindConfirm { recomputed: true, later_messages: 0, ..mixed_preflight() });
        shot(&state, "p2-confirm-recomputed", viewport, &none);

        // The composer's buttons in each state.
        let mut state = two_turns("Primer pedido: revisá el proyecto.");
        state.prompt.clear();
        shot(&state, "p3-composer-idle", viewport, &none);
        state.prompt = "Revisá también las pruebas".into();
        shot(&state, "p3-composer-text", viewport, &none);
        state.composer_dictation = openide_ui::DictationState::Starting;
        shot(&state, "p3-composer-dictation-starting", viewport, &none);
        state.composer_dictation = openide_ui::DictationState::Listening;
        state.composer_transcript = "y después corré la suite completa antes de".into();
        shot(&state, "p3-composer-recording", viewport, &none);
        state.composer_dictation = openide_ui::DictationState::Idle;
        state.composer_transcript.clear();
        state.running = true;
        shot(&state, "p3-composer-running", viewport, &none);

        let mut home = fixture(false);
        home.prompt.clear();
        let (context, mut tree) = native_tree(
            Element::panel(crate::home::build_home(&home, viewport.width)).background(crate::theme::BACKGROUND),
        );
        export(&tree.layout(viewport), directory, "p3-home", viewport);
        context.end_pass().textures_delta.clear();
    }

    // The composer as the IDE's agent column has it.
    let mut state = two_turns("Primer pedido: revisá el proyecto.");
    state.prompt = "Revisá también las pruebas".into();
    let viewport = Size::new(300.0, 160.0);
    for (name, running) in [("p3-ide-compact", false), ("p3-ide-compact-running", true)] {
        state.running = running;
        let mut props = crate::composer_ui::ComposerProps::from_state(&state, 284.0, false, None, "Enviar un mensaje", None);
        props.embedded_in_ide = true;
        let (context, mut tree) = native_tree(
            Element::panel(crate::composer_ui::build_composer(&props)).padding(8.0).background(crate::theme::BACKGROUND),
        );
        export(&tree.layout(viewport), directory, name, viewport);
        context.end_pass().textures_delta.clear();
    }
}
