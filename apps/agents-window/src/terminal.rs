//! Interactive terminal sessions backed by the operating system's PTY.
//!
//! The UI owns these sessions by dock tab ID. A reader thread updates a bounded
//! ANSI screen; the UI only copies a snapshot after `poll` reports a change.
//! This keeps process I/O and parsing away from the render thread.

use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex, Weak,
    },
    thread,
};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// Limits terminal history even if a command writes indefinitely.
pub const SCROLLBACK_LINES: usize = 1_000;
pub const MAX_COLUMNS: u16 = 240;
pub const MAX_ROWS: u16 = 120;
const READ_CHUNK_BYTES: usize = 8 * 1024;
const MAX_INPUT_BYTES: usize = 16 * 1024;
const INPUT_QUEUE_SLOTS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Dimensions {
    cols: u16,
    rows: u16,
}

impl Dimensions {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.clamp(2, MAX_COLUMNS),
            rows: rows.clamp(1, MAX_ROWS),
        }
    }

    fn pty_size(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

struct TerminalCallbacks {
    // Weak avoids a cycle: the writer thread owns SharedScreen while waiting
    // on its receiver. Closing the session must drop the final strong sender.
    reply: Option<Weak<SyncSender<Vec<u8>>>>,
}

impl vt100::Callbacks for TerminalCallbacks {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        intermediate: Option<u8>,
        _second_intermediate: Option<u8>,
        params: &[&[u16]],
        final_char: char,
    ) {
        let Some(reply) = self.reply.as_ref().and_then(Weak::upgrade) else {
            return;
        };
        let parameter = params.first().and_then(|part| part.first()).copied();
        let response = match (intermediate, parameter, final_char) {
            // ConPTY queries the cursor position as soon as it starts. If no
            // DSR response is sent, cmd.exe/PowerShell can wait indefinitely.
            (None, Some(6), 'n') => {
                let (row, col) = screen.cursor_position();
                format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
            }
            (None, Some(5), 'n') => b"\x1b[0n".to_vec(),
            (None, None | Some(0), 'c') => b"\x1b[?1;2c".to_vec(),
            _ => return,
        };
        let _ = reply.try_send(response);
    }
}

struct SharedScreen {
    parser: Mutex<vt100::Parser<TerminalCallbacks>>,
    error: Mutex<Option<String>>,
    revision: AtomicU64,
    closed: AtomicBool,
}

impl SharedScreen {
    fn new(size: Dimensions, reply: Option<Weak<SyncSender<Vec<u8>>>>) -> Self {
        Self {
            parser: Mutex::new(vt100::Parser::new_with_callbacks(
                size.rows,
                size.cols,
                SCROLLBACK_LINES,
                TerminalCallbacks { reply },
            )),
            error: Mutex::new(None),
            revision: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::Release);
    }

    fn set_error(&self, error: io::Error) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut stored) = self.error.lock() {
            *stored = Some(error.to_string());
            self.changed();
        }
    }
}

