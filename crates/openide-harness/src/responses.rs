//! OpenAI Responses transport for the native agent.
//!
//! The public `ModelBackend` interface carries text and local function calls.
//! `ResponsesResult` also exposes the response ID, token usage, and original
//! output items to callers that can persist the richer Responses transcript.
//! The native runner commits those provider items before tool effects.

use crate::backend::{
    read_model_http_body, retry_model_transport, send_model_request,
    stable_request_profile_fingerprint, take_sse_frame, ModelAttemptError,
};
use crate::{AgentError, ModelBackend, ModelCompletion, ModelOutput, ToolDefinition};
use async_trait::async_trait;
use futures_util::StreamExt;
use openide_core::{AgentMessage, MessageRole, ToolCall};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

#[path = "hosted_web_search.rs"]
mod hosted_web_search;
pub use hosted_web_search::{
    hosted_web_citations, hosted_web_search_calls, HostedWebCitation, HostedWebSearchAction,
    HostedWebSearchCall, HostedWebSearchConfig, HostedWebSearchContextSize,
    HostedWebSearchLocation, HostedWebSearchMode, HostedWebSource,
};

const MAX_STREAM_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_ASSEMBLED_BYTES: usize = 8 * 1024 * 1024;
const MAX_REPLAY_ITEMS: usize = 24;
const MAX_REPLAY_BYTES: usize = 16 * 1024 * 1024;
const MAX_INPUT_TOKEN_COUNT_BODY_BYTES: usize = 64 * 1024;

pub use crate::backend::ModelTokenUsage as ResponseUsage;

#[derive(Clone, Debug, PartialEq)]
pub struct ResponsesResult {
    pub output: ModelOutput,
    pub response_id: String,
    pub usage: Option<ResponseUsage>,
    /// Original output items, including opaque reasoning items. Store securely
    /// if persisting them for a stateless continuation.
    pub output_items: Vec<Value>,
}

impl ResponsesResult {
    /// Typed hosted search actions from the original provider output items.
    pub fn web_search_calls(&self) -> Result<Vec<HostedWebSearchCall>, AgentError> {
        hosted_web_search_calls(&self.output_items)
    }

    /// URL citation annotations with offsets into each provider output part.
    /// The host UI must render these as visible, clickable links.
    pub fn web_citations(&self) -> Result<Vec<HostedWebCitation>, AgentError> {
        hosted_web_citations(&self.output_items)
    }
}

#[derive(Clone)]
struct CachedOutput {
    call_ids: Vec<String>,
    items: Vec<Value>,
}

/// Stateless OpenAI Responses adapter using `store: false`.
///
/// It replays original reasoning and function-call items between model steps.
/// Native journal replay carries committed provider items across restarts.
pub struct ResponsesBackend {
    endpoint: String,
    api_key: Option<String>,
    model: String,
    reasoning_effort: Option<String>,
    web_search: Option<HostedWebSearchConfig>,
    client: Client,
    replay: Mutex<Vec<CachedOutput>>,
}

impl ResponsesBackend {
    pub fn new(base: impl Into<String>, api_key: Option<String>, model: impl Into<String>) -> Self {
        let base = base.into();
        let endpoint = if base.trim_end_matches('/').ends_with("/responses") {
            base.trim_end_matches('/').to_owned()
        } else {
            format!("{}/responses", base.trim_end_matches('/'))
        };
        Self {
            endpoint,
            api_key,
            model: model.into(),
            reasoning_effort: None,
            web_search: None,
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client configuration is valid"),
            replay: Mutex::new(Vec::new()),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client configuration is valid");
        self
    }

    /// Validates the parameter spelling; the API validates model availability.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Result<Self, AgentError> {
        let effort = effort.into();
        if !matches!(
            effort.as_str(),
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
        ) {
            return Err(AgentError::InvalidRequest(format!(
                "unsupported Responses reasoning effort: {effort}"
            )));
        }
        self.reasoning_effort = Some(effort);
        Ok(self)
    }

    /// Offer provider-hosted web search to the model on Responses requests.
    /// Search is opt-in and remains at the model's discretion (`tool_choice: auto`).
    pub fn with_hosted_web_search(
        mut self,
        config: HostedWebSearchConfig,
    ) -> Result<Self, AgentError> {
        config.validate()?;
        self.web_search = Some(config);
        Ok(self)
    }

