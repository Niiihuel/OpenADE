use crate::{AgentError, ToolDefinition};
use async_trait::async_trait;
use futures_util::StreamExt;
use openide_core::{AgentMessage, MessageRole, ToolCall};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

const MAX_STREAM_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const MAX_ASSEMBLED_STREAM_BYTES: usize = 8 * 1024 * 1024;
const NATIVE_STANDARD_EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
const NATIVE_GPT55_EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh"];
const NATIVE_GPT6_TOOL_EFFORTS: &[&str] = &["none"];

/// Whether this Chat Completions backend can send its function tools to the
/// model. Catalog metadata alone cannot describe this transport limitation.
pub fn native_agent_supports_tool_calls(model: &str) -> bool {
    !model.starts_with("gpt-6-astra")
}

/// Efforts safe for this native agent's Chat Completions requests, which
/// include function tools. An empty/unknown result means the model's own
/// default remains usable, but the UI must not claim an adjustable effort.
pub fn native_agent_supported_efforts(model: &str) -> Option<&'static [&'static str]> {
    if model.starts_with("gpt-5.6") && !model.contains("-codex") {
        Some(NATIVE_STANDARD_EFFORTS)
    } else if model.starts_with("gpt-5.5") && !model.contains("-codex") {
        Some(NATIVE_GPT55_EFFORTS)
    } else if model.starts_with("gpt-6-sol") || model.starts_with("gpt-6-luna") {
        Some(NATIVE_GPT6_TOOL_EFFORTS)
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelOutput {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

#[async_trait]
pub trait ModelBackend: Send + Sync {
    fn model_id(&self) -> &str;

    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError>;

    async fn stream_complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        _deltas: UnboundedSender<String>,
    ) -> Result<ModelOutput, AgentError> {
        self.complete(messages, tools, cancel).await
    }
}

/// Adapter for the common `/chat/completions` wire format.
pub struct OpenAiCompatibleBackend {
    endpoint: String,
    api_key: Option<String>,
    model: String,
    reasoning_effort: Option<String>,
    deepseek_non_thinking: bool,
    client: Client,
}

impl OpenAiCompatibleBackend {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        let base = endpoint.into();
        let endpoint = if base.trim_end_matches('/').ends_with("/chat/completions") {
            base.trim_end_matches('/').to_owned()
        } else {
            format!("{}/chat/completions", base.trim_end_matches('/'))
        };
        let model = model.into();
        let required_none = model.starts_with("gpt-6-sol") || model.starts_with("gpt-6-luna");
        Self {
            endpoint,
            api_key,
            model,
            // Chat Completions requires an explicit `none` for these models
            // when tools are present. Set it here for interactive and scheduled
            // runs alike, including when the composer hides a one-choice menu.
            reasoning_effort: if required_none {
                Some("none".to_owned())
            } else {
                None
            },
            deepseek_non_thinking: false,
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client configuration is valid"),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client configuration is valid");
        self
    }

    /// DeepSeek thinking mode requires replaying `reasoning_content` after
    /// tool calls. Until this backend persists that field, request its
    /// documented non-thinking mode explicitly for the DeepSeek adapter.
    pub fn with_deepseek_non_thinking(mut self) -> Self {
        self.deepseek_non_thinking = true;
        self
    }

    /// Opt in to the OpenAI Chat Completions `reasoning_effort` parameter.
    /// Unknown OpenAI-compatible models and ordinary non-reasoning models are
    /// rejected instead of silently displaying a setting that has no effect.
    /// Model-specific server support remains the provider's responsibility.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Result<Self, AgentError> {
        let effort = effort.into();
        validate_reasoning_effort(&self.model, &effort)?;
        self.reasoning_effort = Some(effort);
        Ok(self)
    }

    fn request_body(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        stream: bool,
    ) -> Result<Value, AgentError> {
        if !native_agent_supports_tool_calls(&self.model) && !tools.is_empty() {
            return Err(AgentError::InvalidRequest(
                "GPT-6 Astra function calling requires the Responses API; this native backend uses Chat Completions".into(),
            ));
        }
        if (self.model.starts_with("gpt-6-sol") || self.model.starts_with("gpt-6-luna"))
            && !tools.is_empty()
            && self.reasoning_effort.as_deref() != Some("none")
        {
            return Err(AgentError::InvalidRequest(
                "GPT-6 Sol and Luna function calling in Chat Completions requires reasoning effort 'none'".into(),
            ));
        }
        let wire_messages: Vec<Value> = messages.iter().map(message_to_wire).collect();
        let wire_tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect();
        let mut body = json!({
            "model": self.model,
            "messages": wire_messages,
            "stream": stream,
        });
        if !wire_tools.is_empty() {
            body["tools"] = Value::Array(wire_tools);
        }
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = Value::String(effort.clone());
        }
        if self.deepseek_non_thinking {
            body["thinking"] = json!({"type": "disabled"});
        }
        Ok(body)
    }

    fn request(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        stream: bool,
    ) -> Result<reqwest::RequestBuilder, AgentError> {
        let body = self.request_body(messages, tools, stream)?;
        let mut request = self.client.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        Ok(request)
    }
}

