use async_trait::async_trait;
use openide_core::{AgentMessage, ModelStreamEvent};
use openide_harness::{
    model_event_channel, AgentError, AnthropicMessagesBackend, ModelBackend, ModelOutput,
    OpenAiCompatibleBackend, ResponsesBackend, ToolDefinition,
};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
enum Provider {
    Chat,
    Responses,
    Anthropic,
}

const PROVIDERS: [Provider; 3] = [Provider::Chat, Provider::Responses, Provider::Anthropic];

fn backend(provider: Provider, address: std::net::SocketAddr) -> Box<dyn ModelBackend> {
    let base = format!("http://{address}/v1");
    match provider {
        Provider::Chat => Box::new(OpenAiCompatibleBackend::new(base, None, "fixture")),
        Provider::Responses => Box::new(ResponsesBackend::new(base, None, "fixture")),
        Provider::Anthropic => Box::new(AnthropicMessagesBackend::new(base, None, "fixture")),
    }
}

async fn read_request(socket: &mut TcpStream) {
    let mut request = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let count = socket.read(&mut chunk).await.unwrap();
        assert!(count > 0, "client closed before sending its request");
        request.extend_from_slice(&chunk[..count]);
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&request[..end])
                .unwrap()
                .to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse::<usize>()
                .unwrap();
            if request.len() >= end + 4 + length {
                return;
            }
        }
    }
}

#[tokio::test]
async fn native_stream_abandonment_closes_stalled_http_before_headers_and_between_events() {
    for provider in PROVIDERS {
        for send_headers in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let backend = backend(provider, listener.local_addr().unwrap());
            let (ready, received) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                if send_headers {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n:\n\n\r\n").await.unwrap();
                }
                ready.send(()).unwrap();
                let mut buffer = [0; 1];
                let count = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buffer))
                    .await
                    .expect("abandoned provider connection stayed open")
                    .unwrap();
                assert_eq!(count, 0, "client continued its abandoned request");
            });
            let cancel = CancellationToken::new();
            let token = cancel.clone();
            let (sender, receiver) = model_event_channel(cancel);
            let run = tokio::spawn(async move {
                backend
                    .stream_events(&[AgentMessage::user("Hi")], &[], token, sender)
                    .await
            });
            received.await.unwrap();
            drop(receiver);
            let result = tokio::time::timeout(Duration::from_secs(1), run)
                .await
                .unwrap_or_else(|_| panic!("{provider:?} did not abandon its HTTP operation"))
                .unwrap();
            assert!(
                matches!(result, Err(AgentError::Runtime(message)) if message.contains("consumer closed"))
            );
            server.await.unwrap();
        }
    }
}

fn completion(provider: Provider, arguments: &Value) -> Value {
    match provider {
        Provider::Chat => json!({
            "id":"response-1",
            "choices":[{"finish_reason":"tool_calls","message":{
                "role":"assistant","content":"Listo 🦀",
                "tool_calls":[{"id":"call-1","type":"function","function":{
                    "name":"write_file","arguments":arguments.to_string()
                }}]
            }}]
        }),
        Provider::Responses => json!({
            "id":"response-1","status":"completed","output":[
                {"type":"message","id":"message-1","role":"assistant","content":[{"type":"output_text","text":"Listo 🦀"}]},
                {"type":"function_call","id":"item-1","call_id":"call-1","name":"write_file","arguments":arguments.to_string(),"status":"completed"}
            ]
        }),
        Provider::Anthropic => json!({
            "type":"message","id":"response-1","role":"assistant","model":"fixture",
            "stop_reason":"tool_use","stop_sequence":null,
            "usage":{"input_tokens":10,"output_tokens":20},
            "content":[
                {"type":"text","text":"Listo 🦀"},
                {"type":"tool_use","id":"call-1","name":"write_file","input":arguments}
            ]
        }),
    }
}

