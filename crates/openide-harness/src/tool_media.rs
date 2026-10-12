//! Extract image observations before the model-facing result is truncated:
//! what the user is shown, and which of those images the model is given.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use openide_core::{
    tool_image_for_model, AgentEventKind, ToolCall, UserImage, UserImageMime,
    MAX_TOOL_MEDIA_BYTES, MAX_TOOL_MEDIA_IMAGES, MAX_TOOL_MEDIA_NOTICES,
    MAX_TOOL_MEDIA_TEXT_BYTES,
};
use serde::Deserialize;
use std::borrow::Cow;

const MAX_RESULT_BYTES: usize = 12 * 1024 * 1024;
const MAX_BLOCKS: usize = 128;

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(borrow)]
    content: Blocks<'a>,
}

struct Blocks<'a> {
    values: Vec<Block<'a>>,
    limited: bool,
    saw_image: bool,
}

impl<'de: 'a, 'a> Deserialize<'de> for Blocks<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<'a>(std::marker::PhantomData<&'a ()>);
        impl<'de: 'a, 'a> serde::de::Visitor<'de> for Visitor<'a> {
            type Value = Blocks<'a>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("MCP content blocks")
            }
            fn visit_seq<S: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: S,
            ) -> Result<Self::Value, S::Error> {
                let mut result = Blocks {
                    values: Vec::new(),
                    limited: false,
                    saw_image: false,
                };
                while result.values.len() < MAX_BLOCKS {
                    let Some(block) = sequence.next_element::<Block<'a>>()? else {
                        return Ok(result);
                    };
                    result.saw_image |= block.kind == "image";
                    result.values.push(block);
                }
                // The remaining bodies are ignored; retain only evidence that
                // images were omitted, without allocating an unbounded vector.
                #[derive(Deserialize)]
                struct Kind<'a> {
                    #[serde(rename = "type", borrow)]
                    kind: Cow<'a, str>,
                }
                while let Some(block) = sequence.next_element::<Kind<'a>>()? {
                    result.limited = true;
                    result.saw_image |= block.kind == "image";
                }
                Ok(result)
            }
        }
        deserializer.deserialize_seq(Visitor(std::marker::PhantomData))
    }
}

#[derive(Deserialize)]
struct Block<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(rename = "mimeType", borrow)]
    mime: Option<Cow<'a, str>>,
    #[serde(borrow)]
    data: Option<Cow<'a, str>>,
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

fn notice(notices: &mut Vec<String>, text: &str) {
    if notices.len() < MAX_TOOL_MEDIA_NOTICES && !notices.iter().any(|item| item == text) {
        notices.push(text.into());
    }
}

/// The images of one tool result: the event the user's view is built from,
/// and what the model reads where each image block was.
pub(crate) struct ToolMedia {
    pub(crate) event: AgentEventKind,
    /// One line per image block, in the result's order.
    notes: Vec<String>,
}

