//! Delivery budget tests. These exercise real channels and transcript replay;
//! HTTP first-paint acceptance lives in native_streaming_app_tests instead.

use super::*;
use crate::transcript::TranscriptItem;

#[test]
fn streaming_burst_keeps_unread_tail_and_terminal_in_order() {
    let (sender, receiver) = mpsc::channel();
    let mut source = Vec::new();
    let mut emit = |kind| {
        let event = AgentEvent::new("streaming-queue", source.len() as u64 + 1, kind);
        sender.send(event.clone()).unwrap();
        source.push(event);
    };
    emit(AgentEventKind::TurnStarted {
        prompt: "Explica el proyecto".into(),
    });
    emit(AgentEventKind::ModelRequestStarted {
        step: 1,
        model: "exact-local-model".into(),
    });
    let mut content = String::new();
    for index in 0..512 {
        let delta = format!("{index}: árbol 🦀\n");
        content.push_str(&delta);
        emit(AgentEventKind::AssistantDelta { content: delta });
    }
    emit(AgentEventKind::AssistantMessage {
        content: content.clone(),
    });
    emit(AgentEventKind::TurnCompleted {
        response: content.clone(),
        steps: 1,
    });
    drop(sender);

    let (first, disconnected) = drain_ui_updates(&receiver);
    assert!(!disconnected, "closure must not skip the unread tail");
    assert!(!first.is_empty() && first.len() < source.len());
    assert!(first.len() <= MAX_RUN_UPDATES_PER_FRAME);
    assert!(!first
        .iter()
        .any(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. })));
    let mut projection = TranscriptProjection::default();
    for event in &first {
        projection.apply_event(event);
    }
    assert!(projection.items().iter().any(|item| matches!(
        item,
        TranscriptItem::Assistant { content, streaming: true } if content.contains("árbol 🦀")
    )));

    let mut delivered = first;
    loop {
        let (batch, disconnected) = drain_ui_updates(&receiver);
        assert!(batch.len() <= MAX_RUN_UPDATES_PER_FRAME);
        for event in &batch {
            projection.apply_event(event);
        }
        delivered.extend(batch);
        if disconnected {
            break;
        }
        assert!(delivered.len() <= source.len());
    }
    assert_eq!(delivered, source, "FIFO records must arrive exactly once");
    assert_eq!(
        projection.items(),
        TranscriptProjection::from_events(&source).items()
    );
    let assistants = projection
        .items()
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Assistant { content, streaming } => Some((content, streaming)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(assistants, vec![(&content, &false)]);
}

#[test]
fn live_empty_queue_and_closed_exact_budget_do_not_fabricate_completion() {
    let (sender, receiver) = mpsc::channel();
    assert_eq!(drain_ui_updates(&receiver), (Vec::<u64>::new(), false));
    for sequence in 1..=MAX_RUN_UPDATES_PER_FRAME as u64 {
        sender.send(sequence).unwrap();
    }
    drop(sender);
    let (batch, disconnected) = drain_ui_updates(&receiver);
    assert_eq!(
        batch,
        (1..=MAX_RUN_UPDATES_PER_FRAME as u64).collect::<Vec<_>>()
    );
    assert!(!disconnected);
    assert_eq!(drain_ui_updates(&receiver), (Vec::<u64>::new(), true));
}
