//! Bounded Markdown presentation for agent responses.
//!
//! The renderer builds an ordinary `openide-ui` element tree. It deliberately
//! supports a useful subset of Markdown instead of treating agent output as
//! trusted HTML. WarpUI's MIT formatted-text element informed the separation
//! between parsing, line layout, and painting; this is an original renderer.
//! The UI engine flows inline spans and decorative link icons with the host
//! font measurer. HTTP(S) links retain their destinations as scoped actions;
//! editable drafts keep their exact source and use paint-only highlighting.

use std::{
    cell::RefCell,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem::size_of,
    ops::Range,
    rc::Rc,
};

use openide_ui::{
    Button, ButtonVariant, Color, Component, ControlSize, Element, ElementKind, FontFamily, Icon,
    TextHighlight, TextSpan, Tooltip,
};

use crate::theme;

#[cfg(test)]
#[path = "rich_text_link_tests.rs"]
mod link_tests;

const MAX_SOURCE_CHARS: usize = 20_000;
const MAX_BLOCKS: usize = 160;
const MAX_SOURCE_ROW_CHARS: usize = 512;
const MAX_MARKUP_LOOKAHEAD: usize = 2_048;
const MAX_URL_BYTES: usize = 2_048;
const BODY_SIZE: f32 = 14.0;
const BODY_LINE_HEIGHT: f32 = 22.0;
const MARKDOWN_RENDER_VERSION: u16 = 4;
const MAX_CACHED_MESSAGES: usize = 64;
const MAX_CACHED_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_STREAMING_CACHE_BYTES: usize = 512 * 1_024;
const MAX_PENDING_CACHE_BYTES: usize = 512 * 1_024;

#[derive(Clone, Copy, Eq, PartialEq)]
struct MarkdownPresentation {
    width_bits: u32,
    renderer_version: u16,
    colors: [Color; 10],
}

impl MarkdownPresentation {
    fn at_width(width: f32) -> Self {
        Self {
            width_bits: width.to_bits(),
            renderer_version: MARKDOWN_RENDER_VERSION,
            colors: [
                theme::FOREGROUND,
                theme::BRAND,
                Color::rgb(194, 194, 194),
                theme::USER,
                theme::SELECTED,
                theme::FOREGROUND_SUBTLE,
                theme::DIFF_ADDED,
                theme::PANEL,
                theme::BORDER,
                theme::LINK,
            ],
        }
    }
}

struct MarkdownCacheEntry {
    source: String,
    truncated: bool,
    limited: bool,
    presentation: MarkdownPresentation,
    element: Element,
    heap_bytes: usize,
    blocks: Vec<Block>,
    incomplete_block: Option<usize>,
}

impl MarkdownCacheEntry {
    fn new(
        source: &str,
        truncated: bool,
        presentation: MarkdownPresentation,
        preview: &MarkdownPreview,
        blocks: &[Block],
        incomplete_block: Option<usize>,
    ) -> Option<Self> {
        // Cache bounded presentation trees. Their only actions contain the
        // immutable destination and a source hash; they do not refer to any
        // conversation, tool call, or mutable host resource.
        passive_heap_bytes(&preview.element)?;
        let source = source.to_owned();
        let element = preview.element.clone();
        // Account the stored clone's actual capacities, including child vector
        // storage, before deciding whether it fits the retained byte budget.
        let element_bytes = passive_heap_bytes(&element)?;
        let blocks = blocks.to_vec();
        let heap_bytes = source
            .capacity()
            .saturating_add(element_bytes)
            .saturating_add(blocks.capacity() * size_of::<Block>())
            .saturating_add(blocks.iter().map(Block::heap_bytes).sum::<usize>());
        Some(Self {
            source,
            truncated,
            limited: preview.limited,
            presentation,
            element,
            heap_bytes,
            blocks,
            incomplete_block,
        })
    }
}

struct MarkdownCache {
    // Oldest entry first. A small fixed-capacity list avoids hashing or retaining
    // an unbounded source string as a key, and source equality cannot collide.
    entries: Vec<MarkdownCacheEntry>,
    retained_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl MarkdownCache {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        let entries = Vec::with_capacity(max_entries);
        let retained_bytes = entries.capacity() * size_of::<MarkdownCacheEntry>();
        Self {
            entries,
            retained_bytes,
            max_entries,
            max_bytes,
        }
    }

    fn get(
        &mut self,
        source: &str,
        truncated: bool,
        presentation: MarkdownPresentation,
    ) -> Option<MarkdownPreview> {
        let index = self.entries.iter().position(|entry| {
            entry.source == source
                && entry.truncated == truncated
                && entry.presentation == presentation
        })?;
        let entry = self.entries.remove(index);
        let preview = MarkdownPreview {
            element: entry.element.clone(),
            limited: entry.limited,
        };
        self.entries.push(entry);
        Some(preview)
    }

    fn can_insert_without_eviction(&self, entry: &MarkdownCacheEntry) -> bool {
        self.entries.len() < self.max_entries
            && self.retained_bytes.saturating_add(entry.heap_bytes) <= self.max_bytes
    }

    fn insert_entry(&mut self, entry: MarkdownCacheEntry) {
        let heap_bytes = entry.heap_bytes;
        let storage_bytes = self.entries.capacity() * size_of::<MarkdownCacheEntry>();
        if storage_bytes.saturating_add(heap_bytes) > self.max_bytes {
            return;
        }
        while self.entries.len() >= self.max_entries
            || self.retained_bytes.saturating_add(heap_bytes) > self.max_bytes
        {
            let entry = self.entries.remove(0);
            self.retained_bytes -= entry.heap_bytes;
        }
        self.retained_bytes += heap_bytes;
        self.entries.push(entry);
    }

    fn take_latest(&mut self) -> Option<MarkdownCacheEntry> {
        let entry = self.entries.pop()?;
        self.retained_bytes -= entry.heap_bytes;
        Some(entry)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.retained_bytes = self.entries.capacity() * size_of::<MarkdownCacheEntry>();
    }
}

struct MarkdownCaches {
    completed: MarkdownCache,
    streaming: MarkdownCache,
    pending_completed: MarkdownCache,
    frame_depth: usize,
    frame_context: Option<(u64, MarkdownPresentation)>,
}

impl MarkdownCaches {
    fn new() -> Self {
        Self {
            completed: MarkdownCache::new(MAX_CACHED_MESSAGES, MAX_CACHED_BYTES),
            // A changing response replaces its previous version instead of
            // evicting completed messages from their LRU cache.
            streaming: MarkdownCache::new(2, MAX_STREAMING_CACHE_BYTES),
            // Defer one admission until the transcript finishes its forward
            // scan. Older misses cannot evict cached rows before they are read.
            pending_completed: MarkdownCache::new(1, MAX_PENDING_CACHE_BYTES),
            frame_depth: 0,
            frame_context: None,
        }
    }

    fn get(
        &mut self,
        source: &str,
        truncated: bool,
        presentation: MarkdownPresentation,
        streaming: bool,
    ) -> Option<MarkdownPreview> {
        if streaming {
            self.streaming.get(source, truncated, presentation)
        } else {
            self.pending_completed
                .get(source, truncated, presentation)
                .or_else(|| self.completed.get(source, truncated, presentation))
        }
    }

