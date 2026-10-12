//! Bounded previews of typed MCP result blocks. These never fetch URLs or
//! reconstruct truncated JSON/base64; the original output stays in the inspector.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use openide_core::{UserImage, UserImageMime, MAX_USER_IMAGE_BYTES};
use openide_ui::{RasterImage, RasterImageFormat};
use serde::Deserialize;
use std::{borrow::Cow, cell::RefCell, collections::VecDeque, sync::Arc};

const MAX_RESULT_BYTES: usize = 12 * 1024 * 1024;
const MAX_PREVIEW_IMAGES: usize = 4;
const MAX_BLOCKS: usize = 128;
const CACHE_BUDGET: usize = 32 * 1024 * 1024;

struct CacheEntry {
    name: String,
    source: Arc<str>,
    result: Option<Arc<ToolResultMedia>>,
    bytes: usize,
}
thread_local! { static CACHE: RefCell<VecDeque<CacheEntry>> = const { RefCell::new(VecDeque::new()) }; }

#[derive(Default)]
pub(crate) struct ToolResultMedia {
    pub images: Vec<RasterImage>,
    pub text: String,
    pub notices: Vec<&'static str>,
}

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(borrow)]
    content: Blocks<'a>,
}

struct Blocks<'a> {
    values: Vec<Block<'a>>,
    limited: bool,
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
                let mut values = Vec::new();
                while values.len() < MAX_BLOCKS {
                    let Some(block) = sequence.next_element::<Block<'a>>()? else {
                        return Ok(Blocks {
                            values,
                            limited: false,
                        });
                    };
                    values.push(block);
                }
                let mut limited = false;
                while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    limited = true;
                }
                Ok(Blocks { values, limited })
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

pub(crate) fn preview(name: &str, output: &str) -> Option<Arc<ToolResultMedia>> {
    if !name.starts_with("mcp_") || !output.trim_start().starts_with('{') {
        return None;
    }
    // Oversized input gets a constant-sized notice without retaining a copy.
    if output.len() > MAX_RESULT_BYTES {
        return parse(name, output).map(Arc::new);
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache
            .iter()
            .position(|entry| entry.name == name && entry.source.as_ref() == output)
        {
            let entry = cache.remove(index).expect("known cache index");
            let result = entry.result.clone();
            cache.push_back(entry);
            return result;
        }
        let result = parse(name, output).map(Arc::new);
        let bytes = output
            .len()
            .saturating_add(result.as_ref().map_or(0, |result| {
                result
                    .images
                    .iter()
                    .map(|image| image.data_base64().len())
                    .sum::<usize>()
                    + result.text.len()
            }));
        if bytes <= CACHE_BUDGET {
            while cache.len() >= 4
                || cache
                    .iter()
                    .map(|entry| entry.bytes)
                    .sum::<usize>()
                    .saturating_add(bytes)
                    > CACHE_BUDGET
            {
                cache.pop_front();
            }
            cache.push_back(CacheEntry {
                name: name.into(),
                source: Arc::from(output),
                result: result.clone(),
                bytes,
            });
        }
        result
    })
}

