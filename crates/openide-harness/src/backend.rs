use crate::{AgentError, ToolDefinition};
use async_trait::async_trait;
use futures_util::StreamExt;
use openide_core::{AgentMessage, MessageRole, ToolCall};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

const MAX_STREAM_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const MAX_ASSEMBLED_STREAM_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_MODEL_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_MODEL_HTTP_ERROR_BYTES: usize = 64 * 1024;
const MAX_MODEL_TRANSPORT_RETRIES: usize = 2;
const MAX_MODEL_RETRY_DELAY: Duration = Duration::from_secs(10);

/// Declared capabilities for one model on one wire protocol. The provider
/// registry or an explicit custom connection supplies these values; model
/// names never imply capabilities. Enabling tools permits sending them but
/// does not guarantee the remote server accepts them.
/// An empty/default contract advertises neither tools nor adjustable effort.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ModelRequestCapabilities {
    pub tool_calls: bool,
    /// Effort values accepted without function tools. These are provider
    /// values, not a fixed OpenIDE enum.
    pub reasoning_efforts: Vec<String>,
    /// When present, narrows the selectable efforts for requests with tools.
    /// `None` means the ordinary reasoning effort set also applies to tools.
    pub tool_call_reasoning_efforts: Option<Vec<String>>,
    /// Some protocols require an effort to be sent when tools are present.
    /// This value is only emitted for tool requests without a user choice.
    pub default_tool_call_effort: Option<String>,
}

impl ModelRequestCapabilities {
    pub fn efforts_with_tools(&self) -> &[String] {
        self.tool_call_reasoning_efforts
            .as_deref()
            .unwrap_or(&self.reasoning_efforts)
    }

