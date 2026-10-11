use super::*;
use openide_core::{UserImage, UserImageMime};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use tokio::sync::mpsc;

const MODEL: &str = "organization/exact-model-ñ";

fn test_endpoint_digest() -> String {
    openide_core::native_model_endpoint_digest("http://localhost/v1/messages").unwrap()
}

fn backend(endpoint: &str) -> AnthropicMessagesBackend {
    AnthropicMessagesBackend::new(
        endpoint,
        Some("local-test-key-not-an-account".into()),
        MODEL,
    )
    .with_timeout(Duration::from_secs(3))
}

fn tool() -> ToolDefinition {
    ToolDefinition {
        name: "read_file".into(),
        description: "Read exactly".into(),
        parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
    }
}

fn message(blocks: Vec<Value>, reason: &str) -> Value {
    json!({"type":"message","id":"msg_exact","role":"assistant","model":MODEL,"content":blocks,"stop_reason":reason,"stop_sequence":null,"usage":{"input_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":7,"output_tokens":30,"output_tokens_details":{"thinking_tokens":12}}})
}

fn start() -> Value {
    json!({"type":"message_start","message":{"type":"message","id":"msg_exact","role":"assistant","model":MODEL,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":7,"output_tokens":1}}})
}

fn event(value: Value) -> Vec<u8> {
    format!(
        "event: {}\r\ndata: {}\r\n\r\n",
        value["type"].as_str().unwrap(),
        value
    )
    .into_bytes()
}

fn finish(reason: &str) -> Vec<Vec<u8>> {
    vec![
        event(
            json!({"type":"message_delta","delta":{"stop_reason":reason,"stop_sequence":null},"usage":{"output_tokens":30,"output_tokens_details":{"thinking_tokens":12}}}),
        ),
        event(json!({"type":"message_stop"})),
    ]
}

fn text_stream(text: &str, terminal: bool) -> Vec<Vec<u8>> {
    let mut frames = vec![
        event(start()),
        event(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        event(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        ),
        event(json!({"type":"content_block_stop","index":0})),
    ];
    if terminal {
        frames.extend(finish("end_turn"));
    }
    frames
}

#[derive(Clone, Debug)]
struct Request {
    target: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

struct Reply {
    status: u16,
    content_type: &'static str,
    chunks: Vec<Vec<u8>>,
    gate: Option<Arc<AtomicBool>>,
    location: Option<String>,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            chunks: vec![value.to_string().into_bytes()],
            gate: None,
            location: None,
        }
    }
    fn sse(chunks: Vec<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            chunks,
            gate: None,
            location: None,
        }
    }
}

struct Server {
    endpoint: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Server {
    fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = requests.clone();
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            for reply in replies {
                let mut connection = loop {
                    if stopped.load(Ordering::Acquire) {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    }
                };
                connection
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                seen.lock().unwrap().push(read_request(&mut connection));
                let location = reply
                    .location
                    .as_ref()
                    .map(|value| format!("Location: {value}\r\n"))
                    .unwrap_or_default();
                let headers = format!("HTTP/1.1 {} fixture\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nRetry-After: 0\r\n{location}Connection: close\r\n\r\n", reply.status, reply.content_type);
                if connection.write_all(headers.as_bytes()).is_err() {
                    continue;
                }
                for (index, chunk) in reply.chunks.iter().enumerate() {
                    if index > 0 {
                        if let Some(gate) = &reply.gate {
                            while !gate.load(Ordering::Acquire) && !stopped.load(Ordering::Acquire)
                            {
                                std::thread::sleep(Duration::from_millis(2));
                            }
                        }
                    }
                    if stopped.load(Ordering::Acquire) {
                        return;
                    }
                    if write!(connection, "{:x}\r\n", chunk.len())
                        .and_then(|_| connection.write_all(chunk))
                        .and_then(|_| connection.write_all(b"\r\n"))
                        .and_then(|_| connection.flush())
                        .is_err()
                    {
                        break;
                    }
                }
                let _ = connection.write_all(b"0\r\n\r\n");
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 8192];
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "EOF inside request");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(at) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let header = std::str::from_utf8(&bytes[..boundary]).unwrap();
    let target = header
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .into();
    let headers: BTreeMap<String, String> = header
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().into()))
        .collect();
    let length = headers["content-length"].parse::<usize>().unwrap();
    while bytes.len() < boundary + length {
        let mut chunk = [0_u8; 8192];
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "EOF inside request body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Request {
        target,
        headers,
        body: serde_json::from_slice(&bytes[boundary..boundary + length]).unwrap(),
    }
}

