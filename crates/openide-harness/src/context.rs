use crate::{AgentError, ModelBackend, ToolDefinition};
use openide_core::{AgentMessage, MessageRole};
use std::ops::Range;
use tokio_util::sync::CancellationToken;

const CLEARED_TOOL_RESULT: &str = "[Earlier tool result omitted from model context]";

/// Token limits for a specific model and request. The caller supplies the
/// model's context window; a guessed default could silently admit a request
/// that does not fit a smaller model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenBudgetPolicy {
    pub context_window_tokens: u64,
    /// Space reserved for the model's response, including reasoning tokens.
    pub output_reserve_tokens: u64,
    /// Additional allowance for provider-specific tool framing. The tool
    /// definitions themselves are also counted by the fallback estimator.
    pub tool_schema_reserve_tokens: u64,
    /// Optional earlier compaction threshold, clamped to 90% of the window.
    pub auto_compact_token_limit: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenBudgetDecision {
    Continue,
    Compact,
    /// Do not send this request. A caller may still compact its context and
    /// assess the smaller request again before returning a limit error.
    Reject,
}

/// The projection includes input, output reserve, and extra tool framing.
/// For text-only requests, `estimated_input_tokens` is a byte-based fallback
/// and `projected_input_tokens` is calibrated from observed provider usage.
/// For image requests admitted through `assess_model_request`, both input
/// fields use the provider's exact pre-request count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenBudgetAssessment {
    pub estimated_input_tokens: u64,
    pub projected_input_tokens: u64,
    pub projected_total_tokens: u64,
    pub auto_compact_threshold_tokens: u64,
    pub hard_limit_tokens: u64,
    pub decision: TokenBudgetDecision,
}

/// Carries exact input usage from one request into the next request's budget.
/// Reset it after context compaction or a model/provider change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenBudgetState {
    pending_estimate: Option<u64>,
    calibrated_estimate: Option<u64>,
    observed_input_tokens: Option<u64>,
}

impl TokenBudgetState {
    pub fn observed_input_tokens(&self) -> Option<u64> {
        self.observed_input_tokens
    }

