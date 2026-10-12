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

/// Removes CSI (`ESC [ ... final`) and OSC (`ESC ] ... BEL | ESC \`) sequences
/// from a byte stream. A sequence may be split across calls; every byte it
/// looks at is ASCII, so multi-byte characters pass through whole.
#[derive(Default)]
pub(crate) struct AnsiStripper(AnsiState);

#[derive(Clone, Copy, Default)]
enum AnsiState {
    #[default]
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
}

impl AnsiStripper {
    pub(crate) fn strip(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &byte in bytes {
            self.0 = match (self.0, byte) {
                (AnsiState::Text, 0x1b) => AnsiState::Escape,
                (AnsiState::Escape, b'[') => AnsiState::Csi,
                (AnsiState::Escape, b']') => AnsiState::Osc,
                // A lone escape or a two-character sequence: drop the escape only.
                (AnsiState::Escape, 0x1b) => AnsiState::Escape,
                (AnsiState::Text | AnsiState::Escape, _) => {
                    out.push(byte);
                    AnsiState::Text
                }
                (AnsiState::Csi, 0x40..=0x7e) => AnsiState::Text,
                (AnsiState::Csi, _) => AnsiState::Csi,
                (AnsiState::Osc | AnsiState::OscEscape, 0x07) => AnsiState::Text,
                (AnsiState::Osc | AnsiState::OscEscape, 0x1b) => AnsiState::OscEscape,
                (AnsiState::OscEscape, b'\\') => AnsiState::Text,
                (AnsiState::Osc | AnsiState::OscEscape, _) => AnsiState::Osc,
            };
        }
    }
}

/// A terminal's text without its CSI and OSC escape sequences.
pub fn strip_ansi(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    AnsiStripper::default().strip(text.as_bytes(), &mut out);
    // Only whole sequences ending in an ASCII byte were removed from valid UTF-8.
    String::from_utf8_lossy(&out).into_owned()
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
    fn a_sequence_split_across_chunks_is_still_removed() {
        let mut stripper = AnsiStripper::default();
        let mut out = Vec::new();
        for chunk in [
            &b"a\x1b"[..],
            b"[1;3",
            b"1mrojo\x1b]0;ti",
            b"tulo\x1b",
            b"\\ fin \xc3",
            b"\xb1",
        ] {
            stripper.strip(chunk, &mut out);
        }
        assert_eq!(out, "arojo fin ñ".as_bytes());
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
