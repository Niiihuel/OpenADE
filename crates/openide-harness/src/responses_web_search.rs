//! Bounded provider-hosted activity. These observations never enter ToolCall
//! execution; only original output items are used for response replay.
use crate::AgentError;
use openide_core::ModelStreamEvent;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_VISIBLE_SEARCH_ITEMS: usize = 64;

fn prefix(value: &str, cap: usize) -> String {
    let mut end = cap.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Extract a small display snapshot without cloning raw search output. The
/// complete provider item stays authoritative and is not modified or replaced.
pub fn hosted_web_search_presentation(item: &Value) -> Option<ModelStreamEvent> {
    if item.get("type").and_then(Value::as_str) != Some("web_search_call") {
        return None;
    }
    let item_id = item.get("id")?.as_str()?;
    if item_id.is_empty() || item_id.len() > 256 || item_id.chars().any(char::is_control) {
        return None;
    }
    let action = item.get("action");
    let query = action.and_then(|action| {
        action
            .get("query")
            .and_then(Value::as_str)
            .or_else(|| {
                action
                    .get("queries")?
                    .as_array()?
                    .iter()
                    .find_map(Value::as_str)
            })
            .or_else(|| action.get("pattern").and_then(Value::as_str))
    });
    ModelStreamEvent::WebSearchUpdate {
        item_id: item_id.into(),
        status: prefix(
            item.get("status")
                .and_then(Value::as_str)
                .unwrap_or("in_progress"),
            32,
        ),
        action: action
            .and_then(|value| value.get("type")?.as_str())
            .map(|value| prefix(value, 64)),
        query: query.map(|value| prefix(value, 1024)),
        url: action
            .and_then(|value| value.get("url")?.as_str())
            .map(|value| prefix(value, 2048)),
        sources: action
            .and_then(|value| value.get("sources")?.as_array())
            .into_iter()
            .flatten()
            .take(8)
            .filter_map(|source| {
                let url = source.get("url")?.as_str()?;
                let title = source.get("title").and_then(Value::as_str).unwrap_or(url);
                Some((prefix(title, 256), prefix(url, 2048)))
            })
            .collect(),
    }
    .bounded_web_search_update()
}

#[derive(Default)]
pub(super) struct WebSearchStream {
    items: BTreeMap<u64, ModelStreamEvent>,
}

impl WebSearchStream {
    pub(super) fn observe(
        &mut self,
        event: &Value,
    ) -> Result<Option<ModelStreamEvent>, AgentError> {
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_item.added" | "response.output_item.done") => {
                let Some(update) = event.get("item").and_then(hosted_web_search_presentation)
                else {
                    return Ok(None);
                };
                let Some(index) = event.get("output_index").and_then(Value::as_u64) else {
                    return Err(AgentError::Model(
                        "Responses web search item has no output index".into(),
                    ));
                };
                let ModelStreamEvent::WebSearchUpdate { item_id, .. } = &update else {
                    unreachable!()
                };
                if self.items.iter().any(|(other_index, other)| {
                    let ModelStreamEvent::WebSearchUpdate {
                        item_id: other_id, ..
                    } = other
                    else {
                        return false;
                    };
                    (*other_index == index) != (other_id == item_id)
                }) {
                    return Err(AgentError::Model(
                        "Responses web search item identity changed".into(),
                    ));
                }
                if !self.items.contains_key(&index) && self.items.len() >= MAX_VISIBLE_SEARCH_ITEMS
                {
                    return Ok(None);
                }
                if self.items.get(&index) == Some(&update) {
                    return Ok(None);
                }
                self.items.insert(index, update.clone());
                Ok(Some(update))
            }
            Some(
                "response.web_search_call.in_progress"
                | "response.web_search_call.searching"
                | "response.web_search_call.completed",
            ) => {
                let Some(index) = event.get("output_index").and_then(Value::as_u64) else {
                    return Ok(None);
                };
                let Some(item_id) = event.get("item_id").and_then(Value::as_str) else {
                    return Ok(None);
                };
                let Some(current) = self.items.get_mut(&index) else {
                    return Ok(None);
                };
                let ModelStreamEvent::WebSearchUpdate {
                    item_id: known_id,
                    status,
                    ..
                } = current
                else {
                    unreachable!()
                };
                if known_id != item_id {
                    return Ok(None);
                }
                let phase = event["type"].as_str().unwrap().rsplit('.').next().unwrap();
                if status == phase
                    || (status == "searching" && phase == "in_progress")
                    || matches!(
                        status.as_str(),
                        "completed" | "failed" | "cancelled" | "incomplete"
                    )
                {
                    return Ok(None);
                }
                *status = phase.into();
                Ok(Some(current.clone()))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
#[path = "responses_web_search_tests.rs"]
mod tests;
