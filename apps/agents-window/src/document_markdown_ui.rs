//! Native document Markdown; actions remain scoped to the owning preview tab.
use std::{cell::RefCell, collections::VecDeque, sync::Arc};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use openide_preview::{
    markdown::{InlineStyle, MarkdownBlock, MarkdownDocument, MarkdownInline},
    PreviewImage,
};
use openide_ui::{
    CodeBlock, CodeBlockSizing, Component, Element, FontFamily, RasterImage, RasterImageFormat,
    TextSpan,
};
use sha2::{Digest, Sha256};

use crate::theme;

const BODY_SIZE: f32 = 14.0;
const CODE_SIZE: f32 = 13.0;
const IMAGE_CACHE_ENTRIES: usize = 16;
const IMAGE_CACHE_BYTES: usize = 64 * 1024 * 1024;

struct CachedRaster {
    png: Arc<[u8]>,
    raster: RasterImage,
}

thread_local! {
    static RASTER_CACHE: RefCell<VecDeque<CachedRaster>> = const { RefCell::new(VecDeque::new()) };
}

/// Encoded PNG conversion is cached by retained byte allocation and alt text.
/// This helper performs no IO or decoding; the native raster renderer validates it.
pub(crate) fn image_to_raster(image: &PreviewImage, alt: &str) -> Option<RasterImage> {
    let alt = if alt.trim().is_empty() {
        "Imagen del documento"
    } else {
        alt
    };
    let mut alt_end = alt.len().min(256);
    while !alt.is_char_boundary(alt_end) {
        alt_end -= 1;
    }
    let alt = &alt[..alt_end];
    RASTER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache
            .iter()
            .position(|entry| Arc::ptr_eq(&entry.png, &image.png) && entry.raster.alt() == alt)
        {
            let entry = cache.remove(index).unwrap();
            let raster = entry.raster.clone();
            cache.push_back(entry);
            return Some(raster);
        }
        if image.png.is_empty() || image.png.len() > 8 * 1024 * 1024 {
            return None;
        }
        let raster = RasterImage::new(
            format!("{:x}", Sha256::digest(&image.png)),
            RasterImageFormat::Png,
            image.png.len() as u32,
            Arc::from(STANDARD.encode(&image.png)),
            alt,
        )?
        .full_resolution();
        cache.push_back(CachedRaster {
            png: image.png.clone(),
            raster: raster.clone(),
        });
        while cache.len() > IMAGE_CACHE_ENTRIES
            || cache
                .iter()
                .map(|entry| entry.png.len() + entry.raster.data_base64().len())
                .sum::<usize>()
                > IMAGE_CACHE_BYTES
        {
            cache.pop_front();
        }
        Some(raster)
    })
}

/// One direct child per source block keeps the host's heading anchor navigation stable.
/// The host supplies the vertical ScrollArea and its actual available content width.
pub(crate) fn build(document: &MarkdownDocument, width: f32, action_prefix: &str) -> Element {
    let width = if width.is_finite() {
        width.max(0.0)
    } else {
        0.0
    };
    let mut blocks = render_blocks(&document.blocks, document, width, action_prefix);
    if let Some(warning) = &document.warning {
        blocks.push(
            Element::text(warning)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .foreground(theme::FOREGROUND_SUBTLE)
                .padding(10.0)
                .background(theme::PANEL)
                .radius(6.0),
        );
    }
    if blocks.is_empty() {
        blocks.push(
            Element::text("El documento está vacío")
                .font_family(FontFamily::Sans)
                .font_size(BODY_SIZE)
                .foreground(theme::FOREGROUND_SUBTLE),
        );
    }
    Element::column(blocks).gap(14.0).size(Some(width), None)
}

/// Stable native focus target for an internal Markdown heading link.
pub(crate) fn heading_action(action_prefix: &str, heading_id: &str) -> String {
    format!("{action_prefix}:heading:{heading_id}")
}

fn render_blocks(
    blocks: &[MarkdownBlock],
    document: &MarkdownDocument,
    width: f32,
    prefix: &str,
) -> Vec<Element> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| render_block(block, document, width, prefix, index))
        .collect()
}

