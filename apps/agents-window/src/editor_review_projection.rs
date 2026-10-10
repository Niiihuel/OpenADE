//! Translate recorded unified-diff runs into editor decorations. Context and
//! metadata never become removed text or added highlights.
use crate::git_review::{ReviewFile, ReviewLineKind};
use openide_ui::CodeReviewHunk;
use std::sync::Arc;

const MAX_LINES: usize = 10_000;
const MAX_HUNKS: usize = 4096;
const MAX_REMOVED_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct Run {
    anchor: Option<usize>,
    end: usize,
    old_start: Option<usize>,
    removed: Vec<String>,
    added: bool,
}

fn flush(run: &mut Run, hunks: &mut Vec<CodeReviewHunk>) -> Option<()> {
    if !run.added && run.removed.is_empty() {
        return Some(());
    }
    let anchor = run.anchor?;
    if hunks.len() >= MAX_HUNKS
        || hunks
            .last()
            .is_some_and(|previous| previous.new_rows.end > anchor)
    {
        return None;
    }
    hunks.push(CodeReviewHunk {
        new_rows: anchor..if run.added { run.end } else { anchor },
        old_start_row: run.old_start.unwrap_or(0),
        removed_lines: std::mem::take(&mut run.removed).into(),
    });
    *run = Run::default();
    Some(())
}

/// All new-row coordinates come from recorded line numbers. Hunk ranges supply
/// the insertion cursor for pure deletions, including `+N,0` after line N at EOF.
/// Invalid or inconsistent evidence returns no decorations instead of painting
/// a different line; the host still retains the original diff for inspection.
pub(crate) fn build_review_hunks(file: &ReviewFile) -> Arc<[CodeReviewHunk]> {
    project(file).unwrap_or_default().into()
}

fn project(file: &ReviewFile) -> Option<Vec<CodeReviewHunk>> {
    if file.lines.len() > MAX_LINES {
        return None;
    }
    let mut result = Vec::new();
    let mut run = Run::default();
    let mut new_cursor = None;
    let mut old_cursor = None;
    let mut removed_bytes = 0usize;
    for line in &file.lines {
        match line.kind {
            ReviewLineKind::Added => {
                line.text.strip_prefix('+')?;
                let row = line.new_number?.checked_sub(1)?;
                if new_cursor.is_some_and(|cursor| cursor != row) {
                    return None;
                }
                if let Some(anchor) = run.anchor {
                    if row != if run.added { run.end } else { anchor } {
                        return None;
                    }
                } else {
                    run.anchor = Some(row);
                    run.old_start = old_cursor;
                }
                run.added = true;
                run.end = row.checked_add(1)?;
                new_cursor = Some(run.end);
            }
            ReviewLineKind::Removed => {
                let text = line.text.strip_prefix('-')?;
                let old_row = line.old_number?.checked_sub(1)?;
                if old_cursor.is_some_and(|cursor| cursor != old_row) {
                    return None;
                }
                // A deletion before any added row uses the hunk/context cursor.
                if run.anchor.is_none() {
                    run.anchor = new_cursor;
                    run.old_start = Some(old_row);
                } else if run.removed.is_empty() {
                    run.old_start = Some(old_row);
                }
                removed_bytes = removed_bytes.checked_add(text.len())?;
                if removed_bytes > MAX_REMOVED_BYTES {
                    return None;
                }
                run.removed.push(text.to_owned());
                old_cursor = old_row.checked_add(1);
            }
            ReviewLineKind::Context => {
                flush(&mut run, &mut result)?;
                let new = line.new_number?;
                let old = line.old_number?;
                let new_row = new.checked_sub(1)?;
                let old_row = old.checked_sub(1)?;
                if new_cursor.is_some_and(|cursor| cursor != new_row)
                    || old_cursor.is_some_and(|cursor| cursor != old_row)
                {
                    return None;
                }
                new_cursor = Some(new);
                old_cursor = Some(old);
            }
            ReviewLineKind::Hunk => {
                flush(&mut run, &mut result)?;
                let (old, new) = hunk_cursors(&line.text)?;
                old_cursor = Some(old);
                new_cursor = Some(new);
            }
            ReviewLineKind::Meta if line.text == "\\ No newline at end of file" => {
                // Git inserts this annotation between a replacement's removed
                // and added lines. It is not context and must not split the run.
            }
            ReviewLineKind::File | ReviewLineKind::Meta => {
                flush(&mut run, &mut result)?;
                new_cursor = None;
                old_cursor = None;
            }
        }
    }
    flush(&mut run, &mut result)?;
    Some(result)
}

fn hunk_cursors(text: &str) -> Option<(usize, usize)> {
    let mut words = text.split_whitespace();
    if words.next()? != "@@" {
        return None;
    }
    let old = range_cursor(words.next()?.strip_prefix('-')?)?;
    let new = range_cursor(words.next()?.strip_prefix('+')?)?;
    (words.next()? == "@@").then_some((old, new))
}

