//! Bounded, display-only file snippets from recorded tool input/output.
//! This module never resolves paths, opens files, or applies proposed patches.
use crate::transcript::{ActivityStatus, TranscriptItem};
use openide_ui::{CodeDiffLine, CodeDiffLineKind};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

const MAX_BYTES: usize = 32 * 1024;
const MAX_LINES: usize = 160;
const MAX_FILES: usize = 8;
const MAX_PATH: usize = 1024;
const MAX_CACHE_ENTRIES: usize = 32;
const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct FileActivityPreview {
    pub files: Vec<FilePreviewFile>,
    pub limited: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FilePreviewFile {
    pub path: String,
    pub old_path: Option<String>,
    pub deleted: bool,
    pub lines: Arc<[CodeDiffLine]>,
    pub additions: usize,
    pub deletions: usize,
}

pub(crate) fn is_file_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "write_file" | "edit_file" | "apply_patch"
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    // Identity is checked exactly, rather than relying on hashes or pointers.
    identity: String,
    name: String,
    fields: BTreeMap<String, String>,
    start_line: usize,
    limited: bool,
}

struct Cached {
    source: Source,
    result: Option<Arc<FileActivityPreview>>,
    bytes: usize,
}

#[derive(Default)]
struct Cache {
    entries: VecDeque<Cached>,
    bytes: usize,
}

impl Cache {
    fn get_or_build(&mut self, source: Source) -> Option<Arc<FileActivityPreview>> {
        if let Some(index) = self.entries.iter().position(|entry| entry.source == source) {
            let entry = self.entries.remove(index).expect("observed cache entry");
            let result = entry.result.clone();
            self.entries.push_back(entry);
            return result;
        }
        let result = build(&source).map(Arc::new);
        let bytes = 1024
            + std::mem::size_of::<Cached>()
            + source.identity.len()
            + source.name.len()
            + source
                .fields
                .iter()
                .map(|(key, value)| key.len() + value.len() + 64)
                .sum::<usize>()
            + result.as_ref().map_or(0, |preview| {
                preview
                    .files
                    .iter()
                    .map(|file| {
                        std::mem::size_of::<FilePreviewFile>()
                            + file.path.len()
                            + file.old_path.as_ref().map_or(0, String::len)
                            + file.lines.len() * std::mem::size_of::<CodeDiffLine>()
                            + file.lines.iter().map(|line| line.text.len()).sum::<usize>()
                    })
                    .sum::<usize>()
            });
        while !self.entries.is_empty()
            && (self.entries.len() >= MAX_CACHE_ENTRIES || self.bytes + bytes > MAX_CACHE_BYTES)
        {
            self.bytes -= self.entries.pop_front().expect("nonempty cache").bytes;
        }
        if bytes <= MAX_CACHE_BYTES {
            self.bytes += bytes;
            self.entries.push_back(Cached {
                source,
                result: result.clone(),
                bytes,
            });
        }
        result
    }
}

thread_local! { static CACHE: RefCell<Cache> = RefCell::new(Cache::default()); }
thread_local! {
    static PATH_CACHE: RefCell<VecDeque<(String, Option<String>)>> = const { RefCell::new(VecDeque::new()) };
}

/// Closed provisional cards need identity only. Bound and cache decoding so
/// unchanged historical inputs do not allocate authored code on each rebuild.
pub(crate) fn preview_path(raw: &str) -> Option<String> {
    let (raw, _) = prefix(raw, MAX_BYTES);
    PATH_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, path)) = cache.iter().find(|(key, _)| key == raw) {
            return path.clone();
        }
        let path = partial_fields(raw).and_then(|fields| {
            fields
                .get("path")
                .or_else(|| fields.get("file_path"))
                .filter(|path| valid_path(path))
                .cloned()
        });
        if cache.len() >= 16 {
            cache.pop_front();
        }
        cache.push_back((raw.to_owned(), path.clone()));
        path
    })
}

