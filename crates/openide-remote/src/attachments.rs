//! Private, immutable, device/project-scoped uploads. No workspace paths are accepted.
use crate::{
    host::{ApiError, ApiResult},
    storage,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use openide_core::{validate_user_images, UserImage, UserImageMime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_TEXT_CONTEXT_BYTES: usize = 1024 * 1024;
pub const MAX_ATTACHMENTS: usize = 8;
pub const MAX_UPLOAD_BODY_BYTES: usize = 6 * 1024 * 1024;
const MAX_SCOPE_BYTES: u64 = 96 * 1024 * 1024;
const MAX_SCOPE_RECORDS: usize = 128;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UploadAttachment {
    pub request_id: String,
    pub filename: String,
    pub media_type: String,
    pub data_base64: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentSummary {
    pub id: String,
    pub filename: String,
    pub media_type: String,
    pub size_bytes: usize,
    pub sha256: String,
}

#[derive(Serialize, Deserialize)]
struct StoredAttachment {
    version: u32,
    device_id: String,
    project_id: String,
    request_id: String,
    summary: AttachmentSummary,
    data_base64: String,
}

#[derive(Default)]
pub struct PreparedAttachments {
    pub text_context: String,
    pub images: Vec<UserImage>,
}

pub struct AttachmentStore {
    root: PathBuf,
}

fn internal(_: impl std::fmt::Display) -> ApiError {
    ApiError::unavailable()
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl AttachmentStore {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.join("attachments"),
        }
    }

    fn scope(&self, device_id: &str, project_id: &str) -> ApiResult<PathBuf> {
        storage::validate_id(device_id).map_err(|_| ApiError::bad("Invalid device identifier"))?;
        storage::validate_id(project_id)
            .map_err(|_| ApiError::bad("Invalid project identifier"))?;
        storage::prepare_directory(&self.root).map_err(internal)?;
        let device = self.root.join(hash(device_id.as_bytes()));
        storage::prepare_directory(&device).map_err(internal)?;
        let project = device.join(hash(project_id.as_bytes()));
        storage::prepare_directory(&project).map_err(internal)?;
        Ok(project)
    }

    pub fn upload(
        &self,
        device_id: &str,
        project_id: &str,
        request: UploadAttachment,
        allow_images: bool,
    ) -> ApiResult<AttachmentSummary> {
        storage::validate_id(&request.request_id)
            .map_err(|_| ApiError::bad("Invalid request identifier"))?;
        validate_filename(&request.filename)?;
        let bytes = validate_data(
            &request.media_type,
            &request.data_base64,
            &request.sha256,
            allow_images,
        )?;
        let scope = self.scope(device_id, project_id)?;
        let _lock = storage::lock_file(&scope.join("uploads.lock")).map_err(internal)?;
        // A stable opaque identifier also makes a crash after atomic publication retry-safe.
        let id = hash(
            &serde_json::to_vec(&(device_id, project_id, &request.request_id)).map_err(internal)?,
        );
        let summary = AttachmentSummary {
            id: id.clone(),
            filename: request.filename,
            media_type: request.media_type,
            size_bytes: bytes.len(),
            sha256: request.sha256,
        };
        let path = scope.join(format!("{id}.json"));
        if path.exists() {
            let prior = read_record(&path)?;
            if prior.device_id != device_id
                || prior.project_id != project_id
                || prior.request_id != request.request_id
                || prior.summary != summary
                || prior.data_base64 != request.data_base64
            {
                return Err(ApiError::idempotency_conflict());
            }
            return Ok(prior.summary);
        }
        let encoded_size = request.data_base64.len() as u64;
        let mut count = 0;
        let mut total = 0u64;
        for entry in fs::read_dir(&scope).map_err(internal)? {
            let entry = entry.map_err(internal)?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let metadata = entry.path().symlink_metadata().map_err(internal)?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(ApiError::unavailable());
                }
                count += 1;
                total = total.saturating_add(metadata.len());
            }
        }
        if count >= MAX_SCOPE_RECORDS || total.saturating_add(encoded_size + 2048) > MAX_SCOPE_BYTES
        {
            return Err(ApiError::bad(
                "Attachment storage quota reached for this device and project",
            ));
        }
        storage::write_json(
            &path,
            &StoredAttachment {
                version: 1,
                device_id: device_id.into(),
                project_id: project_id.into(),
                request_id: request.request_id,
                summary: summary.clone(),
                data_base64: request.data_base64,
            },
        )
        .map_err(internal)?;
        Ok(summary)
    }

    pub fn resolve(
        &self,
        device_id: &str,
        project_id: &str,
        ids: &[String],
        allow_images: bool,
    ) -> ApiResult<PreparedAttachments> {
        if ids.len() > MAX_ATTACHMENTS {
            return Err(ApiError::bad("At most eight attachments are allowed"));
        }
        let mut seen = HashSet::new();
        let mut prepared = PreparedAttachments::default();
        if ids.is_empty() {
            return Ok(prepared);
        }
        let scope = self.scope(device_id, project_id)?;
        for id in ids {
            if id.len() != 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                || !seen.insert(id)
            {
                return Err(ApiError::bad("Invalid or duplicate attachment identifier"));
            }
            let path = scope.join(format!("{id}.json"));
            if !path.exists() {
                return Err(ApiError::not_found());
            }
            let stored = read_record(&path)?;
            if stored.version != 1
                || stored.device_id != device_id
                || stored.project_id != project_id
                || stored.summary.id != *id
            {
                return Err(ApiError::not_found());
            }
            validate_filename(&stored.summary.filename)?;
            let bytes = validate_data(
                &stored.summary.media_type,
                &stored.data_base64,
                &stored.summary.sha256,
                allow_images,
            )?;
            if bytes.len() != stored.summary.size_bytes {
                return Err(ApiError::unavailable());
            }
            if stored.summary.media_type == "text/plain" {
                let text = String::from_utf8(bytes).map_err(internal)?;
                let addition = format!(
                    "\n\n--- Attached text: {} ---\n{}\n--- End attached text ---",
                    stored.summary.filename, text
                );
                if prepared.text_context.len() + addition.len() > MAX_TEXT_CONTEXT_BYTES {
                    return Err(ApiError::bad("Combined text attachments exceed 1 MiB"));
                }
                prepared.text_context.push_str(&addition);
            } else {
                prepared.images.push(
                    UserImage::from_bytes(image_mime(&stored.summary.media_type)?, &bytes)
                        .map_err(|e| ApiError::bad(e.to_string()))?,
                );
            }
        }
        validate_user_images(&prepared.images).map_err(|e| ApiError::bad(e.to_string()))?;
        Ok(prepared)
    }
}

