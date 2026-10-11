//! Classification of provider failures: what the harness may retry, what
//! pauses a turn until a limit resets, and what ends it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;
use serde_json::Value;

/// A rate limit advised to last longer than this is a reached limit, not a
/// wait the request loop sits through.
pub(crate) const SHORT_RATE_LIMIT: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClass {
    /// Connection, timeout, 5xx or an overloaded provider: retried with backoff.
    Transient,
    /// A short rate limit: retried honouring the provider's advice.
    RateLimited,
    /// A usage limit. `resets_at_ms` says when it renews, if the provider did.
    LimitReached,
    /// No credit or no plan for this model: only the account page fixes it.
    OutOfCredits,
    ContextTooLarge,
    Auth,
    /// Anything else: the turn fails as before.
    Fatal,
}

impl FailureClass {
    pub(crate) fn retryable(self) -> bool {
        matches!(self, Self::Transient | Self::RateLimited)
    }

    /// Classes that leave the turn paused at its model boundary instead of
    /// failing it. A request that was never committed can be sent again.
    pub fn pauses_turn(self) -> bool {
        !matches!(self, Self::Fatal | Self::ContextTooLarge)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderFailure {
    pub class: FailureClass,
    pub status: Option<u16>,
    /// The provider's own `code` or `type`, when it sent one.
    pub code: Option<String>,
    pub retry_after: Option<Duration>,
    pub resets_at_ms: Option<u64>,
    /// Filled by the runner from the backend that failed.
    pub limit_key: Option<String>,
    /// Raw diagnostic text. Hosts show it as a detail, not as the headline.
    pub message: String,
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

impl ProviderFailure {
    pub fn new(class: FailureClass, message: impl Into<String>) -> Self {
        Self {
            class,
            status: None,
            code: None,
            retry_after: None,
            resets_at_ms: None,
            limit_key: None,
            message: message.into(),
        }
    }

    /// A non-success HTTP response. `body` is the bounded error body.
    pub(crate) fn from_http(status: u16, headers: &HeaderMap, body: &[u8]) -> Self {
        let detail: String = String::from_utf8_lossy(body).chars().take(500).collect();
        let value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let mut failure = Self::classify(Some(status), &value, format!("HTTP {status}: {detail}"));
        if matches!(
            failure.class,
            FailureClass::Transient | FailureClass::RateLimited | FailureClass::LimitReached
        ) {
            failure.retry_after = header_retry_after(headers, now_ms());
            failure.settle_reset();
        }
        failure
    }

    /// An error reported inside a response or stream that began with HTTP 200.
    pub(crate) fn from_body(body: &Value, message: String) -> Self {
        let mut failure = Self::classify(None, body, message);
        failure.settle_reset();
        failure
    }

    fn settle_reset(&mut self) {
        if self.resets_at_ms.is_none() {
            self.resets_at_ms = self
                .retry_after
                .filter(|_| self.class != FailureClass::Transient)
                .map(|delay| now_ms().saturating_add(delay.as_millis() as u64));
        }
        if self.class == FailureClass::RateLimited
            && self.retry_after.is_some_and(|delay| delay > SHORT_RATE_LIMIT)
        {
            self.class = FailureClass::LimitReached;
        }
        if self.class == FailureClass::RateLimited {
            // Only a reached limit carries a reset time to pause until.
            self.resets_at_ms = None;
        }
    }

    /// The retry loop gave up on a short rate limit: treat it as reached.
    pub(crate) fn escalate_rate_limit(mut self) -> Self {
        if self.class == FailureClass::RateLimited {
            self.class = FailureClass::LimitReached;
            self.resets_at_ms = self
                .retry_after
                .map(|delay| now_ms().saturating_add(delay.as_millis() as u64));
        }
        self
    }

    fn classify(status: Option<u16>, body: &Value, message: String) -> Self {
        let error = body
            .pointer("/response/error")
            .or_else(|| body.get("error"))
            .filter(|error| error.is_object())
            .unwrap_or(body);
        let token = |name: &str| match error.get(name) {
            Some(Value::String(text)) if !text.is_empty() => Some(text.to_ascii_lowercase()),
            Some(Value::Number(number)) => Some(number.to_string()),
            _ => None,
        };
        let code = token("code");
        let kind = token("type").filter(|kind| kind != "error");
        // MiniMax reports its own code in a 200 envelope; its numbers overlap
        // other vendors', so they are read only from `base_resp`.
        let minimax = body
            .pointer("/base_resp/status_code")
            .and_then(Value::as_i64)
            .filter(|code| *code != 0);
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| body.get("error").and_then(Value::as_str))
            .or_else(|| body.get("detail").and_then(Value::as_str))
            .or_else(|| body.pointer("/base_resp/status_msg").and_then(Value::as_str))
            .unwrap_or(&message)
            .to_ascii_lowercase();
        // OpenRouter puts the HTTP status in `error.code`, also mid-stream.
        let status = status.or_else(|| {
            code.as_deref()
                .and_then(|code| code.parse::<u16>().ok())
                .filter(|code| (400..600).contains(code))
        });
        // Anthropic names its tier spend cap only here.
        let detail_code = error
            .pointer("/details/error_code")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase);
        let class = minimax
            .map(class_for_minimax)
            .or_else(|| detail_code.as_deref().and_then(class_for_token))
            .or_else(|| code.as_deref().and_then(class_for_token))
            .or_else(|| kind.as_deref().and_then(class_for_token))
            .or_else(|| class_for_text(&text))
            .or_else(|| status.map(class_for_status))
            .unwrap_or(FailureClass::Fatal);
        let now = now_ms();
        let number = |name: &str| error.get(name).and_then(Value::as_u64);
        let resets_at_ms = number("resets_at")
            // Unix seconds; a value this large can only be milliseconds.
            .map(|at| if at < 100_000_000_000 { at.saturating_mul(1000) } else { at })
            .or_else(|| {
                number("resets_in_seconds")
                    .map(|seconds| now.saturating_add(seconds.saturating_mul(1000)))
            })
            .filter(|_| matches!(class, FailureClass::LimitReached | FailureClass::OutOfCredits));
        Self {
            class,
            status,
            code: minimax.map(|code| code.to_string()).or(code).or(kind),
            retry_after: None,
            resets_at_ms,
            limit_key: None,
            message,
        }
    }
}

/// Provider `code` and `type` values, lowercased. Z.ai sends its business
/// codes as strings in `error.code`.
fn class_for_token(token: &str) -> Option<FailureClass> {
    Some(match token {
        // Z.ai 1308/1310/1313 and 1316-1321 are plan windows; their reset is
        // only in the message text, so it stays unknown.
        "usage_limit_reached" | "enforced_spend_limit_reached" | "1308" | "1310" | "1313"
        | "1316" | "1317" | "1318" | "1319" | "1320" | "1321" => FailureClass::LimitReached,
        "usage_not_included" | "insufficient_quota" | "credit_balance_exhausted"
        | "organization_spend_limit_exceeded" | "project_spend_limit_exceeded"
        | "organization_usage_limit_exceeded" | "billing_error" | "billing_not_active"
        | "insufficient_balance" | "1113" | "1309" | "1311" => FailureClass::OutOfCredits,
        "overloaded_error" | "server_is_overloaded" | "server_error" | "api_error"
        | "timeout_error" | "1305" => FailureClass::Transient,
        "rate_limit_exceeded" | "rate_limit_error" | "rate_limited" | "slow_down" | "1302" => {
            FailureClass::RateLimited
        }
        "context_length_exceeded" | "request_too_large" | "1261" => FailureClass::ContextTooLarge,
        "authentication_error" | "permission_error" | "invalid_api_key" | "1000" | "1001"
        | "1003" | "1005" => FailureClass::Auth,
        "content_filter" | "content_policy_violation" | "1301" => FailureClass::Fatal,
        _ => return None,
    })
}

fn class_for_minimax(code: i64) -> FailureClass {
    match code {
        1000 | 1001 | 1024 | 1033 => FailureClass::Transient,
        1002 | 1041 | 2045 => FailureClass::RateLimited,
        2056 => FailureClass::LimitReached,
        1008 => FailureClass::OutOfCredits,
        1039 => FailureClass::ContextTooLarge,
        1004 | 2049 => FailureClass::Auth,
        _ => FailureClass::Fatal,
    }
}

/// Message text is the fallback for providers that reuse a generic type.
fn class_for_text(text: &str) -> Option<FailureClass> {
    let has = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    if has(&["credit balance", "insufficient balance", "insufficient credits"]) {
        Some(FailureClass::OutOfCredits)
    } else if has(&["prompt is too long", "context length", "context window", "maximum context"]) {
        Some(FailureClass::ContextTooLarge)
    } else if text.starts_with("you have reached your specified") {
        // Anthropic: a spend limit the account set for itself, sent as a 400.
        Some(FailureClass::LimitReached)
    } else {
        None
    }
}

fn class_for_status(status: u16) -> FailureClass {
    match status {
        401 | 403 => FailureClass::Auth,
        402 => FailureClass::OutOfCredits,
        413 => FailureClass::ContextTooLarge,
        429 => FailureClass::RateLimited,
        408 | 500..=599 => FailureClass::Transient,
        _ => FailureClass::Fatal,
    }
}

/// The wait the provider advises, from the headers it documents. Values are
/// parsed, never logged.
fn header_retry_after(headers: &HeaderMap, now_ms: u64) -> Option<Duration> {
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    if let Some(ms) = text("retry-after-ms").and_then(|value| value.trim().parse::<u64>().ok()) {
        return Some(Duration::from_millis(ms));
    }
    if let Some(delay) = text("retry-after").and_then(crate::backend::parse_retry_after) {
        return Some(delay);
    }
    // A bucket's reset matters only when that bucket is the exhausted one.
    let exhausted = |name: &str| text(name).is_some_and(|left| left.trim() == "0");
    let mut advised: Option<Duration> = None;
    for bucket in ["requests", "tokens", "input-tokens", "output-tokens"] {
        let anthropic = exhausted(&format!("anthropic-ratelimit-{bucket}-remaining"))
            .then(|| text(&format!("anthropic-ratelimit-{bucket}-reset")))
            .flatten()
            .and_then(parse_rfc3339_ms)
            .map(|at| Duration::from_millis(at.saturating_sub(now_ms)));
        let openai = exhausted(&format!("x-ratelimit-remaining-{bucket}"))
            .then(|| text(&format!("x-ratelimit-reset-{bucket}")))
            .flatten()
            .and_then(parse_go_duration);
        for delay in [anthropic, openai].into_iter().flatten() {
            advised = Some(advised.map_or(delay, |current| current.max(delay)));
        }
    }
    let unix = |name: &str| {
        text(name)
            .and_then(|value| value.trim().parse::<u64>().ok())
            // Seconds or milliseconds: only the latter has thirteen digits.
            .map(|at| if at < 1_000_000_000_000 { at.saturating_mul(1000) } else { at })
            .map(|at| Duration::from_millis(at.saturating_sub(now_ms)))
    };
    advised
        // OpenRouter
        .or_else(|| exhausted("x-ratelimit-remaining").then(|| unix("x-ratelimit-reset")).flatten())
        // Claude subscription
        .or_else(|| {
            (text("anthropic-ratelimit-unified-status") == Some("rejected"))
                .then(|| unix("anthropic-ratelimit-unified-reset"))
                .flatten()
        })
}

/// `6m0s`, `1.5s`, `20ms`, or a bare number of seconds.
fn parse_go_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<f64>() {
        return (seconds >= 0.0 && seconds.is_finite()).then(|| Duration::from_secs_f64(seconds));
    }
    let mut total = 0.0;
    let mut rest = value;
    while !rest.is_empty() {
        let digits = rest
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .filter(|end| *end > 0)?;
        let amount = rest[..digits].parse::<f64>().ok()?;
        rest = &rest[digits..];
        let (unit, scale) = [("ms", 0.001), ("h", 3600.0), ("m", 60.0), ("s", 1.0)]
            .into_iter()
            .find(|(unit, _)| rest.starts_with(unit))?;
        total += amount * scale;
        rest = &rest[unit.len()..];
    }
    (total.is_finite() && total < 1e9).then(|| Duration::from_secs_f64(total))
}

