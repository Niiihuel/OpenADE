//! Resource limits shared by durable MCP observations and their projection.

use crate::{UserImage, UserImageError};
use thiserror::Error;

pub const MAX_TOOL_MEDIA_IMAGES: usize = 4;
pub const MAX_TOOL_MEDIA_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TOOL_MEDIA_TEXT_BYTES: usize = 4096;
pub const MAX_TOOL_MEDIA_NOTICES: usize = 8;
pub const MAX_TOOL_MEDIA_NOTICE_BYTES: usize = 256;

/// The largest tool image a model is given: 5 MB once base64 encoded, the
/// per-image limit of the strictest route (Anthropic models served through
/// Amazon Bedrock or Google Cloud; the Claude API itself takes 10 MB).
pub const MAX_TOOL_MODEL_IMAGE_BYTES: usize = 3_750_000;
/// Anthropic rejects an image wider or taller than this; no route needs more.
pub const MAX_TOOL_MODEL_IMAGE_SIDE: u32 = 8_000;

/// The pixel size of a tool image a model may be given, or why it may not.
/// The size comes from the image's own header: one that cannot be read is
/// never sent unmeasured.
pub fn tool_image_for_model(
    mime: crate::UserImageMime,
    bytes: &[u8],
) -> Result<(u32, u32), &'static str> {
    if bytes.len() > MAX_TOOL_MODEL_IMAGE_BYTES {
        return Err("larger than the 3.75 MB a model accepts for one image");
    }
    match crate::image_dimensions(mime, bytes) {
        Some((width, height)) if width.max(height) <= MAX_TOOL_MODEL_IMAGE_SIDE => {
            Ok((width, height))
        }
        Some(_) => Err("wider or taller than the 8000 px a model accepts"),
        None => Err("its pixel size could not be read"),
    }
}

/// The built-in tool that asks the user: the images attached to the answer
/// are that tool result's images.
pub const ASK_USER_TOOL: &str = "ask_user";

/// Whether a result of this tool may carry `ToolResultMedia`: the image
/// blocks of an MCP tool, or the images of the user's answer to a question.
pub fn tool_carries_media(name: &str) -> bool {
    name.starts_with("mcp__") || name == ASK_USER_TOOL
}

/// Whether these can be the images of the user's answer to a question. They
/// travel as that tool result's images, so they obey a result's budget, and
/// an image no model would be given is refused instead of recorded.
pub fn validate_answer_images(images: &[UserImage]) -> Result<(), ToolResultMediaError> {
    validate_tool_result_media(images, "", &[])?;
    for image in images {
        tool_image_for_model(image.mime_type, &image.verified_bytes()?)
            .map_err(|_| ToolResultMediaError::Limit)?;
    }
    Ok(())
}

