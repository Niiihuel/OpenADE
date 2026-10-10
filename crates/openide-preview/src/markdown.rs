//! Bounded, inert Markdown document model. Parsing never performs IO or opens URLs.
use std::{iter::Peekable, ops::Range};

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_BLOCKS: usize = 10_000;
const MAX_EVENTS: usize = 200_000;
const MAX_NESTING: usize = 64;

#[derive(Clone, Debug, Default)]
pub struct MarkdownDocument {
    pub blocks: Vec<MarkdownBlock>,
    pub links: Vec<MarkdownLink>,
    pub images: Vec<MarkdownImage>,
    pub truncated: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownLink {
    /// Unmodified CommonMark destination. The host must validate it on activation.
    pub destination: String,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct MarkdownImage {
    pub destination: String,
    pub alt: String,
    pub title: String,
    /// Filled by the background worker after workspace-scoped local resolution.
    pub image: Option<crate::PreviewImage>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownBlock {
    Heading {
        level: u8,
        id: String,
        content: Vec<MarkdownInline>,
    },
    Paragraph(Vec<MarkdownInline>),
    Quote(Vec<MarkdownBlock>),
    List {
        start: Option<u64>,
        items: Vec<MarkdownListItem>,
    },
    Code {
        language: String,
        source: String,
    },
    Table {
        alignments: Vec<TableAlignment>,
        header: Vec<Vec<MarkdownInline>>,
        rows: Vec<Vec<Vec<MarkdownInline>>>,
    },
    Rule,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownListItem {
    pub checked: Option<bool>,
    pub blocks: Vec<MarkdownBlock>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TableAlignment {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownInline {
    Text {
        text: String,
        style: InlineStyle,
    },
    Image {
        index: usize,
        link: Option<usize>,
    },
    Break,
    /// Consumed into MarkdownListItem when it begins a task-list item.
    Task(bool),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub strong: bool,
    pub emphasis: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub link: Option<usize>,
}

/// Produces a display model from the supplied live buffer, never from disk.
/// Source, event, block and nesting limits are surfaced as an explicit warning.
pub fn parse(source: &str) -> MarkdownDocument {
    let mut end = source.len().min(MAX_SOURCE_BYTES);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_GFM;
    let mut reader = Reader {
        events: Parser::new_ext(&source[..end], options)
            .into_offset_iter()
            .peekable(),
        document: MarkdownDocument::default(),
        blocks: 0,
        events_read: 0,
        stopped: false,
        heading_ids: std::collections::HashMap::new(),
    };
    reader.document.truncated = end < source.len();
    reader.document.blocks = reader.blocks_until(None, 0);
    if reader.document.truncated {
        reader.document.warning = Some(
            "Vista previa limitada: el documento supera el límite de tamaño o complejidad. El archivo original se conserva completo.".into(),
        );
    }
    reader.document
}

struct Reader<'a> {
    events: Peekable<pulldown_cmark::OffsetIter<'a>>,
    document: MarkdownDocument,
    blocks: usize,
    events_read: usize,
    stopped: bool,
    heading_ids: std::collections::HashMap<String, usize>,
}

impl<'a> Reader<'a> {
    fn stop(&mut self) {
        self.stopped = true;
        self.document.truncated = true;
    }

    fn next(&mut self) -> Option<(Event<'a>, Range<usize>)> {
        if self.stopped {
            return None;
        }
        if self.events_read >= MAX_EVENTS {
            self.stop();
            return None;
        }
        self.events_read += 1;
        self.events.next()
    }

    fn peek_end(&mut self, end: TagEnd) -> bool {
        matches!(self.events.peek(), Some((Event::End(actual), _)) if *actual == end)
    }

    fn push(&mut self, blocks: &mut Vec<MarkdownBlock>, block: MarkdownBlock) {
        blocks.push(block);
    }

    fn blocks_until(&mut self, end: Option<TagEnd>, depth: usize) -> Vec<MarkdownBlock> {
        let mut blocks = Vec::new();
        if depth >= MAX_NESTING {
            self.stop();
            return blocks;
        }
        while !self.stopped && self.events.peek().is_some() {
            if end.is_some_and(|end| self.peek_end(end)) {
                self.next();
                break;
            }
            if self.blocks >= MAX_BLOCKS {
                self.stop();
                break;
            }
            // Reserve containers before parsing children, so reaching a limit
            // still retains the already parsed portion of a list or quotation.
            self.blocks += 1;
            if self
                .events
                .peek()
                .is_some_and(|(event, _)| is_inline(event))
            {
                let inline = self.inlines_until(None);
                self.push(&mut blocks, MarkdownBlock::Paragraph(inline));
                continue;
            }
            let Some((event, _)) = self.next() else {
                break;
            };
            let block = match event {
                Event::Start(Tag::Paragraph) => Some(MarkdownBlock::Paragraph(
                    self.inlines_until(Some(TagEnd::Paragraph)),
                )),
                Event::Start(Tag::Heading { level, .. }) => {
                    let content = self.inlines_until(Some(TagEnd::Heading(level)));
                    let id = self.heading_id(&content);
                    Some(MarkdownBlock::Heading {
                        level: level as u8,
                        id,
                        content,
                    })
                }
                Event::Start(Tag::BlockQuote(kind)) => Some(MarkdownBlock::Quote(
                    self.blocks_until(Some(TagEnd::BlockQuote(kind)), depth + 1),
                )),
                Event::Start(Tag::List(start)) => Some(self.list(start, depth + 1)),
                Event::Start(Tag::CodeBlock(kind)) => {
                    let language = match kind {
                        CodeBlockKind::Fenced(value) => value
                            .split_whitespace()
                            .next()
                            .unwrap_or_default()
                            .to_owned(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    Some(MarkdownBlock::Code {
                        language,
                        source: self.literal_until(TagEnd::CodeBlock),
                    })
                }
                Event::Start(Tag::HtmlBlock) => Some(MarkdownBlock::Code {
                    language: "html".into(),
                    source: self.literal_until(TagEnd::HtmlBlock),
                }),
                Event::Start(Tag::Table(alignments)) => Some(self.table(alignments)),
                Event::Rule => Some(MarkdownBlock::Rule),
                _ => None,
            };
            if let Some(block) = block {
                self.push(&mut blocks, block);
            }
        }
        blocks
    }

    fn literal_until(&mut self, end: TagEnd) -> String {
        let mut text = String::new();
        while let Some((event, _)) = self.next() {
            match event {
                Event::End(actual) if actual == end => break,
                Event::Text(value)
                | Event::Code(value)
                | Event::Html(value)
                | Event::InlineHtml(value) => text.push_str(&value),
                Event::SoftBreak | Event::HardBreak => text.push('\n'),
                _ => {}
            }
        }
        text
    }

    fn list(&mut self, start: Option<u64>, depth: usize) -> MarkdownBlock {
        let mut items = Vec::new();
        while !self.stopped {
            let Some((event, _)) = self.next() else {
                break;
            };
            match event {
                Event::End(TagEnd::List(_)) => break,
                Event::Start(Tag::Item) => {
                    if self.blocks >= MAX_BLOCKS {
                        self.stop();
                        break;
                    }
                    self.blocks += 1;
                    let mut blocks = self.blocks_until(Some(TagEnd::Item), depth);
                    let checked = match blocks.first_mut() {
                        Some(MarkdownBlock::Paragraph(content))
                            if matches!(content.first(), Some(MarkdownInline::Task(_))) =>
                        {
                            match content.remove(0) {
                                MarkdownInline::Task(value) => Some(value),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    items.push(MarkdownListItem { checked, blocks });
                }
                _ => {}
            }
        }
        MarkdownBlock::List { start, items }
    }

    fn table(&mut self, alignments: Vec<Alignment>) -> MarkdownBlock {
        let mut header = Vec::new();
        let mut rows = Vec::new();
        let mut row = Vec::new();
        let mut in_header = false;
        while let Some((event, _)) = self.next() {
            match event {
                Event::Start(Tag::TableHead) => {
                    in_header = true;
                    row.clear();
                }
                Event::Start(Tag::TableRow) => row.clear(),
                Event::Start(Tag::TableCell) => {
                    if self.blocks >= MAX_BLOCKS {
                        self.stop();
                        break;
                    }
                    self.blocks += 1;
                    row.push(self.inlines_until(Some(TagEnd::TableCell)));
                }
                Event::End(TagEnd::TableHead) => {
                    header = std::mem::take(&mut row);
                    in_header = false;
                }
                Event::End(TagEnd::TableRow) => {
                    if in_header {
                        header = std::mem::take(&mut row);
                    } else {
                        rows.push(std::mem::take(&mut row));
                    }
                }
                Event::End(TagEnd::Table) => break,
                _ => {}
            }
        }
        MarkdownBlock::Table {
            alignments: alignments
                .into_iter()
                .map(|alignment| match alignment {
                    Alignment::Center => TableAlignment::Center,
                    Alignment::Right => TableAlignment::Right,
                    _ => TableAlignment::Left,
                })
                .collect(),
            header,
            rows,
        }
    }

    fn inlines_until(&mut self, end: Option<TagEnd>) -> Vec<MarkdownInline> {
        let mut content = Vec::new();
        let mut style = InlineStyle::default();
        let mut stack: Vec<(TagEnd, InlineStyle)> = Vec::new();
        while !self.stopped {
            if end.is_none()
                && stack.is_empty()
                && !self
                    .events
                    .peek()
                    .is_some_and(|(event, _)| is_inline(event))
            {
                break;
            }
            let Some((event, _)) = self.next() else {
                break;
            };
            match event {
                Event::End(actual) if Some(actual) == end && stack.is_empty() => break,
                Event::Start(Tag::Image {
                    dest_url, title, ..
                }) => {
                    let alt = self.literal_until(TagEnd::Image);
                    let index = self.document.images.len();
                    self.document.images.push(MarkdownImage {
                        destination: dest_url.to_string(),
                        alt,
                        title: title.to_string(),
                        image: None,
                        error: None,
                    });
                    content.push(MarkdownInline::Image {
                        index,
                        link: style.link,
                    });
                }
                Event::Start(
                    tag @ (Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. }),
                ) => {
                    if stack.len() >= MAX_NESTING {
                        self.stop();
                        break;
                    }
                    stack.push((tag.to_end(), style));
                    match tag {
                        Tag::Emphasis => style.emphasis = true,
                        Tag::Strong => style.strong = true,
                        Tag::Strikethrough => style.strikethrough = true,
                        Tag::Link {
                            dest_url, title, ..
                        } => {
                            let index = self.document.links.len();
                            self.document.links.push(MarkdownLink {
                                destination: dest_url.to_string(),
                                title: title.to_string(),
                            });
                            style.link = Some(index);
                        }
                        _ => {}
                    }
                }
                Event::End(actual) => {
                    if stack.last().is_some_and(|(tag, _)| *tag == actual) {
                        style = stack.pop().unwrap().1;
                    }
                }
                Event::Text(text) => append_text(&mut content, &text, style),
                Event::Code(text) => append_text(
                    &mut content,
                    &text,
                    InlineStyle {
                        code: true,
                        ..style
                    },
                ),
                // HTML is rendered as inert source, never interpreted by a browser or UI parser.
                Event::Html(text) | Event::InlineHtml(text) => append_text(
                    &mut content,
                    &text,
                    InlineStyle {
                        code: true,
                        ..style
                    },
                ),
                Event::SoftBreak => append_text(&mut content, " ", style),
                Event::HardBreak => content.push(MarkdownInline::Break),
                Event::TaskListMarker(checked) => content.push(MarkdownInline::Task(checked)),
                _ => {}
            }
        }
        content
    }

    fn heading_id(&mut self, content: &[MarkdownInline]) -> String {
        let mut slug = String::new();
        for inline in content {
            if let MarkdownInline::Text { text, .. } = inline {
                for character in text.chars().flat_map(char::to_lowercase) {
                    if character.is_alphanumeric() || character == '-' || character == '_' {
                        slug.push(character);
                    } else if character.is_whitespace() {
                        slug.push('-');
                    }
                }
            }
        }
        let occurrence = self.heading_ids.entry(slug.clone()).or_default();
        let id = if *occurrence == 0 {
            slug
        } else {
            format!("{slug}-{occurrence}")
        };
        *occurrence += 1;
        id
    }
}

fn is_inline(event: &Event<'_>) -> bool {
    matches!(
        event,
        Event::Text(_)
            | Event::Code(_)
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::TaskListMarker(_)
            | Event::Start(
                Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Link { .. }
                    | Tag::Image { .. }
            )
    )
}

fn append_text(content: &mut Vec<MarkdownInline>, text: &str, style: InlineStyle) {
    if let Some(MarkdownInline::Text {
        text: previous,
        style: previous_style,
    }) = content.last_mut()
    {
        if *previous_style == style {
            previous.push_str(text);
            return;
        }
    }
    content.push(MarkdownInline::Text {
        text: text.to_owned(),
        style,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gfm_model_preserves_tables_tasks_nesting_and_fences() {
        let document = parse(
            "# Hello\n\n| Item | Status |\n|:---|---:|\n| **A** | `ok` |\n\n- [x] Done\n  - Nested\n- [ ] Pending\n\n> Quote\n\n```rust\nlet n = 1;\n```\n",
        );
        assert!(!document.truncated);
        assert!(
            matches!(&document.blocks[1], MarkdownBlock::Table { header, rows, alignments } if header.len() == 2 && rows.len() == 1 && alignments[1] == TableAlignment::Right)
        );
        let MarkdownBlock::List { items, .. } = &document.blocks[2] else {
            panic!("expected list")
        };
        assert_eq!(items[0].checked, Some(true));
        assert_eq!(items[1].checked, Some(false));
        assert!(matches!(
            items[0].blocks.last(),
            Some(MarkdownBlock::List { .. })
        ));
        assert!(
            matches!(&document.blocks[4], MarkdownBlock::Code { language, source } if language == "rust" && source == "let n = 1;\n")
        );
    }

    #[test]
    fn link_and_image_targets_remain_separate_from_display_and_do_no_io() {
        let document = parse(
            "[Source](../src/main.rs#L3) [Site](https://example.com) ![A *diagram*](./assets/a%20b.png \"Caption\") [Unsafe](javascript:alert(1))",
        );
        assert_eq!(document.links[0].destination, "../src/main.rs#L3");
        assert_eq!(document.links[2].destination, "javascript:alert(1)");
        assert_eq!(document.images[0].destination, "./assets/a%20b.png");
        assert_eq!(document.images[0].alt, "A diagram");
        assert!(document.images[0].image.is_none());
        assert!(document.images[0].error.is_none());

        let badge = parse("[![Build](./badge.svg)](https://example.com/build)");
        assert!(matches!(&badge.blocks[0], MarkdownBlock::Paragraph(content)
            if matches!(&content[0], MarkdownInline::Image { index: 0, link: Some(0) })));
    }

    #[test]
    fn unsaved_source_html_and_duplicate_heading_anchors_are_preserved() {
        let document =
            parse("# Changed locally\n\n<span onclick='run()'>Text</span>\n\n# Changed locally\n");
        assert!(
            matches!(&document.blocks[0], MarkdownBlock::Heading { id, .. } if id == "changed-locally")
        );
        assert!(
            matches!(&document.blocks[2], MarkdownBlock::Heading { id, .. } if id == "changed-locally-1")
        );
        assert!(document.links.is_empty());
        let MarkdownBlock::Paragraph(inline) = &document.blocks[1] else {
            panic!("expected inert inline HTML")
        };
        assert!(inline.iter().any(|item| matches!(item, MarkdownInline::Text { text, style } if style.code && text.contains("onclick"))));
    }

    #[test]
    fn large_documents_are_explicitly_limited_without_splitting_utf8() {
        let document = parse(&"🦀".repeat(MAX_SOURCE_BYTES / 4 + 1));
        assert!(document.truncated && document.warning.is_some());
        let many = parse(&"paragraph\n\n".repeat(MAX_BLOCKS + 1));
        assert!(many.truncated && many.blocks.len() == MAX_BLOCKS);
        let list = parse(&"- item\n".repeat(MAX_BLOCKS));
        assert!(
            list.truncated
                && matches!(list.blocks.first(), Some(MarkdownBlock::List { items, .. }) if !items.is_empty())
        );
        assert!(parse(&format!("{}deep", "> ".repeat(MAX_NESTING + 1))).truncated);
    }
}