    fn insert(&mut self, entry: MarkdownCacheEntry, streaming: bool) {
        let cache = if streaming {
            &mut self.streaming
        } else if self.frame_depth > 0 && !self.completed.can_insert_without_eviction(&entry) {
            &mut self.pending_completed
        } else {
            &mut self.completed
        };
        cache.insert_entry(entry);
    }

    fn finish_frame(&mut self) {
        self.frame_depth = self.frame_depth.saturating_sub(1);
        if self.frame_depth == 0 {
            if let Some(entry) = self.pending_completed.take_latest() {
                self.completed.insert_entry(entry);
            }
        }
    }
}

thread_local! {
    // Session presentation happens on the UI thread. No shared mutex is held
    // during frame construction; another UI thread has its own bounded cache.
    static MARKDOWN_CACHES: RefCell<MarkdownCaches> = RefCell::new(MarkdownCaches::new());
}

/// Defer completed-cache eviction during a transcript scan. Without this guard,
/// a conversation larger than the cache could evict every upcoming hit while
/// rebuilding its older rows. At most one bounded candidate waits for admission.
pub struct MarkdownFrame {
    _thread: PhantomData<Rc<()>>,
}

impl Drop for MarkdownFrame {
    fn drop(&mut self) {
        MARKDOWN_CACHES.with(|caches| caches.borrow_mut().finish_frame());
    }
}

#[must_use]
pub fn begin_markdown_frame(session: Option<&str>, width: f32) -> MarkdownFrame {
    let mut session_hash = DefaultHasher::new();
    session.hash(&mut session_hash);
    // This hash only decides when to clear an old frame's cache. Each actual
    // hit still checks the bounded source and full presentation for equality.
    let context = (
        session_hash.finish(),
        MarkdownPresentation::at_width(normalized_width(width)),
    );
    MARKDOWN_CACHES.with(|caches| {
        let mut caches = caches.borrow_mut();
        if caches.frame_depth == 0 && caches.frame_context != Some(context) {
            caches.completed.clear();
            caches.streaming.clear();
            caches.pending_completed.clear();
            caches.frame_context = Some(context);
        }
        caches.frame_depth = caches.frame_depth.saturating_add(1);
    });
    MarkdownFrame {
        _thread: PhantomData,
    }
}

/// Heap retained by the passive subset generated below. Root storage is part
/// of MarkdownCacheEntry; child capacity and owned string capacity are counted.
/// Unknown or interactive kinds are deliberately ineligible for caching.
fn passive_heap_bytes(element: &Element) -> Option<usize> {
    if element.hover_peer.is_some()
        || element.hover_surface.is_some()
        || element.action_alias.is_some()
        || element.menu_navigation
    {
        return None;
    }
    match &element.kind {
        ElementKind::Column(children) | ElementKind::Row(children) => {
            let mut bytes = children.capacity().saturating_mul(size_of::<Element>());
            for child in children {
                bytes = bytes.saturating_add(passive_heap_bytes(child)?);
            }
            Some(bytes)
        }
        ElementKind::Panel(child) => {
            Some(size_of::<Element>().saturating_add(passive_heap_bytes(child)?))
        }
        ElementKind::Text(text) => Some(text.capacity()),
        ElementKind::FormattedText(spans) => Some(spans.iter().fold(
            spans.capacity().saturating_mul(size_of::<TextSpan>()),
            |bytes, span| {
                bytes
                    .saturating_add(span.text.capacity())
                    .saturating_add(span.action.as_ref().map_or(0, String::capacity))
            },
        )),
        ElementKind::Spacer => Some(0),
        _ => None,
    }
}

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

impl Block {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Heading(_, text) | Self::Paragraph(text) | Self::Quote(text) => text.capacity(),
            Self::ListItem(_, marker, text) => {
                text.capacity()
                    + match marker {
                        ListMarker::Number(number) => number.capacity(),
                        _ => 0,
                    }
            }
            Self::Code(language, content) => language.capacity() + content.capacity(),
            Self::Rule => 0,
        }
    }
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
    destination: Option<String>,
}

