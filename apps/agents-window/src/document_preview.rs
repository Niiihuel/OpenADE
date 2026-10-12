//! Document presentations share editor buffers without owning their lifetime.
use std::sync::Arc;

use openide_preview::{Document, PreviewKind, Scope};
use openide_ui::RasterImage;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PreviewMode {
    #[default]
    Source,
    Preview,
    Split,
}

#[derive(Clone, Debug)]
pub(crate) struct PreviewView {
    pub scope: Scope,
    pub kind: PreviewKind,
    pub mode: PreviewMode,
    pub document: Option<Arc<Document>>,
    pub raster: Option<RasterImage>,
    pub loading: bool,
    pub error: Option<String>,
    pub page: usize,
    pub pages: usize,
    /// Of a PDF's page: None fits to the viewport, Some is a scale relative
    /// to rendered pixels. An image always fits.
    pub zoom: Option<f32>,
}

impl PreviewView {
    /// Controls survive asynchronous document refreshes and pointer gestures.
    pub fn chrome_action_prefix(&self) -> String {
        format!("document_preview_view:{}", self.scope.tab_id)
    }

    pub fn action_prefix(&self) -> String {
        format!(
            "document_preview:{}:{}",
            self.scope.tab_id, self.scope.generation
        )
    }

    pub fn scroll_id(&self) -> String {
        format!("document_preview_scroll:{}", self.scope.tab_id)
    }

    pub fn is_source_visible(&self) -> bool {
        self.kind == PreviewKind::Markdown && self.mode != PreviewMode::Preview
    }
}

/// A secondary editor group has its own tab identity and can be closed without
/// closing, saving or reloading the source document.
#[derive(Clone, Debug)]
pub(crate) struct MarkdownPreviewTab {
    pub id: u64,
    pub source_tab_id: u64,
    pub preview: PreviewView,
}

impl MarkdownPreviewTab {
    pub fn action_prefix(&self) -> String {
        format!("markdown_preview:{}", self.id)
    }

    pub fn document_action_prefix(&self) -> String {
        format!(
            "{}:document:{}",
            self.action_prefix(),
            self.preview.scope.generation
        )
    }

    pub fn scroll_id(&self) -> String {
        format!("markdown_preview_scroll:{}", self.id)
    }
}