/// Colors that can be mapped to the application's terminal palette.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TerminalColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl From<vt100::Color> for TerminalColor {
    fn from(value: vt100::Color) -> Self {
        match value {
            vt100::Color::Default => Self::Default,
            vt100::Color::Idx(index) => Self::Indexed(index),
            vt100::Color::Rgb(red, green, blue) => Self::Rgb(red, green, blue),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalStyle {
    pub foreground: TerminalColor,
    pub background: TerminalColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSpan {
    pub text: String,
    pub style: TerminalStyle,
}

/// Snapshot of visible rows. `styled_lines` groups adjacent cells with the
/// same ANSI attributes, so the dock can render color without parsing escapes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSnapshot {
    pub lines: Vec<String>,
    pub styled_lines: Vec<Vec<TerminalSpan>>,
    pub cursor: (u16, u16),
    pub cols: u16,
    pub rows: u16,
    pub scrollback_position: usize,
    pub running: bool,
    pub exit_code: Option<u32>,
    pub error: Option<String>,
    pub revision: u64,
}

#[derive(Debug)]
enum SessionStatus {
    Running,
    Exited(u32),
    Failed(String),
    Closed,
}

/// One interactive shell. Dropping the session terminates its child process.
/// The read/write workers never block the render thread on PTY I/O.
pub struct TerminalSession {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    input: Option<Arc<SyncSender<Vec<u8>>>>,
    shared: Arc<SharedScreen>,
    observed_revision: u64,
    status: SessionStatus,
    size: Dimensions,
}

impl TerminalSession {
    /// Launch the user's shell in `workspace`. On Windows, PowerShell is the
    /// built-in default; `OPENIDE_TERMINAL_SHELL` can point to another shell.
    pub fn spawn(workspace: &Path, cols: u16, rows: u16) -> io::Result<Self> {
        let (program, args) = default_shell();
        Self::spawn_with_command(workspace, cols, rows, &program, &args)
    }

    /// Launch an explicit program. Useful for configured shells and tests.
    pub fn spawn_with_command(
        workspace: &Path,
        cols: u16,
        rows: u16,
        program: &OsStr,
        args: &[OsString],
    ) -> io::Result<Self> {
        if program.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal program is empty",
            ));
        }
        let workspace = workspace.canonicalize()?;
        if !workspace.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal workspace is not a directory",
            ));
        }

        let size = Dimensions::new(cols, rows);
        let pair = native_pty_system()
            .openpty(size.pty_size())
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(&workspace);
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| io::Error::other(error.to_string()))?;
        // The parent must not keep the slave end open; otherwise EOF can be
        // hidden from the reader after the child exits.
        drop(pair.slave);

        let reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                return Err(io::Error::other(error.to_string()));
            }
        };
        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                let _ = child.kill();
                return Err(io::Error::other(error.to_string()));
            }
        };
        let (input, queued_input) = mpsc::sync_channel(INPUT_QUEUE_SLOTS);
        let input = Arc::new(input);
        let shared = Arc::new(SharedScreen::new(size, Some(Arc::downgrade(&input))));
        if let Err(error) = start_reader(reader, Arc::clone(&shared)) {
            let _ = child.kill();
            return Err(error);
        }
        if let Err(error) = start_writer(writer, queued_input, Arc::clone(&shared)) {
            shared.closed.store(true, Ordering::Release);
            let _ = child.kill();
            return Err(error);
        }

        Ok(Self {
            master: Some(pair.master),
            child: Some(child),
            input: Some(input),
            shared,
            observed_revision: 0,
            status: SessionStatus::Running,
            size,
        })
    }

    /// Queue raw PTY input without waiting for the shell to consume it. Enter
    /// is `b"\r"`; special keys use their standard terminal escape sequences.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if !matches!(&self.status, SessionStatus::Running) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"));
        }
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal input exceeds 16 KiB",
            ));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let input = self
            .input
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"))?;
        input.try_send(bytes.to_vec()).map_err(|error| match error {
            TrySendError::Full(_) => {
                io::Error::new(io::ErrorKind::WouldBlock, "terminal input queue is full")
            }
            TrySendError::Disconnected(_) => {
                io::Error::new(io::ErrorKind::BrokenPipe, "terminal input worker stopped")
            }
        })
    }

    /// Send a command as if typed and followed by Enter. This is for the
    /// dock's initial command line; interactive key events should use `write`.
    pub fn send_command(&mut self, command: &str) -> io::Result<()> {
        if command.as_bytes().contains(&0) || command.contains('\r') || command.contains('\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal command contains a control separator",
            ));
        }
        if command.len() >= MAX_INPUT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal command exceeds 16 KiB",
            ));
        }
        let mut bytes = command.as_bytes().to_vec();
        bytes.push(b'\r');
        self.write(&bytes)
    }

    /// Resize the PTY and ANSI grid together. Repeated sizes are no-ops, which
    /// prevents a perpetual repaint during normal layout passes.
    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<bool> {
        let next = Dimensions::new(cols, rows);
        if next == self.size {
            return Ok(false);
        }
        let master = self
            .master
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"))?;
        master
            .resize(next.pty_size())
            .map_err(|error| io::Error::other(error.to_string()))?;
        if let Ok(mut parser) = self.shared.parser.lock() {
            parser.screen_mut().set_size(next.rows, next.cols);
            self.size = next;
            self.shared.changed();
            Ok(true)
        } else {
            Err(io::Error::other("terminal screen lock poisoned"))
        }
    }

    /// Scroll the retained history. Positive rows move toward older output.
    pub fn scroll(&mut self, rows: i32) -> bool {
        let Ok(mut parser) = self.shared.parser.lock() else {
            return false;
        };
        let screen = parser.screen_mut();
        let previous = screen.scrollback();
        let target = if rows >= 0 {
            previous.saturating_add(rows as usize)
        } else {
            previous.saturating_sub(rows.unsigned_abs() as usize)
        };
        screen.set_scrollback(target);
        if screen.scrollback() == previous {
            return false;
        }
        self.shared.changed();
        true
    }

    /// Check for output or process exit without blocking. The caller only
    /// needs to update the view when this returns true.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if matches!(&self.status, SessionStatus::Running) {
            if let Some(child) = self.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        self.status = SessionStatus::Exited(status.exit_code());
                        changed = true;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        self.status = SessionStatus::Failed(error.to_string());
                        changed = true;
                    }
                }
            }
        }
        let revision = self.shared.revision.load(Ordering::Acquire);
        if revision != self.observed_revision {
            self.observed_revision = revision;
            changed = true;
        }
        changed
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        let parser = self.shared.parser.lock().unwrap_or_else(|e| e.into_inner());
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let cursor = screen.cursor_position();
        let visible_cursor = (screen.scrollback() == 0 && !screen.hide_cursor()).then_some((
            cursor.0.min(rows.saturating_sub(1)),
            cursor.1.min(cols.saturating_sub(1)),
        ));
        let error = match &self.status {
            SessionStatus::Failed(reason) => Some(reason.clone()),
            _ => self
                .shared
                .error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        };
        TerminalSnapshot {
            lines: screen.rows(0, cols).collect(),
            styled_lines: (0..rows)
                .map(|row| styled_row(screen, row, cols, visible_cursor))
                .collect(),
            cursor,
            cols,
            rows,
            scrollback_position: screen.scrollback(),
            running: matches!(&self.status, SessionStatus::Running),
            exit_code: match &self.status {
                SessionStatus::Exited(code) => Some(*code),
                _ => None,
            },
            error,
            revision: self.shared.revision.load(Ordering::Acquire),
        }
    }

    /// Stop the child and release PTY handles. Worker threads finish when the
    /// associated pipe reaches EOF; they are never joined on the UI thread.
    pub fn close(&mut self) {
        if matches!(&self.status, SessionStatus::Closed) {
            return;
        }
        self.shared.closed.store(true, Ordering::Release);
        self.input.take();
        let resources = Arc::new(Mutex::new((self.child.take(), self.master.take())));
        let worker_resources = Arc::clone(&resources);
        // ConPTY teardown can block while its pipes are drained. Move both
        // termination and handle release off the render thread.
        if thread::Builder::new()
            .name("openide-terminal-reap".to_owned())
            .spawn(move || stop_resources(worker_resources))
            .is_err()
        {
            // An OS thread creation failure is rare, but still terminate the
            // child rather than leaving a process behind.
            stop_resources(resources);
        }
        self.status = SessionStatus::Closed;
        self.shared.changed();
    }
}

