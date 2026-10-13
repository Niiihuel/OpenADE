//! Projection regressions use recorded events only; no model or network runs.

use super::*;
use openide_core::{AgentMessage, ToolCall};
use serde_json::json;

fn event(sequence: u64, kind: AgentEventKind) -> AgentEvent {
    AgentEvent::new("stream-session", sequence, kind)
}

fn started() -> Vec<AgentEvent> {
    vec![
        event(
            1,
            AgentEventKind::TurnStarted {
                prompt: "Inspect".into(),
            },
        ),
        event(
            2,
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "fixture".into(),
            },
        ),
    ]
}

fn stream(update: ModelStreamEvent) -> AgentEventKind {
    AgentEventKind::ModelStreamUpdate { step: 1, update }
}

fn text(content: &str) -> AgentEventKind {
    stream(ModelStreamEvent::TextDelta {
        content: content.into(),
    })
}

fn reasoning(item_id: &str, content: &str) -> AgentEventKind {
    stream(ModelStreamEvent::ReasoningDelta {
        item_id: item_id.into(),
        content: content.into(),
    })
}

fn preview() -> AgentEventKind {
    stream(ModelStreamEvent::ToolCallDelta {
        item_id: "item-read".into(),
        call_id: Some("call-read".into()),
        name: Some("read_file".into()),
        arguments_delta: r#"{"path":"README.md"}"#.into(),
    })
}

#[test]
fn file_argument_previews_continue_beyond_four_kib_but_keep_request_and_row_caps() {
    let mut projection = TranscriptProjection::from_live_events(&started());
    for index in 0..12 {
        projection.apply_kind(&stream(ModelStreamEvent::ToolCallDelta {
            item_id: format!("item-file-{index}"),
            call_id: Some(format!("call-file-{index}")),
            name: Some("write_file".into()),
            arguments_delta: format!(
                r#"{{"path":"src/{index}.rs","content":"{}"}}"#,
                "🦀".repeat(12_000)
            ),
        }));
    }
    let rows: Vec<_> = projection
        .items()
        .iter()
        .filter_map(|row| match row {
            TranscriptItem::ToolPreview {
                arguments,
                truncated,
                ..
            } => Some((arguments, *truncated)),
            _ => None,
        })
        .collect();
    assert!(rows[0].0.len() > 4 * 1024);
    assert!(rows
        .iter()
        .all(|(arguments, truncated)| arguments.len() <= MAX_TOOL_PREVIEW_BYTES && *truncated));
    assert!(
        rows.iter()
            .map(|(arguments, _)| arguments.len())
            .sum::<usize>()
            <= MAX_MODEL_STREAM_BYTES
    );
    assert_eq!(projection.model_stream_bytes, MAX_MODEL_STREAM_BYTES);
}

fn call() -> ToolCall {
    ToolCall {
        id: "call-read".into(),
        name: "read_file".into(),
        arguments: json!({ "path": "README.md" }),
    }
}

fn usage() -> ModelTokenUsage {
    ModelTokenUsage {
        input_tokens: 12,
        output_tokens: 8,
        total_tokens: 20,
        reasoning_tokens: Some(3),
        cached_input_tokens: Some(2),
    }
}

fn commit(content: &str, tool_calls: Vec<ToolCall>) -> AgentEventKind {
    AgentEventKind::ModelResponseCommitted {
        step: 1,
        assistant: AgentMessage::assistant(content, tool_calls),
        response_id: Some("response-fixture".into()),
        usage: Some(usage()),
    }
}

fn append(events: &mut Vec<AgentEvent>, kind: AgentEventKind) {
    events.push(event(events.len() as u64 + 1, kind));
}

fn assistant_rows(projection: &TranscriptProjection) -> Vec<(&str, bool)> {
    projection
        .items
        .iter()
        .filter_map(|row| match row {
            TranscriptItem::Assistant { content, streaming } => {
                Some((content.as_str(), *streaming))
            }
            _ => None,
        })
        .collect()
}

fn visible_order(projection: &TranscriptProjection) -> Vec<(&str, &str)> {
    projection
        .items
        .iter()
        .filter_map(|row| match row {
            TranscriptItem::Assistant { content, .. } => Some(("text", content.as_str())),
            TranscriptItem::Reasoning { content, .. } => Some(("reasoning", content.as_str())),
            TranscriptItem::ToolPreview { arguments, .. } => Some(("preview", arguments.as_str())),
            TranscriptItem::Tool { name, .. } => Some(("tool", name.as_str())),
            _ => None,
        })
        .collect()
}

fn assert_indices(projection: &TranscriptProjection) {
    for index in projection
        .active_assistant_rows
        .iter()
        .chain(&projection.committed_final_assistant)
    {
        assert!(matches!(
            projection.items.get(*index),
            Some(TranscriptItem::Assistant { .. })
        ));
    }
    for index in projection
        .model_reasoning_rows
        .values()
        .chain(&projection.model_reasoning_segments)
    {
        assert!(matches!(
            projection.items.get(*index),
            Some(TranscriptItem::Reasoning { .. })
        ));
    }
    for index in projection.model_tool_preview_rows.values() {
        assert!(matches!(
            projection.items.get(*index),
            Some(TranscriptItem::ToolPreview { .. })
        ));
    }
    for index in projection.tool_rows.values() {
        assert!(matches!(
            projection.items.get(*index),
            Some(TranscriptItem::Tool { .. })
        ));
    }
    for index in projection.hook_rows.values() {
        assert!(matches!(
            projection.items.get(*index),
            Some(TranscriptItem::Hook(_))
        ));
    }
    if let Some(index) = projection.active_model {
        assert!(matches!(
            projection.items.get(index),
            Some(TranscriptItem::Activity(TranscriptActivity::Model { .. }))
        ));
    }
    if let Some(index) = projection.model_usage_row {
        assert!(matches!(
            projection.items.get(index),
            Some(TranscriptItem::ModelUsage { .. })
        ));
    }
    if let Some(index) = projection.stdout_row {
        assert!(matches!(
            projection.items.get(index),
            Some(TranscriptItem::ProcessOutput {
                stream: OutputStream::Stdout,
                ..
            })
        ));
    }
}

#[test]
fn text_reasoning_text_preserves_stream_and_exact_snapshot_order() {
    let mut events = started();
    append(&mut events, text("A"));
    append(&mut events, reasoning("reason-1", "B"));
    append(&mut events, text("C"));
    let mut projection = TranscriptProjection::from_live_events(&events);
    assert_eq!(
        visible_order(&projection),
        [("text", "A"), ("reasoning", "B"), ("text", "C")]
    );
    assert_eq!(assistant_rows(&projection), [("A", false), ("C", true)]);
    projection.apply_event(&event(6, commit("AC", vec![])));
    assert_eq!(
        visible_order(&projection),
        [("text", "A"), ("reasoning", "B"), ("text", "C")]
    );
    assert_eq!(assistant_rows(&projection), [("A", false), ("C", false)]);
    assert_indices(&projection);
}