/// A preview is evidence of the supplied text only. ToolPreview input remains
/// provisional even when a JSON string or the complete input has arrived.
pub(crate) fn preview(
    item: &TranscriptItem,
    partial: Option<(&str, bool)>,
) -> Option<Arc<FileActivityPreview>> {
    let source = source(item, partial)?;
    CACHE.with(|cache| cache.borrow_mut().get_or_build(source))
}

fn prefix(text: &str, limit: usize) -> (&str, bool) {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], end < text.len())
}

fn known_field(key: &str) -> bool {
    matches!(
        key,
        "path" | "file_path" | "content" | "patch" | "old_string" | "new_string"
    )
}

fn source(item: &TranscriptItem, partial: Option<(&str, bool)>) -> Option<Source> {
    let (identity, name) = match item {
        TranscriptItem::ToolPreview {
            id,
            name: Some(name),
            ..
        } => (id, name),
        TranscriptItem::Tool { id, name, .. } => (id, name),
        _ => return None,
    };
    if !is_file_tool(name) || identity.len() > 4096 {
        return None;
    }
    let mut source = Source {
        identity: identity.clone(),
        name: name.clone(),
        fields: BTreeMap::new(),
        start_line: 1,
        limited: false,
    };
    match item {
        TranscriptItem::ToolPreview {
            arguments,
            truncated,
            ..
        } => {
            let (raw, clipped) = prefix(arguments, MAX_BYTES);
            // Cache the bounded source before decoding it. Hover/repaint of
            // unchanged input must not repeatedly parse a growing JSON string.
            source.fields.insert("__raw".into(), raw.into());
            source.limited = clipped || *truncated;
        }
        TranscriptItem::Tool {
            arguments,
            output,
            activity,
            is_error,
            ..
        } => {
            let mut available = MAX_BYTES;
            // Read only known leaves; never serialize an authored file/Value.
            let keys: &[&str] = match name.as_str() {
                "read_file" => &["path", "file_path"],
                "write_file" => &["path", "file_path", "content"],
                "edit_file" => &["path", "file_path", "old_string", "new_string"],
                "apply_patch" => &["patch"],
                _ => return None,
            };
            for &key in keys {
                let Some(value) = arguments.get(key).and_then(Value::as_str) else {
                    continue;
                };
                let (text, clipped) = prefix(
                    value,
                    if matches!(key, "path" | "file_path") {
                        MAX_PATH
                    } else {
                        available
                    },
                );
                source.limited |= clipped;
                if matches!(key, "path" | "file_path") && clipped {
                    continue;
                }
                if !matches!(key, "path" | "file_path") {
                    available -= text.len();
                }
                source.fields.insert(key.into(), text.into());
            }
            source.start_line = arguments
                .get("start_line")
                .and_then(Value::as_u64)
                .and_then(|line| usize::try_from(line).ok())
                .filter(|line| *line > 0)
                .unwrap_or(1);
            if name == "read_file" {
                // Failure text is a diagnostic, never source code read from a file.
                if *is_error {
                    return None;
                }
                let (text, already_limited) = match (activity, output.as_deref(), partial) {
                    (ActivityStatus::Completed, Some(text), _) => (text, false),
                    (ActivityStatus::Running, None, Some(partial)) => partial,
                    _ => return None,
                };
                let (text, clipped) = prefix(text, MAX_BYTES);
                source.fields.insert("read_output".into(), text.into());
                source.limited |= clipped || already_limited;
            }
        }
        _ => return None,
    }
    Some(source)
}

