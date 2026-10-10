//! CPU raster backends. All filesystem and decoding work is confined to the
//! worker; only bounded PNG images cross the native UI boundary.
use crate::path::{self, FileIdentity};
use crate::{
    Document, PreviewImage, Request, MAX_IMAGE_PIXELS, MAX_IMAGE_SIDE, MAX_PNG_BYTES,
    MAX_RENDER_PIXELS,
};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use image::{ImageEncoder, ImageFormat};
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::Arc;

const MAX_PDF_RENDER_WIDTH: u32 = 1600;
const MAX_PDF_PAGES: usize = 20_000;
const MAX_SVG_BYTES: usize = 8 * 1024 * 1024;

struct CachedPdf {
    identity: FileIdentity,
    document: Pdf,
}

#[derive(Default)]
pub(crate) struct Media {
    pdf: Option<CachedPdf>,
    svg_fonts: Option<Arc<resvg::usvg::fontdb::Database>>,
}

impl Media {
    pub fn pdf(&mut self, request: &Request) -> Result<Document, String> {
        let file = path::open(&request.scope.workspace, &request.scope.path)?;
        if !self
            .pdf
            .as_ref()
            .is_some_and(|cache| cache.identity == file.identity)
        {
            self.pdf = None;
            let identity = file.identity.clone();
            let bytes = file.read()?;
            let document = Pdf::new(bytes).map_err(|error| match error {
                LoadPdfError::Decryption(_) => {
                    "Este PDF requiere una contraseña o un cifrado compatible.".to_owned()
                }
                LoadPdfError::Invalid => "El PDF está dañado o no se pudo interpretar.".to_owned(),
            })?;
            if document.pages().is_empty() || document.pages().len() > MAX_PDF_PAGES {
                return Err("El PDF no tiene páginas o supera el límite de 20 000 páginas.".into());
            }
            self.pdf = Some(CachedPdf { identity, document });
        }
        let pdf = &self.pdf.as_ref().unwrap().document;
        let pages = pdf.pages().len();
        let page = pdf
            .pages()
            .get(request.page)
            .ok_or("La página solicitada no existe.")?;
        let (page_width, page_height) = page.render_dimensions();
        let (width, height, scale) = fit_dimensions(
            page_width as f64,
            page_height as f64,
            request.render_width.clamp(64, MAX_PDF_RENDER_WIDTH),
            true,
        )?;
        let settings = hayro::RenderSettings {
            x_scale: scale,
            y_scale: scale,
            width: Some(width as u16),
            height: Some(height as u16),
            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
        };
        // This cache borrows its PDF and uses Rc internally. Construct it on
        // this worker and retain the parsed document across page requests.
        let cache = hayro::RenderCache::new();
        let pixmap = hayro::render(page, &cache, &InterpreterSettings::default(), &settings);
        let png = pixmap
            .into_png()
            .map_err(|e| format!("No se pudo rasterizar el PDF: {e}"))?;
        let image = preview_image(width, height, png)?;
        Ok(Document::Pdf {
            image,
            page: request.page,
            pages,
        })
    }

    pub fn image(
        &mut self,
        workspace: &Path,
        relative: &Path,
        width: u32,
    ) -> Result<PreviewImage, String> {
        let bytes = path::open(workspace, relative)?.read()?;
        self.image_bytes(&bytes, relative, width)
    }

    fn image_bytes(
        &mut self,
        bytes: &[u8],
        path: &Path,
        width: u32,
    ) -> Result<PreviewImage, String> {
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
        {
            self.svg(bytes, width)
        } else {
            raster(bytes, width)
        }
    }

