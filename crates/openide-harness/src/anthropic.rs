//! Direct Anthropic Messages transport; no external agent process or SDK.
//! Contracts: https://platform.claude.com/docs/en/api/messages/create,
//! https://platform.claude.com/docs/en/build-with-claude/streaming and
//! https://platform.claude.com/docs/en/api/messages/count_tokens.
//! Opaque thinking/signatures stay in ordered provider items, never final text.

#[cfg(test)]
use crate::backend::SseFrameBuffer;
use crate::backend::{
    model_http_client_builder, model_response_body_kind, model_response_mime, read_model_http_body,
    retry_model_transport, send_model_request, sse_lines, stable_request_profile_fingerprint,
    ModelAttemptError, ModelStreamBody, MAX_MODEL_HTTP_BODY_BYTES, MAX_MODEL_TOOL_CALLS,
};
#[cfg(test)]
use crate::model_stream::publish_legacy;
use crate::model_stream::ModelStreamDelivery;
use crate::{
    AgentError, ModelBackend, ModelCompletion, ModelEventSender, ModelOutput, ModelTokenUsage,
    ToolDefinition,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use openide_core::{AgentMessage, MessageRole, ModelStreamEvent, ToolCall};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

const VERSION: &str = "2023-06-01";
pub const DEFAULT_ANTHROPIC_MAX_OUTPUT_TOKENS: u64 = 4096;
const MAX_RETAINED_BYTES: usize = 8 * 1024 * 1024;
const MAX_BLOCKS: usize = 256;
const MAX_STREAM_EVENTS: usize = 100_000;
/// What the Messages endpoints accept for one request.
const MAX_REQUEST_BYTES: usize = 32_000_000;

pub struct AnthropicMessagesBackend {
    endpoint: String,
    api_key: Option<String>,
    model: String,
    max_tokens: u64,
    fields: BTreeMap<String, Value>,
    headers: BTreeMap<String, String>,
    image_input_enabled: bool,
    client: Client,
}

impl AnthropicMessagesBackend {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        let base = endpoint.into();
        let base = base.trim_end_matches('/');
        Self {
            endpoint: if base.ends_with("/messages") {
                base.into()
            } else {
                format!("{base}/messages")
            },
            api_key,
            model: model.into(),
            // A request policy, never a declaration of a model's output capacity.
            max_tokens: DEFAULT_ANTHROPIC_MAX_OUTPUT_TOKENS,
            fields: BTreeMap::new(),
            headers: BTreeMap::new(),
            image_input_enabled: true,
            client: model_http_client_builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("valid HTTP client configuration"),
        }
    }

    /// Override the default idle timeout with an explicit total request limit.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .expect("valid HTTP client configuration");
        self
    }

    pub fn with_image_input_enabled(mut self, enabled: bool) -> Self {
        self.image_input_enabled = enabled;
        self
    }

    pub fn with_max_output_tokens(mut self, tokens: u64) -> Result<Self, AgentError> {
        if tokens == 0 || tokens > u64::from(u32::MAX) {
            return Err(AgentError::InvalidRequest(
                "Anthropic max_tokens must be a positive bounded integer".into(),
            ));
        }
        validate_fields(&self.fields, tokens)?;
        self.max_tokens = tokens;
        Ok(self)
    }

    pub fn with_request_fields(
        mut self,
        fields: BTreeMap<String, Value>,
    ) -> Result<Self, AgentError> {
        validate_fields(&fields, self.max_tokens)?;
        self.fields = fields;
        Ok(self)
    }

    /// Additional noncredential headers. Authentication remains separately
    /// owned, and the parser pins the documented Messages protocol version.
    pub fn with_headers(mut self, headers: BTreeMap<String, String>) -> Result<Self, AgentError> {
        let mut normalized = BTreeMap::new();
        let mut bytes = 0_usize;
        if headers.len() > 16 {
            return Err(AgentError::InvalidRequest(
                "too many Anthropic headers".into(),
            ));
        }
        for (name, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| AgentError::InvalidRequest("invalid Anthropic header name".into()))?
                .as_str()
                .to_owned();
            bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
            if [
                "x-api-key",
                "authorization",
                "proxy-authorization",
                "cookie",
                "set-cookie",
            ]
            .contains(&name.as_str())
                || name.len() > 128
                || value.len() > 2048
                || value.contains(['\r', '\n'])
                || bytes > 8192
                || normalized.contains_key(&name)
            {
                return Err(AgentError::InvalidRequest(
                    "reserved, repeated or oversized Anthropic header".into(),
                ));
            }
            reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| AgentError::InvalidRequest("invalid Anthropic header value".into()))?;
            if name == "anthropic-version" && value != VERSION {
                return Err(AgentError::InvalidRequest(
                    "unsupported Anthropic protocol version".into(),
                ));
            }
            normalized.insert(name, value);
        }
        self.headers = normalized;
        Ok(self)
    }

    fn endpoint_digest(&self) -> Result<String, AgentError> {
        openide_core::native_model_endpoint_digest(&self.endpoint)
            .ok_or_else(|| AgentError::InvalidRequest("invalid Anthropic endpoint identity".into()))
    }

    fn request_body(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        stream: bool,
        count: bool,
    ) -> Result<Value, AgentError> {
        let endpoint_digest = self.endpoint_digest()?;
        if self.model.trim().is_empty()
            || self.model.len() > openide_core::MAX_MODEL_ROUTE_MODEL_BYTES
            || self.model.chars().any(char::is_control)
        {
            return Err(AgentError::InvalidRequest(
                "invalid exact Anthropic model ID".into(),
            ));
        }
        let mut system = Vec::new();
        let mut history = Vec::<Value>::new();
        let mut pending_calls = BTreeSet::<String>::new();
        let mut all_call_ids = BTreeSet::<String>::new();
        for message in messages {
            if !matches!(message.role, MessageRole::User | MessageRole::Tool)
                && !message.images.is_empty()
            {
                return Err(AgentError::InvalidRequest(
                    "only user messages and tool results may contain Anthropic images".into(),
                ));
            }
            if !message.images.is_empty() {
                if !self.image_input_enabled {
                    return Err(AgentError::InvalidRequest("Anthropic image input is disabled by the exact model capability contract".into()));
                }
                openide_core::validate_user_images(&message.images).map_err(|error| {
                    AgentError::InvalidRequest(format!("invalid images: {error}"))
                })?;
            }
            let images = message.images.iter().map(|image| json!({"type":"image","source":{"type":"base64","media_type":image.mime_type.as_str(),"data":image.data_base64}}));
            if message.role != MessageRole::Assistant
                && (!message.tool_calls.is_empty() || !message.provider_output_items.is_empty())
            {
                return Err(AgentError::InvalidRequest(
                    "provider items and tool calls require an assistant message".into(),
                ));
            }
            if message.role != MessageRole::Tool && message.tool_call_id.is_some() {
                return Err(AgentError::InvalidRequest(
                    "tool result ID requires a tool message".into(),
                ));
            }
            if message.role != MessageRole::Tool && !pending_calls.is_empty() {
                return Err(AgentError::InvalidRequest(
                    "Anthropic tool results must immediately follow their assistant tool uses"
                        .into(),
                ));
            }
            match message.role {
                MessageRole::System => {
                    if !message.content.is_empty() {
                        system.push(json!({"type":"text","text":message.content}));
                    }
                }
                MessageRole::User => {
                    let mut blocks = Vec::new();
                    if !message.content.is_empty() {
                        blocks.push(json!({"type":"text","text":message.content}));
                    }
                    blocks.extend(images);
                    if blocks.is_empty() {
                        return Err(AgentError::InvalidRequest(
                            "empty Anthropic user message".into(),
                        ));
                    }
                    push_message(&mut history, "user", blocks);
                }
                MessageRole::Assistant => {
                    let blocks = if message.provider_output_items.is_empty() {
                        let mut blocks = Vec::new();
                        if !message.content.is_empty() {
                            blocks.push(json!({"type":"text","text":message.content}));
                        }
                        for call in &message.tool_calls {
                            blocks.push(json!({"type":"tool_use","id":call.id,"name":call.name,"input":call.arguments}));
                        }
                        blocks
                    } else {
                        let blocks = replay_blocks(
                            &message.provider_output_items,
                            &self.model,
                            &endpoint_digest,
                        )?;
                        let output = extract_blocks(blocks)?;
                        if output.content != message.content
                            || output.tool_calls != message.tool_calls
                        {
                            return Err(AgentError::InvalidRequest("Anthropic provider items conflict with canonical assistant history".into()));
                        }
                        blocks.clone()
                    };
                    let output = extract_blocks(&blocks)?;
                    if blocks.is_empty() {
                        return Err(AgentError::InvalidRequest(
                            "empty Anthropic assistant message".into(),
                        ));
                    }
                    for call in output.tool_calls {
                        if !all_call_ids.insert(call.id.clone()) {
                            return Err(AgentError::InvalidRequest(
                                "duplicate Anthropic history tool ID".into(),
                            ));
                        }
                        pending_calls.insert(call.id);
                    }
                    push_message(&mut history, "assistant", blocks);
                }
                MessageRole::Tool => {
                    let id = message.tool_call_id.as_ref().ok_or_else(|| {
                        AgentError::InvalidRequest("Anthropic tool result has no call ID".into())
                    })?;
                    if !pending_calls.remove(id) {
                        return Err(AgentError::InvalidRequest(
                            "orphan or repeated Anthropic tool result".into(),
                        ));
                    }
                    // A result without images keeps its plain string form.
                    // With images, `content` is the list of blocks Messages
                    // accepts inside a tool_result: its text, then each image.
                    let content = if message.images.is_empty() {
                        json!(message.content)
                    } else {
                        let text = (!message.content.is_empty())
                            .then(|| json!({"type":"text","text":message.content}));
                        json!(text.into_iter().chain(images).collect::<Vec<_>>())
                    };
                    push_message(
                        &mut history,
                        "user",
                        vec![json!({"type":"tool_result","tool_use_id":id,"content":content})],
                    );
                }
            }
        }
        if !pending_calls.is_empty() || history.is_empty() || history[0]["role"] != "user" {
            return Err(AgentError::InvalidRequest(
                "Anthropic request needs user input and all tool results".into(),
            ));
        }
        let mut body = json!({"model":self.model,"messages":history});
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if !count {
            body["max_tokens"] = json!(self.max_tokens);
            body["stream"] = json!(stream);
        }
        if !tools.is_empty() {
            let mut names = BTreeSet::new();
            for tool in tools {
                if !valid_name(&tool.name)
                    || !names.insert(&tool.name)
                    || !tool.parameters.is_object()
                    || tool.parameters["type"] != "object"
                {
                    return Err(AgentError::InvalidRequest(
                        "invalid or repeated Anthropic tool definition".into(),
                    ));
                }
            }
            body["tools"] = json!(tools.iter().map(|tool| json!({"name":tool.name,"description":tool.description,"input_schema":tool.parameters})).collect::<Vec<_>>());
        }
        for (key, value) in &self.fields {
            body[key] = value.clone();
        }
        if serde_json::to_vec(&body)
            .map_err(|error| AgentError::InvalidRequest(error.to_string()))?
            .len()
            > MAX_REQUEST_BYTES
        {
            return Err(AgentError::Limit("Anthropic request exceeds 32 MB".into()));
        }
        Ok(body)
    }

    async fn send(
        &self,
        body: &Value,
        count: bool,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, ModelAttemptError> {
        let endpoint = if count {
            format!("{}/count_tokens", self.endpoint)
        } else {
            self.endpoint.clone()
        };
        let mut request = self
            .client
            .post(endpoint)
            .header("anthropic-version", VERSION)
            .json(body);
        if body.get("stream").and_then(Value::as_bool) == Some(true) {
            request = request.header(reqwest::header::ACCEPT, "text/event-stream");
        }
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(key) = &self.api_key {
            request = request.header("x-api-key", key);
        }
        send_model_request(request, cancel).await
    }

    pub async fn complete_detailed(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelCompletion, AgentError> {
        let body = self.request_body(messages, tools, false, false)?;
        let endpoint_digest = self.endpoint_digest()?;
        retry_model_transport(&cancel, None, || async {
            let response = self.send(&body, false, &cancel).await?;
            let bytes = read_model_http_body(response, MAX_MODEL_HTTP_BODY_BYTES, &cancel)
                .await
                .map_err(ModelAttemptError::Terminal)?;
            let value = serde_json::from_slice(&bytes).map_err(|error| {
                ModelAttemptError::Terminal(model_error(format!("invalid Anthropic JSON: {error}")))
            })?;
            parse_message(&value, &self.model, &endpoint_digest)
                .map_err(ModelAttemptError::Terminal)
        })
        .await
    }

    async fn stream_detailed(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelCompletion, AgentError> {
        self.stream_with_delivery(messages, tools, cancel, ModelStreamDelivery::Legacy(deltas))
            .await
    }

    async fn stream_with_delivery(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        delivery: ModelStreamDelivery,
    ) -> Result<ModelCompletion, AgentError> {
        let body = self.request_body(messages, tools, true, false)?;
        let endpoint_digest = self.endpoint_digest()?;
        retry_model_transport(&cancel, Some(&delivery), || async {
            let response = self.send(&body, false, &cancel).await?;
            let status = response.status().as_u16();
            let mime = model_response_mime(response.headers());
            let mut pending = ModelStreamBody::new(response.headers());
            let mut wire = Box::pin(response.bytes_stream());
            let mut assembler = AnthropicStream::default();
            loop {
                let next = tokio::select! {
                    _ = cancel.cancelled() => return Err(ModelAttemptError::Terminal(AgentError::Cancelled)),
                    next = wire.next() => next,
                };
                let Some(chunk) = next else { break; };
                let chunk = chunk.map_err(|error| ModelAttemptError::stream(error, assembler.observed))?;
                if pending.extend(&chunk, MAX_RETAINED_BYTES).is_err() { return Err(ModelAttemptError::Terminal(AgentError::Limit("Anthropic SSE frame exceeds 8 MiB".into()))); }
                while let Some(frame) = pending.take_sse_frame() {
                    let mut events = Vec::new();
                    assembler.ingest_events(&frame, &self.model, &mut events).map_err(|error| ModelAttemptError::classified(error, assembler.observed))?;
                    delivery.publish(events).await.map_err(ModelAttemptError::Terminal)?;
                }
                if assembler.stopped {
                    if pending.bytes().iter().any(|byte| !byte.is_ascii_whitespace()) { return Err(ModelAttemptError::Terminal(model_error("data follows Anthropic message_stop"))); }
                    let ready = assembler.ready_events();
                    let completion = assembler.finish(&self.model, &endpoint_digest).map_err(ModelAttemptError::Terminal)?;
                    delivery.publish(ready).await.map_err(ModelAttemptError::Terminal)?;
                    if let Some(usage) = &completion.usage {
                        delivery.publish(vec![ModelStreamEvent::Usage { usage: usage.clone() }]).await.map_err(ModelAttemptError::Terminal)?;
                    }
                    return Ok(completion);
                }
            }
            if !pending.is_sse() {
                let bytes = pending.bytes();
                let body_kind = model_response_body_kind(bytes);
                let value = serde_json::from_slice(bytes).map_err(|error| ModelAttemptError::Terminal(model_error(format!("invalid Anthropic JSON: {error} (HTTP {status}; MIME {mime}; body {body_kind})"))))?;
                let completion = parse_message(&value, &self.model, &endpoint_digest).map_err(ModelAttemptError::Terminal)?;
                delivery.publish(crate::backend::validated_completion_events(&completion)).await.map_err(ModelAttemptError::Terminal)?;
                return Ok(completion);
            }
            Err(ModelAttemptError::incomplete_stream("Anthropic stream ended without a complete message_stop frame", assembler.observed))
        }).await
    }
}

#[async_trait]
impl ModelBackend for AnthropicMessagesBackend {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn limit_key(&self) -> Option<String> {
        crate::backend::limit_key_for(&self.endpoint, self.api_key.as_deref())
    }
    fn limit_family(&self) -> Option<String> {
        // Anthropic counts its API rate limits per model class.
        let model = self.model.to_ascii_lowercase();
        ["opus", "sonnet", "haiku"]
            .into_iter()
            .find(|class| model.contains(class))
            .map(str::to_owned)
            .or(Some(model))
    }
    fn supports_user_images(&self) -> bool {
        self.image_input_enabled
    }
    fn target_caps(&self) -> crate::TargetCaps {
        crate::TargetCaps {
            vision: self.image_input_enabled,
            tools: true,
            // tool_use.id must match ^[a-zA-Z0-9_-]+$; names are capped at 64.
            tool_call_id: crate::ToolIdRule::Token { max_len: 64 },
            tool_name_max: Some(64),
        }
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some(stable_request_profile_fingerprint(&[
            "anthropic-messages-v1",
            VERSION,
            &self.max_tokens.to_string(),
            &serde_json::to_string(&self.fields).ok()?,
            &serde_json::to_string(&self.headers).ok()?,
            if self.image_input_enabled {
                "images-enabled"
            } else {
                "images-disabled"
            },
        ]))
    }
    async fn count_input_tokens(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<Option<u64>, AgentError> {
        let body = self.request_body(messages, tools, false, true)?;
        retry_model_transport(&cancel, None, || async {
            let response = self.send(&body, true, &cancel).await?;
            let bytes = read_model_http_body(response, MAX_MODEL_HTTP_BODY_BYTES, &cancel)
                .await
                .map_err(ModelAttemptError::Terminal)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                ModelAttemptError::Terminal(model_error(format!(
                    "invalid Anthropic token count: {error}"
                )))
            })?;
            if value.get("error").is_some_and(|error| !error.is_null()) {
                return Err(ModelAttemptError::Terminal(model_error(
                    "Anthropic count_tokens returned an error",
                )));
            }
            let count = value
                .get("input_tokens")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    ModelAttemptError::Terminal(model_error(
                        "missing or invalid Anthropic input_tokens",
                    ))
                })?;
            Ok(Some(count))
        })
        .await
    }
    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        self.complete_detailed(messages, tools, cancel)
            .await
            .map(|completion| completion.output)
    }
    async fn stream_complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelOutput, AgentError> {
        self.stream_detailed(messages, tools, cancel, deltas)
            .await
            .map(|completion| completion.output)
    }
    async fn stream_complete_detailed(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelCompletion, AgentError> {
        self.stream_detailed(messages, tools, cancel, deltas).await
    }

    async fn stream_events(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        events: ModelEventSender,
    ) -> Result<ModelCompletion, AgentError> {
        events
            .while_open(self.stream_with_delivery(
                messages,
                tools,
                cancel,
                ModelStreamDelivery::Events(events.clone()),
            ))
            .await
    }
}