/// Extract top-level fields, accepting a decoded prefix for authored strings.
/// Paths require their closing quote; nested keys and malformed JSON never
/// become path/code sources. Incomplete escape sequences remain undisplayed.
fn partial_fields(raw: &str) -> Option<BTreeMap<String, String>> {
    let mut rest = raw.trim_start().strip_prefix('{')?;
    let mut fields = BTreeMap::new();
    let mut after_comma = false;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(after_close) = rest.strip_prefix('}') {
            if after_comma || !after_close.trim().is_empty() {
                return None;
            }
            break;
        }
        let (key, consumed, complete) = decode_string(rest)?;
        if !complete {
            break;
        }
        rest = rest[consumed..].trim_start();
        let Some(after_colon) = rest.strip_prefix(':') else {
            if rest.is_empty() {
                break;
            } else {
                return None;
            }
        };
        rest = after_colon.trim_start();
        // A duplicate top-level field supersedes any previous value, including
        // when its new value is unfinished or has a different JSON type.
        fields.remove(&key);
        if rest.starts_with('"') {
            let (value, consumed, complete) = decode_string(rest)?;
            if known_field(&key) && (!matches!(key.as_str(), "path" | "file_path") || complete) {
                fields.insert(key, value);
            }
            if !complete {
                break;
            }
            rest = &rest[consumed..];
        } else {
            // This bounded parse skips nested values atomically. It cannot
            // mistake a nested path/content for a top-level field.
            let mut values = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
            match values.next() {
                Some(Ok(_)) => rest = &rest[values.byte_offset()..],
                Some(Err(error)) if error.is_eof() => break,
                None => break,
                Some(Err(_)) => return None,
            }
        }
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        if let Some(after_close) = rest.strip_prefix('}') {
            if !after_close.trim().is_empty() {
                return None;
            }
            break;
        }
        rest = rest.strip_prefix(',')?;
        after_comma = true;
    }
    Some(fields)
}

fn hex_quad(input: &str) -> Option<u16> {
    (input.len() == 4 && input.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| u16::from_str_radix(input, 16).ok())
        .flatten()
}

fn decode_string(input: &str) -> Option<(String, usize, bool)> {
    if !input.starts_with('"') {
        return None;
    }
    let mut result = String::new();
    let mut offset = 1;
    while offset < input.len() {
        let character = input[offset..].chars().next()?;
        match character {
            '"' => return Some((result, offset + 1, true)),
            '\\' => {
                let escape_start = offset;
                offset += 1;
                let Some(escape) = input.as_bytes().get(offset).copied() else {
                    break;
                };
                offset += 1;
                match escape {
                    b'"' => result.push('"'),
                    b'\\' => result.push('\\'),
                    b'/' => result.push('/'),
                    b'b' => result.push('\u{0008}'),
                    b'f' => result.push('\u{000c}'),
                    b'n' => result.push('\n'),
                    b'r' => result.push('\r'),
                    b't' => result.push('\t'),
                    b'u' => {
                        if input.len() - offset < 4 {
                            break;
                        }
                        let first = hex_quad(input.get(offset..offset + 4)?)?;
                        offset += 4;
                        let value = if (0xD800..=0xDBFF).contains(&first) {
                            if input.len() - offset < 6 {
                                offset = escape_start;
                                break;
                            }
                            if input.get(offset..offset + 2)? != "\\u" {
                                return None;
                            }
                            let second = hex_quad(input.get(offset + 2..offset + 6)?)?;
                            if !(0xDC00..=0xDFFF).contains(&second) {
                                return None;
                            }
                            offset += 6;
                            0x10000
                                + ((u32::from(first) - 0xD800) << 10)
                                + (u32::from(second) - 0xDC00)
                        } else if (0xDC00..=0xDFFF).contains(&first) {
                            return None;
                        } else {
                            u32::from(first)
                        };
                        result.push(char::from_u32(value)?);
                    }
                    _ => return None,
                }
            }
            character if character < ' ' => return None,
            character => {
                result.push(character);
                offset += character.len_utf8();
            }
        }
    }
    Some((result, offset, false))
}

fn valid_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= MAX_PATH && !path.chars().any(char::is_control)
}

fn path(source: &Source) -> Option<&str> {
    ["path", "file_path"]
        .iter()
        .find_map(|key| source.fields.get(*key).map(String::as_str))
        .filter(|path| valid_path(path))
}

struct Builder {
    files: Vec<FilePreviewFile>,
    lines: Vec<CodeDiffLine>,
    total_lines: usize,
    bytes: usize,
    limited: bool,
}

