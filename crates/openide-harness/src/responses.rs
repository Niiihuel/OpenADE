//! OpenAI Responses transport for the native agent.
//!
//! The public `ModelBackend` interface carries text and local function calls.
//! `ResponsesResult` also exposes the response ID, token usage, and original
//! output items to callers that can persist the richer Responses transcript.
//! The native runner commits those provider items before tool effects.

use crate::backend::{
    model_http_client_builder, model_response_body_kind, model_response_mime, read_model_http_body,
    retry_model_transport, send_model_request, sse_lines, stable_request_profile_fingerprint,
    ModelAttemptError, ModelStreamBody, MAX_MODEL_TOOL_CALLS,
};
#[cfg(test)]
use crate::model_stream::publish_legacy;
use crate::model_stream::ModelStreamDelivery;
use crate::{
    AgentError, ModelBackend, ModelCompletion, ModelEventSender, ModelOutput, ToolDefinition,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use openide_core::{AgentMessage, MessageRole, ModelStreamEvent, ToolCall};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

#[path = "hosted_web_search.rs"]
mod hosted_web_search;
#[path = "responses_web_search.rs"]
mod web_search_stream;
pub use hosted_web_search::{
    hosted_web_citations, hosted_web_search_calls, HostedWebCitation, HostedWebSearchAction,
    HostedWebSearchCall, HostedWebSearchConfig, HostedWebSearchContextSize,
    HostedWebSearchLocation, HostedWebSearchMode, HostedWebSource,
};
pub use web_search_stream::hosted_web_search_presentation;

const MAX_STREAM_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_ASSEMBLED_BYTES: usize = 8 * 1024 * 1024;
const MAX_STREAM_OUTPUT_ITEMS: usize = 4096;
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

impl From<ResponsesResult> for ModelCompletion {
    fn from(result: ResponsesResult) -> Self {
        Self {
            output: result.output,
            response_id: Some(result.response_id),
            usage: result.usage,
            provider_output_items: result.output_items,
            truncated: false,
        }
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
    max_output_tokens: Option<u64>,
    web_search: Option<HostedWebSearchConfig>,
    chatgpt_plan: bool,
    image_input_enabled: bool,
    bearer_token_provider: Option<Arc<dyn Fn() -> Result<String, String> + Send + Sync>>,
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
            max_output_tokens: None,
            web_search: None,
            chatgpt_plan: false,
            image_input_enabled: true,
            bearer_token_provider: None,
            client: model_http_client_builder()
                .build()
                .expect("reqwest client configuration is valid"),
            replay: Mutex::new(Vec::new()),
        }
    }

    /// A model that cannot see images gets a text placeholder for each one.
    pub fn with_image_input_enabled(mut self, enabled: bool) -> Self {
        self.image_input_enabled = enabled;
        self
    }

    /// Override the default idle timeout with an explicit total request limit.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client configuration is valid");
        self
    }

    /// Public Responses API with Sign in with ChatGPT plan permission. This
    /// route requires streaming, developer instructions and namespaced tools.
    pub fn with_chatgpt_plan(mut self) -> Self {
        self.chatgpt_plan = true;
        self
    }

    /// Fetch the current OAuth credential before every model step. The host
    /// owns refresh and protected storage; the harness never persists tokens.
    pub fn with_bearer_token_provider(
        mut self,
        provider: impl Fn() -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        self.bearer_token_provider = Some(Arc::new(provider));
        self.api_key = None;
        self
    }

    /// Validates a bounded wire value. The caller must authorize this value
    /// against the exact provider/model route before constructing the backend.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Result<Self, AgentError> {
        let effort = effort.into();
        if effort.is_empty()
            || effort.len() > 64
            || effort.chars().any(char::is_whitespace)
            || effort.chars().any(char::is_control)
        {
            return Err(AgentError::InvalidRequest(
                "invalid Responses reasoning effort value".into(),
            ));
        }
        self.reasoning_effort = Some(effort);
        Ok(self)
    }

    /// Limit all generated tokens, including reasoning. The host must validate
    /// this cap against the exact model's published output limit when known.
    pub fn with_max_output_tokens(mut self, limit: u64) -> Result<Self, AgentError> {
        if !(16..=i64::MAX as u64).contains(&limit) {
            return Err(AgentError::InvalidRequest(
                "Responses output token limit must be an integer of at least 16".into(),
            ));
        }
        self.max_output_tokens = Some(limit);
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
            if !message.images.is_empty()
                && !matches!(message.role, MessageRole::User | MessageRole::Tool)
            {
                return Err(AgentError::InvalidRequest(
                    "only user messages and tool results may contain image inputs".into(),
                ));
            }
            // The text of a message with images, then each image as a data URL.
            let with_images = || -> Result<Vec<Value>, AgentError> {
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
                Ok(content)
            };
            match message.role {
                MessageRole::System => {
                    input.push(json!({"role": if self.chatgpt_plan { "developer" } else { "system" }, "content": message.content}));
                }
                MessageRole::User if message.images.is_empty() => {
                    input.push(json!({"role": "user", "content": message.content}));
                }
                MessageRole::User => {
                    input.push(json!({"role": "user", "content": with_images()?}));
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
                            let mut item = json!({
                                "type": "function_call",
                                "call_id": call.id,
                                "name": call.name,
                                "arguments": call.arguments.to_string(),
                            });
                            if self.chatgpt_plan {
                                item["namespace"] = json!("openide");
                            }
                            input.push(item);
                        }
                    }
                }
                MessageRole::Tool => {
                    let call_id = message.tool_call_id.as_deref().ok_or_else(|| {
                        AgentError::InvalidRequest("tool result has no call ID".into())
                    })?;
                    // A result without images keeps its plain string form.
                    // With images, `output` is the list of content items
                    // Responses accepts there: input_text, then input_image.
                    let output = if message.images.is_empty() {
                        json!(message.content)
                    } else {
                        json!(with_images()?)
                    };
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
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
        if self.chatgpt_plan && self.web_search.is_some() {
            return Err(AgentError::InvalidRequest(
                "Hosted search is not configured for this ChatGPT plan route".into(),
            ));
        }
        if let Some(config) = &self.web_search {
            tool_items.push(config.wire_tool());
        }
        let mut body = json!({
            "model": self.model,
            "input": input,
            "stream": stream || self.chatgpt_plan,
            "store": false,
        });
        if !tool_items.is_empty() {
            if self.chatgpt_plan {
                body["tools"] = json!([{"type":"namespace", "name":"openide", "description":"Herramientas locales de OpenIDE", "tools":tool_items}]);
            } else {
                body["tools"] = Value::Array(tool_items);
            }
        }
        if let Some(limit) = self.max_output_tokens {
            if self.chatgpt_plan {
                return Err(AgentError::InvalidRequest(
                    "ChatGPT plan usage does not accept max_output_tokens".into(),
                ));
            }
            body["max_output_tokens"] = json!(limit);
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
        fields.remove("max_output_tokens");
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
        retry_model_transport(&cancel, None, || async {
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
        if stream {
            request = request.header(reqwest::header::ACCEPT, "text/event-stream");
        }
        if let Some(provider) = &self.bearer_token_provider {
            let provider = provider.clone();
            let task = tokio::task::spawn_blocking(move || provider());
            let key = tokio::select! {
                _ = cancel.cancelled() => return Err(ModelAttemptError::Terminal(AgentError::Cancelled)),
                result = task => result.map_err(|_| ModelAttemptError::Terminal(AgentError::Model("No se pudo renovar la conexión".into())))?
                    .map_err(|error| ModelAttemptError::Terminal(AgentError::Model(error)))?,
            };
            request = request.bearer_auth(key);
        } else if let Some(key) = &self.api_key {
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
        if self.chatgpt_plan {
            let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
            return self
                .stream_complete_detailed(messages, tools, cancel, sender)
                .await;
        }
        let result = retry_model_transport(&cancel, None, || async {
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
        self.stream_with_delivery(messages, tools, cancel, ModelStreamDelivery::Legacy(deltas))
            .await
    }

    async fn stream_with_delivery(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        delivery: ModelStreamDelivery,
    ) -> Result<ResponsesResult, AgentError> {
        let result = retry_model_transport(&cancel, Some(&delivery), || async {
            let response = self.send_once(messages, tools, true, &cancel).await?;
            let status = response.status().as_u16();
            let mime = model_response_mime(response.headers());
            let mut pending = ModelStreamBody::new(response.headers());
            // A hosted search can execute remotely before any text is emitted.
            // Once the provider accepted an SSE request, a disconnect is
            // ambiguous and repeating it could execute the search twice.
            let hosted_search_may_have_run = self.web_search.is_some();
            let mut stream = Box::pin(response.bytes_stream());
            let mut assembler = ResponseStream::default();
            loop {
                let next = tokio::select! {
                    _ = cancel.cancelled() => return Err(ModelAttemptError::Terminal(AgentError::Cancelled)),
                    next = stream.next() => next,
                };
                let Some(next) = next else { break };
                let chunk = next.map_err(|error| ModelAttemptError::stream(error, assembler.published_activity || hosted_search_may_have_run))?;
                if pending.extend(&chunk, MAX_STREAM_FRAME_BYTES).is_err() {
                    return Err(ModelAttemptError::Terminal(AgentError::Limit(
                        "Responses stream frame is too large".into(),
                    )));
                }
                while let Some(frame) = pending.take_sse_frame() {
                    let mut events = Vec::new();
                    assembler.ingest_events(&frame, &mut events).map_err(|error| ModelAttemptError::classified(error, assembler.published_activity || hosted_search_may_have_run))?;
                    delivery.publish(events).await.map_err(ModelAttemptError::Terminal)?;
                    if assembler.completed.is_some() {
                        let published_text = assembler.published_delta;
                        let published_calls = assembler.calls.values().map(|call| call.item_id.clone()).collect();
                        let result = assembler.finish().map_err(ModelAttemptError::Terminal)?;
                        delivery.publish(response_ready_events(&result, published_text, &published_calls)).await.map_err(ModelAttemptError::Terminal)?;
                        return Ok(result);
                    }
                }
            }
            if !pending.is_sse() {
                // JSON may contain blank lines. Format detection keeps it
                // outside the SSE framer, and it is validated only after EOF.
                let bytes = pending.bytes();
                let body_kind = model_response_body_kind(bytes);
                let body: Value = serde_json::from_slice(bytes).map_err(|error| {
                    ModelAttemptError::Terminal(AgentError::Model(format!("invalid Responses JSON: {error} (HTTP {status}; MIME {mime}; body {body_kind})")))
                })?;
                let result = parse_response(&body).map_err(ModelAttemptError::Terminal)?;
                let completion: ModelCompletion = result.clone().into();
                delivery.publish(crate::backend::validated_completion_events(&completion)).await.map_err(ModelAttemptError::Terminal)?;
                return Ok(result);
            }
            // SSE requires the final blank line. Even a complete JSON payload
            // at EOF is an undelimited event, not an authoritative completion.
            Err(ModelAttemptError::incomplete_stream(
                if pending.bytes().is_empty() {
                    "Responses stream ended before response.completed"
                } else {
                    "Responses stream ended inside an SSE frame"
                },
                assembler.published_activity || hosted_search_may_have_run,
            ))
        })
        .await?;
        self.remember(&result)?;
        Ok(result)
    }
}

fn response_ready_events(
    result: &ResponsesResult,
    published_text: bool,
    published_calls: &HashSet<String>,
) -> Vec<ModelStreamEvent> {
    let mut events = Vec::new();
    events.extend(
        result
            .output_items
            .iter()
            .filter_map(hosted_web_search_presentation),
    );
    // Some compatible gateways send only a completed snapshot. Present its
    // validated content once, preserving the same tool preview as live deltas.
    if !published_text && !result.output.content.is_empty() {
        events.push(ModelStreamEvent::TextDelta {
            content: result.output.content.clone(),
        });
    }
    for item in &result.output_items {
        if item.get("type").and_then(Value::as_str) == Some("function_call") {
            let item_id = item["id"].as_str().unwrap_or_default();
            if !published_calls.contains(item_id) {
                events.push(ModelStreamEvent::ToolCallDelta {
                    item_id: item_id.to_owned(),
                    call_id: item["call_id"].as_str().map(str::to_owned),
                    name: item["name"].as_str().map(str::to_owned),
                    arguments_delta: item["arguments"].as_str().unwrap_or_default().to_owned(),
                });
            }
            events.push(ModelStreamEvent::ToolCallReady {
                item_id: item["id"].as_str().unwrap_or_default().to_owned(),
                call_id: item["call_id"].as_str().unwrap_or_default().to_owned(),
                name: item["name"].as_str().unwrap_or_default().to_owned(),
            });
        }
    }
    if let Some(usage) = &result.usage {
        events.push(ModelStreamEvent::Usage {
            usage: usage.clone(),
        });
    }
    events
}

#[async_trait]
impl ModelBackend for ResponsesBackend {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn limit_key(&self) -> Option<String> {
        // A subscription token rotates; its endpoint names the account limit.
        crate::backend::limit_key_for(&self.endpoint, self.api_key.as_deref())
    }

    fn limit_family(&self) -> Option<String> {
        // A ChatGPT plan has one usage limit for every model. The API
        // counts per model; dated snapshots share their model's limit.
        if self.chatgpt_plan {
            return None;
        }
        let model = self.model.to_ascii_lowercase();
        let dated = model.len() > 11
            && model.as_bytes()[model.len() - 11] == b'-'
            && model[model.len() - 10..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-');
        Some(if dated { model[..model.len() - 11].to_owned() } else { model })
    }

    fn supports_user_images(&self) -> bool {
        self.image_input_enabled
    }

    fn target_caps(&self) -> crate::TargetCaps {
        crate::TargetCaps {
            vision: self.image_input_enabled,
            tools: true,
            tool_call_id: crate::ToolIdRule::Token { max_len: 64 },
            tool_name_max: Some(64),
        }
    }

    async fn count_input_tokens(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<Option<u64>, AgentError> {
        if self.chatgpt_plan || messages.iter().all(|message| message.images.is_empty()) {
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
        if self.chatgpt_plan {
            fields.push("chatgpt-plan-namespaced-tools-v1");
        }
        if let Some(web_search) = &web_search {
            fields.extend(["hosted-web-search-v1", web_search.as_str()]);
        }
        // Keep the historical fingerprint unchanged when the cap is absent.
        let output_limit = self.max_output_tokens.map(|limit| limit.to_string());
        if let Some(limit) = &output_limit {
            fields.extend(["max-output-tokens-v1", limit.as_str()]);
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
            truncated: false,
        })
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
            .map(ModelCompletion::from)
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
        let detail = response_failure_detail(body, status);
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
    validate_response_output_resources(body)?;
    let output_items = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| AgentError::Model("Responses result has no output array".into()))?
        .clone();
    let mut content = String::new();
    let mut content_bytes = 0;
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
                            append_limited(&mut content, text, &mut content_bytes)?;
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

/// Responses failures can carry fields on the event or inside either error
/// envelope. Keep the code so the host can offer the appropriate recovery.
fn response_failure_detail(value: &Value, fallback: &str) -> String {
    let sources = [
        value.pointer("/response/error"),
        value.get("error"),
        Some(value),
    ];
    let field = |name: &str| {
        sources.iter().find_map(|source| {
            source
                .and_then(|source| source.get(name))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        })
    };
    let detail = field("message")
        .or_else(|| value.get("error").and_then(Value::as_str))
        .or_else(|| {
            value
                .pointer("/response/incomplete_details/reason")
                .or_else(|| value.pointer("/incomplete_details/reason"))
                .and_then(Value::as_str)
        })
        .unwrap_or(fallback);
    // Limit diagnostic text independently of the transport's frame budget.
    let detail: String = detail.chars().take(1_000).collect();
    if let Some(code) = field("code") {
        let code: String = code.chars().take(256).collect();
        format!("{code}: {detail}")
    } else {
        detail
    }
}

fn required_string<'a>(value: &'a Value, field: &str, source: &str) -> Result<&'a str, AgentError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentError::Model(format!("{source} has no {field}")))
}

fn checked_assembled_bytes(current: usize, added: usize) -> Result<usize, AgentError> {
    if added > MAX_ASSEMBLED_BYTES.saturating_sub(current) {
        return Err(AgentError::Limit(format!(
            "Responses assembled output is too large ({MAX_ASSEMBLED_BYTES} byte limit)"
        )));
    }
    Ok(current + added)
}

fn append_limited(
    target: &mut String,
    fragment: &str,
    assembled_bytes: &mut usize,
) -> Result<(), AgentError> {
    let next = checked_assembled_bytes(*assembled_bytes, fragment.len())?;
    target.push_str(fragment);
    *assembled_bytes = next;
    Ok(())
}

/// Validate ordinary JSON and response.completed before retaining their copy.
/// Cache entry count (MAX_REPLAY_ITEMS) is a separate multi-response limit.
fn validate_response_output_resources(body: &Value) -> Result<(), AgentError> {
    let items = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| AgentError::Model("Responses result has no output array".into()))?;
    validate_output_items_resources(items)
}

fn validate_output_items_resources(items: &[Value]) -> Result<(), AgentError> {
    let mut bytes = 0;
    let mut calls = 0;
    for item in items {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            bytes = checked_assembled_bytes(bytes, id.len())?;
        }
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
                            bytes = checked_assembled_bytes(bytes, text.len())?;
                        }
                    }
                }
            }
            Some("function_call") => {
                if item
                    .get("status")
                    .is_some_and(|status| status.as_str() != Some("completed"))
                {
                    return Err(AgentError::Model(
                        "Responses function call is not completed".into(),
                    ));
                }
                calls += 1;
                if calls > MAX_MODEL_TOOL_CALLS {
                    return Err(AgentError::Limit(format!(
                        "model response exceeds {MAX_MODEL_TOOL_CALLS} tool calls"
                    )));
                }
                for field in ["call_id", "name", "arguments"] {
                    let value = required_string(item, field, "function call")?;
                    bytes = checked_assembled_bytes(bytes, value.len())?;
                }
            }
            _ => {}
        }
    }
    // Include opaque reasoning/output payloads without creating another buffer.
    limited_json_size(items)?;
    Ok(())
}

