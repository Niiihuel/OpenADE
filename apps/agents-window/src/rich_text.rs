//! Bounded Markdown presentation for agent responses.
//!
//! The renderer builds an ordinary `openide-ui` element tree. It deliberately
//! supports a useful subset of Markdown instead of treating agent output as
//! trusted HTML. WarpUI's MIT formatted-text element informed the separation
//! between parsing, line layout, and painting; this is an original renderer.
//! The UI engine flows inline spans with the host font measurer. It does not
//! have font weight, italics, selectable text, or link activation yet, so
//! emphasis uses neutral colors and links show their destinations as text.

use openide_ui::{Color, Element, FontFamily, TextSpan};

use crate::theme;

const MAX_SOURCE_CHARS: usize = 20_000;
const MAX_BLOCKS: usize = 160;
const MAX_MARKUP_LOOKAHEAD: usize = 2_048;
const MAX_URL_BYTES: usize = 2_048;
const BODY_SIZE: f32 = 14.0;

#[derive(Clone, Debug, Eq, PartialEq)]
enum ListMarker {
    Bullet,
    Number(String),
    Done,
    Todo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Block {
    Heading(u8, String),
    Paragraph(String),
    ListItem(usize, ListMarker, String),
    Quote(String),
    Code(String, String),
    Rule,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InlineKind {
    Normal,
    Strong,
    Emphasis,
    Code,
    Link,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InlineRun {
    kind: InlineKind,
    text: String,
}

fn push_run(runs: &mut Vec<InlineRun>, kind: InlineKind, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut().filter(|last| last.kind == kind) {
        last.text.push_str(text);
    } else {
        runs.push(InlineRun {
            kind,
            text: text.to_owned(),
        });
    }
}

fn flush_paragraph(blocks: &mut Vec<Block>, paragraph: &mut Vec<String>) {
    if paragraph.is_empty() {
        return;
    }
    let mut content = String::new();
    for line in paragraph.drain(..) {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push(' ');
        }
        content.push_str(&line);
    }
    blocks.push(Block::Paragraph(content));
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let count = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    (1..=3)
        .contains(&count)
        .then(|| line.get(count..))
        .flatten()
        .and_then(|rest| rest.strip_prefix(' ').map(|text| (count as u8, text)))
}

fn list_item(line: &str) -> Option<(usize, ListMarker, &str)> {
    let indent = line
        .chars()
        .take_while(|character| *character == ' ')
        .count();
    let body = line.get(indent..)?;
    let depth = (indent / 2).min(4);
    if let Some(text) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
        if let Some(text) = text
            .strip_prefix("[x] ")
            .or_else(|| text.strip_prefix("[X] "))
        {
            return Some((depth, ListMarker::Done, text));
        }
        if let Some(text) = text.strip_prefix("[ ] ") {
            return Some((depth, ListMarker::Todo, text));
        }
        return Some((depth, ListMarker::Bullet, text));
    }
    let number_len = body.chars().take_while(char::is_ascii_digit).count();
    if (1..=5).contains(&number_len) {
        let number = body.get(..number_len)?;
        let text = body.get(number_len..)?.strip_prefix(". ")?;
        return Some((depth, ListMarker::Number(number.to_owned()), text));
    }
    None
}

fn parse(source: &str) -> (Vec<Block>, bool) {
    let limited: String = source.chars().take(MAX_SOURCE_CHARS + 1).collect();
    let mut truncated = limited.chars().count() > MAX_SOURCE_CHARS;
    let limited: String = limited.chars().take(MAX_SOURCE_CHARS).collect();
    let mut blocks = Vec::new();
    let mut paragraph = Vec::new();
    let mut code = Vec::new();
    let mut language = String::new();
    let mut in_code = false;
    for line in limited.lines() {
        if blocks.len() >= MAX_BLOCKS {
            truncated = true;
            break;
        }
        let trimmed = line.trim_end();
        if let Some(after_fence) = trimmed.trim_start().strip_prefix("```") {
            if in_code {
                blocks.push(Block::Code(language.clone(), code.join("\n")));
                code.clear();
                language.clear();
                in_code = false;
            } else {
                flush_paragraph(&mut blocks, &mut paragraph);
                language = after_fence.trim().chars().take(24).collect();
                in_code = true;
            }
            continue;
        }
        if in_code {
            code.push(line.to_owned());
            continue;
        }
        if trimmed.trim().is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
        } else if let Some((level, text)) = heading(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Heading(level, text.to_owned()));
        } else if let Some((depth, marker, text)) = list_item(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::ListItem(depth, marker, text.to_owned()));
        } else if let Some(text) = trimmed.trim_start().strip_prefix("> ") {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Quote(text.to_owned()));
        } else if matches!(trimmed.trim(), "---" | "***" | "___") {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::Rule);
        } else {
            let hard_break = line.ends_with("  ") || line.ends_with('\\');
            let mut value = trimmed.trim().to_owned();
            if hard_break {
                value = value.trim_end_matches('\\').to_owned();
                value.push('\n');
            }
            paragraph.push(value);
        }
    }
    if in_code {
        blocks.push(Block::Code(language, code.join("\n")));
    }
    flush_paragraph(&mut blocks, &mut paragraph);
    if blocks.len() > MAX_BLOCKS {
        blocks.truncate(MAX_BLOCKS);
        truncated = true;
    }
    (blocks, truncated)
}

