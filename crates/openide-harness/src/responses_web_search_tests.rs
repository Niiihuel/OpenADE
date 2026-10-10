use super::super::{response_ready_events, ResponseStream};
use super::*;
use serde_json::json;
use std::collections::HashSet;

fn ingest(stream: &mut ResponseStream, event: Value) -> Vec<ModelStreamEvent> {
    let mut events = Vec::new();
    stream
        .ingest_events(format!("data: {event}\n\n").as_bytes(), &mut events)
        .unwrap();
    events
}

#[test]
fn hosted_search_lifecycle_is_correlated_and_never_becomes_an_executable_call() {
    let mut stream = ResponseStream::default();
    let mut events = ingest(
        &mut stream,
        json!({"type":"response.created","response":{"id":"resp_web"}}),
    );
    assert!(ingest(
        &mut stream,
        json!({"type":"response.web_search_call.searching","output_index":0,"item_id":"unknown"})
    )
    .is_empty());
    events.extend(ingest(&mut stream, json!({"type":"response.output_item.added","output_index":0,"item":{"type":"web_search_call","id":"ws_1","status":"in_progress","action":{"type":"search","query":"Rust 🦀"}}})));
    assert!(ingest(
        &mut stream,
        json!({"type":"response.web_search_call.searching","output_index":0,"item_id":"wrong"})
    )
    .is_empty());
    events.extend(ingest(
        &mut stream,
        json!({"type":"response.web_search_call.searching","output_index":0,"item_id":"ws_1"}),
    ));
    events.extend(ingest(
        &mut stream,
        json!({"type":"response.web_search_call.completed","output_index":0,"item_id":"ws_1"}),
    ));
    let item = json!({"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"Rust 🦀","sources":[{"url":"https://www.rust-lang.org/","title":"Rust"}]}});
    events.extend(ingest(
        &mut stream,
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
    ));
    events.extend(ingest(&mut stream, json!({"type":"response.completed","response":{"id":"resp_web","status":"completed","output":[]}})));
    let result = stream.finish().unwrap();
    assert!(result.output.tool_calls.is_empty());
    assert_eq!(result.output_items, vec![item]);
    let phases = events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::WebSearchUpdate {
                item_id,
                status,
                query,
                ..
            } => {
                assert_eq!(item_id, "ws_1");
                assert_eq!(query.as_deref(), Some("Rust 🦀"));
                Some(status.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        phases,
        ["in_progress", "searching", "completed", "completed"]
    );
    assert!(events.iter().all(|event| !matches!(
        event,
        ModelStreamEvent::ToolCallDelta { .. } | ModelStreamEvent::ToolCallReady { .. }
    )));
    let ready = response_ready_events(&result, false, &HashSet::new());
    assert!(
        matches!(&ready[0], ModelStreamEvent::WebSearchUpdate { sources, .. } if sources.len() == 1)
    );
}

#[test]
fn hosted_search_sanitizes_utf8_and_escaped_payload_without_changing_raw_output() {
    let item = json!({"type":"web_search_call","id":"ws_large","status":"completed","action":{
        "type":"search", "query":"🦀\n\u{0001}".repeat(2000), "url":"https://example.com/".to_owned()+&"中".repeat(4000),
        "sources":(0..30).map(|index| json!({"title":format!("{index} {}", "\u{0001}".repeat(600)),"url":format!("https://example.com/{index}/{}", "é".repeat(4000))})).collect::<Vec<_>>()
    }});
    let original = item.clone();
    let event = hosted_web_search_presentation(&item).unwrap();
    assert!(serde_json::to_vec(&event).unwrap().len() <= 8 * 1024);
    match event {
        ModelStreamEvent::WebSearchUpdate {
            query,
            url,
            sources,
            ..
        } => {
            assert!(query.unwrap().len() <= 1024);
            assert!(url.unwrap().len() <= 2048);
            assert!(sources.len() <= 8);
            assert!(sources
                .iter()
                .all(|(title, url)| title.len() <= 256 && url.len() <= 2048));
        }
        _ => unreachable!(),
    }
    assert_eq!(item, original);
    for invalid_id in [String::new(), "x".repeat(257), "line\nbreak".into()] {
        let mut invalid = original.clone();
        invalid["id"] = json!(invalid_id);
        assert!(hosted_web_search_presentation(&invalid).is_none());
    }
}

#[tokio::test]
async fn hosted_search_sender_enforces_small_snapshot_before_general_event_channel() {
    let (sender, mut receiver) =
        crate::model_event_channel(tokio_util::sync::CancellationToken::new());
    sender
        .send(ModelStreamEvent::WebSearchUpdate {
            item_id: "ws_channel".into(),
            status: "in_progress".into(),
            action: Some("search".into()),
            query: Some("\u{0001}".repeat(100_000)),
            url: Some("🦀".repeat(100_000)),
            sources: vec![("title".repeat(1000), "url".repeat(1000)); 16],
        })
        .await
        .unwrap();
    let event = receiver.recv().await.unwrap();
    let bytes = serde_json::to_vec(&event).unwrap().len();
    assert!(bytes <= 8 * 1024);
    assert!(bytes <= crate::MAX_MODEL_EVENT_BYTES);
}

#[test]
fn hosted_search_identity_collisions_fail_and_incomplete_response_never_finishes() {
    let mut stream = ResponseStream::default();
    ingest(
        &mut stream,
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"web_search_call","id":"ws_1","status":"in_progress"}}),
    );
    assert!(stream.web_search.observe(&json!({"type":"response.output_item.added","output_index":1,"item":{"type":"web_search_call","id":"ws_1","status":"in_progress"}})).is_err());
    assert!(stream.web_search.observe(&json!({"type":"response.output_item.added","output_index":0,"item":{"type":"web_search_call","id":"ws_2","status":"in_progress"}})).is_err());
    assert!(
        stream.published_activity,
        "a remotely started search makes retry ambiguous"
    );
    assert!(stream.finish().is_err());
    let mut failed = ResponseStream::default();
    let mut ignored = Vec::new();
    assert!(failed.ingest_events(b"data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\"}}\n\n", &mut ignored).is_err());
}
