//! Continuation matrix: a conversation written on one wire API is sent to
//! each of the others, through the adapter and the real converter, to a
//! loopback server that enforces the target's structural rules.

use super::*;
use crate::{AnthropicMessagesBackend, ModelBackend, OpenAiCompatibleBackend, ResponsesBackend};
use openide_core::{reconcile_model_history_route, ModelRouteSnapshot, ToolCall};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Api {
    Anthropic,
    Responses,
    Chat,
}

fn token(value: &str, max_len: usize) -> bool {
    ToolIdRule::Token { max_len }.accepts(value)
}

/// The structural rules of each provider, as its server enforces them.
fn violation(api: Api, body: &Value) -> Option<String> {
    let text = body.to_string();
    for foreign in ["encrypted_content", "signature", "web_search_call", "reasoning_content"] {
        if text.contains(foreign) {
            return Some(format!("foreign provider item: {foreign}"));
        }
    }
    let tools = body["tools"].as_array().cloned().unwrap_or_default();
    for tool in &tools {
        let name = tool["name"].as_str().or(tool["function"]["name"].as_str()).unwrap_or("");
        if !token(name, 64) {
            return Some(format!("tool name: {name}"));
        }
    }
    let mut open = Vec::<String>::new();
    let close = |open: &mut Vec<String>, id: &str| match open.iter().position(|o| o == id) {
        Some(index) => {
            open.remove(index);
            None
        }
        None => Some(format!("result without a call: {id}")),
    };
    match api {
        Api::Anthropic => {
            let messages = body["messages"].as_array()?;
            if messages.first().map(|m| &m["role"]) != Some(&json!("user")) {
                return Some("first message is not from the user".into());
            }
            for pair in messages.windows(2) {
                if pair[0]["role"] == pair[1]["role"] {
                    return Some("consecutive messages share a role".into());
                }
            }
            for message in messages {
                let blocks = message["content"].as_array()?;
                if message["role"] == "user" && !open.is_empty() {
                    let answered = blocks.iter().filter(|b| b["type"] == "tool_result").count();
                    if answered != open.len() {
                        return Some("tool_use without tool_result in the next message".into());
                    }
                }
                for block in blocks {
                    match block["type"].as_str()? {
                        "text" => {}
                        "image" if body["x_vision"] != false => {}
                        "tool_use" => {
                            let id = block["id"].as_str()?;
                            if !token(id, 64) || !token(block["name"].as_str()?, 64) {
                                return Some(format!("tool_use: {block}"));
                            }
                            open.push(id.into());
                        }
                        "tool_result" => {
                            if let Some(error) = close(&mut open, block["tool_use_id"].as_str()?) {
                                return Some(error);
                            }
                        }
                        other => return Some(format!("block type: {other}")),
                    }
                }
            }
        }
        Api::Chat => {
            for message in body["messages"].as_array()? {
                if !message["content"].is_string() && !message["content"].is_null() {
                    return Some("content is not text".into());
                }
                if message["role"] != "tool" && !open.is_empty() {
                    return Some("tool call without its result".into());
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    let id = call["id"].as_str()?;
                    if !token(id, 40) || !token(call["function"]["name"].as_str()?, 64) {
                        return Some(format!("tool call: {call}"));
                    }
                    open.push(id.into());
                }
                if message["role"] == "tool" {
                    if let Some(error) = close(&mut open, message["tool_call_id"].as_str()?) {
                        return Some(error);
                    }
                }
            }
        }
        Api::Responses => {
            for item in body["input"].as_array()? {
                match item["type"].as_str() {
                    Some("function_call") => {
                        let id = item["call_id"].as_str()?;
                        if !token(id, 64) || !token(item["name"].as_str()?, 64) {
                            return Some(format!("function_call: {item}"));
                        }
                        open.push(id.into());
                    }
                    Some("function_call_output") => {
                        if let Some(error) = close(&mut open, item["call_id"].as_str()?) {
                            return Some(error);
                        }
                    }
                    None if item["role"].is_string() => {}
                    other => return Some(format!("input item: {other:?}")),
                }
            }
        }
    }
    (!open.is_empty()).then(|| format!("calls without a result: {open:?}"))
}