fn parse(name: &str, output: &str) -> Option<ToolResultMedia> {
    if !(name.starts_with("mcp_") || name == "mcp_call") || !output.trim_start().starts_with('{') {
        return None;
    }
    if output.len() > MAX_RESULT_BYTES {
        return Some(ToolResultMedia {
            notices: vec!["El resultado supera el límite de vista previa multimedia"],
            ..Default::default()
        });
    }
    let parsed = match serde_json::from_str::<Envelope<'_>>(output) {
        Ok(parsed) => parsed,
        Err(_) if output.ends_with("[tool output truncated]") => {
            return Some(ToolResultMedia {
                notices: vec![
                    "El resultado fue truncado; la vista previa multimedia no está disponible",
                ],
                ..Default::default()
            })
        }
        Err(_) => return None,
    };
    if !parsed.content.limited
        && !parsed
            .content
            .values
            .iter()
            .any(|block| block.kind == "image")
    {
        return None;
    }
    let mut preview = ToolResultMedia::default();
    if parsed.content.limited {
        preview
            .notices
            .push("Vista previa limitada a los primeros 128 bloques del resultado");
    }
    let mut total_bytes = 0usize;
    for block in parsed.content.values {
        if block.kind == "text" {
            if let Some(text) = block.text {
                let remaining = 4000usize.saturating_sub(preview.text.chars().count());
                if remaining > 0 {
                    if !preview.text.is_empty() {
                        preview.text.push('\n');
                    }
                    let mut chars = text.chars();
                    preview.text.extend(chars.by_ref().take(remaining));
                    if chars.next().is_some() {
                        preview
                            .notices
                            .push("Texto limitado en esta vista · completo en Actividad");
                    }
                } else if !text.is_empty() {
                    preview
                        .notices
                        .push("Texto limitado en esta vista · completo en Actividad");
                }
            }
            continue;
        }
        if block.kind != "image" {
            continue;
        }
        if preview.images.len() >= MAX_PREVIEW_IMAGES {
            preview
                .notices
                .push("Se muestran las primeras 4 imágenes del resultado");
            continue;
        }
        let (mime, format) = match block.mime.as_deref() {
            Some("image/png") => (UserImageMime::Png, RasterImageFormat::Png),
            Some("image/jpeg") => (UserImageMime::Jpeg, RasterImageFormat::Jpeg),
            Some("image/webp") => (UserImageMime::Webp, RasterImageFormat::Webp),
            Some("image/gif") => (UserImageMime::Gif, RasterImageFormat::Gif),
            _ => {
                preview.notices.push("Formato de imagen no disponible");
                continue;
            }
        };
        let Some(data) = block.data else {
            preview.notices.push("La imagen no contiene datos");
            continue;
        };
        let remaining = MAX_USER_IMAGE_BYTES.saturating_sub(total_bytes);
        if data.len() > remaining.div_ceil(3) * 4 + 4 {
            preview
                .notices
                .push("Las imágenes superan el límite de vista previa de 8 MiB");
            continue;
        }
        let Some(bytes) = STANDARD
            .decode(data.as_bytes())
            .ok()
            .filter(|bytes| bytes.len() <= remaining)
        else {
            preview
                .notices
                .push("Datos de imagen incompletos o inválidos");
            continue;
        };
        let Ok(image) = UserImage::from_bytes(mime, &bytes) else {
            preview
                .notices
                .push("El contenido de la imagen no coincide con su formato");
            continue;
        };
        // The shared renderer verifies the digest, actual dimensions and its
        // 64-MiB decoder allocation budget before painting or caching pixels.
        let Some(raster) = RasterImage::new(
            image.sha256,
            format,
            image.size_bytes,
            Arc::from(image.data_base64),
            format!("Imagen {} del resultado MCP", preview.images.len() + 1),
        ) else {
            continue;
        };
        total_bytes += bytes.len();
        preview.images.push(raster);
    }
    preview.notices.sort_unstable();
    preview.notices.dedup();
    Some(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn image_data() -> String {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        STANDARD.encode(bytes.into_inner())
    }

    #[test]
    fn the_size_a_model_limit_is_checked_against_is_the_size_a_real_encoder_wrote() {
        // The harness reads the header without a decoder; this is the decoder's word.
        for (mime, format) in [
            (UserImageMime::Png, image::ImageFormat::Png),
            (UserImageMime::Jpeg, image::ImageFormat::Jpeg),
            (UserImageMime::Webp, image::ImageFormat::WebP),
            (UserImageMime::Gif, image::ImageFormat::Gif),
        ] {
            for (width, height) in [(1, 1), (37, 53), (1280, 800), (640, 4001)] {
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(width, height).write_to(&mut bytes, format).unwrap();
                let bytes = bytes.into_inner();
                assert_eq!(
                    openide_core::image_dimensions(mime, &bytes),
                    Some((width, height)),
                    "{format:?}"
                );
                assert!(UserImage::from_bytes(mime, &bytes).is_ok());
                assert_eq!(openide_core::tool_image_for_model(mime, &bytes), Ok((width, height)));
            }
        }
    }

    #[test]
    fn typed_mcp_images_reuse_raster_identity_and_keep_accompanying_text() {
        let output = json!({"content":[{"type":"text", "text":"Captura recibida"},
            {"type":"image", "mimeType":"image/png", "data":image_data()}],"isError":false})
        .to_string();
        let result = preview("mcp__openide_browser__browser_snapshot", &output).unwrap();
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].format(), RasterImageFormat::Png);
        assert_eq!(result.images[0].sha256().len(), 64);
        assert_eq!(result.text, "Captura recibida");
        assert!(result.notices.is_empty());
        assert!(preview("run_command", &output).is_none());
    }

    #[test]
    fn malformed_truncated_and_mime_mismatched_images_never_become_rasters() {
        for block in [
            json!({"type":"image","mimeType":"image/png","data":"not base64"}),
            json!({"type":"image","mimeType":"image/jpeg","data":image_data()}),
            json!({"type":"image","mimeType":"image/svg+xml","data":""}),
        ] {
            let result = preview("mcp__image", &json!({"content":[block]}).to_string()).unwrap();
            assert!(result.images.is_empty());
            assert!(!result.notices.is_empty());
        }
        let result = preview("mcp__image", "{\"content\":[\n[tool output truncated]").unwrap();
        assert!(result.images.is_empty());
        assert!(result.notices[0].contains("truncado"));
        let output = json!({"content":vec![json!({"type":"image","mimeType":"image/png","data":image_data()});6]}).to_string();
        let result = preview("mcp__image", &output).unwrap();
        assert_eq!(result.images.len(), 4);
        assert!(!result.notices.is_empty());
    }
    #[test]
    fn media_cache_and_block_budgets_preserve_notices_and_text_after_images() {
        let mut blocks =
            vec![json!({"type":"image","mimeType":"image/png","data":image_data()}); 6];
        blocks.push(json!({"type":"text","text":"after images"}));
        let output = json!({"content":blocks}).to_string();
        let first = preview("mcp__image", &output).unwrap();
        let second = preview("mcp__image", &output).unwrap();
        assert!(
            Arc::ptr_eq(&first, &second),
            "repaints reuse validated media, not base64 decode and hashing"
        );
        assert!(first.text.contains("after images"));
        let mut blocks = vec![json!({"type":"image","mimeType":"image/png","data":"invalid"}); 300];
        blocks[0] = json!({"type":"text","text":"x".repeat(5000)});
        let result = preview("mcp__image", &json!({"content":blocks}).to_string()).unwrap();
        assert!(result.images.is_empty());
        assert!(result
            .notices
            .iter()
            .any(|notice| notice.contains("128 bloques")));
        assert!(result
            .notices
            .iter()
            .any(|notice| notice.contains("Texto limitado")));
    }
}