impl ToolMedia {
    /// The images the model-facing tool result carries.
    pub(crate) fn model_images(&self) -> Vec<UserImage> {
        match &self.event {
            AgentEventKind::ToolResultMedia { images, model_images, .. } => {
                openide_core::model_tool_images(images, model_images).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }
}

/// What the model reads for an image it is not given.
fn omitted(mime: &str, detail: &str, why: &str) -> String {
    format!("[image omitted ({mime}{detail}): {why}]")
}

pub(crate) fn extract_tool_result_media(call: &ToolCall, output: &str) -> Option<ToolMedia> {
    if !call.name.starts_with("mcp__") || output.len() > MAX_RESULT_BYTES {
        return None;
    }
    let mut documents = serde_json::Deserializer::from_str(output).into_iter::<Envelope<'_>>();
    let parsed = documents.next()?.ok()?;
    let suffix = &output[documents.byte_offset()..];
    // Command hooks append trusted model context after the original MCP JSON.
    // Accept exactly that representation, without splitting JSON string data
    // on a marker or silently accepting arbitrary trailing garbage.
    if !suffix.trim().is_empty() && !suffix.starts_with("\nHook context:\n") {
        return None;
    }
    if !parsed.content.saw_image {
        return None;
    }
    let mut images = Vec::new();
    let mut model_images = Vec::new();
    let mut notes = Vec::new();
    let mut text = String::new();
    let mut notices = Vec::new();
    if parsed.content.limited {
        notice(
            &mut notices,
            "Vista previa limitada a los primeros 128 bloques del resultado",
        );
    }
    let mut total_bytes = 0;
    for block in parsed.content.values {
        if block.kind == "text" {
            if let Some(content) = block.text.filter(|content| !content.is_empty()) {
                if !text.is_empty() && text.len() < MAX_TOOL_MEDIA_TEXT_BYTES {
                    text.push('\n');
                }
                let mut end = content.len().min(MAX_TOOL_MEDIA_TEXT_BYTES - text.len());
                while !content.is_char_boundary(end) {
                    end -= 1;
                }
                text.push_str(&content[..end]);
                if end < content.len() {
                    notice(&mut notices, "Texto limitado en la vista previa multimedia");
                }
            }
            continue;
        }
        if block.kind != "image" {
            continue;
        }
        let named = block.mime.as_deref().unwrap_or("unknown type");
        // The user reads the notice; the model reads the note, in the place
        // of the block. Neither is shown an image that fails here.
        let mut refuse = |notices: &mut Vec<String>, text: &str, why: &str| {
            notice(notices, text);
            notes.push(omitted(named, "", &format!("{why}; neither the user nor the model sees it")));
        };
        if images.len() >= MAX_TOOL_MEDIA_IMAGES {
            refuse(
                &mut notices,
                "Se muestran las primeras 4 imágenes del resultado",
                "only the first 4 images of a result are kept",
            );
            continue;
        }
        let mime = match block.mime.as_deref() {
            Some("image/png") => UserImageMime::Png,
            Some("image/jpeg") => UserImageMime::Jpeg,
            Some("image/webp") => UserImageMime::Webp,
            Some("image/gif") => UserImageMime::Gif,
            _ => {
                refuse(&mut notices, "Formato de imagen no disponible", "unsupported image type");
                continue;
            }
        };
        let Some(data) = block.data else {
            refuse(&mut notices, "La imagen no contiene datos", "the block has no data");
            continue;
        };
        let remaining = MAX_TOOL_MEDIA_BYTES - total_bytes;
        let bytes = if data.len() > remaining.div_ceil(3) * 4 + 4 {
            None
        } else {
            match STANDARD.decode(data.as_bytes()) {
                Ok(bytes) => Some(bytes).filter(|bytes| bytes.len() <= remaining),
                Err(_) => {
                    refuse(
                        &mut notices,
                        "Datos de imagen incompletos o inválidos",
                        "its data is incomplete or invalid",
                    );
                    continue;
                }
            }
        };
        let Some(bytes) = bytes else {
            refuse(
                &mut notices,
                "Las imágenes superan el límite de vista previa de 8 MiB",
                "the images of this result exceed 8 MiB",
            );
            continue;
        };
        let Ok(image) = UserImage::from_bytes(mime, &bytes) else {
            refuse(
                &mut notices,
                "El contenido de la imagen no coincide con su formato",
                "its content does not match its type (an animated GIF is not accepted)",
            );
            continue;
        };
        total_bytes += bytes.len();
        notes.push(match tool_image_for_model(mime, &bytes) {
            Ok((width, height)) => {
                model_images.push(images.len() as u8);
                format!("[image {}: {named}, {width}x{height} px]", model_images.len())
            }
            Err(why) => omitted(
                named,
                &format!(", {} bytes", bytes.len()),
                &format!("{why}; it is shown to the user, the model does not receive it"),
            ),
        });
        images.push(image);
    }
    Some(ToolMedia {
        event: AgentEventKind::ToolResultMedia {
            tool_call_id: call.id.clone(),
            images,
            text,
            notices,
            model_images,
        },
        notes,
    })
}

/// The text of the result as the model reads it: the same MCP JSON with each
/// image block replaced by a line that numbers the image given alongside it
/// (`media`) or says why there is none. The base64 itself would only fill the
/// result limit with noise.
pub(crate) fn without_image_data<'a>(
    call: &ToolCall,
    output: &'a str,
    media: Option<&ToolMedia>,
) -> Cow<'a, str> {
    if !call.name.starts_with("mcp__") || output.len() > MAX_RESULT_BYTES || !output.contains("\"image\"") {
        return Cow::Borrowed(output);
    }
    let mut documents = serde_json::Deserializer::from_str(output).into_iter::<serde_json::Value>();
    let Some(Ok(mut result)) = documents.next() else {
        return Cow::Borrowed(output);
    };
    // The same trailing hook context `extract_tool_result_media` accepts.
    let suffix = &output[documents.byte_offset()..];
    if !suffix.trim().is_empty() && !suffix.starts_with("\nHook context:\n") {
        return Cow::Borrowed(output);
    }
    let mut notes = media.map(|media| media.notes.iter()).into_iter().flatten();
    let mut replaced = false;
    for block in result.get_mut("content").and_then(serde_json::Value::as_array_mut).into_iter().flatten() {
        if block.get("type").and_then(serde_json::Value::as_str) != Some("image") {
            continue;
        }
        let mime = block.get("mimeType").and_then(serde_json::Value::as_str).unwrap_or("unknown type");
        // Past the blocks that were read, or in a result that was not: no
        // image was kept for anyone.
        let note = notes.next().cloned().unwrap_or_else(|| {
            omitted(mime, "", "it was not read; neither the user nor the model sees it")
        });
        *block = serde_json::json!({"type": "text", "text": note});
        replaced = true;
    }
    if replaced {
        Cow::Owned(format!("{result}{suffix}"))
    } else {
        Cow::Borrowed(output)
    }
}

