#[cfg(test)]
use crate::model_stream::publish_legacy;
use crate::model_stream::ModelStreamDelivery;
use crate::provider_failure::SHORT_RATE_LIMIT;
use crate::ModelEventSender;
use crate::{FailureClass, ProviderFailure};
use crate::{AgentError, ToolDefinition};
use async_trait::async_trait;
use futures_util::StreamExt;
use openide_core::{AgentMessage, MessageRole, ModelStreamEvent, ToolCall};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

#[path = "chat_reasoning.rs"]
mod chat_reasoning;
use chat_reasoning::ChatReasoning;
#[cfg(test)]
#[path = "provider_chat_contract_tests.rs"]
mod provider_chat_contract_tests;

const MAX_STREAM_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const MAX_ASSEMBLED_STREAM_BYTES: usize = 8 * 1024 * 1024;
/// Per-response adapter resource cap. RunLimits bounds execution across a turn.
/// This is independent of model/provider capabilities and replay cache entries.
pub(crate) const MAX_MODEL_TOOL_CALLS: usize = 64;
pub(crate) const MAX_MODEL_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_MODEL_HTTP_ERROR_BYTES: usize = 64 * 1024;
const MAX_MODEL_TRANSPORT_RETRIES: u32 = 6;
const MAX_RATE_LIMITS_IN_A_ROW: u32 = 3;
const MODEL_RETRY_BASE: Duration = Duration::from_millis(500);
const MAX_MODEL_RETRY_BACKOFF: Duration = Duration::from_secs(30);
/// Shorter waits than this pass without a visible retry line.
const VISIBLE_RETRY_AFTER: Duration = Duration::from_secs(5);

/// Closed diagnostic categories: never expose arbitrary header/body values.
pub(crate) fn model_response_mime(headers: &reqwest::header::HeaderMap) -> &'static str {
    let Some(value) = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return "missing";
    };
    let value = value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match value.as_str() {
        "text/event-stream" => "sse",
        "application/json" => "json",
        "text/html" => "html",
        "text/plain" => "text",
        _ if value.ends_with("+json") => "json",
        _ => "other",
    }
}

pub(crate) fn model_response_body_kind(bytes: &[u8]) -> &'static str {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let bytes = bytes.trim_ascii_start();
    if bytes.is_empty() {
        "empty"
    } else if bytes.starts_with(b"{") || bytes.starts_with(b"[") {
        "json"
    } else if bytes.starts_with(b"data:")
        || bytes.starts_with(b"event:")
        || bytes.starts_with(b":")
        || bytes.starts_with(b"id:")
        || bytes.starts_with(b"retry:")
    {
        "sse"
    } else if bytes.starts_with(b"<") {
        "markup"
    } else {
        "other"
    }
}

#[derive(Default)]
enum ModelBodyFormat {
    #[default]
    Undecided,
    Json,
    Sse,
}

/// Keep ordinary JSON outside the SSE framer, but accommodate streaming
/// gateways that omit or mislabel Content-Type. A declared SSE body always
/// remains SSE and requires a delimited terminal event, including at EOF.
/// Undeclared SSE is recognized from its initial field prefix as bytes arrive;
/// it is not buffered until EOF, so previews and backpressure remain live.
pub(crate) struct ModelStreamBody {
    format: ModelBodyFormat,
    pending: SseFrameBuffer,
    sniff_offset: usize,
    sniff_bom_checked: bool,
}

impl ModelStreamBody {
    pub(crate) fn new(headers: &reqwest::header::HeaderMap) -> Self {
        Self {
            format: if model_response_mime(headers) == "sse" {
                ModelBodyFormat::Sse
            } else {
                ModelBodyFormat::Undecided
            },
            pending: SseFrameBuffer::default(),
            sniff_offset: 0,
            sniff_bom_checked: false,
        }
    }

    pub(crate) fn extend(&mut self, chunk: &[u8], limit: usize) -> Result<(), ()> {
        self.pending.extend(chunk, limit)?;
        if matches!(self.format, ModelBodyFormat::Undecided) {
            let bytes = self.pending.bytes();
            if !self.sniff_bom_checked {
                const BOM: &[u8] = b"\xef\xbb\xbf";
                if bytes.len() < BOM.len() && BOM.starts_with(bytes) {
                    return Ok(());
                }
                self.sniff_offset = usize::from(bytes.starts_with(BOM)) * BOM.len();
                self.sniff_bom_checked = true;
            }
            // Advance over each leading whitespace byte only once. Otherwise
            // a long, fragmented prefix would make format sniffing quadratic.
            while bytes
                .get(self.sniff_offset)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.sniff_offset += 1;
            }
            if let Some(format) = sniff_model_body_format(&bytes[self.sniff_offset..]) {
                self.format = format;
            }
        }
        Ok(())
    }

    pub(crate) fn take_sse_frame(&mut self) -> Option<Vec<u8>> {
        self.is_sse()
            .then(|| self.pending.take_sse_frame())
            .flatten()
    }

    pub(crate) fn is_sse(&self) -> bool {
        matches!(self.format, ModelBodyFormat::Sse)
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        self.pending.bytes()
    }
}

fn sniff_model_body_format(bytes: &[u8]) -> Option<ModelBodyFormat> {
    if bytes.is_empty() {
        return None;
    }
    // An SSE field name can be split across arbitrary chunks.
    const SSE_PREFIXES: &[&[u8]] = &[b"data:", b"event:", b":", b"id:", b"retry:"];
    if SSE_PREFIXES.iter().any(|prefix| bytes.starts_with(prefix)) {
        return Some(ModelBodyFormat::Sse);
    }
    if SSE_PREFIXES.iter().any(|prefix| prefix.starts_with(bytes)) {
        return None;
    }
    Some(ModelBodyFormat::Json)
}

/// Bound connection setup and stalled reads without cutting off a healthy
/// long-running inference. reqwest's total timeout includes the entire body.
pub(crate) fn model_http_client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
}

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

    /// Exact input count for the complete request when the transport offers
    /// a counting API. None retains the text fallback; images require a count.
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

    /// The provider limit this backend draws from: endpoint plus credential,
    /// never the credential itself. Agents sharing a key pause together when
    /// that limit closes. None opts out of the shared gate.
    fn limit_key(&self) -> Option<String> {
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

    /// Bounded typed production transport. The default is a compatibility
    /// bridge for older backends: its cumulative text is capped at 8 MiB, but
    /// an implementation that synchronously floods its legacy unbounded
    /// sender cannot be backpressured. Production adapters override this.
    /// Accepted legacy text is drained before propagating a backend error;
    /// text still in the private legacy bridge after token cancellation is
    /// discarded. Already accepted bounded events remain with the consumer.
    async fn stream_events(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        events: ModelEventSender,
    ) -> Result<ModelCompletion, AgentError> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut completion =
            Box::pin(self.stream_complete_detailed(messages, tools, cancel.clone(), sender));
        let mut collected_bytes = 0usize;
        let result = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break Err(AgentError::Cancelled),
                error = events.interrupted() => return Err(error),
                delta = receiver.recv() => {
                    let Some(content) = delta else {
                        break tokio::select! {
                            biased;
                            _ = cancel.cancelled() => Err(AgentError::Cancelled),
                            error = events.interrupted() => Err(error),
                            result = &mut completion => result,
                        };
                    };
                    collected_bytes = checked_legacy_stream_bytes(collected_bytes, content.len())?;
                    if !content.is_empty() {
                        events.send(ModelStreamEvent::TextDelta { content }).await?;
                    }
                }
                result = &mut completion => break result,
            }
        };
        // Drop producers and close before draining, so a retained legacy
        // sender cannot append late output behind the finite accepted tail.
        drop(completion);
        receiver.close();
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        while let Ok(content) = receiver.try_recv() {
            collected_bytes = checked_legacy_stream_bytes(collected_bytes, content.len())?;
            if !content.is_empty() {
                events.send(ModelStreamEvent::TextDelta { content }).await?;
            }
        }
        let result = result?;
        let mut output_bytes = result.output.content.len();
        if result.output.tool_calls.len() > MAX_MODEL_TOOL_CALLS {
            return Err(AgentError::Limit(
                "legacy model completion exceeds 64 tool calls".into(),
            ));
        }
        for call in &result.output.tool_calls {
            output_bytes = checked_legacy_stream_bytes(output_bytes, call.id.len())?;
            output_bytes = checked_legacy_stream_bytes(output_bytes, call.name.len())?;
            output_bytes =
                checked_legacy_stream_bytes(output_bytes, call.arguments.to_string().len())?;
        }
        checked_legacy_stream_bytes(output_bytes, 0)?;
        for call in &result.output.tool_calls {
            events
                .send(ModelStreamEvent::ToolCallDelta {
                    item_id: call.id.clone(),
                    call_id: Some(call.id.clone()),
                    name: Some(call.name.clone()),
                    arguments_delta: call.arguments.to_string(),
                })
                .await?;
            events
                .send(ModelStreamEvent::ToolCallReady {
                    item_id: call.id.clone(),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                })
                .await?;
        }
        if let Some(usage) = &result.usage {
            events
                .send(ModelStreamEvent::Usage {
                    usage: usage.clone(),
                })
                .await?;
        }
        Ok(result)
    }
}