fn reply(api: Api) -> Value {
    match api {
        Api::Anthropic => json!({"type":"message","id":"msg_1","role":"assistant","model":"target",
            "content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","stop_sequence":null,
            "usage":{"input_tokens":1,"output_tokens":1}}),
        Api::Responses => json!({"id":"resp_1","status":"completed","output":[
            {"type":"message","id":"msg_1","status":"completed","role":"assistant",
             "content":[{"type":"output_text","text":"ok","annotations":[]}]}]}),
        Api::Chat => json!({"id":"c1","choices":[{"finish_reason":"stop",
            "message":{"role":"assistant","content":"ok"}}]}),
    }
}

/// Answers one request: 200 when the body obeys `api`, 400 naming the rule.
async fn validating_server(api: Api) -> (String, tokio::task::JoinHandle<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 8192];
        let body = loop {
            let count = socket.read(&mut chunk).await.unwrap();
            assert!(count > 0, "request ended early");
            request.extend_from_slice(&chunk[..count]);
            let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map_or(0, |value| value.trim().parse::<usize>().unwrap());
            if request.len() >= end + 4 + length {
                break serde_json::from_slice::<Value>(&request[end + 4..end + 4 + length]).unwrap();
            }
        };
        let (status, payload) = match violation(api, &body) {
            None => ("200 OK", reply(api).to_string()),
            Some(rule) => (
                "400 Bad Request",
                json!({"error":{"type":"invalid_request_error","message":rule}}).to_string(),
            ),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();
        body
    });
    (address, server)
}

fn route(api: Api) -> ModelRouteSnapshot {
    ModelRouteSnapshot::new(&format!("{api:?}"), "m", "https://example.test/v1", [api as u8; 32]).unwrap()
}

/// What the journal of a turn on `source` replays: its own identifiers, its
/// own reasoning and hosted-search items, an image, an old tool whose name no
/// catalog would accept today, and a call that never got its result.
fn history(source: Api) -> Vec<AgentMessage> {
    let (first, second, items) = match source {
        Api::Anthropic => (
            format!("toolu_{}", "A".repeat(70)),
            "toolu_01:x".to_owned(),
            vec![json!({"type":"openide_anthropic_message_v1","content":[
                {"type":"thinking","thinking":"t","signature":"sig"}]})],
        ),
        Api::Responses => (
            "call_abc|fc_0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
            format!("call_{}", "9".repeat(80)),
            vec![
                json!({"type":"reasoning","id":"rs_1","encrypted_content":"opaque","summary":[]}),
                json!({"type":"web_search_call","id":"ws_1","status":"completed"}),
            ],
        ),
        Api::Chat => (
            "chatcmpl-tool:0.1".to_owned(),
            "functions.read_file:7/α".to_owned(),
            vec![json!({"type":"openide_chat_reasoning_v1","reasoning_content":"hidden"})],
        ),
    };
    let call = |id: &str, name: &str| ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: json!({"path": "a.txt"}),
    };
    let png = [b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".as_slice(), &[0, 0, 0, 8, 0, 0, 0, 6]].concat();
    let image = UserImage::from_bytes(UserImageMime::Png, &png).unwrap();
    let mut reasoned = AgentMessage::assistant(
        "leo",
        vec![
            call(&first, "mcp.server/tool con espacios"),
            call(&second, &format!("mcp__server__{}", "x".repeat(90))),
        ],
    );
    reasoned.provider_output_items = items.clone();
    let mut searched = AgentMessage::assistant("", vec![]);
    searched.provider_output_items = items;
    vec![
        AgentMessage::system("system"),
        AgentMessage::user_with_images("mira esto", vec![image]).unwrap(),
        reasoned,
        AgentMessage::tool(first, "uno"),
        AgentMessage::tool(second, "dos"),
        searched,
        AgentMessage::assistant("sigo", vec![call("dangling id", "read_file")]),
        AgentMessage::user("continúa"),
    ]
}