fn ingest(
    values: impl IntoIterator<Item = Value>,
) -> Result<(AnthropicStream, Vec<String>), AgentError> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut stream = AnthropicStream::default();
    for value in values {
        stream.ingest(&event(value), MODEL, &sender)?;
    }
    let mut deltas = Vec::new();
    while let Ok(delta) = receiver.try_recv() {
        deltas.push(delta);
    }
    Ok((stream, deltas))
}

#[test]
fn options_and_headers_are_explicit_bounded_and_part_of_profile_without_credentials() {
    let original = backend("http://localhost/v1");
    let fingerprint = original.request_profile_fingerprint().unwrap();
    assert_eq!(
        fingerprint,
        AnthropicMessagesBackend::new(
            "http://different/v1",
            Some("different-secret".into()),
            MODEL
        )
        .request_profile_fingerprint()
        .unwrap()
    );
    assert_ne!(
        fingerprint,
        backend("http://localhost/v1")
            .with_max_output_tokens(2048)
            .unwrap()
            .request_profile_fingerprint()
            .unwrap()
    );
    assert_ne!(
        fingerprint,
        backend("http://localhost/v1")
            .with_image_input_enabled(false)
            .request_profile_fingerprint()
            .unwrap()
    );
    assert_ne!(
        fingerprint,
        backend("http://localhost/v1")
            .with_headers(BTreeMap::from([(
                "anthropic-workspace-id".into(),
                "workspace-exact".into()
            )]))
            .unwrap()
            .request_profile_fingerprint()
            .unwrap()
    );
    for fields in [
        json!({"model":"override"}),
        json!({"thinking":{"type":"enabled","budget_tokens":100}}),
        json!({"thinking":{"type":"enabled","budget_tokens":4096}}),
        json!({"thinking":{"type":"adaptive","budget_tokens":1024}}),
        json!({"thinking":{"type":"disabled","display":"omitted"}}),
        json!({"output_config":{"effort":"made-up"}}),
        json!({"output_config":{"effort":"high","format":{}}}),
        json!({"tools":[]}),
    ] {
        assert!(backend("http://localhost/v1")
            .with_request_fields(serde_json::from_value(fields).unwrap())
            .is_err());
    }
    let fields = BTreeMap::from([
        (
            "thinking".into(),
            json!({"type":"enabled","budget_tokens":1024,"display":"omitted"}),
        ),
        ("output_config".into(), json!({"effort":"high"})),
    ]);
    assert_ne!(
        fingerprint,
        backend("http://localhost/v1")
            .with_request_fields(fields.clone())
            .unwrap()
            .request_profile_fingerprint()
            .unwrap()
    );
    assert!(backend("http://localhost/v1")
        .with_request_fields(fields)
        .unwrap()
        .with_max_output_tokens(1024)
        .is_err());
    for headers in [
        BTreeMap::from([("X-Api-Key".into(), "secret".into())]),
        BTreeMap::from([("Authorization".into(), "secret".into())]),
        BTreeMap::from([("cookie".into(), "secret".into())]),
        BTreeMap::from([("anthropic-version".into(), "other".into())]),
        BTreeMap::from([("x-test".into(), "bad\nvalue".into())]),
        BTreeMap::from([("X-Test".into(), "a".into()), ("x-test".into(), "b".into())]),
    ] {
        assert!(backend("http://localhost/v1")
            .with_headers(headers)
            .is_err());
    }
}