    /// Record the provider's `input_tokens` for the last assessed request.
    /// The next projection keeps that observed amount and adds any positive
    /// growth in the fallback estimate; shorter requests do not assume a
    /// proportional token decrease.
    pub fn observe_provider_input_tokens(&mut self, input_tokens: u64) -> Result<(), AgentError> {
        let estimate = self.pending_estimate.take().ok_or_else(|| {
            AgentError::InvalidRequest("assess a request before recording input usage".into())
        })?;
        self.calibrated_estimate = Some(estimate);
        self.observed_input_tokens = Some(input_tokens);
        Ok(())
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl TokenBudgetPolicy {
    /// Assess a model request. Images require a provider preflight count of
    /// the complete request (including text, images, and tools). Text-only
    /// requests preserve the existing local fallback and usage calibration.
    pub async fn assess_model_request(
        self,
        state: &mut TokenBudgetState,
        model: &dyn ModelBackend,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<TokenBudgetAssessment, AgentError> {
        if messages.iter().any(|message| !message.images.is_empty()) {
            let exact_input_tokens = model
                .count_input_tokens(messages, tools, cancel)
                .await?
                .ok_or_else(|| {
                    AgentError::Limit(format!(
                        "image input token count is unknown for model {}; the request cannot pass the verified context budget",
                        model.model_id()
                    ))
                })?;
            self.assess_request_with_exact_input_tokens(state, exact_input_tokens)
        } else {
            self.assess_request(state, messages, tools)
        }
    }

    /// Assess an already counted request. Use the provider count from the
    /// exact request payload; do not add a local image-token guess to it.
    pub fn assess_request_with_exact_input_tokens(
        self,
        state: &mut TokenBudgetState,
        exact_input_tokens: u64,
    ) -> Result<TokenBudgetAssessment, AgentError> {
        self.assess_projected_tokens(state, exact_input_tokens, exact_input_tokens)
    }

    /// Assess before sending a request. `Reject` forbids sending the current
    /// request, but compaction followed by reassessment may recover it.
    /// `Compact` requests a context reduction before the hard limit. The
    /// fallback deliberately budgets at least one token per UTF-8 byte plus
    /// message/tool framing. Exact admission still depends on the provider's
    /// tokenizer and its actual usage report.
    pub fn assess_request(
        self,
        state: &mut TokenBudgetState,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
    ) -> Result<TokenBudgetAssessment, AgentError> {
        if messages.iter().any(|message| !message.images.is_empty()) {
            return Err(AgentError::Limit(
                "image input token count is unknown; use provider preflight before applying a context budget".into(),
            ));
        }
        let estimated_input_tokens = estimate_input_tokens(messages, tools);
        let projected_input_tokens =
            match (state.calibrated_estimate, state.observed_input_tokens) {
                (Some(previous_estimate), Some(observed)) => observed
                    .saturating_add(estimated_input_tokens.saturating_sub(previous_estimate)),
                _ => estimated_input_tokens,
            };
        self.assess_projected_tokens(state, estimated_input_tokens, projected_input_tokens)
    }

    fn assess_projected_tokens(
        self,
        state: &mut TokenBudgetState,
        estimated_input_tokens: u64,
        projected_input_tokens: u64,
    ) -> Result<TokenBudgetAssessment, AgentError> {
        if self.context_window_tokens == 0 {
            return Err(AgentError::InvalidRequest(
                "context_window_tokens must be greater than zero".into(),
            ));
        }
        if self.output_reserve_tokens == 0 {
            return Err(AgentError::InvalidRequest(
                "output_reserve_tokens must be greater than zero".into(),
            ));
        }
        if self.auto_compact_token_limit == Some(0) {
            return Err(AgentError::InvalidRequest(
                "auto_compact_token_limit must be greater than zero".into(),
            ));
        }

        let projected_total_tokens = projected_input_tokens
            .saturating_add(self.output_reserve_tokens)
            .saturating_add(self.tool_schema_reserve_tokens);
        let hard_limit_tokens = percent_of(self.context_window_tokens, 95);
        let default_auto_limit = percent_of(self.context_window_tokens, 90);
        let auto_compact_threshold_tokens = self
            .auto_compact_token_limit
            .unwrap_or(default_auto_limit)
            .min(default_auto_limit);
        let decision = if projected_total_tokens > hard_limit_tokens {
            TokenBudgetDecision::Reject
        } else if projected_total_tokens >= auto_compact_threshold_tokens {
            TokenBudgetDecision::Compact
        } else {
            TokenBudgetDecision::Continue
        };
        state.pending_estimate = Some(estimated_input_tokens);
        Ok(TokenBudgetAssessment {
            estimated_input_tokens,
            projected_input_tokens,
            projected_total_tokens,
            auto_compact_threshold_tokens,
            hard_limit_tokens,
            decision,
        })
    }
}

fn percent_of(tokens: u64, percent: u64) -> u64 {
    ((tokens as u128 * percent as u128) / 100) as u64
}

fn estimate_input_tokens(messages: &[AgentMessage], tools: &[ToolDefinition]) -> u64 {
    let mut count = 0u64;
    for message in messages {
        count = count
            .saturating_add(32)
            .saturating_add(byte_count(&message.content));
        if let Some(call_id) = &message.tool_call_id {
            count = count.saturating_add(byte_count(call_id));
        }
        for call in &message.tool_calls {
            count = count
                .saturating_add(32)
                .saturating_add(byte_count(&call.id))
                .saturating_add(byte_count(&call.name))
                .saturating_add(byte_count(&call.arguments.to_string()));
        }
        for item in &message.provider_output_items {
            count = count.saturating_add(byte_count(&item.to_string()));
        }
    }
    for tool in tools {
        count = count
            .saturating_add(64)
            .saturating_add(byte_count(&tool.name))
            .saturating_add(byte_count(&tool.description))
            .saturating_add(byte_count(&tool.parameters.to_string()));
    }
    count
}

fn byte_count(value: &str) -> u64 {
    u64::try_from(value.len()).unwrap_or(u64::MAX)
}

/// Bounds the messages sent to a model using an approximate character count.
///
/// The active prompt and system messages are pinned. Older conversation turns
/// are dropped as units; old tool result bodies in the active turn may be
/// replaced while their assistant calls and tool-call IDs remain intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextPolicy {
    pub max_request_chars: usize,
    pub keep_recent_tool_results: usize,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            max_request_chars: 120_000,
            keep_recent_tool_results: 3,
        }
    }
}