fn checked_legacy_stream_bytes(current: usize, additional: usize) -> Result<usize, AgentError> {
    current
        .checked_add(additional)
        .filter(|bytes| *bytes <= MAX_ASSEMBLED_STREAM_BYTES)
        .ok_or_else(|| AgentError::Limit("legacy model stream exceeds 8 MiB".into()))
}

pub(crate) fn limit_key_for(endpoint: &str, credential: Option<&str>) -> Option<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"openide-limit-key\0");
    hasher.update(endpoint.as_bytes());
    hasher.update([0]);
    hasher.update(credential.unwrap_or_default().as_bytes());
    let digest = hasher.finalize();
    Some(digest[..8].iter().map(|byte| format!("{byte:02x}")).collect())
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
    /// A classified failure is repeated only when its class allows it and no
    /// output of this attempt reached the consumer.
    pub(crate) fn classified(error: AgentError, published: bool) -> Self {
        match &error {
            AgentError::Provider(failure)
                if !published
                    && failure.class.retryable()
                    && failure.retry_after.is_none_or(|delay| delay <= SHORT_RATE_LIMIT) =>
            {
                let retry_after = failure.retry_after;
                Self::Retryable { error, retry_after }
            }
            _ => Self::Terminal(error),
        }
    }

    /// A lost connection is transient only while nothing of the attempt was
    /// published. After that it ends the turn as it always did: the visible
    /// prefix stays and nothing is sent again on its own.
    fn transport(message: String, published: bool) -> Self {
        if published {
            return Self::Terminal(AgentError::Model(message));
        }
        Self::classified(
            AgentError::Provider(ProviderFailure::new(FailureClass::Transient, message)),
            false,
        )
    }

    fn request(error: reqwest::Error) -> Self {
        if error.is_connect() || error.is_timeout() {
            Self::transport(error.to_string(), false)
        } else {
            Self::Terminal(AgentError::Model(error.to_string()))
        }
    }

    pub(crate) fn stream(error: reqwest::Error, published_delta: bool) -> Self {
        if error.is_connect() || error.is_timeout() || error.is_body() || error.is_decode() {
            Self::transport(error.to_string(), published_delta)
        } else {
            Self::Terminal(AgentError::Model(error.to_string()))
        }
    }

    pub(crate) fn incomplete_stream(message: &'static str, published_delta: bool) -> Self {
        Self::transport(message.into(), published_delta)
    }
}

static RETRY_SLEEP_DIVISOR: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(if cfg!(test) { 1000 } else { 1 });

/// Tests shorten every retry wait by this factor instead of sleeping for the
/// real schedule. The schedule reported to the consumer is unchanged.
#[doc(hidden)]
pub fn set_model_retry_sleep_divisor(divisor: u32) {
    RETRY_SLEEP_DIVISOR.store(divisor.max(1), std::sync::atomic::Ordering::Relaxed);
}

/// 0.5 s doubling to a 30 s cap, ±20 % so a tree of agents does not retry in
/// step.
fn retry_backoff(retry: u32) -> Duration {
    let base = MODEL_RETRY_BASE
        .saturating_mul(1u32 << retry.min(16))
        .min(MAX_MODEL_RETRY_BACKOFF);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    base.mul_f64(0.8 + 0.4 * f64::from(nanos % 1000) / 1000.0)
}