fn validate_reasoning_effort(model: &str, effort: &str) -> Result<(), AgentError> {
    let Some(levels) = native_agent_supported_efforts(model) else {
        return Err(AgentError::InvalidRequest(format!(
            "Model '{model}' has no verified reasoning effort with Chat Completions function tools; leave effort unset or use Codex CLI"
        )));
    };
    if !levels.contains(&effort) {
        return Err(AgentError::InvalidRequest(format!(
            "Model '{model}' does not support reasoning effort '{effort}' with Chat Completions function tools"
        )));
    }
    Ok(())
}

#[async_trait]
impl ModelBackend for OpenAiCompatibleBackend {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        let request = self.request(messages, tools, false)?;
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            response = request.send() => response.map_err(|error| AgentError::Model(error.to_string()))?,
        };
        let status = response.status();
        let bytes = tokio::select! {
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            bytes = response.bytes() => bytes.map_err(|error| AgentError::Model(error.to_string()))?,
        };
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            let detail: String = body.chars().take(500).collect();
            return Err(AgentError::Model(format!("HTTP {status}: {detail}")));
        }
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|error| AgentError::Model(format!("invalid JSON response: {error}")))?;
        parse_model_output(&body)
    }

    async fn stream_complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelOutput, AgentError> {
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            response = self.request(messages, tools, true)?.send() => {
                response.map_err(|error| AgentError::Model(error.to_string()))?
            }
        };
        let status = response.status();
        if !status.is_success() {
            let body = tokio::select! {
                _ = cancel.cancelled() => return Err(AgentError::Cancelled),
                body = response.text() => body.map_err(|error| AgentError::Model(error.to_string()))?,
            };
            let detail: String = body.chars().take(500).collect();
            return Err(AgentError::Model(format!("HTTP {status}: {detail}")));
        }
        let mut stream = Box::pin(response.bytes_stream());
        let mut pending = Vec::<u8>::new();
        let mut assembler = SseAssembler::default();
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err(AgentError::Cancelled),
                next = stream.next() => next,
            };
            let Some(next) = next else { break };
            let chunk = next.map_err(|error| AgentError::Model(error.to_string()))?;
            pending.extend_from_slice(&chunk);
            if pending.len() > MAX_STREAM_BUFFER_BYTES {
                return Err(AgentError::Limit("streaming response is too large".into()));
            }
            while let Some(frame) = take_sse_frame(&mut pending) {
                assembler.ingest(&frame, &deltas)?;
                if assembler.done {
                    return assembler.finish();
                }
            }
        }
        if assembler.seen_frame {
            if !pending.is_empty() {
                assembler.ingest(&pending, &deltas)?;
            }
            assembler.finish()
        } else {
            // Some compatible providers ignore `stream: true` and return ordinary JSON.
            let body: Value = serde_json::from_slice(&pending).map_err(|error| {
                AgentError::Model(format!("invalid streaming response: {error}"))
            })?;
            parse_model_output(&body)
        }
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct SseAssembler {
    content: String,
    calls: BTreeMap<usize, PartialToolCall>,
    assembled_bytes: usize,
    done: bool,
    seen_frame: bool,
}