impl Builder {
    fn new(limited: bool) -> Self {
        Self {
            files: vec![],
            lines: vec![],
            total_lines: 0,
            bytes: 0,
            limited,
        }
    }
    fn file(&mut self, path: &str) -> bool {
        self.flush();
        if self.files.len() >= MAX_FILES {
            self.limited = true;
            return false;
        }
        self.files.push(FilePreviewFile {
            path: path.into(),
            old_path: None,
            deleted: false,
            lines: Arc::from([]),
            additions: 0,
            deletions: 0,
        });
        true
    }
    fn line(
        &mut self,
        text: &str,
        kind: CodeDiffLineKind,
        old_line: Option<usize>,
        new_line: Option<usize>,
    ) {
        if self.total_lines >= MAX_LINES || self.bytes >= MAX_BYTES {
            self.limited = true;
            return;
        }
        let (text, clipped) = prefix(text, MAX_BYTES - self.bytes);
        self.bytes += text.len();
        self.total_lines += 1;
        self.limited |= clipped;
        if let Some(file) = self.files.last_mut() {
            file.additions += usize::from(kind == CodeDiffLineKind::Added);
            file.deletions += usize::from(kind == CodeDiffLineKind::Removed);
        }
        self.lines.push(CodeDiffLine {
            kind,
            old_line,
            new_line,
            text: text.into(),
        });
    }
    fn text(&mut self, text: &str, kind: CodeDiffLineKind, first: usize) {
        for (index, line) in text.split_terminator('\n').enumerate() {
            let number = Some(first.saturating_add(index));
            self.line(
                line.strip_suffix('\r').unwrap_or(line),
                kind,
                (kind != CodeDiffLineKind::Added)
                    .then_some(number)
                    .flatten(),
                (kind != CodeDiffLineKind::Removed)
                    .then_some(number)
                    .flatten(),
            );
        }
    }
    fn flush(&mut self) {
        if self.lines.is_empty() {
            return;
        }
        if let Some(file) = self.files.last_mut() {
            file.lines = std::mem::take(&mut self.lines).into();
        }
    }
    fn finish(mut self) -> Option<FileActivityPreview> {
        self.flush();
        (!self.files.is_empty()).then_some(FileActivityPreview {
            files: self.files,
            limited: self.limited,
        })
    }
}

fn build(source: &Source) -> Option<FileActivityPreview> {
    let decoded;
    let source = if let Some(raw) = source.fields.get("__raw") {
        decoded = Source {
            identity: source.identity.clone(),
            name: source.name.clone(),
            fields: partial_fields(raw)?,
            start_line: source.start_line,
            limited: source.limited,
        };
        &decoded
    } else {
        source
    };
    let mut result = Builder::new(source.limited);
    match source.name.as_str() {
        "apply_patch" => patch_preview(source.fields.get("patch")?, &mut result),
        "read_file" => {
            let text = source.fields.get("read_output")?;
            result.file(path(source)?);
            result.text(text, CodeDiffLineKind::Context, source.start_line);
        }
        "write_file" => {
            let content = source.fields.get("content")?;
            result.file(path(source)?);
            // Existing/new file state cannot be inferred from proposed text.
            result.text(content, CodeDiffLineKind::Context, 1);
        }
        "edit_file" => {
            let old = source.fields.get("old_string");
            let new = source.fields.get("new_string");
            if old.is_none() && new.is_none() {
                return None;
            }
            result.file(path(source)?);
            // These are relative snippet lines, not positions in a disk file.
            if let Some(old) = old {
                result.text(old, CodeDiffLineKind::Removed, 1);
            }
            if let Some(new) = new {
                result.text(new, CodeDiffLineKind::Added, 1);
            }
        }
        _ => return None,
    }
    result.finish()
}

