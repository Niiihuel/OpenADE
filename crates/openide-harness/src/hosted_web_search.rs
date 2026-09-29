//! Hosted Responses web search request options and typed result extraction.
//!
//! The original output items remain authoritative for stateless replay. These
//! types are a bounded view for the host UI and never replace the raw items.

use crate::AgentError;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MAX_SEARCH_DOMAINS: usize = 100;
const MAX_SEARCH_DOMAIN_BYTES: usize = 253;
const MAX_LOCATION_FIELD_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedWebSearchMode {
    Cached,
    Indexed,
    Live,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedWebSearchContextSize {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostedWebSearchLocation {
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub timezone: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostedWebSearchConfig {
    pub mode: HostedWebSearchMode,
    pub context_size: Option<HostedWebSearchContextSize>,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
    pub location: Option<HostedWebSearchLocation>,
    /// Ask the provider to include all consulted sources in search actions.
    pub include_sources: bool,
}

impl HostedWebSearchConfig {
    pub fn new(mode: HostedWebSearchMode) -> Self {
        Self {
            mode,
            context_size: None,
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            location: None,
            include_sources: true,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), AgentError> {
        if !self.allowed_domains.is_empty() && !self.blocked_domains.is_empty() {
            return Err(AgentError::InvalidRequest(
                "hosted web search accepts allowed or blocked domains, not both".into(),
            ));
        }
        for domains in [&self.allowed_domains, &self.blocked_domains] {
            if domains.len() > MAX_SEARCH_DOMAINS {
                return Err(AgentError::InvalidRequest(format!(
                    "hosted web search accepts at most {MAX_SEARCH_DOMAINS} domains"
                )));
            }
            for domain in domains {
                if domain.is_empty()
                    || domain.len() > MAX_SEARCH_DOMAIN_BYTES
                    || domain.trim() != domain
                    || domain.starts_with(['.', '-'])
                    || domain.ends_with(['.', '-'])
                    || domain.contains("..")
                    || !domain
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
                {
                    return Err(AgentError::InvalidRequest(format!(
                        "invalid hosted web search domain: {domain}"
                    )));
                }
            }
        }
        if let Some(location) = &self.location {
            for value in [
                &location.country,
                &location.region,
                &location.city,
                &location.timezone,
            ] {
                if value
                    .as_deref()
                    .is_some_and(|value| value.is_empty() || value.len() > MAX_LOCATION_FIELD_BYTES)
                {
                    return Err(AgentError::InvalidRequest(
                        "hosted web search location field is empty or too long".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn wire_tool(&self) -> Value {
        let mut tool = json!({
            "type": "web_search",
            "external_web_access": self.mode != HostedWebSearchMode::Cached,
        });
        if self.mode == HostedWebSearchMode::Indexed {
            tool["indexed_web_access"] = Value::Bool(true);
        }
        if !self.allowed_domains.is_empty() {
            tool["filters"] = json!({"allowed_domains": self.allowed_domains});
        } else if !self.blocked_domains.is_empty() {
            tool["filters"] = json!({"blocked_domains": self.blocked_domains});
        }
        if let Some(size) = self.context_size {
            tool["search_context_size"] = Value::String(
                match size {
                    HostedWebSearchContextSize::Low => "low",
                    HostedWebSearchContextSize::Medium => "medium",
                    HostedWebSearchContextSize::High => "high",
                }
                .into(),
            );
        }
        if let Some(location) = &self.location {
            let mut wire = json!({"type":"approximate"});
            for (name, value) in [
                ("country", &location.country),
                ("region", &location.region),
                ("city", &location.city),
                ("timezone", &location.timezone),
            ] {
                if let Some(value) = value {
                    wire[name] = Value::String(value.clone());
                }
            }
            tool["user_location"] = wire;
        }
        tool
    }

    pub(crate) fn fingerprint_component(&self) -> String {
        // All fields are JSON scalar/string collections, so serialization is infallible.
        serde_json::to_string(self).expect("hosted search config is serializable")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostedWebSource {
    pub source_type: Option<String>,
    pub url: Option<String>,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostedWebSearchAction {
    Search {
        query: Option<String>,
        queries: Vec<String>,
        sources: Vec<HostedWebSource>,
    },
    OpenPage {
        url: Option<String>,
    },
    FindInPage {
        url: Option<String>,
        pattern: Option<String>,
    },
    Other {
        kind: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostedWebSearchCall {
    pub id: Option<String>,
    pub status: Option<String>,
    pub action: HostedWebSearchAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostedWebCitation {
    pub output_index: usize,
    pub content_index: usize,
    pub start_index: usize,
    pub end_index: usize,
    pub url: String,
    pub title: Option<String>,
}

/// Parse provider-hosted search calls from durable Responses output items.
pub fn hosted_web_search_calls(items: &[Value]) -> Result<Vec<HostedWebSearchCall>, AgentError> {
    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
        .map(parse_search_call)
        .collect()
}

fn parse_search_call(item: &Value) -> Result<HostedWebSearchCall, AgentError> {
    let action = item.get("action");
    let kind = action
        .and_then(|action| action.get("type"))
        .and_then(Value::as_str);
    let action = match kind {
        Some("search") => {
            let queries = action
                .and_then(|action| action.get("queries"))
                .and_then(Value::as_array)
                .map(|queries| {
                    queries
                        .iter()
                        .map(|query| {
                            query.as_str().map(str::to_owned).ok_or_else(|| {
                                AgentError::Model("web search query is not a string".into())
                            })
                        })
                        .collect()
                })
                .transpose()?
                .unwrap_or_default();
            let sources = action
                .and_then(|action| action.get("sources"))
                .and_then(Value::as_array)
                .map(|sources| {
                    sources
                        .iter()
                        .map(|source| HostedWebSource {
                            source_type: optional_string(source, "type"),
                            url: optional_string(source, "url"),
                            title: optional_string(source, "title"),
                        })
                        .collect()
                })
                .unwrap_or_default();
            HostedWebSearchAction::Search {
                query: action.and_then(|action| optional_string(action, "query")),
                queries,
                sources,
            }
        }
        Some("open_page") => HostedWebSearchAction::OpenPage {
            url: action.and_then(|action| optional_string(action, "url")),
        },
        Some("find_in_page") => HostedWebSearchAction::FindInPage {
            url: action.and_then(|action| optional_string(action, "url")),
            pattern: action.and_then(|action| optional_string(action, "pattern")),
        },
        _ => HostedWebSearchAction::Other {
            kind: kind.map(str::to_owned),
        },
    };
    Ok(HostedWebSearchCall {
        id: optional_string(item, "id"),
        status: optional_string(item, "status"),
        action,
    })
}

/// Parse URL citations from durable Responses output items for transcript UI.
pub fn hosted_web_citations(items: &[Value]) -> Result<Vec<HostedWebCitation>, AgentError> {
    let mut citations = Vec::new();
    for (output_index, item) in items.iter().enumerate() {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for (content_index, part) in parts.iter().enumerate() {
            if part.get("type").and_then(Value::as_str) != Some("output_text") {
                continue;
            }
            let Some(annotations) = part.get("annotations").and_then(Value::as_array) else {
                continue;
            };
            for annotation in annotations {
                if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
                    continue;
                }
                let start_index = required_index(annotation, "start_index")?;
                let end_index = required_index(annotation, "end_index")?;
                if end_index < start_index {
                    return Err(AgentError::Model(
                        "web citation end index precedes start index".into(),
                    ));
                }
                let url = annotation
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|url| !url.is_empty())
                    .ok_or_else(|| AgentError::Model("web citation has no URL".into()))?;
                let parsed = Url::parse(url)
                    .map_err(|_| AgentError::Model("web citation URL is invalid".into()))?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err(AgentError::Model(
                        "web citation URL must use HTTP or HTTPS".into(),
                    ));
                }
                citations.push(HostedWebCitation {
                    output_index,
                    content_index,
                    start_index,
                    end_index,
                    url: url.to_owned(),
                    title: optional_string(annotation, "title"),
                });
            }
        }
    }
    Ok(citations)
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn required_index(value: &Value, field: &str) -> Result<usize, AgentError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| AgentError::Model(format!("web citation has no valid {field}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_map_to_codex_hosted_tool_wire_shape() {
        let mut config = HostedWebSearchConfig::new(HostedWebSearchMode::Indexed);
        config.allowed_domains = vec!["example.com".into()];
        config.context_size = Some(HostedWebSearchContextSize::High);
        config.location = Some(HostedWebSearchLocation {
            country: Some("US".into()),
            city: Some("Boston".into()),
            ..Default::default()
        });
        config.validate().unwrap();
        assert_eq!(
            config.wire_tool(),
            json!({
                "type":"web_search",
                "external_web_access":true,
                "indexed_web_access":true,
                "filters":{"allowed_domains":["example.com"]},
                "search_context_size":"high",
                "user_location":{"type":"approximate","country":"US","city":"Boston"},
            })
        );
        assert_eq!(
            HostedWebSearchConfig::new(HostedWebSearchMode::Cached).wire_tool()
                ["external_web_access"],
            false
        );
        assert_eq!(
            HostedWebSearchConfig::new(HostedWebSearchMode::Live).wire_tool()
                ["external_web_access"],
            true
        );
    }

    #[test]
    fn filters_fail_closed_before_http() {
        let mut config = HostedWebSearchConfig::new(HostedWebSearchMode::Live);
        config.allowed_domains.push("https://example.com".into());
        assert!(config.validate().is_err());
        config.allowed_domains = vec!["example.com".into()];
        config.blocked_domains = vec!["other.example".into()];
        assert!(config.validate().is_err());
        config.blocked_domains.clear();
        config.allowed_domains = vec!["example.com?ignored=true".into()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn extracts_actions_sources_and_citations_without_losing_raw_items() {
        let items = vec![
            json!({"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","queries":["term"],"sources":[{"type":"url","url":"https://example.com","title":"Example"}]}}),
            json!({"type":"message","content":[{"type":"output_text","text":"Answer","annotations":[{"type":"url_citation","start_index":0,"end_index":6,"url":"https://example.com","title":"Example"}]}]}),
        ];
        let calls = hosted_web_search_calls(&items).unwrap();
        assert_eq!(calls[0].id.as_deref(), Some("ws_1"));
        assert!(
            matches!(&calls[0].action, HostedWebSearchAction::Search { queries, sources, .. } if queries == &["term"] && sources[0].url.as_deref() == Some("https://example.com"))
        );
        let citations = hosted_web_citations(&items).unwrap();
        assert_eq!(citations[0].url, "https://example.com");
        assert_eq!(citations[0].start_index, 0);
        assert_eq!(citations[0].end_index, 6);
        let bad = vec![
            json!({"type":"message","content":[{"type":"output_text","text":"x","annotations":[{"type":"url_citation","start_index":0,"end_index":1,"url":"javascript:alert(1)"}]}]}),
        ];
        assert!(hosted_web_citations(&bad).is_err());
    }
}