fn read_record(path: &Path) -> ApiResult<StoredAttachment> {
    let bytes = storage::read_private(path, MAX_UPLOAD_BODY_BYTES as u64).map_err(internal)?;
    serde_json::from_slice(&bytes).map_err(internal)
}

fn validate_filename(filename: &str) -> ApiResult<()> {
    if filename.is_empty()
        || filename.len() > 255
        || filename == "."
        || filename == ".."
        || filename
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(ApiError::bad(
            "Attachment filename must be a simple name without a path",
        ));
    }
    Ok(())
}

fn image_mime(media_type: &str) -> ApiResult<UserImageMime> {
    match media_type {
        "image/png" => Ok(UserImageMime::Png),
        "image/jpeg" => Ok(UserImageMime::Jpeg),
        "image/webp" => Ok(UserImageMime::Webp),
        "image/gif" => Ok(UserImageMime::Gif),
        _ => Err(ApiError::bad(
            "Only UTF-8 text and supported images can be attached",
        )),
    }
}

fn validate_data(
    media_type: &str,
    encoded: &str,
    sha256: &str,
    allow_images: bool,
) -> ApiResult<Vec<u8>> {
    let max = if media_type == "text/plain" {
        MAX_TEXT_BYTES
    } else {
        if !allow_images {
            return Err(ApiError::bad(
                "Image input is not enabled for this exact model",
            ));
        }
        image_mime(media_type)?;
        MAX_IMAGE_BYTES
    };
    if encoded.len() > max.div_ceil(3) * 4 || sha256.len() != 64 {
        return Err(ApiError::bad(
            "Attachment exceeds its size limit or has an invalid digest",
        ));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::bad("Invalid attachment base64"))?;
    if bytes.is_empty() || bytes.len() > max || STANDARD.encode(&bytes) != encoded {
        return Err(ApiError::bad(
            "Invalid attachment size or noncanonical base64",
        ));
    }
    if hash(&bytes) != sha256 {
        return Err(ApiError::bad("Attachment SHA-256 mismatch"));
    }
    if media_type == "text/plain" {
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ApiError::bad("Text attachment must be valid UTF-8"))?;
        if text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
        {
            return Err(ApiError::bad(
                "Text attachment contains binary control characters",
            ));
        }
    } else {
        UserImage::from_bytes(image_mime(media_type)?, &bytes)
            .map_err(|e| ApiError::bad(e.to_string()))?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn upload(id: &str, name: &str, media: &str, bytes: &[u8]) -> UploadAttachment {
        UploadAttachment {
            request_id: id.into(),
            filename: name.into(),
            media_type: media.into(),
            data_base64: STANDARD.encode(bytes),
            sha256: hash(bytes),
        }
    }

    #[test]
    fn retries_are_immutable_and_scoped_to_device_and_project() {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path());
        let first = store
            .upload(
                "phone",
                "project",
                upload("req", "notes.txt", "text/plain", b"hello"),
                false,
            )
            .unwrap();
        assert_eq!(
            first,
            store
                .upload(
                    "phone",
                    "project",
                    upload("req", "notes.txt", "text/plain", b"hello"),
                    false
                )
                .unwrap()
        );
        assert_eq!(
            store
                .upload(
                    "phone",
                    "project",
                    upload("req", "notes.txt", "text/plain", b"changed"),
                    false
                )
                .unwrap_err()
                .code,
            "idempotency_conflict"
        );
        assert!(store
            .resolve(
                "other-phone",
                "project",
                std::slice::from_ref(&first.id),
                false
            )
            .is_err());
        assert!(store
            .resolve(
                "phone",
                "other-project",
                std::slice::from_ref(&first.id),
                false
            )
            .is_err());
        assert!(store
            .resolve("phone", "project", &[first.id], false)
            .unwrap()
            .text_context
            .contains("hello"));
    }

    #[test]
    fn malformed_binary_oversized_and_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path());
        for name in ["../secret", "a/b", "a\\b", "bad\nname", ".", ".."] {
            assert!(store
                .upload(
                    "phone",
                    "p",
                    upload("r", name, "text/plain", b"text"),
                    false
                )
                .is_err());
        }
        for bytes in [b"a\0b".as_slice(), &[0xff, 0xfe], &[0x1b, b'[']] {
            assert!(store
                .upload(
                    "phone",
                    "p",
                    upload("r", "a.txt", "text/plain", bytes),
                    false
                )
                .is_err());
        }
        assert!(store
            .upload(
                "phone",
                "p",
                upload("r", "a.txt", "text/plain", &vec![b'x'; MAX_TEXT_BYTES + 1]),
                false
            )
            .is_err());
        let mut bad = upload("r", "a.txt", "text/plain", b"text");
        bad.sha256 = "0".repeat(64);
        assert!(store.upload("phone", "p", bad, false).is_err());
        assert!(store
            .resolve("phone", "p", &["../outside".into()], false)
            .is_err());
    }

    #[test]
    fn image_capability_and_core_format_validation_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path());
        let png = b"\x89PNG\r\n\x1a\nfixture";
        assert!(store
            .upload("phone", "p", upload("r", "a.png", "image/png", png), false)
            .is_err());
        assert!(store
            .upload("phone", "p", upload("r", "a.jpg", "image/jpeg", png), true)
            .is_err());
        let accepted = store
            .upload("phone", "p", upload("r", "a.png", "image/png", png), true)
            .unwrap();
        let prepared = store
            .resolve("phone", "p", std::slice::from_ref(&accepted.id), true)
            .unwrap();
        assert_eq!(prepared.images[0].verified_bytes().unwrap(), png);
        assert!(store.resolve("phone", "p", &[accepted.id], false).is_err());
    }
}