    fn request_body(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        stream: bool,
    ) -> Result<Value, AgentError> {
        let replay = self
            .replay
            .lock()
            .map_err(|_| AgentError::Runtime("Responses replay state is unavailable".into()))?;
        let mut input = Vec::<Value>::new();
        for message in messages {
            if !message.images.is_empty() && message.role != MessageRole::User {
                return Err(AgentError::InvalidRequest(
                    "only user messages may contain image inputs".into(),
                ));
            }
            match message.role {
                MessageRole::System => {
                    input.push(json!({"role": "system", "content": message.content}));
                }
                MessageRole::User if message.images.is_empty() => {
                    input.push(json!({"role": "user", "content": message.content}));
                }
                MessageRole::User => {
                    openide_core::validate_user_images(&message.images).map_err(|error| {
                        AgentError::InvalidRequest(format!("invalid user images: {error}"))
                    })?;
                    let mut content = Vec::with_capacity(message.images.len() + 1);
                    if !message.content.is_empty() {
                        content.push(json!({"type": "input_text", "text": message.content}));
                    }
                    for image in &message.images {
                        let data_url = image.verified_data_url().map_err(|error| {
                            AgentError::InvalidRequest(format!("invalid user image: {error}"))
                        })?;
                        content.push(json!({"type": "input_image", "image_url": data_url}));
                    }
                    input.push(json!({"role": "user", "content": content}));
                }
                MessageRole::Assistant => {
                    if !message.provider_output_items.is_empty() {
                        input.extend(message.provider_output_items.iter().cloned());
                        continue;
                    }
                    let call_ids = message
                        .tool_calls
                        .iter()
                        .map(|call| call.id.as_str())
                        .collect::<Vec<_>>();
                    let cached = if call_ids.is_empty() {
                        None
                    } else {
                        replay.iter().rev().find(|output| {
                            output
                                .call_ids
                                .iter()
                                .map(String::as_str)
                                .eq(call_ids.iter().copied())
                        })
                    };
                    if let Some(cached) = cached {
                        input.extend(cached.items.iter().cloned());
                    } else {
                        if !message.content.is_empty() {
                            input.push(json!({"role": "assistant", "content": message.content}));
                        }
                        for call in &message.tool_calls {
                            input.push(json!({
                                "type": "function_call",
                                "call_id": call.id,
                                "name": call.name,
                                "arguments": call.arguments.to_string(),
                            }));
                        }
                    }
                }
                MessageRole::Tool => {
                    let call_id = message.tool_call_id.as_deref().ok_or_else(|| {
                        AgentError::InvalidRequest("tool result has no call ID".into())
                    })?;
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": message.content,
                    }));
                }
            }
        }
        drop(replay);
        let mut tool_items = tools
            .iter()
            .map(|tool| {
                // Current LocalTools schemas have optional fields, so explicitly
                // opt out of Responses' strict-schema normalization.
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                    "strict": false,
                })
            })
            .collect::<Vec<_>>();
        if let Some(config) = &self.web_search {
            tool_items.push(config.wire_tool());
        }
        let mut body = json!({
            "model": self.model,
            "input": input,
            "stream": stream,
            "store": false,
        });
        if !tool_items.is_empty() {
            body["tools"] = Value::Array(tool_items);
        }
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning"] = json!({"effort": effort});
        }
        if self
            .web_search
            .as_ref()
            .is_some_and(|config| config.include_sources)
        {
            body["include"] = json!(["web_search_call.action.sources"]);
        }
        Ok(body)
    }

    /// Derive the count payload from the response payload so image data URLs,
    /// replayed output items, tool schemas, and reasoning options stay exact.
    /// These three response-only fields are absent from the count API schema.
    fn input_token_count_body(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
    ) -> Result<Value, AgentError> {
        let mut body = self.request_body(messages, tools, true)?;
        let fields = body
            .as_object_mut()
            .ok_or_else(|| AgentError::Runtime("Responses request body is not an object".into()))?;
        fields.remove("stream");
        fields.remove("store");
        fields.remove("include");
        Ok(body)
    }

    async fn count_image_input_tokens(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<u64, AgentError> {
        let body = self.input_token_count_body(messages, tools)?;
        let endpoint = format!("{}/input_tokens", self.endpoint);
        retry_model_transport(&cancel, || async {
            let mut request = self.client.post(&endpoint).json(&body);
            if let Some(key) = &self.api_key {
                request = request.bearer_auth(key);
            }
            let response = send_model_request(request, &cancel).await?;
            let bytes = read_model_http_body(response, MAX_INPUT_TOKEN_COUNT_BODY_BYTES, &cancel)
                .await
                .map_err(ModelAttemptError::Terminal)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                ModelAttemptError::Terminal(AgentError::Model(format!(
                    "invalid Responses input token count JSON: {error}"
                )))
            })?;
            let count = parse_input_token_count(&value).map_err(ModelAttemptError::Terminal)?;
            Ok(count)
        })
        .await
    }

    async fn send_once(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        stream: bool,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, ModelAttemptError> {
        let body = self
            .request_body(messages, tools, stream)
            .map_err(ModelAttemptError::Terminal)?;
        let mut request = self.client.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        send_model_request(request, cancel).await
    }

    fn remember(&self, result: &ResponsesResult) -> Result<(), AgentError> {
        if result.output.tool_calls.is_empty() {
            return Ok(());
        }
        let mut replay = self
            .replay
            .lock()
            .map_err(|_| AgentError::Runtime("Responses replay state is unavailable".into()))?;
        replay.push(CachedOutput {
            call_ids: result
                .output
                .tool_calls
                .iter()
                .map(|call| call.id.clone())
                .collect(),
            items: result.output_items.clone(),
        });
        if replay.len() > MAX_REPLAY_ITEMS {
            replay.remove(0);
        }
        while replay
            .iter()
            .flat_map(|entry| entry.items.iter())
            .map(|item| item.to_string().len())
            .sum::<usize>()
            > MAX_REPLAY_BYTES
        {
            replay.remove(0);
        }
        Ok(())
    }

    pub async fn complete_detailed(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ResponsesResult, AgentError> {
        let result = retry_model_transport(&cancel, || async {
            let response = self.send_once(messages, tools, false, &cancel).await?;
            let bytes = read_model_http_body(response, MAX_ASSEMBLED_BYTES, &cancel)
                .await
                .map_err(ModelAttemptError::Terminal)?;
            let body: Value = serde_json::from_slice(&bytes).map_err(|error| {
                ModelAttemptError::Terminal(AgentError::Model(format!(
                    "invalid Responses JSON: {error}"
                )))
            })?;
            parse_response(&body).map_err(ModelAttemptError::Terminal)
        })
        .await?;
        self.remember(&result)?;
        Ok(result)
    }

    pub async fn stream_complete_detailed(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ResponsesResult, AgentError> {
        let result = retry_model_transport(&cancel, || async {
            let response = self.send_once(messages, tools, true, &cancel).await?;
            // A hosted search can execute remotely before any text is emitted.
            // Once the provider accepted an SSE request, a disconnect is
            // ambiguous and repeating it could execute the search twice.
            let hosted_search_may_have_run = self.web_search.is_some();
            let mut stream = Box::pin(response.bytes_stream());
            let mut pending = Vec::<u8>::new();
            let mut assembler = ResponseStream::default();
            loop {
                let next = tokio::select! {
                    _ = cancel.cancelled() => return Err(ModelAttemptError::Terminal(AgentError::Cancelled)),
                    next = stream.next() => next,
                };
                let Some(next) = next else { break };
                let chunk = next.map_err(|error| ModelAttemptError::stream(error, assembler.published_delta || hosted_search_may_have_run))?;
                pending.extend_from_slice(&chunk);
                if pending.len() > MAX_STREAM_FRAME_BYTES {
                    return Err(ModelAttemptError::Terminal(AgentError::Limit(
                        "Responses stream frame is too large".into(),
                    )));
                }
                while let Some(frame) = take_sse_frame(&mut pending) {
                    assembler.ingest(&frame, &deltas).map_err(ModelAttemptError::Terminal)?;
                    if assembler.completed.is_some() {
                        return assembler.finish().map_err(ModelAttemptError::Terminal);
                    }
                }
            }
            if !assembler.seen_frame && pending.starts_with(b"{") {
                // A compatible gateway may return ordinary JSON despite `stream`.
                let body: Value = serde_json::from_slice(&pending).map_err(|error| {
                    ModelAttemptError::Terminal(AgentError::Model(format!("invalid Responses JSON: {error}")))
                })?;
                return parse_response(&body).map_err(ModelAttemptError::Terminal);
            }
            if !pending.is_empty() {
                if incomplete_sse_frame(&pending) {
                    return Err(ModelAttemptError::incomplete_stream(
                        "Responses stream ended inside an SSE frame",
                        assembler.published_delta || hosted_search_may_have_run,
                    ));
                }
                assembler.ingest(&pending, &deltas).map_err(ModelAttemptError::Terminal)?;
                if assembler.completed.is_some() {
                    return assembler.finish().map_err(ModelAttemptError::Terminal);
                }
            }
            Err(ModelAttemptError::incomplete_stream(
                "Responses stream ended before response.completed",
                assembler.published_delta || hosted_search_may_have_run,
            ))
        })
        .await?;
        self.remember(&result)?;
        Ok(result)
    }
}

fn incomplete_sse_frame(frame: &[u8]) -> bool {
    let Ok(frame) = std::str::from_utf8(frame) else {
        return true;
    };
    let data = frame
        .lines()
        .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return true;
    }
    serde_json::from_str::<Value>(&data).is_err_and(|error| error.is_eof())
}

#[async_trait]
impl ModelBackend for ResponsesBackend {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn supports_user_images(&self) -> bool {
        true
    }

    async fn count_input_tokens(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<Option<u64>, AgentError> {
        if messages.iter().all(|message| message.images.is_empty()) {
            return Ok(None);
        }
        self.count_image_input_tokens(messages, tools, cancel)
            .await
            .map(Some)
    }

    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        let mut fields = vec![
            "responses-v1",
            &self.endpoint,
            &self.model,
            self.reasoning_effort.as_deref().unwrap_or("<absent>"),
            "store-false",
            "tool-strict-false",
            "stream-true",
        ];
        let web_search = self
            .web_search
            .as_ref()
            .map(HostedWebSearchConfig::fingerprint_component);
        if let Some(web_search) = &web_search {
            fields.extend(["hosted-web-search-v1", web_search.as_str()]);
        }
        Some(stable_request_profile_fingerprint(&fields))
    }

    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        Ok(self
            .complete_detailed(messages, tools, cancel)
            .await?
            .output)
    }

    async fn stream_complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelOutput, AgentError> {
        Ok(self
            .stream_complete_detailed(messages, tools, cancel, deltas)
            .await?
            .output)
    }

    async fn stream_complete_detailed(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelCompletion, AgentError> {
        let result =
            ResponsesBackend::stream_complete_detailed(self, messages, tools, cancel, deltas)
                .await?;
        Ok(ModelCompletion {
            output: result.output,
            response_id: Some(result.response_id),
            usage: result.usage,
            provider_output_items: result.output_items,
        })
    }
}

fn parse_input_token_count(value: &Value) -> Result<u64, AgentError> {
    if value.get("object").and_then(Value::as_str) != Some("response.input_tokens") {
        return Err(AgentError::Model(
            "Responses input token count has an unexpected object type".into(),
        ));
    }
    value
        .get("input_tokens")
        .and_then(Value::as_u64)
        .filter(|count| *count > 0)
        .ok_or_else(|| {
            AgentError::Model("Responses input token count is missing or invalid".into())
        })
}

fn parse_response(body: &Value) -> Result<ResponsesResult, AgentError> {
    let status = body.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "completed" {
        let detail = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| {
                body.pointer("/incomplete_details/reason")
                    .and_then(Value::as_str)
            })
            .unwrap_or(status);
        return Err(AgentError::Model(format!(
            "Responses request {status}: {detail}"
        )));
    }
    let response_id = body
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentError::Model("Responses result has no ID".into()))?
        .to_owned();
    let output_items = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| AgentError::Model("Responses result has no output array".into()))?
        .clone();
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    let mut seen_call_ids = HashSet::new();
    for item in &output_items {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        let text = match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => part.get("text").and_then(Value::as_str),
                            Some("refusal") => part.get("refusal").and_then(Value::as_str),
                            _ => None,
                        };
                        if let Some(text) = text {
                            append_limited(&mut content, text)?;
                        }
                    }
                }
            }
            Some("function_call") => {
                let call_id = required_string(item, "call_id", "function call")?;
                let name = required_string(item, "name", "function call")?;
                if !seen_call_ids.insert(call_id) {
                    return Err(AgentError::Model(format!(
                        "duplicate Responses function call ID: {call_id}"
                    )));
                }
                let arguments = required_string(item, "arguments", "function call")?;
                let arguments = serde_json::from_str(arguments).map_err(|error| {
                    AgentError::Model(format!("invalid Responses function arguments: {error}"))
                })?;
                tool_calls.push(ToolCall {
                    id: call_id.to_owned(),
                    name: name.to_owned(),
                    arguments,
                });
            }
            _ => {}
        }
    }
    let usage = body
        .get("usage")
        .filter(|value| !value.is_null())
        .map(|value| {
            let number = |field: &str| -> Result<u64, AgentError> {
                value.get(field).and_then(Value::as_u64).ok_or_else(|| {
                    AgentError::Model(format!("Responses usage has no valid {field}"))
                })
            };
            Ok(ResponseUsage {
                input_tokens: number("input_tokens")?,
                output_tokens: number("output_tokens")?,
                total_tokens: number("total_tokens")?,
                reasoning_tokens: value
                    .pointer("/output_tokens_details/reasoning_tokens")
                    .and_then(Value::as_u64),
                cached_input_tokens: value
                    .pointer("/input_tokens_details/cached_tokens")
                    .and_then(Value::as_u64),
            })
        })
        .transpose()?;
    Ok(ResponsesResult {
        output: ModelOutput {
            content,
            tool_calls,
        },
        response_id,
        usage,
        output_items,
    })
}