fn validate_fields(fields: &BTreeMap<String, Value>, max_tokens: u64) -> Result<(), AgentError> {
    let invalid =
        || AgentError::InvalidRequest("unsupported or malformed Anthropic request options".into());
    if fields.len() > 2 || serde_json::to_vec(fields).map_err(|_| invalid())?.len() > 4096 {
        return Err(invalid());
    }
    for (field, value) in fields {
        let object = value.as_object().ok_or_else(invalid)?;
        match field.as_str() {
            "thinking" => {
                if object
                    .keys()
                    .any(|key| !["type", "budget_tokens", "display"].contains(&key.as_str()))
                {
                    return Err(invalid());
                }
                match value["type"].as_str() {
                    Some("enabled")
                        if value["budget_tokens"]
                            .as_u64()
                            .is_some_and(|budget| budget >= 1024 && budget < max_tokens) => {}
                    Some("adaptive" | "disabled") if !object.contains_key("budget_tokens") => {}
                    _ => return Err(invalid()),
                }
                if let Some(display) = object.get("display") {
                    if value["type"] == "disabled"
                        || !matches!(display.as_str(), Some("summarized" | "omitted"))
                    {
                        return Err(invalid());
                    }
                }
            }
            "output_config"
                if object.len() == 1
                    && matches!(
                        value["effort"].as_str(),
                        Some("low" | "medium" | "high" | "xhigh" | "max")
                    ) => {}
            _ => return Err(invalid()),
        }
    }
    Ok(())
}