fn limited_json_size(value: &(impl serde::Serialize + ?Sized)) -> Result<usize, AgentError> {
    struct OutputSizeCounter(usize);
    impl std::io::Write for OutputSizeCounter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(buffer.len())
                .filter(|size| *size <= MAX_ASSEMBLED_BYTES)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Responses output items are too large",
                    )
                })?;
            Ok(buffer.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = OutputSizeCounter(0);
    serde_json::to_writer(&mut counter, value).map_err(|_| {
        AgentError::Limit(format!(
            "Responses output items are too large ({MAX_ASSEMBLED_BYTES} byte limit)"
        ))
    })?;
    Ok(counter.0)
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
    published_activity: bool,
    completed: Option<Value>,
    response_id: Option<String>,
    calls: BTreeMap<u64, PartialCall>,
    web_search: web_search_stream::WebSearchStream,
    // Canonical completed items are delivered separately from the terminal
    // response summary by Codex/ChatGPT routes. Retain them for replay only;
    // they never authorize tool execution before finish() validates the turn.
    done_items: BTreeMap<u64, Value>,
    done_item_ids: HashSet<String>,
    done_items_bytes: usize,
    // Summary parts are separate paragraphs within one provider reasoning
    // item. Preserve their boundary without changing that item's replay ID.
    reasoning_summary_parts: BTreeMap<String, u64>,
    text: String,
    assembled_bytes: usize,
}

impl ResponseStream {
    #[cfg(test)]
    fn ingest(&mut self, frame: &[u8], deltas: &UnboundedSender<String>) -> Result<(), AgentError> {
        let mut events = Vec::new();
        let result = self.ingest_events(frame, &mut events);
        publish_legacy(events, deltas);
        result
    }

