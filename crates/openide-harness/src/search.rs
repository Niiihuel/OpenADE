use crate::tools::{directory_path_argument, existing_workspace_path, required_string};
use crate::{AgentError, ToolContext, ToolOutput};
use openide_core::ToolCall;
use regex::Regex;
use serde_json::Value;
use std::fs::{self, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 5_000;
const MAX_PATHS: usize = 20_000;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_MATCHES: usize = 200;
const MAX_PATTERN_BYTES: usize = 16 * 1024;

const MAX_TERMS: usize = 8;
const MAX_PASSAGES: usize = 200_000;
const PASSAGE_LINES: usize = 40;
const PASSAGE_STEP: usize = 20;
const MAX_RESULTS: usize = 10;
const EXCERPT_LINES: usize = 4;
const MAX_PER_FILE: usize = 2;
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "by", "do", "does", "for", "from", "how", "in", "is",
    "it", "of", "on", "or", "that", "the", "this", "to", "what", "where", "which", "with", "como",
    "con", "de", "del", "donde", "el", "en", "la", "las", "los", "para", "por", "que", "se", "un",
    "una", "y",
];
const DEFINITION_WORDS: &[&str] = &[
    "fn", "def", "class", "struct", "enum", "trait", "interface", "function", "func", "impl",
    "type", "const", "mod", "module",
];

pub(crate) async fn search_files(
    call: &ToolCall,
    context: &ToolContext,
) -> Result<ToolOutput, AgentError> {
    let given = |key| {
        call.arguments
            .get(key)
            .and_then(Value::as_str)
            .filter(|text: &&str| !text.is_empty())
    };
    if let Some(query) = given("query") {
        if given("pattern").is_some() {
            return Ok(ToolOutput::error("give pattern or query, not both"));
        }
        return ranked_search(query, call, context);
    }
    let pattern = required_string(&call.arguments, "pattern")?;
    if pattern.is_empty() || pattern.len() > MAX_PATTERN_BYTES {
        return Ok(ToolOutput::error(
            "search pattern must contain 1..16384 bytes",
        ));
    }
    let regex = Regex::new(pattern)
        .map_err(|error| AgentError::Tool(format!("invalid search pattern: {error}")))?;
    let root = context.workspace.canonicalize().map_err(tool_io_error)?;
    let mut matches = Vec::new();
    let stopped = walk_text_files(call, context, &root, |path, content| {
        let relative = relative_path(path, &root);
        for (index, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                matches.push(format!(
                    "{}:{}: {}",
                    relative,
                    index + 1,
                    line.chars().take(240).collect::<String>()
                ));
                if matches.len() >= MAX_MATCHES {
                    matches.push(format!("[results truncated at {MAX_MATCHES} matches]"));
                    return false;
                }
            }
        }
        true
    })?;
    matches.extend(stopped);
    if matches.is_empty() {
        Ok(ToolOutput::success("no matches"))
    } else {
        Ok(ToolOutput::success(matches.join("\n")))
    }
}

/// One window of lines that holds at least one of the query's words.
struct Passage {
    file: u32,
    start: u32,
    words: u32,
    hits: [u16; MAX_TERMS],
}