impl SseAssembler {
    fn ingest(&mut self, frame: &[u8], deltas: &UnboundedSender<String>) -> Result<(), AgentError> {
        let frame = std::str::from_utf8(frame)
            .map_err(|error| AgentError::Model(format!("invalid SSE UTF-8: {error}")))?;
        let data = frame
            .lines()
            .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return Ok(());
        }
        self.seen_frame = true;
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let payload: Value = serde_json::from_str(&data)
            .map_err(|error| AgentError::Model(format!("invalid SSE payload: {error}")))?;
        let Some(delta) = payload.pointer("/choices/0/delta") else {
            return Ok(());
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            append_assembled_fragment(&mut self.content, text, &mut self.assembled_bytes)?;
            let _ = deltas.send(text.to_owned());
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index =
                    call.get("index").and_then(Value::as_u64).ok_or_else(|| {
                        AgentError::Model("streaming tool call has no index".into())
                    })? as usize;
                let partial = self.calls.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    if partial.id.is_empty() {
                        append_assembled_fragment(&mut partial.id, id, &mut self.assembled_bytes)?;
                    }
                }
                if let Some(function) = call.get("function") {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        append_assembled_fragment(
                            &mut partial.name,
                            name,
                            &mut self.assembled_bytes,
                        )?;
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        append_assembled_fragment(
                            &mut partial.arguments,
                            arguments,
                            &mut self.assembled_bytes,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<ModelOutput, AgentError> {
        let mut tool_calls = Vec::new();
        for (_, partial) in self.calls {
            if partial.id.is_empty() || partial.name.is_empty() {
                return Err(AgentError::Model("incomplete streaming tool call".into()));
            }
            let arguments = if partial.arguments.is_empty() {
                json!({})
            } else {
                serde_json::from_str(&partial.arguments).map_err(|error| {
                    AgentError::Model(format!("invalid streaming tool arguments: {error}"))
                })?
            };
            tool_calls.push(ToolCall {
                id: partial.id,
                name: partial.name,
                arguments,
            });
        }
        Ok(ModelOutput {
            content: self.content,
            tool_calls,
        })
    }
}

fn append_assembled_fragment(
    target: &mut String,
    fragment: &str,
    assembled_bytes: &mut usize,
) -> Result<(), AgentError> {
    if fragment.len() > MAX_ASSEMBLED_STREAM_BYTES.saturating_sub(*assembled_bytes) {
        return Err(AgentError::Limit(format!(
            "assembled streaming output exceeds the {MAX_ASSEMBLED_STREAM_BYTES} byte limit"
        )));
    }
    target.push_str(fragment);
    *assembled_bytes += fragment.len();
    Ok(())
}

fn take_sse_frame(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let lf = pending.windows(2).position(|window| window == b"\n\n");
    let crlf = pending.windows(4).position(|window| window == b"\r\n\r\n");
    let (position, delimiter_len) = match (lf, crlf) {
        (Some(left), Some(right)) if left < right => (left, 2),
        (Some(_), Some(right)) => (right, 4),
        (Some(left), None) => (left, 2),
        (None, Some(right)) => (right, 4),
        (None, None) => return None,
    };
    let frame = pending[..position].to_vec();
    pending.drain(..position + delimiter_len);
    Some(frame)
}

fn message_to_wire(message: &AgentMessage) -> Value {
    let role = match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };
    let mut wire = json!({ "role": role, "content": message.content });
    if !message.tool_calls.is_empty() {
        wire["tool_calls"] = Value::Array(
            message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        }
                    })
                })
                .collect(),
        );
    }
    if let Some(call_id) = &message.tool_call_id {
        wire["tool_call_id"] = json!(call_id);
    }
    wire
}