/// Retry only failures that cannot have returned a completed model response to
/// the runner. Streaming callers must mark an attempt terminal after publishing
/// the first delta, preserving live delivery and journaled partial output.
///
/// Bounded: at most `MAX_MODEL_TRANSPORT_RETRIES` repeats, and the third rate
/// limit in a row ends the loop as a reached limit. A wait is announced on
/// `notify` once the loop has been waiting for `VISIBLE_RETRY_AFTER`, and
/// always for a rate limit.
pub(crate) async fn retry_model_transport<T, F, Fut>(
    cancel: &CancellationToken,
    notify: Option<&ModelStreamDelivery>,
    mut attempt: F,
) -> Result<T, AgentError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ModelAttemptError>>,
{
    let mut waited = Duration::ZERO;
    let mut rate_limits_in_a_row = 0;
    for retry in 0..=MAX_MODEL_TRANSPORT_RETRIES {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(ModelAttemptError::Terminal(error)) => return Err(error),
            Err(ModelAttemptError::Retryable { error, retry_after }) => {
                let rate_limited = matches!(
                    &error,
                    AgentError::Provider(failure) if failure.class == FailureClass::RateLimited
                );
                rate_limits_in_a_row = if rate_limited { rate_limits_in_a_row + 1 } else { 0 };
                if retry == MAX_MODEL_TRANSPORT_RETRIES
                    || rate_limits_in_a_row == MAX_RATE_LIMITS_IN_A_ROW
                {
                    return Err(match error {
                        AgentError::Provider(failure) => {
                            AgentError::Provider(failure.escalate_rate_limit())
                        }
                        error => error,
                    });
                }
                let delay = retry_after.unwrap_or_else(|| retry_backoff(retry));
                waited += delay;
                if let Some(notify) = notify.filter(|_| rate_limited || waited >= VISIBLE_RETRY_AFTER) {
                    notify
                        .publish(vec![ModelStreamEvent::Retrying {
                            attempt: retry + 1,
                            max_attempts: MAX_MODEL_TRANSPORT_RETRIES,
                            delay_ms: delay.as_millis() as u64,
                            rate_limited,
                        }])
                        .await?;
                }
                let divisor = RETRY_SLEEP_DIVISOR.load(std::sync::atomic::Ordering::Relaxed);
                tokio::select! {
                    _ = cancel.cancelled() => return Err(AgentError::Cancelled),
                    _ = tokio::time::sleep(delay / divisor) => {}
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
    let headers = response.headers().clone();
    let bytes = match read_model_http_body(response, MAX_MODEL_HTTP_ERROR_BYTES, cancel).await {
        Ok(bytes) => bytes,
        // The status still classifies a response whose body could not be read.
        Err(AgentError::Model(detail)) => detail.into_bytes(),
        Err(error) => return Err(ModelAttemptError::Terminal(error)),
    };
    Err(ModelAttemptError::classified(
        AgentError::Provider(ProviderFailure::from_http(status.as_u16(), &headers, &bytes)),
        false,
    ))
}

pub(crate) fn parse_retry_after(value: &str) -> Option<Duration> {
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
            client: model_http_client_builder()
                .build()
                .expect("reqwest client configuration is valid"),
        }
    }

    /// Override the default idle timeout with an explicit total request limit.
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
        if matches!(field.as_str(), "max_tokens" | "max_completion_tokens") {
            let other = if field == "max_tokens" {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            if value
                .as_u64()
                .is_none_or(|limit| limit == 0 || limit > i64::MAX as u64)
                || self.request_fields.contains_key(other)
            {
                return Err(AgentError::InvalidRequest(
                    "invalid or conflicting Chat Completions output token limit".into(),
                ));
            }
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
            if field == "stream_options" && !stream {
                continue;
            }
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
        if stream {
            request = request.header(reqwest::header::ACCEPT, "text/event-stream");
        }
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

    fn limit_key(&self) -> Option<String> {
        limit_key_for(&self.endpoint, self.api_key.as_deref())
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
        retry_model_transport(&cancel, None, || async {
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
        self.stream_with_delivery(messages, tools, cancel, ModelStreamDelivery::Legacy(deltas))
            .await
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

impl OpenAiCompatibleBackend {
    async fn stream_with_delivery(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        delivery: ModelStreamDelivery,
    ) -> Result<ModelCompletion, AgentError> {
        retry_model_transport(&cancel, Some(&delivery), || async {
            let request = self
                .request(messages, tools, true)
                .map_err(ModelAttemptError::Terminal)?;
            let response = send_model_request(request, &cancel).await?;
            let status = response.status().as_u16();
            let mime = model_response_mime(response.headers());
            let mut pending = ModelStreamBody::new(response.headers());
            let mut stream = Box::pin(response.bytes_stream());
            let mut assembler = SseAssembler::default();
            loop {
                let next = tokio::select! {
                    _ = cancel.cancelled() => return Err(ModelAttemptError::Terminal(AgentError::Cancelled)),
                    next = stream.next() => next,
                };
                let Some(next) = next else { break };
                let chunk = next.map_err(|error| ModelAttemptError::stream(error, assembler.published_activity))?;
                if pending.extend(&chunk, MAX_STREAM_BUFFER_BYTES).is_err() {
                    return Err(ModelAttemptError::Terminal(AgentError::Limit("streaming response is too large".into())));
                }
                while let Some(frame) = pending.take_sse_frame() {
                    let mut events = Vec::new();
                    assembler.ingest_events(&frame, &mut events).map_err(|error| ModelAttemptError::classified(error, assembler.published_activity))?;
                    delivery.publish(events).await.map_err(ModelAttemptError::Terminal)?;
                    if assembler.done {
                        let ready = assembler.ready_events();
                        let completion = assembler.finish_detailed().map_err(ModelAttemptError::Terminal)?;
                        delivery.publish(ready).await.map_err(ModelAttemptError::Terminal)?;
                        if let Some(usage) = &completion.usage {
                            delivery.publish(vec![ModelStreamEvent::Usage { usage: usage.clone() }]).await.map_err(ModelAttemptError::Terminal)?;
                        }
                        return Ok(completion);
                    }
                }
            }
            if !pending.is_sse() {
                let bytes = pending.bytes();
                let body_kind = model_response_body_kind(bytes);
                let body: Value = serde_json::from_slice(bytes).map_err(|error| {
                    ModelAttemptError::Terminal(AgentError::Model(format!("invalid streaming response JSON: {error} (HTTP {status}; MIME {mime}; body {body_kind})")))
                })?;
                let mut reasoning = ChatReasoning::default();
                let mut reasoning_bytes = 0;
                let mut reasoning_events = Vec::new();
                reasoning.ingest(body.pointer("/choices/0/message").unwrap_or(&Value::Null), &mut reasoning_bytes, &mut reasoning_events).map_err(ModelAttemptError::Terminal)?;
                let completion = ModelCompletion {
                    output: parse_model_output(&body).map_err(ModelAttemptError::Terminal)?,
                    response_id: body.get("id").and_then(Value::as_str).map(str::to_owned),
                    usage: parse_chat_usage(&body).map_err(ModelAttemptError::Terminal)?,
                    provider_output_items: reasoning.into_items(),
                };
                let mut events = validated_completion_events(&completion);
                events.splice(1..1, reasoning_events);
                delivery.publish(events).await.map_err(ModelAttemptError::Terminal)?;
                return Ok(completion);
            }
            // A truncated stream may be repeated only before live output.
            Err(ModelAttemptError::incomplete_stream(
                "streaming response ended before [DONE]",
                assembler.published_activity,
            ))
        })
        .await
    }
}

/// Presentation derived from an already validated non-streaming fallback.
pub(crate) fn validated_completion_events(completion: &ModelCompletion) -> Vec<ModelStreamEvent> {
    let mut events = vec![ModelStreamEvent::Started {
        response_id: completion.response_id.clone(),
    }];
    if !completion.output.content.is_empty() {
        events.push(ModelStreamEvent::TextDelta {
            content: completion.output.content.clone(),
        });
    }
    for call in &completion.output.tool_calls {
        events.push(ModelStreamEvent::ToolCallDelta {
            item_id: call.id.clone(),
            call_id: Some(call.id.clone()),
            name: Some(call.name.clone()),
            arguments_delta: call.arguments.to_string(),
        });
        events.push(ModelStreamEvent::ToolCallReady {
            item_id: call.id.clone(),
            call_id: call.id.clone(),
            name: call.name.clone(),
        });
    }
    if let Some(usage) = &completion.usage {
        events.push(ModelStreamEvent::Usage {
            usage: usage.clone(),
        });
    }
    events
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
    reasoning: ChatReasoning,
    calls: BTreeMap<usize, PartialToolCall>,
    assembled_bytes: usize,
    done: bool,
    seen_frame: bool,
    published_delta: bool,
    published_activity: bool,
    started: bool,
    usage: Option<ModelTokenUsage>,
    response_id: Option<String>,
    finish_reason: Option<String>,
    provider_error: Option<String>,
}

impl SseAssembler {
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
        if let Some(error) = &self.provider_error {
            return Err(AgentError::Model(error.clone()));
        }
        let frame = std::str::from_utf8(frame)
            .map_err(|error| AgentError::Model(format!("invalid SSE UTF-8: {error}")))?;
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
            self.done = true;
            return Ok(());
        }
        let payload: Value = serde_json::from_str(&data)
            .map_err(|error| AgentError::Model(format!("invalid SSE payload: {error}")))?;
        if let Err(error) = validate_chat_provider_status(&payload) {
            self.provider_error = Some(error.to_string());
            return Err(error);
        }
        if let Some(error) = payload.get("error").filter(|error| !error.is_null()) {
            let detail = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .filter(|detail| !detail.is_empty())
                .unwrap_or("provider reported an error");
            let detail: String = detail.chars().take(500).collect();
            let message = format!("streaming response failed: {detail}");
            self.provider_error = Some(message.clone());
            return Err(AgentError::Provider(ProviderFailure::from_body(&payload, message)));
        }
        if let Some(id) = payload.get("id").and_then(Value::as_str) {
            self.response_id.get_or_insert_with(|| id.to_owned());
        }
        if !self.started {
            events.push(ModelStreamEvent::Started {
                response_id: self.response_id.clone(),
            });
            self.started = true;
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
        for text in ["content", "refusal"]
            .into_iter()
            .filter_map(|field| delta.get(field).and_then(Value::as_str))
        {
            if !text.is_empty() {
                append_assembled_fragment(&mut self.content, text, &mut self.assembled_bytes)?;
                self.published_delta = true;
                self.published_activity = true;
                events.push(ModelStreamEvent::TextDelta {
                    content: text.to_owned(),
                });
            }
        }
        let prior_reasoning_bytes = self.assembled_bytes;
        self.reasoning.ingest(delta, &mut self.assembled_bytes, events)?;
        self.published_activity |= self.assembled_bytes != prior_reasoning_bytes;
        // The legacy public reasoning alias is presentation-only.
        for reasoning in ["reasoning"]
            .into_iter()
            .filter_map(|field| delta.get(field).and_then(Value::as_str))
        {
            if !reasoning.is_empty() {
                self.assembled_bytes = self
                    .assembled_bytes
                    .checked_add(reasoning.len())
                    .filter(|bytes| *bytes <= MAX_ASSEMBLED_STREAM_BYTES)
                    .ok_or_else(|| {
                        AgentError::Limit(
                            "assembled streaming output exceeds the 8 MiB byte limit".into(),
                        )
                    })?;
                self.published_activity = true;
                events.push(ModelStreamEvent::ReasoningDelta {
                    item_id: "chat-reasoning-0".into(),
                    content: reasoning.to_owned(),
                });
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index =
                    call.get("index").and_then(Value::as_u64).ok_or_else(|| {
                        AgentError::Model("streaming tool call has no index".into())
                    })? as usize;
                if !self.calls.contains_key(&index) && self.calls.len() >= MAX_MODEL_TOOL_CALLS {
                    return Err(AgentError::Limit(format!(
                        "model response exceeds {MAX_MODEL_TOOL_CALLS} tool calls"
                    )));
                }
                let partial = self.calls.entry(index).or_default();
                let arguments_delta = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    if partial.id.is_empty() {
                        append_assembled_fragment(&mut partial.id, id, &mut self.assembled_bytes)?;
                    } else if partial.id != id {
                        return Err(AgentError::Model(
                            "streaming tool call changed identity".into(),
                        ));
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
                self.published_activity = true;
                events.push(ModelStreamEvent::ToolCallDelta {
                    item_id: format!("chat-tool-{index}"),
                    call_id: (!partial.id.is_empty()).then(|| partial.id.clone()),
                    name: (!partial.name.is_empty()).then(|| partial.name.clone()),
                    arguments_delta,
                });
            }
        }
        Ok(())
    }

    fn ready_events(&self) -> Vec<ModelStreamEvent> {
        self.calls
            .iter()
            .map(|(index, call)| ModelStreamEvent::ToolCallReady {
                item_id: format!("chat-tool-{index}"),
                call_id: call.id.clone(),
                name: call.name.clone(),
            })
            .collect()
    }

    fn finish(self) -> Result<ModelOutput, AgentError> {
        if let Some(error) = self.provider_error {
            return Err(AgentError::Model(error));
        }
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
        let mut call_ids = std::collections::BTreeSet::new();
        for (_, partial) in self.calls {
            if partial.id.is_empty() || partial.name.is_empty() {
                return Err(AgentError::Model("incomplete streaming tool call".into()));
            }
            if !call_ids.insert(partial.id.clone()) {
                return Err(AgentError::Model(
                    "streaming tool call ID is duplicated".into(),
                ));
            }
            let arguments = serde_json::from_str(&partial.arguments).map_err(|error| {
                AgentError::Model(format!("invalid streaming tool arguments: {error}"))
            })?;
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

    fn finish_detailed(mut self) -> Result<ModelCompletion, AgentError> {
        let response_id = self.response_id.clone();
        let usage = self.usage.clone();
        let provider_output_items = std::mem::take(&mut self.reasoning).into_items();
        Ok(ModelCompletion {
            output: self.finish()?,
            response_id,
            usage,
            provider_output_items,
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

/// Incremental byte framing for SSE's CR, LF, and CRLF line endings.
/// A blank line seals a frame; EOF alone never invents that boundary. UTF-8
/// is decoded only after framing, so a network chunk can split any code point.
/// https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream
#[derive(Default)]
pub(crate) struct SseFrameBuffer {
    buffer: Vec<u8>,
    frame_start: usize,
    scan: usize,
    line_start: usize,
    last_line_end: usize,
    after_cr: bool,
    bom_checked: bool,
    #[cfg(test)]
    scanned_bytes: usize,
}

impl SseFrameBuffer {
    /// Retain at most the caller's existing transport budget. Compact once
    /// per network chunk, never once per event in a chunk with many frames.
    pub(crate) fn extend(&mut self, chunk: &[u8], limit: usize) -> Result<(), ()> {
        if chunk.len() > limit.saturating_sub(self.bytes().len()) {
            return Err(());
        }
        if self.frame_start > 0 {
            let consumed = self.frame_start;
            self.buffer.drain(..consumed);
            self.scan -= consumed;
            self.line_start -= consumed;
            self.last_line_end -= consumed;
            self.frame_start = 0;
        }
        self.buffer.extend_from_slice(chunk);
        Ok(())
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.buffer[self.frame_start..]
    }

    pub(crate) fn take_sse_frame(&mut self) -> Option<Vec<u8>> {
        if !self.bom_checked {
            const BOM: &[u8] = b"\xef\xbb\xbf";
            if self.buffer.len() < BOM.len() && BOM.starts_with(&self.buffer) {
                return None;
            }
            self.bom_checked = true;
            if self.buffer.starts_with(BOM) {
                self.frame_start = BOM.len();
                self.scan = BOM.len();
                self.line_start = BOM.len();
                self.last_line_end = BOM.len();
            }
        }
        while self.scan < self.buffer.len() {
            let position = self.scan;
            let byte = self.buffer[position];
            self.scan += 1;
            #[cfg(test)]
            {
                self.scanned_bytes += 1;
            }
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    // CRLF is one terminator even when its LF arrives in the
                    // next chunk, or after a frame was returned at the CR.
                    if self.frame_start == position {
                        self.frame_start = self.scan;
                        self.last_line_end = self.scan;
                    }
                    self.line_start = self.scan;
                    continue;
                }
            }
            if byte != b'\r' && byte != b'\n' {
                continue;
            }
            let blank_line = self.line_start == position;
            self.after_cr = byte == b'\r';
            self.line_start = self.scan;
            if blank_line {
                let frame = self.buffer[self.frame_start..self.last_line_end].to_vec();
                self.frame_start = self.scan;
                self.last_line_end = self.scan;
                return Some(frame);
            }
            self.last_line_end = position;
        }
        None
    }
}

/// `str::lines` excludes bare CR. Split LF first and remove only its paired
/// CR, then split remaining bare CR without changing any field contents.
pub(crate) fn sse_lines(frame: &str) -> impl Iterator<Item = &str> {
    frame
        .split('\n')
        .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
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
    chat_reasoning::restore(message, &mut wire);
    wire
}

fn parse_model_output(body: &Value) -> Result<ModelOutput, AgentError> {
    validate_chat_provider_status(body)?;
    let finish_reason = body
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    validate_finish_reason(finish_reason)?;
    let message = body
        .pointer("/choices/0/message")
        .ok_or_else(|| AgentError::Model("response has no assistant message".into()))?;
    let mut content = String::new();
    let mut assembled_bytes = 0;
    match message.get("content") {
        Some(Value::String(text)) => {
            append_assembled_fragment(&mut content, text, &mut assembled_bytes)?
        }
        Some(Value::Array(parts)) => {
            for text in parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
            {
                append_assembled_fragment(&mut content, text, &mut assembled_bytes)?;
            }
        }
        Some(Value::Null) | None => {}
        _ => return Err(AgentError::Model("unsupported assistant content".into())),
    }
    if let Some(refusal) = message.get("refusal").and_then(Value::as_str) {
        append_assembled_fragment(&mut content, refusal, &mut assembled_bytes)?;
    }
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").filter(|calls| !calls.is_null()) {
        let calls = calls
            .as_array()
            .ok_or_else(|| AgentError::Model("tool_calls is not an array".into()))?;
        if calls.len() > MAX_MODEL_TOOL_CALLS {
            return Err(AgentError::Limit(format!(
                "model response exceeds {MAX_MODEL_TOOL_CALLS} tool calls"
            )));
        }
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
            let arguments_value = function
                .get("arguments")
                .ok_or_else(|| AgentError::Model("tool call has no arguments".into()))?;
            let argument_bytes = match arguments_value {
                Value::String(text) => text.len(),
                value => value.to_string().len(),
            };
            for bytes in [id.len(), name.len(), argument_bytes] {
                if bytes > MAX_ASSEMBLED_STREAM_BYTES.saturating_sub(assembled_bytes) {
                    return Err(AgentError::Limit(format!(
                        "assembled model output exceeds the {MAX_ASSEMBLED_STREAM_BYTES} byte limit"
                    )));
                }
                assembled_bytes += bytes;
            }
            let arguments = match arguments_value {
                Value::String(text) => serde_json::from_str(text).map_err(|error| {
                    AgentError::Model(format!("invalid tool arguments: {error}"))
                })?,
                value => value.clone(),
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

fn validate_chat_provider_status(body: &Value) -> Result<(), AgentError> {
    // MiniMax can report an API error in an HTTP 200 JSON/SSE envelope.
    if let Some(status) = body.pointer("/base_resp/status_code") {
        let code = status.as_i64().ok_or_else(|| AgentError::Model("invalid provider status code".into()))?;
        if code != 0 {
            let detail = body.pointer("/base_resp/status_msg").and_then(Value::as_str).unwrap_or("provider reported an error");
            let message = format!("provider error {code}: {}", detail.chars().take(500).collect::<String>());
            return Err(AgentError::Provider(ProviderFailure::from_body(body, message)));
        }
    }
    Ok(())
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

    #[test]
    fn sse_body_routing_sniffs_fragmented_fields_bom_and_missing_or_wrong_mime() {
        for mime in [
            None,
            Some("application/json"),
            Some("text/plain"),
            Some("text/event-stream"),
        ] {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Some(mime) = mime {
                headers.insert(reqwest::header::CONTENT_TYPE, mime.parse().unwrap());
            }
            for prefix in ["data:", "event:", ":", "id:", "retry:"] {
                let body = format!("\u{feff}{prefix} Café 中🦀\r\n\r\n");
                for chunk_size in [1, 2, 3, 7, body.len()] {
                    let mut pending = ModelStreamBody::new(&headers);
                    let mut frames = Vec::new();
                    for chunk in body.as_bytes().chunks(chunk_size) {
                        pending.extend(chunk, MAX_STREAM_BUFFER_BYTES).unwrap();
                        while let Some(frame) = pending.take_sse_frame() {
                            frames.push(frame);
                        }
                    }
                    assert!(pending.is_sse());
                    assert_eq!(frames.len(), 1);
                    assert_eq!(
                        std::str::from_utf8(&frames[0]).unwrap(),
                        format!("{prefix} Café 中🦀")
                    );
                    assert!(pending.bytes().is_empty());
                }
            }
        }
    }

    #[test]
    fn sse_body_routing_keeps_json_blank_lines_and_declared_sse_contract_distinct() {
        let body = b"\n \r\n{\n\n\"status\":\"completed\"\n\n}";
        for mime in [None, Some("application/json"), Some("text/plain")] {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Some(mime) = mime {
                headers.insert(reqwest::header::CONTENT_TYPE, mime.parse().unwrap());
            }
            let mut pending = ModelStreamBody::new(&headers);
            for chunk in body.chunks(1) {
                pending.extend(chunk, 1024).unwrap();
                assert!(pending.take_sse_frame().is_none());
            }
            assert!(!pending.is_sse());
            assert_eq!(pending.bytes(), body);
            assert!(serde_json::from_slice::<Value>(pending.bytes()).is_ok());
        }
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "text/event-stream".parse().unwrap(),
        );
        let mut pending = ModelStreamBody::new(&headers);
        pending
            .extend(b"{\"status\":\"completed\",\"output\":[]}", 1024)
            .unwrap();
        assert!(
            pending.is_sse(),
            "declared SSE never becomes an EOF JSON fallback"
        );
        assert!(pending.take_sse_frame().is_none());
    }

    #[test]
    fn sse_body_routing_sniffing_advances_linearly_and_retains_resource_bound() {
        let mut pending = ModelStreamBody::new(&reqwest::header::HeaderMap::new());
        for _ in 0..128 * 1024 {
            pending.extend(b" ", 256 * 1024).unwrap();
        }
        assert_eq!(pending.sniff_offset, 128 * 1024);
        pending.extend(b"d", 256 * 1024).unwrap();
        assert!(!pending.is_sse());
        pending.extend(b"ata:\n\n", 256 * 1024).unwrap();
        assert!(pending.is_sse());
        let mut capped = ModelStreamBody::new(&reqwest::header::HeaderMap::new());
        capped.extend(b"{", 4).unwrap();
        assert!(capped.extend(b"long", 4).is_err());
        assert_eq!(capped.bytes(), b"{");
    }

    #[test]
    fn sse_transport_diagnostics_are_closed_categories() {
        for (value, expected) in [
            (None, "missing"),
            (Some(" TEXT/EVENT-STREAM ; charset=utf-8"), "sse"),
            (Some("application/json"), "json"),
            (Some("application/problem+json"), "json"),
            (Some("text/html"), "html"),
            (Some("text/plain"), "text"),
            (Some("private-header-value"), "other"),
        ] {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Some(value) = value {
                headers.insert(reqwest::header::CONTENT_TYPE, value.parse().unwrap());
            }
            assert_eq!(model_response_mime(&headers), expected);
        }
        for (body, expected) in [
            ("", "empty"),
            (" \r\n", "empty"),
            ("{}", "json"),
            ("\u{feff}data: {}", "sse"),
            ("event: complete", "sse"),
            ("<html>private</html>", "markup"),
            ("private body", "other"),
        ] {
            assert_eq!(model_response_body_kind(body.as_bytes()), expected);
        }
    }

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
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {declared_len}\r\nConnection: close\r\n{extra_headers}\r\n{body}"
        )
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
            parse_retry_after("Fri, 31 Dec 9999 23:59:59 GMT").unwrap() > SHORT_RATE_LIMIT
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
    fn chat_output_caps_are_positive_exclusive_and_bound_to_profile() {
        for field in ["max_tokens", "max_completion_tokens"] {
            let base = OpenAiCompatibleBackend::new("https://test.example/v1", None, "exact-model");
            let legacy = base.request_profile_fingerprint();
            let capped = base.with_request_field(field, json!(4096)).unwrap();
            assert_eq!(
                capped
                    .request_body(&[AgentMessage::user("Hi")], &[], true)
                    .unwrap()[field],
                4096
            );
            assert_ne!(legacy, capped.request_profile_fingerprint());
            let other = if field == "max_tokens" {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            assert!(capped.with_request_field(other, json!(4096)).is_err());
            for invalid in [json!(0), json!(-1), json!("4096"), json!(u64::MAX)] {
                assert!(OpenAiCompatibleBackend::new(
                    "https://test.example/v1",
                    None,
                    "exact-model"
                )
                .with_request_field(field, invalid)
                .is_err());
            }
        }
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
    fn sse_frame_decoder_accepts_legal_line_endings_at_every_chunk_boundary() {
        // CR followed immediately by LF is one line ending, not a blank line.
        let separators = [
            ("\n", "\n"),
            ("\r", "\r"),
            ("\r\n", "\r\n"),
            ("\r\n", "\n"),
            ("\n", "\r\n"),
            ("\r", "\r\n"),
            ("\r\n", "\r"),
            ("\n", "\r"),
        ];
        for internal in ["\r", "\n", "\r\n"] {
            for (line_end, blank_end) in separators {
                let wire = format!("\u{feff}: hello{internal}event: delta{internal}data: Café 中🦀{line_end}{blank_end}data: next{blank_end}{blank_end}");
                for chunk_size in 1..=wire.len() {
                    let mut pending = SseFrameBuffer::default();
                    let mut frames = Vec::new();
                    for chunk in wire.as_bytes().chunks(chunk_size) {
                        pending.extend(chunk, MAX_STREAM_BUFFER_BYTES).unwrap();
                        while let Some(frame) = pending.take_sse_frame() {
                            frames.push(
                                sse_lines(std::str::from_utf8(&frame).unwrap())
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            );
                        }
                    }
                    assert_eq!(frames, [": hello\nevent: delta\ndata: Café 中🦀", "data: next"], "internal={internal:?}, separator={line_end:?}/{blank_end:?}, chunk={chunk_size}");
                    assert!(pending.bytes().is_empty());
                    assert_eq!(
                        pending.scanned_bytes,
                        wire.len() - 3,
                        "each non-BOM byte is visited once"
                    );
                }
            }
        }
    }

    #[test]
    fn sse_frame_decoder_crlf_is_one_ending_even_when_split_after_cr() {
        let mut pending = SseFrameBuffer::default();
        pending.extend(b"data: first\r", 100).unwrap();
        assert!(pending.take_sse_frame().is_none());
        pending.extend(b"\n", 100).unwrap();
        assert!(pending.take_sse_frame().is_none());
        pending.extend(b"\r", 100).unwrap();
        assert_eq!(pending.take_sse_frame().unwrap(), b"data: first");
        pending.extend(b"\ndata: second\r", 100).unwrap();
        assert!(pending.take_sse_frame().is_none());
        pending.extend(b"\r", 100).unwrap();
        assert_eq!(pending.take_sse_frame().unwrap(), b"data: second");
        assert!(pending.take_sse_frame().is_none());
        assert!(pending.bytes().is_empty());
    }

    #[test]
    fn sse_frame_decoder_eof_never_dispatches_an_undelimited_frame() {
        for ending in ["", "\r", "\n", "\r\n"] {
            let wire = format!("data: {{\"complete_json\":true}}{ending}");
            let mut pending = SseFrameBuffer::default();
            pending.extend(wire.as_bytes(), 100).unwrap();
            assert!(pending.take_sse_frame().is_none());
            assert_eq!(pending.bytes(), wire.as_bytes());
            let scanned = pending.scanned_bytes;
            assert!(pending.take_sse_frame().is_none());
            assert_eq!(
                pending.scanned_bytes, scanned,
                "EOF never rescans or flushes data"
            );
        }
        let mut pending = SseFrameBuffer::default();
        pending.extend(b"data: \xe4\xb8", 100).unwrap();
        assert!(pending.take_sse_frame().is_none());
        pending.extend(b"\xad\r\r", 100).unwrap();
        assert_eq!(
            std::str::from_utf8(&pending.take_sse_frame().unwrap()).unwrap(),
            "data: 中"
        );
    }

    #[test]
    fn sse_frame_decoder_bounds_retention_before_mutation_and_scans_linearly() {
        let mut pending = SseFrameBuffer::default();
        pending.extend(b"data: done\n\n", 16).unwrap();
        assert_eq!(pending.take_sse_frame().unwrap(), b"data: done");
        assert!(pending.take_sse_frame().is_none());
        pending.extend(b"data: 1234567890", 16).unwrap();
        assert!(pending.take_sse_frame().is_none());
        let before = (
            pending.bytes().to_vec(),
            pending.scan,
            pending.line_start,
            pending.scanned_bytes,
        );
        assert!(pending.extend(b"!", 16).is_err());
        assert_eq!(
            (
                pending.bytes().to_vec(),
                pending.scan,
                pending.line_start,
                pending.scanned_bytes
            ),
            before
        );
        let wire = format!("data: {}\r\n\r\n", "x".repeat(128 * 1024));
        let mut pending = SseFrameBuffer::default();
        let mut frames = 0;
        for chunk in wire.as_bytes().chunks(1) {
            pending.extend(chunk, MAX_STREAM_BUFFER_BYTES).unwrap();
            while pending.take_sse_frame().is_some() {
                frames += 1;
            }
        }
        assert_eq!(frames, 1);
        assert_eq!(pending.scanned_bytes, wire.len());
        assert!(pending.bytes().is_empty());
    }

    #[test]
    fn sse_lines_preserve_field_whitespace_and_non_newline_unicode() {
        assert_eq!(
            sse_lines("data:  Café\rdata:\t中\r\ndata: 🦀\u{2028}text\ndata:").collect::<Vec<_>>(),
            ["data:  Café", "data:\t中", "data: 🦀\u{2028}text", "data:"]
        );
    }

    #[test]
    fn sse_chat_assembler_accepts_cr_mixed_endings_and_rejects_invalid_utf8() {
        for ending in ["\r", "\n", "\r\n"] {
            let delta = json!({"choices":[{"delta":{"content":"Café 中🦀"}}]});
            let finish = json!({"choices":[{"delta":{},"finish_reason":"stop"}]});
            let wire = format!("data: {delta}{ending}{ending}data: {finish}\r\n\ndata: [DONE]\n\r");
            let mut pending = SseFrameBuffer::default();
            let mut assembler = SseAssembler::default();
            for chunk in wire.as_bytes().chunks(1) {
                pending.extend(chunk, MAX_STREAM_BUFFER_BYTES).unwrap();
                while let Some(frame) = pending.take_sse_frame() {
                    assembler.ingest_events(&frame, &mut Vec::new()).unwrap();
                }
            }
            assert_eq!(assembler.finish().unwrap().content, "Café 中🦀");
        }
        let mut pending = SseFrameBuffer::default();
        pending.extend(b"data: \xff\r\r", 100).unwrap();
        let frame = pending.take_sse_frame().unwrap();
        assert!(SseAssembler::default()
            .ingest_events(&frame, &mut Vec::new())
            .unwrap_err()
            .to_string()
            .contains("UTF-8"));
    }

    #[tokio::test]
    async fn sse_chat_missing_or_wrong_mime_uses_stream_validation() {
        for mime in [None, Some("application/json")] {
            let body = format!(
                "data: {}\r\rdata: {}\r\rdata: [DONE]\r\r",
                json!({"id":"chat_missing_mime","choices":[{"delta":{"content":"Café 中🦀"}}]}),
                json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
            );
            let mime = mime
                .map(|value| format!("Content-Type: {value}\r\n"))
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 200 OK\r\n{mime}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let (address, server) = scripted_http_server(vec![response]).await;
            let backend =
                OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "scripted");
            let request = backend
                .request(&[AgentMessage::user("Hello")], &[], true)
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(
                request.headers()[reqwest::header::ACCEPT],
                "text/event-stream"
            );
            let (sender, _) = tokio::sync::mpsc::unbounded_channel();
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
            assert_eq!(result.response_id.as_deref(), Some("chat_missing_mime"));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn sse_chat_ordinary_json_fallback_keeps_pretty_blank_lines_outside_framer() {
        for ending in ["\r", "\n", "\r\n"] {
            let body = serde_json::to_string_pretty(&json!({"id":"json_fallback","choices":[{"message":{"role":"assistant","content":"Café 中🦀"},"finish_reason":"stop"}]})).unwrap().replace('\n', &format!("{ending}{ending}"));
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let (address, server) = scripted_http_server(vec![response]).await;
            let backend =
                OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "scripted");
            let (sender, _) = tokio::sync::mpsc::unbounded_channel();
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
            assert_eq!(result.response_id.as_deref(), Some("json_fallback"));
            server.await.unwrap();
        }
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
        let mut pending = SseFrameBuffer::default();
        pending
            .extend(
                format!("data: {first}\r\n\r\ndata: {second}\n\n").as_bytes(),
                MAX_STREAM_BUFFER_BYTES,
            )
            .unwrap();
        while let Some(frame) = pending.take_sse_frame() {
            assembler.ingest(&frame, &sender).unwrap();
        }
        pending
            .extend(
                format!("data: {third}\n\n").as_bytes(),
                MAX_STREAM_BUFFER_BYTES,
            )
            .unwrap();
        while let Some(frame) = pending.take_sse_frame() {
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
    fn chat_sse_provider_error_cannot_become_a_success_after_done() {
        for error in [
            json!({"message":"quota ended"}),
            json!("provider failure"),
            json!(false),
            json!(42),
        ] {
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut assembler = SseAssembler::default();
            assembler
                .ingest(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"visible prefix\"}}]}",
                    &sender,
                )
                .unwrap();
            let frame = format!("data: {}", json!({"error":error}));
            assert!(
                matches!(assembler.ingest(frame.as_bytes(), &sender), Err(AgentError::Provider(failure)) if failure.message.contains("streaming response failed"))
            );
            assert!(assembler.ingest(b"data: [DONE]", &sender).is_err());
            assert!(
                matches!(assembler.finish(), Err(error) if error.to_string().contains("streaming response failed"))
            );
            assert_eq!(receiver.try_recv().unwrap(), "visible prefix");
            assert!(receiver.try_recv().is_err());
        }
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut assembler = SseAssembler::default();
        assembler.ingest(b"data: {\"error\":null,\"choices\":[{\"delta\":{\"content\":\"complete\"},\"finish_reason\":\"stop\"}]}", &sender).unwrap();
        assembler.ingest(b"data: [DONE]", &sender).unwrap();
        assert_eq!(assembler.finish().unwrap().content, "complete");
        assert_eq!(receiver.try_recv().unwrap(), "complete");
    }

    #[tokio::test]
    async fn chat_sse_provider_error_after_delta_is_terminal_over_real_http() {
        let prefix = json!({"choices":[{"delta":{"content":"Café 中"}}]});
        let error = json!({"error":{"message":"quota exhausted after stream started","type":"provider_failure"}});
        let body = format!("data: {prefix}\n\ndata: {error}\n\ndata: [DONE]\n\n");
        let (address, server) =
            scripted_http_server(vec![scripted_response("200 OK", &body, "", body.len())]).await;
        let backend = OpenAiCompatibleBackend::new(
            format!("http://{address}/v1"),
            None,
            "exact-fixture-model",
        );
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let result = backend
            .stream_complete(
                &[AgentMessage::user("hi")],
                &[],
                CancellationToken::new(),
                sender,
            )
            .await;
        assert!(
            matches!(result, Err(AgentError::Provider(failure)) if failure.message.contains("quota exhausted after stream started"))
        );
        assert_eq!(receiver.try_recv().unwrap(), "Café 中");
        assert!(receiver.try_recv().is_err());
        // The local server rejects an unexpected retry after publishing text.
        server.await.unwrap();
    }

    #[tokio::test]
    async fn empty_chat_delta_keeps_pre_visible_disconnect_retryable() {
        let empty = "data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\n";
        let completed = "data: {\"choices\":[{\"delta\":{\"content\":\"Café 中\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let (address, server) = scripted_http_server(vec![
            scripted_response("200 OK", empty, "", empty.len() + 40),
            scripted_response("200 OK", completed, "", completed.len()),
        ])
        .await;
        let backend = OpenAiCompatibleBackend::new(
            format!("http://{address}/v1"),
            None,
            "exact-fixture-model",
        );
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
        assert_eq!(output.content, "Café 中");
        assert_eq!(receiver.try_recv().unwrap(), "Café 中");
        assert!(
            receiver.try_recv().is_err(),
            "empty deltas must never be published"
        );
        server.await.unwrap();
    }

    #[test]
    fn chat_call_cardinality_and_json_output_bounds_precede_mutation() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut assembler = SseAssembler::default();
        assembler
            .ingest(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}",
                &sender,
            )
            .unwrap();
        assert!(!assembler.published_delta && receiver.try_recv().is_err());
        for index in 0..MAX_MODEL_TOOL_CALLS {
            assembler.ingest(format!("data: {}", json!({"choices":[{"delta":{"tool_calls":[{"index":index,"function":{"arguments":""}}]}}]})).as_bytes(), &sender).unwrap();
        }
        assert_eq!(assembler.calls.len(), MAX_MODEL_TOOL_CALLS);
        assert_eq!(assembler.assembled_bytes, 0);
        let overflow = format!(
            "data: {}",
            json!({"choices":[{"delta":{"tool_calls":[{"index":MAX_MODEL_TOOL_CALLS,"function":{"arguments":""}}]}}]})
        );
        assert!(matches!(
            assembler.ingest(overflow.as_bytes(), &sender),
            Err(AgentError::Limit(_))
        ));
        assert_eq!(assembler.calls.len(), MAX_MODEL_TOOL_CALLS);
        assert_eq!(assembler.assembled_bytes, 0);

        let calls = (0..MAX_MODEL_TOOL_CALLS).map(|index| json!({"id":format!("call-中-{index}"),"function":{"name":"read_file","arguments":"{\"text\":\"Café 中\"}"}})).collect::<Vec<_>>();
        let valid = json!({"choices":[{"finish_reason":"tool_calls","message":{"content":"Café 中", "tool_calls":calls}}]});
        assert_eq!(
            parse_model_output(&valid).unwrap().tool_calls.len(),
            MAX_MODEL_TOOL_CALLS
        );
        let mut too_many = valid.clone();
        too_many["choices"][0]["message"]["tool_calls"]
            .as_array_mut()
            .unwrap()
            .push(calls[0].clone());
        assert!(matches!(
            parse_model_output(&too_many),
            Err(AgentError::Limit(_))
        ));
        let arguments = json!({"text":"x".repeat(MAX_ASSEMBLED_STREAM_BYTES / 2)}).to_string();
        let huge = json!({"choices":[{"finish_reason":"tool_calls","message":{"content":"Café 中", "tool_calls":[{"id":"a","function":{"name":"read_file","arguments":arguments}},{"id":"b","function":{"name":"read_file","arguments":arguments}}]}}]});
        assert!(matches!(
            parse_model_output(&huge),
            Err(AgentError::Limit(_))
        ));
    }

    #[tokio::test]
    async fn stream_call_cardinality_limits_are_terminal_over_real_http() {
        for responses in [false, true] {
            let mut body = if responses {
                format!(
                    "data: {}\n\n",
                    json!({"type":"response.output_text.delta","delta":"Café 中"})
                )
            } else {
                format!(
                    "data: {}\n\n",
                    json!({"choices":[{"delta":{"content":"Café 中"}}]})
                )
            };
            for index in 0..=MAX_MODEL_TOOL_CALLS {
                let event = if responses {
                    json!({"type":"response.output_item.added","output_index":index,"item":{"type":"function_call","id":format!("item-{index}"),"call_id":format!("call-{index}"),"name":"read_file","arguments":""}})
                } else {
                    json!({"choices":[{"delta":{"tool_calls":[{"index":index,"function":{"arguments":""}}]}}]})
                };
                body.push_str(&format!("data: {event}\n\n"));
            }
            body.push_str("data: [DONE]\n\n");
            let (address, server) =
                scripted_http_server(vec![scripted_response("200 OK", &body, "", body.len())])
                    .await;
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let result = if responses {
                crate::ResponsesBackend::new(
                    format!("http://{address}/v1"),
                    None,
                    "exact-fixture-model",
                )
                .stream_complete(
                    &[AgentMessage::user("hi")],
                    &[],
                    CancellationToken::new(),
                    sender,
                )
                .await
            } else {
                OpenAiCompatibleBackend::new(
                    format!("http://{address}/v1"),
                    None,
                    "exact-fixture-model",
                )
                .stream_complete(
                    &[AgentMessage::user("hi")],
                    &[],
                    CancellationToken::new(),
                    sender,
                )
                .await
            };
            assert!(
                matches!(result, Err(AgentError::Limit(detail)) if detail.contains("64 tool calls"))
            );
            assert_eq!(receiver.try_recv().unwrap(), "Café 中");
            assert!(receiver.try_recv().is_err());
            server.await.unwrap();
        }
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
        // An hour of advice is a reached limit: one request, no wait.
        let (address, server) = scripted_http_server(vec![scripted_response(
            "429 Too Many Requests",
            "busy",
            "Retry-After: 3600\r\n",
            4,
        )])
        .await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let error = backend
            .complete(&[AgentMessage::user("hi")], &[], CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AgentError::Provider(failure)
                if failure.class == FailureClass::LimitReached && failure.resets_at_ms.is_some()
        ));
        server.await.unwrap();

        // 200 s of advice is waited for (200 ms under the test divisor).
        let (address, server) = scripted_http_server(vec![scripted_response(
            "429 Too Many Requests",
            "busy",
            "Retry-After: 200\r\n",
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
        tokio::time::sleep(Duration::from_millis(50)).await;
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
        assert!(matches!(
            &error,
            AgentError::Provider(failure) if failure.class == FailureClass::Fatal
        ));
        assert!(error.to_string().contains("HTTP 400"));
        server.await.unwrap();
    }

    fn json_error(status: &str, body: Value, headers: &str) -> String {
        let body = body.to_string();
        scripted_response(status, &body, headers, body.len())
    }

    async fn stream_collect(
        backend: &dyn ModelBackend,
    ) -> (Result<ModelCompletion, AgentError>, Vec<ModelStreamEvent>) {
        let cancel = CancellationToken::new();
        let (sender, mut receiver) = crate::model_event_channel(cancel.clone());
        let messages = [AgentMessage::user("hi")];
        let run = backend.stream_events(&messages, &[], cancel, sender);
        tokio::pin!(run);
        let mut events = Vec::new();
        let result = loop {
            tokio::select! {
                result = &mut run => break result,
                Some(event) = receiver.recv() => events.push(event),
            }
        };
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        (result, events)
    }

    fn retries(events: &[ModelStreamEvent]) -> Vec<(u32, u64, bool)> {
        events
            .iter()
            .filter_map(|event| match event {
                ModelStreamEvent::Retrying { attempt, max_attempts, delay_ms, rate_limited } => {
                    assert_eq!(*max_attempts, 6);
                    Some((*attempt, *delay_ms, *rate_limited))
                }
                _ => None,
            })
            .collect()
    }

    fn failure(result: Result<ModelCompletion, AgentError>) -> ProviderFailure {
        match result {
            Err(AgentError::Provider(failure)) => failure,
            other => panic!("expected a classified failure, got {other:?}"),
        }
    }

    const CHAT_OK: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";

    #[tokio::test]
    async fn transient_failures_back_off_six_times_and_only_long_waits_are_announced() {
        // The scripted server fails the test on an eighth request.
        let (address, server) = scripted_http_server(
            (0..7).map(|_| scripted_response("500 Internal Server Error", "boom", "", 4)).collect(),
        )
        .await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let (result, events) = stream_collect(&backend).await;
        let failure = failure(result);
        assert_eq!((failure.class, failure.status, failure.resets_at_ms), (FailureClass::Transient, Some(500), None));
        // 0.5 + 1 + 2 s pass silently; the 4 s wait crosses the 5 s mark.
        let announced = retries(&events);
        assert_eq!(announced.iter().map(|retry| retry.0).collect::<Vec<_>>(), vec![4, 5, 6]);
        for ((_, delay_ms, rate_limited), base) in announced.iter().zip([4000u64, 8000, 16000]) {
            assert!(!rate_limited);
            assert!((base * 8 / 10..=base * 12 / 10).contains(delay_ms), "{delay_ms} is not {base} ±20 %");
        }
        server.await.unwrap();

        // One blip is invisible.
        let (address, server) = scripted_http_server(vec![
            scripted_response("502 Bad Gateway", "boom", "", 4),
            scripted_response("200 OK", CHAT_OK, "", CHAT_OK.len()),
        ])
        .await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let (result, events) = stream_collect(&backend).await;
        assert_eq!(result.unwrap().output.content, "ok");
        assert!(retries(&events).is_empty());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_short_rate_limit_is_waited_out_visibly_and_the_third_in_a_row_is_a_limit() {
        let limited = || {
            json_error(
                "429 Too Many Requests",
                json!({"error":{"message":"slow","type":"requests","code":"rate_limit_exceeded"}}),
                "Retry-After: 7\r\n",
            )
        };
        let (address, server) = scripted_http_server(vec![
            limited(),
            scripted_response("200 OK", CHAT_OK, "", CHAT_OK.len()),
        ])
        .await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let (result, events) = stream_collect(&backend).await;
        assert_eq!(result.unwrap().output.content, "ok");
        assert_eq!(retries(&events), vec![(1, 7000, true)]);
        server.await.unwrap();

        let (address, server) = scripted_http_server(vec![limited(), limited(), limited()]).await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let (result, events) = stream_collect(&backend).await;
        let failure = failure(result);
        assert_eq!(failure.class, FailureClass::LimitReached);
        assert_eq!(failure.code.as_deref(), Some("rate_limit_exceeded"));
        assert!(failure.resets_at_ms.is_some());
        assert_eq!(retries(&events).len(), 2);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn limits_credits_auth_and_context_are_never_retried() {
        let resets_at = crate::provider_failure::now_ms() / 1000 + 7200;
        for (status, body, class, reset) in [
            ("429 Too Many Requests", json!({"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":resets_at}}), FailureClass::LimitReached, Some(resets_at * 1000)),
            ("429 Too Many Requests", json!({"error":{"type":"usage_not_included","message":"no"}}), FailureClass::OutOfCredits, None),
            ("429 Too Many Requests", json!({"error":{"message":"quota","type":"insufficient_quota","code":"insufficient_quota"}}), FailureClass::OutOfCredits, None),
            ("401 Unauthorized", json!({"error":{"message":"bad key","code":"invalid_api_key"}}), FailureClass::Auth, None),
            ("400 Bad Request", json!({"error":{"message":"too long","code":"context_length_exceeded"}}), FailureClass::ContextTooLarge, None),
        ] {
            let (address, server) = scripted_http_server(vec![json_error(status, body, "")]).await;
            let backend = crate::ResponsesBackend::new(format!("http://{address}/v1"), None, "mock");
            let (result, events) = stream_collect(&backend).await;
            let failure = failure(result);
            assert_eq!((failure.class, failure.resets_at_ms), (class, reset), "{status}");
            assert!(retries(&events).is_empty());
            server.await.unwrap();
        }
        // Chat-compatible families: OpenRouter and DeepSeek 402, Z.ai codes.
        for (status, body, class) in [
            ("402 Payment Required", json!({"error":{"code":402,"message":"Insufficient credits"}}), FailureClass::OutOfCredits),
            ("402 Payment Required", json!({"error":{"message":"Insufficient Balance","type":"unknown_error"}}), FailureClass::OutOfCredits),
            ("429 Too Many Requests", json!({"error":{"code":"1308","message":"Usage limit reached for 5 hour."}}), FailureClass::LimitReached),
            ("429 Too Many Requests", json!({"error":{"code":"1113","message":"arrears"}}), FailureClass::OutOfCredits),
            ("400 Bad Request", json!({"error":{"code":"1261","message":"prompt too long"}}), FailureClass::ContextTooLarge),
            ("403 Forbidden", json!({"error":{"message":"no"}}), FailureClass::Auth),
            ("404 Not Found", json!({"error":{"message":"no such model"}}), FailureClass::Fatal),
        ] {
            let (address, server) = scripted_http_server(vec![json_error(status, body, "")]).await;
            let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
            let (result, events) = stream_collect(&backend).await;
            assert_eq!(failure(result).class, class, "{status}");
            assert!(retries(&events).is_empty());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn errors_inside_a_200_stream_are_classified_and_retried_only_before_output() {
        // Z.ai overload and a MiniMax rate limit, then success.
        let zai = json_error("429 Too Many Requests", json!({"error":{"code":"1305","message":"busy"}}), "Retry-After: 0\r\n");
        let minimax = "data: {\"base_resp\":{\"status_code\":1002,\"status_msg\":\"rate limit\"}}\n\n";
        let (address, server) = scripted_http_server(vec![
            zai,
            scripted_response("200 OK", minimax, "", minimax.len()),
            scripted_response("200 OK", CHAT_OK, "", CHAT_OK.len()),
        ])
        .await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let (result, events) = stream_collect(&backend).await;
        assert_eq!(result.unwrap().output.content, "ok");
        assert_eq!(retries(&events).iter().map(|retry| retry.2).collect::<Vec<_>>(), vec![true]);
        server.await.unwrap();

        let plan = "data: {\"base_resp\":{\"status_code\":2056,\"status_msg\":\"usage limit exceeded\"}}\n\n";
        let (address, server) = scripted_http_server(vec![scripted_response("200 OK", plan, "", plan.len())]).await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let failure_2056 = failure(stream_collect(&backend).await.0);
        assert_eq!((failure_2056.class, failure_2056.code.as_deref()), (FailureClass::LimitReached, Some("2056")));
        server.await.unwrap();

        // OpenRouter reports an upstream failure in a chunk after text: the
        // text was published, so the request is not repeated.
        let after_text = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\ndata: {\"error\":{\"code\":502,\"message\":\"upstream\"},\"choices\":[{\"delta\":{},\"finish_reason\":\"error\"}]}\n\n";
        let (address, server) = scripted_http_server(vec![scripted_response("200 OK", after_text, "", after_text.len())]).await;
        let backend = OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "mock");
        let (result, events) = stream_collect(&backend).await;
        assert_eq!(failure(result).class, FailureClass::Transient);
        assert!(events.iter().any(|event| matches!(event, ModelStreamEvent::TextDelta { content } if content == "partial")));
        server.await.unwrap();

        // Responses: `response.failed` before any output is repeated.
        let failed = "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"r\",\"status\":\"failed\",\"error\":{\"code\":\"server_is_overloaded\",\"message\":\"busy\"}}}\n\n";
        let completed = "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_retry\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}]}}\n\n";
        let (address, server) = scripted_http_server(vec![
            scripted_response("200 OK", failed, "", failed.len()),
            scripted_response("200 OK", completed, "", completed.len()),
        ])
        .await;
        let backend = crate::ResponsesBackend::new(format!("http://{address}/v1"), None, "mock");
        assert_eq!(stream_collect(&backend).await.0.unwrap().output.content, "ok");
        server.await.unwrap();

        let quota = "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"r\",\"status\":\"failed\",\"error\":{\"code\":\"insufficient_quota\",\"message\":\"quota\"}}}\n\n";
        let (address, server) = scripted_http_server(vec![scripted_response("200 OK", quota, "", quota.len())]).await;
        let backend = crate::ResponsesBackend::new(format!("http://{address}/v1"), None, "mock");
        assert_eq!(failure(stream_collect(&backend).await.0).class, FailureClass::OutOfCredits);
        server.await.unwrap();
    }
}