fn styled_row(
    screen: &vt100::Screen,
    row: u16,
    cols: u16,
    visible_cursor: Option<(u16, u16)>,
) -> Vec<TerminalSpan> {
    let mut spans: Vec<TerminalSpan> = Vec::new();
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else {
            continue;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        let mut style = TerminalStyle {
            foreground: cell.fgcolor().into(),
            background: cell.bgcolor().into(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        };
        if visible_cursor == Some((row, col)) {
            style.inverse = !style.inverse;
        }
        let content = if cell.has_contents() {
            cell.contents()
        } else {
            " "
        };
        if let Some(last) = spans.last_mut().filter(|last| last.style == style) {
            last.text.push_str(content);
        } else {
            spans.push(TerminalSpan {
                text: content.to_owned(),
                style,
            });
        }
    }
    spans
}

type TerminalResources = (
    Option<Box<dyn Child + Send + Sync>>,
    Option<Box<dyn MasterPty + Send>>,
);

fn stop_resources(resources: Arc<Mutex<TerminalResources>>) {
    let (child, master) = {
        let mut guard = resources.lock().unwrap_or_else(|error| error.into_inner());
        (guard.0.take(), guard.1.take())
    };
    if let Some(mut child) = child {
        let _ = child.kill();
        drop(master);
        let _ = child.wait();
    } else {
        drop(master);
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.close();
    }
}

fn start_reader(mut reader: Box<dyn Read + Send>, shared: Arc<SharedScreen>) -> io::Result<()> {
    thread::Builder::new()
        .name("openide-terminal-read".to_owned())
        .spawn(move || {
            let mut chunk = [0; READ_CHUNK_BYTES];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(len) => {
                        if shared.closed.load(Ordering::Acquire) {
                            break;
                        }
                        match shared.parser.lock() {
                            Ok(mut parser) => {
                                parser.process(&chunk[..len]);
                                shared.changed();
                            }
                            Err(_) => break,
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        shared.set_error(error);
                        break;
                    }
                }
            }
        })
        .map(|_| ())
}

fn start_writer(
    mut writer: Box<dyn Write + Send>,
    input: mpsc::Receiver<Vec<u8>>,
    shared: Arc<SharedScreen>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("openide-terminal-write".to_owned())
        .spawn(move || {
            while let Ok(bytes) = input.recv() {
                if shared.closed.load(Ordering::Acquire) {
                    break;
                }
                if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                    shared.set_error(error);
                    break;
                }
            }
        })
        .map(|_| ())
}

