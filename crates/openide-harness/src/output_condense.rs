//! Lossless-in-meaning condensing of command output before it reaches the
//! model: terminal control sequences, overwritten progress lines and runs of
//! identical lines cost tokens and carry nothing. Safe to apply to output an
//! external proxy already condensed; a second pass changes nothing.

/// Runs shorter than this are left alone; two equal lines are often real.
const MIN_REPEAT_RUN: usize = 4;

pub(crate) fn condense_output(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let stripped = strip_ansi(text);
    for line in stripped.split('\n') {
        // A carriage return rewinds the line: only what follows the last one
        // stayed on screen. A trailing one is a Windows line ending.
        let line = line.strip_suffix('\r').unwrap_or(line);
        lines.push(line.rsplit('\r').next().unwrap_or(line));
    }
    let mut out = String::with_capacity(stripped.len());
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let run = lines[index..].iter().take_while(|next| **next == line).count();
        if index > 0 {
            out.push('\n');
        }
        out.push_str(line);
        if run >= MIN_REPEAT_RUN && !line.is_empty() {
            out.push_str(&format!("\n[previous line repeated {} more times]", run - 1));
            index += run;
        } else {
            index += 1;
        }
    }
    out
}

/// Removes CSI (`ESC [ ... final`) and OSC (`ESC ] ... BEL | ESC \`) sequences.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' || next == '\u{1b}' && chars.next_if_eq(&'\\').is_some() {
                        break;
                    }
                }
            }
            // A lone escape or a two-character sequence: drop the escape only.
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_output_is_returned_unchanged() {
        for text in ["", "one", "a\nb\n", "same\nsame\nsame\n", "\n\n\n\n\n"] {
            assert_eq!(condense_output(text), text);
        }
    }

    #[test]
    fn control_sequences_and_overwritten_progress_are_removed() {
        assert_eq!(
            condense_output("\u{1b}[1;31merror\u{1b}[0m: \u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{1b}\\ ok"),
            "error: link ok"
        );
        assert_eq!(condense_output(" 10%\r 55%\r100% done\nnext\r\n"), "100% done\nnext\n");
    }

    #[test]
    fn long_runs_collapse_and_keep_their_count() {
        let text = format!("start\n{}end", "warning: unused\n".repeat(50));
        assert_eq!(
            condense_output(&text),
            "start\nwarning: unused\n[previous line repeated 49 more times]\nend"
        );
    }

    #[test]
    fn a_second_pass_changes_nothing() {
        let text = format!("\u{1b}[32mok\u{1b}[0m\n{}tail\r\n", "x\n".repeat(9));
        let once = condense_output(&text);
        assert_eq!(condense_output(&once), once);
    }
}
