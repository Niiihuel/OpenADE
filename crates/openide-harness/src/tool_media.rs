//! Extract image observations before the model-facing result is truncated.
//! This is presentation data: it never changes the executor result or history.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use openide_core::{
    AgentEventKind, ToolCall, UserImage, UserImageMime, MAX_TOOL_MEDIA_BYTES,
    MAX_TOOL_MEDIA_IMAGES, MAX_TOOL_MEDIA_NOTICES, MAX_TOOL_MEDIA_TEXT_BYTES,
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

pub(crate) fn extract_tool_result_media(call: &ToolCall, output: &str) -> Option<AgentEventKind> {
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
        if images.len() >= MAX_TOOL_MEDIA_IMAGES {
            notice(
                &mut notices,
                "Se muestran las primeras 4 imágenes del resultado",
            );
            continue;
        }
        let mime = match block.mime.as_deref() {
            Some("image/png") => UserImageMime::Png,
            Some("image/jpeg") => UserImageMime::Jpeg,
            Some("image/webp") => UserImageMime::Webp,
            Some("image/gif") => UserImageMime::Gif,
            _ => {
                notice(&mut notices, "Formato de imagen no disponible");
                continue;
            }
        };
        let Some(data) = block.data else {
            notice(&mut notices, "La imagen no contiene datos");
            continue;
        };
        let remaining = MAX_TOOL_MEDIA_BYTES - total_bytes;
        if data.len() > remaining.div_ceil(3) * 4 + 4 {
            notice(
                &mut notices,
                "Las imágenes superan el límite de vista previa de 8 MiB",
            );
            continue;
        }
        let bytes = match STANDARD.decode(data.as_bytes()) {
            Ok(bytes) if bytes.len() <= remaining => bytes,
            Ok(_) => {
                notice(
                    &mut notices,
                    "Las imágenes superan el límite de vista previa de 8 MiB",
                );
                continue;
            }
            Err(_) => {
                notice(&mut notices, "Datos de imagen incompletos o inválidos");
                continue;
            }
        };
        match UserImage::from_bytes(mime, &bytes) {
            Ok(image) => {
                total_bytes += bytes.len();
                images.push(image);
            }
            Err(_) => notice(
                &mut notices,
                "El contenido de la imagen no coincide con su formato",
            ),
        }
    }
    Some(AgentEventKind::ToolResultMedia {
        tool_call_id: call.id.clone(),
        images,
        text,
        notices,
    })
}

/// The result as the model reads it: the same MCP JSON with each image block
/// replaced by a line of text saying what was left out. No provider adapter
/// sends tool-result images, so the base64 would only fill the result limit
/// with noise; the images reach the user through [`extract_tool_result_media`].
pub(crate) fn without_image_data<'a>(call: &ToolCall, output: &'a str) -> Cow<'a, str> {
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
    let mut replaced = false;
    for block in result.get_mut("content").and_then(serde_json::Value::as_array_mut).into_iter().flatten() {
        if block.get("type").and_then(serde_json::Value::as_str) != Some("image") {
            continue;
        }
        let mime = block.get("mimeType").and_then(serde_json::Value::as_str).unwrap_or("unknown type");
        let chars = block.get("data").and_then(serde_json::Value::as_str).map_or(0, str::len);
        *block = serde_json::json!({
            "type": "text",
            "text": format!("[image omitted ({mime}, {chars} base64 chars): it is shown to the user, the model does not receive it]"),
        });
        replaced = true;
    }
    if replaced {
        Cow::Owned(format!("{result}{suffix}"))
    } else {
        Cow::Borrowed(output)
    }
}

#[cfg(test)]
pub(crate) fn test_mcp_image_result() -> (String, Vec<u8>) {
    // A valid-sized PNG signature fixture larger than the normal 16K tool
    // result limit. Pixel decoding is independently checked in the UI layer.
    let mut bytes = vec![0; 32 * 1024];
    bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
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
    fn the_model_reads_a_note_where_an_image_was_and_everything_else_as_it_came() {
        let (output, _) = test_mcp_image_result();
        let call = ToolCall { id: "c".into(), name: "mcp__server__shot".into(), arguments: serde_json::json!({}) };
        let read = without_image_data(&call, &output);
        assert!(read.len() < 400, "{} bytes", read.len());
        assert!(read.contains("image omitted (image/png") && read.contains("shown to the user"));
        assert!(!read.contains("\"image\""), "no image block is left for a preview to misread");
        // What the user is shown still comes from the untouched result.
        assert!(matches!(
            extract_tool_result_media(&call, &output),
            Some(AgentEventKind::ToolResultMedia { images, .. }) if images.len() == 1
        ));
        // Hook context appended after the JSON stays where it was.
        let hooked = format!("{output}\nHook context:\nrevisar");
        assert!(without_image_data(&call, &hooked).ends_with("\nHook context:\nrevisar"));
        // Text blocks and results without images are not rewritten.
        let text = serde_json::json!({"content":[{"type":"text","text":"una \"image\" citada"}]}).to_string();
        assert!(matches!(without_image_data(&call, &text), Cow::Borrowed(_)));
        // Another tool's output is never parsed, whatever it looks like.
        let local = ToolCall { name: "read_file".into(), ..call.clone() };
        assert!(matches!(without_image_data(&local, &output), Cow::Borrowed(_)));
        assert!(matches!(without_image_data(&call, "{\"content\":[{\"type\":\"image\"}]} trailing"), Cow::Borrowed(_)));
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
        } = extract_tool_result_media(&call(), &output).unwrap()
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
        } = extract_tool_result_media(&call(), &output).unwrap()
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
        } = extract_tool_result_media(&call(), &json!({"content":blocks}).to_string()).unwrap()
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
        } = extract_tool_result_media(&call(), &json!({"content":blocks}).to_string()).unwrap()
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
            else {
                unreachable!()
            };
            assert!(images.is_empty());
            assert_eq!(notices.len(), 1);
            validate_tool_result_media(&images, &text, &notices).unwrap();
        }
    }
}