fn default_shell() -> (OsString, Vec<OsString>) {
    if let Some(shell) = std::env::var_os("OPENIDE_TERMINAL_SHELL").filter(|s| !s.is_empty()) {
        return (shell, Vec::new());
    }
    #[cfg(windows)]
    {
        (
            OsString::from("powershell.exe"),
            vec![OsString::from("-NoLogo")],
        )
    }
    #[cfg(not(windows))]
    {
        (
            std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("sh")),
            Vec::new(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn dimensions_cap_screen_memory() {
        assert_eq!(Dimensions::new(0, 0), Dimensions { cols: 2, rows: 1 });
        assert_eq!(
            Dimensions::new(u16::MAX, u16::MAX),
            Dimensions {
                cols: MAX_COLUMNS,
                rows: MAX_ROWS
            }
        );
    }

    #[test]
    fn ansi_output_is_rendered_as_screen_cells() {
        let shared = SharedScreen::new(Dimensions::new(16, 3), None);
        shared
            .parser
            .lock()
            .unwrap()
            .process(b"first\r\n\x1b[31mred\x1b[0m");
        let parser = shared.parser.lock().unwrap();
        let rows: Vec<_> = parser.screen().rows(0, 16).collect();
        assert!(rows.iter().any(|row| row.contains("first")));
        assert!(rows.iter().any(|row| row.contains("red")));
        assert!(rows.iter().all(|row| !row.contains('\u{1b}')));
        assert_eq!(
            parser.screen().cell(1, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
    }

    #[test]
    fn answers_conpty_cursor_query_and_marks_visible_cursor() {
        let size = Dimensions::new(16, 3);
        let (reply, input) = mpsc::sync_channel(2);
        let reply = Arc::new(reply);
        let shared = Arc::new(SharedScreen::new(size, Some(Arc::downgrade(&reply))));
        shared.parser.lock().unwrap().process(b"hi\x1b[6n\x1b[5n");
        assert_eq!(input.try_recv().unwrap(), b"\x1b[1;3R");
        assert_eq!(input.try_recv().unwrap(), b"\x1b[0n");
        let session = TerminalSession {
            master: None,
            child: None,
            input: None,
            shared,
            observed_revision: 0,
            status: SessionStatus::Exited(0),
            size,
        };
        let snapshot = session.snapshot();
        assert!(snapshot.styled_lines[0]
            .iter()
            .any(|span| span.style.inverse));
    }

    #[test]
    fn poll_reports_a_revision_once_instead_of_repainting_forever() {
        let size = Dimensions::new(16, 3);
        let shared = Arc::new(SharedScreen::new(size, None));
        let mut session = TerminalSession {
            master: None,
            child: None,
            input: None,
            shared: Arc::clone(&shared),
            observed_revision: 0,
            status: SessionStatus::Exited(0),
            size,
        };
        assert!(!session.poll());
        shared.parser.lock().unwrap().process(b"hello");
        shared.changed();
        assert!(session.poll());
        assert!(!session.poll());
    }

    #[test]
    fn terminal_history_does_not_exceed_the_configured_limit() {
        let size = Dimensions::new(8, 2);
        let mut parser = vt100::Parser::new(size.rows, size.cols, SCROLLBACK_LINES);
        for _ in 0..(SCROLLBACK_LINES + 200) {
            parser.process(b"line\r\n");
        }
        parser.screen_mut().set_scrollback(usize::MAX);
        assert!(parser.screen().scrollback() <= SCROLLBACK_LINES);
    }

    #[test]
    fn launches_real_process_and_reports_exit() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let (program, args) = (
            OsString::from("cmd.exe"),
            vec![
                OsString::from("/D"),
                OsString::from("/C"),
                OsString::from("echo openide-pty-test"),
            ],
        );
        #[cfg(not(windows))]
        let (program, args) = (
            OsString::from("sh"),
            vec![
                OsString::from("-c"),
                OsString::from("printf openide-pty-test"),
            ],
        );
        let mut session =
            TerminalSession::spawn_with_command(directory.path(), 40, 4, &program, &args).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_output = false;
        while Instant::now() < deadline {
            session.poll();
            saw_output |= session
                .snapshot()
                .lines
                .iter()
                .any(|row| row.contains("openide-pty-test"));
            if saw_output && !session.snapshot().running {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            saw_output,
            "PTY output never reached the screen: {:?}",
            session.snapshot()
        );
        assert_eq!(session.snapshot().exit_code, Some(0));
    }

    #[test]
    fn rejects_oversized_input_without_allocating_it() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let program = OsString::from("cmd.exe");
        #[cfg(not(windows))]
        let program = OsString::from("sh");
        let mut session =
            TerminalSession::spawn_with_command(directory.path(), 40, 4, &program, &[]).unwrap();
        let input = vec![b'x'; MAX_INPUT_BYTES + 1];
        assert_eq!(
            session.write(&input).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        session.close();
        assert!(!session.snapshot().running);
    }
}