fn push_message(history: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if let Some(previous) = history
        .last_mut()
        .filter(|previous| previous["role"] == role)
    {
        previous["content"]
            .as_array_mut()
            .expect("owned message array")
            .extend(blocks);
    } else {
        history.push(json!({"role":role,"content":blocks}));
    }
}

fn model_error(message: impl Into<String>) -> AgentError {
    AgentError::Model(message.into())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, AgentError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| model_error(format!("missing Anthropic string field {field}")))
}

fn serialized_size(value: &Value) -> Result<usize, AgentError> {
    serde_json::to_vec(value)
        .map(|value| value.len())
        .map_err(|error| model_error(error.to_string()))
}

fn check_size(size: usize) -> Result<(), AgentError> {
    if size > MAX_RETAINED_BYTES {
        Err(AgentError::Limit(
            "Anthropic response exceeds 8 MiB retained bytes".into(),
        ))
    } else {
        Ok(())
    }
}

fn extract_blocks(blocks: &[Value]) -> Result<ModelOutput, AgentError> {
    if blocks.len() > MAX_BLOCKS {
        return Err(AgentError::Limit(
            "Anthropic response has too many content blocks".into(),
        ));
    }
    let mut retained = 0_usize;
    let mut content = String::new();
    let mut calls = Vec::new();
    let mut ids = BTreeSet::new();
    for block in blocks {
        retained = retained
            .checked_add(serialized_size(block)?)
            .ok_or_else(|| AgentError::Limit("Anthropic response size overflow".into()))?;
        check_size(retained)?;
        match string(block, "type")? {
            "text" => content.push_str(string(block, "text")?),
            "thinking" => {
                string(block, "thinking")?;
                if string(block, "signature")?.is_empty() {
                    return Err(model_error("Anthropic thinking block has no signature"));
                }
            }
            "redacted_thinking" => {
                if string(block, "data")?.is_empty() {
                    return Err(model_error("empty redacted Anthropic thinking"));
                }
            }
            "tool_use" => {
                if calls.len() == MAX_MODEL_TOOL_CALLS {
                    return Err(AgentError::Limit(
                        "Anthropic response exceeds 64 tool calls".into(),
                    ));
                }
                let id = string(block, "id")?;
                let name = string(block, "name")?;
                let arguments = block
                    .get("input")
                    .filter(|input| input.is_object())
                    .ok_or_else(|| model_error("Anthropic tool input is not an object"))?;
                if id.is_empty()
                    || id.len() > 512
                    || id.chars().any(char::is_control)
                    || !ids.insert(id)
                    || !valid_name(name)
                {
                    return Err(model_error("invalid or duplicate Anthropic tool identity"));
                }
                calls.push(ToolCall {
                    id: id.into(),
                    name: name.into(),
                    arguments: arguments.clone(),
                });
            }
            kind => {
                return Err(model_error(format!(
                    "unsupported Anthropic content block: {kind}"
                )))
            }
        }
    }
    Ok(ModelOutput {
        content,
        tool_calls: calls,
    })
}