fn render_block(
    block: &MarkdownBlock,
    document: &MarkdownDocument,
    width: f32,
    prefix: &str,
    index: usize,
) -> Element {
    match block {
        MarkdownBlock::Heading { level, id, content } => {
            let size = match level {
                1 => 28.0,
                2 => 23.0,
                3 => 19.0,
                4 => 17.0,
                _ => 15.0,
            };
            Element::clickable(
                heading_action(prefix, id),
                inline(content, document, width, size, prefix),
            )
        }
        MarkdownBlock::Paragraph(content) => inline(content, document, width, BODY_SIZE, prefix),
        MarkdownBlock::Quote(blocks) => Element::row(vec![
            Element::spacer()
                .size(Some(3.0), None)
                .background(theme::BORDER),
            Element::column(render_blocks(
                blocks,
                document,
                (width - 18.0).max(0.0),
                prefix,
            ))
            .gap(10.0)
            .flex(1.0),
        ])
        .gap(12.0)
        .size(Some(width), None),
        MarkdownBlock::List { start, items } => Element::column(
            items
                .iter()
                .enumerate()
                .map(|(item_index, item)| {
                    let marker = match item.checked {
                        Some(true) => "☑".to_owned(),
                        Some(false) => "☐".to_owned(),
                        None => start.map_or_else(
                            || "•".to_owned(),
                            |start| format!("{}.", start.saturating_add(item_index as u64)),
                        ),
                    };
                    let marker_width = if start.is_some() { 34.0_f32 } else { 22.0 }.min(width);
                    Element::row(vec![
                        Element::text(marker)
                            .font_family(FontFamily::Sans)
                            .font_size(BODY_SIZE)
                            .foreground(if item.checked == Some(true) {
                                theme::DIFF_ADDED
                            } else {
                                theme::FOREGROUND_SUBTLE
                            })
                            .size(Some(marker_width), None),
                        Element::column(render_blocks(
                            &item.blocks,
                            document,
                            (width - marker_width - 4.0).max(0.0),
                            prefix,
                        ))
                        .gap(8.0)
                        .flex(1.0),
                    ])
                    .gap(4.0)
                })
                .collect(),
        )
        .gap(7.0)
        .size(Some(width), None),
        MarkdownBlock::Code { language, source } => code(language, source, width),
        MarkdownBlock::Table { header, rows, .. } => {
            let columns = header
                .len()
                .max(rows.iter().map(Vec::len).max().unwrap_or_default())
                .max(1);
            // Equal measured columns wrap content within the native document width.
            let cell_width = (width / columns as f32).max(0.0);
            let make_row = |cells: &[Vec<MarkdownInline>], header: bool| {
                Element::row(
                    (0..columns)
                        .map(|column| {
                            let content = cells.get(column).map(Vec::as_slice).unwrap_or_default();
                            Element::panel(inline(
                                content,
                                document,
                                (cell_width - 16.0).max(0.0),
                                BODY_SIZE,
                                prefix,
                            ))
                            .padding(8.0)
                            .size(Some(cell_width), None)
                            .background(if header {
                                theme::PANEL
                            } else {
                                theme::BACKGROUND
                            })
                            .border(theme::BORDER, 1.0)
                        })
                        .collect(),
                )
                .size(Some(width), None)
            };
            let mut elements = vec![make_row(header, true)];
            elements.extend(rows.iter().map(|cells| make_row(cells, false)));
            Element::column(elements)
                .size(Some(width), None)
                .hover_with(format!("{prefix}:table:{index}"))
        }
        MarkdownBlock::Rule => Element::spacer()
            .size(Some(width), Some(1.0))
            .background(theme::BORDER),
    }
}