    fn svg(&mut self, bytes: &[u8], requested_width: u32) -> Result<PreviewImage, String> {
        if bytes.len() > MAX_SVG_BYTES {
            return Err("El SVG supera el límite de 8 MiB.".into());
        }
        // Parse only the already-bounded UTF-8 source. `from_data` can also
        // decompress SVGZ when another crate enables that feature on usvg.
        let source = std::str::from_utf8(bytes)
            .map_err(|_| "El SVG debe contener texto UTF-8 sin comprimir.".to_owned())?;
        let fonts = self
            .svg_fonts
            .get_or_insert_with(|| {
                let mut fonts = resvg::usvg::fontdb::Database::new();
                fonts.load_system_fonts();
                Arc::new(fonts)
            })
            .clone();
        let options = resvg::usvg::Options {
            fontdb: fonts,
            font_family: "sans-serif".into(),
            // usvg's default resolver can read arbitrary local files. The
            // document preview accepts only bounded embedded raster images.
            image_href_resolver: resvg::usvg::ImageHrefResolver {
                resolve_string: Box::new(|_, _| None),
                resolve_data: Box::new(|_, bytes, _| {
                    if bytes.len() > MAX_PNG_BYTES {
                        return None;
                    }
                    let format = supported_format(&bytes).ok()?;
                    raster_dimensions(&bytes, format).ok()?;
                    match format {
                        ImageFormat::Png => Some(resvg::usvg::ImageKind::PNG(bytes)),
                        ImageFormat::Jpeg => Some(resvg::usvg::ImageKind::JPEG(bytes)),
                        ImageFormat::WebP => Some(resvg::usvg::ImageKind::WEBP(bytes)),
                        ImageFormat::Gif => Some(resvg::usvg::ImageKind::GIF(bytes)),
                        _ => None,
                    }
                }),
            },
            ..Default::default()
        };
        let tree = resvg::usvg::Tree::from_str(source, &options)
            .map_err(|e| format!("No se pudo interpretar el SVG: {e}"))?;
        let size = tree.size();
        let (width, height, scale) = fit_dimensions(
            size.width() as f64,
            size.height() as f64,
            requested_width.clamp(32, MAX_IMAGE_SIDE),
            true,
        )?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
            .ok_or("No hay memoria para la imagen SVG.")?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        let png = pixmap
            .encode_png()
            .map_err(|e| format!("No se pudo rasterizar el SVG: {e}"))?;
        preview_image(width, height, png)
    }
}

fn supported_format(bytes: &[u8]) -> Result<ImageFormat, String> {
    let format = image::guess_format(bytes)
        .map_err(|_| "La imagen está dañada o tiene un formato no compatible.".to_owned())?;
    if matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif
    ) {
        Ok(format)
    } else {
        Err("Formato de imagen no compatible.".into())
    }
}

fn raster_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits
}

fn raster_dimensions(bytes: &[u8], format: ImageFormat) -> Result<(u32, u32), String> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(raster_limits());
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| "No se pudieron leer las dimensiones de la imagen.".to_owned())?;
    if width == 0
        || height == 0
        || width > MAX_IMAGE_SIDE
        || height > MAX_IMAGE_SIDE
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Err("La imagen supera 4096 píxeles por lado o 16 megapíxeles.".into());
    }
    Ok((width, height))
}

fn raster(bytes: &[u8], requested_width: u32) -> Result<PreviewImage, String> {
    let format = supported_format(bytes)?;
    let (source_width, source_height) = raster_dimensions(bytes, format)?;
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(raster_limits());
    // Animated formats intentionally show their first frame, without retaining
    // an animation's unbounded sequence of decoded frames.
    let source = reader
        .decode()
        .map_err(|e| format!("No se pudo decodificar la imagen: {e}"))?;
    let (width, height, _) = fit_dimensions(
        source_width as f64,
        source_height as f64,
        requested_width.clamp(32, MAX_IMAGE_SIDE),
        false,
    )?;
    let image = if (source_width, source_height) == (width, height) {
        source.to_rgba8()
    } else {
        source
            .resize_exact(width, height, image::imageops::FilterType::Triangle)
            .to_rgba8()
    };
    let mut encoded = BoundedPng(Vec::new());
    image::codecs::png::PngEncoder::new(&mut encoded)
        .write_image(
            image.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|_| "La imagen decodificada supera el límite de vista previa.".to_owned())?;
    preview_image(width, height, encoded.0)
}

fn preview_image(width: u32, height: u32, png: Vec<u8>) -> Result<PreviewImage, String> {
    if png.len() > MAX_PNG_BYTES {
        return Err("La imagen renderizada supera el límite de 8 MiB.".into());
    }
    Ok(PreviewImage {
        width,
        height,
        png: png.into(),
    })
}

fn fit_dimensions(
    width: f64,
    height: f64,
    requested_width: u32,
    upscale: bool,
) -> Result<(u32, u32, f32), String> {
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
        || width > 1e8
        || height > 1e8
    {
        return Err("El documento contiene dimensiones inválidas.".into());
    }
    let mut scale = f64::from(requested_width) / width;
    if !upscale {
        scale = scale.min(1.0);
    }
    scale = scale
        .min(f64::from(MAX_IMAGE_SIDE) / width)
        .min(f64::from(MAX_IMAGE_SIDE) / height)
        .min((MAX_RENDER_PIXELS as f64 / (width * height)).sqrt());
    if !scale.is_finite() || scale <= 0.0 {
        return Err("No se pudo calcular el tamaño de vista previa.".into());
    }
    // Floor keeps the actual allocation within the configured pixel budget.
    let output_width = (width * scale).floor().max(1.0) as u32;
    let output_height = (height * scale).floor().max(1.0) as u32;
    Ok((output_width, output_height, scale as f32))
}