fn replay_blocks<'a>(
    items: &'a [Value],
    model: &str,
    endpoint_digest: &str,
) -> Result<&'a Vec<Value>, AgentError> {
    let invalid = || {
        AgentError::InvalidRequest("foreign or invalid Anthropic provider replay envelope".into())
    };
    let [item] = items else {
        return Err(invalid());
    };
    if item["type"] != "openide_anthropic_message_v1"
        || item["model_id"] != model
        || item["endpoint_digest"] != endpoint_digest
        || item.as_object().is_none_or(|object| {
            object.keys().any(|key| {
                ![
                    "type",
                    "model_id",
                    "endpoint_digest",
                    "message_id",
                    "content",
                ]
                .contains(&key.as_str())
            })
        })
    {
        return Err(invalid());
    }
    let id = item
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return Err(invalid());
    }
    item.get("content")
        .and_then(Value::as_array)
        .ok_or_else(invalid)
}

fn parse_usage(usage: &Value) -> Result<ModelTokenUsage, AgentError> {
    let get = |field: &str| {
        usage
            .get(field)
            .and_then(Value::as_u64)
            .ok_or_else(|| model_error(format!("missing or invalid Anthropic usage {field}")))
    };
    let optional = |field: &str| -> Result<Option<u64>, AgentError> {
        usage
            .get(field)
            .map(|value| {
                value
                    .as_u64()
                    .ok_or_else(|| model_error(format!("invalid Anthropic usage {field}")))
            })
            .transpose()
    };
    let ordinary = get("input_tokens")?;
    let output_tokens = get("output_tokens")?;
    let cached_input_tokens = optional("cache_read_input_tokens")?;
    let created = optional("cache_creation_input_tokens")?.unwrap_or(0);
    let input_tokens = ordinary
        .checked_add(cached_input_tokens.unwrap_or(0))
        .and_then(|value| value.checked_add(created))
        .ok_or_else(|| model_error("Anthropic input token count overflow"))?;
    let total_tokens = input_tokens
        .checked_add(output_tokens)
        .ok_or_else(|| model_error("Anthropic token count overflow"))?;
    let reasoning_tokens = usage
        .get("output_tokens_details")
        .map(|details| {
            details
                .get("thinking_tokens")
                .and_then(Value::as_u64)
                .ok_or_else(|| model_error("invalid Anthropic thinking_tokens"))
        })
        .transpose()?;
    if reasoning_tokens.is_some_and(|tokens| tokens > output_tokens) {
        return Err(model_error(
            "Anthropic thinking tokens exceed output tokens",
        ));
    }
    Ok(ModelTokenUsage {
        input_tokens,
        output_tokens,
        total_tokens,
        cached_input_tokens,
        reasoning_tokens,
    })
}