#[test]
fn repeated_reasoning_identity_creates_only_consecutive_segments_and_seals_all() {
    let mut events = started();
    append(&mut events, text("A"));
    append(&mut events, reasoning("same", "B"));
    append(&mut events, reasoning("same", "b"));
    append(&mut events, text("C"));
    append(&mut events, reasoning("same", "D"));
    append(&mut events, text("E"));
    append(&mut events, commit("ACE", vec![]));
    let projection = TranscriptProjection::from_live_events(&events);
    assert_eq!(
        visible_order(&projection),
        [
            ("text", "A"),
            ("reasoning", "Bb"),
            ("text", "C"),
            ("reasoning", "D"),
            ("text", "E")
        ]
    );
    let rows = projection
        .items
        .iter()
        .filter_map(|row| match row {
            TranscriptItem::Reasoning { id, status, .. } => Some((id, status)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].0, rows[1].0);
    assert!(rows
        .iter()
        .all(|(_, status)| **status == ModelStreamStatus::Committed));
    assert_eq!(projection.model_reasoning_segments.len(), 2);
    assert_indices(&projection);
}

#[test]
fn preview_separates_text_then_confirmed_start_reuses_its_position() {
    let mut events = started();
    append(&mut events, text("Before"));
    append(&mut events, preview());
    append(&mut events, text("After"));
    append(&mut events, commit("BeforeAfter", vec![call()]));
    let mut projection = TranscriptProjection::from_live_events(&events);
    assert_eq!(
        visible_order(&projection),
        [
            ("text", "Before"),
            ("preview", r#"{"path":"README.md"}"#),
            ("text", "After")
        ]
    );
    projection.apply_event(&event(7, AgentEventKind::ToolStarted { tool_call: call() }));
    assert_eq!(
        visible_order(&projection),
        [("text", "Before"), ("tool", "read_file"), ("text", "After")]
    );
    assert!(!projection
        .items
        .iter()
        .any(|row| matches!(row, TranscriptItem::ToolPreview { .. })));
    projection.apply_event(&event(
        8,
        AgentEventKind::ToolFinished {
            tool_call_id: "call-read".into(),
            output: "Read".into(),
            is_error: false,
        },
    ));
    assert!(projection.items.iter().any(|row| matches!(row, TranscriptItem::Tool { output: Some(output), activity: ActivityStatus::Completed, .. } if output == "Read")));
    assert_indices(&projection);
}

#[test]
fn exact_snapshot_display_repeat_and_turn_completion_never_duplicate_segments() {
    let mut events = started();
    append(&mut events, text("ñ🙂"));
    append(&mut events, reasoning("reason-1", "Why"));
    append(&mut events, text("終わり"));
    append(&mut events, commit("ñ🙂終わり", vec![]));
    append(
        &mut events,
        AgentEventKind::AssistantMessage {
            content: "ñ🙂終わり".into(),
        },
    );
    append(
        &mut events,
        AgentEventKind::AssistantMessage {
            content: "untrusted duplicate display".into(),
        },
    );
    append(
        &mut events,
        AgentEventKind::TurnCompleted {
            response: "ñ🙂終わり".into(),
            steps: 1,
        },
    );
    let live = TranscriptProjection::from_live_events(&events);
    let reopened = TranscriptProjection::from_events(&events);
    assert_eq!(assistant_rows(&live), [("ñ🙂", false), ("終わり", false)]);
    assert_eq!(live.items(), reopened.items());
}

#[test]
fn mismatched_snapshot_is_authoritative_and_repairs_all_later_row_indices() {
    let mut events = started();
    append(&mut events, text("First"));
    append(&mut events, reasoning("reason-1", "Reason"));
    append(&mut events, text("Second"));
    append(&mut events, preview());
    append(&mut events, text("Third"));
    append(
        &mut events,
        stream(ModelStreamEvent::Usage { usage: usage() }),
    );
    append(
        &mut events,
        AgentEventKind::ProcessOutput {
            stream: OutputStream::Stdout,
            text: "Process".into(),
        },
    );
    let mut projection = TranscriptProjection::from_live_events(&events);
    assert_indices(&projection);
    projection.apply_event(&event(10, commit("Authoritative", vec![call()])));
    assert_eq!(assistant_rows(&projection), [("Authoritative", false)]);
    assert_eq!(
        visible_order(&projection),
        [
            ("text", "Authoritative"),
            ("reasoning", "Reason"),
            ("preview", r#"{"path":"README.md"}"#)
        ]
    );
    assert_indices(&projection);
    assert!(projection.apply_kind(&AgentEventKind::ProcessOutput {
        stream: OutputStream::Stdout,
        text: " tail".into()
    }));
    assert!(projection.items.iter().any(
        |row| matches!(row, TranscriptItem::ProcessOutput { text, .. } if text == "Process tail")
    ));
    projection.apply_kind(&AgentEventKind::ToolStarted { tool_call: call() });
    projection.apply_kind(&AgentEventKind::ToolFinished {
        tool_call_id: "call-read".into(),
        output: "Read".into(),
        is_error: false,
    });
    assert_indices(&projection);
}

#[test]
fn snapshot_matching_first_segment_still_removes_later_provisional_rows() {
    let mut projection = TranscriptProjection::new();
    projection.apply_kind(&AgentEventKind::ModelRequestStarted {
        step: 1,
        model: "fixture".into(),
    });
    projection.apply_kind(&text("A"));
    projection.apply_kind(&reasoning("reason-1", "B"));
    projection.apply_kind(&text("discard"));
    assert!(projection.assistant_message("A"));
    assert_eq!(
        visible_order(&projection),
        [("text", "A"), ("reasoning", "B")]
    );
    assert_indices(&projection);
}

#[test]
fn empty_authoritative_text_removes_every_provisional_segment() {
    let mut events = started();
    append(&mut events, text("First"));
    append(&mut events, reasoning("reason-1", "Reason"));
    append(&mut events, text("Second"));
    append(&mut events, preview());
    append(&mut events, text("Third"));
    append(
        &mut events,
        stream(ModelStreamEvent::Usage { usage: usage() }),
    );
    let mut projection = TranscriptProjection::from_live_events(&events);
    projection.apply_event(&event(9, commit("", vec![call()])));
    assert!(assistant_rows(&projection).is_empty());
    assert_eq!(
        visible_order(&projection),
        [
            ("reasoning", "Reason"),
            ("preview", r#"{"path":"README.md"}"#)
        ]
    );
    assert_indices(&projection);
    projection.apply_kind(&AgentEventKind::ToolStarted { tool_call: call() });
    projection.apply_kind(&AgentEventKind::ToolFinished {
        tool_call_id: "call-read".into(),
        output: "Read".into(),
        is_error: false,
    });
    assert!(projection.items.iter().any(|row| matches!(
        row,
        TranscriptItem::Tool {
            activity: ActivityStatus::Completed,
            ..
        }
    )));
    assert_indices(&projection);
}

#[test]
fn cancel_and_failure_preserve_partial_segments_and_seal_all_provisional_rows() {
    for (terminal, expected) in [
        (AgentEventKind::TurnCancelled, ModelStreamStatus::Cancelled),
        (
            AgentEventKind::TurnFailed {
                error: "Fixture failure".into(),
            },
            ModelStreamStatus::Failed,
        ),
    ] {
        let mut events = started();
        append(&mut events, text("A"));
        append(&mut events, reasoning("same", "B"));
        append(&mut events, text("C"));
        append(&mut events, reasoning("same", "D"));
        append(&mut events, preview());
        append(&mut events, text("E"));
        append(&mut events, terminal);
        let projection = TranscriptProjection::from_events(&events);
        assert_eq!(
            assistant_rows(&projection),
            [("A", false), ("C", false), ("E", false)]
        );
        assert!(projection
            .items
            .iter()
            .filter_map(|row| match row {
                TranscriptItem::Reasoning { status, .. }
                | TranscriptItem::ToolPreview { status, .. } => Some(status),
                _ => None,
            })
            .all(|status| *status == expected));
        assert!(!projection
            .items
            .iter()
            .any(|row| matches!(row, TranscriptItem::Tool { .. })));
        assert_eq!(
            projection
                .items
                .iter()
                .filter(|row| matches!(row, TranscriptItem::Notice { .. }))
                .count(),
            1
        );
        assert_indices(&projection);
    }
}

#[test]
fn reopen_seals_unfinished_segments_but_live_child_replay_keeps_current_stream() {
    let mut events = started();
    append(&mut events, text("A"));
    append(&mut events, reasoning("same", "B"));
    append(&mut events, text("C"));
    let live = TranscriptProjection::from_live_events(&events);
    assert_eq!(assistant_rows(&live), [("A", false), ("C", true)]);
    assert!(live.items.iter().any(|row| matches!(
        row,
        TranscriptItem::Reasoning {
            status: ModelStreamStatus::Receiving,
            ..
        }
    )));
    assert!(!live
        .items
        .iter()
        .any(|row| matches!(row, TranscriptItem::Notice { .. })));
    assert!(live.active_model.is_some());
    let reopened = TranscriptProjection::from_events(&events);
    assert_eq!(assistant_rows(&reopened), [("A", false), ("C", false)]);
    assert!(reopened.items.iter().any(|row| matches!(
        row,
        TranscriptItem::Reasoning {
            status: ModelStreamStatus::Interrupted,
            ..
        }
    )));
    assert!(reopened
        .items
        .iter()
        .any(|row| matches!(row, TranscriptItem::Notice { is_error: true, .. })));
    assert!(reopened.active_model.is_none());
}

#[test]
fn resume_completion_retains_already_committed_segmented_final_answer() {
    let mut events = started();
    append(&mut events, text("A"));
    append(&mut events, reasoning("same", "B"));
    append(&mut events, text("C"));
    append(&mut events, commit("AC", vec![]));
    let mut reopened = TranscriptProjection::from_events(&events);
    assert_eq!(assistant_rows(&reopened), [("A", false), ("C", false)]);
    assert_eq!(reopened.committed_final_assistant.len(), 2);
    assert_indices(&reopened);
    reopened.apply_event(&event(
        7,
        AgentEventKind::TurnCompleted {
            response: "AC".into(),
            steps: 1,
        },
    ));
    assert_eq!(assistant_rows(&reopened), [("A", false), ("C", false)]);
    assert!(reopened.committed_final_assistant.is_empty());
    assert_indices(&reopened);
}

#[test]
fn legacy_final_messages_reconcile_segmented_deltas_without_response_commit() {
    let mut events = started();
    append(
        &mut events,
        AgentEventKind::AssistantDelta {
            content: "A".into(),
        },
    );
    append(&mut events, reasoning("same", "B"));
    append(
        &mut events,
        AgentEventKind::AssistantDelta {
            content: "C".into(),
        },
    );
    append(
        &mut events,
        AgentEventKind::AssistantMessage {
            content: "AC".into(),
        },
    );
    append(
        &mut events,
        AgentEventKind::TurnCompleted {
            response: "AC".into(),
            steps: 1,
        },
    );
    let projection = TranscriptProjection::from_events(&events);
    assert_eq!(
        visible_order(&projection),
        [("text", "A"), ("reasoning", "B"), ("text", "C")]
    );
    assert_eq!(assistant_rows(&projection), [("A", false), ("C", false)]);
}

#[test]
fn subsequent_request_and_steering_do_not_reconcile_against_previous_segments() {
    let mut events = started();
    append(&mut events, text("Old"));
    append(&mut events, reasoning("same", "Reason"));
    append(&mut events, text("Text"));
    let mut projection = TranscriptProjection::from_live_events(&events);
    projection.apply_event(&event(
        6,
        AgentEventKind::ModelRequestStarted {
            step: 2,
            model: "fixture".into(),
        },
    ));
    projection.apply_event(&event(
        7,
        AgentEventKind::AssistantDelta {
            content: "New".into(),
        },
    ));
    projection.apply_event(&event(
        8,
        AgentEventKind::ModelResponseCommitted {
            step: 2,
            assistant: AgentMessage::assistant("New", vec![]),
            response_id: None,
            usage: None,
        },
    ));
    assert_eq!(
        assistant_rows(&projection),
        [("Old", false), ("Text", false), ("New", false)]
    );
    projection.apply_kind(&AgentEventKind::TurnSteered {
        turn_id: "turn".into(),
        input_id: "input".into(),
        text: "Steer".into(),
    });
    projection.apply_kind(&AgentEventKind::AssistantDelta {
        content: "Steered".into(),
    });
    projection.apply_kind(&AgentEventKind::AssistantMessage {
        content: "Steered".into(),
    });
    assert_eq!(
        assistant_rows(&projection),
        [
            ("Old", false),
            ("Text", false),
            ("New", false),
            ("Steered", false)
        ]
    );
    assert_indices(&projection);
}

#[test]
fn every_reasoning_segment_counts_toward_row_bound_without_reordering_text() {
    let mut events = started();
    for _ in 0..MAX_MODEL_STREAM_ROWS + 2 {
        append(&mut events, reasoning("same", "Reason"));
        append(&mut events, text("Text"));
    }
    let mut projection = TranscriptProjection::from_live_events(&events);
    assert_eq!(
        projection.model_reasoning_segments.len(),
        MAX_MODEL_STREAM_ROWS
    );
    assert_eq!(
        projection
            .items
            .iter()
            .filter(|row| matches!(row, TranscriptItem::Reasoning { .. }))
            .count(),
        MAX_MODEL_STREAM_ROWS
    );
    projection.apply_kind(&AgentEventKind::TurnCancelled);
    assert!(projection
        .items
        .iter()
        .filter_map(|row| match row {
            TranscriptItem::Reasoning { status, .. } => Some(status),
            _ => None,
        })
        .all(|status| *status == ModelStreamStatus::Cancelled));
    assert!(assistant_rows(&projection)
        .iter()
        .all(|(_, streaming)| !streaming));
}

#[test]
fn delayed_or_duplicate_stream_events_do_not_modify_segment_order() {
    let mut events = started();
    append(&mut events, text("A"));
    append(&mut events, reasoning("same", "B"));
    append(&mut events, text("C"));
    let mut projection = TranscriptProjection::from_live_events(&events);
    let before = projection.items().to_vec();
    assert!(!projection.apply_event(&event(4, reasoning("same", "duplicate"))));
    let foreign = AgentEvent::new("other-session", 6, text("foreign"));
    assert!(!projection.apply_event(&foreign));
    assert_eq!(projection.items(), before);
    projection.apply_event(&event(6, commit("AC", vec![])));
    let committed = projection.items().to_vec();
    assert!(!projection.apply_event(&event(7, text("late"))));
    assert_eq!(projection.items(), committed);
}

#[test]
fn incremental_snapshot_matches_replay_through_reconciliation_and_tool_lifecycle() {
    // The commit replaces two interleaved text segments and removes a row
    // before the tool preview. Later results must still update the right row.
    let mut events = started();
    for kind in [
        text("primer segmento"),
        reasoning("analysis", "Resumen público"),
        text("segundo segmento"),
        preview(),
        commit("Texto definitivo 🦀", vec![call()]),
        AgentEventKind::ToolStarted { tool_call: call() },
        AgentEventKind::HookStarted {
            tool_call_id: call().id,
            hook_id: "before".into(),
            phase: "pre_tool_use".into(),
        },
        AgentEventKind::PermissionRequested {
            request_id: "permission".into(),
            tool_call: call(),
        },
        AgentEventKind::PermissionResolved {
            request_id: "permission".into(),
            tool_call_id: call().id,
            resolution: PermissionResolution::Allowed,
        },
        AgentEventKind::ToolFinished {
            tool_call_id: call().id,
            output: "README contents".into(),
            is_error: false,
        },
        AgentEventKind::ModelRequestStarted {
            step: 2,
            model: "fixture".into(),
        },
        AgentEventKind::AssistantDelta {
            content: "Listo".into(),
        },
        AgentEventKind::TurnCompleted {
            response: "Listo".into(),
            steps: 2,
        },
    ] {
        append(&mut events, kind);
    }
    // Exercise both publishing every delta and several mutations per frame.
    for batch_size in [1, 3, 64] {
        let mut live = TranscriptProjection::new();
        let mut snapshot = Vec::new();
        let mut delivered = Vec::new();
        for batch in events.chunks(batch_size) {
            for event in batch {
                live.apply_event(event);
                delivered.push(event.clone());
            }
            live.sync_into(&mut snapshot);
            assert_eq!(snapshot, live.items());
            assert_eq!(
                snapshot,
                TranscriptProjection::from_live_events(&delivered).items()
            );
            live.sync_into(&mut snapshot);
            assert_eq!(snapshot, live.items(), "publishing twice is idempotent");
        }
        assert_eq!(snapshot, TranscriptProjection::from_events(&events).items());
    }
}

#[test]
fn incremental_snapshot_keeps_historical_allocations_during_live_bursts() {
    let mut projection = TranscriptProjection::new();
    projection.apply_kind(&AgentEventKind::TurnStarted {
        prompt: "Historia".repeat(1000),
    });
    projection.apply_kind(&AgentEventKind::TurnCompleted {
        response: "Respuesta histórica extensa 🦀\n".repeat(1000),
        steps: 1,
    });
    let mut snapshot = Vec::new();
    projection.sync_into(&mut snapshot);
    let historical_allocations = |items: &[TranscriptItem]| {
        items[..2]
            .iter()
            .map(|item| match item {
                TranscriptItem::User { content, .. }
                | TranscriptItem::Assistant { content, .. } => content.as_ptr(),
                _ => panic!("expected historical text"),
            })
            .collect::<Vec<_>>()
    };
    let original = historical_allocations(&snapshot);
    projection.apply_kind(&AgentEventKind::TurnStarted {
        prompt: "Nueva tarea".into(),
    });
    projection.apply_kind(&AgentEventKind::ModelRequestStarted {
        step: 1,
        model: "fixture".into(),
    });
    for index in 0..512 {
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: format!("{index}: árbol 🦀\n"),
        });
        projection.sync_into(&mut snapshot);
        assert_eq!(snapshot, projection.items());
        assert_eq!(
            historical_allocations(&snapshot),
            original,
            "history is never cloned for a delta"
        );
    }
    projection.apply_kind(&AgentEventKind::TurnCancelled);
    projection.sync_into(&mut snapshot);
    assert_eq!(snapshot, projection.items());
    assert_eq!(historical_allocations(&snapshot), original);
}

#[test]
fn incremental_snapshot_rebases_after_session_replacement_and_external_length_change() {
    let mut projection = TranscriptProjection::from_live_events(&started());
    let mut snapshot = Vec::new();
    projection.sync_into(&mut snapshot);
    snapshot.clear();
    projection.sync_into(&mut snapshot);
    assert_eq!(snapshot, projection.items());
    let mut other = started();
    other[0] = event(
        1,
        AgentEventKind::TurnStarted {
            prompt: "Otra sesión".into(),
        },
    );
    projection = TranscriptProjection::from_live_events(&other);
    projection.sync_into(&mut snapshot);
    assert_eq!(snapshot, projection.items());
    projection = TranscriptProjection::new();
    projection.sync_into(&mut snapshot);
    assert!(snapshot.is_empty());
}

#[test]
fn tool_output_is_incremental_scoped_and_reconciles_without_shifting_other_calls() {
    let mut projection = TranscriptProjection::new();
    let mut snapshot = Vec::new();
    let mut events = started();
    let second = ToolCall {
        id: "call-second".into(),
        ..call()
    };
    for kind in [
        AgentEventKind::ToolStarted { tool_call: call() },
        AgentEventKind::ToolStarted {
            tool_call: second.clone(),
        },
        AgentEventKind::ToolOutputDelta {
            tool_call_id: call().id,
            stream: OutputStream::Stdout,
            text: "primero 🦀".into(),
        },
        AgentEventKind::ToolOutputDelta {
            tool_call_id: second.id.clone(),
            stream: OutputStream::Stdout,
            text: "segundo".into(),
        },
        AgentEventKind::ToolOutputDelta {
            tool_call_id: call().id,
            stream: OutputStream::Stderr,
            text: " diagnóstico".into(),
        },
        AgentEventKind::ToolFinished {
            tool_call_id: call().id,
            output: "Resultado completo".into(),
            is_error: false,
        },
        // A late delta is no longer eligible to create a live output row.
        AgentEventKind::ToolOutputDelta {
            tool_call_id: call().id,
            stream: OutputStream::Stdout,
            text: "tardío".into(),
        },
        AgentEventKind::ToolOutputDelta {
            tool_call_id: second.id,
            stream: OutputStream::Stdout,
            text: " texto".into(),
        },
        AgentEventKind::TurnCancelled,
    ] {
        append(&mut events, kind);
    }
    for (index, event) in events.iter().enumerate() {
        projection.apply_event(event);
        projection.sync_into(&mut snapshot);
        assert_eq!(
            snapshot,
            TranscriptProjection::from_live_events(&events[..=index]).items()
        );
        for (row, item) in snapshot.iter().enumerate() {
            if let TranscriptItem::ToolOutput { tool_call_id, .. } = item {
                assert!(
                    matches!(&snapshot[row - 1], TranscriptItem::Tool { id, .. } if id == tool_call_id)
                );
            }
        }
    }
    let partial = snapshot
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolOutput {
                tool_call_id,
                text,
                truncated,
            } => Some((tool_call_id.as_str(), text.as_str(), *truncated)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        partial,
        [
            ("call-read", "primero 🦀 diagnóstico", false),
            ("call-second", "segundo texto", false),
        ]
    );
    assert!(snapshot.iter().any(|item| matches!(item, TranscriptItem::Tool { id, output: None, activity: ActivityStatus::Interrupted, .. } if id == "call-second")));
    assert_eq!(snapshot, TranscriptProjection::from_events(&events).items());
}

#[test]
fn tool_output_preview_caps_bytes_on_unicode_boundaries_and_ignores_orphan_deltas() {
    let mut projection = TranscriptProjection::new();
    let update = |id: &str, text: &str| AgentEventKind::ToolOutputDelta {
        tool_call_id: id.into(),
        stream: OutputStream::Stdout,
        text: text.into(),
    };
    assert!(!projection.apply_kind(&update("orphan", "not attached")));
    projection.apply_kind(&AgentEventKind::ToolStarted { tool_call: call() });
    projection.apply_kind(&update(
        &call().id,
        &"🦀".repeat(MAX_TOOL_OUTPUT_PREVIEW_BYTES),
    ));
    let TranscriptItem::ToolOutput {
        text, truncated, ..
    } = &projection.items()[1]
    else {
        panic!("missing preview")
    };
    assert_eq!(text.len(), MAX_TOOL_OUTPUT_PREVIEW_BYTES);
    assert!(*truncated);
    assert!(projection.apply_kind(&update(&call().id, "última línea")));
    assert!(
        matches!(&projection.items()[1], TranscriptItem::ToolOutput { text, truncated: true, .. }
        if text.len() <= MAX_TOOL_OUTPUT_PREVIEW_BYTES && text.ends_with("última línea"))
    );
    projection.apply_kind(&AgentEventKind::ToolFinished {
        tool_call_id: call().id,
        output: "Authoritative full output".into(),
        is_error: false,
    });
    assert_eq!(
        projection.items().len(),
        2,
        "captured output survives a bounded final result"
    );
    assert!(
        matches!(&projection.items()[0], TranscriptItem::Tool { output: Some(value), .. } if value == "Authoritative full output")
    );
    assert!(!projection.apply_kind(&update(&call().id, "late")));
    assert!(
        matches!(&projection.items()[1], TranscriptItem::ToolOutput { text, .. } if text.ends_with("última línea"))
    );
}

#[test]
fn verified_result_after_cancellation_updates_the_original_tool_and_capture() {
    let events = vec![
        AgentEventKind::ToolStarted { tool_call: call() },
        AgentEventKind::ToolOutputDelta {
            tool_call_id: call().id,
            stream: OutputStream::Stdout,
            text: "observed before cancellation".into(),
        },
        AgentEventKind::TurnCancelled,
        AgentEventKind::ToolFinished {
            tool_call_id: call().id,
            output: "manually verified result".into(),
            is_error: false,
        },
    ];
    let mut projection = TranscriptProjection::new();
    let mut snapshot = Vec::new();
    for event in &events {
        projection.apply_kind(event);
        projection.sync_into(&mut snapshot);
        assert_eq!(snapshot, projection.items());
    }
    assert_eq!(
        snapshot
            .iter()
            .filter(|item| matches!(item,
                TranscriptItem::Tool { id, .. } if id == &call().id
            ))
            .count(),
        1
    );
    assert!(matches!(&snapshot[0], TranscriptItem::Tool {
        name, output: Some(output), activity: ActivityStatus::Completed, ..
    } if name == &call().name && output == "manually verified result"));
    assert!(
        matches!(&snapshot[1], TranscriptItem::ToolOutput { text, .. }
        if text == "observed before cancellation")
    );
}

fn media_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "mcp__browser__screenshot".into(),
        arguments: json!({"url":"https://example.test"}),
    }
}