fn parse_model_output(body: &Value) -> Result<ModelOutput, AgentError> {
    let message = body
        .pointer("/choices/0/message")
        .ok_or_else(|| AgentError::Model("response has no assistant message".into()))?;
    let content = match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        Some(Value::Null) | None => String::new(),
        _ => return Err(AgentError::Model("unsupported assistant content".into())),
    };
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").filter(|calls| !calls.is_null()) {
        let calls = calls
            .as_array()
            .ok_or_else(|| AgentError::Model("tool_calls is not an array".into()))?;
        for call in calls {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| AgentError::Model("tool call has no id".into()))?;
            let function = call
                .get("function")
                .ok_or_else(|| AgentError::Model("tool call has no function".into()))?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| AgentError::Model("tool call has no name".into()))?;
            let arguments = match function.get("arguments") {
                Some(Value::String(text)) => serde_json::from_str(text).map_err(|error| {
                    AgentError::Model(format!("invalid tool arguments: {error}"))
                })?,
                Some(value) => value.clone(),
                None => Value::Object(Default::default()),
            };
            tool_calls.push(ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                arguments,
            });
        }
    }
    Ok(ModelOutput {
        content,
        tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    fn sample_tool() -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: json!({"type":"object"}),
        }
    }

    #[test]
    fn reasoning_effort_is_explicit_in_both_streaming_and_regular_requests() {
        let backend = OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-5.6-sol")
            .with_reasoning_effort("high")
            .unwrap();
        let messages = [AgentMessage::user("hello")];
        for streaming in [false, true] {
            let body = backend
                .request_body(&messages, &[sample_tool()], streaming)
                .unwrap();
            assert_eq!(body["model"], "gpt-5.6-sol");
            assert_eq!(body["reasoning_effort"], "high");
            assert_eq!(body["stream"], streaming);
            assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        }
    }

    #[test]
    fn ordinary_and_unknown_models_do_not_accept_decorative_effort() {
        assert!(
            OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-4.1")
                .with_reasoning_effort("high")
                .is_err()
        );
        assert!(
            OpenAiCompatibleBackend::new("http://localhost/v1", None, "custom-local")
                .with_reasoning_effort("high")
                .is_err()
        );
        assert!(
            OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-5.6-sol")
                .with_reasoning_effort("ultra")
                .is_err()
        );
    }

    #[test]
    fn deepseek_adapter_explicitly_disables_thinking_when_tools_are_replayed() {
        let backend =
            OpenAiCompatibleBackend::new("https://api.deepseek.com", None, "deepseek-flash")
                .with_deepseek_non_thinking();
        let body = backend
            .request_body(&[AgentMessage::user("hello")], &[sample_tool()], true)
            .unwrap();
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn chat_completion_tool_rules_are_checked_before_http() {
        let astra = OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-6-astra");
        assert!(astra.request_body(&[], &[sample_tool()], false).is_err());
        assert!(
            OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-6-astra")
                .with_reasoning_effort("medium")
                .is_err()
        );
        assert!(
            OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-6-astra")
                .with_reasoning_effort("none")
                .is_err()
        );

        let sol = OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-6-sol");
        assert_eq!(
            sol.request_body(&[], &[sample_tool()], false).unwrap()["reasoning_effort"],
            "none"
        );
        let luna = OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-6-luna");
        assert_eq!(
            luna.request_body(&[], &[sample_tool()], false).unwrap()["reasoning_effort"],
            "none"
        );
        assert!(
            OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-6-sol")
                .with_reasoning_effort("medium")
                .is_err()
        );
        let sol = OpenAiCompatibleBackend::new("http://localhost/v1", None, "gpt-6-sol")
            .with_reasoning_effort("none")
            .unwrap();
        assert_eq!(
            sol.request_body(&[], &[sample_tool()], false).unwrap()["reasoning_effort"],
            "none"
        );
        assert_eq!(
            native_agent_supported_efforts("gpt-6-sol"),
            Some(&["none"][..])
        );
        assert!(native_agent_supported_efforts("gpt-4.1").is_none());
        assert!(native_agent_supported_efforts("gpt-5.6-codex").is_none());
    }

    #[test]
    fn parses_tool_calls_and_null_content() {
        let output = parse_model_output(&json!({
            "choices": [{"message": {
                "content": null,
                "tool_calls": [{"id":"call-1","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]
            }}]
        }))
        .unwrap();
        assert!(output.content.is_empty());
        assert_eq!(output.tool_calls[0].name, "read_file");
        assert_eq!(output.tool_calls[0].arguments["path"], "README.md");
    }

    #[test]
    fn serializes_assistant_tool_roundtrip() {
        let message = AgentMessage::assistant(
            "",
            vec![ToolCall {
                id: "call-1".into(),
                name: "list_files".into(),
                arguments: json!({"path":"."}),
            }],
        );
        let wire = message_to_wire(&message);
        assert_eq!(
            wire["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\".\"}"
        );
    }

    #[test]
    fn assembles_split_sse_text_and_tool_arguments() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut assembler = SseAssembler::default();
        let first = json!({"choices":[{"delta":{"content":"Hi "}}]});
        let second = json!({"choices":[{"delta":{
            "content":"there",
            "tool_calls":[{"index":0,"id":"call-1","function":{
                "name":"read_file","arguments":"{\"path\":"
            }}]
        }}]});
        let third = json!({"choices":[{"delta":{
            "tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]
        }}]});
        let mut pending = format!("data: {first}\r\n\r\ndata: {second}\n\n").into_bytes();
        while let Some(frame) = take_sse_frame(&mut pending) {
            assembler.ingest(&frame, &sender).unwrap();
        }
        pending.extend_from_slice(format!("data: {third}\n\n").as_bytes());
        while let Some(frame) = take_sse_frame(&mut pending) {
            assembler.ingest(&frame, &sender).unwrap();
        }
        let result = assembler.finish().unwrap();
        assert_eq!(result.content, "Hi there");
        assert_eq!(result.tool_calls[0].arguments["path"], "a.txt");
        assert_eq!(receiver.try_recv().unwrap(), "Hi ");
        assert_eq!(receiver.try_recv().unwrap(), "there");
    }

    #[test]
    fn limits_text_assembled_from_many_small_sse_frames_before_appending() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        drop(receiver);
        let mut assembler = SseAssembler::default();
        let fragment = "x".repeat(1024);
        let frame = format!(
            "data: {}",
            json!({"choices":[{"delta":{"content":fragment.as_str()}}]})
        );
        for _ in 0..MAX_ASSEMBLED_STREAM_BYTES / fragment.len() {
            assembler.ingest(frame.as_bytes(), &sender).unwrap();
        }
        assert_eq!(assembler.content.len(), MAX_ASSEMBLED_STREAM_BYTES);
        let error = assembler.ingest(frame.as_bytes(), &sender).unwrap_err();
        assert!(
            matches!(error, AgentError::Limit(message) if message.contains("assembled streaming output"))
        );
        assert_eq!(assembler.content.len(), MAX_ASSEMBLED_STREAM_BYTES);
    }

    #[test]
    fn limits_tool_arguments_assembled_from_many_small_sse_frames_before_appending() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        drop(receiver);
        let mut assembler = SseAssembler::default();
        let start = json!({"choices":[{"delta":{"tool_calls":[{
            "index":0,"id":"call-1","function":{"name":"read_file"}
        }]}}]});
        assembler
            .ingest(format!("data: {start}").as_bytes(), &sender)
            .unwrap();
        let fragment = "x".repeat(1024);
        let frame = format!(
            "data: {}",
            json!({"choices":[{"delta":{"tool_calls":[{
                "index":0,"function":{"arguments":fragment.as_str()}
            }]}}]})
        );
        let accepted = (MAX_ASSEMBLED_STREAM_BYTES - assembler.assembled_bytes) / fragment.len();
        for _ in 0..accepted {
            assembler.ingest(frame.as_bytes(), &sender).unwrap();
        }
        let before = assembler.calls.get(&0).unwrap().arguments.len();
        let error = assembler.ingest(frame.as_bytes(), &sender).unwrap_err();
        assert!(
            matches!(error, AgentError::Limit(message) if message.contains("assembled streaming output"))
        );
        assert_eq!(assembler.calls.get(&0).unwrap().arguments.len(), before);
    }

    #[tokio::test]
    async fn streams_http_deltas_from_compatible_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            assert!(request_line.starts_with("POST /v1/chat/completions "));
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(length) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = length.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap()["stream"],
                true
            );
            let event_one = json!({"choices":[{"delta":{"content":"hello "}}]});
            let event_two = json!({"choices":[{"delta":{"content":"world"}}]});
            let sse = format!("data: {event_one}\n\ndata: {event_two}\n\ndata: [DONE]\n\n");
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                sse.len(),
                sse
            )
            .unwrap();
            socket.flush().unwrap();
        });
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let output = backend
            .stream_complete(
                &[AgentMessage::user("hi")],
                &[],
                CancellationToken::new(),
                sender,
            )
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(output.content, "hello world");
        assert_eq!(receiver.try_recv().unwrap(), "hello ");
        assert_eq!(receiver.try_recv().unwrap(), "world");
    }
}