fn backend(api: Api, address: &str, vision: bool) -> Box<dyn ModelBackend> {
    let endpoint = format!("http://{address}/v1");
    match api {
        Api::Anthropic => Box::new(
            AnthropicMessagesBackend::new(endpoint, Some("k".into()), "target")
                .with_image_input_enabled(vision),
        ),
        Api::Responses => Box::new(
            ResponsesBackend::new(endpoint, Some("k".into()), "target")
                .with_image_input_enabled(vision),
        ),
        Api::Chat => Box::new(
            OpenAiCompatibleBackend::new(endpoint, Some("k".into()), "target")
                .with_capabilities(crate::ModelRequestCapabilities {
                    tool_calls: true,
                    ..Default::default()
                })
                .unwrap(),
        ),
    }
}

fn offered() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "read_file".into(),
        description: "read".into(),
        parameters: json!({"type":"object","properties":{}}),
    }]
}

async fn send(source: Api, target: Api, vision: bool, adapt: bool) -> Result<(), String> {
    let journal = history(source);
    let mut request = journal.clone();
    // What every request-assembly site does first on a route change.
    reconcile_model_history_route(&mut request, Some(&route(source)), &route(target));
    let (address, server) = validating_server(target).await;
    let model = backend(target, &address, vision);
    let tools = offered();
    let result = if adapt {
        let adapted = adapt_for(model.as_ref(), &request, &tools).map_err(|e| e.to_string())?;
        model.complete(&adapted.messages, &adapted.tools, CancellationToken::new()).await
    } else {
        model.complete(&request, &tools, CancellationToken::new()).await
    };
    assert_eq!(journal, history(source), "the journal is never rewritten");
    let output = result.map_err(|error| {
        server.abort();
        error.to_string()
    })?;
    let body = server.await.unwrap();
    assert_eq!(output.content, "ok");
    let sent = body.to_string();
    assert!(sent.contains("continúa") && sent.contains("uno") && sent.contains("dos"));
    assert!(sent.contains(INTERRUPTED_TOOL_RESULT), "{target:?} <- {source:?}");
    if !vision || target == Api::Chat {
        assert!(sent.contains("[Imagen adjunta: image/png, 8x6"), "{target:?} <- {source:?}");
    }
    Ok(())
}

#[tokio::test]
async fn every_target_accepts_a_history_written_by_every_other_api() {
    let apis = [Api::Anthropic, Api::Responses, Api::Chat];
    for target in apis {
        for source in apis.into_iter().filter(|source| *source != target) {
            for vision in [true, false] {
                send(source, target, vision, true)
                    .await
                    .unwrap_or_else(|error| panic!("{target:?} <- {source:?} ({vision}): {error}"));
            }
        }
    }
}

#[tokio::test]
async fn the_same_histories_are_refused_without_the_adapter() {
    // The fixture is not lenient: unadapted, each pair fails in the converter
    // or at the server.
    let apis = [Api::Anthropic, Api::Responses, Api::Chat];
    for target in apis {
        for source in apis.into_iter().filter(|source| *source != target) {
            assert!(send(source, target, true, false).await.is_err(), "{target:?} <- {source:?}");
        }
    }
}

#[tokio::test]
async fn a_mistral_target_gets_nine_alphanumerics_for_every_foreign_id() {
    let (address, server) = validating_server(Api::Chat).await;
    let model = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "target")
        .with_capabilities(crate::ModelRequestCapabilities { tool_calls: true, ..Default::default() })
        .unwrap()
        .with_tool_call_id_rule(ToolIdRule::Alnum9);
    let mut request = history(Api::Anthropic);
    reconcile_model_history_route(&mut request, Some(&route(Api::Anthropic)), &route(Api::Chat));
    let adapted = adapt_for(&model, &request, &offered()).unwrap();
    model.complete(&adapted.messages, &adapted.tools, CancellationToken::new()).await.unwrap();
    let body = server.await.unwrap();
    let ids: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["tool_calls"].as_array().into_iter().flatten())
        .map(|call| call["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.iter().all(|id| ToolIdRule::Alnum9.accepts(id)), "{ids:?}");
}