fn bounded_lookahead(value: &str) -> &str {
    let end = value
        .char_indices()
        .find(|(index, _)| *index >= MAX_MARKUP_LOOKAHEAD)
        .map_or(value.len(), |(index, _)| index);
    &value[..end]
}

fn safe_url(url: &str) -> bool {
    let authority = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    authority.is_some_and(|authority| {
        !authority.is_empty()
            && !authority.starts_with('/')
            && url.len() <= MAX_URL_BYTES
            && !url
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
    })
}

fn flush_plain(runs: &mut Vec<InlineRun>, plain: &mut String) {
    push_run(runs, InlineKind::Normal, plain);
    plain.clear();
}

fn parse_inline(source: &str) -> Vec<InlineRun> {
    let mut runs = Vec::new();
    let mut plain = String::new();
    let mut index = 0;
    while index < source.len() {
        let rest = &source[index..];
        if let Some(escaped) = rest.strip_prefix('\\') {
            if let Some(character) = escaped.chars().next().filter(|character| {
                matches!(character, '*' | '_' | '`' | '[' | ']' | '(' | ')' | '\\')
            }) {
                plain.push(character);
                index += 1 + character.len_utf8();
                continue;
            }
        }
        if let Some(body) = rest.strip_prefix('`') {
            if let Some(end) = bounded_lookahead(body).find('`').filter(|end| *end > 0) {
                flush_plain(&mut runs, &mut plain);
                push_run(&mut runs, InlineKind::Code, &body[..end]);
                index += end + 2;
                continue;
            }
        }
        if let Some(body) = rest.strip_prefix("**") {
            if let Some(end) = bounded_lookahead(body).find("**").filter(|end| *end > 0) {
                flush_plain(&mut runs, &mut plain);
                push_run(&mut runs, InlineKind::Strong, &body[..end]);
                index += end + 4;
                continue;
            }
        }
        if let Some(body) = rest.strip_prefix('*') {
            if let Some(end) = bounded_lookahead(body).find('*').filter(|end| *end > 0) {
                flush_plain(&mut runs, &mut plain);
                push_run(&mut runs, InlineKind::Emphasis, &body[..end]);
                index += end + 2;
                continue;
            }
        }
        if let Some(body) = rest.strip_prefix('[') {
            if let Some(label_end) = bounded_lookahead(body).find("](") {
                let tail = &body[label_end + 2..];
                if let Some(url_end) = bounded_lookahead(tail).find(')') {
                    let url = &tail[..url_end];
                    if !body[..label_end].is_empty() && safe_url(url) {
                        flush_plain(&mut runs, &mut plain);
                        push_run(&mut runs, InlineKind::Link, &body[..label_end]);
                        push_run(&mut runs, InlineKind::Normal, " (");
                        push_run(&mut runs, InlineKind::Normal, url);
                        push_run(&mut runs, InlineKind::Normal, ")");
                        index += label_end + url_end + 4;
                        continue;
                    }
                }
            }
        }
        let character = rest.chars().next().expect("index is inside source");
        plain.push(character);
        index += character.len_utf8();
    }
    flush_plain(&mut runs, &mut plain);
    runs
}

fn span_element(run: &InlineRun, size: f32) -> TextSpan {
    let color = match run.kind {
        InlineKind::Normal => theme::FOREGROUND,
        InlineKind::Strong | InlineKind::Link => theme::BRAND,
        InlineKind::Emphasis => Color::rgb(194, 194, 194),
        InlineKind::Code => theme::USER,
    };
    let mut span = TextSpan::new(
        &run.text,
        color,
        size,
        if run.kind == InlineKind::Code {
            FontFamily::Monospace
        } else {
            FontFamily::Sans
        },
    );
    if run.kind == InlineKind::Code {
        span = span.background(theme::SELECTED);
    }
    span
}