    fn ingest_events(
        &mut self,
        frame: &[u8],
        events: &mut Vec<ModelStreamEvent>,
    ) -> Result<(), AgentError> {
        let frame = std::str::from_utf8(frame)
            .map_err(|error| AgentError::Model(format!("invalid Responses SSE UTF-8: {error}")))?;
        let data = sse_lines(frame)
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
        if let Some(update) = self.web_search.observe(&event)? {
            self.published_activity = true;
            events.push(update);
        }
        match event.get("type").and_then(Value::as_str) {
            Some("response.created") => {
                let response_id = event
                    .pointer("/response/id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty());
                if let Some(response_id) = response_id {
                    if self
                        .response_id
                        .as_deref()
                        .is_some_and(|id| id != response_id)
                    {
                        return Err(AgentError::Model(
                            "Responses stream changed response ID".into(),
                        ));
                    }
                    self.response_id = Some(response_id.to_owned());
                }
                events.push(ModelStreamEvent::Started {
                    response_id: self.response_id.clone(),
                });
            }
            Some("response.reasoning_summary_text.delta") => {
                let item_id = required_string(&event, "item_id", "reasoning summary")?;
                let content = event.get("delta").and_then(Value::as_str).ok_or_else(|| {
                    AgentError::Model("Responses reasoning summary has no text".into())
                })?;
                if !content.is_empty() {
                    // Older Responses-compatible gateways omit the index;
                    // preserve their single-summary behavior as part zero.
                    let summary_index = event
                        .get("summary_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let previous_index = self.reasoning_summary_parts.get(item_id);
                    if previous_index.is_none()
                        && self.reasoning_summary_parts.len() >= MAX_STREAM_OUTPUT_ITEMS
                    {
                        return Err(AgentError::Limit(format!(
                            "Responses stream exceeds {MAX_STREAM_OUTPUT_ITEMS} reasoning summary items"
                        )));
                    }
                    let separator = previous_index.is_some_and(|index| *index != summary_index);
                    let next_bytes = checked_assembled_bytes(self.assembled_bytes, content.len())?;
                    let next_bytes =
                        checked_assembled_bytes(next_bytes, usize::from(separator) * 2)?;
                    let next_bytes = checked_assembled_bytes(
                        next_bytes,
                        if previous_index.is_none() {
                            item_id.len()
                        } else {
                            0
                        },
                    )?;
                    let content = if separator {
                        format!("\n\n{content}")
                    } else {
                        content.to_owned()
                    };
                    self.assembled_bytes = next_bytes;
                    if let Some(index) = self.reasoning_summary_parts.get_mut(item_id) {
                        *index = summary_index;
                    } else {
                        self.reasoning_summary_parts
                            .insert(item_id.to_owned(), summary_index);
                    }
                    self.published_activity = true;
                    events.push(ModelStreamEvent::ReasoningDelta {
                        item_id: item_id.to_owned(),
                        content,
                    });
                }
            }
            Some("response.output_text.delta" | "response.refusal.delta") => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AgentError::Model("Responses text delta has no text".into()))?;
                if !delta.is_empty() {
                    append_limited(&mut self.text, delta, &mut self.assembled_bytes)?;
                    self.published_delta = true;
                    self.published_activity = true;
                    events.push(ModelStreamEvent::TextDelta {
                        content: delta.to_owned(),
                    });
                }
            }
            Some("response.output_item.added" | "response.output_item.done") => {
                let is_done =
                    event.get("type").and_then(Value::as_str) == Some("response.output_item.done");
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
                        let previous_arguments_len = self
                            .calls
                            .get(&index)
                            .map_or(0, |call| call.arguments.len());
                        if let Some(partial) = self.calls.get_mut(&index) {
                            update_partial_call(partial, item, &mut self.assembled_bytes)?;
                        } else {
                            if self.calls.len() >= MAX_MODEL_TOOL_CALLS {
                                return Err(AgentError::Limit(format!(
                                    "model response exceeds {MAX_MODEL_TOOL_CALLS} tool calls"
                                )));
                            }
                            let mut partial = PartialCall::default();
                            let mut next_bytes = self.assembled_bytes;
                            update_partial_call(&mut partial, item, &mut next_bytes)?;
                            self.calls.insert(index, partial);
                            self.assembled_bytes = next_bytes;
                        }
                        let partial = self.calls.get(&index).expect("inserted function item");
                        self.published_activity = true;
                        events.push(ModelStreamEvent::ToolCallDelta {
                            item_id: partial.item_id.clone(),
                            call_id: Some(partial.call_id.clone()),
                            name: Some(partial.name.clone()),
                            arguments_delta: partial.arguments[previous_arguments_len..].to_owned(),
                        });
                    }
                    if is_done {
                        let index = event
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .ok_or_else(|| {
                                AgentError::Model(
                                    "Responses completed item has no output index".into(),
                                )
                            })?;
                        self.remember_done_item(index, item)?;
                    }
                } else if is_done {
                    return Err(AgentError::Model(
                        "Responses completed item event has no item".into(),
                    ));
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
                append_limited(&mut partial.arguments, delta, &mut self.assembled_bytes)?;
                if !delta.is_empty() {
                    self.published_activity = true;
                    events.push(ModelStreamEvent::ToolCallDelta {
                        item_id: partial.item_id.clone(),
                        call_id: Some(partial.call_id.clone()),
                        name: Some(partial.name.clone()),
                        arguments_delta: delta.to_owned(),
                    });
                }
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
                    if partial.arguments.is_empty() {
                        append_limited(
                            &mut partial.arguments,
                            arguments,
                            &mut self.assembled_bytes,
                        )?;
                        if !arguments.is_empty() {
                            self.published_activity = true;
                            events.push(ModelStreamEvent::ToolCallDelta {
                                item_id: partial.item_id.clone(),
                                call_id: Some(partial.call_id.clone()),
                                name: Some(partial.name.clone()),
                                arguments_delta: arguments.to_owned(),
                            });
                        }
                    }
                }
            }
            Some("response.completed") => {
                let response = event.get("response").ok_or_else(|| {
                    AgentError::Model("Responses completed event has no response".into())
                })?;
                // A terminal summary may omit output. A present malformed
                // snapshot is still an error; it must never be hidden by the
                // completed-item collection.
                if response.get("output").is_some() {
                    validate_response_output_resources(response)?;
                }
                self.completed = Some(response.clone());
            }
            Some("response.failed" | "response.incomplete" | "response.error" | "error") => {
                let detail = response_failure_detail(&event, "unknown failure");
                return Err(AgentError::Provider(crate::ProviderFailure::from_body(
                    &event,
                    format!("Responses stream failed: {detail}"),
                )));
            }
            _ => {}
        }
        Ok(())
    }

    fn remember_done_item(&mut self, index: u64, item: &Value) -> Result<(), AgentError> {
        let item_id = required_string(item, "id", "completed output item")?;
        required_string(item, "type", "completed output item")?;
        validate_output_items_resources(std::slice::from_ref(item))?;
        if let Some(previous) = self.done_items.get(&index) {
            if previous != item {
                return Err(AgentError::Model(
                    "Responses completed output index changed item".into(),
                ));
            }
            return Ok(());
        }
        if self.done_item_ids.contains(item_id) {
            return Err(AgentError::Model(
                "Responses completed output item appeared at multiple indexes".into(),
            ));
        }
        if self.done_items.len() >= MAX_STREAM_OUTPUT_ITEMS {
            return Err(AgentError::Limit(format!(
                "Responses stream exceeds {MAX_STREAM_OUTPUT_ITEMS} output items"
            )));
        }
        let next_bytes = checked_assembled_bytes(self.done_items_bytes, limited_json_size(item)?)?;
        self.done_items.insert(index, item.clone());
        self.done_item_ids.insert(item_id.to_owned());
        self.done_items_bytes = next_bytes;
        self.published_activity = true;
        Ok(())
    }

    fn finish(mut self) -> Result<ResponsesResult, AgentError> {
        let mut response = self.completed.take().ok_or_else(|| {
            AgentError::Model("Responses stream ended before response.completed".into())
        })?;
        if let Some(response_id) = &self.response_id {
            if response.get("id").and_then(Value::as_str) != Some(response_id.as_str()) {
                return Err(AgentError::Model(
                    "Responses completed response ID mismatch".into(),
                ));
            }
        }
        reconcile_completed_output(&mut response, self.done_items)?;
        let result = parse_response(&response)?;
        if !self.text.is_empty() && self.text != result.output.content {
            return Err(AgentError::Model(
                "Responses streamed text differs from completed response".into(),
            ));
        }
        // SSE output indexes are correlation keys, not necessarily positions
        // in completed.output: providers can leave gaps after empty added items.
        // Keep the final response's order for replay, and validate stable IDs.
        let mut final_calls = BTreeMap::new();
        for item in &result.output_items {
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                let item_id = required_string(item, "id", "completed function call")?;
                if final_calls.insert(item_id, item).is_some() {
                    return Err(AgentError::Model(
                        "Responses completed function item ID is duplicated".into(),
                    ));
                }
            }
        }
        let mut seen_items = HashSet::new();
        for partial in self.calls.into_values() {
            if !seen_items.insert(partial.item_id.clone()) {
                return Err(AgentError::Model(
                    "Responses function item appeared at multiple stream indexes".into(),
                ));
            }
            let item = final_calls.get(partial.item_id.as_str()).ok_or_else(|| {
                AgentError::Model(
                    "Responses streamed function call is missing from completed output".into(),
                )
            })?;
            if item.get("call_id").and_then(Value::as_str) != Some(partial.call_id.as_str())
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

/// `output_item.done` supplies canonical items; `response.completed` seals the
/// request and may carry only a summary. A full snapshot defines output order;
/// a subset of known done IDs only overlays fields onto stream-index order. A
/// partial call or an arguments-done event is never a fallback output item.
fn reconcile_completed_output(
    response: &mut Value,
    done_items: BTreeMap<u64, Value>,
) -> Result<(), AgentError> {
    if done_items.is_empty() {
        return Ok(());
    }
    let response = response
        .as_object_mut()
        .ok_or_else(|| AgentError::Model("Responses completed response is not an object".into()))?;
    let mut snapshot = match response.remove("output") {
        Some(Value::Array(items)) => items,
        Some(_) => {
            return Err(AgentError::Model(
                "Responses result has no output array".into(),
            ))
        }
        None => Vec::new(),
    };
    let mut done = done_items.into_values().map(Some).collect::<Vec<_>>();
    let positions = done
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let id = required_string(
                item.as_ref().expect("completed item"),
                "id",
                "completed output item",
            )?;
            Ok((id.to_owned(), index))
        })
        .collect::<Result<BTreeMap<_, _>, AgentError>>()?;
    let mut snapshot_ids = HashSet::new();
    for item in &snapshot {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            if !snapshot_ids.insert(id.to_owned()) {
                return Err(AgentError::Model(
                    "Responses completed output item ID is duplicated".into(),
                ));
            }
        }
    }
    let omits_done_item = positions.keys().any(|id| !snapshot_ids.contains(id));
    let adds_snapshot_item = snapshot.iter().any(|item| {
        item.get("id")
            .and_then(Value::as_str)
            .is_none_or(|id| !positions.contains_key(id))
    });
    // Without every completed item, a snapshot-only item has no reliable
    // position among the stream items. Guessing could replay opaque reasoning
    // after the function call it belongs to. A summary/empty snapshot and a
    // subset of known done IDs are recoverable; a mixed partial timeline is not.
    if omits_done_item && adds_snapshot_item {
        return Err(AgentError::Model(
            "Responses partial completed snapshot has ambiguous output order".into(),
        ));
    }
    if omits_done_item {
        // Every snapshot ID is known here. A partial snapshot cannot redefine
        // the sequence around omitted reasoning/call items: use it strictly as
        // a field overlay, including when its known IDs arrive reordered.
        snapshot.sort_by_key(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .and_then(|id| positions.get(id))
                .copied()
        });
    }
    let mut output = Vec::new();
    let mut next_done = 0;
    for snapshot_item in snapshot {
        let position = snapshot_item
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| positions.get(id))
            .copied();
        if let Some(position) = position {
            // Missing predecessors must remain before this correlated item,
            // especially reasoning items required for stateless tool replay.
            for slot in done.iter_mut().take(position).skip(next_done) {
                let item = slot.as_ref().expect("unvisited completed item");
                if !snapshot_ids.contains(required_string(item, "id", "completed output item")?) {
                    output.push(slot.take().expect("completed predecessor"));
                }
            }
            next_done = next_done.max(position + 1);
            let mut canonical = done[position].take().expect("unique completed snapshot ID");
            let canonical_fields = canonical.as_object_mut().ok_or_else(|| {
                AgentError::Model("Responses completed output item is not an object".into())
            })?;
            let snapshot_fields = snapshot_item.as_object().ok_or_else(|| {
                AgentError::Model("Responses completed snapshot item is not an object".into())
            })?;
            for (field, value) in snapshot_fields {
                if canonical_fields
                    .get(field)
                    .is_some_and(|previous| previous != value)
                {
                    return Err(AgentError::Model(
                        "Responses completed item conflicts with terminal snapshot".into(),
                    ));
                }
                canonical_fields
                    .entry(field.clone())
                    .or_insert_with(|| value.clone());
            }
            output.push(canonical);
        } else {
            output.push(snapshot_item);
        }
    }
    output.extend(done.into_iter().flatten());
    // Retained done items and the terminal snapshot each have their own cap;
    // their combined, canonical transcript must fit the same response budget.
    validate_output_items_resources(&output)?;
    response.insert("output".into(), Value::Array(output));
    Ok(())
}