fn push_run(runs: &mut Vec<InlineRun>, kind: InlineKind, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs
        .last_mut()
        .filter(|last| last.kind == kind && last.destination.is_none())
    {
        last.text.push_str(text);
    } else {
        runs.push(InlineRun {
            kind,
            text: text.to_owned(),
            destination: None,
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

fn bounded_source(source: &str) -> (&str, bool) {
    match source.char_indices().nth(MAX_SOURCE_CHARS) {
        Some((end, _)) => (&source[..end], true),
        None => (source, false),
    }
}

#[cfg(test)]
fn parse(source: &str) -> (Vec<Block>, bool) {
    let (source, truncated) = bounded_source(source);
    parse_bounded(source, truncated)
}

#[derive(Clone, Copy)]
struct CodeFence {
    marker: u8,
    length: usize,
}

fn fence_line(line: &str) -> Option<(CodeFence, &str)> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let body = &line[indent..];
    let marker = *body.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = body.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then(|| (CodeFence { marker, length }, &body[length..]))
}

fn pending_fence_line(line: &str) -> bool {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    indent <= 3 && matches!(line[indent..].trim_end(), "`" | "``" | "~" | "~~")
}

#[cfg(test)]
fn parse_bounded(source: &str, truncated: bool) -> (Vec<Block>, bool) {
    parse_bounded_mode(source, truncated, false)
}

fn parse_bounded_mode(source: &str, mut truncated: bool, streaming: bool) -> (Vec<Block>, bool) {
    let mut blocks = Vec::new();
    let mut paragraph = Vec::new();
    let mut code = Vec::new();
    let mut language = String::new();
    let mut fence: Option<CodeFence> = None;
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if blocks.len() >= MAX_BLOCKS {
            truncated = true;
            break;
        }
        let trimmed = line.trim_end();
        if let Some(open) = fence {
            let closes = fence_line(trimmed).is_some_and(|(close, suffix)| {
                close.marker == open.marker
                    && close.length >= open.length
                    && suffix.trim().is_empty()
            });
            if closes {
                blocks.push(Block::Code(std::mem::take(&mut language), code.join("\n")));
                code.clear();
                fence = None;
            } else {
                code.push(line.to_owned());
            }
            continue;
        }
        if let Some((opening, info)) = fence_line(trimmed) {
            // Backticks in an info string make this an ordinary text line,
            // and another language/fence inside a code block is code itself.
            if opening.marker != b'`' || !info.contains('`') {
                flush_paragraph(&mut blocks, &mut paragraph);
                language = info
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(24)
                    .collect();
                fence = Some(opening);
                continue;
            }
        }
        // A delimiter arriving one token at a time must not flash as prose
        // just before it becomes a fence. Only the unfinished final line is
        // provisional; final answers always retain literal source semantics.
        if streaming
            && !source.ends_with('\n')
            && lines.peek().is_none()
            && pending_fence_line(line)
        {
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
    if fence.is_some() {
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

fn web_url(value: &str) -> Option<reqwest::Url> {
    if value.len() > MAX_URL_BYTES
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch == '\\')
    {
        return None;
    }
    let url = reqwest::Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

fn safe_url(value: &str) -> bool {
    web_url(value).is_some()
}

fn link_icon(url: &reqwest::Url) -> Icon {
    match url.host_str() {
        Some("github.com" | "www.github.com") => Icon::Github,
        _ => Icon::Globe,
    }
}

fn source_hash(source: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    source.hash(&mut hash);
    hash.finish()
}

fn link_action(source: &str, offset: usize, destination: &str) -> String {
    link_action_with_hash(source_hash(source), offset, destination)
}

fn link_action_with_hash(scope: u64, offset: usize, destination: &str) -> String {
    format!("chat_link_open:{scope:x}:{offset}:{destination}")
}

/// Only web navigation is carried by this action. It never resolves local
/// files, executes a scheme or infers a destination from a visible label.
pub(crate) fn web_link_url_from_action(action: &str) -> Option<String> {
    let mut parts = action.strip_prefix("chat_link_open:")?.splitn(3, ':');
    u64::from_str_radix(parts.next()?, 16).ok()?;
    parts.next()?.parse::<usize>().ok()?;
    Some(web_url(parts.next()?)?.to_string())
}

/// Cached presentation is independent of a conversation. Give each cloned
/// instance distinct focus/hover targets, including repeated identical blocks.
pub(crate) fn scope_web_links(mut element: Element, scope: &str) -> Element {
    fn visit(element: &mut Element, scope: u64, ordinal: &mut usize) {
        match &mut element.kind {
            ElementKind::Column(children) | ElementKind::Row(children) => {
                for child in children {
                    visit(child, scope, ordinal);
                }
            }
            ElementKind::Panel(child) => visit(child, scope, ordinal),
            ElementKind::FormattedText(spans) => {
                for span in spans {
                    if let Some(url) = span.action.as_deref().and_then(web_link_url_from_action) {
                        span.action = Some(link_action_with_hash(scope, *ordinal, &url));
                        *ordinal += 1;
                    }
                }
            }
            _ => {}
        }
    }
    visit(&mut element, source_hash(scope), &mut 0);
    element
}

fn url_token_len(value: &str) -> usize {
    let mut end = 0;
    let mut balance = [0i32; 3];
    for ch in value.chars() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | '`') {
            break;
        }
        end += ch.len_utf8();
        if end > MAX_URL_BYTES {
            return 0;
        }
        match ch {
            '(' => balance[0] += 1,
            ')' => balance[0] -= 1,
            '[' => balance[1] += 1,
            ']' => balance[1] -= 1,
            '{' => balance[2] += 1,
            '}' => balance[2] -= 1,
            _ => {}
        }
    }
    let mut candidate = &value[..end];
    loop {
        let Some(last) = candidate.chars().last() else {
            break;
        };
        let closing = match last {
            ')' => Some(0),
            ']' => Some(1),
            '}' => Some(2),
            _ => None,
        };
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?')
            || closing.is_some_and(|index| balance[index] < 0)
        {
            if let Some(index) = closing {
                balance[index] += 1;
            }
            candidate = &candidate[..candidate.len() - last.len_utf8()];
        } else {
            break;
        }
    }
    candidate.len()
}

fn begins_web_url(value: &str) -> bool {
    value
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        || value
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
}

fn web_url_boundary(previous: Option<char>) -> bool {
    previous.is_none_or(|ch| !ch.is_alphanumeric() && !matches!(ch, '_' | '/' | '\\'))
}

/// Character ranges refer to the unchanged editable source, including any
/// preceding emoji or Markdown syntax. Work is bounded independently of a
/// potentially large pasted draft, and every URL is parsed without a fetch.
fn composer_web_links(source: &str) -> Vec<(Range<usize>, reqwest::Url)> {
    let (scan_source, _) = bounded_source(source);
    let mut links = Vec::new();
    let mut cursor = scan_source.char_indices().enumerate().peekable();
    let mut previous = None;
    while let Some((offset, (byte, ch))) = cursor.next() {
        if web_url_boundary(previous) && begins_web_url(&source[byte..]) {
            let length = url_token_len(&source[byte..]);
            if let Some(url) = (length > 0)
                .then(|| web_url(&source[byte..byte + length]))
                .flatten()
            {
                let end = offset + source[byte..byte + length].chars().count();
                links.push((offset..end, url));
                while cursor
                    .peek()
                    .is_some_and(|(_, (next, _))| *next < byte + length)
                {
                    previous = cursor.next().map(|(_, (_, ch))| ch);
                }
                if links.len() >= 64 {
                    break;
                }
                continue;
            }
        }
        previous = Some(ch);
    }
    links
}

/// Compact link affordances are separate from the editor's immutable source
/// geometry: the URL stays editable and paint-only color adds no glyph width.
pub(crate) fn composer_links(source: &str, width: f32) -> (Vec<TextHighlight>, Option<Element>) {
    let links = composer_web_links(source);
    let highlights = links
        .iter()
        .map(|(range, _)| TextHighlight {
            range: range.clone(),
            color: theme::LINK,
        })
        .collect();
    if links.is_empty() || !width.is_finite() || width < 48.0 {
        return (highlights, None);
    }
    let mut destinations = Vec::new();
    for (_, url) in links {
        if !destinations.contains(&url) {
            destinations.push(url);
        }
    }
    let count = destinations.len().min(if width < 240.0 { 1 } else { 2 });
    let item_width = ((width - 4.0 * count.saturating_sub(1) as f32) / count as f32).min(280.0);
    let palette = theme::action_palette(theme::INPUT, theme::LINK);
    let items = destinations
        .iter()
        .take(count)
        .enumerate()
        .map(|(index, url)| {
            let label = url
                .as_str()
                .split_once("://")
                .map_or(url.as_str(), |(_, label)| label);
            let action = link_action("composer", index, url.as_str());
            let button = Button::new(action.clone(), label, palette)
                .icon(link_icon(url))
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Dense)
                .width(item_width)
                .render()
                .font_size(12.0)
                .radius(6.0)
                .size(Some(item_width), Some(24.0));
            Tooltip::new(
                button,
                format!("{action}:tooltip"),
                url.as_str(),
                theme::component_palette(),
            )
            .render()
        })
        .collect();
    (
        highlights,
        Some(Element::row(items).gap(4.0).size(Some(width), Some(24.0))),
    )
}

/// User messages preserve literal Markdown and URLs. Only complete HTTP(S)
/// destinations receive link styling; file paths and code remain unchanged.
pub(crate) fn render_user_message(source: &str, scope: &str) -> Element {
    let mut spans = Vec::new();
    let mut byte = 0;
    let mut character = 0;
    let hash = source_hash(scope);
    for (index, (range, url)) in composer_web_links(source).into_iter().enumerate() {
        let before = source[byte..]
            .char_indices()
            .nth(range.start - character)
            .map_or(source.len(), |(offset, _)| byte + offset);
        let end = source[before..]
            .char_indices()
            .nth(range.end - range.start)
            .map_or(source.len(), |(offset, _)| before + offset);
        if before > byte {
            spans.push(TextSpan::new(
                &source[byte..before],
                theme::FOREGROUND,
                BODY_SIZE,
                FontFamily::Sans,
            ));
        }
        spans.push(
            TextSpan::new(
                &source[before..end],
                theme::LINK,
                BODY_SIZE,
                FontFamily::Sans,
            )
            .icon(link_icon(&url))
            .action(link_action_with_hash(hash, index, url.as_str())),
        );
        byte = end;
        character = range.end;
    }
    if byte < source.len() {
        spans.push(TextSpan::new(
            &source[byte..],
            theme::FOREGROUND,
            BODY_SIZE,
            FontFamily::Sans,
        ));
    }
    Element::formatted_text(spans).line_height(21.0)
}

fn markdown_destination_end(value: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, ch) in bounded_lookahead(value).char_indices() {
        match ch {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(index),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn flush_plain(runs: &mut Vec<InlineRun>, plain: &mut String) {
    push_run(runs, InlineKind::Normal, plain);
    plain.clear();
}

#[cfg(test)]
fn parse_inline(source: &str) -> Vec<InlineRun> {
    parse_inline_mode(source, false)
}

#[cfg(test)]
thread_local! {
    static INLINE_PARSE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn parse_inline_mode(source: &str, incomplete: bool) -> Vec<InlineRun> {
    parse_inline_with_tail(source, incomplete, false)
}

fn parse_inline_with_tail(
    source: &str,
    incomplete: bool,
    url_tail_complete: bool,
) -> Vec<InlineRun> {
    #[cfg(test)]
    INLINE_PARSE_COUNT.with(|count| count.set(count.get() + 1));
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
        let delimiter = if rest.starts_with('`') {
            let count = rest.bytes().take_while(|byte| *byte == b'`').count();
            Some((&rest[..count], InlineKind::Code))
        } else if rest.starts_with("**") {
            Some(("**", InlineKind::Strong))
        } else if rest.starts_with('*') {
            Some(("*", InlineKind::Emphasis))
        } else {
            None
        };
        if let Some((delimiter, kind)) = delimiter {
            let body = &rest[delimiter.len()..];
            let lookahead = bounded_lookahead(body);
            if let Some(end) = lookahead.find(delimiter).filter(|end| *end > 0) {
                flush_plain(&mut runs, &mut plain);
                push_run(&mut runs, kind, &body[..end]);
                index += end + 2 * delimiter.len();
                continue;
            }
            if incomplete
                && lookahead.len() == body.len()
                && (body.is_empty() || !body.starts_with(char::is_whitespace))
            {
                flush_plain(&mut runs, &mut plain);
                // A single star can be the first half of a closing ** pair.
                let body = if delimiter == "**" {
                    body.strip_suffix('*').unwrap_or(body)
                } else {
                    body
                };
                push_run(&mut runs, kind, body);
                break;
            }
            if kind == InlineKind::Code {
                // Treat an unmatched delimiter run atomically. Rechecking a
                // 20,000-backtick run at each following byte is quadratic.
                plain.push_str(delimiter);
                index += delimiter.len();
                continue;
            }
        }
        if let Some(body) = rest.strip_prefix('[') {
            if let Some(label_end) = bounded_lookahead(body).find("](") {
                let tail = &body[label_end + 2..];
                if let Some(url_end) = markdown_destination_end(tail) {
                    let url = &tail[..url_end];
                    if !body[..label_end].is_empty() && safe_url(url) {
                        flush_plain(&mut runs, &mut plain);
                        runs.push(InlineRun {
                            kind: InlineKind::Link,
                            text: body[..label_end].to_owned(),
                            destination: Some(url.to_owned()),
                        });
                        index += label_end + url_end + 4;
                        continue;
                    }
                }
            }
        }
        if begins_web_url(rest) && web_url_boundary(source[..index].chars().next_back()) {
            let length = url_token_len(rest);
            let destination = &rest[..length];
            if length > 0 && safe_url(destination) {
                flush_plain(&mut runs, &mut plain);
                // A live token can still grow into a different host or path.
                // Color is provisional; navigation requires a delimiter or a
                // completed response, so github.co cannot open before its m.
                let after_token = rest[length..]
                    .chars()
                    .find(|ch| !matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}'));
                let complete = !incomplete
                    || url_tail_complete
                    || after_token.is_some_and(|ch| {
                        ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | '`')
                    });
                runs.push(InlineRun {
                    kind: InlineKind::Link,
                    text: destination.to_owned(),
                    destination: complete.then(|| destination.to_owned()),
                });
                index += length;
                continue;
            }
        }
        if incomplete {
            if let Some(body) = rest.strip_prefix('[') {
                let bounded = bounded_lookahead(body);
                if bounded.len() == body.len() {
                    let provisional_label = if let Some(label_end) = body.find("](") {
                        (!body[label_end + 2..].contains(')')).then_some(&body[..label_end])
                    } else if let Some(label) = body.strip_suffix(']') {
                        Some(label)
                    } else if !body.contains(']') {
                        Some(body)
                    } else {
                        None
                    };
                    if let Some(label) = provisional_label {
                        flush_plain(&mut runs, &mut plain);
                        push_run(&mut runs, InlineKind::Link, label);
                        break;
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
        InlineKind::Strong => theme::BRAND,
        InlineKind::Link => theme::LINK,
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

#[cfg(test)]
fn render_inline(source: &str, width: f32, size: f32) -> Element {
    render_inline_mode(source, width, size, false, false)
}

fn render_inline_mode(
    source: &str,
    width: f32,
    size: f32,
    incomplete: bool,
    url_tail_complete: bool,
) -> Element {
    let hash = source_hash(source);
    let spans = parse_inline_with_tail(source, incomplete, url_tail_complete)
        .iter()
        .enumerate()
        .map(|(index, run)| {
            let span = span_element(run, size);
            match run.destination.as_deref().and_then(web_url) {
                Some(url) => span.icon(link_icon(&url)).action(link_action_with_hash(
                    hash,
                    index,
                    url.as_str(),
                )),
                None => span,
            }
        })
        .collect();
    Element::formatted_text(spans)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .line_height(if size == BODY_SIZE {
            BODY_LINE_HEIGHT
        } else {
            size * 1.4
        })
        .size(Some(width.max(0.0)), None)
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

/// Render with the actual reading-column width in logical pixels. HTTP(S)
/// links retain their URI; the host opens them only after an explicit action.
/// Completed messages reuse a bounded presentation tree until their displayed
/// content, width or presentation changes. The host still performs layout.
pub fn render_markdown_at_width(source: &str, width: f32) -> Element {
    render_markdown_preview_at_width(source, width).element
}

/// Keep at most two live trees in a shared bounded pool, so reasoning and
/// assistant text can coexist without churning the completed-message cache.
/// An unchanged live prefix can also reuse its previous tree.
pub fn render_streaming_markdown_at_width(source: &str, width: f32) -> Element {
    render_streaming_markdown_preview_at_width(source, width).element
}

/// A bounded Markdown preview. The host scopes URI actions after cloning with
/// `scope_web_links`; resource-mutating controls remain outside the cache.
/// A message-scoped continuation action may accompany a limited preview.
pub struct MarkdownPreview {
    pub element: Element,
    pub limited: bool,
}

pub fn render_markdown_preview_at_width(source: &str, width: f32) -> MarkdownPreview {
    render_cached_markdown(source, width, false)
}

pub fn render_streaming_markdown_preview_at_width(source: &str, width: f32) -> MarkdownPreview {
    render_cached_markdown(source, width, true)
}

/// One exact slice of the original Markdown source. Pagination limits only
/// the presentation and never modifies, drops, or parses the retained source.
/// Cursors are UTF-8 byte offsets into the original message, not character
/// counts or offsets into an independently copied page.
pub struct MarkdownSourcePage<'a> {
    pub text: &'a str,
    pub start_byte: usize,
    pub end_byte: usize,
    pub previous_byte: Option<usize>,
    pub next_byte: Option<usize>,
}

/// Return at most 20,000 Unicode scalar values. Invalid or stale offsets reset
/// to the beginning instead of slicing through a UTF-8 character. Each cursor
/// calculation scans at most one page, even for a very large message.
pub fn markdown_source_page(source: &str, start_byte: usize) -> MarkdownSourcePage<'_> {
    let start_byte = if source.get(start_byte..).is_some() {
        start_byte
    } else {
        0
    };
    let (text, _) = bounded_source(&source[start_byte..]);
    let end_byte = start_byte + text.len();
    let previous_byte = (start_byte > 0).then(|| {
        source[..start_byte]
            .char_indices()
            .rev()
            .nth(MAX_SOURCE_CHARS - 1)
            .map_or(0, |(offset, _)| offset)
    });
    MarkdownSourcePage {
        text,
        start_byte,
        end_byte,
        previous_byte,
        next_byte: (end_byte < source.len()).then_some(end_byte),
    }
}

/// Present source literally with wrapping and no Markdown block limit. Split
/// each bounded page into small passive rows so a single long paragraph or
/// code line cannot create a 20,000-character layout block. The modal host
/// supplies the scroll viewport; this function never retains the full source.
pub fn render_markdown_source_page_at_width(page: &MarkdownSourcePage<'_>, width: f32) -> Element {
    let width = normalized_width(width);
    let mut rows = Vec::new();
    let (mut remaining, _) = bounded_source(page.text);
    while !remaining.is_empty() {
        let end = remaining
            .char_indices()
            .nth(MAX_SOURCE_ROW_CHARS)
            .map_or(remaining.len(), |(offset, _)| offset);
        let mut text = &remaining[..end];
        remaining = &remaining[end..];
        // The boundary between two rows already supplies a visual line break.
        // Avoid displaying a second empty line when a chunk ends at a newline.
        // The exact source, including that newline, remains in `page.text`.
        if !remaining.is_empty() {
            text = text
                .strip_suffix("\r\n")
                .or_else(|| text.strip_suffix('\n'))
                .unwrap_or(text);
        }
        rows.push(
            Element::text(text)
                .font_family(FontFamily::Monospace)
                .font_size(12.0)
                .foreground(theme::FOREGROUND)
                .size(Some(width), None),
        );
    }
    Element::column(rows).gap(0.0).size(Some(width), None)
}

fn render_cached_markdown(source: &str, width: f32, streaming: bool) -> MarkdownPreview {
    let width = normalized_width(width);
    let (source, truncated) = bounded_source(source);
    let presentation = MarkdownPresentation::at_width(width);
    let cached = MARKDOWN_CACHES.with(|caches| {
        caches
            .borrow_mut()
            .get(source, truncated, presentation, streaming)
    });
    if let Some(element) = cached {
        return element;
    }
    let (blocks, limited) = parse_bounded_mode(source, truncated, streaming);
    let pending_fence =
        !source.ends_with('\n') && source.lines().next_back().is_some_and(pending_fence_line);
    let incomplete_block =
        (streaming && !limited && !pending_fence && !ends_with_blank_line(source))
            .then(|| blocks.len().checked_sub(1))
            .flatten()
            .filter(|index| !matches!(blocks[*index], Block::Code(..) | Block::Rule));
    let mut preview = MARKDOWN_CACHES.with(|caches| {
        let caches = caches.borrow();
        let previous = streaming
            .then(|| {
                caches.streaming.entries.iter().rev().find(|entry| {
                    entry.presentation == presentation && source.starts_with(&entry.source)
                })
            })
            .flatten();
        render_markdown_blocks(
            &blocks,
            width,
            limited,
            incomplete_block,
            source.ends_with(char::is_whitespace),
            previous,
        )
    });
    if truncated && !source.ends_with(char::is_whitespace) {
        suspend_truncated_tail_link(&mut preview.element, source);
    }
    // Completed rows already have an exact-source tree; only live rows
    // need block metadata to reconcile the next append.
    let retained_blocks = if streaming { blocks.as_slice() } else { &[] };
    if let Some(entry) = MarkdownCacheEntry::new(
        source,
        truncated,
        presentation,
        &preview,
        retained_blocks,
        incomplete_block,
    ) {
        MARKDOWN_CACHES.with(|caches| caches.borrow_mut().insert(entry, streaming));
    }
    preview
}

/// A presentation limit must never turn a URL prefix into a different target.
/// Keep the bounded text readable, but wait for the full-source view to open it.
fn suspend_truncated_tail_link(element: &mut Element, source: &str) {
    match &mut element.kind {
        ElementKind::Column(children) | ElementKind::Row(children) => {
            for child in children {
                suspend_truncated_tail_link(child, source);
            }
        }
        ElementKind::Panel(child) => suspend_truncated_tail_link(child, source),
        ElementKind::FormattedText(spans) => {
            let mut suffix = String::new();
            for span in spans.iter_mut().rev() {
                suffix.insert_str(0, &span.text);
                if begins_web_url(&span.text) && span.action.is_some() {
                    if source.ends_with(&suffix) {
                        span.action = None;
                        span.icon = None;
                    }
                    break;
                }
                if !span.text.chars().all(|ch| ch.is_ascii_punctuation()) {
                    break;
                }
            }
        }
        _ => {}
    }
}

fn normalized_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(48.0, 1_200.0)
    } else {
        600.0
    }
}

fn ends_with_blank_line(source: &str) -> bool {
    let mut lines = source.lines().rev();
    lines.next().is_some_and(|line| line.trim().is_empty())
}

fn block_spacing(previous: Option<&Block>, current: &Block) -> f32 {
    match (previous, current) {
        (None, _) => 0.0,
        (_, Block::Heading(..)) => 20.0,
        (Some(Block::Heading(..)), _) => 8.0,
        (Some(Block::ListItem(..)), Block::ListItem(..)) => 4.0,
        _ => 14.0,
    }
}

fn render_markdown_blocks(
    blocks: &[Block],
    width: f32,
    truncated: bool,
    incomplete_block: Option<usize>,
    url_tail_complete: bool,
    previous: Option<&MarkdownCacheEntry>,
) -> MarkdownPreview {
    let mut elements = Vec::with_capacity(blocks.len() + usize::from(truncated));
    for (index, block) in blocks.iter().enumerate() {
        let incomplete = incomplete_block == Some(index);
        let spacing = block_spacing(index.checked_sub(1).and_then(|i| blocks.get(i)), block);
        // The owned UI tree still participates in native layout. Reusing each
        // unchanged block avoids reparsing inline markup above the stream tail.
        let reused = previous.and_then(|entry| {
            if entry.blocks.get(index) != Some(block)
                || (entry.incomplete_block == Some(index)) != incomplete
                || (incomplete && entry.source.ends_with(char::is_whitespace) != url_tail_complete)
                || block_spacing(
                    index.checked_sub(1).and_then(|i| entry.blocks.get(i)),
                    block,
                ) != spacing
            {
                return None;
            }
            let ElementKind::Column(children) = &entry.element.kind else {
                return None;
            };
            children.get(index).cloned()
        });
        if let Some(element) = reused {
            elements.push(element);
            continue;
        }
        let element = match block {
            Block::Heading(level, text) => {
                let size = match level {
                    1 => 22.0,
                    2 => 18.0,
                    _ => 16.0,
                };
                render_inline_mode(text, width, size, incomplete, url_tail_complete)
            }
            Block::Paragraph(text) => {
                render_inline_mode(text, width, BODY_SIZE, incomplete, url_tail_complete)
            }
            Block::ListItem(depth, marker, text) => {
                let (label, color) = match marker {
                    ListMarker::Bullet => ("•".to_owned(), theme::FOREGROUND_SUBTLE),
                    ListMarker::Number(number) => (format!("{number}."), theme::FOREGROUND_SUBTLE),
                    ListMarker::Done => ("✓".to_owned(), theme::DIFF_ADDED),
                    ListMarker::Todo => ("○".to_owned(), theme::FOREGROUND_SUBTLE),
                };
                let indent = (*depth as f32 * 14.0).min((width - 48.0).max(0.0));
                Element::row(vec![
                    Element::spacer().size(Some(indent), Some(1.0)),
                    Element::formatted_text(vec![TextSpan::new(
                        label,
                        color,
                        BODY_SIZE,
                        FontFamily::Sans,
                    )])
                    .font_family(FontFamily::Sans)
                    .font_size(BODY_SIZE)
                    .line_height(BODY_LINE_HEIGHT)
                    .foreground(color)
                    .size(Some(22.0), None),
                    render_inline_mode(
                        text,
                        width - indent - 28.0,
                        BODY_SIZE,
                        incomplete,
                        url_tail_complete,
                    )
                    .size(None, None)
                    .flex(1.0),
                ])
                .gap(3.0)
            }
            Block::Quote(text) => Element::panel(
                Element::row(vec![
                    Element::spacer()
                        .size(Some(2.0), None)
                        .background(theme::FOREGROUND_SUBTLE),
                    render_inline_mode(
                        text,
                        width - 28.0,
                        BODY_SIZE,
                        incomplete,
                        url_tail_complete,
                    )
                    .size(None, None)
                    .flex(1.0),
                ])
                .gap(10.0),
            )
            .padding(8.0)
            .background(theme::PANEL)
            .radius(6.0),
            Block::Code(language, content) => code_block(language.clone(), content.clone()),
            Block::Rule => Element::spacer()
                .size(None, Some(1.0))
                .background(theme::BORDER),
        };
        // One root per source block keeps streaming reconciliation stable.
        // List rows stay compact while prose and headings have their own rhythm.
        elements.push(Element::column(vec![
            Element::spacer().size(None, Some(spacing)),
            element,
        ]));
    }
    if truncated {
        elements.push(
            Element::text("Respuesta extensa: vista limitada a los primeros bloques.")
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(theme::FOREGROUND_SUBTLE),
        );
    }
    MarkdownPreview {
        element: Element::column(elements),
        limited: truncated,
    }
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
        assert_eq!(visible, "Listo: revisa src/main.rs y docs.");
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
    fn prose_has_reading_leading_and_distinct_paragraph_list_and_heading_spacing() {
        let source = "First line  \nsecond line\n\nAnother paragraph\n\n- first item\n- second item\n\n## Next section\n\nFinal paragraph";
        for streaming in [false, true] {
            let root = render_cached_markdown(source, 320.0, streaming).element;
            let frame = UiTree::new(root).layout(Size::new(320.0, 800.0));
            let blocks = frame.text_layouts();
            let first = blocks
                .iter()
                .find(|block| block.source.starts_with("First line"))
                .unwrap();
            assert_eq!(first.runs[0].rect.height, BODY_LINE_HEIGHT);
            assert_eq!(
                first.runs[1].rect.y - first.runs[0].rect.y,
                BODY_LINE_HEIGHT
            );
            let top = |source: &str| {
                blocks
                    .iter()
                    .find(|block| block.source == source)
                    .unwrap()
                    .runs[0]
                    .rect
                    .y
            };
            let bottom = |source: &str| {
                let block = blocks.iter().find(|block| block.source == source).unwrap();
                let run = block.runs.last().unwrap();
                run.rect.y + run.rect.height
            };
            assert!(top("Another paragraph") - bottom("First line\nsecond line") >= 14.0);
            assert_eq!(top("second item") - bottom("first item"), 4.0);
            let paint_y = |text: &str| {
                frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Text {
                            position,
                            text: value,
                            ..
                        } if value == text => Some(position.y),
                        _ => None,
                    })
                    .unwrap()
            };
            assert_eq!(paint_y("•"), paint_y("first item"));
            assert!(top("Next section") - bottom("second item") >= 20.0);
            assert!(top("Final paragraph") - bottom("Next section") >= 8.0);
        }
    }

    #[test]
    fn resized_list_items_and_quotes_reserve_every_wrapped_line() {
        let markdown = "- Primero: conservar el orden de cada actualización y la respuesta completa con acentos.\n- Segundo: mostrar los archivos y las herramientas sin solapar las líneas del párrafo.\n\n> Tercero: esta cita también debe calcular su altura con el ancho real disponible.\n\n- Cuarto: el siguiente bloque permanece debajo del contenido anterior.";
        // The reading width is an upper bound. Scrollbars, dock animation and
        // parent flex layout can allocate a narrower viewport in this frame.
        let mut tree = UiTree::new(render_markdown_at_width(markdown, 480.0));
        tree.set_text_measurer(|text, _, _| text.graphemes(true).count() as f32 * 6.5);
        for width in [480.0, 220.0, 360.0, 140.0, 480.0] {
            let frame = tree.layout(Size::new(width, 1600.0));
            let blocks: Vec<_> = frame
                .text_layouts()
                .into_iter()
                .filter(|block| {
                    ["Primero:", "Segundo:", "Tercero:", "Cuarto:"]
                        .iter()
                        .any(|prefix| block.source.starts_with(prefix))
                })
                .collect();
            assert_eq!(blocks.len(), 4);
            for pair in blocks.windows(2) {
                let bottom = pair[0]
                    .runs
                    .iter()
                    .map(|run| run.rect.y + run.rect.height)
                    .fold(0.0_f32, f32::max);
                let next_top = pair[1].runs.first().unwrap().rect.y;
                assert!(
                    next_top >= bottom,
                    "overlapping Markdown blocks at {width}px: {bottom} > {next_top}"
                );
            }
            if width <= 220.0 {
                assert!(blocks[0].runs.len() > 1);
            }
        }
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
        assert!(text.contains("guía"));
        assert!(!text.contains("https://example.com/docs"));
        assert!(frame
            .regions
            .iter()
            .any(|region| web_link_url_from_action(&region.id).as_deref()
                == Some("https://example.com/docs")));
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

    #[test]
    fn preview_limits_and_cache_hits_keep_actual_block_limit_separate_from_source_limit() {
        MARKDOWN_CACHES.with(|caches| *caches.borrow_mut() = MarkdownCaches::new());
        let block_limited = "paragraph\n\n".repeat(MAX_BLOCKS + 1);
        assert!(!bounded_source(&block_limited).1);
        let first = render_markdown_preview_at_width(&block_limited, 320.0);
        assert!(first.limited);
        assert!(passive_heap_bytes(&first.element).is_some());
        MARKDOWN_CACHES.with(|caches| {
            let caches = caches.borrow();
            assert_eq!(caches.completed.entries.len(), 1);
            let entry = &caches.completed.entries[0];
            assert!(!entry.truncated);
            assert!(entry.limited);
        });
        let cached = render_markdown_preview_at_width(&block_limited, 320.0);
        assert!(cached.limited);
        MARKDOWN_CACHES.with(|caches| {
            let caches = caches.borrow();
            assert_eq!(caches.completed.entries.len(), 1);
            assert!(caches.completed.retained_bytes <= MAX_CACHED_BYTES);
        });

        assert!(!render_markdown_preview_at_width("Respuesta breve 🦀", 320.0).limited);
        let exactly_at_limit = "🦀".repeat(MAX_SOURCE_CHARS);
        assert!(!render_markdown_preview_at_width(&exactly_at_limit, 320.0).limited);
        let source_limited = format!("{exactly_at_limit}FIN");
        assert!(render_markdown_preview_at_width(&source_limited, 320.0).limited);
        assert!(render_markdown_preview_at_width(&source_limited, 320.0).limited);
        MARKDOWN_CACHES.with(|caches| {
            let caches = caches.borrow();
            let entry = caches.completed.entries.last().unwrap();
            assert!(entry.truncated && entry.limited);
            assert_eq!(entry.source.chars().count(), MAX_SOURCE_CHARS);
            assert!(caches.completed.retained_bytes <= MAX_CACHED_BYTES);
        });
    }

    #[test]
    fn live_preview_cache_keeps_limit_metadata_and_bounded_admission() {
        MARKDOWN_CACHES.with(|caches| *caches.borrow_mut() = MarkdownCaches::new());
        for index in 0..5 {
            let source = format!("{index}{}", "\n\nparagraph".repeat(MAX_BLOCKS + 1));
            assert!(render_streaming_markdown_preview_at_width(&source, 320.0).limited);
            assert!(render_streaming_markdown_preview_at_width(&source, 320.0).limited);
        }
        MARKDOWN_CACHES.with(|caches| {
            let caches = caches.borrow();
            // Very large block trees can reach the byte budget before the
            // two-entry limit. They must still retain the newest preview.
            assert!((1..=2).contains(&caches.streaming.entries.len()));
            assert!(caches
                .streaming
                .entries
                .last()
                .unwrap()
                .source
                .starts_with('4'));
            assert!(caches.streaming.entries.iter().all(|entry| entry.limited));
            assert!(caches.streaming.retained_bytes <= MAX_STREAMING_CACHE_BYTES);
            assert!(caches.completed.entries.is_empty());
        });
        let interactive = MarkdownPreview {
            element: Element::button("message-specific-action", "Ver respuesta completa"),
            limited: true,
        };
        assert!(MarkdownCacheEntry::new(
            "source",
            false,
            MarkdownPresentation::at_width(320.0),
            &interactive,
            &[],
            None,
        )
        .is_none());
    }

    #[test]
    fn source_pages_round_trip_unicode_crlf_and_exact_previous_next_offsets() {
        let source = format!(
            "{}\r\n{}{}FIN 🦀",
            "a".repeat(MAX_SOURCE_CHARS - 1),
            "日本語 👩‍💻 e\u{301}\r\n".repeat(3_700),
            "🧪".repeat(MAX_SOURCE_CHARS + 9),
        );
        let mut reconstructed = String::new();
        let mut starts = Vec::new();
        let mut cursor = 0;
        loop {
            let page = markdown_source_page(&source, cursor);
            assert_eq!(page.start_byte, cursor);
            assert!(source.is_char_boundary(page.start_byte));
            assert!(source.is_char_boundary(page.end_byte));
            assert_eq!(page.text, &source[page.start_byte..page.end_byte]);
            assert!(page.text.chars().count() <= MAX_SOURCE_CHARS);
            assert_eq!(page.previous_byte, starts.last().copied());
            if let Some(previous) = page.previous_byte {
                assert_eq!(
                    markdown_source_page(&source, previous).next_byte,
                    Some(cursor)
                );
            }
            if starts.is_empty() {
                assert!(page.text.ends_with('\r'));
                assert!(source[page.next_byte.unwrap()..].starts_with('\n'));
            }
            reconstructed.push_str(page.text);
            starts.push(cursor);
            match page.next_byte {
                Some(next) => {
                    assert_eq!(next, page.end_byte);
                    assert!(next > cursor);
                    cursor = next;
                }
                None => {
                    assert_eq!(page.end_byte, source.len());
                    break;
                }
            }
        }
        assert!(starts.len() > 3);
        assert_eq!(reconstructed, source);
    }

    #[test]
    fn source_page_handles_empty_invalid_utf8_and_stale_cursors() {
        let empty = markdown_source_page("", 0);
        assert_eq!(empty.text, "");
        assert_eq!((empty.start_byte, empty.end_byte), (0, 0));
        assert_eq!((empty.previous_byte, empty.next_byte), (None, None));
        let source = format!("🦀{}", "x".repeat(MAX_SOURCE_CHARS + 3));
        for cursor in [1, 2, 3, source.len() + 1, usize::MAX] {
            let reset = markdown_source_page(&source, cursor);
            assert_eq!(reset.start_byte, 0);
            assert_eq!(reset.previous_byte, None);
            assert!(reset.text.starts_with('🦀'));
        }
        let end = markdown_source_page(&source, source.len());
        assert_eq!(end.text, "");
        assert_eq!(end.next_byte, None);
        assert!(end.previous_byte.is_some());
    }

    #[test]
    fn source_render_is_bounded_to_forty_rows_and_bypasses_markdown_block_limit() {
        let source = "🦀".repeat(MAX_SOURCE_CHARS + 1);
        let page = markdown_source_page(&source, 0);
        let rendered = render_markdown_source_page_at_width(&page, 320.0);
        let ElementKind::Column(rows) = &rendered.kind else {
            panic!("source pages render as a passive row column");
        };
        assert_eq!(rows.len(), MAX_SOURCE_CHARS.div_ceil(MAX_SOURCE_ROW_CHARS));
        assert_eq!(rows.len(), 40);
        let mut reconstructed = String::new();
        for row in rows {
            let ElementKind::Text(text) = &row.kind else {
                panic!("source rows remain literal text");
            };
            assert!(text.chars().count() <= MAX_SOURCE_ROW_CHARS);
            reconstructed.push_str(text);
        }
        assert_eq!(reconstructed, page.text);
        assert!(passive_heap_bytes(&rendered).is_some());

        let blocks = (0..MAX_BLOCKS + 9)
            .map(|index| format!("## bloque {index}\n\n"))
            .collect::<String>();
        assert!(render_markdown_preview_at_width(&blocks, 320.0).limited);
        let page = markdown_source_page(&blocks, 0);
        assert_eq!(page.next_byte, None);
        let rendered = render_markdown_source_page_at_width(&page, 320.0);
        let ElementKind::Column(rows) = rendered.kind else {
            panic!("source pages render as a passive row column");
        };
        let text = rows
            .iter()
            .filter_map(|row| match &row.kind {
                ElementKind::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(text.contains(&format!("## bloque {}", MAX_BLOCKS + 8)));
        assert!(!text.contains("vista limitada"));
    }

    fn visible_text(element: Element) -> String {
        UiTree::new(element)
            .layout(Size::new(640.0, 2_000.0))
            .text_layouts()
            .iter()
            .map(|layout| layout.source.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn live_inline_tokens_keep_their_style_and_terminal_render_uses_authoritative_source() {
        for (source, expected, kind) in [
            ("**acción 👩‍💻", "acción 👩‍💻", InlineKind::Strong),
            ("**acción*", "acción", InlineKind::Strong),
            ("*énfasis", "énfasis", InlineKind::Emphasis),
            ("`src/main.rs", "src/main.rs", InlineKind::Code),
            ("[guía](https://exa", "guía", InlineKind::Link),
            ("[guía]", "guía", InlineKind::Link),
        ] {
            let live = parse_inline_mode(source, true);
            assert_eq!(
                live,
                vec![InlineRun {
                    kind,
                    text: expected.into(),
                    destination: None,
                }],
                "{source}"
            );
            let final_text: String = parse_inline(source)
                .iter()
                .map(|run| run.text.as_str())
                .collect();
            let final_expected = if source == "**acción*" {
                "*acción"
            } else {
                source
            };
            assert_eq!(
                final_text, final_expected,
                "only the live tail may synthesize completion"
            );
            assert_eq!(
                visible_text(render_streaming_markdown_at_width(source, 640.0)),
                expected
            );
            assert_eq!(
                visible_text(render_markdown_at_width(source, 640.0)),
                final_expected
            );
        }
        assert_eq!(
            parse_inline_mode("\\*literal", true),
            vec![InlineRun {
                kind: InlineKind::Normal,
                text: "*literal".into(),
                destination: None,
            }]
        );
        assert_eq!(parse_inline_mode("2 * 3", true), parse_inline("2 * 3"));
        assert_eq!(
            parse_inline_mode("[x](javascript:bad)", true),
            parse_inline("[x](javascript:bad)")
        );
    }

    #[test]
    fn completed_blocks_do_not_inherit_provisional_formatting_from_the_live_tail() {
        let source = "**literal\n\nAhora *énfasis";
        let live = visible_text(render_streaming_markdown_at_width(source, 640.0));
        assert!(live.starts_with("**literal\n"), "{live}");
        assert!(live.ends_with("Ahora énfasis"), "{live}");
        let closed = visible_text(render_streaming_markdown_at_width("**literal\n\n", 640.0));
        assert_eq!(closed, "**literal");
        assert_eq!(
            visible_text(render_streaming_markdown_at_width("**literal\n\n``", 640.0)),
            "**literal"
        );
        let limited = format!("{}\n\nactual tail", "**literal\n\n".repeat(MAX_BLOCKS));
        let rendered = visible_text(render_streaming_markdown_at_width(&limited, 640.0));
        assert!(rendered
            .lines()
            .filter(|line| line.contains("literal"))
            .all(|line| line == "**literal"));
    }

    #[test]
    fn fences_respect_marker_length_language_and_literal_embedded_fences() {
        let source = "````markdown\n```rust\nlet value = 1;\n```\n````\n\n~~~text\n``` is content\n~~~ trailing text\n~~~\nAfter";
        let (blocks, limited) = parse(source);
        assert!(!limited);
        assert_eq!(
            blocks,
            vec![
                Block::Code("markdown".into(), "```rust\nlet value = 1;\n```".into()),
                Block::Code("text".into(), "``` is content\n~~~ trailing text".into()),
                Block::Paragraph("After".into()),
            ]
        );
        assert_eq!(
            parse("```rust extra-info\n  ñ 🦀\n```").0,
            vec![Block::Code("rust".into(), "  ñ 🦀".into())]
        );
        assert_eq!(
            parse("```not`a`fence").0,
            vec![Block::Paragraph("```not`a`fence".into())]
        );
    }

    #[test]
    fn split_fence_openers_do_not_flash_as_prose_and_never_modify_retained_source() {
        for marker in ["`", "``", "~", "~~"] {
            let source = format!("Introducción\n\n{marker}");
            assert_eq!(
                visible_text(render_streaming_markdown_at_width(&source, 640.0)),
                "Introducción"
            );
            assert!(visible_text(render_markdown_at_width(&source, 640.0)).ends_with(marker));
        }
        let source = "Introducción\n\n```rust\nfn main() {}\n```\n\n**Final 👩‍💻**";
        for end in source
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(source.len()))
        {
            let prefix = &source[..end];
            let _ = visible_text(render_streaming_markdown_at_width(prefix, 360.0));
            assert_eq!(prefix, &source[..end]);
        }
        assert_eq!(
            visible_text(render_streaming_markdown_at_width(source, 640.0)),
            visible_text(render_markdown_at_width(source, 640.0))
        );
    }

    #[test]
    fn live_append_reuses_completed_blocks_and_only_reparses_changed_tail() {
        MARKDOWN_CACHES.with(|caches| *caches.borrow_mut() = MarkdownCaches::new());
        INLINE_PARSE_COUNT.with(|count| count.set(0));
        let source =
            "# Encabezado\n\nPárrafo **estable**\n\n```rust\nfn main() {}\n```\n\nCola **vi";
        let _ = render_streaming_markdown_at_width(source, 640.0);
        assert_eq!(INLINE_PARSE_COUNT.with(|count| count.get()), 3);
        INLINE_PARSE_COUNT.with(|count| count.set(0));
        let appended = format!("{source}va**");
        let appended_text = visible_text(render_streaming_markdown_at_width(&appended, 640.0));
        assert!(appended_text.ends_with("Cola viva"));
        assert_eq!(
            INLINE_PARSE_COUNT.with(|count| count.get()),
            1,
            "unchanged heading and paragraph should not parse again"
        );
        let _ = render_streaming_markdown_at_width(&appended, 640.0);
        assert_eq!(
            INLINE_PARSE_COUNT.with(|count| count.get()),
            1,
            "same-frame hit needs no parsing"
        );
        let _ = render_streaming_markdown_at_width(&appended, 360.0);
        assert_eq!(
            INLINE_PARSE_COUNT.with(|count| count.get()),
            4,
            "width changes rebuild every block"
        );
        MARKDOWN_CACHES.with(|caches| {
            let caches = caches.borrow();
            assert!(caches.streaming.retained_bytes <= MAX_STREAMING_CACHE_BYTES);
            assert!(caches.completed.entries.is_empty());
        });
    }

    #[test]
    fn long_unmatched_inline_code_delimiters_remain_literal_and_bounded() {
        let source = format!("Texto {} fin", "`".repeat(MAX_SOURCE_CHARS - 20));
        assert_eq!(
            parse_inline(&source),
            vec![InlineRun {
                kind: InlineKind::Normal,
                text: source.clone(),
                destination: None,
            }]
        );
        assert_eq!(parse_inline_mode(&source, true), parse_inline(&source));
    }
}