fn render_inline(source: &str, width: f32, size: f32) -> Element {
    let spans = parse_inline(source)
        .iter()
        .map(|run| span_element(run, size))
        .collect();
    Element::formatted_text(spans).size(Some(width.max(0.0)), None)
}

fn code_block(language: String, content: String) -> Element {
    let label = if language.is_empty() {
        "CÓDIGO".to_owned()
    } else {
        language.to_uppercase()
    };
    Element::panel(
        Element::column(vec![
            Element::text(label)
                .font_family(FontFamily::Monospace)
                .font_size(10.0)
                .foreground(theme::FOREGROUND_SUBTLE),
            Element::text(content)
                .font_family(FontFamily::Monospace)
                .font_size(12.0)
                .foreground(theme::FOREGROUND),
        ])
        .gap(8.0),
    )
    .padding(12.0)
    .background(theme::PANEL)
    .border(theme::BORDER, 1.0)
    .radius(8.0)
}

/// Render with the actual reading-column width in logical pixels. Link
/// destinations are displayed as text; no URL is opened by this function.
pub fn render_markdown_at_width(source: &str, width: f32) -> Element {
    let width = if width.is_finite() {
        width.clamp(120.0, 1_200.0)
    } else {
        600.0
    };
    let (blocks, truncated) = parse(source);
    let mut elements = Vec::new();
    for block in blocks {
        elements.push(match block {
            Block::Heading(level, text) => {
                let size = match level {
                    1 => 22.0,
                    2 => 18.0,
                    _ => 16.0,
                };
                render_inline(&text, width - 4.0, size).padding(2.0)
            }
            Block::Paragraph(text) => render_inline(&text, width - 4.0, BODY_SIZE).padding(2.0),
            Block::ListItem(depth, marker, text) => {
                let (label, color) = match marker {
                    ListMarker::Bullet => ("•".to_owned(), theme::FOREGROUND_SUBTLE),
                    ListMarker::Number(number) => (format!("{number}."), theme::FOREGROUND_SUBTLE),
                    ListMarker::Done => ("✓".to_owned(), theme::DIFF_ADDED),
                    ListMarker::Todo => ("○".to_owned(), theme::FOREGROUND_SUBTLE),
                };
                let indent = depth as f32 * 14.0;
                Element::row(vec![
                    Element::spacer().size(Some(indent), Some(1.0)),
                    Element::text(label)
                        .font_family(FontFamily::Sans)
                        .font_size(13.0)
                        .foreground(color)
                        .size(Some(22.0), None),
                    render_inline(&text, width - indent - 28.0, BODY_SIZE).flex(1.0),
                ])
                .gap(3.0)
            }
            Block::Quote(text) => Element::panel(
                Element::row(vec![
                    Element::spacer()
                        .size(Some(2.0), None)
                        .background(theme::FOREGROUND_SUBTLE),
                    render_inline(&text, width - 28.0, BODY_SIZE).flex(1.0),
                ])
                .gap(10.0),
            )
            .padding(8.0)
            .background(theme::PANEL)
            .radius(6.0),
            Block::Code(language, content) => code_block(language, content),
            Block::Rule => Element::spacer()
                .size(None, Some(1.0))
                .background(theme::BORDER),
        });
    }
    if truncated {
        elements.push(
            Element::text("Respuesta extensa: vista limitada a los primeros bloques.")
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(theme::FOREGROUND_SUBTLE),
        );
    }
    Element::column(elements).gap(9.0)
}

#[cfg(test)]
mod tests {
    use openide_ui::{DrawCommand, Size, UiTree};
    use unicode_segmentation::UnicodeSegmentation;

    use super::*;

    #[test]
    fn parses_blocks_in_order_and_keeps_number_and_checklist_state() {
        let (blocks, truncated) = parse(
            "# Plan\n\nIntro\n- Uno\n  - [x] Dos\n3. Tres\n> Nota\n```rust\nfn main() {}\n```\n---",
        );
        assert!(!truncated);
        assert!(matches!(&blocks[0], Block::Heading(1, value) if value == "Plan"));
        assert!(matches!(&blocks[1], Block::Paragraph(value) if value == "Intro"));
        assert!(
            matches!(&blocks[2], Block::ListItem(0, ListMarker::Bullet, value) if value == "Uno")
        );
        assert!(
            matches!(&blocks[3], Block::ListItem(1, ListMarker::Done, value) if value == "Dos")
        );
        assert!(
            matches!(&blocks[4], Block::ListItem(0, ListMarker::Number(number), value) if number == "3" && value == "Tres")
        );
        assert!(matches!(&blocks[5], Block::Quote(value) if value == "Nota"));
        assert!(
            matches!(&blocks[6], Block::Code(language, value) if language == "rust" && value == "fn main() {}")
        );
        assert_eq!(blocks[7], Block::Rule);
    }

