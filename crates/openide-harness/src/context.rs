use crate::AgentError;
use openide_core::{AgentMessage, MessageRole};
use std::ops::Range;

const CLEARED_TOOL_RESULT: &str = "[Earlier tool result omitted from model context]";

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
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::ToolCall;
    use serde_json::json;

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
