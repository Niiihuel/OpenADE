use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

fn event(assembler: &mut SseAssembler, delta: Value, events: &mut Vec<ModelStreamEvent>) {
    assembler
        .ingest_events(
            format!("data: {}", json!({"choices":[{"delta":delta}]})).as_bytes(),
            events,
        )
        .unwrap();
}

#[test]
fn chat_reasoning_preserves_fragmented_details_and_never_surfaces_signatures() {
    let mut assembler = SseAssembler::default();
    let mut events = Vec::new();
    event(
        &mut assembler,
        json!({"reasoning_details":[
            {"index":0,"id":"think-1","type":"reasoning.text","format":"MiniMax-response-v1","text":"Café "},
            {"index":1,"id":"opaque-1","type":"reasoning.encrypted","data":"secret-part-"}
        ]}),
        &mut events,
    );
    event(
        &mut assembler,
        json!({"reasoning_details":[
        {"index":0,"id":"think-1","text":"🦀"},
        {"index":1,"id":"opaque-1","data":"two","signature":"signature-only"}
    ],"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}}]}),
        &mut events,
    );
    assembler
        .ingest_events(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}",
            &mut events,
        )
        .unwrap();
    assembler
        .ingest_events(b"data: [DONE]", &mut events)
        .unwrap();
    let completion = assembler.finish_detailed().unwrap();
    assert!(completion.output.content.is_empty());
    let mut message =
        AgentMessage::assistant(completion.output.content, completion.output.tool_calls);
    message.provider_output_items = completion.provider_output_items;
    let restored: AgentMessage =
        serde_json::from_slice(&serde_json::to_vec(&message).unwrap()).unwrap();
    let wire = message_to_wire(&restored);
    assert_eq!(wire["reasoning_details"][0]["text"], "Café 🦀");
    assert_eq!(
        wire["reasoning_details"][0]["format"],
        "MiniMax-response-v1"
    );
    assert_eq!(wire["reasoning_details"][1]["data"], "secret-part-two");
    assert_eq!(wire["reasoning_details"][1]["signature"], "signature-only");
    assert_eq!(wire["tool_calls"][0]["id"], "call-1");
    let reasoning: String = events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::ReasoningDelta { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "Café 🦀");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::TextDelta { .. }))
    );
    let mut user = AgentMessage::user("literal user text");
    user.provider_output_items = restored.provider_output_items;
    assert!(message_to_wire(&user).get("reasoning_details").is_none());
}

#[test]
fn chat_reasoning_is_bounded_and_rejects_changed_identity() {
    let mut reasoning = ChatReasoning::default();
    let mut bytes = MAX_ASSEMBLED_STREAM_BYTES - 1;
    assert!(matches!(
        reasoning.ingest(
            &json!({"reasoning_content":"🦀"}),
            &mut bytes,
            &mut Vec::new()
        ),
        Err(AgentError::Limit(_))
    ));
    let mut reasoning = ChatReasoning::default();
    let mut bytes = 0;
    reasoning
        .ingest(
            &json!({"reasoning_details":[{"index":0,"id":"first","text":"a"}]}),
            &mut bytes,
            &mut Vec::new(),
        )
        .unwrap();
    assert!(
        reasoning
            .ingest(
                &json!({"reasoning_details":[{"index":0,"id":"second","text":"b"}]}),
                &mut bytes,
                &mut Vec::new()
            )
            .is_err()
    );
    assert!(
        ChatReasoning::default()
            .ingest(
                &json!({"reasoning_details":[{"index":128,"text":"x"}]}),
                &mut 0,
                &mut Vec::new()
            )
            .is_err()
    );
    assert!(
        ChatReasoning::default()
            .ingest(&json!({"reasoning_content":{}}), &mut 0, &mut Vec::new())
            .is_err()
    );
}

#[test]
fn minimax_http_200_error_envelope_is_not_a_successful_completion() {
    let body = json!({"base_resp":{"status_code":1008,"status_msg":"Insufficient balance"},"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":""}}]});
    assert!(
        parse_model_output(&body)
            .unwrap_err()
            .to_string()
            .contains("1008")
    );
    let mut assembler = SseAssembler::default();
    assert!(
        assembler
            .ingest_events(format!("data: {body}").as_bytes(), &mut Vec::new())
            .is_err()
    );
    assembler.done = true;
    assembler.finish_reason = Some("stop".into());
    assert!(
        assembler.finish_detailed().is_err(),
        "provider error remains terminal"
    );
    assert!(validate_chat_provider_status(&json!({"base_resp":{"status_code":0}})).is_ok());
}

#[test]
fn chat_usage_stream_options_are_only_sent_for_streaming_requests() {
    let backend = OpenAiCompatibleBackend::new("http://127.0.0.1:9/v1", None, "MiniMax-M3")
        .with_request_field("stream_options", json!({"include_usage":true}))
        .unwrap()
        .with_request_field("reasoning_split", json!(true))
        .unwrap();
    let messages = [AgentMessage::user("fixture")];
    let streamed = backend.request_body(&messages, &[], true).unwrap();
    let ordinary = backend.request_body(&messages, &[], false).unwrap();
    assert_eq!(streamed["stream_options"]["include_usage"], true);
    assert!(ordinary.get("stream_options").is_none());
    assert_eq!(ordinary["reasoning_split"], true);
}