fn parse_message(
    value: &Value,
    model: &str,
    endpoint_digest: &str,
) -> Result<ModelCompletion, AgentError> {
    check_size(serialized_size(value)?)?;
    if value.get("error").is_some_and(|error| !error.is_null())
        || value
            .get("stop_details")
            .is_some_and(|details| !details.is_null())
    {
        return Err(model_error(
            "Anthropic returned an error or unsupported stop details",
        ));
    }
    if string(value, "type")? != "message"
        || string(value, "role")? != "assistant"
        || string(value, "model")? != model
    {
        return Err(model_error(
            "Anthropic response model, type or role mismatch",
        ));
    }
    let id = string(value, "id")?;
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return Err(model_error("invalid Anthropic response ID"));
    }
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| model_error("missing Anthropic content array"))?;
    let mut output = extract_blocks(blocks)?;
    let truncated = string(value, "stop_reason")? == "max_tokens";
    match string(value, "stop_reason")? {
        "end_turn" if output.tool_calls.is_empty() => {}
        "tool_use" if !output.tool_calls.is_empty() => {}
        // A call cut by the limit has no usable input: keep the text only.
        "max_tokens" => output.tool_calls.clear(),
        reason => {
            return Err(model_error(format!(
                "unsafe or inconsistent Anthropic stop reason: {reason}"
            )))
        }
    }
    if value
        .get("stop_sequence")
        .is_some_and(|sequence| !sequence.is_null())
    {
        return Err(model_error("unsupported Anthropic stop_sequence"));
    }
    let usage = parse_usage(
        value
            .get("usage")
            .ok_or_else(|| model_error("missing Anthropic usage"))?,
    )?;
    Ok(ModelCompletion {
        output,
        response_id: Some(id.into()),
        usage: Some(usage),
        // The blocks of a cut response may hold half a tool use: not replayed.
        provider_output_items: if truncated {
            Vec::new()
        } else {
            vec![
                json!({"type":"openide_anthropic_message_v1","model_id":model,"endpoint_digest":endpoint_digest,"message_id":id,"content":blocks}),
            ]
        },
        truncated,
    })
}

#[derive(Default)]
struct AnthropicStream {
    message: Option<Value>,
    blocks: Vec<Value>,
    active: Option<usize>,
    partial_json: String,
    cut_tool_json: Option<String>,
    retained: usize,
    observed: bool,
    delta_seen: bool,
    stopped: bool,
    events: usize,
}

impl AnthropicStream {
    #[cfg(test)]
    fn ingest(
        &mut self,
        frame: &[u8],
        model: &str,
        deltas: &UnboundedSender<String>,
    ) -> Result<(), AgentError> {
        let mut events = Vec::new();
        let result = self.ingest_events(frame, model, &mut events);
        publish_legacy(events, deltas);
        result
    }