    fn validate(&self) -> Result<(), AgentError> {
        fn validate_efforts(values: &[String]) -> Result<(), AgentError> {
            if values.len() > 32 {
                return Err(AgentError::InvalidRequest(
                    "model capability contract has too many effort values".into(),
                ));
            }
            let mut seen = std::collections::BTreeSet::new();
            for value in values {
                if value.is_empty()
                    || value.len() > 64
                    || value.chars().any(char::is_whitespace)
                    || value.chars().any(char::is_control)
                    || !seen.insert(value)
                {
                    return Err(AgentError::InvalidRequest(
                        "model capability contract has an invalid or repeated effort".into(),
                    ));
                }
            }
            Ok(())
        }

        validate_efforts(&self.reasoning_efforts)?;
        if let Some(tool_efforts) = &self.tool_call_reasoning_efforts {
            validate_efforts(tool_efforts)?;
            if !tool_efforts
                .iter()
                .all(|effort| self.reasoning_efforts.contains(effort))
            {
                return Err(AgentError::InvalidRequest(
                    "tool effort values must be supported model efforts".into(),
                ));
            }
        }
        if !self.tool_calls
            && (self.tool_call_reasoning_efforts.is_some()
                || self.default_tool_call_effort.is_some())
        {
            return Err(AgentError::InvalidRequest(
                "a model without tool calls cannot declare tool effort settings".into(),
            ));
        }
        if let Some(default) = &self.default_tool_call_effort {
            if !self.efforts_with_tools().contains(default) {
                return Err(AgentError::InvalidRequest(
                    "default tool effort must be an accepted tool effort".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelOutput {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

pub use openide_core::ModelTokenUsage;

/// A provider-neutral completion with optional protocol metadata.
///
/// `provider_output_items` may contain opaque reasoning state. Callers that
/// persist it must treat it as sensitive and replay it unchanged when the
/// provider requires stateless continuation.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelCompletion {
    pub output: ModelOutput,
    pub response_id: Option<String>,
    pub usage: Option<ModelTokenUsage>,
    pub provider_output_items: Vec<Value>,
}

impl From<ModelOutput> for ModelCompletion {
    fn from(output: ModelOutput) -> Self {
        Self {
            output,
            response_id: None,
            usage: None,
            provider_output_items: Vec::new(),
        }
    }
}

#[async_trait]
pub trait ModelBackend: Send + Sync {
    fn model_id(&self) -> &str;

    /// Whether this transport can send durable user images. The selected
    /// provider and model must be checked separately before offering images.
    fn supports_user_images(&self) -> bool {
        false
    }

    /// Exact input token count for a request containing images, when this
    /// transport offers a pre-request counting API. `None` means unknown and
    /// must not be treated as a zero-cost image by a budget gate.
    async fn count_input_tokens(
        &self,
        _messages: &[AgentMessage],
        _tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<Option<u64>, AgentError> {
        Ok(None)
    }

    /// Stable identity of the request protocol and effective model options.
    /// Native journaled runs fail closed when this is None. Test doubles must
    /// supply a fixed value explicitly; production adapters must include
    /// every option that changes the request sent to the provider.
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        None
    }

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

    /// Backends with richer protocols can override this method. Existing
    /// Chat Completions and test backends keep their original implementation.
    async fn stream_complete_detailed(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelCompletion, AgentError> {
        self.stream_complete(messages, tools, cancel, deltas)
            .await
            .map(ModelCompletion::from)
    }
}

pub(crate) fn stable_request_profile_fingerprint(fields: &[&str]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"openide-model-request-profile\0");
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hasher.finalize().into()
}

pub(crate) async fn read_model_http_body(
    mut response: reqwest::Response,
    limit: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, AgentError> {
    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            chunk = response.chunk() => chunk.map_err(|error| AgentError::Model(error.to_string()))?,
        };
        let Some(chunk) = chunk else {
            return Ok(body);
        };
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(AgentError::Limit(format!(
                "model HTTP body exceeds {limit} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
}

pub(crate) enum ModelAttemptError {
    Terminal(AgentError),
    Retryable {
        error: AgentError,
        retry_after: Option<Duration>,
    },
}

impl ModelAttemptError {
    fn request(error: reqwest::Error) -> Self {
        let retryable = error.is_connect() || error.is_timeout();
        let error = AgentError::Model(error.to_string());
        if retryable {
            Self::Retryable {
                error,
                retry_after: None,
            }
        } else {
            Self::Terminal(error)
        }
    }

    pub(crate) fn stream(error: reqwest::Error, published_delta: bool) -> Self {
        let retryable = !published_delta
            && (error.is_connect() || error.is_timeout() || error.is_body() || error.is_decode());
        let error = AgentError::Model(error.to_string());
        if retryable {
            Self::Retryable {
                error,
                retry_after: None,
            }
        } else {
            Self::Terminal(error)
        }
    }

    pub(crate) fn incomplete_stream(message: &'static str, published_delta: bool) -> Self {
        let error = AgentError::Model(message.into());
        if published_delta {
            Self::Terminal(error)
        } else {
            Self::Retryable {
                error,
                retry_after: None,
            }
        }
    }
}

/// Retry only failures that cannot have returned a completed model response to
/// the runner. Streaming callers must mark an attempt terminal after publishing
/// the first delta, preserving live delivery and journaled partial output.
pub(crate) async fn retry_model_transport<T, F, Fut>(
    cancel: &CancellationToken,
    mut attempt: F,
) -> Result<T, AgentError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ModelAttemptError>>,
{
    for retry in 0..=MAX_MODEL_TRANSPORT_RETRIES {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(ModelAttemptError::Terminal(error)) => return Err(error),
            Err(ModelAttemptError::Retryable { error, retry_after }) => {
                if retry == MAX_MODEL_TRANSPORT_RETRIES {
                    return Err(error);
                }
                let backoff = Duration::from_millis(150 * (1u64 << retry));
                let delay = retry_after.unwrap_or(backoff);
                if delay > MAX_MODEL_RETRY_DELAY {
                    return Err(AgentError::Model(format!(
                        "{error}; Retry-After exceeds the bounded retry window"
                    )));
                }
                tokio::select! {
                    _ = cancel.cancelled() => return Err(AgentError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
    unreachable!("bounded model retry loop returns on every path")
}

pub(crate) async fn send_model_request(
    request: reqwest::RequestBuilder,
    cancel: &CancellationToken,
) -> Result<reqwest::Response, ModelAttemptError> {
    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(ModelAttemptError::Terminal(AgentError::Cancelled)),
        response = request.send() => response.map_err(ModelAttemptError::request)?,
    };
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retryable_status =
        status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
    let retry_after = if retryable_status {
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after)
    } else {
        None
    };
    let bytes = match read_model_http_body(response, MAX_MODEL_HTTP_ERROR_BYTES, cancel).await {
        Ok(bytes) => bytes,
        Err(AgentError::Model(detail)) if retryable_status => {
            return Err(ModelAttemptError::Retryable {
                error: AgentError::Model(format!("HTTP {status}: {detail}")),
                retry_after,
            });
        }
        Err(error) => return Err(ModelAttemptError::Terminal(error)),
    };
    let detail: String = String::from_utf8_lossy(&bytes).chars().take(500).collect();
    let error = AgentError::Model(format!("HTTP {status}: {detail}"));
    if retryable_status {
        Err(ModelAttemptError::Retryable { error, retry_after })
    } else {
        Err(ModelAttemptError::Terminal(error))
    }
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let parts = value.split_ascii_whitespace().collect::<Vec<_>>();
    let [weekday, day, month, year, clock, zone] = parts.as_slice() else {
        return None;
    };
    if !weekday.ends_with(',') || *zone != "GMT" {
        return None;
    }
    let day = day.parse::<u32>().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|candidate| candidate == month)? as u32
        + 1;
    let year = year.parse::<i64>().ok()?;
    if !(1970..=9999).contains(&year) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let time = clock.split(':').collect::<Vec<_>>();
    let [hour, minute, second] = time.as_slice() else {
        return None;
    };
    let (hour, minute, second) = (
        hour.parse::<u64>().ok()?,
        minute.parse::<u64>().ok()?,
        second.parse::<u64>().ok()?,
    );
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year / 400;
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let day_of_year = (153 * adjusted_month + 2) / 5 + i64::from(day) - 1;
    let year_day = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + year_day - 719_468;
    let timestamp = u64::try_from(days)
        .ok()?
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(timestamp.saturating_sub(now)))
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

/// Adapter for the common `/chat/completions` wire format.
pub struct OpenAiCompatibleBackend {
    endpoint: String,
    api_key: Option<String>,
    model: String,
    capabilities: ModelRequestCapabilities,
    reasoning_effort: Option<String>,
    request_fields: BTreeMap<String, Value>,
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
        Self {
            endpoint,
            api_key,
            model: model.into(),
            capabilities: ModelRequestCapabilities::default(),
            reasoning_effort: None,
            request_fields: BTreeMap::new(),
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

    /// Attach the capability declaration for this provider, model, and route.
    /// Call this before requesting function tools or an explicit effort.
    pub fn with_capabilities(
        mut self,
        capabilities: ModelRequestCapabilities,
    ) -> Result<Self, AgentError> {
        capabilities.validate()?;
        if let Some(effort) = &self.reasoning_effort {
            validate_reasoning_effort(&self.model, &capabilities, effort)?;
        }
        self.capabilities = capabilities;
        Ok(self)
    }

    pub fn capabilities(&self) -> &ModelRequestCapabilities {
        &self.capabilities
    }

    /// Add a documented provider-specific Chat Completions request field.
    /// Common fields owned by this adapter cannot be replaced.
    pub fn with_request_field(
        mut self,
        field: impl Into<String>,
        value: Value,
    ) -> Result<Self, AgentError> {
        let field = field.into();
        if field.is_empty()
            || field.len() > 64
            || !field
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            || ["model", "messages", "tools", "stream", "reasoning_effort"]
                .contains(&field.as_str())
            || self.request_fields.len() >= 16 && !self.request_fields.contains_key(&field)
            || serde_json::to_vec(&value).is_ok_and(|encoded| encoded.len() > 8 * 1024)
        {
            return Err(AgentError::InvalidRequest(
                "invalid or oversized Chat Completions request field".into(),
            ));
        }
        self.request_fields.insert(field, value);
        Ok(self)
    }

    /// Opt in to the Chat Completions `reasoning_effort` parameter if the
    /// selected provider/model/route advertises this exact value.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Result<Self, AgentError> {
        let effort = effort.into();
        validate_reasoning_effort(&self.model, &self.capabilities, &effort)?;
        self.reasoning_effort = Some(effort);
        Ok(self)
    }

    fn request_body(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        stream: bool,
    ) -> Result<Value, AgentError> {
        if messages.iter().any(|message| !message.images.is_empty()) {
            return Err(AgentError::InvalidRequest(
                "native Chat Completions transport does not accept user images; use Responses with an image-capable model".into(),
            ));
        }
        if !self.capabilities.tool_calls && !tools.is_empty() {
            return Err(AgentError::InvalidRequest(
                "function tools are not enabled for this model and Chat Completions route".into(),
            ));
        }
        let effort = self.reasoning_effort.as_deref().or({
            if tools.is_empty() {
                None
            } else {
                self.capabilities.default_tool_call_effort.as_deref()
            }
        });
        if !tools.is_empty()
            && effort.is_some_and(|value| {
                !self
                    .capabilities
                    .efforts_with_tools()
                    .iter()
                    .any(|allowed| allowed == value)
            })
        {
            return Err(AgentError::InvalidRequest(format!(
                "reasoning effort '{}' is not enabled with function tools for model '{}' on this route",
                effort.unwrap_or_default(),
                self.model
            )));
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
        if let Some(effort) = effort {
            body["reasoning_effort"] = Value::String(effort.to_owned());
        }
        for (field, value) in &self.request_fields {
            body[field.as_str()] = value.clone();
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

fn validate_reasoning_effort(
    model: &str,
    capabilities: &ModelRequestCapabilities,
    effort: &str,
) -> Result<(), AgentError> {
    if !capabilities
        .reasoning_efforts
        .iter()
        .any(|allowed| allowed == effort)
    {
        return Err(AgentError::InvalidRequest(format!(
            "reasoning effort '{effort}' is not enabled for model '{model}' on this route"
        )));
    }
    Ok(())
}

#[async_trait]
impl ModelBackend for OpenAiCompatibleBackend {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        let capabilities = serde_json::to_string(&self.capabilities)
            .expect("validated model capability contract serializes");
        let request_fields =
            serde_json::to_string(&self.request_fields).expect("JSON request fields serialize");
        Some(stable_request_profile_fingerprint(&[
            "chat-completions-v1",
            &self.endpoint,
            &self.model,
            &capabilities,
            self.reasoning_effort.as_deref().unwrap_or("<absent>"),
            &request_fields,
            "stream-true",
        ]))
    }

    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        retry_model_transport(&cancel, || async {
            let request = self
                .request(messages, tools, false)
                .map_err(ModelAttemptError::Terminal)?;
            let response = send_model_request(request, &cancel).await?;
            let bytes = read_model_http_body(response, MAX_MODEL_HTTP_BODY_BYTES, &cancel)
                .await
                .map_err(ModelAttemptError::Terminal)?;
            let body: Value = serde_json::from_slice(&bytes).map_err(|error| {
                ModelAttemptError::Terminal(AgentError::Model(format!(
                    "invalid JSON response: {error}"
                )))
            })?;
            parse_model_output(&body).map_err(ModelAttemptError::Terminal)
        })
        .await
    }

    async fn stream_complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelOutput, AgentError> {
        self.stream_complete_detailed(messages, tools, cancel, deltas)
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
        retry_model_transport(&cancel, || async {
            let request = self
                .request(messages, tools, true)
                .map_err(ModelAttemptError::Terminal)?;
            let response = send_model_request(request, &cancel).await?;
            let is_sse = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/event-stream"));
            let mut stream = Box::pin(response.bytes_stream());
            let mut pending = Vec::<u8>::new();
            let mut assembler = SseAssembler::default();
            loop {
                let next = tokio::select! {
                    _ = cancel.cancelled() => return Err(ModelAttemptError::Terminal(AgentError::Cancelled)),
                    next = stream.next() => next,
                };
                let Some(next) = next else { break };
                let chunk = next.map_err(|error| ModelAttemptError::stream(error, assembler.published_delta))?;
                pending.extend_from_slice(&chunk);
                if pending.len() > MAX_STREAM_BUFFER_BYTES {
                    return Err(ModelAttemptError::Terminal(AgentError::Limit("streaming response is too large".into())));
                }
                while let Some(frame) = take_sse_frame(&mut pending) {
                    assembler.ingest(&frame, &deltas).map_err(ModelAttemptError::Terminal)?;
                    if assembler.done {
                        return assembler.finish_detailed().map_err(ModelAttemptError::Terminal);
                    }
                }
            }
            if assembler.seen_frame || is_sse {
                // A truncated stream may be repeated only before live output.
                Err(ModelAttemptError::incomplete_stream(
                    "streaming response ended before [DONE]",
                    assembler.published_delta,
                ))
            } else {
                // Some compatible providers ignore `stream: true` and return ordinary JSON.
                let body: Value = serde_json::from_slice(&pending).map_err(|error| {
                    ModelAttemptError::Terminal(AgentError::Model(format!("invalid streaming response: {error}")))
                })?;
                Ok(ModelCompletion {
                    output: parse_model_output(&body).map_err(ModelAttemptError::Terminal)?,
                    response_id: body.get("id").and_then(Value::as_str).map(str::to_owned),
                    usage: parse_chat_usage(&body).map_err(ModelAttemptError::Terminal)?,
                    provider_output_items: Vec::new(),
                })
            }
        })
        .await
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
    published_delta: bool,
    usage: Option<ModelTokenUsage>,
    response_id: Option<String>,
    finish_reason: Option<String>,
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
        if let Some(id) = payload.get("id").and_then(Value::as_str) {
            self.response_id.get_or_insert_with(|| id.to_owned());
        }
        if let Some(usage) = parse_chat_usage(&payload)? {
            self.usage = Some(usage);
        }
        if let Some(reason) = payload
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            if self
                .finish_reason
                .as_deref()
                .is_some_and(|prior| prior != reason)
            {
                return Err(AgentError::Model(
                    "streaming response has conflicting finish reasons".into(),
                ));
            }
            self.finish_reason = Some(reason.to_owned());
        }
        let Some(delta) = payload.pointer("/choices/0/delta") else {
            return Ok(());
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            append_assembled_fragment(&mut self.content, text, &mut self.assembled_bytes)?;
            self.published_delta = true;
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
        if !self.done {
            return Err(AgentError::Model(
                "streaming response ended before [DONE]".into(),
            ));
        }
        validate_finish_reason(self.finish_reason.as_deref())?;
        if !self.calls.is_empty()
            && !matches!(
                self.finish_reason.as_deref(),
                Some("tool_calls" | "function_call")
            )
        {
            return Err(AgentError::Model(
                "streaming tool calls have no matching finish reason".into(),
            ));
        }
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

    fn finish_detailed(self) -> Result<ModelCompletion, AgentError> {
        let response_id = self.response_id.clone();
        let usage = self.usage.clone();
        Ok(ModelCompletion {
            output: self.finish()?,
            response_id,
            usage,
            provider_output_items: Vec::new(),
        })
    }
}

fn parse_chat_usage(body: &Value) -> Result<Option<ModelTokenUsage>, AgentError> {
    let Some(usage) = body.get("usage").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let required = |field: &str| {
        usage.get(field).and_then(Value::as_u64).ok_or_else(|| {
            AgentError::Model(format!("Chat Completions usage has no valid {field}"))
        })
    };
    Ok(Some(ModelTokenUsage {
        input_tokens: required("prompt_tokens")?,
        output_tokens: required("completion_tokens")?,
        total_tokens: required("total_tokens")?,
        reasoning_tokens: usage
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
        cached_input_tokens: usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
    }))
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

pub(crate) fn take_sse_frame(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
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
    let finish_reason = body
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    validate_finish_reason(finish_reason)?;
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
    if !tool_calls.is_empty() && !matches!(finish_reason, Some("tool_calls" | "function_call")) {
        return Err(AgentError::Model(
            "tool calls have no matching finish reason".into(),
        ));
    }
    Ok(ModelOutput {
        content,
        tool_calls,
    })
}

fn validate_finish_reason(reason: Option<&str>) -> Result<(), AgentError> {
    match reason {
        None | Some("stop" | "tool_calls" | "function_call") => Ok(()),
        Some("length") => Err(AgentError::Model(
            "model stopped at its output limit; refusing an incomplete response".into(),
        )),
        Some("content_filter") => Err(AgentError::Model(
            "provider filtered the model response".into(),
        )),
        Some(other) => Err(AgentError::Model(format!(
            "unsupported model finish reason: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::{UserImage, UserImageMime};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn scripted_http_server(responses: Vec<String>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let server = tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .expect("expected HTTP request")
                        .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0, "request ended before headers");
                    request.extend_from_slice(&chunk[..count]);
                    if let Some(header_end) =
                        request.windows(4).position(|part| part == b"\r\n\r\n")
                    {
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
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(450), listener.accept())
                    .await
                    .is_err(),
                "unexpected model retry"
            );
        });
        (address, server)
    }

    fn scripted_response(
        status: &str,
        body: &str,
        extra_headers: &str,
        declared_len: usize,
    ) -> String {
        format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {declared_len}\r\nConnection: close\r\n{extra_headers}\r\n{body}")
    }

    fn sample_tool() -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: json!({"type":"object"}),
        }
    }

    fn tool_capabilities(efforts: &[&str]) -> ModelRequestCapabilities {
        ModelRequestCapabilities {
            tool_calls: true,
            reasoning_efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
            tool_call_reasoning_efforts: None,
            default_tool_call_effort: None,
        }
    }

    #[test]
    fn chat_transport_rejects_images_before_http() {
        let image = UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let message = AgentMessage::user_with_images("describe", vec![image]).unwrap();
        let chat = OpenAiCompatibleBackend::new("http://127.0.0.1:1/v1", None, "mock");
        assert!(!chat.supports_user_images());
        let error = chat.request_body(&[message], &[], true).unwrap_err();
        assert!(
            matches!(error, AgentError::InvalidRequest(detail) if detail.contains("does not accept user images"))
        );
    }

    #[test]
    fn retry_after_accepts_http_dates_and_bounds_future_advice() {
        assert_eq!(
            parse_retry_after("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(Duration::ZERO)
        );
        assert!(
            parse_retry_after("Fri, 31 Dec 9999 23:59:59 GMT").unwrap() > MAX_MODEL_RETRY_DELAY
        );
        assert_eq!(parse_retry_after("not a date"), None);
    }

    #[test]
    fn reasoning_effort_is_explicit_in_both_streaming_and_regular_requests() {
        let backend = OpenAiCompatibleBackend::new("http://localhost/v1", None, "vendor/model-a")
            .with_capabilities(tool_capabilities(&["low", "high", "turbo"]))
            .unwrap()
            .with_reasoning_effort("high")
            .unwrap();
        let messages = [AgentMessage::user("hello")];
        for streaming in [false, true] {
            let body = backend
                .request_body(&messages, &[sample_tool()], streaming)
                .unwrap();
            assert_eq!(body["model"], "vendor/model-a");
            assert_eq!(body["reasoning_effort"], "high");
            assert_eq!(body["stream"], streaming);
            assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        }
    }

    #[test]
    fn unknown_capabilities_do_not_accept_decorative_effort_or_tools() {
        assert!(
            OpenAiCompatibleBackend::new("http://localhost/v1", None, "vendor/model-a")
                .with_reasoning_effort("high")
                .is_err()
        );
        let unknown = OpenAiCompatibleBackend::new("http://localhost/v1", None, "vendor/model-a");
        assert!(unknown.request_body(&[], &[sample_tool()], false).is_err());
        let configured = unknown
            .with_capabilities(tool_capabilities(&["turbo"]))
            .unwrap();
        assert!(configured
            .request_body(&[], &[sample_tool()], false)
            .is_ok());
        assert!(
            OpenAiCompatibleBackend::new("http://localhost/v1", None, "vendor/model-a")
                .with_capabilities(tool_capabilities(&["turbo"]))
                .unwrap()
                .with_reasoning_effort("ultra")
                .is_err()
        );
    }

    #[test]
    fn provider_specific_request_fields_are_explicit_and_bounded() {
        let backend = OpenAiCompatibleBackend::new("https://provider.example/v1", None, "model")
            .with_capabilities(tool_capabilities(&[]))
            .unwrap()
            .with_request_field("thinking", json!({"type": "disabled"}))
            .unwrap();
        let body = backend
            .request_body(&[AgentMessage::user("hello")], &[sample_tool()], true)
            .unwrap();
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert!(backend
            .with_request_field("model", json!("override"))
            .is_err());
        assert!(
            OpenAiCompatibleBackend::new("http://localhost", None, "model")
                .with_request_field("option", json!("x".repeat(9_000)))
                .is_err()
        );
    }

    #[test]
    fn request_profile_changes_with_reasoning_and_capabilities() {
        let base = OpenAiCompatibleBackend::new("http://localhost/v1", None, "vendor/model-a");
        let configured =
            OpenAiCompatibleBackend::new("http://localhost/v1", None, "vendor/model-a")
                .with_capabilities(tool_capabilities(&["high"]))
                .unwrap();
        let high = OpenAiCompatibleBackend::new("http://localhost/v1", None, "vendor/model-a")
            .with_capabilities(tool_capabilities(&["high"]))
            .unwrap()
            .with_reasoning_effort("high")
            .unwrap();
        assert_ne!(
            base.request_profile_fingerprint(),
            configured.request_profile_fingerprint()
        );
        assert_ne!(
            configured.request_profile_fingerprint(),
            high.request_profile_fingerprint()
        );

        let generic = OpenAiCompatibleBackend::new("https://provider.example", None, "model");
        let with_option = OpenAiCompatibleBackend::new("https://provider.example", None, "model")
            .with_request_field("thinking", json!({"type":"disabled"}))
            .unwrap();
        assert_ne!(
            generic.request_profile_fingerprint(),
            with_option.request_profile_fingerprint()
        );
    }

    #[test]
    fn chat_completion_tool_effort_contract_is_checked_before_http() {
        let caps = ModelRequestCapabilities {
            tool_calls: true,
            reasoning_efforts: vec!["none".into(), "medium".into()],
            tool_call_reasoning_efforts: Some(vec!["none".into()]),
            default_tool_call_effort: Some("none".into()),
        };
        let backend = OpenAiCompatibleBackend::new("http://localhost/v1", None, "any-model-id")
            .with_capabilities(caps.clone())
            .unwrap();
        assert_eq!(backend.capabilities().efforts_with_tools(), &["none"]);
        assert_eq!(
            backend.request_body(&[], &[sample_tool()], false).unwrap()["reasoning_effort"],
            "none"
        );
        assert!(backend
            .request_body(&[], &[], false)
            .unwrap()
            .get("reasoning_effort")
            .is_none());
        let medium = OpenAiCompatibleBackend::new("http://localhost/v1", None, "any-model-id")
            .with_capabilities(caps)
            .unwrap()
            .with_reasoning_effort("medium")
            .unwrap();
        assert!(medium.request_body(&[], &[sample_tool()], false).is_err());
        assert_eq!(
            medium.request_body(&[], &[], false).unwrap()["reasoning_effort"],
            "medium"
        );

        let denied = OpenAiCompatibleBackend::new("http://localhost/v1", None, "same-model-id")
            .with_capabilities(ModelRequestCapabilities {
                tool_calls: false,
                reasoning_efforts: vec!["medium".into()],
                ..Default::default()
            })
            .unwrap();
        assert!(denied.request_body(&[], &[sample_tool()], false).is_err());
        assert!(denied.with_reasoning_effort("medium").is_ok());
    }

    #[test]
    fn capability_contract_rejects_invalid_tool_effort_defaults() {
        let backend = OpenAiCompatibleBackend::new("http://localhost/v1", None, "custom");
        assert!(backend
            .with_capabilities(ModelRequestCapabilities {
                tool_calls: true,
                reasoning_efforts: vec!["medium".into()],
                tool_call_reasoning_efforts: Some(vec!["none".into()]),
                default_tool_call_effort: Some("none".into()),
            })
            .is_err());
    }

    #[test]
    fn parses_tool_calls_and_null_content() {
        let output = parse_model_output(&json!({
            "choices": [{"finish_reason":"tool_calls","message": {
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
        assembler
            .ingest(
                b"data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}",
                &sender,
            )
            .unwrap();
        assembler.ingest(b"data: [DONE]", &sender).unwrap();
        let result = assembler.finish().unwrap();
        assert_eq!(result.content, "Hi there");
        assert_eq!(result.tool_calls[0].arguments["path"], "a.txt");
        assert_eq!(receiver.try_recv().unwrap(), "Hi ");
        assert_eq!(receiver.try_recv().unwrap(), "there");
    }

    #[test]
    fn refuses_output_limit_even_if_tool_arguments_are_valid_json() {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut assembler = SseAssembler::default();
        let call = json!({"choices":[{"delta":{"tool_calls":[{
            "index":0,"id":"call-1","function":{"name":"write_file","arguments":"{\"path\":\"a.txt\",\"content\":\"x\"}"}
        }]}}]});
        assembler
            .ingest(format!("data: {call}").as_bytes(), &sender)
            .unwrap();
        assembler
            .ingest(
                b"data: {\"choices\":[{\"finish_reason\":\"length\"}]}",
                &sender,
            )
            .unwrap();
        assembler.ingest(b"data: [DONE]", &sender).unwrap();
        assert!(matches!(
            assembler.finish(),
            Err(AgentError::Model(message)) if message.contains("output limit")
        ));
        assert!(matches!(
            parse_model_output(&json!({"choices":[{"finish_reason":"length","message":{"content":"","tool_calls":[]}}]})),
            Err(AgentError::Model(message)) if message.contains("output limit")
        ));
    }

    #[test]
    fn refuses_tool_calls_without_matching_finish_reason() {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let call = json!({"choices":[{"delta":{"tool_calls":[{
            "index":0,"id":"call-1","function":{"name":"write_file","arguments":"{}"}
        }]}}]});
        for finish_reason in [None, Some("stop")] {
            let mut assembler = SseAssembler::default();
            assembler
                .ingest(format!("data: {call}").as_bytes(), &sender)
                .unwrap();
            if let Some(reason) = finish_reason {
                assembler
                    .ingest(
                        format!("data: {}", json!({"choices":[{"finish_reason":reason}]}))
                            .as_bytes(),
                        &sender,
                    )
                    .unwrap();
            }
            assembler.ingest(b"data: [DONE]", &sender).unwrap();
            assert!(matches!(
                assembler.finish(),
                Err(AgentError::Model(message)) if message.contains("matching finish reason")
            ));
        }
        assert!(matches!(
            parse_model_output(&json!({"choices":[{"finish_reason":"stop","message":{
                "content":"","tool_calls":[{"id":"call-1","function":{"name":"write_file","arguments":"{}"}}]
            }}]})),
            Err(AgentError::Model(message)) if message.contains("matching finish reason")
        ));
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
                assert!(reader.read_line(&mut line).unwrap() > 0);
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

    #[tokio::test]
    async fn bounds_non_streaming_http_body_while_reading() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            let body = "x".repeat(64);
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            socket.flush().unwrap();
        });
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/body"))
            .send()
            .await
            .unwrap();
        let result = read_model_http_body(response, 16, &CancellationToken::new()).await;
        server.join().unwrap();
        assert!(matches!(
            result,
            Err(AgentError::Limit(message)) if message.contains("16 bytes")
        ));
    }

    #[tokio::test]
    async fn chat_retries_429_then_streams_live() {
        let success =
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
        let (address, server) = scripted_http_server(vec![
            scripted_response("429 Too Many Requests", "busy", "Retry-After: 0\r\n", 4),
            scripted_response("200 OK", success, "", success.len()),
        ])
        .await;
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
        assert_eq!(output.content, "hello");
        assert_eq!(receiver.try_recv().unwrap(), "hello");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn chat_retries_broken_sse_only_before_first_delta() {
        let pre_delta = "data: {\"choices\":[{\"delta\":{}}]}\n\n";
        let success = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
        let (address, server) = scripted_http_server(vec![
            scripted_response("200 OK", pre_delta, "", pre_delta.len() + 40),
            scripted_response("200 OK", success, "", success.len()),
        ])
        .await;
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
        assert_eq!(output.content, "ok");
        assert_eq!(receiver.try_recv().unwrap(), "ok");
        assert!(receiver.try_recv().is_err());
        server.await.unwrap();

        let emitted = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
        let (address, server) = scripted_http_server(vec![scripted_response(
            "200 OK",
            emitted,
            "",
            emitted.len() + 40,
        )])
        .await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let error = backend
            .stream_complete(
                &[AgentMessage::user("hi")],
                &[],
                CancellationToken::new(),
                sender,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::Model(_)));
        assert_eq!(receiver.try_recv().unwrap(), "partial");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn responses_retries_503_and_pre_delta_disconnect_but_not_post_delta() {
        let completed = "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_retry\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}]}}\n\n";
        let (address, server) = scripted_http_server(vec![
            scripted_response("503 Service Unavailable", "busy", "Retry-After: 0\r\n", 4),
            scripted_response("200 OK", completed, "", completed.len()),
        ])
        .await;
        let backend = crate::ResponsesBackend::new(format!("http://{address}/v1"), None, "mock");
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        let result = backend
            .stream_complete_detailed(
                &[AgentMessage::user("hi")],
                &[],
                CancellationToken::new(),
                sender,
            )
            .await
            .unwrap();
        assert_eq!(result.output.content, "ok");
        server.await.unwrap();

        let created = "data: {\"type\":\"response.created\"}\n\n";
        let (address, server) = scripted_http_server(vec![
            scripted_response("200 OK", created, "", created.len() + 40),
            scripted_response("200 OK", completed, "", completed.len()),
        ])
        .await;
        let backend = crate::ResponsesBackend::new(format!("http://{address}/v1"), None, "mock");
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        let result = backend
            .stream_complete_detailed(
                &[AgentMessage::user("hi")],
                &[],
                CancellationToken::new(),
                sender,
            )
            .await
            .unwrap();
        assert_eq!(result.output.content, "ok");
        server.await.unwrap();

        let emitted = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n";
        let (address, server) = scripted_http_server(vec![scripted_response(
            "200 OK",
            emitted,
            "",
            emitted.len() + 40,
        )])
        .await;
        let backend = crate::ResponsesBackend::new(format!("http://{address}/v1"), None, "mock");
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let error = backend
            .stream_complete_detailed(
                &[AgentMessage::user("hi")],
                &[],
                CancellationToken::new(),
                sender,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::Model(_)));
        assert_eq!(receiver.try_recv().unwrap(), "partial");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn retry_after_is_bounded_and_cancellation_stops_waiting() {
        let (address, server) = scripted_http_server(vec![scripted_response(
            "429 Too Many Requests",
            "busy",
            "Retry-After: 60\r\n",
            4,
        )])
        .await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let error = backend
            .complete(&[AgentMessage::user("hi")], &[], CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentError::Model(message) if message.contains("Retry-After exceeds"))
        );
        server.await.unwrap();

        let (address, server) = scripted_http_server(vec![scripted_response(
            "429 Too Many Requests",
            "busy",
            "Retry-After: 2\r\n",
            4,
        )])
        .await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let cancel = CancellationToken::new();
        let cancellation = cancel.clone();
        let task = tokio::spawn(async move {
            backend
                .complete(&[AgentMessage::user("hi")], &[], cancellation)
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
        assert!(matches!(task.await.unwrap(), Err(AgentError::Cancelled)));
        server.await.unwrap();

        let (address, server) = scripted_http_server(vec![scripted_response(
            "400 Bad Request",
            "invalid request",
            "",
            15,
        )])
        .await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let error = backend
            .complete(&[AgentMessage::user("hi")], &[], CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::Model(message) if message.contains("HTTP 400")));
        server.await.unwrap();
    }
}