#[tokio::test]
async fn minimax_and_zai_roundtrip_reasoning_tools_and_bearer_auth_over_local_http() {
    for (model, prefix, minimax) in [
        ("MiniMax-M3", "/v1", true),
        ("glm-5.3", "/api/paas/v4", false),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            for round in 0..2 {
                let deadline = std::time::Instant::now() + Duration::from_secs(8);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "local fixture request timed out"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("local fixture accept failed: {error}"),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                assert!(first.starts_with(&format!("POST {prefix}/chat/completions ")));
                let mut length = 0;
                let mut authorization = String::new();
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(n) = lower.strip_prefix("content-length:") {
                        length = n.trim().parse::<usize>().unwrap();
                    }
                    if let Some(value) = line
                        .split_once(':')
                        .filter(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                    {
                        authorization = value.1.trim().into();
                    }
                }
                assert_eq!(authorization, "Bearer fake-contract-key");
                assert!(length < 1024 * 1024);
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(body["model"], model);
                assert_eq!(body["stream"], true);
                assert_eq!(body["tools"][0]["function"]["name"], "read_file");
                if minimax {
                    assert_eq!(body["reasoning_split"], true);
                } else {
                    assert_eq!(
                        body["thinking"],
                        json!({"type":"enabled","clear_thinking":false})
                    );
                    assert_eq!(body["tool_stream"], true);
                }
                if round == 1 {
                    assert_eq!(body["messages"][1]["reasoning_content"], "Inspect café 🦀");
                    assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call-1");
                    assert_eq!(body["messages"][2]["role"], "tool");
                    assert_eq!(body["messages"][2]["tool_call_id"], "call-1");
                    assert_eq!(body["messages"][2]["content"], "safe fixture content");
                }
                let (mime, response) = if round == 0 {
                    let chunks = [
                        json!({"id":"response-1","choices":[{"delta":{"reasoning_content":"Inspect café "}}]}),
                        json!({"choices":[{"delta":{"reasoning_content":"🦀","tool_calls":[{"index":0,"id":"call-1","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}),
                        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"main.rs\"}"}}]},"finish_reason":"tool_calls"}]}),
                        json!({"choices":[],"usage":{"prompt_tokens":21,"completion_tokens":8,"total_tokens":29}}),
                    ];
                    (
                        "text/event-stream",
                        chunks
                            .iter()
                            .map(|c| format!("data: {c}\n\n"))
                            .collect::<String>()
                            + "data: [DONE]\n\n",
                    )
                } else {
                    ("application/json", json!({"id":"response-2","choices":[{"finish_reason":"stop","message":{"role":"assistant","reasoning_content":"Now answer","content":"Complete."}}]}).to_string())
                };
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
                socket.flush().unwrap();
            }
        });
        let mut backend = OpenAiCompatibleBackend::new(
            format!("http://{address}{prefix}"),
            Some("fake-contract-key".into()),
            model,
        )
        .with_timeout(Duration::from_secs(5))
        .with_capabilities(ModelRequestCapabilities {
            tool_calls: true,
            ..Default::default()
        })
        .unwrap();
        backend = if minimax {
            backend
                .with_request_field("reasoning_split", json!(true))
                .unwrap()
        } else {
            backend
                .with_request_field("thinking", json!({"type":"enabled","clear_thinking":false}))
                .unwrap()
                .with_request_field("tool_stream", json!(true))
                .unwrap()
        };
        let tools = [ToolDefinition {
            name: "read_file".into(),
            description: "Read fixture".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
        }];
        let mut history = vec![AgentMessage::user("Read the fixture")];
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        let first = backend
            .stream_complete_detailed(&history, &tools, CancellationToken::new(), sender)
            .await
            .unwrap();
        assert_eq!(
            first.output.tool_calls[0].arguments,
            json!({"path":"main.rs"})
        );
        assert_eq!(first.usage.unwrap().input_tokens, 21);
        let mut assistant = AgentMessage::assistant(first.output.content, first.output.tool_calls);
        assistant.provider_output_items = first.provider_output_items;
        // Same durable serialization path as journal/replay; no reconstituted reasoning.
        history.push(serde_json::from_slice(&serde_json::to_vec(&assistant).unwrap()).unwrap());
        history.push(AgentMessage::tool("call-1", "safe fixture content"));
        let (sender, mut receiver) = crate::model_event_channel(CancellationToken::new());
        let second = backend
            .stream_events(&history, &tools, CancellationToken::new(), sender)
            .await
            .unwrap();
        assert_eq!(second.output.content, "Complete.");
        assert_eq!(
            second.provider_output_items[0]["reasoning_content"],
            "Now answer"
        );
        assert!(matches!(
            receiver.try_recv().unwrap(),
            ModelStreamEvent::Started { .. }
        ));
        assert!(
            matches!(receiver.try_recv().unwrap(), ModelStreamEvent::ReasoningDelta { content, .. } if content == "Now answer")
        );
        assert!(
            matches!(receiver.try_recv().unwrap(), ModelStreamEvent::TextDelta { content } if content == "Complete.")
        );
        assert!(receiver.try_recv().is_err());
        server.join().unwrap();
    }
}