    fn ingest_events(
        &mut self,
        frame: &[u8],
        model: &str,
        events: &mut Vec<ModelStreamEvent>,
    ) -> Result<(), AgentError> {
        let frame = std::str::from_utf8(frame)
            .map_err(|_| model_error("invalid UTF-8 in Anthropic SSE frame"))?;
        let mut name = None;
        let mut data = Vec::new();
        for line in sse_lines(frame) {
            if let Some(value) = line.strip_prefix("event:") {
                if name.replace(value.trim()).is_some() {
                    return Err(model_error("duplicate Anthropic SSE event name"));
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                data.push(value.strip_prefix(' ').unwrap_or(value));
            } else if !line.is_empty() && !line.starts_with(':') {
                return Err(model_error("unsupported Anthropic SSE field"));
            }
        }
        if data.is_empty() {
            return if name.is_none() {
                Ok(())
            } else {
                Err(model_error("Anthropic SSE event has no data"))
            };
        }
        self.events += 1;
        if self.events > MAX_STREAM_EVENTS {
            return Err(AgentError::Limit(
                "Anthropic SSE event limit exceeded".into(),
            ));
        }
        let event: Value = serde_json::from_str(&data.join("\n"))
            .map_err(|error| model_error(format!("invalid Anthropic SSE JSON: {error}")))?;
        let kind = string(&event, "type")?;
        if name != Some(kind) {
            return Err(model_error("Anthropic SSE event name and type mismatch"));
        }
        if self.stopped {
            return Err(model_error("Anthropic event follows message_stop"));
        }
        match kind {
            "ping" => Ok(()),
            "error" => Err(AgentError::Provider(crate::ProviderFailure::from_body(
                &event,
                format!(
                    "Anthropic stream error: {}",
                    event
                        .get("error")
                        .map(Value::to_string)
                        .unwrap_or_default()
                        .chars()
                        .take(500)
                        .collect::<String>()
                ),
            ))),
            "message_start" => {
                if self.message.is_some() {
                    return Err(model_error("duplicate Anthropic message_start"));
                }
                let message = event
                    .get("message")
                    .ok_or_else(|| model_error("missing Anthropic start message"))?;
                if string(message, "type")? != "message"
                    || string(message, "role")? != "assistant"
                    || string(message, "model")? != model
                    || !message["content"].as_array().is_some_and(Vec::is_empty)
                    || message
                        .get("stop_reason")
                        .is_none_or(|reason| !reason.is_null())
                    || message
                        .get("stop_sequence")
                        .is_none_or(|sequence| !sequence.is_null())
                {
                    return Err(model_error("invalid or mismatched Anthropic message_start"));
                }
                let id = string(message, "id")?;
                if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
                    return Err(model_error("invalid Anthropic message ID"));
                }
                parse_usage(&message["usage"])?;
                self.retained = serialized_size(message)?;
                check_size(self.retained)?;
                self.message = Some(message.clone());
                events.push(ModelStreamEvent::Started {
                    response_id: Some(id.to_owned()),
                });
                Ok(())
            }
            "content_block_start" => {
                if self.message.is_none()
                    || self.active.is_some()
                    || self.delta_seen
                    || self.blocks.len() >= MAX_BLOCKS
                {
                    return Err(model_error("out-of-order Anthropic content_block_start"));
                }
                let index = self.index(&event)?;
                if index != self.blocks.len() {
                    return Err(model_error("Anthropic block index is not contiguous"));
                }
                let block = event
                    .get("content_block")
                    .ok_or_else(|| model_error("missing Anthropic content block"))?;
                match string(block, "type")? {
                    "text" => {
                        string(block, "text")?;
                    }
                    "thinking" => {
                        string(block, "thinking")?;
                        string(block, "signature")?;
                    }
                    "redacted_thinking" => {
                        extract_blocks(std::slice::from_ref(block))?;
                    }
                    "tool_use" => {
                        extract_blocks(std::slice::from_ref(block))?;
                        let id = string(block, "id")?;
                        if self
                            .blocks
                            .iter()
                            .any(|old| old["type"] == "tool_use" && old["id"] == id)
                        {
                            return Err(model_error("duplicate Anthropic tool ID"));
                        }
                        if self
                            .blocks
                            .iter()
                            .filter(|block| block["type"] == "tool_use")
                            .count()
                            == MAX_MODEL_TOOL_CALLS
                        {
                            return Err(AgentError::Limit(
                                "Anthropic response exceeds 64 tool calls".into(),
                            ));
                        }
                    }
                    kind => {
                        return Err(model_error(format!(
                            "unsupported Anthropic content block: {kind}"
                        )))
                    }
                }
                self.grow(serialized_size(block)?)?;
                self.blocks.push(block.clone());
                self.active = Some(index);
                self.partial_json.clear();
                match string(block, "type")? {
                    "text" => self.publish(string(block, "text")?, events),
                    "thinking" => {
                        let content = string(block, "thinking")?;
                        self.observed |=
                            !content.is_empty() || !string(block, "signature")?.is_empty();
                        if !content.is_empty() {
                            events.push(ModelStreamEvent::ReasoningDelta {
                                item_id: format!("anthropic-block-{index}"),
                                content: content.to_owned(),
                            });
                        }
                    }
                    "tool_use" => {
                        self.observed = true;
                        let input = &block["input"];
                        events.push(ModelStreamEvent::ToolCallDelta {
                            item_id: format!("anthropic-block-{index}"),
                            call_id: Some(string(block, "id")?.to_owned()),
                            name: Some(string(block, "name")?.to_owned()),
                            arguments_delta: if input
                                .as_object()
                                .is_some_and(serde_json::Map::is_empty)
                            {
                                String::new()
                            } else {
                                input.to_string()
                            },
                        });
                    }
                    _ => self.observed = true,
                }
                Ok(())
            }
            "content_block_delta" => {
                let index = self.index(&event)?;
                if self.active != Some(index) || self.delta_seen {
                    return Err(model_error("Anthropic delta has no matching active block"));
                }
                let delta = event
                    .get("delta")
                    .ok_or_else(|| model_error("missing Anthropic content delta"))?;
                let block_kind = string(&self.blocks[index], "type")?;
                let (field, fragment) = match (block_kind, string(delta, "type")?) {
                    ("text", "text_delta") => ("text", string(delta, "text")?),
                    ("thinking", "thinking_delta")
                        if self.blocks[index]["signature"]
                            .as_str()
                            .is_some_and(str::is_empty) =>
                    {
                        ("thinking", string(delta, "thinking")?)
                    }
                    ("thinking", "signature_delta") => ("signature", string(delta, "signature")?),
                    ("tool_use", "input_json_delta") => ("input", string(delta, "partial_json")?),
                    _ => {
                        return Err(model_error(
                            "unsupported or mismatched Anthropic block delta",
                        ))
                    }
                };
                if field == "input" {
                    if !self.blocks[index]["input"]
                        .as_object()
                        .is_some_and(serde_json::Map::is_empty)
                    {
                        return Err(model_error(
                            "Anthropic streamed input conflicts with initial input",
                        ));
                    }
                    self.grow(fragment.len())?;
                    self.partial_json.push_str(fragment);
                    if !fragment.is_empty() {
                        events.push(ModelStreamEvent::ToolCallDelta {
                            item_id: format!("anthropic-block-{index}"),
                            call_id: Some(string(&self.blocks[index], "id")?.to_owned()),
                            name: Some(string(&self.blocks[index], "name")?.to_owned()),
                            arguments_delta: fragment.to_owned(),
                        });
                    }
                } else {
                    let encoded = serde_json::to_vec(fragment)
                        .map_err(|error| model_error(error.to_string()))?
                        .len()
                        .saturating_sub(2);
                    self.grow(encoded)?;
                    if let Value::String(target) = &mut self.blocks[index][field] {
                        target.push_str(fragment);
                    } else {
                        unreachable!("validated string field");
                    }
                    if field == "text" {
                        self.publish(fragment, events);
                    } else if field == "thinking" && !fragment.is_empty() {
                        // Public thinking text only; signature and redacted
                        // thinking payloads never enter presentation events.
                        events.push(ModelStreamEvent::ReasoningDelta {
                            item_id: format!("anthropic-block-{index}"),
                            content: fragment.to_owned(),
                        });
                    }
                }
                self.observed |= !fragment.is_empty();
                Ok(())
            }
            "content_block_stop" => {
                let index = self.index(&event)?;
                if self.active != Some(index) {
                    return Err(model_error("out-of-order Anthropic block stop"));
                }
                if self.blocks[index]["type"] == "tool_use" && !self.partial_json.is_empty() {
                    let input: Value = match serde_json::from_str(&self.partial_json) {
                        Ok(input) => input,
                        // Only a max_tokens stop excuses this; `finish` checks.
                        Err(error) => {
                            self.cut_tool_json = Some(error.to_string());
                            json!({})
                        }
                    };
                    if !input.is_object() {
                        return Err(model_error(
                            "completed Anthropic tool input is not an object",
                        ));
                    }
                    let old_size = serialized_size(&self.blocks[index])?;
                    let mut candidate = self.blocks[index].clone();
                    candidate["input"] = input;
                    let retained = self.retained - old_size - self.partial_json.len()
                        + serialized_size(&candidate)?;
                    check_size(retained)?;
                    self.retained = retained;
                    self.blocks[index] = candidate;
                }
                extract_blocks(std::slice::from_ref(&self.blocks[index]))?;
                self.partial_json.clear();
                self.active = None;
                Ok(())
            }
            "message_delta" => {
                if self.message.is_none() || self.active.is_some() {
                    return Err(model_error("out-of-order Anthropic message_delta"));
                }
                let delta = event
                    .get("delta")
                    .and_then(Value::as_object)
                    .ok_or_else(|| model_error("missing Anthropic message delta"))?;
                if delta.keys().any(|key| {
                    !["stop_reason", "stop_sequence", "stop_details"].contains(&key.as_str())
                }) {
                    return Err(model_error("unsupported Anthropic message delta field"));
                }
                let mut candidate = self.message.as_ref().expect("validated message").clone();
                for (key, value) in delta {
                    if !candidate[key].is_null() && candidate[key] != *value {
                        return Err(model_error(
                            "Anthropic message delta changed terminal metadata",
                        ));
                    }
                    candidate[key] = value.clone();
                }
                let incoming = event
                    .get("usage")
                    .and_then(Value::as_object)
                    .ok_or_else(|| model_error("missing Anthropic delta usage"))?;
                if incoming
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .is_none()
                {
                    return Err(model_error("missing Anthropic cumulative output_tokens"));
                }
                for (key, value) in incoming {
                    if [
                        "input_tokens",
                        "output_tokens",
                        "cache_read_input_tokens",
                        "cache_creation_input_tokens",
                    ]
                    .contains(&key.as_str())
                        && (value.as_u64().is_none()
                            || candidate["usage"][key]
                                .as_u64()
                                .is_some_and(|old| value.as_u64().is_some_and(|new| new < old)))
                    {
                        return Err(model_error("invalid decreasing Anthropic cumulative usage"));
                    }
                    candidate["usage"][key] = value.clone();
                }
                parse_usage(&candidate["usage"])?;
                let retained = self.retained
                    - serialized_size(self.message.as_ref().expect("validated message"))?
                    + serialized_size(&candidate)?;
                check_size(retained)?;
                self.retained = retained;
                self.message = Some(candidate);
                self.delta_seen = true;
                Ok(())
            }
            "message_stop" => {
                if self.message.is_none() || self.active.is_some() || !self.delta_seen {
                    return Err(model_error("out-of-order Anthropic message_stop"));
                }
                self.stopped = true;
                Ok(())
            }
            kind => Err(model_error(format!(
                "unsupported Anthropic SSE event: {kind}"
            ))),
        }
    }

