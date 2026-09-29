//! Bounded, self-contained user images for durable native turns.
//!
//! Bytes live in the journal rather than in machine-local paths. A replayed
//! message therefore remains usable after a restart or workspace move.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_USER_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_USER_IMAGES_PER_MESSAGE: usize = 8;
pub const MAX_USER_IMAGE_MESSAGE_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UserImageMime {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/webp")]
    Webp,
    #[serde(rename = "image/gif")]
    Gif,
}

impl UserImageMime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }

    fn verify_format(self, bytes: &[u8]) -> Result<(), UserImageError> {
        match self {
            Self::Png if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Ok(()),
            Self::Jpeg if bytes.starts_with(b"\xff\xd8\xff") => Ok(()),
            Self::Webp
                if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" =>
            {
                Ok(())
            }
            Self::Gif => verify_static_gif(bytes),
            _ => Err(UserImageError::MimeMismatch),
        }
    }
}

/// Walk GIF blocks rather than searching for image markers in compressed
/// bytes. The Responses image input accepts only a single-frame GIF.
fn verify_static_gif(bytes: &[u8]) -> Result<(), UserImageError> {
    if bytes.len() < 13 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Err(UserImageError::MimeMismatch);
    }
    let mut at = 13usize;
    let global_packed = bytes[10];
    if global_packed & 0x80 != 0 {
        let colors = 1usize << (usize::from(global_packed & 0x07) + 1);
        at = at
            .checked_add(colors * 3)
            .filter(|end| *end <= bytes.len())
            .ok_or(UserImageError::MimeMismatch)?;
    }
    let mut frames = 0;
    loop {
        let tag = *bytes.get(at).ok_or(UserImageError::MimeMismatch)?;
        at += 1;
        match tag {
            0x2c => {
                let descriptor = bytes.get(at..at + 9).ok_or(UserImageError::MimeMismatch)?;
                let local_packed = descriptor[8];
                at += 9;
                if local_packed & 0x80 != 0 {
                    let colors = 1usize << (usize::from(local_packed & 0x07) + 1);
                    at = at
                        .checked_add(colors * 3)
                        .filter(|end| *end <= bytes.len())
                        .ok_or(UserImageError::MimeMismatch)?;
                }
                // LZW minimum code size precedes image-data subblocks.
                at = at
                    .checked_add(1)
                    .filter(|end| *end <= bytes.len())
                    .ok_or(UserImageError::MimeMismatch)?;
                at = skip_gif_subblocks(bytes, at)?;
                frames += 1;
                if frames > 1 {
                    return Err(UserImageError::AnimatedGif);
                }
            }
            0x21 => {
                at = at
                    .checked_add(1)
                    .filter(|end| *end <= bytes.len())
                    .ok_or(UserImageError::MimeMismatch)?;
                at = skip_gif_subblocks(bytes, at)?;
            }
            0x3b if frames == 1 && at == bytes.len() => return Ok(()),
            _ => return Err(UserImageError::MimeMismatch),
        }
    }
}

