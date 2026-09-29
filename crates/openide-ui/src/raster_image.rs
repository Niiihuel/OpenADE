use std::sync::Arc;

/// A bounded encoded image which the host paints after validating and decoding.
/// The UI tree keeps its journal payload; no machine-local path is needed.
#[derive(Clone, PartialEq, Eq)]
pub struct RasterImage {
    sha256: String,
    format: RasterImageFormat,
    size_bytes: u32,
    data_base64: Arc<str>,
    alt: String,
}

impl std::fmt::Debug for RasterImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RasterImage")
            .field("sha256", &self.sha256)
            .field("format", &self.format)
            .field("size_bytes", &self.size_bytes)
            .field("alt", &self.alt)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RasterImageFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl RasterImage {
    /// The host must recompute the digest and inspect the format before decoding.
    /// Construction alone does not attest to the bytes.
    pub fn new(
        sha256: impl Into<String>,
        format: RasterImageFormat,
        size_bytes: u32,
        data_base64: Arc<str>,
        alt: impl Into<String>,
    ) -> Option<Self> {
        let sha256 = sha256.into();
        let alt = alt.into();
        if size_bytes == 0
            || size_bytes as usize > 8 * 1024 * 1024
            || data_base64.is_empty()
            || data_base64.len() > (8 * 1024 * 1024_usize).div_ceil(3) * 4 + 4
            || sha256.len() != 64
            || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || alt.is_empty()
            || alt.len() > 256
        {
            return None;
        }
        Some(Self {
            sha256,
            format,
            size_bytes,
            data_base64,
            alt,
        })
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn format(&self) -> RasterImageFormat {
        self.format
    }

    pub fn size_bytes(&self) -> u32 {
        self.size_bytes
    }

    pub fn data_base64(&self) -> &str {
        &self.data_base64
    }

    pub fn alt(&self) -> &str {
        &self.alt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, Element, Size, UiTree};

    #[test]
    fn raster_image_layout_keeps_alt_and_bounded_payload() {
        let image = RasterImage::new(
            "a".repeat(64),
            RasterImageFormat::Png,
            3,
            Arc::from("cG5n"),
            "Imagen adjunta 1 de 1",
        )
        .unwrap();
        let frame = UiTree::new(Element::column(vec![Element::raster_image(image.clone())]))
            .layout(Size::new(180.0, 220.0));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::RasterImage { rect, image: painted, .. }
                if painted == &image && painted.alt() == "Imagen adjunta 1 de 1"
                    && rect.width <= 180.0 && rect.height <= 180.0
        )));
        assert!(RasterImage::new(
            "a".repeat(64),
            RasterImageFormat::Png,
            8 * 1024 * 1024 + 1,
            Arc::from("cG5n"),
            "Imagen",
        )
        .is_none());
    }
}