fn media_image() -> UserImage {
    // Real PNG deliberately larger than the model's 16k text result limit.
    let mut seed = 0x7351_942du32;
    let pixels = image::RgbaImage::from_fn(128, 128, |_, _| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        image::Rgba(seed.to_le_bytes())
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let image = UserImage::from_bytes(UserImageMime::Png, &bytes.into_inner()).unwrap();
    assert!(image.data_base64.len() > 16_000);
    image
}

fn media_event(id: &str, image: &UserImage) -> AgentEventKind {
    AgentEventKind::ToolResultMedia {
        tool_call_id: id.into(),
        images: vec![image.clone()],
        text: "Captura recibida 🦀".into(),
        notices: vec!["Observación de prueba".into()],
        model_images: vec![],
    }
}

fn assert_tool_observation_order(items: &[TranscriptItem]) {
    for (index, item) in items.iter().enumerate() {
        match item {
            TranscriptItem::ToolOutput { tool_call_id, .. } => assert!(matches!(&items[index - 1],
                TranscriptItem::Tool { id, .. } if id == tool_call_id)),
            TranscriptItem::ToolMedia { tool_call_id, .. } => {
                let mut preceding = index - 1;
                if matches!(&items[preceding], TranscriptItem::ToolOutput { tool_call_id: id, .. } if id == tool_call_id)
                {
                    preceding -= 1;
                }
                assert!(
                    matches!(&items[preceding], TranscriptItem::Tool { id, .. } if id == tool_call_id)
                );
            }
            _ => {}
        }
    }
}

#[test]
fn typed_media_and_output_keep_adjacent_owners_across_insertions_live_and_replay() {
    let image = media_image();
    for media_first in [false, true] {
        let mut projection = TranscriptProjection::new();
        let mut snapshot = Vec::new();
        let mut events = started();
        append(
            &mut events,
            commit("", vec![media_call("first"), media_call("second")]),
        );
        append(
            &mut events,
            AgentEventKind::ToolStarted {
                tool_call: media_call("first"),
            },
        );
        append(
            &mut events,
            AgentEventKind::ToolStarted {
                tool_call: media_call("second"),
            },
        );
        append(&mut events, media_event("second", &image));
        let output = AgentEventKind::ToolOutputDelta {
            tool_call_id: "first".into(),
            stream: OutputStream::Stdout,
            text: "captured before result".into(),
        };
        if media_first {
            append(&mut events, media_event("first", &image));
            append(&mut events, output);
        } else {
            append(&mut events, output);
            append(&mut events, media_event("first", &image));
        }
        append(
            &mut events,
            AgentEventKind::ToolOutputDelta {
                tool_call_id: "second".into(),
                stream: OutputStream::Stderr,
                text: "second diagnostic".into(),
            },
        );
        for id in ["first", "second"] {
            append(
                &mut events,
                AgentEventKind::ToolFinished {
                    tool_call_id: id.into(),
                    output: "{\"content\":[\n[tool output truncated]".into(),
                    is_error: false,
                },
            );
        }
        append(
            &mut events,
            AgentEventKind::TurnCompleted {
                response: "Listo".into(),
                steps: 1,
            },
        );
        for (index, event) in events.iter().enumerate() {
            projection.apply_event(event);
            projection.sync_into(&mut snapshot);
            assert_eq!(
                snapshot,
                TranscriptProjection::from_live_events(&events[..=index]).items()
            );
            assert_tool_observation_order(&snapshot);
        }
        let rows = snapshot
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::ToolMedia { images, text, .. } => Some((images, text)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        for (images, text) in rows {
            assert_eq!(text, "Captura recibida 🦀");
            let raster = images[0].raster.as_ref().unwrap();
            assert_eq!(raster.sha256(), image.sha256);
            assert_eq!(raster.data_base64(), image.data_base64);
        }
        assert_eq!(snapshot, TranscriptProjection::from_events(&events).items());
    }
}

#[test]
fn typed_media_survives_reopen_cancellation_and_manual_confirmation_without_repeating_the_call() {
    let image = media_image();
    let mut events = started();
    append(&mut events, commit("", vec![media_call("captured")]));
    append(
        &mut events,
        AgentEventKind::ToolStarted {
            tool_call: media_call("captured"),
        },
    );
    append(&mut events, media_event("captured", &image));
    let mut reopened = TranscriptProjection::from_events(&events);
    assert!(reopened.items().iter().any(|item| matches!(item,
        TranscriptItem::Tool { id, output: None, activity: ActivityStatus::Interrupted, .. } if id == "captured")));
    assert!(reopened
        .items()
        .iter()
        .any(|item| matches!(item, TranscriptItem::ToolMedia { .. })));
    // The explicit verification updates the historical tool, not its captured media.
    reopened.apply_kind(&AgentEventKind::ToolFinished {
        tool_call_id: "captured".into(),
        output: "Manually verified".into(),
        is_error: false,
    });
    assert_tool_observation_order(reopened.items());
    assert_eq!(
        reopened
            .items()
            .iter()
            .filter(|item| matches!(item, TranscriptItem::Tool { .. }))
            .count(),
        1
    );
    assert_eq!(
        reopened
            .items()
            .iter()
            .filter(|item| matches!(item, TranscriptItem::ToolMedia { .. }))
            .count(),
        1
    );
    append(&mut events, AgentEventKind::TurnCancelled);
    let mut cancelled = TranscriptProjection::from_events(&events);
    assert!(
        !cancelled.apply_kind(&media_event("captured", &image)),
        "late payload cannot revive cancelled call"
    );
    cancelled.apply_kind(&AgentEventKind::ToolFinished {
        tool_call_id: "captured".into(),
        output: "Verified after cancellation".into(),
        is_error: false,
    });
    assert_tool_observation_order(cancelled.items());
    assert_eq!(
        cancelled
            .items()
            .iter()
            .filter(|item| matches!(item, TranscriptItem::ToolMedia { .. }))
            .count(),
        1
    );
}

#[test]
fn typed_media_rejects_orphan_late_duplicate_and_over_budget_payloads_before_retaining_them() {
    let image = media_image();
    let mut projection = TranscriptProjection::new();
    assert!(!projection.apply_kind(&media_event("missing", &image)));
    projection.apply_kind(&AgentEventKind::ToolStarted { tool_call: call() });
    assert!(
        !projection.apply_kind(&media_event(&call().id, &image)),
        "ordinary tools cannot inject MCP media"
    );
    projection.apply_kind(&AgentEventKind::ToolStarted {
        tool_call: media_call("capture"),
    });
    for invalid in [
        AgentEventKind::ToolResultMedia {
            tool_call_id: "capture".into(),
            images: vec![image.clone(); 5],
            text: String::new(),
            notices: vec![],
            model_images: vec![],
        },
        AgentEventKind::ToolResultMedia {
            tool_call_id: "capture".into(),
            images: vec![image.clone()],
            text: "🦀".repeat(1025),
            notices: vec![],
            model_images: vec![],
        },
        AgentEventKind::ToolResultMedia {
            tool_call_id: "capture".into(),
            images: vec![image.clone()],
            text: String::new(),
            notices: vec!["notice".into(); 9],
            model_images: vec![],
        },
        AgentEventKind::ToolResultMedia {
            tool_call_id: "capture".into(),
            images: vec![image.clone()],
            text: String::new(),
            notices: vec!["x".repeat(257)],
            model_images: vec![],
        },
    ] {
        assert!(!projection.apply_kind(&invalid));
        assert!(!projection
            .items()
            .iter()
            .any(|item| matches!(item, TranscriptItem::ToolMedia { .. })));
    }
    assert!(projection.apply_kind(&media_event("capture", &image)));
    assert!(!projection.apply_kind(&media_event("capture", &image)));
    projection.apply_kind(&AgentEventKind::ToolFinished {
        tool_call_id: "capture".into(),
        output: "done".into(),
        is_error: false,
    });
    assert!(!projection.apply_kind(&media_event("capture", &image)));
}

#[test]
fn the_images_of_an_answer_belong_to_the_question_they_answered() {
    let image = media_image();
    let ask = ToolCall {
        id: "ask".into(),
        name: openide_harness::ASK_USER_TOOL.into(),
        arguments: json!({"questions": []}),
    };
    let kinds = vec![
        AgentEventKind::TurnStarted { prompt: "Ask".into() },
        AgentEventKind::PermissionRequested {
            request_id: "request".into(),
            tool_call: ask.clone(),
        },
        AgentEventKind::PermissionResolved {
            request_id: "request".into(),
            tool_call_id: "ask".into(),
            resolution: PermissionResolution::Allowed,
        },
        AgentEventKind::ToolStarted { tool_call: ask },
        AgentEventKind::ToolResultMedia {
            tool_call_id: "ask".into(),
            images: vec![image.clone()],
            text: String::new(),
            notices: vec![],
            model_images: vec![0],
        },
        AgentEventKind::ToolFinished {
            tool_call_id: "ask".into(),
            output: "{\"answered_by\":\"user\",\"answers\":[]}".into(),
            is_error: false,
        },
    ];
    let mut live = TranscriptProjection::new();
    for kind in &kinds {
        live.apply_kind(kind);
    }
    let events = kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| AgentEvent::new("session", index as u64 + 1, kind))
        .collect::<Vec<_>>();
    // Live and after reopening the conversation: the answered row, then its images.
    for items in [live.items(), TranscriptProjection::from_events(&events).items()] {
        let at = items
            .iter()
            .position(|item| matches!(item, TranscriptItem::Tool { id, .. } if id == "ask"))
            .unwrap();
        assert!(
            matches!(
                &items[at],
                TranscriptItem::Tool { output: Some(_), activity: ActivityStatus::Completed, .. }
            ),
            "{:?}",
            items[at]
        );
        assert!(
            matches!(
                items.get(at + 1),
                Some(TranscriptItem::ToolMedia { tool_call_id, images, .. })
                    if tool_call_id == "ask"
                        && images[0].raster.as_ref().unwrap().sha256() == image.sha256
            ),
            "{:?}",
            items.get(at + 1).map(std::mem::discriminant)
        );
    }
}

#[test]
fn typed_media_snapshot_shares_encoded_pixels_and_keeps_existing_allocations_during_other_deltas() {
    let image = media_image();
    let mut projection = TranscriptProjection::new();
    projection.apply_kind(&AgentEventKind::ToolStarted {
        tool_call: media_call("capture"),
    });
    projection.apply_kind(&media_event("capture", &image));
    let mut snapshot = Vec::new();
    projection.sync_into(&mut snapshot);
    let pointers = |items: &[TranscriptItem]| {
        let TranscriptItem::ToolMedia {
            images,
            text,
            notices,
            ..
        } = &items[1]
        else {
            panic!("typed media follows its owner")
        };
        (
            images[0].raster.as_ref().unwrap().data_base64().as_ptr(),
            text.as_ptr(),
            notices[0].as_ptr(),
        )
    };
    let initial = pointers(&snapshot);
    assert_eq!(
        initial.0,
        pointers(projection.items()).0,
        "snapshot shares its RasterImage Arc"
    );
    projection.apply_kind(&AgentEventKind::AssistantDelta {
        content: "Respuesta incremental".into(),
    });
    projection.sync_into(&mut snapshot);
    assert_eq!(
        initial,
        pointers(&snapshot),
        "unrelated text cannot clone media text or encoded pixels"
    );
    assert!(!projection.apply_kind(&media_event("capture", &image)));
    projection.sync_into(&mut snapshot);
    assert_eq!(initial, pointers(&snapshot));
}

#[test]
fn permission_promotes_committed_preview_in_place_before_tool_start_and_replays_once() {
    let mut events = started();
    append(&mut events, preview());
    append(&mut events, commit("", vec![call()]));
    let mut projection = TranscriptProjection::from_live_events(&events);
    let row = projection
        .items()
        .iter()
        .position(|item| {
            matches!(item,
        TranscriptItem::ToolPreview { call_id: Some(id), .. } if id == "call-read")
        })
        .unwrap();
    let count = projection.items().len();
    for kind in [
        AgentEventKind::PermissionRequested {
            request_id: "approve".into(),
            tool_call: call(),
        },
        AgentEventKind::PermissionRequested {
            request_id: "approve".into(),
            tool_call: call(),
        },
    ] {
        append(&mut events, kind);
        projection.apply_event(events.last().unwrap());
        assert_eq!(projection.items().len(), count);
        assert!(matches!(&projection.items()[row], TranscriptItem::Tool {
            id, activity: ActivityStatus::WaitingPermission, output: None, ..
        } if id == "call-read"));
        assert_eq!(
            projection.items(),
            TranscriptProjection::from_live_events(&events).items()
        );
    }
    append(
        &mut events,
        AgentEventKind::PermissionResolved {
            request_id: "approve".into(),
            tool_call_id: call().id,
            resolution: PermissionResolution::Allowed,
        },
    );
    projection.apply_event(events.last().unwrap());
    append(
        &mut events,
        AgentEventKind::ToolStarted { tool_call: call() },
    );
    projection.apply_event(events.last().unwrap());
    assert!(matches!(
        &projection.items()[row],
        TranscriptItem::Tool {
            activity: ActivityStatus::Running,
            ..
        }
    ));
    assert_eq!(
        projection
            .items()
            .iter()
            .filter(|item| matches!(item,
        TranscriptItem::Tool { id, .. } if id == "call-read"))
            .count(),
        1
    );
    assert!(!projection.items().iter().any(|item| matches!(item,
        TranscriptItem::Notice { message, .. } if message.starts_with("Permiso requerido"))));
    assert_eq!(
        projection.items(),
        TranscriptProjection::from_live_events(&events).items()
    );
}

#[test]
fn a_permission_that_was_denied_or_expired_is_one_error_on_the_calls_own_row() {
    for (resolution, raw, shown) in [
        (
            PermissionResolution::TimedOut,
            "Permission request timed out for read_file",
            "El permiso venció sin respuesta",
        ),
        (PermissionResolution::Denied, "Permission denied for read_file", "Permiso denegado"),
    ] {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::PermissionRequested {
            request_id: "approve".into(),
            tool_call: call(),
        });
        projection.apply_kind(&AgentEventKind::PermissionResolved {
            request_id: "approve".into(),
            tool_call_id: call().id,
            resolution,
        });
        projection.apply_kind(&AgentEventKind::ToolFinished {
            tool_call_id: call().id,
            output: raw.into(),
            is_error: true,
        });
        // No card repeats under the row what the row already says.
        assert!(!projection
            .items()
            .iter()
            .any(|item| matches!(item, TranscriptItem::Notice { .. })));
        let failed: Vec<_> = projection
            .items()
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::Tool { output, is_error: true, activity: ActivityStatus::Failed, .. } => {
                    output.as_deref()
                }
                _ => None,
            })
            .collect();
        assert_eq!(failed, [shown]);
    }
}