fn inline(
    content: &[MarkdownInline],
    document: &MarkdownDocument,
    width: f32,
    size: f32,
    prefix: &str,
) -> Element {
    let mut elements = Vec::new();
    let mut spans = Vec::new();
    let flush = |spans: &mut Vec<TextSpan>, elements: &mut Vec<Element>| {
        if !spans.is_empty() {
            elements.push(Element::formatted_text(std::mem::take(spans)).size(Some(width), None));
        }
    };
    for item in content {
        match item {
            MarkdownInline::Text { text, style } => spans.push(span(text, *style, size, prefix)),
            MarkdownInline::Break => spans.push(span("\n", InlineStyle::default(), size, prefix)),
            MarkdownInline::Task(checked) => spans.push(span(
                if *checked { "☑ " } else { "☐ " },
                InlineStyle::default(),
                size,
                prefix,
            )),
            MarkdownInline::Image { index, link } => {
                flush(&mut spans, &mut elements);
                if let Some(image) = document.images.get(*index) {
                    let raster = image
                        .image
                        .as_ref()
                        .and_then(|loaded| image_to_raster(loaded, &image.alt));
                    let visual = if let (Some(loaded), Some(raster)) = (&image.image, raster) {
                        let image_width = width.min(loaded.width as f32);
                        let height =
                            image_width * loaded.height as f32 / loaded.width.max(1) as f32;
                        Element::raster_image(raster).size(Some(image_width), Some(height))
                    } else {
                        let alt = if image.alt.is_empty() {
                            "Imagen"
                        } else {
                            &image.alt
                        };
                        let reason = image
                            .error
                            .as_deref()
                            .unwrap_or("La imagen no está disponible");
                        Element::column(vec![
                            Element::text(alt)
                                .font_family(FontFamily::Sans)
                                .font_size(BODY_SIZE)
                                .foreground(theme::FOREGROUND),
                            Element::text(reason)
                                .font_family(FontFamily::Sans)
                                .font_size(12.0)
                                .foreground(theme::FOREGROUND_SUBTLE),
                        ])
                        .gap(6.0)
                        .padding(10.0)
                        .background(theme::PANEL)
                        .border(theme::BORDER, 1.0)
                        .radius(6.0)
                        .size(Some(width), None)
                    };
                    elements.push(if let Some(link) = link {
                        Element::clickable(format!("{prefix}:link:{link}"), visual)
                    } else {
                        visual
                    });
                    if !image.title.is_empty() {
                        elements.push(
                            Element::text(&image.title)
                                .font_family(FontFamily::Sans)
                                .font_size(12.0)
                                .foreground(theme::FOREGROUND_SUBTLE)
                                .size(Some(width), None),
                        );
                    }
                }
            }
        }
    }
    flush(&mut spans, &mut elements);
    Element::column(elements).gap(8.0).size(Some(width), None)
}

fn span(text: &str, style: InlineStyle, size: f32, prefix: &str) -> TextSpan {
    let color = if style.link.is_some() {
        theme::BRAND
    } else if style.strong {
        theme::USER
    } else if style.emphasis || style.strikethrough {
        theme::FOREGROUND_SUBTLE
    } else {
        theme::FOREGROUND
    };
    let mut span = TextSpan::new(
        text,
        color,
        if style.code { CODE_SIZE } else { size },
        if style.code {
            FontFamily::Monospace
        } else {
            FontFamily::Sans
        },
    );
    if style.code {
        span = span.background(theme::SELECTED);
    }
    if let Some(index) = style.link {
        span = span.action(format!("{prefix}:link:{index}"));
    }
    span
}

fn code_spans(language: &str, source: &str) -> Vec<TextSpan> {
    use super::{preview_source_spans, PreviewLanguage, PreviewLexState};
    let syntax = match language.to_ascii_lowercase().as_str() {
        "rs" | "rust" => PreviewLanguage::Rust,
        "ts" | "typescript" | "tsx" => PreviewLanguage::TypeScript,
        "js" | "javascript" | "jsx" => PreviewLanguage::JavaScript,
        "json" | "jsonc" => PreviewLanguage::Json,
        _ => PreviewLanguage::Text,
    };
    let mut state = PreviewLexState::default();
    let mut spans = Vec::new();
    for line in source.split_inclusive('\n') {
        let line_content = line.strip_suffix('\n').unwrap_or(line);
        spans.extend(
            preview_source_spans(line_content, syntax, &mut state)
                .into_iter()
                .map(|mut span| {
                    span.font_size = CODE_SIZE;
                    span
                }),
        );
        if line.ends_with('\n') {
            spans.push(TextSpan::new(
                "\n",
                theme::FOREGROUND,
                CODE_SIZE,
                FontFamily::Monospace,
            ));
        }
    }
    spans
}

