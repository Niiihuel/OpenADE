//! Pure, incremental projection of stored or live agent events into timeline rows.

use openide_core::{AgentEvent, AgentEventKind, OutputStream, PermissionResolution};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptItem {
    User {
        content: String,
    },
    Assistant {
        content: String,
        streaming: bool,
    },
    Tool {
        id: String,
        name: String,
        arguments: Value,
        output: Option<String>,
        is_error: bool,
    },
    ProcessOutput {
        stream: OutputStream,
        text: String,
    },
    Notice {
        message: String,
        is_error: bool,
    },
}

/// Replaying the same ordered events produces the same rows as applying them live.
#[derive(Debug, Default)]
pub struct TranscriptProjection {
    items: Vec<TranscriptItem>,
    tool_rows: HashMap<String, usize>,
    active_assistant: Option<usize>,
    stdout_row: Option<usize>,
    stdout_text: String,
}

impl TranscriptProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_events(events: &[AgentEvent]) -> Self {
        let mut projection = Self::new();
        for event in events {
            projection.apply_event(event);
        }
        let unfinished_turn = events
            .iter()
            .rposition(|event| matches!(&event.kind, AgentEventKind::TurnStarted { .. }))
            .is_some_and(|start| {
                !events[start + 1..].iter().any(|event| {
                    matches!(
                        &event.kind,
                        AgentEventKind::TurnCompleted { .. }
                            | AgentEventKind::TurnFailed { .. }
                            | AgentEventKind::TurnCancelled
                    )
                })
            });
        if unfinished_turn {
            projection.finish_streaming();
            projection.items.push(TranscriptItem::Notice {
                message: "Turno interrumpido al cerrar la aplicación".to_owned(),
                is_error: true,
            });
            projection.reset_turn();
        }
        projection
    }

    pub fn items(&self) -> &[TranscriptItem] {
        &self.items
    }

    /// Returns whether the visible timeline changed.
    pub fn apply_event(&mut self, event: &AgentEvent) -> bool {
        self.apply_kind(&event.kind)
    }

    /// Event metadata is intentionally ignored; only its kind affects the timeline.
    pub fn apply_kind(&mut self, kind: &AgentEventKind) -> bool {
        match kind {
            AgentEventKind::SessionStarted { .. }
            | AgentEventKind::ModelRequestStarted { step: 1, .. } => false,
            AgentEventKind::ModelRequestStarted { .. } => {
                let changed = self.finish_streaming();
                self.active_assistant = None;
                changed
            }
            AgentEventKind::TurnStarted { prompt } => {
                self.reset_turn();
                self.items.push(TranscriptItem::User {
                    content: prompt.clone(),
                });
                true
            }
            AgentEventKind::AssistantDelta { content } => self.assistant_delta(content),
            AgentEventKind::AssistantMessage { content } => self.assistant_message(content),
            AgentEventKind::ToolStarted { tool_call } => {
                self.finish_streaming();
                self.active_assistant = None;
                if let Some(index) = self.tool_rows.get(&tool_call.id).copied() {
                    if let TranscriptItem::Tool {
                        name,
                        arguments,
                        output,
                        ..
                    } = &mut self.items[index]
                    {
                        if name.is_empty() || output.is_none() {
                            *name = tool_call.name.clone();
                            *arguments = tool_call.arguments.clone();
                            return true;
                        }
                    }
                }
                let index = self.items.len();
                self.items.push(TranscriptItem::Tool {
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    arguments: tool_call.arguments.clone(),
                    output: None,
                    is_error: false,
                });
                self.tool_rows.insert(tool_call.id.clone(), index);
                true
            }
            AgentEventKind::PermissionRequested { tool_call, .. } => {
                self.finish_streaming();
                self.items.push(TranscriptItem::Notice {
                    message: format!("Permiso requerido para {}", tool_call.name),
                    is_error: false,
                });
                true
            }
            AgentEventKind::PermissionResolved { resolution, .. } => {
                let (message, is_error) = match resolution {
                    PermissionResolution::Allowed => ("Permiso concedido", false),
                    PermissionResolution::Denied => ("Permiso denegado", true),
                    PermissionResolution::TimedOut => ("Permiso vencido", true),
                    PermissionResolution::Cancelled => ("Permiso cancelado", false),
                };
                self.items.push(TranscriptItem::Notice {
                    message: message.to_owned(),
                    is_error,
                });
                true
            }
            AgentEventKind::ToolFinished {
                tool_call_id,
                output,
                is_error,
            } => {
                if let Some(index) = self.tool_rows.get(tool_call_id).copied() {
                    if let TranscriptItem::Tool {
                        output: row_output,
                        is_error: row_error,
                        ..
                    } = &mut self.items[index]
                    {
                        *row_output = Some(output.clone());
                        *row_error = *is_error;
                        return true;
                    }
                }
                let index = self.items.len();
                self.items.push(TranscriptItem::Tool {
                    id: tool_call_id.clone(),
                    name: String::new(),
                    arguments: Value::Null,
                    output: Some(output.clone()),
                    is_error: *is_error,
                });
                self.tool_rows.insert(tool_call_id.clone(), index);
                true
            }
            AgentEventKind::ProcessOutput { stream, text } => self.process_output(*stream, text),
            AgentEventKind::TurnCompleted { response, .. } => {
                let changed = self.complete_assistant(response);
                self.reset_turn();
                changed
            }
            AgentEventKind::TurnFailed { error } => {
                self.finish_streaming();
                self.items.push(TranscriptItem::Notice {
                    message: error.clone(),
                    is_error: true,
                });
                self.reset_turn();
                true
            }
            AgentEventKind::TurnCancelled => {
                self.finish_streaming();
                self.items.push(TranscriptItem::Notice {
                    message: "Turno cancelado".into(),
                    is_error: false,
                });
                self.reset_turn();
                true
            }
        }
    }

    fn assistant_delta(&mut self, content: &str) -> bool {
        if content.is_empty() {
            return false;
        }
        if let Some(index) = self.active_assistant {
            if let TranscriptItem::Assistant {
                content: existing,
                streaming: true,
            } = &mut self.items[index]
            {
                existing.push_str(content);
                return true;
            }
        }
        self.active_assistant = Some(self.items.len());
        self.items.push(TranscriptItem::Assistant {
            content: content.to_owned(),
            streaming: true,
        });
        true
    }

    fn assistant_message(&mut self, content: &str) -> bool {
        if content.is_empty() {
            return false;
        }
        if let Some(index) = self.active_assistant {
            if let TranscriptItem::Assistant {
                content: existing,
                streaming,
            } = &mut self.items[index]
            {
                let changed = existing != content || *streaming;
                *existing = content.to_owned();
                *streaming = false;
                return changed;
            }
        }
        self.active_assistant = Some(self.items.len());
        self.items.push(TranscriptItem::Assistant {
            content: content.to_owned(),
            streaming: false,
        });
        true
    }

    fn process_output(&mut self, stream: OutputStream, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        if stream == OutputStream::Stdout {
            self.stdout_text.push_str(text);
            if let Some(index) = self.stdout_row {
                if let TranscriptItem::ProcessOutput { text: existing, .. } = &mut self.items[index]
                {
                    existing.push_str(text);
                    return true;
                }
            }
            self.stdout_row = Some(self.items.len());
        } else if let Some(TranscriptItem::ProcessOutput {
            stream: OutputStream::Stderr,
            text: existing,
        }) = self.items.last_mut()
        {
            existing.push_str(text);
            return true;
        }
        self.items.push(TranscriptItem::ProcessOutput {
            stream,
            text: text.to_owned(),
        });
        true
    }

    fn complete_assistant(&mut self, response: &str) -> bool {
        if let Some(index) = self.active_assistant {
            if let TranscriptItem::Assistant { content, streaming } = &mut self.items[index] {
                let changed = (!response.is_empty() && content != response) || *streaming;
                if !response.is_empty() {
                    *content = response.to_owned();
                }
                *streaming = false;
                return changed;
            }
        }
        // The CLI's final response is its captured stdout, possibly truncated by
        // the runner. Reuse the streamed row even when the capture was limited.
        if let Some(index) = self.stdout_row {
            self.items[index] = TranscriptItem::Assistant {
                content: self.stdout_text.clone(),
                streaming: false,
            };
            return true;
        }
        if !response.is_empty() {
            self.items.push(TranscriptItem::Assistant {
                content: response.to_owned(),
                streaming: false,
            });
            return true;
        }
        false
    }

    fn finish_streaming(&mut self) -> bool {
        if let Some(index) = self.active_assistant {
            if let TranscriptItem::Assistant { streaming, .. } = &mut self.items[index] {
                let changed = *streaming;
                *streaming = false;
                return changed;
            }
        }
        false
    }

    fn reset_turn(&mut self) {
        self.tool_rows.clear();
        self.active_assistant = None;
        self.stdout_row = None;
        self.stdout_text.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::ToolCall;
    use serde_json::json;

    fn event(sequence: u64, kind: AgentEventKind) -> AgentEvent {
        AgentEvent::new("session", sequence, kind)
    }

    #[test]
    fn groups_tool_calls_and_results_in_live_and_replayed_timeline() {
        let call = ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/main.rs"}),
        };
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Read".into(),
                },
            ),
            event(2, AgentEventKind::ToolStarted { tool_call: call }),
            event(
                3,
                AgentEventKind::ToolFinished {
                    tool_call_id: "call-1".into(),
                    output: "fn main() {}".into(),
                    is_error: false,
                },
            ),
            event(
                4,
                AgentEventKind::TurnCompleted {
                    response: "Found it".into(),
                    steps: 2,
                },
            ),
        ];
        let mut live = TranscriptProjection::new();
        for event in &events {
            live.apply_event(event);
        }
        let replayed = TranscriptProjection::from_events(&events);
        assert_eq!(live.items(), replayed.items());
        assert_eq!(live.items().len(), 3);
        assert!(matches!(&live.items()[1], TranscriptItem::Tool {
            id, name, arguments, output: Some(output), is_error: false,
        } if id == "call-1" && name == "read_file"
            && arguments == &json!({"path":"src/main.rs"}) && output == "fn main() {}"));
    }

    #[test]
    fn streaming_deltas_are_replaced_by_final_answer_without_duplicate_row() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Hi".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: "Hel".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: "lo".into(),
        });
        assert!(matches!(&projection.items()[1], TranscriptItem::Assistant {
            content, streaming: true,
        } if content == "Hello"));
        projection.apply_kind(&AgentEventKind::AssistantMessage {
            content: "Hello".into(),
        });
        projection.apply_kind(&AgentEventKind::TurnCompleted {
            response: "Hello!".into(),
            steps: 1,
        });
        assert_eq!(projection.items().len(), 2);
        assert_eq!(
            projection.items()[1],
            TranscriptItem::Assistant {
                content: "Hello!".into(),
                streaming: false,
            }
        );
    }

    #[test]
    fn cli_stdout_becomes_one_final_response_and_stderr_remains_visible() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Run".into(),
        });
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stdout,
            text: "Good ".into(),
        });
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stderr,
            text: "warning".into(),
        });
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stdout,
            text: "morning".into(),
        });
        assert!(
            matches!(&projection.items()[1], TranscriptItem::ProcessOutput {
            stream: OutputStream::Stdout, text,
        } if text == "Good morning")
        );
        projection.apply_kind(&AgentEventKind::TurnCompleted {
            response: "Good morning".into(),
            steps: 1,
        });
        assert_eq!(projection.items().len(), 3);
        assert_eq!(
            projection.items()[1],
            TranscriptItem::Assistant {
                content: "Good morning".into(),
                streaming: false,
            }
        );
        assert!(
            matches!(&projection.items()[2], TranscriptItem::ProcessOutput {
            stream: OutputStream::Stderr, text,
        } if text == "warning")
        );
    }

    #[test]
    fn truncated_cli_capture_does_not_duplicate_streamed_output() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Run".into(),
        });
        projection.apply_kind(&AgentEventKind::ProcessOutput {
            stream: OutputStream::Stdout,
            text: "complete streamed output".into(),
        });
        projection.apply_kind(&AgentEventKind::TurnCompleted {
            response: "complete\n[output truncated]".into(),
            steps: 1,
        });
        assert_eq!(projection.items().len(), 2);
        assert_eq!(
            projection.items()[1],
            TranscriptItem::Assistant {
                content: "complete streamed output".into(),
                streaming: false,
            }
        );
    }

    #[test]
    fn replay_marks_crashed_turn_as_interrupted() {
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Run".into(),
                },
            ),
            event(
                2,
                AgentEventKind::AssistantDelta {
                    content: "Partial".into(),
                },
            ),
        ];
        let projection = TranscriptProjection::from_events(&events);
        assert_eq!(projection.items().len(), 3);
        assert!(matches!(&projection.items()[1], TranscriptItem::Assistant {
            content, streaming: false,
        } if content == "Partial"));
        assert!(matches!(
            &projection.items()[2],
            TranscriptItem::Notice { is_error: true, .. }
        ));
    }

    #[test]
    fn cancellation_preserves_partial_output_and_adds_notice() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Long task".into(),
        });
        projection.apply_kind(&AgentEventKind::AssistantDelta {
            content: "Partial".into(),
        });
        projection.apply_kind(&AgentEventKind::TurnCancelled);
        assert_eq!(projection.items().len(), 3);
        assert_eq!(
            projection.items()[1],
            TranscriptItem::Assistant {
                content: "Partial".into(),
                streaming: false,
            }
        );
        assert_eq!(
            projection.items()[2],
            TranscriptItem::Notice {
                message: "Turno cancelado".into(),
                is_error: false,
            }
        );
    }
}