impl ContextPolicy {
    pub(crate) fn compact(
        self,
        messages: &mut Vec<AgentMessage>,
        current_prompt_index: &mut usize,
    ) -> Result<(), AgentError> {
        if self.max_request_chars == 0 {
            return Err(AgentError::InvalidRequest(
                "max_request_chars must be greater than zero".into(),
            ));
        }
        if *current_prompt_index >= messages.len()
            || messages[*current_prompt_index].role != MessageRole::User
        {
            return Err(AgentError::Runtime(
                "active user message is missing from context".into(),
            ));
        }
        let mut used = context_chars(messages);
        while used > self.max_request_chars {
            let Some(range) = oldest_historical_turn(messages, *current_prompt_index) else {
                break;
            };
            let removed = context_chars(&messages[range.clone()]);
            *current_prompt_index -= range.len();
            messages.drain(range);
            used = used.saturating_sub(removed);
        }
        if used > self.max_request_chars {
            let candidates = messages
                .iter()
                .enumerate()
                .skip(*current_prompt_index + 1)
                .filter_map(|(index, message)| {
                    (message.role == MessageRole::Tool && message.content != CLEARED_TOOL_RESULT)
                        .then_some(index)
                })
                .collect::<Vec<_>>();
            let clear_count = candidates
                .len()
                .saturating_sub(self.keep_recent_tool_results);
            for index in candidates.into_iter().take(clear_count) {
                if used <= self.max_request_chars {
                    break;
                }
                let previous = message_chars(&messages[index]);
                if messages[index].content.chars().count() <= CLEARED_TOOL_RESULT.chars().count() {
                    continue;
                }
                messages[index].content = CLEARED_TOOL_RESULT.to_owned();
                let replacement = message_chars(&messages[index]);
                used = used.saturating_sub(previous).saturating_add(replacement);
            }
        }
        if used > self.max_request_chars {
            return Err(AgentError::Limit(format!(
                "model context needs about {used} characters; policy allows {}",
                self.max_request_chars
            )));
        }
        Ok(())
    }
}

fn oldest_historical_turn(messages: &[AgentMessage], prompt_index: usize) -> Option<Range<usize>> {
    for start in 1..prompt_index {
        if messages[start].role == MessageRole::System {
            continue;
        }
        let mut end = start + 1;
        while end < prompt_index
            && !matches!(messages[end].role, MessageRole::User | MessageRole::System)
        {
            end += 1;
        }
        return Some(start..end);
    }
    None
}

fn context_chars(messages: &[AgentMessage]) -> usize {
    messages
        .iter()
        .map(message_chars)
        .fold(0usize, usize::saturating_add)
}