#[test]
fn permission_without_stream_preview_still_has_one_truthful_file_row() {
    for (resolution, expected) in [
        (PermissionResolution::Denied, ActivityStatus::Failed),
        (PermissionResolution::Cancelled, ActivityStatus::Cancelled),
        (PermissionResolution::TimedOut, ActivityStatus::Failed),
    ] {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::PermissionRequested {
            request_id: "approve".into(),
            tool_call: call(),
        });
        assert!(matches!(
            &projection.items()[0],
            TranscriptItem::Tool {
                activity: ActivityStatus::WaitingPermission,
                output: None,
                ..
            }
        ));
        projection.apply_kind(&AgentEventKind::PermissionResolved {
            request_id: "approve".into(),
            tool_call_id: call().id,
            resolution,
        });
        assert!(
            matches!(&projection.items()[0], TranscriptItem::Tool { activity, .. } if *activity == expected)
        );
        assert_eq!(
            projection
                .items()
                .iter()
                .filter(|item| matches!(item, TranscriptItem::Tool { .. }))
                .count(),
            1
        );
    }
}

#[test]
fn resumed_committed_call_reuses_its_preview_or_pending_permission_row() {
    for permission_already_requested in [false, true] {
        let mut events = started();
        append(&mut events, preview());
        append(&mut events, commit("", vec![call()]));
        let requested = AgentEventKind::PermissionRequested {
            request_id: "resume-permission".into(),
            tool_call: call(),
        };
        if permission_already_requested {
            append(&mut events, requested.clone());
        }
        let mut projection = TranscriptProjection::from_events(&events);
        let original_row = projection
            .items()
            .iter()
            .position(|item| match item {
                TranscriptItem::ToolPreview {
                    call_id: Some(id), ..
                }
                | TranscriptItem::Tool { id, .. } => id == "call-read",
                _ => false,
            })
            .unwrap();
        assert!(!projection.apply_kind(&AgentEventKind::ToolOutputDelta {
            tool_call_id: "call-read".into(),
            stream: OutputStream::Stdout,
            text: "late output while recovery has not resumed".into(),
        }));
        assert!(!projection.apply_kind(&preview()));

        let mut resumed = Vec::new();
        if !permission_already_requested {
            resumed.push(requested);
        }
        resumed.extend([
            AgentEventKind::PermissionResolved {
                request_id: "resume-permission".into(),
                tool_call_id: "call-read".into(),
                resolution: PermissionResolution::Allowed,
            },
            AgentEventKind::ToolStarted { tool_call: call() },
            AgentEventKind::ToolOutputDelta {
                tool_call_id: "call-read".into(),
                stream: OutputStream::Stdout,
                text: "recovered output".into(),
            },
            AgentEventKind::ToolFinished {
                tool_call_id: "call-read".into(),
                output: "read complete".into(),
                is_error: false,
            },
        ]);
        for kind in resumed {
            append(&mut events, kind);
            assert!(projection.apply_event(events.last().unwrap()));
            let rows: Vec<_> = projection
                .items()
                .iter()
                .enumerate()
                .filter(|(_, item)| match item {
                    TranscriptItem::ToolPreview {
                        call_id: Some(id), ..
                    }
                    | TranscriptItem::Tool { id, .. } => id == "call-read",
                    _ => false,
                })
                .collect();
            assert_eq!(rows.len(), 1, "recovery must not duplicate the call");
            assert_eq!(rows[0].0, original_row);
        }
        let replayed = TranscriptProjection::from_live_events(&events);
        assert_eq!(
            projection.items()[original_row],
            replayed.items()[original_row]
        );
        assert!(matches!(
            &projection.items()[original_row + 1],
            TranscriptItem::ToolOutput { text, .. } if text == "recovered output"
        ));
        assert!(matches!(
            &projection.items()[original_row],
            TranscriptItem::Tool {
                output: Some(output), activity: ActivityStatus::Completed, ..
            } if output == "read complete"
        ));
        assert_indices(&projection);
    }
}