    fn index(&self, event: &Value) -> Result<usize, AgentError> {
        event
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| model_error("invalid Anthropic block index"))
    }
    fn grow(&mut self, bytes: usize) -> Result<(), AgentError> {
        let size = self
            .retained
            .checked_add(bytes)
            .ok_or_else(|| AgentError::Limit("Anthropic response size overflow".into()))?;
        check_size(size)?;
        self.retained = size;
        Ok(())
    }
    fn publish(&mut self, text: &str, events: &mut Vec<ModelStreamEvent>) {
        if !text.is_empty() {
            self.observed = true;
            events.push(ModelStreamEvent::TextDelta {
                content: text.into(),
            });
        }
    }
    fn ready_events(&self) -> Vec<ModelStreamEvent> {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block["type"] == "tool_use")
            .map(|(index, block)| ModelStreamEvent::ToolCallReady {
                item_id: format!("anthropic-block-{index}"),
                call_id: block["id"].as_str().unwrap_or_default().to_owned(),
                name: block["name"].as_str().unwrap_or_default().to_owned(),
            })
            .collect()
    }
    fn finish(mut self, model: &str, endpoint_digest: &str) -> Result<ModelCompletion, AgentError> {
        if !self.stopped {
            return Err(model_error("missing Anthropic message_stop"));
        }
        let mut message = self
            .message
            .take()
            .ok_or_else(|| model_error("missing Anthropic message_start"))?;
        message["content"] = json!(self.blocks);
        if let Some(error) = self.cut_tool_json.filter(|_| message["stop_reason"] != "max_tokens") {
            return Err(model_error(format!("invalid completed Anthropic tool JSON: {error}")));
        }
        parse_message(&message, model, endpoint_digest)
    }
}

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod tests;