fn update_partial_call(
    partial: &mut PartialCall,
    item: &Value,
    assembled_bytes: &mut usize,
) -> Result<(), AgentError> {
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
    let arguments = item.get("arguments").and_then(Value::as_str);
    if let Some(arguments) = arguments {
        if arguments.len() > MAX_ASSEMBLED_BYTES {
            return Err(AgentError::Limit(
                "Responses function arguments are too large".into(),
            ));
        }
        // `added` starts with an empty string; `done` carries the final string.
        if !arguments.is_empty() && !partial.arguments.is_empty() && partial.arguments != arguments
        {
            return Err(AgentError::Model(
                "Responses function argument deltas mismatch completed item".into(),
            ));
        }
    }
    let mut next = *assembled_bytes;
    if partial.item_id.is_empty() {
        for bytes in [item_id.len(), call_id.len(), name.len()] {
            next = checked_assembled_bytes(next, bytes)?;
        }
    }
    if partial.arguments.is_empty() {
        if let Some(arguments) = arguments {
            next = checked_assembled_bytes(next, arguments.len())?;
        }
    }
    // All identity, equality and aggregate limits are checked before mutation.
    if partial.item_id.is_empty() {
        partial.item_id = item_id.to_owned();
        partial.call_id = call_id.to_owned();
        partial.name = name.to_owned();
    }
    if partial.arguments.is_empty() {
        if let Some(arguments) = arguments {
            partial.arguments = arguments.to_owned();
        }
    }
    *assembled_bytes = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::{
        AgentEventKind, AgentRunRequest, SessionStore, TurnMode, UserImage, UserImageMime,
    };
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
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

    fn resource_call(index: usize, arguments: &str) -> Value {
        json!({"type":"function_call","id":format!("item-中-{index}"),"call_id":format!("call-{index}"),"name":"read_file","arguments":arguments})
    }

    fn resource_event(
        stream: &mut ResponseStream,
        sender: &UnboundedSender<String>,
        event: Value,
    ) -> Result<(), AgentError> {
        stream.ingest(format!("data: {event}").as_bytes(), sender)
    }

    fn regression_event(
        stream: &mut ResponseStream,
        event: Value,
    ) -> Result<Vec<ModelStreamEvent>, AgentError> {
        let mut events = Vec::new();
        stream.ingest_events(format!("data: {event}\n\n").as_bytes(), &mut events)?;
        Ok(events)
    }

    #[test]
    fn reasoning_summary_parts_keep_paragraphs_across_fragmented_sse_and_final_replay() {
        let reasoning = json!({"type":"reasoning","id":"rs_parts","summary":[
            {"type":"summary_text","text":"Revisando Café 中."},
            {"type":"summary_text","text":"Validando 🦀."}
        ],"encrypted_content":"opaque-fixture"});
        let next_reasoning = json!({"type":"reasoning","id":"rs_next","summary":[
            {"type":"summary_text","text":"Otro item."}
        ]});
        let body = reconciliation_http_stream(&[
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_parts","summary_index":0,"delta":"Revisando "}),
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_parts","summary_index":0,"delta":"Café 中."}),
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_next","summary_index":0,"delta":"Otro "}),
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_parts","summary_index":1,"delta":""}),
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_parts","summary_index":1,"delta":"Validando 🦀."}),
            json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_next","summary_index":0,"delta":"item."}),
            json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
            json!({"type":"response.output_item.done","output_index":1,"item":next_reasoning}),
            json!({"type":"response.completed","response":{"id":"resp_parts","status":"completed","output":[]}}),
        ]);
        for chunk_size in [1, 3, 7, body.len()] {
            let mut pending = crate::backend::SseFrameBuffer::default();
            let mut stream = ResponseStream::default();
            let mut events = Vec::new();
            for chunk in body.as_bytes().chunks(chunk_size) {
                pending.extend(chunk, MAX_STREAM_FRAME_BYTES).unwrap();
                while let Some(frame) = pending.take_sse_frame() {
                    stream.ingest_events(&frame, &mut events).unwrap();
                }
            }
            let summaries = events
                .iter()
                .filter_map(|event| match event {
                    ModelStreamEvent::ReasoningDelta { item_id, content } => {
                        Some((item_id.as_str(), content.as_str()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                summaries,
                vec![
                    ("rs_parts", "Revisando "),
                    ("rs_parts", "Café 中."),
                    ("rs_next", "Otro "),
                    ("rs_parts", "\n\nValidando 🦀."),
                    ("rs_next", "item."),
                ]
            );
            assert_eq!(
                stream.assembled_bytes,
                "Revisando Café 中.\n\nValidando 🦀.Otro item.".len()
                    + "rs_parts".len()
                    + "rs_next".len()
            );
            let completed = stream.finish().unwrap();
            assert_eq!(
                completed.output_items,
                vec![reasoning.clone(), next_reasoning.clone()]
            );
            let mut assistant = AgentMessage::assistant(completed.output.content, vec![]);
            assistant.provider_output_items = completed.output_items;
            let replay = ResponsesBackend::new("https://example.invalid/v1", None, "mock")
                .request_body(
                    &[
                        AgentMessage::user("Revisa"),
                        assistant,
                        AgentMessage::user("Sigue"),
                    ],
                    &[],
                    false,
                )
                .unwrap();
            assert_eq!(replay["input"][1], reasoning);
            assert_eq!(replay["input"][2], next_reasoning);
        }
    }

    #[test]
    fn reasoning_summary_separator_respects_aggregate_limit_without_partial_mutation() {
        let first = "x".repeat((MAX_ASSEMBLED_BYTES - "rs_limit".len()) / 2);
        for remaining in [3, 4] {
            let mut stream = ResponseStream::default();
            for content in [first.as_str(), &first[..first.len() - remaining]] {
                regression_event(&mut stream, json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_limit","summary_index":0,"delta":content})).unwrap();
            }
            let bytes_before = stream.assembled_bytes;
            let parts_before = stream.reasoning_summary_parts.clone();
            let mut events = Vec::new();
            let event = json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_limit","summary_index":1,"delta":"é"});
            let result = stream.ingest_events(format!("data: {event}").as_bytes(), &mut events);
            if remaining == 3 {
                assert!(matches!(result, Err(AgentError::Limit(_))));
                assert!(events.is_empty());
                assert_eq!(stream.assembled_bytes, bytes_before);
                assert_eq!(stream.reasoning_summary_parts, parts_before);
            } else {
                result.unwrap();
                assert_eq!(stream.assembled_bytes, MAX_ASSEMBLED_BYTES);
                assert_eq!(
                    events,
                    vec![ModelStreamEvent::ReasoningDelta {
                        item_id: "rs_limit".into(),
                        content: "\n\né".into()
                    }]
                );
            }
        }
    }

    #[test]
    fn reasoning_summary_without_index_preserves_compatible_gateway_fragments() {
        let mut stream = ResponseStream::default();
        for text in ["Resumen ", "Café 中🦀"] {
            assert_eq!(
                regression_event(&mut stream, json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_legacy","delta":text})).unwrap(),
                vec![ModelStreamEvent::ReasoningDelta {
                    item_id: "rs_legacy".into(), content: text.into()
                }]
            );
        }
    }

    #[test]
    fn reasoning_summary_item_cardinality_is_bounded_before_mutation() {
        let mut stream = ResponseStream::default();
        for index in 0..MAX_STREAM_OUTPUT_ITEMS {
            regression_event(&mut stream, json!({"type":"response.reasoning_summary_text.delta","item_id":format!("rs_{index}"),"summary_index":0,"delta":"x"})).unwrap();
        }
        assert_eq!(
            stream.reasoning_summary_parts.len(),
            MAX_STREAM_OUTPUT_ITEMS
        );
        let bytes_before = stream.assembled_bytes;
        let mut events = Vec::new();
        let event = json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_over_limit","summary_index":0,"delta":"x"});
        assert!(matches!(
            stream.ingest_events(format!("data: {event}").as_bytes(), &mut events),
            Err(AgentError::Limit(_))
        ));
        assert!(events.is_empty());
        assert_eq!(stream.assembled_bytes, bytes_before);
        assert_eq!(
            stream.reasoning_summary_parts.len(),
            MAX_STREAM_OUTPUT_ITEMS
        );
        assert!(!stream.reasoning_summary_parts.contains_key("rs_over_limit"));
    }

    fn regression_items() -> Vec<Value> {
        vec![
            json!({"type":"reasoning","id":"rs_regression","encrypted_content":"opaque-reasoning","summary":[]}),
            json!({"type":"function_call","id":"fc_a","call_id":"call_a","name":"read_file","namespace":"openide","arguments":"{\"path\":\"a\"}","status":"completed"}),
            json!({"type":"function_call","id":"fc_b","call_id":"call_b","name":"read_file","namespace":"openide","arguments":"{\"path\":\"b\"}","status":"completed"}),
            json!({"type":"function_call","id":"fc_c","call_id":"call_c","name":"read_file","namespace":"openide","arguments":"{\"path\":\"c\"}","status":"completed"}),
            json!({"type":"message","id":"msg_regression","status":"completed","content":[{"type":"output_text","text":"Fuente [1]","annotations":[{"type":"url_citation","start_index":7,"end_index":10,"url":"https://example.com/source","title":"Fuente"}]}]}),
        ]
    }

    fn regression_done_stream(items: &[Value]) -> ResponseStream {
        let mut stream = ResponseStream::default();
        regression_event(
            &mut stream,
            json!({"type":"response.created","response":{"id":"resp_regression"}}),
        )
        .unwrap();
        for (index, item) in items.iter().enumerate() {
            // Sparse indexes prove that indexes correlate
            // items, rather than addressing a compact terminal array.
            let events = regression_event(&mut stream, json!({"type":"response.output_item.done","output_index":index * 3 + 2,"item":item})).unwrap();
            assert!(!events
                .iter()
                .any(|event| matches!(event, ModelStreamEvent::ToolCallReady { .. })));
        }
        stream
    }

    fn regression_finish(
        mut stream: ResponseStream,
        output: Option<Value>,
    ) -> Result<ResponsesResult, AgentError> {
        let mut response = json!({"id":"resp_regression","status":"completed","usage":{"input_tokens":12,"output_tokens":7,"total_tokens":19}});
        if let Some(output) = output {
            response["output"] = output;
        }
        regression_event(
            &mut stream,
            json!({"type":"response.completed","response":response}),
        )?;
        stream.finish()
    }

    #[test]
    fn reconciliation_terminal_summary_and_empty_output_preserve_all_done_items() {
        let items = regression_items();
        for output in [None, Some(json!([]))] {
            let result = regression_finish(regression_done_stream(&items), output).unwrap();
            assert_eq!(result.output_items, items);
            assert_eq!(result.output.content, "Fuente [1]");
            assert_eq!(result.output.tool_calls.len(), 3);
            assert_eq!(result.usage.as_ref().unwrap().total_tokens, 19);
            assert_eq!(
                result.output_items[0]["encrypted_content"],
                "opaque-reasoning"
            );
            assert_eq!(result.output_items[1]["namespace"], "openide");
            assert_eq!(
                result.web_citations().unwrap()[0].url,
                "https://example.com/source"
            );
        }
    }

    #[test]
    fn reconciliation_partial_known_ids_overlay_stream_order_even_when_reordered() {
        let items = regression_items();
        for indexes in [vec![1, 3], vec![3, 1], vec![4, 0, 2]] {
            let snapshot = indexes
                .into_iter()
                .map(|index| {
                    let mut item = items[index].clone();
                    item.as_object_mut().unwrap().remove("encrypted_content");
                    item.as_object_mut().unwrap().remove("namespace");
                    item
                })
                .collect::<Vec<_>>();
            let result =
                regression_finish(regression_done_stream(&items), Some(json!(snapshot))).unwrap();
            assert_eq!(
                result.output_items, items,
                "partial order must never move opaque reasoning around calls"
            );
        }
    }

    #[test]
    fn reconciliation_complete_snapshot_preserves_final_order_and_extra_fields() {
        let items = regression_items();
        let mut snapshot = [4, 3, 2, 1, 0]
            .into_iter()
            .map(|index| items[index].clone())
            .collect::<Vec<_>>();
        snapshot[0]["phase"] = json!("final_answer");
        snapshot.push(json!({"type":"reasoning","id":"rs_snapshot","encrypted_content":"extra"}));
        let result =
            regression_finish(regression_done_stream(&items), Some(json!(snapshot))).unwrap();
        assert_eq!(result.output_items, snapshot);
        assert_eq!(result.output.tool_calls[0].id, "call_c");
    }

    #[test]
    fn reconciliation_done_arrival_order_and_index_gaps_do_not_change_canonical_order() {
        let items = regression_items();
        let mut stream = ResponseStream::default();
        for index in [4, 2, 0, 3, 1] {
            regression_event(&mut stream, json!({"type":"response.output_item.done","output_index":index * 7 + 5,"item":items[index]})).unwrap();
        }
        assert_eq!(
            regression_finish(stream, Some(json!([])))
                .unwrap()
                .output_items,
            items
        );
    }

    #[test]
    fn reconciliation_rejects_mixed_partial_timeline_and_malformed_present_snapshot() {
        let items = regression_items();
        for snapshot in [
            json!([items[1],{"type":"reasoning","id":"rs_new","encrypted_content":"new"}]),
            json!([{"type":"function_call","id":"fc_new","call_id":"call_new","name":"read_file","arguments":"{}"}]),
        ] {
            let error =
                regression_finish(regression_done_stream(&items), Some(snapshot)).unwrap_err();
            assert!(error.to_string().contains("ambiguous output order"));
        }
        for malformed in [Value::Null, json!({}), json!("not an array")] {
            assert!(
                regression_finish(regression_done_stream(&items), Some(malformed))
                    .unwrap_err()
                    .to_string()
                    .contains("output array")
            );
        }
    }

    #[test]
    fn reconciliation_duplicate_done_is_idempotent_but_conflicts_and_duplicate_ids_fail() {
        let items = regression_items();
        let mut stream = regression_done_stream(&items[..2]);
        let bytes = stream.done_items_bytes;
        regression_event(
            &mut stream,
            json!({"type":"response.output_item.done","output_index":5,"item":items[1]}),
        )
        .unwrap();
        assert_eq!(stream.done_items_bytes, bytes);
        assert_eq!(stream.done_items.len(), 2);
        let mut changed = items[1].clone();
        changed["namespace"] = json!("foreign");
        assert!(regression_event(
            &mut stream,
            json!({"type":"response.output_item.done","output_index":5,"item":changed})
        )
        .unwrap_err()
        .to_string()
        .contains("changed item"));
        let mut stream = regression_done_stream(&items[..2]);
        assert!(regression_event(
            &mut stream,
            json!({"type":"response.output_item.done","output_index":99,"item":items[1]})
        )
        .unwrap_err()
        .to_string()
        .contains("multiple indexes"));
        assert!(regression_finish(
            regression_done_stream(&items[..2]),
            Some(json!([items[0], items[1], items[1]]))
        )
        .unwrap_err()
        .to_string()
        .contains("duplicated"));
    }

    #[test]
    fn reconciliation_validates_status_response_identity_arguments_and_namespace() {
        let items = regression_items();
        for status in [
            json!("in_progress"),
            json!("incomplete"),
            json!("failed"),
            Value::Null,
        ] {
            let mut call = items[1].clone();
            call["status"] = status;
            let mut stream = ResponseStream::default();
            assert!(regression_event(
                &mut stream,
                json!({"type":"response.output_item.done","output_index":0,"item":call})
            )
            .unwrap_err()
            .to_string()
            .contains("not completed"));
            assert!(
                regression_finish(ResponseStream::default(), Some(json!([call])))
                    .unwrap_err()
                    .to_string()
                    .contains("not completed")
            );
        }
        let mut stream = regression_done_stream(&items[..2]);
        regression_event(&mut stream, json!({"type":"response.completed","response":{"id":"other_response","status":"completed","output":[]}})).unwrap();
        assert!(stream
            .finish()
            .unwrap_err()
            .to_string()
            .contains("response ID mismatch"));
        for field in [
            "arguments",
            "namespace",
            "name",
            "call_id",
            "encrypted_content",
        ] {
            let mut snapshot = items[..2].to_vec();
            let index = if field == "encrypted_content" { 0 } else { 1 };
            snapshot[index][field] = if field == "arguments" {
                json!("{\"path\":\"changed\"}")
            } else {
                json!("changed")
            };
            assert!(
                regression_finish(regression_done_stream(&items[..2]), Some(json!(snapshot)))
                    .unwrap_err()
                    .to_string()
                    .contains("conflicts with terminal snapshot")
            );
        }
        for status in ["in_progress", "incomplete", "failed"] {
            let mut stream = regression_done_stream(&items[..2]);
            regression_event(&mut stream, json!({"type":"response.completed","response":{"id":"resp_regression","status":status,"output":[]}})).unwrap();
            assert!(
                stream.finish().is_err(),
                "a completed item never masks terminal status {status}"
            );
        }
        for event_type in ["response.failed", "response.incomplete", "response.error"] {
            let mut stream = regression_done_stream(&items[..2]);
            assert!(regression_event(&mut stream, json!({"type":event_type,"response":{"id":"resp_regression","error":{"message":"fixture terminal failure"}}})).is_err());
            assert!(stream.finish().is_err());
        }
    }

    #[test]
    fn reconciliation_partial_arguments_or_invalid_json_never_become_validated_tools() {
        let call = regression_items()[1].clone();
        let mut added = call.clone();
        added["arguments"] = json!("");
        added["status"] = json!("in_progress");
        let mut stream = ResponseStream::default();
        regression_event(
            &mut stream,
            json!({"type":"response.output_item.added","output_index":8,"item":added}),
        )
        .unwrap();
        regression_event(&mut stream, json!({"type":"response.function_call_arguments.done","output_index":8,"item_id":"fc_a","arguments":call["arguments"]})).unwrap();
        assert!(regression_finish(stream, Some(json!([])))
            .unwrap_err()
            .to_string()
            .contains("missing from completed output"));
        let mut invalid = call.clone();
        invalid["arguments"] = json!("{not json}");
        assert!(
            regression_finish(regression_done_stream(&[invalid]), Some(json!([])))
                .unwrap_err()
                .to_string()
                .contains("invalid Responses function arguments")
        );
        let mut stream = ResponseStream::default();
        regression_event(
            &mut stream,
            json!({"type":"response.output_item.added","output_index":8,"item":added}),
        )
        .unwrap();
        regression_event(&mut stream, json!({"type":"response.function_call_arguments.delta","output_index":8,"item_id":"fc_a","delta":"{\"path\":\"different\"}"})).unwrap();
        assert!(regression_event(
            &mut stream,
            json!({"type":"response.output_item.done","output_index":8,"item":call})
        )
        .unwrap_err()
        .to_string()
        .contains("deltas mismatch"));
    }

    #[test]
    fn reconciliation_done_collection_and_merged_output_have_bounded_resources() {
        let mut stream = ResponseStream::default();
        for index in 0..MAX_STREAM_OUTPUT_ITEMS {
            regression_event(&mut stream, json!({"type":"response.output_item.done","output_index":index,"item":{"type":"reasoning","id":format!("rs_{index}")}})).unwrap();
        }
        let bytes = stream.done_items_bytes;
        assert!(matches!(
            regression_event(
                &mut stream,
                json!({"type":"response.output_item.done","output_index":MAX_STREAM_OUTPUT_ITEMS,"item":{"type":"reasoning","id":"one_too_many"}})
            ),
            Err(AgentError::Limit(_))
        ));
        assert_eq!(stream.done_items.len(), MAX_STREAM_OUTPUT_ITEMS);
        assert_eq!(stream.done_items_bytes, bytes);
        let big = json!({"type":"reasoning","id":"rs_big","encrypted_content":"x".repeat(MAX_ASSEMBLED_BYTES / 2)});
        let mut stream = regression_done_stream(std::slice::from_ref(&big));
        let bytes = stream.done_items_bytes;
        let mut second = big.clone();
        second["id"] = json!("rs_second");
        assert!(matches!(
            regression_event(
                &mut stream,
                json!({"type":"response.output_item.done","output_index":8,"item":second})
            ),
            Err(AgentError::Limit(_))
        ));
        assert_eq!(stream.done_items.len(), 1);
        assert_eq!(stream.done_items_bytes, bytes);
        let overlay = json!({"type":"reasoning","id":"rs_big"});
        let extra = json!({"type":"reasoning","id":"rs_extra","encrypted_content":"y".repeat(MAX_ASSEMBLED_BYTES / 2)});
        assert!(matches!(
            regression_finish(
                regression_done_stream(&[big]),
                Some(json!([overlay, extra]))
            ),
            Err(AgentError::Limit(_))
        ));
        let mut stream = ResponseStream::default();
        for index in 0..MAX_MODEL_TOOL_CALLS {
            regression_event(&mut stream, json!({"type":"response.output_item.done","output_index":index,"item":resource_call(index,"{}")})).unwrap();
        }
        assert!(matches!(
            regression_event(
                &mut stream,
                json!({"type":"response.output_item.done","output_index":MAX_MODEL_TOOL_CALLS,"item":resource_call(MAX_MODEL_TOOL_CALLS,"{}")})
            ),
            Err(AgentError::Limit(_))
        ));
        assert_eq!(stream.calls.len(), MAX_MODEL_TOOL_CALLS);
    }

    struct ReconciliationReadFixture {
        executions: Arc<AtomicUsize>,
        store: Arc<SessionStore>,
        session_id: String,
    }

    #[async_trait]
    impl crate::ToolExecutor for ReconciliationReadFixture {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![tool()]
        }

        async fn execute(
            &self,
            call: &ToolCall,
            context: &crate::ToolContext,
        ) -> Result<crate::ToolOutput, AgentError> {
            // Observe the durable journal from inside the effect boundary,
            // rather than merely trusting presentation-event ordering.
            let saved = self.store.load(&self.session_id).unwrap().unwrap();
            let committed = saved
                .events
                .iter()
                .find(|event| {
                    matches!(&event.kind,
                        AgentEventKind::ModelResponseCommitted { assistant, .. }
                        if assistant.tool_calls.iter().any(|item| item.id == call.id)
                    )
                })
                .expect("validated response is durable before a tool effect");
            let started = saved
                .events
                .iter()
                .find(|event| {
                    matches!(&event.kind,
                        AgentEventKind::ToolStarted { tool_call } if tool_call.id == call.id
                    )
                })
                .expect("tool start is durable before a tool effect");
            assert!(committed.sequence < started.sequence);
            self.executions.fetch_add(1, Ordering::SeqCst);
            let path = call.arguments["path"].as_str().unwrap();
            let content = std::fs::read_to_string(context.workspace.join(path))
                .map_err(|error| AgentError::Runtime(error.to_string()))?;
            Ok(crate::ToolOutput::success(content))
        }
    }

    fn reconciliation_http_request(socket: &std::net::TcpStream) -> Value {
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("POST /v1/responses "));
        let mut length = None;
        let mut accepts_sse = false;
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("accept:") {
                accepts_sse = value.trim() == "text/event-stream";
            }
        }
        let mut bytes = vec![0; length.unwrap()];
        reader.read_exact(&mut bytes).unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["model"], "scripted");
        assert!(accepts_sse);
        body
    }

    fn reconciliation_http_stream(events: &[Value]) -> String {
        events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect()
    }

    fn reconciliation_http_headers_with_mime(
        socket: &mut std::net::TcpStream,
        length: usize,
        mime: Option<&str>,
    ) {
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(socket, "HTTP/1.1 200 OK\r\n").unwrap();
        if let Some(mime) = mime {
            write!(socket, "Content-Type: {mime}\r\n").unwrap();
        }
        write!(
            socket,
            "Content-Length: {length}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    }

    #[derive(Clone, Copy)]
    enum ReconciliationTerminalFixture {
        Empty,
        Conflict,
        CrDelimited,
        UndelimitedEof,
        MissingMime,
        BomMissingMime,
        BomDeclaredSse,
        MislabeledJsonMime,
        MislabeledJsonMimeUndelimitedEof,
    }

    fn reconciliation_http_journal_fixture(terminal_kind: ReconciliationTerminalFixture) {
        let conflicting_terminal = matches!(terminal_kind, ReconciliationTerminalFixture::Conflict);
        let undelimited_eof = matches!(
            terminal_kind,
            ReconciliationTerminalFixture::UndelimitedEof
                | ReconciliationTerminalFixture::MislabeledJsonMimeUndelimitedEof
        );
        let mime = match terminal_kind {
            ReconciliationTerminalFixture::MissingMime
            | ReconciliationTerminalFixture::BomMissingMime => None,
            ReconciliationTerminalFixture::MislabeledJsonMime
            | ReconciliationTerminalFixture::MislabeledJsonMimeUndelimitedEof => {
                Some("application/json")
            }
            _ => Some("text/event-stream"),
        };
        let has_bom = matches!(
            terminal_kind,
            ReconciliationTerminalFixture::BomMissingMime
                | ReconciliationTerminalFixture::BomDeclaredSse
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let (release_terminal, await_terminal) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let first = reconciliation_http_request(&socket);
            assert!(first["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["role"] == "user"));
            let reasoning = json!({"type":"reasoning","id":"rs_fixture","encrypted_content":"fixture-opaque-reasoning","summary":[]});
            let arguments = "{\"path\":\"README.md\"}";
            let done = json!({"type":"function_call","id":"fc_fixture","call_id":"call_fixture","name":"read_file","namespace":"openide","arguments":arguments,"status":"completed"});
            let mut added = done.clone();
            added["arguments"] = json!("");
            added["status"] = json!("in_progress");
            let mut prefix = reconciliation_http_stream(&[
                json!({"type":"response.created","response":{"id":"resp_fixture"}}),
                json!({"type":"response.output_item.done","output_index":2,"item":reasoning}),
                json!({"type":"response.output_item.added","output_index":7,"item":added}),
                json!({"type":"response.function_call_arguments.delta","output_index":7,"item_id":"fc_fixture","delta":arguments}),
                json!({"type":"response.function_call_arguments.done","output_index":7,"item_id":"fc_fixture","arguments":arguments}),
                json!({"type":"response.output_item.done","output_index":7,"item":done}),
            ]);
            if has_bom {
                prefix.insert(0, '\u{feff}');
            }
            let output = if conflicting_terminal {
                let mut changed = done;
                changed["name"] = json!("write_file");
                json!([reasoning, changed])
            } else {
                json!([])
            };
            let mut terminal = reconciliation_http_stream(&[
                json!({"type":"response.completed","response":{"id":"resp_fixture","status":"completed","output":output,"usage":{"input_tokens":12,"output_tokens":7,"total_tokens":19}}}),
            ]);
            if matches!(terminal_kind, ReconciliationTerminalFixture::CrDelimited) {
                prefix = prefix.replace('\n', "\r");
                terminal = terminal.replace('\n', "\r");
            } else if undelimited_eof {
                terminal.truncate(terminal.len() - 2);
            }
            reconciliation_http_headers_with_mime(&mut socket, prefix.len() + terminal.len(), mime);
            socket.write_all(prefix.as_bytes()).unwrap();
            socket.flush().unwrap();
            // The parent test observes every preview through output_item.done
            // before allowing the terminal response to reach the runner.
            await_terminal
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            socket.write_all(terminal.as_bytes()).unwrap();
            socket.flush().unwrap();
            drop(socket);
            if conflicting_terminal || undelimited_eof {
                return;
            }
            let (mut socket, _) = listener.accept().unwrap();
            let followup = reconciliation_http_request(&socket);
            let input = followup["input"].as_array().unwrap();
            assert!(input.iter().any(|item| item["id"] == "rs_fixture"
                && item["encrypted_content"] == "fixture-opaque-reasoning"));
            assert!(input.iter().any(|item| item["id"] == "fc_fixture"
                && item["namespace"] == "openide"
                && item["call_id"] == "call_fixture"));
            assert!(input
                .iter()
                .any(|item| item["type"] == "function_call_output"
                    && item["call_id"] == "call_fixture"
                    && item["output"] == "fixture file payload"));
            let mut followup = reconciliation_http_stream(&[
                json!({"type":"response.created","response":{"id":"resp_followup"}}),
                json!({"type":"response.output_text.delta","delta":"Done"}),
                json!({"type":"response.completed","response":{"id":"resp_followup","status":"completed","output":[{"type":"message","id":"msg_followup","content":[{"type":"output_text","text":"Done"}]}]}}),
            ]);
            if has_bom {
                followup.insert(0, '\u{feff}');
            }
            reconciliation_http_headers_with_mime(&mut socket, followup.len(), mime);
            socket.write_all(followup.as_bytes()).unwrap();
        });
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("README.md"), "fixture file payload").unwrap();
        let store_root = workspace.path().join("sessions");
        let store = Arc::new(SessionStore::open(&store_root).unwrap());
        let session = store.create(workspace.path(), "native").unwrap();
        let prompt = "Read README.md";
        let admitted = store
            .admit_native_turn(
                &session.id,
                "fixture-turn",
                prompt,
                "scripted",
                TurnMode::Build,
            )
            .unwrap();
        let lease = store.try_acquire_native_lease(&session.id).unwrap();
        let executions = Arc::new(AtomicUsize::new(0));
        let tools = ReconciliationReadFixture {
            executions: executions.clone(),
            store: store.clone(),
            session_id: session.id.clone(),
        };
        let runner = crate::AgentRunner::new(
            Arc::new(ResponsesBackend::new(&endpoint, None, "scripted")),
            Arc::new(tools),
        )
        .with_native_runtime_identity("fixture", &endpoint)
        .with_event_journal(store.clone());
        let handle = runner.start_with_lease(
            AgentRunRequest {
                session_id: Some(session.id.clone()),
                sequence_start: admitted.sequence,
                workspace: workspace.path().to_path_buf(),
                history: Vec::new(),
                prompt: prompt.into(),
                mode: TurnMode::Build,
            },
            lease,
        );
        let mut events = Vec::new();
        let mut saw_arguments = false;
        loop {
            let event = handle.events.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(
                !matches!(
                    &event.kind,
                    AgentEventKind::ModelResponseCommitted { .. }
                        | AgentEventKind::ToolStarted { .. }
                        | AgentEventKind::ToolFinished { .. }
                        | AgentEventKind::ModelStreamUpdate {
                            update: ModelStreamEvent::ToolCallReady { .. },
                            ..
                        }
                        | AgentEventKind::TurnCompleted { .. }
                        | AgentEventKind::TurnFailed { .. }
                ),
                "a preview must never commit or execute a tool: {:?}",
                event.kind
            );
            let done_preview = if let AgentEventKind::ModelStreamUpdate {
                update:
                    ModelStreamEvent::ToolCallDelta {
                        item_id,
                        arguments_delta,
                        ..
                    },
                ..
            } = &event.kind
            {
                assert_eq!(item_id, "fc_fixture");
                let done = saw_arguments && arguments_delta.is_empty();
                saw_arguments |= !arguments_delta.is_empty();
                done
            } else {
                false
            };
            events.push(event);
            if done_preview {
                break;
            }
        }
        assert!(
            events.iter().any(|event| matches!(
                &event.kind,
                AgentEventKind::ModelStreamUpdate {
                    update: ModelStreamEvent::Started { response_id: Some(response_id) },
                    ..
                } if response_id == "resp_fixture"
            )),
            "the very first data frame is parsed before the terminal is released"
        );
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        let preview = store.load(&session.id).unwrap().unwrap();
        assert!(!preview.events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::ModelResponseCommitted { .. }
                | AgentEventKind::ToolStarted { .. }
                | AgentEventKind::ToolFinished { .. }
        )));
        release_terminal.send(()).unwrap();
        loop {
            let event = handle.events.recv_timeout(Duration::from_secs(10)).unwrap();
            let terminal = matches!(
                event.kind,
                AgentEventKind::TurnCompleted { .. }
                    | AgentEventKind::TurnFailed { .. }
                    | AgentEventKind::TurnCancelled | AgentEventKind::TurnSuperseded { .. }
            );
            events.push(event);
            if terminal {
                break;
            }
        }
        if conflicting_terminal || undelimited_eof {
            assert_eq!(executions.load(Ordering::SeqCst), 0);
            assert!(
                matches!(&events.last().unwrap().kind, AgentEventKind::TurnFailed { error }
                if error.contains(if undelimited_eof { "ended inside an SSE frame" } else { "conflicts with terminal snapshot" }))
            );
            assert!(!events.iter().any(|event| matches!(
                &event.kind,
                AgentEventKind::ModelResponseCommitted { .. }
                    | AgentEventKind::ToolStarted { .. }
                    | AgentEventKind::ToolFinished { .. }
                    | AgentEventKind::ModelStreamUpdate {
                        update: ModelStreamEvent::ToolCallReady { .. },
                        ..
                    }
            )));
        } else {
            assert_eq!(executions.load(Ordering::SeqCst), 1);
            assert!(
                matches!(&events.last().unwrap().kind, AgentEventKind::TurnCompleted { response, .. } if response == "Done")
            );
            let committed = events.iter().find(|event| matches!(&event.kind,
                AgentEventKind::ModelResponseCommitted { response_id, .. } if response_id.as_deref() == Some("resp_fixture")
            )).unwrap();
            let started = events
                .iter()
                .find(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. }))
                .unwrap();
            let finished = events
                .iter()
                .find(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. }))
                .unwrap();
            assert!(committed.sequence < started.sequence && started.sequence < finished.sequence);
        }
        server.join().unwrap();
        assert!(matches!(
            handle.events.recv_timeout(Duration::from_secs(10)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));
        let reopened = SessionStore::open(&store_root).unwrap();
        let saved = reopened.load(&session.id).unwrap().unwrap();
        for event in events {
            assert!(
                saved.events.contains(&event),
                "every observed native event survives reopen"
            );
        }
    }

    #[test]
    fn reconciliation_http_empty_terminal_commits_before_effects_and_replays_opaque_items() {
        reconciliation_http_journal_fixture(ReconciliationTerminalFixture::Empty);
    }

    #[test]
    fn reconciliation_http_conflicting_terminal_never_commits_or_executes_previewed_tools() {
        reconciliation_http_journal_fixture(ReconciliationTerminalFixture::Conflict);
    }

    #[test]
    fn reconciliation_http_cr_delimited_terminal_commits_and_executes_validated_tool() {
        reconciliation_http_journal_fixture(ReconciliationTerminalFixture::CrDelimited);
    }

    #[test]
    fn reconciliation_http_complete_json_at_sse_eof_never_commits_or_executes_previewed_tool() {
        reconciliation_http_journal_fixture(ReconciliationTerminalFixture::UndelimitedEof);
    }

    #[test]
    fn reconciliation_http_sse_without_mime_streams_live_and_commits_before_effects() {
        reconciliation_http_journal_fixture(ReconciliationTerminalFixture::MissingMime);
    }

    #[test]
    fn reconciliation_http_bom_missing_mime_preserves_first_event_and_live_preview() {
        reconciliation_http_journal_fixture(ReconciliationTerminalFixture::BomMissingMime);
    }

    #[test]
    fn reconciliation_http_bom_declared_sse_preserves_first_event_and_live_preview() {
        reconciliation_http_journal_fixture(ReconciliationTerminalFixture::BomDeclaredSse);
    }

    #[test]
    fn reconciliation_http_sse_mislabeled_json_streams_live_and_commits_before_effects() {
        reconciliation_http_journal_fixture(ReconciliationTerminalFixture::MislabeledJsonMime);
    }

    #[test]
    fn reconciliation_http_mislabeled_sse_eof_never_commits_or_executes_previewed_tool() {
        reconciliation_http_journal_fixture(
            ReconciliationTerminalFixture::MislabeledJsonMimeUndelimitedEof,
        );
    }

    #[tokio::test]
    async fn sse_invalid_json_html_empty_and_truncated_bodies_publish_no_ready_events() {
        for (mime, body, kind) in [
            ("text/html", "<html>PRIVATE_BODY_MARKER</html>", "markup"),
            ("application/json", "", "empty"),
            (
                "application/json",
                "{\"status\":\"completed\",\"output\":[",
                "json",
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                reconciliation_http_request(&socket);
                reconciliation_http_headers_with_mime(&mut socket, body.len(), Some(mime));
                socket.write_all(body.as_bytes()).unwrap();
            });
            let backend = ResponsesBackend::new(endpoint, None, "scripted");
            let cancel = CancellationToken::new();
            let (sender, mut receiver) = crate::model_event_channel(cancel.clone());
            let error = backend
                .stream_events(&[AgentMessage::user("Hello")], &[], cancel, sender)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("invalid Responses JSON"));
            assert!(error.contains("HTTP 200"));
            assert!(error.contains(&format!("body {kind}")));
            assert!(!error.contains("PRIVATE_BODY_MARKER"));
            assert!(
                receiver.try_recv().is_err(),
                "invalid bodies publish no events"
            );
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn sse_ordinary_json_fallback_preserves_pretty_blank_lines_and_validates_completion() {
        for ending in ["\r", "\n", "\r\n"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                reconciliation_http_request(&socket);
                let body = serde_json::to_string_pretty(&json!({"id":"json_fallback","status":"completed","output":[{"type":"message","id":"msg_json","content":[{"type":"output_text","text":"Café 中🦀"}]}]})).unwrap().replace('\n', &format!("{ending}{ending}"));
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let backend = ResponsesBackend::new(endpoint, None, "scripted");
            let (sender, _) = unbounded_channel();
            let result = backend
                .stream_complete_detailed(
                    &[AgentMessage::user("Hello")],
                    &[],
                    CancellationToken::new(),
                    sender,
                )
                .await
                .unwrap();
            assert_eq!(result.output.content, "Café 中🦀");
            assert_eq!(result.response_id, "json_fallback");
            server.join().unwrap();
        }
    }

    #[test]
    fn response_call_cardinality_and_completed_json_boundaries_preserve_unicode() {
        let (sender, mut receiver) = unbounded_channel();
        let mut stream = ResponseStream::default();
        resource_event(
            &mut stream,
            &sender,
            json!({"type":"response.output_text.delta","delta":"Café 中"}),
        )
        .unwrap();
        for index in 0..MAX_MODEL_TOOL_CALLS {
            let added = json!({"type":"response.output_item.added","output_index":index,"item":resource_call(index, "")});
            resource_event(&mut stream, &sender, added.clone()).unwrap();
            let before = stream.assembled_bytes;
            resource_event(&mut stream, &sender, added).unwrap();
            assert_eq!(
                stream.assembled_bytes, before,
                "identical IDs are retained once"
            );
            resource_event(&mut stream, &sender, json!({"type":"response.output_item.done","output_index":index,"item":resource_call(index, "{}") })).unwrap();
        }
        let mut output = (0..MAX_MODEL_TOOL_CALLS)
            .map(|index| resource_call(index, "{}"))
            .collect::<Vec<_>>();
        output.push(json!({"type":"message","content":[{"type":"output_text","text":"Café 中"}]}));
        let response = json!({"id":"response-boundary","status":"completed","output":output});
        assert_eq!(
            parse_response(&response).unwrap().output.tool_calls.len(),
            MAX_MODEL_TOOL_CALLS
        );
        let mut too_many = response.clone();
        too_many["output"].as_array_mut().unwrap().insert(
            MAX_MODEL_TOOL_CALLS,
            resource_call(MAX_MODEL_TOOL_CALLS, "{}"),
        );
        assert!(matches!(
            parse_response(&too_many),
            Err(AgentError::Limit(_))
        ));
        resource_event(
            &mut stream,
            &sender,
            json!({"type":"response.completed","response":response}),
        )
        .unwrap();
        let result = stream.finish().unwrap();
        assert_eq!(result.output.tool_calls.len(), MAX_MODEL_TOOL_CALLS);
        assert_eq!(result.output.content, "Café 中");
        assert_eq!(
            result.output_items.len(),
            MAX_MODEL_TOOL_CALLS + 1,
            "replay-cache entries are not a per-response output-item cap"
        );
        assert_eq!(receiver.try_recv().unwrap(), "Café 中");
        assert!(receiver.try_recv().is_err());
        let (sender, mut receiver) = unbounded_channel();
        let mut denied = ResponseStream::default();
        resource_event(
            &mut denied,
            &sender,
            json!({"type":"response.output_text.delta","delta":"Café 中"}),
        )
        .unwrap();
        for index in 0..MAX_MODEL_TOOL_CALLS {
            resource_event(&mut denied, &sender, json!({"type":"response.output_item.added","output_index":index,"item":resource_call(index, "")})).unwrap();
        }
        let before = denied.assembled_bytes;
        let overflow = json!({"type":"response.output_item.added","output_index":MAX_MODEL_TOOL_CALLS,"item":resource_call(MAX_MODEL_TOOL_CALLS, "")});
        assert!(matches!(
            resource_event(&mut denied, &sender, overflow),
            Err(AgentError::Limit(_))
        ));
        assert_eq!(denied.calls.len(), MAX_MODEL_TOOL_CALLS);
        assert_eq!(denied.assembled_bytes, before);
        assert!(matches!(
            resource_event(
                &mut denied,
                &sender,
                json!({"type":"response.completed","response":too_many})
            ),
            Err(AgentError::Limit(_))
        ));
        assert!(denied.completed.is_none());
        assert!(denied.finish().is_err());
        assert_eq!(receiver.try_recv().unwrap(), "Café 中");
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn response_aggregate_small_frames_count_metadata_and_reject_before_mutation() {
        let (sender, mut receiver) = unbounded_channel();
        let mut stream = ResponseStream::default();
        resource_event(
            &mut stream,
            &sender,
            json!({"type":"response.output_text.delta","delta":"Café 中"}),
        )
        .unwrap();
        for index in 0..2 {
            resource_event(&mut stream, &sender, json!({"type":"response.output_item.added","output_index":index,"item":resource_call(index, "")})).unwrap();
        }
        let metadata = (0..2)
            .map(|index| {
                let call = resource_call(index, "");
                ["id", "call_id", "name"]
                    .into_iter()
                    .map(|field| call[field].as_str().unwrap().len())
                    .sum::<usize>()
            })
            .sum::<usize>();
        assert_eq!(stream.assembled_bytes, "Café 中".len() + metadata);
        let fragment = "x".repeat(1024);
        for index in 0..(MAX_ASSEMBLED_BYTES - stream.assembled_bytes) / fragment.len() {
            let call = index % 2;
            resource_event(&mut stream, &sender, json!({"type":"response.function_call_arguments.delta","output_index":call,"item_id":format!("item-中-{call}"),"delta":fragment})).unwrap();
        }
        let remainder = MAX_ASSEMBLED_BYTES - stream.assembled_bytes;
        resource_event(&mut stream, &sender, json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"item-中-1","delta":"y".repeat(remainder)})).unwrap();
        assert_eq!(stream.assembled_bytes, MAX_ASSEMBLED_BYTES);
        let before = stream
            .calls
            .iter()
            .map(|(index, call)| (*index, call.arguments.len()))
            .collect::<Vec<_>>();
        assert!(before.iter().all(|(_, bytes)| *bytes < MAX_ASSEMBLED_BYTES));
        assert!(matches!(
            resource_event(
                &mut stream,
                &sender,
                json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"item-中-0","delta":"終"})
            ),
            Err(AgentError::Limit(_))
        ));
        assert_eq!(stream.assembled_bytes, MAX_ASSEMBLED_BYTES);
        assert_eq!(
            stream
                .calls
                .iter()
                .map(|(index, call)| (*index, call.arguments.len()))
                .collect::<Vec<_>>(),
            before
        );
        let arguments = stream.calls[&0].arguments.clone();
        for _ in 0..2 {
            resource_event(&mut stream, &sender, json!({"type":"response.function_call_arguments.done","output_index":0,"item_id":"item-中-0","arguments":arguments})).unwrap();
            resource_event(&mut stream, &sender, json!({"type":"response.output_item.done","output_index":0,"item":resource_call(0, &arguments)})).unwrap();
            assert_eq!(
                stream.assembled_bytes, MAX_ASSEMBLED_BYTES,
                "done snapshots must not double count retained bytes"
            );
        }
        let oversized = "z".repeat(MAX_ASSEMBLED_BYTES + 1);
        assert!(matches!(
            resource_event(
                &mut stream,
                &sender,
                json!({"type":"response.function_call_arguments.done","output_index":0,"item_id":"item-中-0","arguments":oversized})
            ),
            Err(AgentError::Limit(_))
        ));
        assert!(matches!(
            resource_event(
                &mut stream,
                &sender,
                json!({"type":"response.output_item.done","output_index":0,"item":resource_call(0,&oversized)})
            ),
            Err(AgentError::Limit(_))
        ));
        assert!(matches!(
            resource_event(
                &mut stream,
                &sender,
                json!({"type":"response.output_item.added","output_index":2,"item":resource_call(2,&oversized)})
            ),
            Err(AgentError::Limit(_))
        ));
        assert_eq!(stream.calls.len(), 2);
        assert_eq!(stream.calls[&0].arguments, arguments);
        assert_eq!(stream.assembled_bytes, MAX_ASSEMBLED_BYTES);
        assert_eq!(stream.text, "Café 中");
        assert_eq!(receiver.try_recv().unwrap(), "Café 中");
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn response_final_json_aggregate_and_opaque_output_cannot_bypass_limits() {
        let arguments = json!({"text":"x".repeat(MAX_ASSEMBLED_BYTES / 2)}).to_string();
        let response = json!({"id":"response-bounds","status":"completed","output":[resource_call(0,&arguments),resource_call(1,&arguments)]});
        assert!(matches!(
            parse_response(&response),
            Err(AgentError::Limit(_))
        ));
        let (sender, _receiver) = unbounded_channel();
        let mut stream = ResponseStream::default();
        assert!(matches!(
            resource_event(
                &mut stream,
                &sender,
                json!({"type":"response.completed","response":response})
            ),
            Err(AgentError::Limit(_))
        ));
        assert!(stream.completed.is_none());
        let opaque = json!({"type":"reasoning","id":"reasoning-id","encrypted_content":"x".repeat(MAX_ASSEMBLED_BYTES + 1)});
        let response = json!({"id":"response-opaque","status":"completed","output":[opaque]});
        assert!(matches!(
            parse_response(&response),
            Err(AgentError::Limit(_))
        ));
        assert!(matches!(
            resource_event(
                &mut stream,
                &sender,
                json!({"type":"response.completed","response":response})
            ),
            Err(AgentError::Limit(_))
        ));
        assert!(stream.completed.is_none());
        let opaque = json!({"type":"reasoning","id":"reasoning-id","encrypted_content":"Café 中"});
        let response =
            json!({"id":"response-opaque-valid","status":"completed","output":[opaque.clone()]});
        assert_eq!(
            parse_response(&response).unwrap().output_items,
            vec![opaque]
        );
    }

    #[test]
    fn a_function_call_output_carries_its_images_as_content_items() {
        let image = tiny_png();
        let call = ToolCall { id: "call_shot".into(), name: "shot".into(), arguments: json!({}) };
        let mut result = AgentMessage::tool("call_shot", "captured");
        result.images = vec![image.clone()];
        let mut history =
            vec![AgentMessage::user("look"), AgentMessage::assistant("", vec![call]), result];
        let item = json!({"type":"input_image","image_url":image.verified_data_url().unwrap()});
        // The API-key route and the ChatGPT plan route send the same item.
        for backend in [
            ResponsesBackend::new("https://api.openai.com/v1", None, "mock"),
            ResponsesBackend::new("https://chatgpt.example/backend-api/codex", None, "mock")
                .with_chatgpt_plan(),
        ] {
            let body = backend.request_body(&history, &[], true).unwrap();
            assert_eq!(
                body["input"][2],
                json!({"type":"function_call_output","call_id":"call_shot","output":[
                    {"type":"input_text","text":"captured"}, item,
                ]})
            );
            // The input-token count is asked for this same input.
            assert_eq!(backend.input_token_count_body(&history, &[]).unwrap()["input"], body["input"]);
        }
        let backend = ResponsesBackend::new("https://api.openai.com/v1", None, "mock");
        // A result that is only an image sends no empty text item.
        history[2].content.clear();
        let body = backend.request_body(&history, &[], true).unwrap();
        assert_eq!(body["input"][2]["output"], json!([item]));
        // A result without images keeps the string it always was.
        history[2].images.clear();
        let body = backend.request_body(&history, &[], true).unwrap();
        assert_eq!(body["input"][2]["output"], "");
        // A corrupt image and an image on an assistant message are refused.
        history[2].images = vec![image.clone()];
        history[2].images[0].sha256 = "0".repeat(64);
        assert!(backend.request_body(&history, &[], true).is_err());
        history[2].images.clear();
        history[1].images = vec![image];
        assert!(backend.request_body(&history, &[], true).is_err());
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

    #[test]
    fn output_limit_is_validated_and_part_of_recovery_identity() {
        let base = ResponsesBackend::new("https://test.example/v1", None, "exact-model");
        let legacy = base.request_profile_fingerprint();
        let capped = base.with_max_output_tokens(4096).unwrap();
        assert_ne!(legacy, capped.request_profile_fingerprint());
        assert_eq!(
            capped
                .request_body(&[AgentMessage::user("Hi")], &[], true)
                .unwrap()["max_output_tokens"],
            4096
        );
        assert_ne!(
            capped.request_profile_fingerprint(),
            ResponsesBackend::new("https://test.example/v1", None, "exact-model")
                .with_max_output_tokens(8192)
                .unwrap()
                .request_profile_fingerprint()
        );
        for invalid in [0, 15, u64::MAX] {
            assert!(
                ResponsesBackend::new("https://test.example/v1", None, "exact-model")
                    .with_max_output_tokens(invalid)
                    .is_err()
            );
        }
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
            assert_eq!(request["max_output_tokens"], 4096);
            assert_eq!(request["input"][0]["content"][0]["text"], "describe");
            assert_eq!(request["input"][0]["content"][1]["image_url"], expected_url);
            let completed = json!({"type":"response.completed","response":{"id":"resp_img","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"a picture"}]}]}});
            let sse = format!(
                "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"a picture\"}}\n\ndata: {completed}\n\n"
            );
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}", sse.len(), sse).unwrap();
            socket.flush().unwrap();
        });

        let stored = AgentMessage::user_with_images("describe", vec![tiny_png()]).unwrap();
        let restored: AgentMessage =
            serde_json::from_value(serde_json::to_value(stored).unwrap()).unwrap();
        let backend = ResponsesBackend::new(format!("http://{address}/v1"), None, "mock")
            .with_max_output_tokens(4096)
            .unwrap();
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
            assert!(request.get("max_output_tokens").is_none());
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
        .unwrap()
        .with_max_output_tokens(4096)
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
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len() + 40
            );
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
    fn accepts_new_provider_effort_without_a_hardcoded_level_list() {
        let backend = ResponsesBackend::new("https://provider.example/v1", None, "new-model")
            .with_reasoning_effort("turbo_2027")
            .unwrap();
        let body = backend
            .request_body(&[AgentMessage::user("hello")], &[], false)
            .unwrap();
        assert_eq!(body["reasoning"]["effort"], "turbo_2027");
        for invalid in ["", "too much", "line\nbreak"] {
            assert!(
                ResponsesBackend::new("https://provider.example/v1", None, "model")
                    .with_reasoning_effort(invalid)
                    .is_err()
            );
        }
        assert!(
            ResponsesBackend::new("https://provider.example/v1", None, "model")
                .with_reasoning_effort("x".repeat(65))
                .is_err()
        );
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
        // An empty added item can leave a gap in SSE indexes. The completed
        // output array is compact, while deltas still use indexes 2 and 7.
        let events = [
            json!({"type":"response.output_item.added","output_index":0,"item":null}),
            json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":""}}),
            json!({"type":"response.output_item.added","output_index":7,"item":{"type":"function_call","id":"fc_2","call_id":"call_2","name":"read_file","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","output_index":7,"item_id":"fc_2","delta":"{\"path\":\"b\"}"}),
            json!({"type":"response.function_call_arguments.delta","output_index":2,"item_id":"fc_1","delta":"{\"path\":\"a\"}"}),
            json!({"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a\"}"}}),
            json!({"type":"response.output_item.done","output_index":7,"item":{"type":"function_call","id":"fc_2","call_id":"call_2","name":"read_file","arguments":"{\"path\":\"b\"}"}}),
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
