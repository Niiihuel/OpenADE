//! Hosted search journals are UI observations, never local execution requests.
use super::*;
use openide_core::AgentMessage;
use serde_json::json;

fn event(sequence: u64, kind: AgentEventKind) -> AgentEvent {
    AgentEvent::new("web-session", sequence, kind)
}
fn started() -> Vec<AgentEvent> {
    vec![
        event(
            1,
            AgentEventKind::TurnStarted {
                prompt: "Investigar".into(),
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
fn update(item: &str, phase: &str, query: &str) -> AgentEventKind {
    AgentEventKind::ModelStreamUpdate {
        step: 1,
        update: ModelStreamEvent::WebSearchUpdate {
            item_id: item.into(),
            status: phase.into(),
            action: Some("search".into()),
            query: Some(query.into()),
            url: None,
            sources: Vec::new(),
        },
    }
}
fn item(item: &str, status: &str, query: &str) -> Value {
    json!({"type":"web_search_call","id":item,"status":status,"action":{"type":"search","query":query,"sources":[{"url":"https://www.rust-lang.org/","title":"Rust"}]}})
}
fn commit(items: Vec<Value>) -> AgentEventKind {
    let mut assistant = AgentMessage::assistant("Respuesta verificada", Vec::new());
    assistant.provider_output_items = items;
    AgentEventKind::ModelResponseCommitted {
        step: 1,
        assistant,
        response_id: Some("resp_web".into()),
        usage: None,
    }
}
fn web_rows(projection: &TranscriptProjection) -> Vec<&TranscriptItem> {
    projection
        .items()
        .iter()
        .filter(|item| matches!(item, TranscriptItem::WebSearch { .. }))
        .collect()
}

#[test]
fn hosted_web_projection_keeps_one_row_until_authoritative_commit_and_replays_identically() {
    let mut events = started();
    let mut projection = TranscriptProjection::from_live_events(&events);
    for (phase, query) in [
        ("in_progress", "Rust"),
        ("searching", "Rust async"),
        ("completed", "Rust async"),
    ] {
        let event = event(events.len() as u64 + 1, update("ws_1", phase, query));
        projection.apply_event(&event);
        events.push(event);
        let rows = web_rows(&projection);
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0],
            TranscriptItem::WebSearch {
                status: ModelStreamStatus::Receiving,
                ..
            }
        ));
    }
    let id = match web_rows(&projection)[0] {
        TranscriptItem::WebSearch { id, .. } => id.clone(),
        _ => unreachable!(),
    };
    assert!(id.contains("web-session:2:1:web:ws_1"));
    let committed = event(
        6,
        commit(vec![item("ws_1", "completed", "Canonical query")]),
    );
    projection.apply_event(&committed);
    events.push(committed);
    assert!(
        matches!(web_rows(&projection)[0], TranscriptItem::WebSearch {
        id: final_id, query: Some(query), sources, status: ModelStreamStatus::Committed, ..
    } if final_id == &id && query == "Canonical query" && sources.len() == 1)
    );
    let before_late = projection.items().to_vec();
    assert!(!projection.apply_event(&event(7, update("ws_1", "searching", "late mutation"))));
    assert_eq!(projection.items(), before_late);
    assert!(projection.items().iter().all(|item| !matches!(
        item,
        TranscriptItem::Tool { .. } | TranscriptItem::ToolPreview { .. }
    )));
    assert_eq!(
        projection.items(),
        TranscriptProjection::from_live_events(&events).items()
    );
}

#[test]
fn uncommitted_searches_are_not_successful_after_disconnect_failure_or_missing_final_item() {
    for (terminal, expected) in [
        (
            AgentEventKind::TurnFailed {
                error: "network failure".into(),
            },
            ModelStreamStatus::Failed,
        ),
        (AgentEventKind::TurnCancelled, ModelStreamStatus::Cancelled),
    ] {
        let mut events = started();
        events.push(event(3, update("ws_1", "completed", "Rust")));
        let mut projection = TranscriptProjection::from_live_events(&events);
        projection.apply_event(&event(4, terminal));
        assert!(
            matches!(web_rows(&projection)[0], TranscriptItem::WebSearch { status, .. } if *status == expected)
        );
        let recovered = TranscriptProjection::from_events(&events);
        assert!(matches!(
            web_rows(&recovered)[0],
            TranscriptItem::WebSearch {
                status: ModelStreamStatus::Interrupted,
                ..
            }
        ));
    }
    let mut projection = TranscriptProjection::from_live_events(&started());
    projection.apply_event(&event(3, update("ws_orphan", "completed", "Rust")));
    projection.apply_event(&event(4, commit(Vec::new())));
    assert!(matches!(
        web_rows(&projection)[0],
        TranscriptItem::WebSearch {
            status: ModelStreamStatus::Interrupted,
            ..
        }
    ));
    for (provider, expected) in [
        ("failed", ModelStreamStatus::Failed),
        ("incomplete", ModelStreamStatus::Interrupted),
        ("in_progress", ModelStreamStatus::Interrupted),
    ] {
        let mut projection = TranscriptProjection::from_live_events(&started());
        projection.apply_event(&event(3, commit(vec![item("ws", provider, "Rust")])));
        assert!(
            matches!(web_rows(&projection)[0], TranscriptItem::WebSearch { status, .. } if *status == expected)
        );
    }
}

#[test]
fn non_streamed_searches_reconcile_before_answer_and_new_requests_keep_distinct_identity() {
    let mut projection = TranscriptProjection::from_live_events(&started());
    projection.apply_event(&event(
        3,
        AgentEventKind::ModelStreamUpdate {
            step: 1,
            update: ModelStreamEvent::TextDelta {
                content: "Respuesta verificada".into(),
            },
        },
    ));
    projection.apply_event(&event(
        4,
        commit(vec![item("ws_same", "completed", "first")]),
    ));
    let before = projection
        .items()
        .iter()
        .position(|item| matches!(item, TranscriptItem::WebSearch { .. }))
        .unwrap();
    let answer = projection
        .items()
        .iter()
        .position(|item| matches!(item, TranscriptItem::Assistant { .. }))
        .unwrap();
    assert!(before < answer);
    projection.apply_event(&event(
        5,
        AgentEventKind::ModelRequestStarted {
            step: 1,
            model: "fixture".into(),
        },
    ));
    projection.apply_event(&event(6, update("ws_same", "searching", "second")));
    let rows = web_rows(&projection);
    assert_eq!(rows.len(), 2);
    assert!(
        matches!((rows[0], rows[1]), (TranscriptItem::WebSearch { id: old, status: ModelStreamStatus::Committed, .. }, TranscriptItem::WebSearch { id: new, status: ModelStreamStatus::Receiving, .. }) if old != new)
    );
    assert!(!projection.apply_event(&AgentEvent::new(
        "foreign-session",
        7,
        update("ws_same", "searching", "wrong session")
    )));
    assert!(!projection.apply_event(&event(3, update("ws_same", "searching", "old sequence"))));
}

#[test]
fn hosted_web_rows_bound_payloads_and_request_budget_without_splitting_unicode() {
    let mut projection = TranscriptProjection::from_live_events(&started());
    for index in 0..100 {
        projection.apply_event(&event(
            3 + index,
            AgentEventKind::ModelStreamUpdate {
                step: 1,
                update: ModelStreamEvent::WebSearchUpdate {
                    item_id: format!("ws_{index}"),
                    status: "searching".into(),
                    action: Some("search".into()),
                    query: Some("🦀".repeat(2000)),
                    url: Some("é".repeat(4000)),
                    sources: vec![("title".repeat(1000), "中".repeat(4000)); 20],
                },
            },
        ));
    }
    assert!(web_rows(&projection).len() <= MAX_MODEL_STREAM_ROWS);
    assert!(projection.model_stream_bytes <= MAX_MODEL_STREAM_BYTES);
    for row in web_rows(&projection) {
        let TranscriptItem::WebSearch {
            item_id,
            action,
            query,
            url,
            sources,
            ..
        } = row
        else {
            unreachable!()
        };
        let event = ModelStreamEvent::WebSearchUpdate {
            item_id: item_id.clone(),
            status: "searching".into(),
            action: Some(action.clone()),
            query: query.clone(),
            url: url.clone(),
            sources: sources.clone(),
        };
        assert!(serde_json::to_vec(&event).unwrap().len() <= 8 * 1024);
        assert!(query.as_ref().unwrap().len() <= 1024);
        assert!(url.as_ref().unwrap().len() <= 2048);
        assert!(sources.len() <= 8);
    }
}