#[test]
fn discarding_a_confirmed_batch_updates_only_its_unexecuted_suffix_after_reopen() {
    let mut events = started();
    let calls: Vec<_> = (0..3)
        .map(|index| ToolCall {
            id: format!("batch-{index}"),
            name: "read_file".into(),
            arguments: json!({ "path": format!("file-{index}.md") }),
        })
        .collect();
    // Provider preview order need not equal the authoritative call order.
    for index in [2, 0, 1] {
        append(
            &mut events,
            stream(ModelStreamEvent::ToolCallDelta {
                item_id: format!("item-{index}"),
                call_id: Some(calls[index].id.clone()),
                name: Some(calls[index].name.clone()),
                arguments_delta: calls[index].arguments.to_string(),
            }),
        );
    }
    append(&mut events, preview());
    append(&mut events, commit("", calls.clone()));
    append(
        &mut events,
        AgentEventKind::ToolStarted {
            tool_call: calls[0].clone(),
        },
    );
    append(
        &mut events,
        AgentEventKind::ToolOutputDelta {
            tool_call_id: calls[0].id.clone(),
            stream: OutputStream::Stdout,
            text: "captured before interruption".into(),
        },
    );
    append(
        &mut events,
        AgentEventKind::ToolFinished {
            tool_call_id: calls[0].id.clone(),
            output: "verified first read".into(),
            is_error: false,
        },
    );
    append(
        &mut events,
        AgentEventKind::PermissionRequested {
            request_id: "batch-permission".into(),
            tool_call: calls[1].clone(),
        },
    );
    append(&mut events, AgentEventKind::TurnCancelled);
    // Discarding the unexecuted suffix must not alter the confirmed result
    // or its capture, and must use call order rather than preview row order.
    let mut projection = TranscriptProjection::from_events(&events);
    append(
        &mut events,
        AgentEventKind::ToolBatchDiscarded {
            next_tool_call_id: calls[1].id.clone(),
        },
    );
    assert!(projection.apply_event(events.last().unwrap()));
    assert!(projection.items().iter().any(|item| matches!(item,
        TranscriptItem::Tool { id, output: Some(output), activity: ActivityStatus::Completed, .. }
            if id == "batch-0" && output == "verified first read"
    )));
    assert!(projection.items().iter().any(|item| matches!(item,
        TranscriptItem::Tool { id, output: None, activity: ActivityStatus::Discarded, .. }
            if id == "batch-1"
    )));
    assert!(projection.items().iter().any(|item| matches!(item,
        TranscriptItem::ToolPreview { call_id: Some(id), status: ModelStreamStatus::Discarded, .. }
            if id == "batch-2"
    )));
    assert!(projection.items().iter().any(|item| matches!(item,
        TranscriptItem::ToolPreview { call_id: Some(id), status: ModelStreamStatus::Interrupted, .. }
            if id == "call-read"
    )));
    assert_eq!(
        projection
            .items()
            .iter()
            .filter(|item| matches!(
                item,
                TranscriptItem::Tool {
                    output: Some(_),
                    ..
                }
            ))
            .count(),
        1,
        "discarding must not synthesize any tool results"
    );
    assert!(projection.items().iter().any(|item| matches!(item,
        TranscriptItem::ToolOutput { tool_call_id, text, .. }
            if tool_call_id == "batch-0" && text == "captured before interruption"
    )));
    assert_eq!(
        projection.items(),
        TranscriptProjection::from_events(&events).items()
    );
    assert_indices(&projection);
}