fn skip_gif_subblocks(bytes: &[u8], mut at: usize) -> Result<usize, UserImageError> {
    loop {
        let size = usize::from(*bytes.get(at).ok_or(UserImageError::MimeMismatch)?);
        at += 1;
        if size == 0 {
            return Ok(at);
        }
        at = at
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or(UserImageError::MimeMismatch)?;
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserImage {
    pub mime_type: UserImageMime,
    pub size_bytes: u32,
    pub sha256: String,
    pub data_base64: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum UserImageError {
    #[error("user image is empty or exceeds 8 MiB")]
    InvalidSize,
    #[error("user image MIME does not match its byte signature")]
    MimeMismatch,
    #[error("animated GIF images are not supported")]
    AnimatedGif,
    #[error("user image base64 is invalid or noncanonical")]
    InvalidBase64,
    #[error("user image SHA-256 does not match its bytes")]
    HashMismatch,
    #[error("user image message exceeds its image count or total byte limit")]
    MessageLimit,
}

impl UserImage {
    pub fn from_bytes(mime_type: UserImageMime, bytes: &[u8]) -> Result<Self, UserImageError> {
        if bytes.is_empty() || bytes.len() > MAX_USER_IMAGE_BYTES {
            return Err(UserImageError::InvalidSize);
        }
        mime_type.verify_format(bytes)?;
        Ok(Self {
            mime_type,
            size_bytes: bytes.len() as u32,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            data_base64: STANDARD.encode(bytes),
        })
    }

    pub fn verified_bytes(&self) -> Result<Vec<u8>, UserImageError> {
        let size = self.size_bytes as usize;
        if size == 0 || size > MAX_USER_IMAGE_BYTES {
            return Err(UserImageError::InvalidSize);
        }
        if self.data_base64.len() > MAX_USER_IMAGE_BYTES.div_ceil(3) * 4 + 4 {
            return Err(UserImageError::InvalidSize);
        }
        let bytes = STANDARD
            .decode(&self.data_base64)
            .map_err(|_| UserImageError::InvalidBase64)?;
        if bytes.len() != size {
            return Err(UserImageError::InvalidSize);
        }
        if STANDARD.encode(&bytes) != self.data_base64 {
            return Err(UserImageError::InvalidBase64);
        }
        self.mime_type.verify_format(&bytes)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if self.sha256 != digest {
            return Err(UserImageError::HashMismatch);
        }
        Ok(bytes)
    }

    pub fn verified_data_url(&self) -> Result<String, UserImageError> {
        self.verified_bytes()?;
        Ok(format!(
            "data:{};base64,{}",
            self.mime_type.as_str(),
            self.data_base64
        ))
    }
}

pub fn validate_user_images(images: &[UserImage]) -> Result<(), UserImageError> {
    if images.len() > MAX_USER_IMAGES_PER_MESSAGE {
        return Err(UserImageError::MessageLimit);
    }
    let mut total = 0usize;
    for image in images {
        total = total
            .checked_add(image.size_bytes as usize)
            .ok_or(UserImageError::MessageLimit)?;
        if total > MAX_USER_IMAGE_MESSAGE_BYTES {
            return Err(UserImageError::MessageLimit);
        }
        image.verified_bytes()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_metadata_verifies_data_and_mime() {
        let bytes = b"\x89PNG\r\n\x1a\nsmall fixture";
        let image = UserImage::from_bytes(UserImageMime::Png, bytes).unwrap();
        assert_eq!(image.verified_bytes().unwrap(), bytes);
        assert!(image
            .verified_data_url()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert_eq!(
            UserImage::from_bytes(UserImageMime::Jpeg, bytes),
            Err(UserImageError::MimeMismatch)
        );
        let mut tampered = image.clone();
        tampered.sha256 = "0".repeat(64);
        assert_eq!(tampered.verified_bytes(), Err(UserImageError::HashMismatch));
        tampered = image;
        tampered.data_base64.push(' ');
        assert_eq!(
            tampered.verified_bytes(),
            Err(UserImageError::InvalidBase64)
        );
    }

    #[test]
    fn gif_parser_rejects_animation_and_truncated_blocks() {
        let prefix = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff";
        let frame = b"\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x4c\x01\x00";
        let mut static_gif = prefix.to_vec();
        static_gif.extend_from_slice(frame);
        static_gif.push(0x3b);
        assert!(UserImage::from_bytes(UserImageMime::Gif, &static_gif).is_ok());
        let mut animated = prefix.to_vec();
        animated.extend_from_slice(frame);
        animated.extend_from_slice(frame);
        animated.push(0x3b);
        assert_eq!(
            UserImage::from_bytes(UserImageMime::Gif, &animated),
            Err(UserImageError::AnimatedGif)
        );
        assert_eq!(
            UserImage::from_bytes(UserImageMime::Gif, &static_gif[..static_gif.len() - 2]),
            Err(UserImageError::MimeMismatch)
        );
    }

    #[test]
    fn image_count_and_size_limits_are_enforced_before_encoding() {
        let image = UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nsmall").unwrap();
        assert_eq!(
            validate_user_images(&vec![image; MAX_USER_IMAGES_PER_MESSAGE + 1]),
            Err(UserImageError::MessageLimit)
        );
        let mut large = vec![0; MAX_USER_IMAGE_BYTES + 1];
        large[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            UserImage::from_bytes(UserImageMime::Png, &large),
            Err(UserImageError::InvalidSize)
        );
    }
}