/// Ranks windows of lines by the words they share with `query` (BM25), so a
/// question in plain words finds its code without one regular expression per
/// guess. It matches words, not meaning.
// shortcut: fixed line windows and a suffix-stripping stemmer; upgrade to
// syntax-sized passages if definitions keep being cut in half.
fn ranked_search(
    query: &str,
    call: &ToolCall,
    context: &ToolContext,
) -> Result<ToolOutput, AgentError> {
    if query.len() > MAX_PATTERN_BYTES {
        return Ok(ToolOutput::error("search query must contain 1..16384 bytes"));
    }
    let mut asked = Vec::new();
    words(query, |word| asked.push(word.to_owned()));
    let mut terms: Vec<&str> = Vec::new();
    for word in &asked {
        let term = stem(word);
        if word.len() > 1
            && !STOP_WORDS.contains(&word.as_str())
            && !terms.contains(&term)
            && terms.len() < MAX_TERMS
        {
            terms.push(term);
        }
    }
    if terms.is_empty() {
        return Ok(ToolOutput::error("search query has no searchable words"));
    }
    let root = context.workspace.canonicalize().map_err(tool_io_error)?;
    // Each file with its weight and the terms its path names.
    let mut files: Vec<(PathBuf, f64, [bool; MAX_TERMS])> = Vec::new();
    let mut passages: Vec<Passage> = Vec::new();
    let (mut windows, mut total_words) = (0u64, 0u64);
    let mut lines: Vec<(u32, [u16; MAX_TERMS])> = Vec::new();
    let mut notes = Vec::new();
    let stopped = walk_text_files(call, context, &root, |path, content| {
        lines.clear();
        lines.extend(content.lines().map(|line| {
            let mut hits = [0; MAX_TERMS];
            (count_line(line, &terms, &mut hits), hits)
        }));
        let mut listed = false;
        let mut start = 0;
        while start < lines.len() {
            let end = (start + PASSAGE_LINES).min(lines.len());
            let mut passage = Passage {
                file: 0,
                start: start as u32,
                words: 0,
                hits: [0; MAX_TERMS],
            };
            for (count, hits) in &lines[start..end] {
                passage.words += count;
                for (sum, hit) in passage.hits.iter_mut().zip(hits) {
                    *sum = sum.saturating_add(*hit);
                }
            }
            windows += 1;
            total_words += u64::from(passage.words);
            if passage.hits.iter().any(|hit| *hit > 0) {
                if passages.len() >= MAX_PASSAGES {
                    notes.push(format!("[search stopped after {MAX_PASSAGES} passages]"));
                    return false;
                }
                if !listed {
                    listed = true;
                    let relative = relative_path(path, &root);
                    let mut named = [0; MAX_TERMS];
                    count_line(&relative, &terms, &mut named);
                    // Tests repeat the words of the code they cover; the code goes first.
                    let mut weight = 1.0;
                    words(&relative, |word| {
                        if matches!(stem(word), "test" | "spec") {
                            weight = 0.7;
                        }
                    });
                    files.push((path.to_path_buf(), weight, named.map(|hit| hit > 0)));
                }
                passage.file = files.len() as u32 - 1;
                passages.push(passage);
            }
            if end == lines.len() {
                break;
            }
            start += PASSAGE_STEP;
        }
        true
    })?;
    notes.extend(stopped);

    // BM25 with k1 = 1.2 and b = 0.75; a word in the file's path counts as
    // one more full match.
    let average = (total_words as f64 / windows.max(1) as f64).max(1.0);
    let rarity: Vec<f64> = (0..terms.len())
        .map(|term| {
            let holding = passages.iter().filter(|p| p.hits[term] > 0).count() as f64;
            (1.0 + (windows as f64 - holding + 0.5) / (holding + 0.5)).ln()
        })
        .collect();
    let mut ranked: Vec<(f64, &Passage)> = passages
        .iter()
        .map(|passage| {
            let length = 0.25 + 0.75 * f64::from(passage.words) / average;
            let score: f64 = rarity
                .iter()
                .enumerate()
                .map(|(term, rarity)| {
                    let hits = f64::from(passage.hits[term]);
                    let named = files[passage.file as usize].2[term];
                    rarity * (hits * 2.2 / (hits + 1.2 * length) + f64::from(u8::from(named)))
                })
                .sum();
            (score * files[passage.file as usize].1, passage)
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then((a.1.file, a.1.start).cmp(&(b.1.file, b.1.start)))
    });

    let mut chosen: Vec<&Passage> = Vec::new();
    let mut output = Vec::new();
    for (score, passage) in ranked {
        if chosen.len() == MAX_RESULTS {
            break;
        }
        let same_file = chosen.iter().filter(|other| other.file == passage.file);
        if same_file.clone().count() == MAX_PER_FILE
            || same_file
                .clone()
                .any(|other| other.start.abs_diff(passage.start) < PASSAGE_LINES as u32)
        {
            continue;
        }
        let path = &files[passage.file as usize].0;
        let Some(content) = read_text(path) else {
            continue;
        };
        chosen.push(passage);
        let relative = relative_path(path, &root);
        let start = passage.start as usize;
        let mut excerpt: Vec<(f64, usize, &str)> = content
            .lines()
            .enumerate()
            .skip(start)
            .take(PASSAGE_LINES)
            .filter_map(|(index, line)| {
                let mut hits = [0; MAX_TERMS];
                count_line(line, &terms, &mut hits);
                let weight: f64 = rarity
                    .iter()
                    .zip(hits)
                    .map(|(rarity, hit)| rarity * f64::from(hit))
                    .sum();
                (weight > 0.0).then_some((weight, index, line))
            })
            .collect();
        let end = (start + PASSAGE_LINES).min(content.lines().count());
        output.push(format!(
            "{relative}:{}-{end} [relevance {score:.1}]",
            start + 1
        ));
        excerpt.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        excerpt.truncate(EXCERPT_LINES);
        excerpt.sort_by_key(|line| line.1);
        output.extend(excerpt.into_iter().map(|(_, index, line)| {
            format!(
                "{relative}:{}: {}",
                index + 1,
                line.chars().take(240).collect::<String>()
            )
        }));
    }
    if output.is_empty() && notes.is_empty() {
        return Ok(ToolOutput::success("no matches"));
    }
    output.extend(notes);
    Ok(ToolOutput::success(output.join("\n")))
}