#[cfg(test)]
mod sse_line_ending_tests {
    use super::*;

    #[tokio::test]
    async fn sse_anthropic_missing_or_wrong_mime_uses_stream_validation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for mime in [None, Some("application/json")] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 1024];
                loop {
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&request[..end])
                            .unwrap()
                            .to_ascii_lowercase();
                        assert!(headers
                            .lines()
                            .any(|line| line == "accept: text/event-stream"));
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse::<usize>()
                            .unwrap();
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let body = wire("\r", "\r\r");
                let mime = mime
                    .map(|value| format!("Content-Type: {value}\r\n"))
                    .unwrap_or_default();
                let headers = format!(
                    "HTTP/1.1 200 OK\r\n{mime}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(headers.as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
            });
            let backend = AnthropicMessagesBackend::new(endpoint, None, "fixture");
            let (sender, _) = tokio::sync::mpsc::unbounded_channel();
            let completion = backend
                .stream_detailed(
                    &[AgentMessage::user("Hello")],
                    &[],
                    CancellationToken::new(),
                    sender,
                )
                .await
                .unwrap();
            assert_eq!(completion.output.content, "Café 中🦀");
            assert_eq!(completion.usage.unwrap().output_tokens, 5);
            server.await.unwrap();
        }
    }

    fn wire(line: &str, end: &str) -> String {
        let values = [
            json!({"type":"message_start","message":{"type":"message","id":"msg_lines","role":"assistant","model":"fixture","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":20,"output_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Café 中🦀"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}),
            json!({"type":"message_stop"}),
        ];
        values
            .iter()
            .map(|value| {
                format!(
                    "event: {}{line}data: {value}{end}",
                    value["type"].as_str().unwrap()
                )
            })
            .collect()
    }

    #[test]
    fn anthropic_accepts_cr_lf_crlf_and_mixed_multiline_frames_with_split_utf8() {
        let endpoint =
            openide_core::native_model_endpoint_digest("http://localhost/v1/messages").unwrap();
        for (line, end) in [
            ("\r", "\r\r"),
            ("\n", "\n\n"),
            ("\r\n", "\r\n\r\n"),
            ("\r", "\n\r"),
            ("\n", "\r\n\n"),
        ] {
            let wire = wire(line, end);
            for chunk_size in [1, 2, 3, 7, wire.len()] {
                let mut pending = SseFrameBuffer::default();
                let mut assembler = AnthropicStream::default();
                let mut events = Vec::new();
                for chunk in wire.as_bytes().chunks(chunk_size) {
                    pending.extend(chunk, MAX_RETAINED_BYTES).unwrap();
                    while let Some(frame) = pending.take_sse_frame() {
                        assembler
                            .ingest_events(&frame, "fixture", &mut events)
                            .unwrap();
                    }
                }
                let completion = assembler.finish("fixture", &endpoint).unwrap();
                assert_eq!(completion.output.content, "Café 中🦀");
                assert_eq!(completion.usage.unwrap().output_tokens, 5);
                assert!(pending.bytes().is_empty());
            }
        }
    }

    #[test]
    fn anthropic_message_stop_at_undelimited_eof_never_finishes() {
        let mut wire = wire("\r", "\r\r");
        wire.truncate(wire.len() - 2);
        let mut pending = SseFrameBuffer::default();
        let mut assembler = AnthropicStream::default();
        for chunk in wire.as_bytes().chunks(1) {
            pending.extend(chunk, MAX_RETAINED_BYTES).unwrap();
            while let Some(frame) = pending.take_sse_frame() {
                assembler
                    .ingest_events(&frame, "fixture", &mut Vec::new())
                    .unwrap();
            }
        }
        assert!(!assembler.stopped);
        assert!(assembler
            .finish("fixture", "unused")
            .unwrap_err()
            .to_string()
            .contains("missing Anthropic message_stop"));
    }
}