#[test]
fn ordinary_json_preserves_signed_blocks_and_rejects_every_unsafe_terminal_and_foreign_identity() {
    let blocks = vec![
        json!({"type":"thinking","thinking":"private ñ","signature":"exact_signature"}),
        json!({"type":"redacted_thinking","data":"opaque_bytes"}),
        json!({"type":"text","text":"Visible 🦀"}),
        json!({"type":"tool_use","id":"tool_exact","name":"read_file","input":{"path":"ñ.rs"}}),
    ];
    let value = message(blocks.clone(), "tool_use");
    let completion = parse_message(&value, MODEL, &test_endpoint_digest()).unwrap();
    assert_eq!(completion.output.content, "Visible 🦀");
    assert_eq!(
        completion.provider_output_items[0]["content"],
        json!(blocks)
    );
    assert_eq!(
        completion.usage.unwrap(),
        ModelTokenUsage {
            input_tokens: 32,
            output_tokens: 30,
            total_tokens: 62,
            reasoning_tokens: Some(12),
            cached_input_tokens: Some(5)
        }
    );
    for reason in [
        "max_tokens",
        "pause_turn",
        "refusal",
        "stop_sequence",
        "future",
        "end_turn",
    ] {
        let mut candidate = value.clone();
        candidate["stop_reason"] = reason.into();
        assert!(parse_message(&candidate, MODEL, &test_endpoint_digest()).is_err());
    }
    for (field, replacement) in [
        ("model", json!("foreign")),
        ("role", json!("user")),
        ("stop_details", json!({"type":"refusal"})),
        ("error", json!({"type":"overloaded_error"})),
        ("usage", json!({"input_tokens":u64::MAX,"output_tokens":1})),
    ] {
        let mut candidate = value.clone();
        candidate[field] = replacement;
        assert!(parse_message(&candidate, MODEL, &test_endpoint_digest()).is_err());
    }
    let mut duplicate = value.clone();
    duplicate["content"]
        .as_array_mut()
        .unwrap()
        .push(blocks[3].clone());
    assert!(parse_message(&duplicate, MODEL, &test_endpoint_digest()).is_err());
    let mut unknown = value;
    unknown["content"][0]["type"] = "server_tool_use".into();
    assert!(parse_message(&unknown, MODEL, &test_endpoint_digest()).is_err());
}

#[test]
fn history_groups_tool_results_preserves_envelope_and_rejects_foreign_or_conflicting_opaque_items()
{
    let completion = parse_message(
        &message(
            vec![
                json!({"type":"thinking","thinking":"opaque","signature":"signed"}),
                json!({"type":"tool_use","id":"a","name":"read_file","input":{"path":"a"}}),
                json!({"type":"tool_use","id":"b","name":"read_file","input":{"path":"b"}}),
            ],
            "tool_use",
        ),
        MODEL,
        &test_endpoint_digest(),
    )
    .unwrap();
    let mut assistant =
        AgentMessage::assistant(completion.output.content, completion.output.tool_calls);
    assistant.provider_output_items = completion.provider_output_items;
    let history = vec![
        AgentMessage::system("system exact\n"),
        AgentMessage::user("question"),
        assistant.clone(),
        AgentMessage::tool("a", "first"),
        AgentMessage::tool("b", "second"),
        AgentMessage::user("next"),
    ];
    let adapter = backend("http://localhost/v1");
    let body = adapter
        .request_body(&history, &[tool()], true, false)
        .unwrap();
    assert_eq!(body["system"][0]["text"], "system exact\n");
    assert_eq!(
        body["messages"][1]["content"],
        assistant.provider_output_items[0]["content"]
    );
    assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(body["messages"][2]["content"][1]["tool_use_id"], "b");
    assert_eq!(body["messages"][2]["content"][2]["text"], "next");
    assert_eq!(body["tools"][0]["input_schema"], tool().parameters);
    for change in 0..4 {
        let mut history = history.clone();
        match change {
            0 => history[2].provider_output_items[0]["model_id"] = "foreign".into(),
            1 => {
                history[2].provider_output_items =
                    vec![json!({"type":"reasoning","encrypted_content":"foreign"})]
            }
            2 => history[2].content = "different text".into(),
            _ => history[4].tool_call_id = Some("a".into()),
        };
        assert!(adapter
            .request_body(&history, &[tool()], false, false)
            .is_err());
    }
}