#[tokio::test]
async fn native_stream_json_fallback_preserves_tool_arguments_before_ready() {
    let arguments = json!({"path":"src/Café.rs","content":"中🦀\n\"\\\0".repeat(8_000)});
    for provider in PROVIDERS {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = backend(provider, listener.local_addr().unwrap());
        let body = completion(provider, &arguments).to_string();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        });
        let cancel = CancellationToken::new();
        let (sender, mut receiver) = model_event_channel(cancel.clone());
        let run = tokio::spawn(async move {
            backend
                .stream_events(&[AgentMessage::user("Hi")], &[], cancel, sender)
                .await
        });
        let mut preview = String::new();
        let mut ready = false;
        while let Some(event) = receiver.recv().await {
            match event {
                ModelStreamEvent::ToolCallDelta {
                    arguments_delta,
                    call_id,
                    name,
                    ..
                } => {
                    assert!(!ready, "arguments arrived after ready");
                    assert_eq!(call_id.as_deref(), Some("call-1"));
                    assert_eq!(name.as_deref(), Some("write_file"));
                    preview.push_str(&arguments_delta);
                }
                ModelStreamEvent::ToolCallReady { call_id, .. } => {
                    assert_eq!(call_id, "call-1");
                    assert_eq!(serde_json::from_str::<Value>(&preview).unwrap(), arguments);
                    ready = true;
                }
                _ => {}
            }
        }
        let completion = run.await.unwrap().unwrap();
        assert!(ready);
        assert_eq!(completion.output.tool_calls[0].arguments, arguments);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn native_stream_responses_snapshot_fills_missing_previews_without_replaying_deltas() {
    let arguments = json!({"path":"src/Café.rs","content":"中🦀"});
    for include_deltas in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = backend(Provider::Responses, listener.local_addr().unwrap());
        let mut frames = Vec::new();
        if include_deltas {
            frames.push(json!({"type":"response.output_text.delta","delta":"Listo 🦀"}));
            frames.push(json!({"type":"response.output_item.added","output_index":1,"item":{
                "type":"function_call","id":"item-1","call_id":"call-1","name":"write_file","arguments":""
            }}));
            frames.push(json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"item-1","delta":arguments.to_string()}));
        }
        frames.push(json!({"type":"response.completed","response":completion(Provider::Responses, &arguments)}));
        let body = frames
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            socket.write_all(headers.as_bytes()).await.unwrap();
            // Exercise SSE framing even when UTF-8 and JSON escape sequences
            // straddle transport writes.
            for fragment in body.as_bytes().chunks(3) {
                socket.write_all(fragment).await.unwrap();
            }
        });
        let cancel = CancellationToken::new();
        let (sender, mut receiver) = model_event_channel(cancel.clone());
        let run = tokio::spawn(async move {
            backend
                .stream_events(&[AgentMessage::user("Hi")], &[], cancel, sender)
                .await
        });
        let mut text = String::new();
        let mut preview = String::new();
        let mut ready = false;
        while let Some(event) = receiver.recv().await {
            match event {
                ModelStreamEvent::TextDelta { content } => text.push_str(&content),
                ModelStreamEvent::ToolCallDelta {
                    item_id,
                    arguments_delta,
                    ..
                } => {
                    assert_eq!(item_id, "item-1");
                    assert!(!ready);
                    preview.push_str(&arguments_delta);
                }
                ModelStreamEvent::ToolCallReady { item_id, .. } => {
                    assert_eq!(item_id, "item-1");
                    assert!(!ready);
                    ready = true;
                }
                _ => {}
            }
        }
        run.await.unwrap().unwrap();
        assert_eq!(text, "Listo 🦀");
        assert_eq!(serde_json::from_str::<Value>(&preview).unwrap(), arguments);
        assert!(ready);
        server.await.unwrap();
    }
}

struct SilentLegacyBackend;

#[async_trait]
impl ModelBackend for SilentLegacyBackend {
    fn model_id(&self) -> &str {
        "silent-legacy"
    }

    async fn complete(
        &self,
        _messages: &[AgentMessage],
        _tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn native_stream_legacy_bridge_abandons_silent_completion() {
    let cancel = CancellationToken::new();
    let (sender, receiver) = model_event_channel(cancel.clone());
    let run = tokio::spawn(async move {
        SilentLegacyBackend
            .stream_events(&[], &[], cancel, sender)
            .await
    });
    tokio::task::yield_now().await;
    drop(receiver);
    let result = tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .expect("legacy bridge waited for a completion after its consumer closed")
        .unwrap();
    assert!(
        matches!(result, Err(AgentError::Runtime(message)) if message.contains("consumer closed"))
    );
}
