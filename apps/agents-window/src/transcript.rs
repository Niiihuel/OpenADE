//! Pure, incremental projection of stored or live agent events into timeline rows.

use openide_core::{
    AgentEvent, AgentEventKind, OutputStream, PermissionResolution, UserImage, UserImageMime,
    MAX_USER_IMAGES_PER_MESSAGE, MAX_USER_IMAGE_MESSAGE_BYTES,
};
use openide_ui::{RasterImage, RasterImageFormat};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptImage {
    /// The renderer verifies and decodes only visible images on cache misses.
    /// An unavailable image retains its accessible label.
    pub raster: Option<RasterImage>,
    pub alt: String,
}

fn transcript_images(images: &[UserImage]) -> Vec<TranscriptImage> {
    let mut encoded_total = 0usize;
    let encoded_cap =
        MAX_USER_IMAGE_MESSAGE_BYTES.div_ceil(3) * 4 + MAX_USER_IMAGES_PER_MESSAGE * 4;
    images
        .iter()
        .take(MAX_USER_IMAGES_PER_MESSAGE)
        .enumerate()
        .map(|(index, image)| {
            let alt = format!("Imagen adjunta {} de {}", index + 1, images.len());
            let format = match image.mime_type {
                UserImageMime::Png => RasterImageFormat::Png,
                UserImageMime::Jpeg => RasterImageFormat::Jpeg,
                UserImageMime::Webp => RasterImageFormat::Webp,
                UserImageMime::Gif => RasterImageFormat::Gif,
            };
            let raster = encoded_total
                .checked_add(image.data_base64.len())
                .filter(|total| *total <= encoded_cap)
                .and_then(|total| {
                    encoded_total = total;
                    RasterImage::new(
                        &image.sha256,
                        format,
                        image.size_bytes,
                        Arc::from(image.data_base64.as_str()),
                        &alt,
                    )
                });
            TranscriptImage { raster, alt }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptItem {
    User {
        content: String,
        images: Vec<TranscriptImage>,
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
    WebCitation {
        title: String,
        url: String,
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
    pending_web_citations: Vec<(String, String)>,
    pending_admission_images: Vec<TranscriptImage>,
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
        projection.flush_web_citations();
        let last_admission = events.iter().rposition(|event| {
            matches!(
                &event.kind,
                AgentEventKind::TurnAdmitted { .. } | AgentEventKind::TurnAdmittedWithImages { .. }
            )
        });
        let last_start = events
            .iter()
            .rposition(|event| matches!(&event.kind, AgentEventKind::TurnStarted { .. }));
        if let Some(index) =
            last_admission.filter(|index| last_start.is_none_or(|start| *index > start))
        {
            if let AgentEventKind::TurnAdmitted { prompt, .. }
            | AgentEventKind::TurnAdmittedWithImages { prompt, .. } = &events[index].kind
            {
                projection.items.push(TranscriptItem::User {
                    content: prompt.clone(),
                    images: std::mem::take(&mut projection.pending_admission_images),
                });
                projection.items.push(TranscriptItem::Notice {
                    message: "Turno admitido, pero interrumpido antes de iniciar".to_owned(),
                    is_error: true,
                });
            }
            return projection;
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
            let uncertain_tools = projection
                .items
                .iter()
                .filter(|item| matches!(item, TranscriptItem::Tool { output: None, .. }))
                .count();
            projection.items.push(TranscriptItem::Notice {
                message: if uncertain_tools > 0 {
                    "Turno interrumpido. Una herramienta pudo haberse ejecutado; su resultado es incierto. No se reintentará automáticamente.".to_owned()
                } else {
                    "Turno interrumpido al cerrar la aplicación".to_owned()
                },
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
            | AgentEventKind::ModelContextCommitted { .. }
            | AgentEventKind::SystemInstructionsCommitted { .. }
            | AgentEventKind::PlanProposed { .. }
            | AgentEventKind::PlanUpdated { .. }
            | AgentEventKind::McpRuntimeCommitted { .. }
            | AgentEventKind::HooksCommitted { .. }
            | AgentEventKind::ExecPolicyCommitted { .. }
            | AgentEventKind::HookStarted { .. }
            | AgentEventKind::HookFinished { .. }
            | AgentEventKind::ToolCatalogCommitted { .. }
            | AgentEventKind::CheckpointCommitted { .. }
            | AgentEventKind::ModelRequestStarted { step: 1, .. } => false,
            AgentEventKind::TurnAdmitted { .. } => {
                self.pending_admission_images.clear();
                false
            }
            AgentEventKind::TurnAdmittedWithImages { images, .. } => {
                self.pending_admission_images = transcript_images(images);
                false
            }
            AgentEventKind::ModelResponseCommitted { assistant, .. } => {
                self.pending_web_citations =
                    openide_harness::hosted_web_citations(&assistant.provider_output_items)
                        .unwrap_or_default()
                        .into_iter()
                        .take(32)
                        .map(|citation| {
                            (
                                citation.title.unwrap_or_else(|| citation.url.clone()),
                                citation.url,
                            )
                        })
                        .collect();
                false
            }
            AgentEventKind::ModelRequestStarted { .. } => {
                let changed = self.finish_streaming();
                self.active_assistant = None;
                changed
            }
            AgentEventKind::TurnStarted { prompt } => {
                self.reset_turn();
                self.items.push(TranscriptItem::User {
                    content: prompt.clone(),
                    images: std::mem::take(&mut self.pending_admission_images),
                });
                true
            }
            AgentEventKind::TurnSteered { text, .. } => {
                self.finish_streaming();
                self.active_assistant = None;
                self.items.push(TranscriptItem::User {
                    content: text.clone(),
                    images: Vec::new(),
                });
                true
            }
            AgentEventKind::TurnSteeredWithImages { text, images, .. } => {
                self.finish_streaming();
                self.active_assistant = None;
                self.items.push(TranscriptItem::User {
                    content: text.clone(),
                    images: transcript_images(images),
                });
                true
            }
            AgentEventKind::AssistantDelta { content } => self.assistant_delta(content),
            AgentEventKind::AssistantMessage { content } => {
                let changed = self.assistant_message(content);
                self.flush_web_citations() || changed
            }
            AgentEventKind::ToolStarted { tool_call } => {
                self.finish_streaming();
                self.active_assistant = None;
                self.flush_web_citations();
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
            AgentEventKind::ToolBatchDiscarded { next_tool_call_id } => {
                self.items.push(TranscriptItem::Notice {
                    message: format!(
                        "Llamadas pendientes descartadas desde {next_tool_call_id}; los efectos anteriores permanecen"
                    ),
                    is_error: false,
                });
                true
            }
            AgentEventKind::ProcessOutput { stream, text } => self.process_output(*stream, text),
            AgentEventKind::TurnCompleted { response, .. } => {
                let changed = self.complete_assistant(response);
                let cited = self.flush_web_citations();
                self.reset_turn();
                changed || cited
            }
            AgentEventKind::TurnFailed { error } => {
                self.finish_streaming();
                self.flush_web_citations();
                self.items.push(TranscriptItem::Notice {
                    message: error.clone(),
                    is_error: true,
                });
                self.reset_turn();
                true
            }
            AgentEventKind::TurnCancelled => {
                self.finish_streaming();
                self.flush_web_citations();
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

    fn flush_web_citations(&mut self) -> bool {
        let mut added = false;
        for (title, url) in self.pending_web_citations.drain(..) {
            if self.items.iter().rev().take(32).any(|item| {
                matches!(item, TranscriptItem::WebCitation { url: existing, .. } if existing == &url)
            }) {
                continue;
            }
            self.items.push(TranscriptItem::WebCitation { title, url });
            added = true;
        }
        added
    }

    fn reset_turn(&mut self) {
        self.tool_rows.clear();
        self.active_assistant = None;
        self.stdout_row = None;
        self.stdout_text.clear();
        self.pending_web_citations.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder as _;
    use openide_core::{AgentMessage, SessionStore, ToolCall, TurnMode};
    use serde_json::json;

    fn event(sequence: u64, kind: AgentEventKind) -> AgentEvent {
        AgentEvent::new("session", sequence, kind)
    }

    fn image_fixture() -> UserImage {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        UserImage::from_bytes(UserImageMime::Png, &png).unwrap()
    }

    #[test]
    fn admission_and_steer_images_match_live_and_replayed_rows() {
        let image = image_fixture();
        let events = vec![
            event(
                1,
                AgentEventKind::TurnAdmittedWithImages {
                    turn_id: "turn-1".into(),
                    prompt: "Describe".into(),
                    model: "model".into(),
                    mode: TurnMode::Plan,
                    images: vec![image.clone()],
                },
            ),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Describe".into(),
                },
            ),
            event(
                3,
                AgentEventKind::TurnSteeredWithImages {
                    turn_id: "turn-1".into(),
                    input_id: "input-1".into(),
                    text: "También esta".into(),
                    images: vec![image.clone()],
                },
            ),
            event(
                4,
                AgentEventKind::TurnCompleted {
                    response: "Listo".into(),
                    steps: 1,
                },
            ),
        ];
        let mut live = TranscriptProjection::new();
        for event in &events {
            live.apply_event(event);
        }
        let reopened = TranscriptProjection::from_events(&events);
        assert_eq!(live.items(), reopened.items());
        for (index, content) in [(0, "Describe"), (1, "También esta")] {
            let TranscriptItem::User {
                content: actual,
                images,
            } = &reopened.items()[index]
            else {
                panic!("expected user image row");
            };
            assert_eq!(actual, content);
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].alt, "Imagen adjunta 1 de 1");
            assert_eq!(
                images[0].raster.as_ref().unwrap().data_base64(),
                image.data_base64
            );
        }
    }

    #[test]
    fn image_admission_reopens_from_disk_without_original_file() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::open(root.path()).unwrap();
        let session = store.create("/workspace", "native").unwrap();
        let image = image_fixture();
        store
            .admit_native_turn_with_images(
                &session.id,
                "turn-image",
                "Describe",
                "model",
                TurnMode::Plan,
                std::slice::from_ref(&image),
            )
            .unwrap();
        drop(store);
        let reopened = SessionStore::open(root.path()).unwrap();
        let events = reopened.load(&session.id).unwrap().unwrap().events;
        let projection = TranscriptProjection::from_events(&events);
        let TranscriptItem::User { images, .. } = &projection.items()[0] else {
            panic!("missing admitted user row");
        };
        assert_eq!(
            images[0].raster.as_ref().unwrap().data_base64(),
            image.data_base64
        );
        assert!(matches!(
            &projection.items()[1],
            TranscriptItem::Notice { .. }
        ));
    }

    #[test]
    fn malformed_live_image_keeps_label_for_render_boundary_validation() {
        let mut image = image_fixture();
        image.sha256 = "0".repeat(64);
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnAdmittedWithImages {
            turn_id: "turn-1".into(),
            prompt: "Describe".into(),
            model: "model".into(),
            mode: TurnMode::Build,
            images: vec![image],
        });
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Describe".into(),
        });
        let TranscriptItem::User { images, .. } = &projection.items()[0] else {
            unreachable!()
        };
        assert_eq!(images[0].alt, "Imagen adjunta 1 de 1");
        assert_eq!(images[0].raster.as_ref().unwrap().sha256(), "0".repeat(64));
    }

    #[test]
    fn saved_system_instructions_never_appear_in_transcript() {
        let content = "Instrucción privada del proyecto";
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Revisar código".into(),
                },
            ),
            event(
                2,
                AgentEventKind::SystemInstructionsCommitted {
                    version: openide_core::SYSTEM_INSTRUCTIONS_VERSION,
                    digest: openide_core::system_instructions_digest(content).unwrap(),
                    content: content.into(),
                },
            ),
        ];
        let mut projection = TranscriptProjection::new();
        for event in &events {
            projection.apply_event(event);
        }
        assert_eq!(
            projection.items(),
            &[TranscriptItem::User {
                content: "Revisar código".into(),
                images: Vec::new(),
            }]
        );
    }

    #[test]
    fn web_citation_survives_live_projection_and_reopen() {
        let mut assistant = AgentMessage::assistant("Encontré una fuente", vec![]);
        assistant.provider_output_items = vec![json!({
            "type": "message",
            "content": [{
                "type": "output_text",
                "text": "Encontré una fuente",
                "annotations": [{
                    "type": "url_citation",
                    "start_index": 0,
                    "end_index": 8,
                    "url": "https://example.com/source",
                    "title": "Fuente primaria"
                }]
            }]
        })];
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Busca".into(),
                },
            ),
            event(
                2,
                AgentEventKind::AssistantDelta {
                    content: "Encontré una fuente".into(),
                },
            ),
            event(
                3,
                AgentEventKind::ModelResponseCommitted {
                    step: 1,
                    assistant,
                    response_id: Some("response-1".into()),
                    usage: None,
                },
            ),
            event(
                4,
                AgentEventKind::AssistantMessage {
                    content: "Encontré una fuente".into(),
                },
            ),
            event(
                5,
                AgentEventKind::TurnCompleted {
                    response: "Encontré una fuente".into(),
                    steps: 1,
                },
            ),
        ];
        let mut live = TranscriptProjection::new();
        for event in &events {
            live.apply_event(event);
        }
        let reopened = TranscriptProjection::from_events(&events);
        assert_eq!(live.items(), reopened.items());
        assert!(matches!(
            &live.items()[2],
            TranscriptItem::WebCitation { title, url }
                if title == "Fuente primaria" && url == "https://example.com/source"
        ));
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
    fn replay_marks_unfinished_tool_effect_as_uncertain() {
        let events = vec![
            event(
                1,
                AgentEventKind::TurnStarted {
                    prompt: "Change file".into(),
                },
            ),
            event(
                2,
                AgentEventKind::ToolStarted {
                    tool_call: ToolCall {
                        id: "call-1".into(),
                        name: "write_file".into(),
                        arguments: json!({"path":"file.txt","content":"x"}),
                    },
                },
            ),
        ];
        let projection = TranscriptProjection::from_events(&events);
        assert!(matches!(
            &projection.items()[1],
            TranscriptItem::Tool { output: None, .. }
        ));
        assert!(matches!(
            &projection.items()[2],
            TranscriptItem::Notice { message, is_error: true }
                if message.contains("resultado es incierto")
                    && message.contains("No se reintentará automáticamente")
        ));
    }

    #[test]
    fn replay_shows_admitted_prompt_when_worker_never_started() {
        let events = vec![event(
            1,
            AgentEventKind::TurnAdmitted {
                turn_id: "turn-1".into(),
                prompt: "Inspect the workspace".into(),
                model: "model-a".into(),
                mode: TurnMode::Plan,
            },
        )];
        let projection = TranscriptProjection::from_events(&events);
        assert_eq!(projection.items().len(), 2);
        assert_eq!(
            projection.items()[0],
            TranscriptItem::User {
                content: "Inspect the workspace".into(),
                images: Vec::new(),
            }
        );
        assert!(matches!(
            &projection.items()[1],
            TranscriptItem::Notice { is_error: true, .. }
        ));
    }

    #[test]
    fn admitted_then_started_turn_projects_one_user_prompt() {
        let events = vec![
            event(
                1,
                AgentEventKind::TurnAdmitted {
                    turn_id: "turn-1".into(),
                    prompt: "Inspect the workspace".into(),
                    model: "model-a".into(),
                    mode: TurnMode::Plan,
                },
            ),
            event(
                2,
                AgentEventKind::TurnStarted {
                    prompt: "Inspect the workspace".into(),
                },
            ),
            event(
                3,
                AgentEventKind::TurnCompleted {
                    response: "Done".into(),
                    steps: 1,
                },
            ),
        ];
        let projection = TranscriptProjection::from_events(&events);
        assert_eq!(projection.items().len(), 2);
        assert!(matches!(
            &projection.items()[0],
            TranscriptItem::User { .. }
        ));
        assert!(matches!(
            &projection.items()[1],
            TranscriptItem::Assistant { .. }
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

    #[test]
    fn discarded_batch_is_visible_without_synthesizing_tool_results() {
        let mut projection = TranscriptProjection::new();
        projection.apply_kind(&AgentEventKind::TurnStarted {
            prompt: "Edit notes".into(),
        });
        projection.apply_kind(&AgentEventKind::TurnCancelled);
        projection.apply_kind(&AgentEventKind::ToolBatchDiscarded {
            next_tool_call_id: "call-2".into(),
        });
        assert_eq!(projection.items().len(), 3);
        assert!(matches!(
            &projection.items()[2],
            TranscriptItem::Notice { message, is_error: false }
                if message.contains("call-2")
        ));
    }
}