fn range_cursor(range: &str) -> Option<usize> {
    let (start, count) = match range.split_once(',') {
        Some((start, count)) => (start.parse::<usize>().ok()?, count.parse::<usize>().ok()?),
        None => (range.parse::<usize>().ok()?, 1),
    };
    start.checked_add(count)?;
    if count == 0 {
        Some(start)
    } else {
        start.checked_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_review::ReviewLine;

    fn row(kind: ReviewLineKind, text: &str, old: Option<usize>, new: Option<usize>) -> ReviewLine {
        ReviewLine {
            kind,
            text: text.into(),
            old_number: old,
            new_number: new,
        }
    }
    fn hunk(text: &str) -> ReviewLine {
        row(ReviewLineKind::Hunk, text, None, None)
    }
    fn projected(lines: Vec<ReviewLine>) -> Arc<[CodeReviewHunk]> {
        build_review_hunks(&ReviewFile {
            path: "fixture.rs".into(),
            lines,
            ..Default::default()
        })
    }

    #[test]
    fn replacements_keep_removed_text_and_exact_new_rows_without_context() {
        let hunks = projected(vec![
            hunk("@@ -5,4 +5,5 @@ fn example()"),
            row(ReviewLineKind::Context, " unchanged", Some(5), Some(5)),
            row(ReviewLineKind::Removed, "-old one", Some(6), None),
            row(ReviewLineKind::Removed, "--literal minus", Some(7), None),
            row(ReviewLineKind::Added, "+new one", None, Some(6)),
            row(ReviewLineKind::Added, "+new two", None, Some(7)),
            row(ReviewLineKind::Added, "+new three", None, Some(8)),
            row(ReviewLineKind::Context, " unchanged tail", Some(8), Some(9)),
        ]);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].new_rows, 5..8);
        assert_eq!(hunks[0].old_start_row, 5);
        assert_eq!(&*hunks[0].removed_lines, &["old one", "-literal minus"]);
    }

    #[test]
    fn added_runs_separated_by_context_remain_independent() {
        let hunks = projected(vec![
            hunk("@@ -1,2 +1,4 @@"),
            row(ReviewLineKind::Added, "+first", None, Some(1)),
            row(ReviewLineKind::Context, " middle", Some(1), Some(2)),
            row(ReviewLineKind::Added, "+second", None, Some(3)),
            row(ReviewLineKind::Context, " tail", Some(2), Some(4)),
        ]);
        assert_eq!(
            hunks.iter().map(|h| h.new_rows.clone()).collect::<Vec<_>>(),
            vec![0..1, 2..3]
        );
        assert!(hunks.iter().all(|h| h.removed_lines.is_empty()));
    }

    #[test]
    fn pure_deletions_anchor_before_next_row_or_after_last_at_eof() {
        for (header, old, expected) in [("@@ -1,1 +0,0 @@", 1, 0), ("@@ -3,1 +2,0 @@", 3, 2)] {
            let hunks = projected(vec![
                hunk(header),
                row(ReviewLineKind::Removed, "-removed", Some(old), None),
            ]);
            assert_eq!(hunks.len(), 1);
            assert_eq!(hunks[0].new_rows, expected..expected);
            assert_eq!(hunks[0].old_start_row, old - 1);
            assert_eq!(&*hunks[0].removed_lines, &["removed"]);
        }
        let middle = projected(vec![
            hunk("@@ -10,3 +10,2 @@"),
            row(ReviewLineKind::Context, " before", Some(10), Some(10)),
            row(ReviewLineKind::Removed, "-removed middle", Some(11), None),
            row(ReviewLineKind::Context, " after", Some(12), Some(11)),
        ]);
        assert_eq!(middle[0].new_rows, 10..10);
    }

    #[test]
    fn separate_hunks_preserve_gaps_and_newline_annotations_do_not_split_replacement() {
        let hunks = projected(vec![
            hunk("@@ -2 +2 @@"),
            row(ReviewLineKind::Removed, "-viejo 🦀", Some(2), None),
            row(
                ReviewLineKind::Meta,
                "\\ No newline at end of file",
                None,
                None,
            ),
            row(ReviewLineKind::Added, "+nuevo 🦀", None, Some(2)),
            row(
                ReviewLineKind::Meta,
                "\\ No newline at end of file",
                None,
                None,
            ),
            hunk("@@ -20,0 +21,1 @@"),
            row(ReviewLineKind::Added, "+far away", None, Some(21)),
        ]);
        assert_eq!(
            hunks.iter().map(|h| h.new_rows.clone()).collect::<Vec<_>>(),
            vec![1..2, 20..21]
        );
        assert_eq!(&*hunks[0].removed_lines, &["viejo 🦀"]);
        assert!(hunks[1].removed_lines.is_empty());
    }

    #[test]
    fn malformed_coordinates_or_missing_deleted_anchor_produce_no_wrong_highlight() {
        assert!(
            projected(vec![row(
                ReviewLineKind::Removed,
                "-unknown anchor",
                Some(1),
                None
            )])
            .is_empty()
        );
        assert!(
            projected(vec![
                hunk("@@ -1 +1 @@"),
                row(ReviewLineKind::Added, "+wrong row", None, Some(9))
            ])
            .is_empty()
        );
        assert!(
            projected(vec![
                hunk("@@ -1 +0,1 @@"),
                row(ReviewLineKind::Added, "+invalid zero", None, Some(0))
            ])
            .is_empty()
        );
    }
}
