use super::*;
use futures_util::FutureExt;

#[tokio::test]
async fn fragments_preserve_unicode_and_json_escapes_for_every_delta_kind() {
    let content = "Café e\u{301} 中🦀\n\r\t\0\"\\".repeat(12_000);
    for kind in 0..3 {
        let event = match kind {
            0 => ModelStreamEvent::TextDelta {
                content: content.clone(),
            },
            1 => ModelStreamEvent::ReasoningDelta {
                item_id: "reasoning-🦀".into(),
                content: content.clone(),
            },
            _ => ModelStreamEvent::ToolCallDelta {
                item_id: "tool-中".into(),
                call_id: Some("call-🦀".into()),
                name: Some("write_file".into()),
                arguments_delta: content.clone(),
            },
        };
        let (sender, mut receiver) = model_event_channel(CancellationToken::new());
        let producer = tokio::spawn(async move { sender.send(event).await });
        let mut assembled = String::new();
        let mut fragments = 0;
        while let Some(event) = receiver.recv().await {
            assert!(serde_json::to_vec(&event).unwrap().len() <= MAX_MODEL_EVENT_BYTES);
            fragments += 1;
            match event {
                ModelStreamEvent::TextDelta { content } => assembled.push_str(&content),
                ModelStreamEvent::ReasoningDelta { item_id, content } => {
                    assert_eq!(item_id, "reasoning-🦀");
                    assembled.push_str(&content);
                }
                ModelStreamEvent::ToolCallDelta {
                    item_id,
                    call_id,
                    name,
                    arguments_delta,
                } => {
                    assert_eq!(item_id, "tool-中");
                    assert_eq!(call_id.as_deref(), Some("call-🦀"));
                    assert_eq!(name.as_deref(), Some("write_file"));
                    assembled.push_str(&arguments_delta);
                }
                _ => panic!("unexpected delta kind"),
            }
        }
        producer.await.unwrap().unwrap();
        assert!(fragments > MODEL_EVENT_CHANNEL_CAPACITY);
        assert_eq!(assembled, content);
    }
}

#[tokio::test]
async fn cancellation_releases_a_backpressured_send_and_keeps_accepted_prefix() {
    let cancel = CancellationToken::new();
    let (sender, mut receiver) = model_event_channel(cancel.clone());
    for index in 0..MODEL_EVENT_CHANNEL_CAPACITY {
        sender
            .send(ModelStreamEvent::TextDelta {
                content: index.to_string(),
            })
            .await
            .unwrap();
    }
    let mut blocked = Box::pin(sender.send(ModelStreamEvent::TextDelta {
        content: "must not appear".into(),
    }));
    assert!((&mut blocked).now_or_never().is_none());
    cancel.cancel();
    assert!(matches!(blocked.await, Err(AgentError::Cancelled)));
    for index in 0..MODEL_EVENT_CHANNEL_CAPACITY {
        assert!(matches!(
            receiver.try_recv().unwrap(),
            ModelStreamEvent::TextDelta { content } if content == index.to_string()
        ));
    }
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn closing_receiver_releases_backpressure_without_cancelling_shared_token() {
    let cancel = CancellationToken::new();
    let (sender, mut receiver) = model_event_channel(cancel.clone());
    for _ in 0..MODEL_EVENT_CHANNEL_CAPACITY {
        sender
            .send(ModelStreamEvent::TextDelta {
                content: "accepted".into(),
            })
            .await
            .unwrap();
    }
    let mut blocked = Box::pin(sender.send(ModelStreamEvent::TextDelta {
        content: "late".into(),
    }));
    assert!((&mut blocked).now_or_never().is_none());
    receiver.close();
    assert!(
        matches!(blocked.await, Err(AgentError::Runtime(message)) if message.contains("consumer closed"))
    );
    assert!(!cancel.is_cancelled());
    assert_eq!(receiver.len(), MODEL_EVENT_CHANNEL_CAPACITY);
}

#[tokio::test]
async fn abandoned_consumer_does_not_poll_provider_operation() {
    let (sender, receiver) = model_event_channel(CancellationToken::new());
    drop(receiver);
    let result: Result<(), AgentError> = sender
        .while_open(async { panic!("an abandoned stream must not start another request") })
        .await;
    assert!(
        matches!(result, Err(AgentError::Runtime(message)) if message.contains("consumer closed"))
    );
}

#[tokio::test]
async fn escaped_metadata_is_rejected_before_enqueue() {
    let (sender, mut receiver) = model_event_channel(CancellationToken::new());
    let result = sender
        .send(ModelStreamEvent::ToolCallDelta {
            item_id: "\0".repeat(6_000),
            call_id: None,
            name: None,
            arguments_delta: "{}".into(),
        })
        .await;
    assert!(matches!(result, Err(AgentError::Limit(_))));
    assert!(receiver.try_recv().is_err());
}