/// The images of a `ToolResultMedia` event that its model-facing result
/// carries. None when the positions are not ascending positions of `images`.
pub fn model_tool_images(images: &[UserImage], model_images: &[u8]) -> Option<Vec<UserImage>> {
    if !model_images.windows(2).all(|pair| pair[0] < pair[1]) {
        return None;
    }
    model_images
        .iter()
        .map(|index| images.get(usize::from(*index)).cloned())
        .collect()
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ToolResultMediaError {
    #[error("tool media exceeds its image, text, or notice budget")]
    Limit,
    #[error("tool media contains an invalid image: {0}")]
    Image(#[from] UserImageError),
}

pub fn validate_tool_result_media(
    images: &[UserImage],
    text: &str,
    notices: &[String],
) -> Result<(), ToolResultMediaError> {
    if images.len() > MAX_TOOL_MEDIA_IMAGES
        || text.len() > MAX_TOOL_MEDIA_TEXT_BYTES
        || notices.len() > MAX_TOOL_MEDIA_NOTICES
        || notices
            .iter()
            .any(|notice| notice.len() > MAX_TOOL_MEDIA_NOTICE_BYTES)
        || images
            .iter()
            .try_fold(0usize, |sum, image| {
                sum.checked_add(image.size_bytes as usize)
            })
            .is_none_or(|sum| sum > MAX_TOOL_MEDIA_BYTES)
    {
        return Err(ToolResultMediaError::Limit);
    }
    for image in images {
        image.verified_bytes()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserImageMime;

    fn png(width: u32, height: u32, len: usize) -> Vec<u8> {
        let mut bytes = [
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".as_slice(),
            &width.to_be_bytes(),
            &height.to_be_bytes(),
        ]
        .concat();
        bytes.resize(len.max(bytes.len()), 0);
        bytes
    }

    #[test]
    fn a_model_is_given_only_a_measured_image_within_its_byte_and_pixel_limits() {
        let fit = |bytes: &[u8]| tool_image_for_model(UserImageMime::Png, bytes);
        assert_eq!(fit(&png(1280, 800, 0)), Ok((1280, 800)));
        assert_eq!(fit(&png(8000, 8000, MAX_TOOL_MODEL_IMAGE_BYTES)), Ok((8000, 8000)));
        assert!(fit(&png(1280, 800, MAX_TOOL_MODEL_IMAGE_BYTES + 1)).unwrap_err().contains("3.75 MB"));
        assert!(fit(&png(1280, 8001, 0)).unwrap_err().contains("8000 px"));
        assert!(fit(b"\x89PNG\r\n\x1a\nfixture").unwrap_err().contains("could not be read"));
        let image = |side| UserImage::from_bytes(UserImageMime::Png, &png(side, side, 0)).unwrap();
        let images = [image(1), image(2)];
        assert_eq!(model_tool_images(&images, &[1]), Some(vec![images[1].clone()]));
        assert_eq!(model_tool_images(&images, &[]), Some(vec![]));
        assert_eq!(model_tool_images(&images, &[1, 0]), None);
        assert_eq!(model_tool_images(&images, &[0, 0]), None);
        assert_eq!(model_tool_images(&images, &[2]), None);
        // The limit is what base64 turns into 5 MB.
        assert_eq!(MAX_TOOL_MODEL_IMAGE_BYTES.div_ceil(3) * 4, 5_000_000);
    }

    #[test]
    fn answer_images_obey_a_results_budget_and_what_a_model_accepts() {
        let image = |width, height, len| {
            UserImage::from_bytes(UserImageMime::Png, &png(width, height, len)).unwrap()
        };
        assert!(validate_answer_images(&[]).is_ok());
        assert!(validate_answer_images(&vec![image(64, 64, 0); MAX_TOOL_MEDIA_IMAGES]).is_ok());
        assert_eq!(
            validate_answer_images(&vec![image(64, 64, 0); MAX_TOOL_MEDIA_IMAGES + 1]),
            Err(ToolResultMediaError::Limit)
        );
        // Each within what a model accepts, together over a result's 8 MiB.
        assert_eq!(
            validate_answer_images(&vec![image(64, 64, MAX_TOOL_MODEL_IMAGE_BYTES); 3]),
            Err(ToolResultMediaError::Limit)
        );
        for refused in [
            image(64, 64, MAX_TOOL_MODEL_IMAGE_BYTES + 1),
            image(64, MAX_TOOL_MODEL_IMAGE_SIDE + 1, 0),
            UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nfixture").unwrap(),
        ] {
            assert_eq!(validate_answer_images(&[refused]), Err(ToolResultMediaError::Limit));
        }
        let mut tampered = image(64, 64, 0);
        tampered.sha256 = "0".repeat(64);
        assert!(matches!(
            validate_answer_images(&[tampered]),
            Err(ToolResultMediaError::Image(_))
        ));
        assert!(tool_carries_media("mcp__server__shot") && tool_carries_media(ASK_USER_TOOL));
        assert!(!tool_carries_media("read_file") && !tool_carries_media("switch_mode"));
    }

    #[test]
    fn tool_media_validates_integrity_and_all_presentation_budgets() {
        let image = UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        assert!(validate_tool_result_media(std::slice::from_ref(&image), "caption", &[]).is_ok());
        assert!(validate_tool_result_media(&vec![image.clone(); 5], "", &[]).is_err());
        assert!(validate_tool_result_media(&[], &"🦀".repeat(1025), &[]).is_err());
        assert!(validate_tool_result_media(&[], "", &vec![String::new(); 9]).is_err());
        assert!(validate_tool_result_media(&[], "", &["x".repeat(257)]).is_err());
        let mut tampered = image;
        tampered.sha256 = "0".repeat(64);
        assert!(matches!(
            validate_tool_result_media(&[tampered], "", &[]),
            Err(ToolResultMediaError::Image(_))
        ));
        let mut bytes = vec![0; MAX_TOOL_MEDIA_BYTES / 2 + 1];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let image = UserImage::from_bytes(UserImageMime::Png, &bytes).unwrap();
        assert_eq!(
            validate_tool_result_media(&[image.clone(), image], "", &[]),
            Err(ToolResultMediaError::Limit)
        );
    }
}