/// Adds the line's occurrences of each term to `hits` and returns how many
/// words the line has.
fn count_line(line: &str, terms: &[&str], hits: &mut [u16; MAX_TERMS]) -> u32 {
    let mut total = 0;
    let mut defining = false;
    words(line, |word| {
        total += 1;
        if let Some(term) = terms.iter().position(|term| *term == stem(word)) {
            // A name counts twice on the line that defines it, so the
            // definition outranks its uses.
            hits[term] = hits[term].saturating_add(1 + u16::from(defining));
        }
        defining |= DEFINITION_WORDS.contains(&word);
    });
    total
}

/// Calls `word` with each lowercase word of `text`. Identifiers split at
/// punctuation, underscores and camelCase humps.
fn words(text: &str, mut word: impl FnMut(&str)) {
    let mut current = String::new();
    let mut after_lower = false;
    for character in text.chars() {
        let part = character.is_alphanumeric();
        if !current.is_empty() && (!part || (after_lower && character.is_uppercase())) {
            word(&current);
            current.clear();
        }
        if part {
            current.extend(character.to_lowercase());
        }
        after_lower = character.is_lowercase() || character.is_numeric();
    }
    if !current.is_empty() {
        word(&current);
    }
}

/// Folds the commonest English endings so "parsing" and "parsed" meet "parse".
fn stem(word: &str) -> &str {
    fn cut<'a>(word: &'a str, suffix: &str) -> Option<&'a str> {
        word.strip_suffix(suffix).filter(|rest| rest.len() >= 3)
    }
    let word = ["ing", "ed", "es"]
        .iter()
        .find_map(|suffix| cut(word, suffix))
        .or_else(|| (!word.ends_with("ss")).then(|| cut(word, "s")).flatten())
        .unwrap_or(word);
    cut(word, "e").unwrap_or(word)
}

/// Walks the UTF-8 text files under the call's `path` in path order, inside
/// the workspace and the search bounds, until `visit` returns false. Returns
/// the notice of the bound that ended the walk early, if one did.
fn walk_text_files(
    call: &ToolCall,
    context: &ToolContext,
    root: &Path,
    mut visit: impl FnMut(&Path, &str) -> bool,
) -> Result<Option<String>, AgentError> {
    let start = directory_path_argument(&call.arguments);
    let start = existing_workspace_path(&context.workspace, start)?;
    let mut stack = vec![start];
    let mut scanned = 0usize;
    let mut visited = 0usize;
    while let Some(path) = stack.pop() {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        visited += 1;
        if visited > MAX_PATHS {
            return Ok(Some(format!("[search stopped after {MAX_PATHS} paths]")));
        }
        // Re-resolve queued entries because a directory may have been
        // replaced by a junction after it was listed.
        let current = path.canonicalize().map_err(tool_io_error)?;
        if !current.starts_with(root) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(tool_io_error)?;
        if is_link_or_reparse(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            let mut children = Vec::new();
            for entry in fs::read_dir(&path).map_err(tool_io_error)? {
                let entry = entry.map_err(tool_io_error)?;
                let child_path = entry.path();
                let child_metadata = fs::symlink_metadata(&child_path).map_err(tool_io_error)?;
                if is_link_or_reparse(&child_metadata) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), ".git" | "node_modules" | "target" | ".next") {
                    continue;
                }
                let child_path = child_path.canonicalize().map_err(tool_io_error)?;
                if !child_path.starts_with(root) {
                    continue;
                }
                children.push(child_path);
            }
            children.sort();
            stack.extend(children.into_iter().rev());
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        scanned += 1;
        if scanned > MAX_FILES {
            return Ok(Some(format!("[search stopped after {MAX_FILES} files]")));
        }
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let Some(content) = read_text(&path) else {
            continue;
        };
        if !visit(&path, &content) {
            break;
        }
    }
    Ok(None)
}