/// `2026-01-02T03:04:05Z`, with optional fraction and numeric offset.
fn parse_rfc3339_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| value.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let zone = &value[19..];
    let zone = zone.trim_start_matches(|character: char| character == '.' || character.is_ascii_digit());
    let offset_minutes = match zone {
        "Z" | "z" => 0,
        _ => {
            let sign = match zone.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (hours, minutes) = zone[1..].split_once(':')?;
            sign * (hours.parse::<i64>().ok()? * 60 + minutes.parse::<i64>().ok()?)
        }
    };
    // Days from civil, as in `parse_retry_after`.
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let days = era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100
        + day_of_year
        - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    u64::try_from(seconds).ok()?.checked_mul(1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn http(status: u16, headers: &[(&'static str, &str)], body: Value) -> ProviderFailure {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(*name, value.parse().unwrap());
        }
        ProviderFailure::from_http(status, &map, body.to_string().as_bytes())
    }

    #[test]
    fn reset_formats_parse() {
        assert_eq!(parse_go_duration("6m0s"), Some(Duration::from_secs(360)));
        assert_eq!(parse_go_duration("20ms"), Some(Duration::from_millis(20)));
        assert_eq!(parse_go_duration("1.5s"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_go_duration("7"), Some(Duration::from_secs(7)));
        assert_eq!(parse_go_duration("soon"), None);
        assert_eq!(parse_rfc3339_ms("1970-01-02T00:00:00Z"), Some(86_400_000));
        assert_eq!(parse_rfc3339_ms("2026-10-11T03:10:00.250+02:00"), parse_rfc3339_ms("2026-10-11T01:10:00Z"));
        assert_eq!(parse_rfc3339_ms("tomorrow"), None);
    }

    #[test]
    fn each_provider_shape_gets_its_class() {
        use FailureClass::*;
        let anthropic = |kind: &str, message: &str| json!({"type":"error","error":{"type":kind,"message":message}});
        let openai = |code: &str| json!({"error":{"message":"x","type":"t","code":code}});
        let zai = |code: &str| json!({"error":{"code":code,"message":"x"}});
        for (status, body, expected) in [
            // Anthropic
            (529, anthropic("overloaded_error", "Overloaded"), Transient),
            (500, anthropic("api_error", "internal"), Transient),
            (429, anthropic("rate_limit_error", "slow"), RateLimited),
            (402, anthropic("billing_error", "pay"), OutOfCredits),
            (400, anthropic("invalid_request_error", "Your credit balance is too low to access the Anthropic API."), OutOfCredits),
            (400, anthropic("invalid_request_error", "prompt is too long: 250000 tokens > 200000 maximum"), ContextTooLarge),
            (413, anthropic("request_too_large", "big"), ContextTooLarge),
            (401, anthropic("authentication_error", "key"), Auth),
            (403, anthropic("permission_error", "no"), Auth),
            (400, anthropic("invalid_request_error", "bad tool"), Fatal),
            // OpenAI and compatible
            (429, openai("rate_limit_exceeded"), RateLimited),
            (429, openai("insufficient_quota"), OutOfCredits),
            (503, openai("server_is_overloaded"), Transient),
            (429, openai("slow_down"), RateLimited),
            (429, openai("organization_spend_limit_exceeded"), OutOfCredits),
            (429, json!({"type":"error","error":{"type":"rate_limit_error","message":"x","details":{"error_code":"enforced_spend_limit_reached"}}}), LimitReached),
            (400, anthropic("invalid_request_error", "You have reached your specified API usage limits."), LimitReached),
            (400, openai("context_length_exceeded"), ContextTooLarge),
            (401, openai("invalid_api_key"), Auth),
            (400, openai("content_filter"), Fatal),
            (404, openai("model_not_found"), Fatal),
            // ChatGPT subscription
            (429, json!({"error":{"type":"usage_not_included","message":"x"}}), OutOfCredits),
            // OpenRouter and DeepSeek: the status decides
            (402, json!({"error":{"code":402,"message":"Insufficient credits"}}), OutOfCredits),
            (429, json!({"error":{"code":429,"message":"Rate limited"}}), RateLimited),
            (402, json!({"error":{"message":"Insufficient Balance","type":"unknown_error"}}), OutOfCredits),
            (502, json!({"error":{"code":502,"message":"upstream"}}), Transient),
            // Z.ai
            (429, zai("1302"), RateLimited),
            (429, zai("1305"), Transient),
            (429, zai("1308"), LimitReached),
            (429, zai("1319"), LimitReached),
            (429, zai("1113"), OutOfCredits),
            (400, zai("1261"), ContextTooLarge),
            (401, zai("1001"), Auth),
            (429, zai("1310"), LimitReached),
            (429, zai("1309"), OutOfCredits),
        ] {
            let failure = http(status, &[], body.clone());
            assert_eq!(failure.class, expected, "{status} {body}");
            assert_eq!(failure.status, Some(status));
            assert!(failure.message.starts_with(&format!("HTTP {status}: ")));
        }
        for (code, expected) in [(1002, RateLimited), (2056, LimitReached), (1008, OutOfCredits), (1039, ContextTooLarge), (1004, Auth), (1001, Transient), (1026, Fatal)] {
            let body = json!({"base_resp":{"status_code":code,"status_msg":"x"}});
            assert_eq!(ProviderFailure::from_body(&body, "provider error".into()).class, expected, "minimax {code}");
        }
        // Mid-stream envelopes.
        let stream = |body: Value| ProviderFailure::from_body(&body, "stream failed".into()).class;
        assert_eq!(stream(anthropic("overloaded_error", "Overloaded")), Transient);
        assert_eq!(stream(json!({"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded","message":"x"}}})), RateLimited);
        assert_eq!(stream(json!({"type":"response.failed","response":{"error":{"code":"server_error","message":"x"}}})), Transient);
        assert_eq!(stream(json!({"error":{"code":429,"message":"x"}})), RateLimited);
        assert_eq!(stream(json!({"error":{"message":"something odd","type":"provider_failure"}})), Fatal);
        assert_eq!(stream(json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}})), Fatal);
    }

    #[test]
    fn resets_come_from_the_body_and_the_documented_headers() {
        let now = now_ms();
        let near = |at: Option<u64>, expected: u64| at.is_some_and(|at| at.abs_diff(expected) < 5_000);
        // ChatGPT subscription: Unix seconds in the body.
        let at = now / 1000 + 7200;
        let failure = http(429, &[], json!({"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":at}}));
        assert_eq!((failure.class, failure.resets_at_ms), (FailureClass::LimitReached, Some(at * 1000)));
        let failure = http(429, &[], json!({"error":{"type":"usage_limit_reached","resets_in_seconds":600}}));
        assert!(near(failure.resets_at_ms, now + 600_000));
        // A short advice stays a retryable rate limit with no reset to pause on.
        let failure = http(429, &[("retry-after", "20")], json!({"error":{"code":"rate_limit_exceeded"}}));
        assert_eq!((failure.class, failure.retry_after, failure.resets_at_ms), (FailureClass::RateLimited, Some(Duration::from_secs(20)), None));
        // A long one is a reached limit with a known reset.
        let failure = http(429, &[("retry-after", "3600")], json!({}));
        assert_eq!(failure.class, FailureClass::LimitReached);
        assert!(near(failure.resets_at_ms, now + 3_600_000));
        let failure = http(429, &[("retry-after-ms", "1500")], json!({}));
        assert_eq!(failure.retry_after, Some(Duration::from_millis(1500)));
        // Only the exhausted bucket's reset counts.
        let failure = http(429, &[("x-ratelimit-remaining-requests", "0"), ("x-ratelimit-reset-requests", "6m0s"), ("x-ratelimit-remaining-tokens", "900"), ("x-ratelimit-reset-tokens", "1h0m0s")], json!({}));
        assert_eq!((failure.class, failure.retry_after), (FailureClass::LimitReached, Some(Duration::from_secs(360))));
        let failure = http(429, &[("x-ratelimit-remaining-tokens", "12"), ("x-ratelimit-reset-tokens", "1h0m0s")], json!({}));
        assert_eq!((failure.class, failure.retry_after), (FailureClass::RateLimited, None));
        let failure = http(429, &[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", &(now + 90_000).to_string())], json!({"error":{"code":429,"message":"x"}}));
        assert!(failure.retry_after.is_some_and(|delay| (85..=90).contains(&delay.as_secs())));
        let failure = http(429, &[("anthropic-ratelimit-tokens-remaining", "0"), ("anthropic-ratelimit-tokens-reset", "2999-01-01T00:00:00Z")], json!({"type":"error","error":{"type":"rate_limit_error","message":"x"}}));
        assert_eq!(failure.class, FailureClass::LimitReached);
        assert!(failure.resets_at_ms.is_some());
        let failure = http(429, &[("anthropic-ratelimit-unified-status", "rejected"), ("anthropic-ratelimit-unified-reset", &(now / 1000 + 7200).to_string())], json!({"type":"error","error":{"type":"rate_limit_error","message":"x"}}));
        assert_eq!(failure.class, FailureClass::LimitReached);
        assert!(near(failure.resets_at_ms, now + 7_200_000));
        // Credits and auth never get a wait from a header.
        let failure = http(402, &[("retry-after", "5")], json!({}));
        assert_eq!((failure.class, failure.retry_after, failure.resets_at_ms), (FailureClass::OutOfCredits, None, None));
    }
}