#[test]
fn sse_rejects_duplicate_out_of_order_unknown_and_malformed_blocks_without_tools() {
    for invalid in [
        start(),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"orphan"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_stop"}),
        json!({"type":"new_event"}),
    ] {
        assert!(ingest([start(), invalid]).is_err());
    }
    let opening = json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"a","name":"read_file","input":{}}});
    for args in ["{\"path\":", "[]", "null"] {
        assert!(ingest([start(),opening.clone(),json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":args}}),json!({"type":"content_block_stop","index":0})]).is_err());
    }
    assert!(ingest([start(),json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),json!({"type":"content_block_stop","index":0})]).is_err());
}

#[test]
fn response_cardinality_and_aggregate_limits_cover_json_and_stream_before_mutation() {
    let calls:Vec<_>=(0..64).map(|index|json!({"type":"tool_use","id":format!("call_{index}"),"name":"read_file","input":{"path":"ñ"}})).collect();
    assert_eq!(
        parse_message(
            &message(calls.clone(), "tool_use"),
            MODEL,
            &test_endpoint_digest()
        )
        .unwrap()
        .output
        .tool_calls
        .len(),
        64
    );
    let mut more = calls;
    more.push(json!({"type":"tool_use","id":"call_64","name":"read_file","input":{}}));
    assert!(matches!(
        parse_message(&message(more, "tool_use"), MODEL, &test_endpoint_digest()),
        Err(AgentError::Limit(_))
    ));
    let (sender, _) = mpsc::unbounded_channel();
    let mut stream = AnthropicStream::default();
    stream.ingest(&event(start()), MODEL, &sender).unwrap();
    for index in 0..64 {
        stream.ingest(&event(json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":format!("c{index}"),"name":"read_file","input":{}}})),MODEL,&sender).unwrap();
        stream.ingest(&event(json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":""}})),MODEL,&sender).unwrap();
        stream
            .ingest(
                &event(json!({"type":"content_block_stop","index":index})),
                MODEL,
                &sender,
            )
            .unwrap();
    }
    assert!(matches!(stream.ingest(&event(json!({"type":"content_block_start","index":64,"content_block":{"type":"tool_use","id":"extra","name":"read_file","input":{}}})),MODEL,&sender),Err(AgentError::Limit(_))));
    assert_eq!(stream.blocks.len(), 64);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut stream = AnthropicStream::default();
    stream.ingest(&event(start()), MODEL, &sender).unwrap();
    stream.ingest(&event(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})),MODEL,&sender).unwrap();
    let fragment = "x".repeat(4096);
    let delta = event(
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":fragment}}),
    );
    let mut accepted = 0;
    loop {
        match stream.ingest(&delta, MODEL, &sender) {
            Ok(()) => accepted += 4096,
            Err(AgentError::Limit(_)) => break,
            Err(error) => panic!("unexpected {error}"),
        }
    }
    assert_eq!(stream.blocks[0]["text"].as_str().unwrap().len(), accepted);
    let mut observed = 0;
    while let Ok(delta) = receiver.try_recv() {
        observed += delta.len();
    }
    assert_eq!(observed, accepted);
    assert!(accepted < MAX_RETAINED_BYTES && accepted > MAX_RETAINED_BYTES - 8192);
    let huge = message(
        vec![json!({"type":"text","text":"x".repeat(MAX_RETAINED_BYTES)})],
        "end_turn",
    );
    assert!(matches!(
        parse_message(&huge, MODEL, &test_endpoint_digest()),
        Err(AgentError::Limit(_))
    ));
}

#[test]
fn late_tool_argument_overflow_and_invalid_utf8_keep_only_the_accepted_prefix() {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut stream = AnthropicStream::default();
    stream.ingest(&event(start()), MODEL, &sender).unwrap();
    stream.ingest(&event(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"accepted text"}})), MODEL, &sender).unwrap();
    stream
        .ingest(
            &event(json!({"type":"content_block_stop","index":0})),
            MODEL,
            &sender,
        )
        .unwrap();
    stream.ingest(&event(json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"large","name":"read_file","input":{}}})), MODEL, &sender).unwrap();
    stream.ingest(&event(json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\""}})), MODEL, &sender).unwrap();
    let delta = event(
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"x".repeat(4096)}}),
    );
    let mut accepted = stream.partial_json.len();
    loop {
        match stream.ingest(&delta, MODEL, &sender) {
            Ok(()) => accepted += 4096,
            Err(AgentError::Limit(_)) => break,
            Err(error) => panic!("unexpected {error}"),
        }
    }
    assert_eq!(stream.partial_json.len(), accepted);
    assert_eq!(stream.blocks[1]["input"], json!({}));
    assert_eq!(receiver.try_recv().unwrap(), "accepted text");
    assert!(receiver.try_recv().is_err());
    assert!(!stream.stopped);
    assert!(stream
        .ingest(
            b"event: ping\ndata: {\"type\":\"ping\",\"bad\":\"\xff\"}\n\n",
            MODEL,
            &sender
        )
        .is_err());
    assert_eq!(stream.partial_json.len(), accepted);
}