#[test]
fn recovery_only_warns_of_uncertain_effects_after_an_unfinished_tool_start() {
    for phase in ["permission", "allowed", "denied", "started", "finished"] {
        let mut events = started();
        append(&mut events, preview());
        append(&mut events, commit("", vec![call()]));
        append(
            &mut events,
            AgentEventKind::PermissionRequested {
                request_id: "recovery-permission".into(),
                tool_call: call(),
            },
        );
        if phase != "permission" {
            append(
                &mut events,
                AgentEventKind::PermissionResolved {
                    request_id: "recovery-permission".into(),
                    tool_call_id: call().id,
                    resolution: if phase == "denied" {
                        PermissionResolution::Denied
                    } else {
                        PermissionResolution::Allowed
                    },
                },
            );
        }
        if matches!(phase, "started" | "finished") {
            append(
                &mut events,
                AgentEventKind::ToolStarted { tool_call: call() },
            );
        }
        if phase == "finished" {
            append(
                &mut events,
                AgentEventKind::ToolFinished {
                    tool_call_id: call().id,
                    output: "confirmed".into(),
                    is_error: false,
                },
            );
        }
        let projection = TranscriptProjection::from_events(&events);
        let notice = projection
            .items()
            .iter()
            .rev()
            .find_map(|item| match item {
                TranscriptItem::Notice { message, .. } => Some(message),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            notice.contains("resultado es incierto"),
            phase == "started",
            "{phase}"
        );
        if phase == "permission" {
            assert!(notice.contains("permiso pendiente"));
            assert!(notice.contains("no se ejecutó"));
            assert!(projection.items().iter().any(|item| matches!(
                item,
                TranscriptItem::Tool {
                    activity: ActivityStatus::PermissionInterrupted,
                    ..
                }
            )));
        }
    }
}