fn patch_preview(patch: &str, result: &mut Builder) {
    let mut parts = patch.split_inclusive('\n');
    if parts.next().map(|line| line.trim_end_matches(['\r', '\n'])) != Some("*** Begin Patch") {
        return;
    }
    let mut active = false;
    let mut old_number = None::<usize>;
    let mut new_number = None::<usize>;
    for raw in parts {
        let complete = raw.ends_with('\n');
        let line = raw.trim_end_matches(['\r', '\n']);
        let header = ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
            .into_iter()
            .find_map(|prefix| line.strip_prefix(prefix).map(|path| (prefix, path)));
        if let Some((operation, path)) = header {
            active = false;
            // An unfinished header must never expose a guessed filename.
            if !complete || !valid_path(path) {
                continue;
            }
            if !result.file(path) {
                break;
            }
            if let Some(file) = result.files.last_mut() {
                file.deleted = operation == "*** Delete File: ";
            }
            active = operation != "*** Delete File: ";
            old_number = None;
            new_number = (operation == "*** Add File: ").then_some(1);
            continue;
        }
        if !active {
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Move to: ") {
            if complete && valid_path(path) {
                if let Some(file) = result.files.last_mut() {
                    file.old_path = Some(std::mem::replace(&mut file.path, path.into()));
                }
            }
            continue;
        }
        if line == "*** End Patch" {
            break;
        }
        if line.starts_with("***") {
            continue;
        }
        if line.starts_with("@@") {
            // Numeric unified hunks can provide exact source positions.
            // Codex-style context anchors do not: leave those gutters empty.
            old_number = None;
            new_number = None;
            if let Some(numbers) = line.strip_prefix("@@ -") {
                let mut values = numbers.split_whitespace();
                old_number = values
                    .next()
                    .and_then(|value| value.split(',').next()?.parse().ok());
                new_number = values
                    .next()
                    .and_then(|value| value.strip_prefix('+')?.split(',').next()?.parse().ok());
            }
            result.line(line, CodeDiffLineKind::Context, None, None);
            continue;
        }
        let (kind, text, old, new) = if let Some(text) = line.strip_prefix('+') {
            (CodeDiffLineKind::Added, text, None, new_number)
        } else if let Some(text) = line.strip_prefix('-') {
            (CodeDiffLineKind::Removed, text, old_number, None)
        } else if let Some(text) = line.strip_prefix(' ') {
            (CodeDiffLineKind::Context, text, old_number, new_number)
        } else {
            continue;
        };
        result.line(text, kind, old, new);
        if kind != CodeDiffLineKind::Added {
            old_number = old_number.map(|line| line.saturating_add(1));
        }
        if kind != CodeDiffLineKind::Removed {
            new_number = new_number.map(|line| line.saturating_add(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::ModelStreamStatus;
    use serde_json::json;

    fn provisional(name: &str, arguments: &str) -> TranscriptItem {
        TranscriptItem::ToolPreview {
            id: "request:1:preview".into(),
            step: 1,
            item_id: "item".into(),
            call_id: Some("call".into()),
            name: Some(name.into()),
            arguments: arguments.into(),
            input_complete: false,
            status: ModelStreamStatus::Receiving,
            truncated: false,
        }
    }

    fn tool(
        name: &str,
        arguments: Value,
        output: Option<&str>,
        status: ActivityStatus,
    ) -> TranscriptItem {
        TranscriptItem::Tool {
            id: "call".into(),
            name: name.into(),
            arguments,
            output: output.map(str::to_owned),
            activity: status,
            is_error: false,
        }
    }

    fn texts(preview: &FileActivityPreview) -> Vec<&str> {
        preview
            .files
            .iter()
            .flat_map(|file| file.lines.iter().map(|line| line.text.as_str()))
            .collect()
    }

    #[test]
    fn every_json_string_fragment_decodes_only_complete_unicode_scalars() {
        let raw = r#""línea\n\"comillas\"\\\t\u00f1\uD83E\uDD80終""#;
        let expected: String = serde_json::from_str(raw).unwrap();
        for end in raw
            .char_indices()
            .map(|(offset, _)| offset)
            .filter(|offset| *offset > 0)
            .chain(std::iter::once(raw.len()))
        {
            let (decoded, _, complete) = decode_string(&raw[..end]).unwrap();
            assert!(
                expected.starts_with(&decoded),
                "invalid decoded prefix at {end}: {decoded:?}"
            );
            assert_eq!(complete, end == raw.len());
        }
        for raw in [r#""bad\q""#, r#""bad\uDC00""#, r#""bad\uD800\u0041""#] {
            assert!(decode_string(raw).is_none());
        }
    }

    #[test]
    fn partial_input_requires_complete_top_level_path_and_preserves_authored_text() {
        for raw in [
            r#"{"path":"src/unfinished"#,
            r#"{"nested":{"path":"src/hidden.rs","content":"hidden"},"content":"body"#,
            r#"{"path":"src/old.rs","path":"src/new"#,
            r#"{"path":"src/a.rs","content":"body"}garbage"#,
            r#"{"path":"src/a.rs","content":"body",}"#,
        ] {
            assert!(preview(&provisional("write_file", raw), None).is_none());
            assert!(preview_path(raw).is_none());
        }
        let input = r#"{"path":"src/\u00f1.rs","content":"unfinished"#;
        assert_eq!(preview_path(input).as_deref(), Some("src/ñ.rs"));
        assert_eq!(preview_path(input).as_deref(), Some("src/ñ.rs"));
        let item = provisional(
            "write_file",
            r#"{"nested":{"content":"ignored"},"file_path":"src/\u00f1.rs","content":"let x = \"🦀\";\npart\uD83E"#,
        );
        let result = preview(&item, None).unwrap();
        assert_eq!(result.files[0].path, "src/ñ.rs");
        assert_eq!(texts(&result), ["let x = \"🦀\";", "part"]);
        assert!(!result.limited);
        assert_eq!(
            (result.files[0].additions, result.files[0].deletions),
            (0, 0)
        );
        assert!(result.files[0]
            .lines
            .iter()
            .all(|line| line.kind == CodeDiffLineKind::Context));
        assert!(preview(
            &provisional("mcp__other__write_file", r#"{"path":"x","content":"x"}"#),
            None
        )
        .is_none());
    }

    #[test]
    fn read_preview_uses_only_recorded_success_or_running_output_with_original_line_numbers() {
        let args =
            json!({"path":"/not/opened/on/disk.rs", "start_line":41, "content":"not the result"});
        let mut item = tool(
            "read_file",
            args,
            Some("{\"path\":\"literal\"}\n第二行"),
            ActivityStatus::Completed,
        );
        let result = preview(&item, None).unwrap();
        assert_eq!(texts(&result), ["{\"path\":\"literal\"}", "第二行"]);
        assert_eq!(result.files[0].lines[0].old_line, Some(41));
        assert_eq!(result.files[0].lines[1].new_line, Some(42));
        if let TranscriptItem::Tool { is_error, .. } = &mut item {
            *is_error = true;
        }
        assert!(preview(&item, Some(("partial", false))).is_none());
        let running = tool(
            "read_file",
            json!({"path":"src/a.rs"}),
            None,
            ActivityStatus::Running,
        );
        assert!(preview(&running, None).is_none());
        let partial = preview(&running, Some(("observed", true))).unwrap();
        assert!(partial.limited);
        assert_eq!(texts(&partial), ["observed"]);
        assert!(preview(&provisional("read_file", r#"{"path":"src/a.rs"}"#), None).is_none());
    }

    #[test]
    fn patch_preview_preserves_proposed_signs_files_and_only_known_line_positions() {
        let patch = "*** Begin Patch\n*** Add File: new.rs\n+first\n+second\n*** Update File: old.rs\n@@ function\n-old\n+new\n context\n*** Move to: moved.rs\n*** Delete File: removed.rs\n*** End Patch\n+ignored after end\n";
        let item = tool(
            "apply_patch",
            json!({"patch":patch}),
            None,
            ActivityStatus::Running,
        );
        let result = preview(&item, None).unwrap();
        assert_eq!(result.files.len(), 3);
        assert_eq!(result.files[0].path, "new.rs");
        assert_eq!(result.files[0].additions, 2);
        assert_eq!(result.files[0].lines[1].new_line, Some(2));
        assert_eq!(result.files[1].path, "moved.rs");
        assert_eq!(result.files[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(
            (result.files[1].additions, result.files[1].deletions),
            (1, 1)
        );
        assert!(result.files[1]
            .lines
            .iter()
            .all(|line| line.old_line.is_none() && line.new_line.is_none()));
        assert!(!result.files[0].deleted);
        assert!(!result.files[1].deleted);
        assert!(result.files[2].deleted);
        assert!(result.files[2].lines.is_empty());
        let partial = provisional(
            "apply_patch",
            r#"{"patch":"*** Begin Patch\n*** Add File: unfinished"#,
        );
        assert!(preview(&partial, None).is_none());
    }

    #[test]
    fn edit_preview_uses_only_explicit_old_and_new_strings() {
        let item = provisional(
            "edit_file",
            r#"{"path":"src/a.rs","old_string":"old\nline","new_string":"new\npart"#,
        );
        let result = preview(&item, None).unwrap();
        assert_eq!(texts(&result), ["old", "line", "new", "part"]);
        assert_eq!(
            (result.files[0].additions, result.files[0].deletions),
            (2, 2)
        );
        assert_eq!(result.files[0].lines[0].new_line, None);
        assert_eq!(result.files[0].lines[2].old_line, None);
    }

    #[test]
    fn preview_caps_bytes_lines_files_and_marks_incomplete_coverage() {
        let item = tool(
            "write_file",
            json!({"path":"large.rs", "content":"🦀".repeat(MAX_BYTES)}),
            None,
            ActivityStatus::Running,
        );
        let result = preview(&item, None).unwrap();
        assert!(result.limited);
        assert!(texts(&result).iter().map(|text| text.len()).sum::<usize>() <= MAX_BYTES);
        assert!(texts(&result)[0].ends_with('🦀'));
        let item = tool(
            "write_file",
            json!({"path":"lines.rs", "content":"line\n".repeat(MAX_LINES + 20)}),
            None,
            ActivityStatus::Running,
        );
        let result = preview(&item, None).unwrap();
        assert!(result.limited);
        assert_eq!(result.files[0].lines.len(), MAX_LINES);
        let patch = format!(
            "*** Begin Patch\n{}*** End Patch\n",
            (0..MAX_FILES + 3)
                .map(|index| format!("*** Add File: f{index}.rs\n+content {index}\n"))
                .collect::<String>()
        );
        let item = tool(
            "apply_patch",
            json!({"patch":patch}),
            None,
            ActivityStatus::Running,
        );
        let result = preview(&item, None).unwrap();
        assert!(result.limited);
        assert_eq!(result.files.len(), MAX_FILES);
        assert_eq!(result.files[MAX_FILES - 1].lines[0].text, "content 7");
    }

    #[test]
    fn cache_reuses_arcs_only_for_exact_identity_and_source_and_stays_bounded() {
        let mut cache = Cache::default();
        let item = tool(
            "write_file",
            json!({"path":"src/a.rs", "content":"old"}),
            None,
            ActivityStatus::Running,
        );
        let original = source(&item, None).unwrap();
        let first = cache.get_or_build(original.clone()).unwrap();
        let repeated = cache.get_or_build(original.clone()).unwrap();
        assert!(Arc::ptr_eq(&first, &repeated));
        assert!(Arc::ptr_eq(&first.files[0].lines, &repeated.files[0].lines));
        let mut changed = original.clone();
        changed.fields.insert("content".into(), "new".into());
        let next = cache.get_or_build(changed).unwrap();
        assert!(!Arc::ptr_eq(&first, &next));
        assert_eq!(texts(&first), ["old"]);
        for index in 0..100 {
            let mut changed = original.clone();
            changed.identity = format!("call:{index}");
            changed
                .fields
                .insert("content".into(), "x".repeat(MAX_BYTES));
            cache.get_or_build(changed);
            assert!(cache.entries.len() <= MAX_CACHE_ENTRIES);
            assert!(cache.bytes <= MAX_CACHE_BYTES);
        }
    }
}