fn message_chars(message: &AgentMessage) -> usize {
    let mut count = 16usize.saturating_add(message.content.chars().count());
    if let Some(call_id) = &message.tool_call_id {
        count = count.saturating_add(call_id.chars().count());
    }
    for call in &message.tool_calls {
        count = count
            .saturating_add(call.id.chars().count())
            .saturating_add(call.name.chars().count())
            .saturating_add(call.arguments.to_string().chars().count());
    }
    for item in &message.provider_output_items {
        count = count.saturating_add(item.to_string().chars().count());
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use openide_core::{ToolCall, UserImage, UserImageMime};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingModel {
        count: Option<u64>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelBackend for CountingModel {
        fn model_id(&self) -> &str {
            "counting-model"
        }

        async fn count_input_tokens(
            &self,
            _messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<Option<u64>, AgentError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.count)
        }

        async fn complete(
            &self,
            _messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<crate::ModelOutput, AgentError> {
            unreachable!("budget assessment never sends a completion")
        }
    }

    fn image_message() -> AgentMessage {
        let image = UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        AgentMessage::user_with_images("describe", vec![image]).unwrap()
    }

    fn token_policy(window: u64, output: u64, tool_reserve: u64) -> TokenBudgetPolicy {
        TokenBudgetPolicy {
            context_window_tokens: window,
            output_reserve_tokens: output,
            tool_schema_reserve_tokens: tool_reserve,
            auto_compact_token_limit: None,
        }
    }

    #[test]
    fn small_window_thresholds_reserve_output_and_tool_framing() {
        let policy = token_policy(1_000, 100, 50);
        let mut state = TokenBudgetState::default();
        let assess = |content_len, state: &mut TokenBudgetState| {
            policy
                .assess_request(state, &[AgentMessage::user("x".repeat(content_len))], &[])
                .unwrap()
        };

        let below = assess(717, &mut state);
        assert_eq!(below.projected_input_tokens, 749);
        assert_eq!(below.projected_total_tokens, 899);
        assert_eq!(below.auto_compact_threshold_tokens, 900);
        assert_eq!(below.hard_limit_tokens, 950);
        assert_eq!(below.decision, TokenBudgetDecision::Continue);

        assert_eq!(
            assess(718, &mut state).decision,
            TokenBudgetDecision::Compact
        );
        assert_eq!(
            assess(768, &mut state).decision,
            TokenBudgetDecision::Compact
        );
        assert_eq!(
            assess(769, &mut state).decision,
            TokenBudgetDecision::Reject
        );
    }

    #[test]
    fn large_window_thresholds_and_custom_limit_are_bounded() {
        let mut policy = token_policy(272_000, 8_192, 1_024);
        let mut state = TokenBudgetState::default();
        let within = policy
            .assess_request(&mut state, &[AgentMessage::user("x".repeat(200_000))], &[])
            .unwrap();
        assert_eq!(within.auto_compact_threshold_tokens, 244_800);
        assert_eq!(within.hard_limit_tokens, 258_400);
        assert_eq!(within.decision, TokenBudgetDecision::Continue);

        let at_auto = policy
            .assess_request(&mut state, &[AgentMessage::user("x".repeat(235_552))], &[])
            .unwrap();
        assert_eq!(at_auto.projected_total_tokens, 244_800);
        assert_eq!(at_auto.decision, TokenBudgetDecision::Compact);

        policy.auto_compact_token_limit = Some(250_000);
        assert_eq!(
            policy
                .assess_request(&mut state, &[AgentMessage::user("x")], &[])
                .unwrap()
                .auto_compact_threshold_tokens,
            244_800
        );
        policy.auto_compact_token_limit = Some(100_000);
        assert_eq!(
            policy
                .assess_request(&mut state, &[AgentMessage::user("x")], &[])
                .unwrap()
                .auto_compact_threshold_tokens,
            100_000
        );
        assert!(percent_of(u64::MAX, 95) < u64::MAX);
    }

    #[test]
    fn unknown_usage_estimates_utf8_bytes_and_tool_schemas() {
        let message = AgentMessage::user("🙂");
        let tool = ToolDefinition {
            name: "read".into(),
            description: "A tool".into(),
            parameters: json!({"type":"object"}),
        };
        let assessment = token_policy(1_000, 100, 0)
            .assess_request(&mut TokenBudgetState::default(), &[message], &[tool])
            .unwrap();
        assert_eq!(assessment.estimated_input_tokens, 36 + 64 + 4 + 6 + 17);
        assert_eq!(
            assessment.projected_input_tokens,
            assessment.estimated_input_tokens
        );
        assert_eq!(assessment.decision, TokenBudgetDecision::Continue);
    }

    #[tokio::test]
    async fn image_budget_requires_exact_count_before_admission() {
        let policy = token_policy(1_000, 100, 0);
        let message = image_message();
        let mut state = TokenBudgetState::default();
        assert!(matches!(
            policy.assess_request(&mut state, std::slice::from_ref(&message), &[]),
            Err(AgentError::Limit(detail)) if detail.contains("unknown")
        ));

        let model = CountingModel {
            count: Some(851),
            calls: AtomicUsize::new(0),
        };
        let assessment = policy
            .assess_model_request(
                &mut state,
                &model,
                std::slice::from_ref(&message),
                &[],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(assessment.estimated_input_tokens, 851);
        assert_eq!(assessment.projected_input_tokens, 851);
        assert_eq!(assessment.projected_total_tokens, 951);
        assert_eq!(assessment.decision, TokenBudgetDecision::Reject);
        assert_eq!(model.calls.load(Ordering::Relaxed), 1);

        let unknown_model = CountingModel {
            count: None,
            calls: AtomicUsize::new(0),
        };
        assert!(matches!(
            policy
                .assess_model_request(
                    &mut state,
                    &unknown_model,
                    &[message],
                    &[],
                    CancellationToken::new(),
                )
                .await,
            Err(AgentError::Limit(detail)) if detail.contains("unknown") && detail.contains("counting-model")
        ));
    }

    #[tokio::test]
    async fn text_budget_does_not_call_provider_count_endpoint() {
        let model = CountingModel {
            count: Some(900),
            calls: AtomicUsize::new(0),
        };
        let assessment = token_policy(1_000, 100, 0)
            .assess_model_request(
                &mut TokenBudgetState::default(),
                &model,
                &[AgentMessage::user("text")],
                &[],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(assessment.estimated_input_tokens, 36);
        assert_eq!(model.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn opaque_provider_items_count_against_context_budget() {
        let mut assistant = AgentMessage::assistant("", Vec::new());
        assistant.provider_output_items =
            vec![json!({"type":"reasoning","encrypted_content":"opaque-state"})];
        let estimated = token_policy(1_000, 100, 0)
            .assess_request(&mut TokenBudgetState::default(), &[assistant.clone()], &[])
            .unwrap()
            .estimated_input_tokens;
        assert_eq!(
            estimated,
            32 + assistant.provider_output_items[0].to_string().len() as u64
        );
    }

    #[test]
    fn provider_usage_calibrates_growth_and_reset_restores_fallback() {
        let policy = token_policy(1_000, 100, 0);
        let mut state = TokenBudgetState::default();
        assert!(state.observe_provider_input_tokens(30).is_err());
        let baseline = policy
            .assess_request(&mut state, &[AgentMessage::user("x".repeat(100))], &[])
            .unwrap();
        assert_eq!(baseline.estimated_input_tokens, 132);
        state.observe_provider_input_tokens(30).unwrap();

        let same = policy
            .assess_request(&mut state, &[AgentMessage::user("x".repeat(100))], &[])
            .unwrap();
        assert_eq!(same.projected_input_tokens, 30);
        let grown = policy
            .assess_request(&mut state, &[AgentMessage::user("x".repeat(120))], &[])
            .unwrap();
        assert_eq!(grown.projected_input_tokens, 50);
        let shorter = policy
            .assess_request(&mut state, &[AgentMessage::user("x".repeat(80))], &[])
            .unwrap();
        assert_eq!(shorter.projected_input_tokens, 30);

        state.reset();
        let uncalibrated = policy
            .assess_request(&mut state, &[AgentMessage::user("x".repeat(80))], &[])
            .unwrap();
        assert_eq!(uncalibrated.projected_input_tokens, 112);
    }

    #[test]
    fn provider_usage_above_fallback_can_enforce_hard_limit() {
        let policy = token_policy(1_000, 100, 0);
        let mut state = TokenBudgetState::default();
        let messages = [AgentMessage::user("x".repeat(100))];
        let first = policy.assess_request(&mut state, &messages, &[]).unwrap();
        assert_eq!(first.decision, TokenBudgetDecision::Continue);
        state.observe_provider_input_tokens(851).unwrap();
        let calibrated = policy.assess_request(&mut state, &messages, &[]).unwrap();
        assert_eq!(calibrated.projected_total_tokens, 951);
        assert_eq!(calibrated.decision, TokenBudgetDecision::Reject);

        state.reset();
        let after_compaction = policy
            .assess_request(&mut state, &[AgentMessage::user("short")], &[])
            .unwrap();
        assert_eq!(after_compaction.decision, TokenBudgetDecision::Continue);
    }

    #[test]
    fn tiny_window_rejects_impossible_request_and_invalid_policy() {
        let mut state = TokenBudgetState::default();
        let message = AgentMessage::user("x");
        let rejected = token_policy(16, 1, 0)
            .assess_request(&mut state, std::slice::from_ref(&message), &[])
            .unwrap();
        assert_eq!(rejected.decision, TokenBudgetDecision::Reject);
        assert!(matches!(
            token_policy(0, 1, 0).assess_request(&mut state, std::slice::from_ref(&message), &[]),
            Err(AgentError::InvalidRequest(_))
        ));
        assert!(matches!(
            token_policy(1_000, 0, 0).assess_request(&mut state, &[message], &[]),
            Err(AgentError::InvalidRequest(_))
        ));
    }

    #[test]
    fn removes_old_turn_with_complete_tool_pair() {
        let call = ToolCall {
            id: "old-call".into(),
            name: "read_file".into(),
            arguments: json!({"path":"old.txt"}),
        };
        let mut messages = vec![
            AgentMessage::system("system"),
            AgentMessage::user("older question"),
            AgentMessage::assistant("", vec![call]),
            AgentMessage::tool("old-call", "large old result"),
            AgentMessage::assistant("older answer", vec![]),
            AgentMessage::user("current question"),
        ];
        let mut prompt_index = 5;
        let budget = context_chars(&messages[..1]) + message_chars(&messages[5]);
        ContextPolicy {
            max_request_chars: budget,
            keep_recent_tool_results: 1,
        }
        .compact(&mut messages, &mut prompt_index)
        .unwrap();
        assert_eq!(prompt_index, 1);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "current question");
    }

    #[test]
    fn clears_old_active_tool_result_but_keeps_matching_call() {
        let call_a = ToolCall {
            id: "call-a".into(),
            name: "read_file".into(),
            arguments: json!({"path":"a.txt"}),
        };
        let call_b = ToolCall {
            id: "call-b".into(),
            name: "read_file".into(),
            arguments: json!({"path":"b.txt"}),
        };
        let mut messages = vec![
            AgentMessage::system("system"),
            AgentMessage::user("current prompt"),
            AgentMessage::assistant("", vec![call_a]),
            AgentMessage::tool("call-a", "a".repeat(1_000)),
            AgentMessage::assistant("", vec![call_b]),
            AgentMessage::tool("call-b", "recent"),
        ];
        let mut prompt_index = 1;
        let original_chars = context_chars(&messages);
        ContextPolicy {
            max_request_chars: original_chars - 100,
            keep_recent_tool_results: 1,
        }
        .compact(&mut messages, &mut prompt_index)
        .unwrap();
        assert_eq!(messages[3].content, CLEARED_TOOL_RESULT);
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("call-a"));
        assert_eq!(messages[2].tool_calls[0].id, "call-a");
        assert_eq!(messages[5].content, "recent");
        assert_eq!(prompt_index, 1);
    }

    #[test]
    fn fails_if_current_prompt_alone_exceeds_limit() {
        let mut messages = vec![
            AgentMessage::system("system"),
            AgentMessage::user("x".repeat(500)),
        ];
        let mut prompt_index = 1;
        let error = ContextPolicy {
            max_request_chars: 100,
            keep_recent_tool_results: 0,
        }
        .compact(&mut messages, &mut prompt_index)
        .unwrap_err();
        assert!(matches!(error, AgentError::Limit(_)));
        assert_eq!(messages[1].content.len(), 500);
    }
}