#[tokio::test]
async fn real_http_messages_headers_and_json_completion() {
    let server = Server::new(vec![Reply::json(message(
        vec![json!({"type":"text","text":"Actual ñ 🦀"})],
        "end_turn",
    ))]);
    let result = backend(&server.endpoint)
        .with_headers(BTreeMap::from([(
            "anthropic-workspace-id".into(),
            "workspace_exact".into(),
        )]))
        .unwrap()
        .complete_detailed(
            &[
                AgentMessage::system("exact system"),
                AgentMessage::user("question"),
            ],
            &[tool()],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.output.content, "Actual ñ 🦀");
    let request = &server.requests()[0];
    assert_eq!(request.target, "/v1/messages");
    assert_eq!(
        request.headers["x-api-key"],
        "local-test-key-not-an-account"
    );
    assert_eq!(request.headers["anthropic-version"], VERSION);
    assert_eq!(request.headers["anthropic-workspace-id"], "workspace_exact");
    assert!(!request.headers.contains_key("authorization"));
    assert_eq!(request.body["max_tokens"], 4096);
    assert_eq!(request.body["stream"], false);
}

#[tokio::test]
async fn real_http_count_tokens_includes_text_images_system_tools_and_options() {
    let image = UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let messages = vec![
        AgentMessage::system("system"),
        AgentMessage::user_with_images("image text", vec![image.clone()]).unwrap(),
    ];
    let server = Server::new(vec![
        Reply::json(json!({"input_tokens":123})),
        Reply::json(json!({"input_tokens":99})),
    ]);
    let adapter = backend(&server.endpoint)
        .with_request_fields(BTreeMap::from([
            ("thinking".into(), json!({"type":"adaptive"})),
            ("output_config".into(), json!({"effort":"high"})),
        ]))
        .unwrap();
    assert_eq!(
        adapter
            .count_input_tokens(&messages, &[tool()], CancellationToken::new())
            .await
            .unwrap(),
        Some(123)
    );
    assert_eq!(
        adapter
            .count_input_tokens(
                &[AgentMessage::user("text too")],
                &[],
                CancellationToken::new()
            )
            .await
            .unwrap(),
        Some(99)
    );
    let requests = server.requests();
    let first = &requests[0];
    assert_eq!(first.target, "/v1/messages/count_tokens");
    assert_eq!(
        first.body["messages"][0]["content"][1]["source"]["data"],
        image.data_base64
    );
    assert_eq!(first.body["system"][0]["text"], "system");
    assert_eq!(first.body["thinking"]["type"], "adaptive");
    assert_eq!(first.body["output_config"]["effort"], "high");
    assert!(first.body.get("max_tokens").is_none() && first.body.get("stream").is_none());
    assert!(backend(&server.endpoint)
        .with_image_input_enabled(false)
        .count_input_tokens(&messages, &[], CancellationToken::new())
        .await
        .is_err());
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn real_sse_streams_only_text_and_preserves_thinking_signature_redacted_and_completed_tool_input(
) {
    let mut frames = vec![
        event(start()),
        event(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        ),
        event(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Hidden ñ 🦀"}}),
        ),
        event(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"exact_signature"}}),
        ),
        event(json!({"type":"content_block_stop","index":0})),
        event(
            json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque"}}),
        ),
        event(json!({"type":"content_block_stop","index":1})),
        event(
            json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}),
        ),
        event(
            json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"Visible ñ 🦀"}}),
        ),
        event(json!({"type":"content_block_stop","index":2})),
        event(
            json!({"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"tool_exact","name":"read_file","input":{}}}),
        ),
        event(
            json!({"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
        ),
        event(
            json!({"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"\"ñ.rs\"}"}}),
        ),
        event(json!({"type":"content_block_stop","index":3})),
    ];
    frames.extend(finish("tool_use"));
    let bytes: Vec<_> = frames.into_iter().flatten().collect();
    let split = bytes
        .windows(4)
        .position(|value| value == "🦀".as_bytes())
        .unwrap()
        + 1;
    let server = Server::new(vec![Reply::sse(vec![
        bytes[..split].to_vec(),
        bytes[split..].to_vec(),
    ])]);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let result = backend(&server.endpoint)
        .stream_complete_detailed(
            &[AgentMessage::user("inspect")],
            &[tool()],
            CancellationToken::new(),
            sender,
        )
        .await
        .unwrap();
    assert_eq!(receiver.try_recv().unwrap(), "Visible ñ 🦀");
    assert!(receiver.try_recv().is_err());
    assert_eq!(result.output.content, "Visible ñ 🦀");
    assert_eq!(
        result.output.tool_calls[0].arguments,
        json!({"path":"ñ.rs"})
    );
    assert_eq!(
        result.provider_output_items[0]["content"][0]["signature"],
        "exact_signature"
    );
    assert_eq!(
        result.provider_output_items[0]["content"][1]["data"],
        "opaque"
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn real_transport_retries_429_5xx_and_empty_stream_without_publishing_empty_deltas() {
    let mut rate = Reply::json(json!({"error":"rate limit"}));
    rate.status = 429;
    let mut unavailable = Reply::json(json!({"error":"unavailable"}));
    unavailable.status = 503;
    let server = Server::new(vec![
        rate,
        unavailable,
        Reply::json(message(
            vec![json!({"type":"text","text":"retry success"})],
            "end_turn",
        )),
    ]);
    assert_eq!(
        backend(&server.endpoint)
            .complete(&[AgentMessage::user("test")], &[], CancellationToken::new())
            .await
            .unwrap()
            .content,
        "retry success"
    );
    assert_eq!(server.requests().len(), 3);
    let server = Server::new(vec![
        Reply::sse(text_stream("", false)),
        Reply::sse(text_stream("visible", true)),
    ]);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    assert_eq!(
        backend(&server.endpoint)
            .stream_complete(
                &[AgentMessage::user("test")],
                &[],
                CancellationToken::new(),
                sender
            )
            .await
            .unwrap()
            .content,
        "visible"
    );
    assert_eq!(server.requests().len(), 2);
    assert_eq!(receiver.try_recv().unwrap(), "visible");
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn real_sse_error_and_eof_after_observable_content_preserve_prefix_and_never_retry() {
    let mut frames = text_stream("prefix ñ 🦀", false);
    frames.push(event(
        json!({"type":"error","error":{"type":"overloaded_error","message":"not a completion"}}),
    ));
    frames.extend(finish("end_turn"));
    let server = Server::new(vec![Reply::sse(frames)]);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    assert!(backend(&server.endpoint)
        .stream_complete_detailed(
            &[AgentMessage::user("test")],
            &[tool()],
            CancellationToken::new(),
            sender
        )
        .await
        .is_err());
    assert_eq!(receiver.try_recv().unwrap(), "prefix ñ 🦀");
    assert!(receiver.try_recv().is_err());
    assert_eq!(server.requests().len(), 1);
    let server = Server::new(vec![Reply::sse(vec![
        event(start()),
        event(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        ),
        event(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"opaque prefix"}}),
        ),
    ])]);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    assert!(backend(&server.endpoint)
        .stream_complete(
            &[AgentMessage::user("test")],
            &[],
            CancellationToken::new(),
            sender
        )
        .await
        .is_err());
    assert!(receiver.try_recv().is_err());
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn real_sse_cancellation_after_first_delta_keeps_prefix_and_returns_no_completion() {
    let gate = Arc::new(AtomicBool::new(false));
    let first: Vec<_> = text_stream("visible prefix", false)
        .into_iter()
        .flatten()
        .collect();
    let reply = Reply {
        status: 200,
        content_type: "text/event-stream",
        chunks: vec![first, finish("end_turn").into_iter().flatten().collect()],
        gate: Some(gate),
        location: None,
    };
    let server = Server::new(vec![reply]);
    let adapter = backend(&server.endpoint);
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let worker = tokio::spawn(async move {
        adapter
            .stream_complete_detailed(&[AgentMessage::user("test")], &[], cancel, sender)
            .await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap(),
        "visible prefix"
    );
    stop.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap(),
        Err(AgentError::Cancelled)
    ));
    assert_eq!(server.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn real_cross_origin_redirect_never_forwards_custom_api_key_or_retries() {
    let destination = Server::new(vec![Reply::json(message(
        vec![json!({"type":"text","text":"must not be reached"})],
        "end_turn",
    ))]);
    let mut redirect = Reply::json(json!({"detail":"redirect forbidden"}));
    redirect.status = 307;
    redirect.location = Some(format!("{}/messages", destination.endpoint));
    let source = Server::new(vec![redirect]);
    assert!(backend(&source.endpoint)
        .complete(&[AgentMessage::user("test")], &[], CancellationToken::new())
        .await
        .is_err());
    assert_eq!(source.requests().len(), 1);
    assert!(destination.requests().is_empty());
}

#[tokio::test]
async fn signed_history_cannot_cross_endpoints_with_the_same_model_id() {
    let first = Server::new(vec![Reply::json(message(
        vec![
            json!({"type":"thinking","thinking":"opaque","signature":"endpoint_owned_signature"}),
            json!({"type":"text","text":"first completion"}),
        ],
        "end_turn",
    ))]);
    let second = Server::new(vec![Reply::json(message(
        vec![json!({"type":"text","text":"must not be requested"})],
        "end_turn",
    ))]);
    let completion = backend(&first.endpoint)
        .complete_detailed(
            &[AgentMessage::user("first")],
            &[],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        completion.provider_output_items[0]["endpoint_digest"],
        openide_core::native_model_endpoint_digest(&format!("{}/messages", first.endpoint))
            .unwrap()
    );
    let mut assistant =
        AgentMessage::assistant(completion.output.content, completion.output.tool_calls);
    assistant.provider_output_items = completion.provider_output_items;
    let history = [
        AgentMessage::user("first"),
        assistant,
        AgentMessage::user("next"),
    ];
    // Changing only output policy on the original endpoint keeps signed history valid.
    assert!(backend(&first.endpoint)
        .with_max_output_tokens(8192)
        .unwrap()
        .request_body(&history, &[], false, false)
        .is_ok());
    assert!(matches!(
        backend(&second.endpoint)
            .complete_detailed(&history, &[], CancellationToken::new())
            .await,
        Err(AgentError::InvalidRequest(_))
    ));
    assert_eq!(first.requests().len(), 1);
    assert!(second.requests().is_empty());
    let mut missing_identity = history.to_vec();
    missing_identity[1].provider_output_items[0]
        .as_object_mut()
        .unwrap()
        .remove("endpoint_digest");
    assert!(backend(&first.endpoint)
        .request_body(&missing_identity, &[], false, false)
        .is_err());
}

#[tokio::test]
async fn provider_failures_are_classified_and_only_overload_is_repeated() {
    use crate::{AgentError, FailureClass};
    let error = |status: u16, kind: &str, text: &str| {
        let mut reply = Reply::json(json!({"type":"error","error":{"type":kind,"message":text}}));
        reply.status = status;
        reply
    };
    // 529 over HTTP, then `overloaded_error` as the stream's first event.
    let server = Server::new(vec![
        error(529, "overloaded_error", "Overloaded"),
        Reply::sse(vec![event(json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}))]),
        Reply::sse(text_stream("visible", true)),
    ]);
    let (sender, _receiver) = mpsc::unbounded_channel();
    assert_eq!(
        backend(&server.endpoint)
            .stream_complete(&[AgentMessage::user("test")], &[], CancellationToken::new(), sender)
            .await
            .unwrap()
            .content,
        "visible"
    );
    assert_eq!(server.requests().len(), 3);
    for (status, kind, text, class) in [
        (400, "invalid_request_error", "Your credit balance is too low to access the Anthropic API.", FailureClass::OutOfCredits),
        (402, "billing_error", "billing", FailureClass::OutOfCredits),
        (401, "authentication_error", "invalid x-api-key", FailureClass::Auth),
        (400, "invalid_request_error", "prompt is too long: 208310 tokens > 200000 maximum", FailureClass::ContextTooLarge),
        (413, "request_too_large", "too large", FailureClass::ContextTooLarge),
        (400, "invalid_request_error", "tools.0: bad schema", FailureClass::Fatal),
    ] {
        let server = Server::new(vec![error(status, kind, text)]);
        let failed = backend(&server.endpoint)
            .complete(&[AgentMessage::user("test")], &[], CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(&failed, AgentError::Provider(failure) if failure.class == class), "{status} {kind}: {failed:?}");
        assert_eq!(server.requests().len(), 1);
    }
    // An overload after visible text is kept as a transient failure, not sent again.
    let mut frames = text_stream("partial", false);
    frames.push(event(json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}})));
    let server = Server::new(vec![Reply::sse(frames)]);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let failed = backend(&server.endpoint)
        .stream_complete(&[AgentMessage::user("test")], &[], CancellationToken::new(), sender)
        .await
        .unwrap_err();
    assert!(matches!(failed, AgentError::Provider(failure) if failure.class == FailureClass::Transient));
    assert_eq!(receiver.try_recv().unwrap(), "partial");
    assert_eq!(server.requests().len(), 1);
}
