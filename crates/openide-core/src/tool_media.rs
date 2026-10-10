//! Resource limits shared by durable MCP observations and their projection.

use crate::{UserImage, UserImageError};
use thiserror::Error;

pub const MAX_TOOL_MEDIA_IMAGES: usize = 4;
pub const MAX_TOOL_MEDIA_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TOOL_MEDIA_TEXT_BYTES: usize = 4096;
pub const MAX_TOOL_MEDIA_NOTICES: usize = 8;
pub const MAX_TOOL_MEDIA_NOTICE_BYTES: usize = 256;

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