fn code(language: &str, source: &str, width: f32) -> Element {
    let mut block = CodeBlock::new(source, theme::panel_card_palette())
        .spans(code_spans(language, source))
        .sizing(CodeBlockSizing::DOCUMENT)
        .width(width);
    if !language.is_empty() {
        block = block.label(language);
    }
    block.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, Size, UiTree};

    // The shell of `code` as it was before limn's CodeBlock; delete with the final audit (S18).
    fn legacy_code_shell(language: &str, spans: Vec<TextSpan>, width: f32) -> Element {
        let mut children = Vec::new();
        if !language.is_empty() {
            children.push(
                Element::text(language)
                    .font_family(FontFamily::Sans)
                    .font_size(11.0)
                    .foreground(theme::FOREGROUND_SUBTLE),
            );
        }
        children.push(Element::formatted_text(spans).size(Some((width - 24.0).max(0.0)), None));
        Element::panel(Element::column(children).gap(8.0))
            .padding(12.0)
            .background(theme::PANEL)
            .border(theme::BORDER, 1.0)
            .radius(8.0)
            .size(Some(width), None)
    }

    #[test]
    fn code_blocks_match_their_legacy_shell() {
        let source = "fn main() {\n    println!(\"hola\");\n}\n";
        for language in ["", "rust", "toml"] {
            for width in [10.0, 220.0, 640.0] {
                crate::icon_button_parity_tests::assert_same(
                    &format!("document code {language:?} at {width}"),
                    legacy_code_shell(language, code_spans(language, source), width),
                    code(language, source, width),
                );
            }
        }
    }

    #[test]
    fn document_markdown_links_are_scoped_and_tables_fit_the_document() {
        let document = openide_preview::markdown::parse(
            "# Native preview\n\nA [local link](../readme.md) and `code`.\n\n| A | B |\n|---|---|\n| uno | dos |\n",
        );
        let mut tree = UiTree::new(build(&document, 420.0, "preview:7:2"));
        let frame = tree.layout(Size::new(420.0, 600.0));
        assert!(frame
            .regions
            .iter()
            .any(|hit| hit.id == "preview:7:2:link:0"));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { text, .. } if text.contains("Native preview"))));
        assert!(frame
            .regions
            .iter()
            .all(|hit| hit.rect.x >= 0.0 && hit.rect.x + hit.rect.width <= 420.1));
    }

    #[test]
    fn missing_local_images_show_alt_and_failure_instead_of_disappearing() {
        let mut document = openide_preview::markdown::parse("![Architecture](./missing.png)");
        document.images[0].error = Some("No existe la imagen local".into());
        let mut tree = UiTree::new(build(&document, 320.0, "preview:1:1"));
        let frame = tree.layout(Size::new(320.0, 500.0));
        let text = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("Architecture") && text.contains("No existe la imagen local"));
    }

    #[test]
    fn internal_heading_target_reveals_an_offscreen_heading_without_replacing_links() {
        use openide_ui::{Component, ScrollArea};

        let source = format!(
            "[Jump](#destination)\n\n{}\n# [Destination](./related.md)\n\nAfter the heading.",
            "Paragraph before the destination.\n\n".repeat(35),
        );
        let document = openide_preview::markdown::parse(&source);
        let prefix = "preview:8:4";
        let heading = heading_action(prefix, "destination");
        let mut tree = UiTree::new(
            ScrollArea::new("document_scroll", build(&document, 360.0, prefix))
                .render()
                .size(Some(360.0), Some(180.0)),
        );
        let viewport = Size::new(360.0, 180.0);
        let initial = tree.layout(viewport);
        assert!(!initial.regions.iter().any(|region| region.id == heading));
        assert!(tree.focus(&heading));
        let revealed = tree.layout(viewport);
        assert!(tree.scroll_offset("document_scroll") > 0.0);
        assert!(revealed.regions.iter().any(|region| region.id == heading));
        assert!(revealed
            .regions
            .iter()
            .any(|region| region.id == "preview:8:4:link:1"));
    }
}