struct BoundedPng(Vec<u8>);
impl Write for BoundedPng {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_PNG_BYTES {
            return Err(std::io::Error::other("PNG preview limit exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn test_pdf() -> Vec<u8> {
    let streams = ["1 0 0 rg 0 0 200 100 re f", "0 1 0 rg 0 0 200 100 re f"];
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>"
            .into(),
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            streams[0].len(),
            streams[0]
        ),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 6 0 R /Resources << >> >>"
            .into(),
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            streams[1].len(),
            streams[1]
        ),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scope;

    fn request(root: &Path, path: &str, page: usize) -> Request {
        Request {
            scope: Scope {
                workspace: root.into(),
                path: path.into(),
                tab_id: 1,
                generation: 1,
            },
            source: None,
            page,
            render_width: 400,
        }
    }

    #[test]
    fn pdf_pages_render_real_pixels_and_reuse_the_parsed_document() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("two.pdf"), test_pdf()).unwrap();
        let mut media = Media::default();
        for (index, rgb) in [(0, [255, 0, 0]), (1, [0, 255, 0])] {
            let Document::Pdf { image, page, pages } =
                media.pdf(&request(root.path(), "two.pdf", index)).unwrap()
            else {
                panic!("expected PDF")
            };
            assert_eq!((page, pages), (index, 2));
            assert_eq!((image.width, image.height), (400, 200));
            let decoded = image::load_from_memory(&image.png).unwrap().to_rgb8();
            assert_eq!(decoded.get_pixel(100, 100).0, rgb);
        }
        assert!(media.pdf(&request(root.path(), "two.pdf", 2)).is_err());
        std::fs::write(root.path().join("bad.pdf"), b"not PDF").unwrap();
        assert!(media.pdf(&request(root.path(), "bad.pdf", 0)).is_err());
    }

    #[test]
    fn png_and_svg_render_native_pixels_and_preserve_aspect_ratio() {
        let source = image::RgbaImage::from_pixel(80, 40, image::Rgba([255, 0, 0, 255]));
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut encoded, ImageFormat::Png)
            .unwrap();
        let raster = raster(encoded.get_ref(), 40).unwrap();
        assert_eq!((raster.width, raster.height), (40, 20));
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="40"><rect width="80" height="40" fill="red"/></svg>"#;
        let image = Media::default().svg(svg, 160).unwrap();
        assert_eq!((image.width, image.height), (160, 80));
        assert_eq!(
            image::load_from_memory(&image.png)
                .unwrap()
                .to_rgb8()
                .get_pixel(20, 20)
                .0,
            [255, 0, 0]
        );
    }

    #[test]
    fn image_and_pdf_dimensions_and_encoded_output_are_bounded() {
        for (width, height) in [(10.0, 1e7), (1e7, 10.0), (4000.0, 4000.0)] {
            let (w, h, _) = fit_dimensions(width, height, 4096, true).unwrap();
            assert!(w <= MAX_IMAGE_SIDE && h <= MAX_IMAGE_SIDE);
            assert!(u64::from(w) * u64::from(h) <= MAX_RENDER_PIXELS);
        }
        assert!(fit_dimensions(f64::NAN, 1.0, 100, true).is_err());
        assert!(fit_dimensions(10.0, 0.0, 100, true).is_err());
        assert!(raster(b"corrupt", 100).is_err());
        let mut writer = BoundedPng(vec![0; MAX_PNG_BYTES]);
        assert!(writer.write_all(&[0]).is_err());
    }

    #[test]
    fn svg_does_not_read_external_file_references() {
        let root = tempfile::tempdir().unwrap();
        let raster = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        raster.save(root.path().join("external.png")).unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><image href="{}" width="20" height="20"/></svg>"#,
            root.path().join("external.png").display()
        );
        let image = Media::default().svg(svg.as_bytes(), 32).unwrap();
        assert_eq!(
            image::load_from_memory(&image.png)
                .unwrap()
                .to_rgba8()
                .get_pixel(10, 10)
                .0[3],
            0
        );
    }
}