/// The first bytes of a PNG of this size, padded to `len`. Enough for the
/// harness, which reads the header and never decodes pixels.
#[cfg(test)]
pub(crate) fn test_png(width: u32, height: u32, len: usize) -> Vec<u8> {
    let mut bytes = [
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".as_slice(),
        &width.to_be_bytes(),
        &height.to_be_bytes(),
    ]
    .concat();
    bytes.resize(len.max(bytes.len()), 0);
    bytes
}

#[cfg(test)]
pub(crate) fn test_mcp_image_result() -> (String, Vec<u8>) {
    // A 1280x800 PNG header larger than the normal 16K tool result limit.
    // Pixel decoding is independently checked in the UI layer.
    let bytes = test_png(1280, 800, 32 * 1024);
    let output = serde_json::json!({"content":[
        {"type":"text", "text":"Captura recibida"},
        {"type":"image", "mimeType":"image/png", "data":STANDARD.encode(&bytes)}
    ], "isError":false})
    .to_string();
    (output, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_reads_a_numbered_line_where_an_image_was_and_everything_else_as_it_came() {
        let (output, _) = test_mcp_image_result();
        let call = ToolCall { id: "c".into(), name: "mcp__server__shot".into(), arguments: serde_json::json!({}) };
        let media = extract_tool_result_media(&call, &output).unwrap();
        let read = without_image_data(&call, &output, Some(&media));
        assert_eq!(
            read,
            r#"{"content":[{"text":"Captura recibida","type":"text"},{"text":"[image 1: image/png, 1280x800 px]","type":"text"}],"isError":false}"#
        );
        // The image itself travels beside the text, and is what the user is shown.
        assert!(matches!(
            &media.event,
            AgentEventKind::ToolResultMedia { images, model_images, .. }
                if *images == media.model_images() && *model_images == [0]
        ));
        // Hook context appended after the JSON stays where it was.
        let hooked = format!("{output}\nHook context:\nrevisar");
        assert!(without_image_data(&call, &hooked, Some(&media)).ends_with("\nHook context:\nrevisar"));
        // A result nobody could read as media keeps no image block either.
        assert!(without_image_data(&call, &output, None)
            .contains("[image omitted (image/png): it was not read; neither the user nor the model sees it]"));
        // Text blocks and results without images are not rewritten.
        let text = serde_json::json!({"content":[{"type":"text","text":"una \"image\" citada"}]}).to_string();
        assert!(matches!(without_image_data(&call, &text, None), Cow::Borrowed(_)));
        // Another tool's output is never parsed, whatever it looks like.
        let local = ToolCall { name: "read_file".into(), ..call.clone() };
        assert!(matches!(without_image_data(&local, &output, None), Cow::Borrowed(_)));
        assert!(matches!(without_image_data(&call, "{\"content\":[{\"type\":\"image\"}]} trailing", None), Cow::Borrowed(_)));
    }

    #[test]
    fn an_image_a_model_cannot_take_is_shown_to_the_user_and_described_to_the_model() {
        let block = |bytes: &[u8]| json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(bytes)});
        let output = json!({"content":[
            block(&test_png(1280, 9000, 0)),
            block(&test_png(640, 480, 0)),
            block(&test_png(800, 600, openide_core::MAX_TOOL_MODEL_IMAGE_BYTES + 1)),
            {"type":"image","mimeType":"image/svg+xml","data":""},
            block(b"\x89PNG\r\n\x1a\nno header"),
            block(&test_png(10, 10, 0)),
        ]})
        .to_string();
        let media = extract_tool_result_media(&call(), &output).unwrap();
        let AgentEventKind::ToolResultMedia { images, model_images, notices, .. } = &media.event else {
            unreachable!()
        };
        // The user is shown the four that are images; the model is given the
        // one within its limits, never one it could not measure.
        assert_eq!(images.len(), 4);
        assert_eq!(*model_images, [1]);
        assert_eq!(media.model_images(), [images[1].clone()]);
        assert_eq!(notices.len(), 2, "{notices:?}");
        validate_tool_result_media(images, "", notices).unwrap();
        let read = without_image_data(&call(), &output, Some(&media));
        let lines: Vec<String> = serde_json::from_str::<serde_json::Value>(&read).unwrap()["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["text"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(lines, [
            "[image omitted (image/png, 24 bytes): wider or taller than the 8000 px a model accepts; it is shown to the user, the model does not receive it]",
            "[image 1: image/png, 640x480 px]",
            "[image omitted (image/png, 3750001 bytes): larger than the 3.75 MB a model accepts for one image; it is shown to the user, the model does not receive it]",
            "[image omitted (image/svg+xml): unsupported image type; neither the user nor the model sees it]",
            "[image omitted (image/png, 17 bytes): its pixel size could not be read; it is shown to the user, the model does not receive it]",
            "[image omitted (image/png): only the first 4 images of a result are kept; neither the user nor the model sees it]",
        ]);
    }
    use openide_core::validate_tool_result_media;
    use serde_json::json;

    fn call() -> ToolCall {
        ToolCall {
            id: "image-call".into(),
            name: "mcp__browser__snapshot".into(),
            arguments: json!({}),
        }
    }
    #[test]
    fn tool_media_preserves_large_images_before_text_truncation_and_keeps_unicode_caption() {
        let (output, bytes) = test_mcp_image_result();
        assert!(output.len() > 16_000);
        let AgentEventKind::ToolResultMedia {
            images,
            text,
            notices,
            ..
        } = extract_tool_result_media(&call(), &output).unwrap().event
        else {
            unreachable!()
        };
        assert_eq!(images[0].verified_bytes().unwrap(), bytes);
        assert_eq!(text, "Captura recibida");
        assert!(notices.is_empty());
        let output = json!({"content":[{"type":"text","text":"🦀".repeat(2000)}, {"type":"image","mimeType":"image/png","data":"invalid"}]}).to_string();
        let AgentEventKind::ToolResultMedia {
            images,
            text,
            notices,
            ..
        } = extract_tool_result_media(&call(), &output).unwrap().event
        else {
            unreachable!()
        };
        assert_eq!(text.len(), MAX_TOOL_MEDIA_TEXT_BYTES);
        assert!(images.is_empty());
        assert_eq!(notices.len(), 2);
        validate_tool_result_media(&images, &text, &notices).unwrap();
    }

    #[test]
    fn tool_media_caps_blocks_images_and_notices_without_hiding_text_after_images() {
        let block = json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture")});
        let mut blocks = vec![block; 130];
        blocks[10] = json!({"type":"text","text":"caption after image limit"});
        let AgentEventKind::ToolResultMedia {
            images,
            text,
            notices,
            ..
        } = extract_tool_result_media(&call(), &json!({"content":blocks}).to_string()).unwrap().event
        else {
            unreachable!()
        };
        assert_eq!(images.len(), 4);
        assert_eq!(text, "caption after image limit");
        assert_eq!(notices.len(), 2);
        validate_tool_result_media(&images, &text, &notices).unwrap();
        let mut blocks = vec![json!({"type":"text","text":"text"}); 128];
        assert!(
            extract_tool_result_media(&call(), &json!({"content":blocks}).to_string()).is_none()
        );
        blocks.push(json!({"type":"image"}));
        let AgentEventKind::ToolResultMedia {
            images, notices, ..
        } = extract_tool_result_media(&call(), &json!({"content":blocks}).to_string()).unwrap().event
        else {
            unreachable!()
        };
        assert!(images.is_empty());
        assert_eq!(notices.len(), 1);
    }

    #[test]
    fn tool_media_does_not_interpret_local_results_or_malformed_envelopes() {
        let (output, _) = test_mcp_image_result();
        assert!(extract_tool_result_media(
            &call(),
            &format!("{output}\nHook context:\nTrusted context")
        )
        .is_some());
        assert!(
            extract_tool_result_media(&call(), &format!("{output}\narbitrary trailing text"))
                .is_none()
        );
        let mut local = call();
        local.name = "read_file".into();
        assert!(extract_tool_result_media(&local, &output).is_none());
        for output in [
            "plain text",
            "{\"content\":[",
            "{\"content\":[{\"type\":\"text\",\"text\":\"fine\"}]}",
        ] {
            assert!(extract_tool_result_media(&call(), output).is_none());
        }
        for block in [
            json!({"type":"image","mimeType":"image/jpeg","data":STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture")}),
            json!({"type":"image","mimeType":"image/png","data":"invalid"}),
            json!({"type":"image","mimeType":"image/svg+xml","data":""}),
            json!({"type":"image","mimeType":"image/png"}),
        ] {
            let AgentEventKind::ToolResultMedia {
                images,
                text,
                notices,
                ..
            } = extract_tool_result_media(&call(), &json!({"content":[block]}).to_string())
                .unwrap()
                .event
            else {
                unreachable!()
            };
            assert!(images.is_empty());
            assert_eq!(notices.len(), 1);
            validate_tool_result_media(&images, &text, &notices).unwrap();
        }
    }
}