/// The file's text, unless it is a link, over the size bound or not UTF-8.
fn read_text(path: &Path) -> Option<String> {
    let file = open_no_follow(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn open_no_follow(path: &Path) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

fn relative_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn tool_io_error(error: std::io::Error) -> AgentError {
    AgentError::Tool(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn finds_lines_inside_workspace() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sample.rs"), "one\nneedle here\nthree").unwrap();
        let call = ToolCall {
            id: "search-1".into(),
            name: "search_files".into(),
            arguments: json!({"pattern":"needle"}),
        };
        let result = search_files(
            &call,
            &ToolContext {
                workspace: dir.path().to_path_buf(),
                cancel: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.content, "sample.rs:2: needle here");
    }

    #[tokio::test]
    async fn skips_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = vec![b'x'; MAX_FILE_BYTES as usize + 1];
        bytes[..6].copy_from_slice(b"needle");
        fs::write(dir.path().join("huge.txt"), bytes).unwrap();
        let result = search_files(
            &ToolCall {
                id: "search-large".into(),
                name: "search_files".into(),
                arguments: json!({"pattern":"needle"}),
            },
            &ToolContext {
                workspace: dir.path().to_path_buf(),
                cancel: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.content, "no matches");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skips_linked_paths_outside_workspace() {
        use std::os::unix::fs::symlink;
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "needle").unwrap();
        symlink(outside.path(), workspace.path().join("linked")).unwrap();
        let result = search_files(
            &ToolCall {
                id: "search-link".into(),
                name: "search_files".into(),
                arguments: json!({"pattern":"needle"}),
            },
            &ToolContext {
                workspace: workspace.path().to_path_buf(),
                cancel: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.content, "no matches");
    }

    async fn search(dir: &Path, arguments: serde_json::Value) -> ToolOutput {
        let call = ToolCall {
            id: "search-query".into(),
            name: "search_files".into(),
            arguments,
        };
        let context = ToolContext {
            workspace: dir.to_path_buf(),
            cancel: CancellationToken::new(),
        };
        search_files(&call, &context).await.unwrap()
    }

    #[tokio::test]
    async fn a_query_ranks_the_passage_sharing_most_of_its_words_first() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("a_notes.md"),
            "We retry sometimes.\nNothing else about it here.\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("net.rs"),
            "// unrelated\nfn retryWithBackoff(attempts: u32) {\n    wait_for_rate_limit();\n}\n",
        )
        .unwrap();
        fs::write(dir.path().join("other.rs"), "fn main() {}\n").unwrap();
        let result = search(
            dir.path(),
            json!({"query": "where do we retrying with backoff after a rate limit"}),
        )
        .await;
        let lines: Vec<&str> = result.content.lines().collect();
        assert!(lines[0].starts_with("net.rs:1-4 [relevance "), "{}", result.content);
        // Identifiers split at humps and underscores, and "retrying" meets "retry".
        assert_eq!(lines[1], "net.rs:2: fn retryWithBackoff(attempts: u32) {");
        assert_eq!(lines[2], "net.rs:3:     wait_for_rate_limit();");
        assert!(lines[3].starts_with("a_notes.md:1-2 [relevance "), "{}", result.content);
        assert_eq!(lines[4], "a_notes.md:1: We retry sometimes.");
        assert_eq!(lines.len(), 5, "{}", result.content);
    }

    #[tokio::test]
    async fn a_definition_outranks_a_use_and_overlapping_windows_are_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        let filler = "let unrelated = 1;\n".repeat(60);
        fs::write(
            dir.path().join("uses.rs"),
            format!("{filler}    token_budget(session);\n{filler}"),
        )
        .unwrap();
        fs::write(
            dir.path().join("zz.rs"),
            format!("{filler}fn token_budget(session: &Session) -> u64 {{\n{filler}"),
        )
        .unwrap();
        let result = search(dir.path(), json!({"query": "token budget"})).await;
        let headers: Vec<&str> = result
            .content
            .lines()
            .filter(|line| line.contains(" [relevance "))
            .collect();
        assert_eq!(headers.len(), 2, "{}", result.content);
        assert!(headers[0].starts_with("zz.rs:"), "{}", result.content);
        assert!(headers[1].starts_with("uses.rs:"), "{}", result.content);
    }

    #[tokio::test]
    async fn a_query_needs_searchable_words_and_excludes_a_pattern() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sample.rs"), "needle").unwrap();
        let none = search(dir.path(), json!({"query": "where is the"})).await;
        assert!(none.is_error);
        assert_eq!(none.content, "search query has no searchable words");
        let both = search(dir.path(), json!({"query": "needle", "pattern": "needle"})).await;
        assert!(both.is_error);
        let absent = search(dir.path(), json!({"query": "haystack"})).await;
        assert_eq!(absent.content, "no matches");
        // An empty query leaves the regular expression search as it was.
        let pattern = search(dir.path(), json!({"query": "", "pattern": "needle"})).await;
        assert_eq!(pattern.content, "sample.rs:1: needle");
    }
}