fn required_string<'a>(value: &'a Value, field: &str, source: &str) -> Result<&'a str, AgentError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentError::Model(format!("{source} has no {field}")))
}

fn append_limited(target: &mut String, fragment: &str) -> Result<(), AgentError> {
    if fragment.len() > MAX_ASSEMBLED_BYTES.saturating_sub(target.len()) {
        return Err(AgentError::Limit("Responses output is too large".into()));
    }
    target.push_str(fragment);
    Ok(())
}

#[derive(Default)]
struct PartialCall {
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct ResponseStream {
    seen_frame: bool,
    published_delta: bool,
    completed: Option<Value>,
    calls: BTreeMap<u64, PartialCall>,
    text: String,
}

impl ResponseStream {
    fn ingest(&mut self, frame: &[u8], deltas: &UnboundedSender<String>) -> Result<(), AgentError> {
        let frame = std::str::from_utf8(frame)
            .map_err(|error| AgentError::Model(format!("invalid Responses SSE UTF-8: {error}")))?;
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
            return Ok(());
        }
        let event: Value = serde_json::from_str(&data)
            .map_err(|error| AgentError::Model(format!("invalid Responses SSE JSON: {error}")))?;
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AgentError::Model("Responses text delta has no text".into()))?;
                if !delta.is_empty() {
                    append_limited(&mut self.text, delta)?;
                    self.published_delta = true;
                    let _ = deltas.send(delta.to_owned());
                }
            }
            Some("response.output_item.added" | "response.output_item.done") => {
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        let index = event
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .ok_or_else(|| {
                                AgentError::Model(
                                    "Responses function item has no output index".into(),
                                )
                            })?;
                        let partial = self.calls.entry(index).or_default();
                        update_partial_call(partial, item)?;
                    }
                }
            }
            Some("response.function_call_arguments.delta") => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        AgentError::Model("Responses function delta has no output index".into())
                    })?;
                let partial = self.calls.get_mut(&index).ok_or_else(|| {
                    AgentError::Model("Responses function delta preceded its call item".into())
                })?;
                let item_id = required_string(&event, "item_id", "function delta")?;
                if partial.item_id != item_id {
                    return Err(AgentError::Model(
                        "Responses function delta item ID mismatch".into(),
                    ));
                }
                let delta = event.get("delta").and_then(Value::as_str).ok_or_else(|| {
                    AgentError::Model("Responses function delta has no text".into())
                })?;
                append_limited(&mut partial.arguments, delta)?;
            }
            Some("response.function_call_arguments.done") => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        AgentError::Model(
                            "Responses function completion has no output index".into(),
                        )
                    })?;
                let partial = self.calls.get_mut(&index).ok_or_else(|| {
                    AgentError::Model("Responses function completion preceded its call item".into())
                })?;
                let item_id = required_string(&event, "item_id", "function completion")?;
                if partial.item_id != item_id {
                    return Err(AgentError::Model(
                        "Responses function completion item ID mismatch".into(),
                    ));
                }
                if let Some(arguments) = event.get("arguments").and_then(Value::as_str) {
                    if arguments.len() > MAX_ASSEMBLED_BYTES {
                        return Err(AgentError::Limit(
                            "Responses function arguments are too large".into(),
                        ));
                    }
                    if !partial.arguments.is_empty() && partial.arguments != arguments {
                        return Err(AgentError::Model(
                            "Responses function argument deltas mismatch final arguments".into(),
                        ));
                    }
                    partial.arguments = arguments.to_owned();
                }
            }
            Some("response.completed") => {
                self.completed = event.get("response").cloned();
                if self.completed.is_none() {
                    return Err(AgentError::Model(
                        "Responses completed event has no response".into(),
                    ));
                }
            }
            Some("response.failed" | "response.incomplete" | "error") => {
                let detail = event
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("message").and_then(Value::as_str))
                    .or_else(|| {
                        event
                            .pointer("/response/incomplete_details/reason")
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("unknown failure");
                return Err(AgentError::Model(format!(
                    "Responses stream failed: {detail}"
                )));
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<ResponsesResult, AgentError> {
        let response = self.completed.ok_or_else(|| {
            AgentError::Model("Responses stream ended before response.completed".into())
        })?;
        let result = parse_response(&response)?;
        if !self.text.is_empty() && self.text != result.output.content {
            return Err(AgentError::Model(
                "Responses streamed text differs from completed response".into(),
            ));
        }
        for (index, partial) in self.calls {
            let item = result.output_items.get(index as usize).ok_or_else(|| {
                AgentError::Model("Responses function call output index is missing".into())
            })?;
            if item.get("type").and_then(Value::as_str) != Some("function_call")
                || item.get("id").and_then(Value::as_str) != Some(partial.item_id.as_str())
                || item.get("call_id").and_then(Value::as_str) != Some(partial.call_id.as_str())
                || item.get("name").and_then(Value::as_str) != Some(partial.name.as_str())
                || item.get("arguments").and_then(Value::as_str) != Some(partial.arguments.as_str())
            {
                return Err(AgentError::Model(
                    "Responses streamed function call mismatch".into(),
                ));
            }
        }
        Ok(result)
    }
}

fn update_partial_call(partial: &mut PartialCall, item: &Value) -> Result<(), AgentError> {
    let item_id = required_string(item, "id", "function item")?;
    let call_id = required_string(item, "call_id", "function item")?;
    let name = required_string(item, "name", "function item")?;
    if !partial.item_id.is_empty()
        && (partial.item_id != item_id || partial.call_id != call_id || partial.name != name)
    {
        return Err(AgentError::Model(
            "Responses function item changed identity".into(),
        ));
    }
    partial.item_id = item_id.to_owned();
    partial.call_id = call_id.to_owned();
    partial.name = name.to_owned();
    if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
        if arguments.len() > MAX_ASSEMBLED_BYTES {
            return Err(AgentError::Limit(
                "Responses function arguments are too large".into(),
            ));
        }
        // `added` starts with an empty string; `done` carries the final string.
        if !arguments.is_empty() {
            if !partial.arguments.is_empty() && partial.arguments != arguments {
                return Err(AgentError::Model(
                    "Responses function argument deltas mismatch completed item".into(),
                ));
            }
            partial.arguments = arguments.to_owned();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::{UserImage, UserImageMime};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc::unbounded_channel;

    fn tool() -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        }
    }

    fn tiny_png() -> UserImage {
        UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nfixture").unwrap()
    }

    #[test]
    fn image_inputs_are_verified_and_keep_text_then_image_order() {
        let backend = ResponsesBackend::new("https://api.openai.com/v1", None, "mock");
        assert!(backend.supports_user_images());
        assert_eq!(
            backend.request_profile_fingerprint(),
            Some(stable_request_profile_fingerprint(&[
                "responses-v1",
                "https://api.openai.com/v1/responses",
                "mock",
                "<absent>",
                "store-false",
                "tool-strict-false",
                "stream-true",
            ]))
        );
        let image = tiny_png();
        let message = AgentMessage::user_with_images("describe", vec![image.clone()]).unwrap();
        let body = backend
            .request_body(std::slice::from_ref(&message), &[], true)
            .unwrap();
        assert_eq!(
            body["input"][0]["content"][0],
            json!({"type":"input_text","text":"describe"})
        );
        assert_eq!(
            body["input"][0]["content"][1],
            json!({
                "type":"input_image",
                "image_url":image.verified_data_url().unwrap(),
            })
        );
        assert!(body["input"][0]["content"][1].get("path").is_none());

        let restored: AgentMessage =
            serde_json::from_value(serde_json::to_value(&message).unwrap()).unwrap();
        assert_eq!(backend.request_body(&[restored], &[], true).unwrap(), body);

        let image_only = AgentMessage::user_with_images("", vec![tiny_png()]).unwrap();
        let body = backend.request_body(&[image_only], &[], false).unwrap();
        assert_eq!(body["input"][0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(body["input"][0]["content"][0]["type"], "input_image");

        let mut tampered = AgentMessage::user_with_images("describe", vec![image]).unwrap();
        tampered.images[0].sha256 = "wrong".into();
        assert!(matches!(
            backend.request_body(&[tampered], &[], true),
            Err(AgentError::InvalidRequest(_))
        ));

        let mut illegal_role = AgentMessage::assistant("", vec![]);
        illegal_role.images.push(tiny_png());
        assert!(matches!(
            backend.request_body(&[illegal_role], &[], true),
            Err(AgentError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn streamed_http_request_replays_durable_image_input() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected_url = tiny_png().verified_data_url().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /v1/responses "));
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["stream"], true);
            assert_eq!(request["input"][0]["content"][0]["text"], "describe");
            assert_eq!(request["input"][0]["content"][1]["image_url"], expected_url);
            let completed = json!({"type":"response.completed","response":{"id":"resp_img","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"a picture"}]}]}});
            let sse = format!("data: {{\"type\":\"response.output_text.delta\",\"delta\":\"a picture\"}}\n\ndata: {completed}\n\n");
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}", sse.len(), sse).unwrap();
            socket.flush().unwrap();
        });

        let stored = AgentMessage::user_with_images("describe", vec![tiny_png()]).unwrap();
        let restored: AgentMessage =
            serde_json::from_value(serde_json::to_value(stored).unwrap()).unwrap();
        let backend = ResponsesBackend::new(format!("http://{address}/v1"), None, "mock");
        let (sender, mut receiver) = unbounded_channel();
        let result = backend
            .stream_complete_detailed(&[restored], &[], CancellationToken::new(), sender)
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(result.output.content, "a picture");
        assert_eq!(receiver.try_recv().unwrap(), "a picture");
    }

    #[tokio::test]
    async fn image_count_preflight_uses_same_input_and_tools_before_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected_url = tiny_png().verified_data_url().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /v1/responses/input_tokens HTTP/1.1"));
            let mut length = 0usize;
            let mut authorized = false;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if line.trim() == "authorization: Bearer secret" {
                    authorized = true;
                }
            }
            assert!(authorized);
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["model"], "mock");
            assert_eq!(request["input"][0]["content"][0]["text"], "describe");
            assert_eq!(request["input"][0]["content"][1]["image_url"], expected_url);
            assert_eq!(request["tools"][0]["name"], "read_file");
            assert_eq!(request["tools"][0]["strict"], false);
            assert_eq!(request["reasoning"]["effort"], "low");
            assert!(request.get("stream").is_none());
            assert!(request.get("store").is_none());
            assert!(request.get("include").is_none());
            let result = r#"{"object":"response.input_tokens","input_tokens":934}"#;
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{result}",
                result.len()
            )
            .unwrap();
            socket.flush().unwrap();
        });

        let backend = ResponsesBackend::new(
            format!("http://{address}/v1"),
            Some("secret".into()),
            "mock",
        )
        .with_reasoning_effort("low")
        .unwrap();
        let message = AgentMessage::user_with_images("describe", vec![tiny_png()]).unwrap();
        let count = backend
            .count_input_tokens(&[message], &[tool()], CancellationToken::new())
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(count, Some(934));
    }

    #[tokio::test]
    async fn image_count_preflight_rejects_unknown_shape_and_honors_cancellation() {
        for value in [
            json!({"object":"response","input_tokens":934}),
            json!({"object":"response.input_tokens","input_tokens":"934"}),
            json!({"object":"response.input_tokens","input_tokens":0}),
        ] {
            assert!(matches!(
                parse_input_token_count(&value),
                Err(AgentError::Model(_))
            ));
        }
        let backend = ResponsesBackend::new("http://127.0.0.1:1/v1", None, "mock");
        assert_eq!(
            backend
                .count_input_tokens(
                    &[AgentMessage::user("text only")],
                    &[],
                    CancellationToken::new(),
                )
                .await
                .unwrap(),
            None
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            backend
                .count_input_tokens(
                    &[AgentMessage::user_with_images("", vec![tiny_png()]).unwrap()],
                    &[],
                    cancel
                )
                .await,
            Err(AgentError::Cancelled)
        ));
    }

    #[test]
    fn hosted_web_search_is_opt_in_and_bound_to_resume_profile() {
        let base = ResponsesBackend::new("https://api.openai.com/v1", None, "gpt-6-astra");
        let original = base
            .request_body(&[AgentMessage::user("search")], &[], true)
            .unwrap();
        assert!(original.get("tools").is_none());
        assert!(original.get("include").is_none());

        let mut config = HostedWebSearchConfig::new(HostedWebSearchMode::Cached);
        config.allowed_domains = vec!["example.com".into()];
        let enabled = ResponsesBackend::new("https://api.openai.com/v1", None, "gpt-6-astra")
            .with_hosted_web_search(config.clone())
            .unwrap();
        let request = enabled
            .request_body(&[AgentMessage::user("search")], &[tool()], true)
            .unwrap();
        assert_eq!(request["tools"][0]["type"], "function");
        assert_eq!(request["tools"][1]["type"], "web_search");
        assert_eq!(request["tools"][1]["external_web_access"], false);
        assert_eq!(
            request["tools"][1]["filters"]["allowed_domains"][0],
            "example.com"
        );
        assert_eq!(request["include"][0], "web_search_call.action.sources");
        assert_ne!(
            base.request_profile_fingerprint(),
            enabled.request_profile_fingerprint()
        );

        config.mode = HostedWebSearchMode::Live;
        config.include_sources = false;
        let live = ResponsesBackend::new("https://api.openai.com/v1", None, "gpt-6-astra")
            .with_hosted_web_search(config)
            .unwrap();
        let live_request = live
            .request_body(&[AgentMessage::user("search")], &[], true)
            .unwrap();
        assert_eq!(live_request["tools"][0]["external_web_access"], true);
        assert!(live_request.get("include").is_none());
        assert_ne!(
            enabled.request_profile_fingerprint(),
            live.request_profile_fingerprint()
        );
    }

    #[tokio::test]
    async fn hosted_web_search_streams_text_and_preserves_typed_citations_for_replay() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /v1/responses "));
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["tools"][0]["type"], "web_search");
            assert_eq!(request["tools"][0]["external_web_access"], true);
            assert_eq!(request["include"][0], "web_search_call.action.sources");
            assert_eq!(request["store"], false);

            let call = json!({"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"topic","queries":["topic"],"sources":[{"type":"url","url":"https://example.com/story","title":"Example"}]}});
            let message = json!({"type":"message","content":[{"type":"output_text","text":"Answer [1]","annotations":[{"type":"url_citation","start_index":7,"end_index":10,"url":"https://example.com/story","title":"Example"}]}]});
            let completed = json!({"type":"response.completed","response":{"id":"resp_web","status":"completed","output":[call,message]}});
            let sse = [
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"web_search_call","id":"ws_1","status":"in_progress"}}),
                json!({"type":"response.output_text.delta","delta":"Answer [1]"}),
                completed,
            ]
            .into_iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}", sse.len(), sse).unwrap();
            socket.flush().unwrap();
        });

        let backend = ResponsesBackend::new(format!("http://{address}/v1"), None, "mock")
            .with_hosted_web_search(HostedWebSearchConfig::new(HostedWebSearchMode::Live))
            .unwrap();
        let (sender, mut receiver) = unbounded_channel();
        let result = backend
            .stream_complete_detailed(
                &[AgentMessage::user("topic")],
                &[],
                CancellationToken::new(),
                sender,
            )
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(receiver.try_recv().unwrap(), "Answer [1]");
        assert_eq!(result.output.content, "Answer [1]");
        assert_eq!(
            result.web_search_calls().unwrap()[0].id.as_deref(),
            Some("ws_1")
        );
        assert_eq!(
            result.web_citations().unwrap()[0].url,
            "https://example.com/story"
        );

        let mut assistant = AgentMessage::assistant(result.output.content, vec![]);
        assistant.provider_output_items = result.output_items.clone();
        let replay = backend
            .request_body(
                &[
                    AgentMessage::user("topic"),
                    assistant,
                    AgentMessage::user("follow up"),
                ],
                &[],
                false,
            )
            .unwrap();
        assert_eq!(replay["input"][1], result.output_items[0]);
        assert_eq!(replay["input"][2], result.output_items[1]);
    }

    #[tokio::test]
    async fn hosted_web_search_does_not_repeat_an_ambiguous_sse_disconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 2048];
            loop {
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let body_len = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + body_len {
                        break;
                    }
                }
            }
            let body = "data: {\"type\":\"response.created\"}\n\n";
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len() + 40);
            socket.write_all(response.as_bytes()).await.unwrap();
            drop(socket);
            if let Ok(Ok((mut unexpected, _))) =
                tokio::time::timeout(Duration::from_millis(500), listener.accept()).await
            {
                let _ = unexpected
                    .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                    .await;
                panic!("hosted search retried after ambiguous SSE disconnect");
            }
        });
        let backend = ResponsesBackend::new(format!("http://{address}/v1"), None, "mock")
            .with_hosted_web_search(HostedWebSearchConfig::new(HostedWebSearchMode::Live))
            .unwrap();
        let (sender, mut receiver) = unbounded_channel();
        let error = backend
            .stream_complete_detailed(
                &[AgentMessage::user("topic")],
                &[],
                CancellationToken::new(),
                sender,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::Model(_)));
        assert!(receiver.try_recv().is_err());
        server.await.unwrap();
    }

    #[test]
    fn maps_chat_history_to_responses_items_without_server_storage() {
        let backend = ResponsesBackend::new("https://api.openai.com/v1", None, "gpt-6-astra")
            .with_reasoning_effort("high")
            .unwrap();
        let call = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"README.md"}),
        };
        let messages = [
            AgentMessage::system("instructions"),
            AgentMessage::user("read"),
            AgentMessage::assistant("", vec![call]),
            AgentMessage::tool("call_1", "contents"),
        ];
        let body = backend.request_body(&messages, &[tool()], true).unwrap();
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["strict"], false);
        assert_eq!(body["input"][2]["type"], "function_call");
        assert_eq!(body["input"][2]["call_id"], "call_1");
        assert_eq!(body["input"][3]["type"], "function_call_output");
    }

    #[test]
    fn request_profile_changes_with_effort_and_wire_protocol() {
        let base = ResponsesBackend::new("https://api.openai.com/v1", None, "gpt-6-sol");
        let high = ResponsesBackend::new("https://api.openai.com/v1", None, "gpt-6-sol")
            .with_reasoning_effort("high")
            .unwrap();
        let chat =
            crate::OpenAiCompatibleBackend::new("https://api.openai.com/v1", None, "gpt-6-sol");
        assert_ne!(
            base.request_profile_fingerprint(),
            high.request_profile_fingerprint()
        );
        assert_ne!(
            base.request_profile_fingerprint(),
            chat.request_profile_fingerprint()
        );
    }

    #[test]
    fn parses_response_id_usage_text_and_parallel_calls() {
        let result = parse_response(&json!({
            "id":"resp_1","status":"completed","usage":{
                "input_tokens":10,"output_tokens":20,"total_tokens":30,
                "input_tokens_details":{"cached_tokens":3},
                "output_tokens_details":{"reasoning_tokens":8}
            },
            "output":[
                {"type":"reasoning","id":"rs_1","encrypted_content":"opaque"},
                {"type":"message","content":[{"type":"output_text","text":"checking"}]},
                {"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a\"}"},
                {"type":"function_call","id":"fc_2","call_id":"call_2","name":"read_file","arguments":"{\"path\":\"b\"}"}
            ]
        })).unwrap();
        assert_eq!(result.response_id, "resp_1");
        assert_eq!(result.output.content, "checking");
        assert_eq!(result.output.tool_calls.len(), 2);
        assert_eq!(result.output.tool_calls[1].id, "call_2");
        assert_eq!(result.usage.unwrap().reasoning_tokens, Some(8));
        assert_eq!(result.output_items[0]["encrypted_content"], "opaque");
    }

    #[test]
    fn replays_original_reasoning_and_call_items_during_followup() {
        let backend = ResponsesBackend::new("https://api.openai.com/v1", None, "gpt-6-astra");
        let result = parse_response(&json!({
            "id":"resp_1","status":"completed","output":[
                {"type":"reasoning","id":"rs_1","encrypted_content":"opaque"},
                {"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a\"}"}
            ]
        })).unwrap();
        backend.remember(&result).unwrap();
        let body = backend
            .request_body(
                &[
                    AgentMessage::user("read"),
                    AgentMessage::assistant("", result.output.tool_calls),
                    AgentMessage::tool("call_1", "contents"),
                ],
                &[tool()],
                false,
            )
            .unwrap();
        assert_eq!(body["input"][1]["type"], "reasoning");
        assert_eq!(body["input"][1]["encrypted_content"], "opaque");
        assert_eq!(body["input"][2]["id"], "fc_1");
        assert_eq!(body["input"][3]["type"], "function_call_output");
    }

    #[test]
    fn replays_persisted_items_after_backend_recreation() {
        let original = parse_response(&json!({
            "id":"resp_1","status":"completed","output":[
                {"type":"reasoning","id":"rs_1","encrypted_content":"opaque"},
                {"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a\"}"}
            ]
        })).unwrap();
        let mut assistant = AgentMessage::assistant("", original.output.tool_calls);
        assistant.provider_output_items = original.output_items;
        let saved = serde_json::to_vec(&assistant).unwrap();
        let restored: AgentMessage = serde_json::from_slice(&saved).unwrap();
        let fresh = ResponsesBackend::new("https://api.openai.com/v1", None, "gpt-6-astra");
        let body = fresh
            .request_body(
                &[
                    AgentMessage::user("read"),
                    restored,
                    AgentMessage::tool("call_1", "contents"),
                ],
                &[tool()],
                false,
            )
            .unwrap();
        assert_eq!(body["input"][1]["encrypted_content"], "opaque");
        assert_eq!(body["input"][2]["id"], "fc_1");
        assert_eq!(body["input"][3]["call_id"], "call_1");
    }

    #[test]
    fn correlates_interleaved_streamed_function_arguments() {
        let (sender, mut receiver) = unbounded_channel();
        let mut stream = ResponseStream::default();
        let events = [
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":""}}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_2","call_id":"call_2","name":"read_file","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"fc_2","delta":"{\"path\":\"b\"}"}),
            json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"fc_1","delta":"{\"path\":\"a\"}"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a\"}"}}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc_2","call_id":"call_2","name":"read_file","arguments":"{\"path\":\"b\"}"}}),
            json!({"type":"response.output_text.delta","delta":"working"}),
            json!({"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[
                {"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a\"}"},
                {"type":"function_call","id":"fc_2","call_id":"call_2","name":"read_file","arguments":"{\"path\":\"b\"}"},
                {"type":"message","content":[{"type":"output_text","text":"working"}]}
            ]}}),
        ];
        for event in events {
            let frame = format!("data: {}\n\n", event);
            stream.ingest(frame.as_bytes(), &sender).unwrap();
        }
        assert_eq!(receiver.try_recv().unwrap(), "working");
        assert_eq!(stream.finish().unwrap().output.tool_calls.len(), 2);
    }

    #[test]
    fn incomplete_stream_and_bad_call_correlation_fail_closed() {
        let (sender, _) = unbounded_channel();
        assert!(ResponseStream::default().finish().is_err());
        let mut stream = ResponseStream::default();
        let added = json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":""}});
        stream
            .ingest(format!("data: {added}\n\n").as_bytes(), &sender)
            .unwrap();
        let wrong = json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"fc_wrong","delta":"{}"});
        assert!(stream
            .ingest(format!("data: {wrong}\n\n").as_bytes(), &sender)
            .is_err());
    }

    #[tokio::test]
    async fn trait_object_streaming_http_preserves_responses_metadata() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /v1/responses "));
            let mut length = 0usize;
            let mut authorized = false;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if lower.starts_with("authorization: bearer test-key") {
                    authorized = true;
                }
            }
            assert!(authorized);
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["model"], "gpt-6-astra");
            assert_eq!(request["store"], false);
            assert_eq!(request["stream"], true);
            assert_eq!(request["tools"][0]["name"], "read_file");
            assert_eq!(request["tools"][0]["strict"], false);

            let final_response = json!({
                "id":"resp_http","status":"completed",
                "usage":{"input_tokens":12,"output_tokens":7,"total_tokens":19},
                "output":[
                    {"type":"reasoning","id":"rs_http","encrypted_content":"opaque"},
                    {"type":"message","content":[{"type":"output_text","text":"hello"}]},
                    {"type":"function_call","id":"fc_http","call_id":"call_http","name":"read_file","arguments":"{\"path\":\"README.md\"}"}
                ]
            });
            let frames = [
                json!({"type":"response.output_text.delta","delta":"hel"}),
                json!({"type":"response.output_text.delta","delta":"lo"}),
                json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc_http","call_id":"call_http","name":"read_file","arguments":""}}),
                json!({"type":"response.function_call_arguments.delta","output_index":2,"item_id":"fc_http","delta":"{\"path\":\"README.md\"}"}),
                json!({"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","id":"fc_http","call_id":"call_http","name":"read_file","arguments":"{\"path\":\"README.md\"}"}}),
                json!({"type":"response.completed","response":final_response}),
            ];
            let sse = frames
                .into_iter()
                .map(|event| format!("data: {event}\r\n\r\n"))
                .collect::<String>();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                sse.len(),
                sse
            )
            .unwrap();
            socket.flush().unwrap();
        });

        let backend = ResponsesBackend::new(
            format!("http://{address}/v1"),
            Some("test-key".into()),
            "gpt-6-astra",
        );
        let model: &dyn ModelBackend = &backend;
        let (sender, mut receiver) = unbounded_channel();
        let result = model
            .stream_complete_detailed(
                &[AgentMessage::user("read the file")],
                &[tool()],
                CancellationToken::new(),
                sender,
            )
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(result.response_id.as_deref(), Some("resp_http"));
        assert_eq!(result.output.content, "hello");
        assert_eq!(result.output.tool_calls[0].id, "call_http");
        assert_eq!(result.usage.unwrap().total_tokens, 19);
        assert_eq!(
            result.provider_output_items[0]["encrypted_content"],
            "opaque"
        );
        assert_eq!(receiver.try_recv().unwrap(), "hel");
        assert_eq!(receiver.try_recv().unwrap(), "lo");
    }

    #[tokio::test]
    async fn nonstream_http_tool_cycle_replays_original_output_items() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for step in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                assert!(first.starts_with("POST /v1/responses "));
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(request["store"], false);
                assert_eq!(request["stream"], false);
                let response = if step == 0 {
                    json!({"id":"resp_first","status":"completed","output":[
                        {"type":"reasoning","id":"rs_first","encrypted_content":"opaque-first"},
                        {"type":"function_call","id":"fc_first","call_id":"call_first","name":"read_file","arguments":"{\"path\":\"README.md\"}"}
                    ]})
                } else {
                    assert_eq!(request["input"][1]["type"], "reasoning");
                    assert_eq!(request["input"][1]["encrypted_content"], "opaque-first");
                    assert_eq!(request["input"][2]["id"], "fc_first");
                    assert_eq!(request["input"][3]["type"], "function_call_output");
                    assert_eq!(request["input"][3]["call_id"], "call_first");
                    json!({"id":"resp_second","status":"completed","output":[
                        {"type":"message","content":[{"type":"output_text","text":"done"}]}
                    ]})
                };
                let payload = response.to_string();
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    payload.len(),
                    payload
                )
                .unwrap();
                socket.flush().unwrap();
            }
        });

        let backend = ResponsesBackend::new(format!("http://{address}/v1"), None, "gpt-6-astra");
        let first = backend
            .complete_detailed(
                &[AgentMessage::user("read")],
                &[tool()],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(first.output.tool_calls[0].id, "call_first");
        let second = backend
            .complete_detailed(
                &[
                    AgentMessage::user("read"),
                    AgentMessage::assistant("", first.output.tool_calls),
                    AgentMessage::tool("call_first", "file contents"),
                ],
                &[tool()],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(second.response_id, "resp_second");
        assert_eq!(second.output.content, "done");
    }
}