    #[test]
    fn inline_emphasis_code_and_https_links_are_readable() {
        let runs =
            parse_inline("**Listo**: *revisa* `src/main.rs` y [docs](https://example.com/a).");
        assert_eq!(runs[0].kind, InlineKind::Strong);
        assert_eq!(runs[0].text, "Listo");
        assert!(runs
            .iter()
            .any(|run| run.kind == InlineKind::Emphasis && run.text == "revisa"));
        assert!(runs
            .iter()
            .any(|run| run.kind == InlineKind::Code && run.text == "src/main.rs"));
        let visible: String = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(
            visible,
            "Listo: revisa src/main.rs y docs (https://example.com/a)."
        );
    }

    #[test]
    fn unsafe_links_remain_literal_and_escapes_keep_unicode() {
        let source = "[click](javascript:alert(1)) \\*literal\\* áé 🧪";
        let visible: String = parse_inline(source)
            .iter()
            .map(|run| run.text.as_str())
            .collect();
        assert_eq!(visible, "[click](javascript:alert(1)) *literal* áé 🧪");
        assert!(!safe_url("file:///etc/passwd"));
        assert!(!safe_url("https://host/path\nother"));
    }

    #[test]
    fn hard_breaks_and_code_indentation_survive() {
        let (blocks, _) = parse("primera  \nsegunda\n\n```rust\n    let x = 1;\n```");
        assert_eq!(blocks[0], Block::Paragraph("primera\nsegunda".to_owned()));
        assert_eq!(
            blocks[1],
            Block::Code("rust".to_owned(), "    let x = 1;".to_owned())
        );
        let frame = UiTree::new(render_inline("primera\nsegunda", 400.0, BODY_SIZE))
            .layout(Size::new(400.0, 100.0));
        let positions: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { position, text, .. }
                    if text == "primera" || text == "segunda" =>
                {
                    Some(position.y)
                }
                _ => None,
            })
            .collect();
        assert_eq!(positions.len(), 2);
        assert!(positions[1] > positions[0]);
    }

    #[test]
    fn narrow_inline_markdown_uses_real_width_and_preserves_unicode() {
        let mut tree = UiTree::new(render_inline("i **WWW** `👩‍💻e\u{301}`", 22.0, BODY_SIZE));
        tree.set_text_measurer(|text, _, family| {
            text.graphemes(true)
                .map(|part| match (part, family) {
                    ("W", _) => 10.0,
                    ("👩‍💻", _) => 14.0,
                    _ => 3.0,
                })
                .sum()
        });
        let frame = tree.layout(Size::new(22.0, 160.0));
        let runs: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text {
                    position,
                    text,
                    font_family,
                    ..
                } => Some((position.y, text.as_str(), *font_family)),
                _ => None,
            })
            .collect();
        assert_eq!(
            runs.iter().map(|(_, text, _)| *text).collect::<String>(),
            "i WWW 👩‍💻e\u{301}"
        );
        assert!(runs
            .iter()
            .any(|(_, text, family)| text.contains("👩‍💻") && *family == FontFamily::Monospace));
        assert!(runs
            .iter()
            .any(|(y, text, _)| *y > 0.0 && text.contains('W')));
    }

    #[test]
    fn bounded_unicode_response_lays_out_without_markup_delimiters() {
        let markdown = "# Resumen\n\nSe corrigió **la sesión** en `src/main.rs`.\n- [x] Compila\n- [ ] Revisar [guía](https://example.com/docs)";
        let root = render_markdown_at_width(markdown, 360.0);
        let frame = UiTree::new(root).layout(Size::new(360.0, 400.0));
        let text: String = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(text.contains("la sesión"));
        assert!(text.contains("src/main.rs"));
        assert!(text.contains("https://example.com/docs"));
        assert!(!text.contains("**"));
        assert!(!text.contains("[guía]"));
    }

    #[test]
    fn source_and_block_limits_are_enforced() {
        let (blocks, truncated) = parse(&"x".repeat(MAX_SOURCE_CHARS + 3));
        assert!(truncated);
        assert_eq!(blocks.len(), 1);
        let many = "paragraph\n\n".repeat(MAX_BLOCKS + 5);
        let (blocks, truncated) = parse(&many);
        assert!(truncated);
        assert_eq!(blocks.len(), MAX_BLOCKS);
    }
}
