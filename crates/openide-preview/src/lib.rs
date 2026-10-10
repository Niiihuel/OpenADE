//! Native document previews shared by the IDE and agent workspace.
//!
//! The host supplies workspace-scoped requests and displays the resulting
//! native Markdown tree or PNG pixels. Parsing and rasterization never happen
//! on the UI thread. This crate performs no network requests or shell commands.

pub mod markdown;
mod media;
mod path;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::Arc;
pub use worker::Worker;

/// Supported file presentations. Markdown retains an editable source buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Markdown,
    Image,
    Pdf,
}

pub fn kind_for_path(path: &Path) -> Option<PreviewKind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "md" | "markdown" | "mdown" | "mkd" => Some(PreviewKind::Markdown),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" => Some(PreviewKind::Image),
        "pdf" => Some(PreviewKind::Pdf),
        _ => None,
    }
}

/// The host must match the entire scope before applying a completed response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub workspace: PathBuf,
    pub path: PathBuf,
    pub tab_id: u64,
    pub generation: u64,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub scope: Scope,
    /// Current Markdown buffer, including unsaved changes. Other formats use
    /// the scoped file on disk; no arbitrary byte source is accepted for them.
    pub source: Option<String>,
    /// Zero-based page index, used only for PDF.
    pub page: usize,
    pub render_width: u32,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub scope: Scope,
    pub result: Result<Document, String>,
}

#[derive(Clone, Debug)]
pub enum Document {
    Markdown(markdown::MarkdownDocument),
    Image(PreviewImage),
    Pdf {
        image: PreviewImage,
        page: usize,
        pages: usize,
    },
}

/// A bounded raster ready for the host's existing native image component.
#[derive(Clone, Debug)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub png: Arc<[u8]>,
}

pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_IMAGE_SIDE: u32 = 4096;
pub const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
/// Raster output fits the native host's full-resolution image limits.
pub const MAX_RENDER_PIXELS: u64 = 4 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_types_are_case_insensitive_without_claiming_unknown_formats() {
        assert_eq!(
            kind_for_path(Path::new("readme.MD")),
            Some(PreviewKind::Markdown)
        );
        assert_eq!(
            kind_for_path(Path::new("diagram.SVG")),
            Some(PreviewKind::Image)
        );
        assert_eq!(
            kind_for_path(Path::new("report.PDF")),
            Some(PreviewKind::Pdf)
        );
        assert_eq!(kind_for_path(Path::new("main.rs")), None);
    }
}
