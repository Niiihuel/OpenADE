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
        Arc, Condvar, Mutex, Weak,
    },
    thread,
};

use openide_harness::{
    PersistentCommandExit, PersistentTerminal, PersistentTerminalEvent, PersistentTerminalInput,
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// Limits terminal history even if a command writes indefinitely.
pub const SCROLLBACK_LINES: usize = 1_000;
pub const MAX_COLUMNS: u16 = 240;
pub const MAX_ROWS: u16 = 120;
pub const FONT_SIZE: f32 = 12.0;
pub const CELL_WIDTH: f32 = 7.2;
pub const LINE_HEIGHT: f32 = 18.0;
// Keep the terminal grid close to its header while retaining horizontal room
// for text selection. Rendering, PTY dimensions and pointer mapping share these.
pub const SCREEN_PADDING_X: f32 = 12.0;
pub const SCREEN_PADDING_Y: f32 = 4.0;
const READ_CHUNK_BYTES: usize = 8 * 1024;
const MAX_INPUT_BYTES: usize = 16 * 1024;
// Keep the former 64 × 16 KiB memory ceiling, but budget bytes rather than
// individual key events. A busy frame can legitimately deliver hundreds of
// one-character events before the PTY writer gets scheduled.
const MAX_PENDING_INPUT_BYTES: usize = 64 * MAX_INPUT_BYTES;

#[derive(Default)]
struct PendingInput {
    bytes: Vec<u8>,
    closed: bool,
}

#[derive(Default)]
struct InputQueue {
    pending: Mutex<PendingInput>,
    ready: Condvar,
}

impl InputQueue {
    fn push(&self, bytes: &[u8]) -> io::Result<()> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| io::Error::other("terminal input lock poisoned"))?;
        if pending.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "terminal input worker stopped",
            ));
        }
        if bytes.len() > MAX_PENDING_INPUT_BYTES.saturating_sub(pending.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "terminal input queue exceeds 1 MiB",
            ));
        }
        // Appending under one short lock preserves ordering across keyboard,
        // paste and terminal-protocol responses. PTY I/O never holds this lock.
        let required = pending.bytes.len() + bytes.len();
        if required > pending.bytes.capacity() {
            let capacity = pending
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(required)
                .min(MAX_PENDING_INPUT_BYTES);
            let additional = capacity - pending.bytes.len();
            pending.bytes.reserve_exact(additional);
        }
        pending.bytes.extend_from_slice(bytes);
        drop(pending);
        self.ready.notify_one();
        Ok(())
    }

    fn take(&self) -> io::Result<Option<Vec<u8>>> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| io::Error::other("terminal input lock poisoned"))?;
        while pending.bytes.is_empty() && !pending.closed {
            pending = self
                .ready
                .wait(pending)
                .map_err(|_| io::Error::other("terminal input lock poisoned"))?;
        }
        if pending.closed {
            return Ok(None);
        }
        Ok(Some(std::mem::take(&mut pending.bytes)))
    }

    fn close(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.closed = true;
            pending.bytes.clear();
        }
        self.ready.notify_all();
    }
}

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
    // Parser callbacks do not own the input queue's lifetime. Explicit session
    // closure wakes the writer and rejects any later protocol responses.
    reply: Option<Weak<InputQueue>>,
    /// The window title the running program last asked for.
    title: String,
}

impl vt100::Callbacks for TerminalCallbacks {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title)
            .chars()
            .filter(|character| !character.is_control())
            .take(160)
            .collect();
    }

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
        let _ = reply.push(&response);
    }
}

struct SharedScreen {
    parser: Mutex<vt100::Parser<TerminalCallbacks>>,
    error: Mutex<Option<String>>,
    revision: AtomicU64,
    closed: AtomicBool,
    visible: AtomicBool,
    repaint: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl SharedScreen {
    fn new(size: Dimensions, reply: Option<Weak<InputQueue>>) -> Self {
        Self {
            parser: Mutex::new(vt100::Parser::new_with_callbacks(
                size.rows,
                size.cols,
                SCROLLBACK_LINES,
                TerminalCallbacks {
                    reply,
                    title: String::new(),
                },
            )),
            error: Mutex::new(None),
            revision: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            visible: AtomicBool::new(true),
            repaint: Mutex::new(None),
        }
    }

    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::Release);
        if self.visible.load(Ordering::Relaxed) {
            if let Ok(repaint) = self.repaint.lock() {
                if let Some(repaint) = &*repaint {
                    repaint();
                }
            }
        }
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
    pub selected: bool,
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
    pub shell: String,
    pub alternate_screen: bool,
    pub selection: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Selection {
    anchor: (u16, u16),
    cursor: (u16, u16),
    dragging: bool,
}

impl Selection {
    fn range(self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    fn contains(self, row: u16, col: u16) -> bool {
        let (start, end) = self.range();
        (row, col) >= start && (row, col) < end
    }
}

#[derive(Debug)]
enum SessionStatus {
    Running,
    Exited(u32),
    Failed(String),
    /// An agent's command that ended without an exit code.
    Stopped,
    Closed,
}

/// What ties a session to a command the agent started instead of a shell.
/// The process belongs to the agent's command manager, never to the tab.
struct AgentLink {
    /// `None`: a pipe-backed command, shown as a read-only mirror.
    input: Option<PersistentTerminalInput>,
    exit: Arc<Mutex<Option<PersistentCommandExit>>>,
    /// The address its server printed, once something answers there.
    preview: Arc<Mutex<Option<String>>>,
}

/// The reader's view of the command's output copy.
struct AgentOutput {
    terminal: PersistentTerminal,
    pending: Vec<u8>,
    mirror: bool,
    exit: Arc<Mutex<Option<PersistentCommandExit>>>,
    /// Reads the start of the output for the address of a server.
    watch: Option<crate::preview::OutputWatch>,
}

impl Read for AgentOutput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.pending.is_empty() {
            let event = self.terminal.next_event();
            if let Some(PersistentTerminalEvent::Output(bytes)) = &event {
                if self.watch.as_mut().is_some_and(|watch| !watch.feed(bytes)) {
                    self.watch = None;
                }
            }
            match event {
                Some(PersistentTerminalEvent::Output(bytes)) if self.mirror => {
                    // Pipes carry bare line feeds; a screen needs the return too.
                    for byte in bytes {
                        if byte == b'\n' {
                            self.pending.push(b'\r');
                        }
                        self.pending.push(byte);
                    }
                }
                Some(PersistentTerminalEvent::Output(bytes)) => self.pending = bytes,
                ended => {
                    // No exit event: the manager is gone, and its trees with it.
                    let exit = match ended {
                        Some(PersistentTerminalEvent::Exited(exit)) => exit,
                        _ => PersistentCommandExit::Terminated,
                    };
                    *self.exit.lock().unwrap_or_else(|e| e.into_inner()) = Some(exit);
                    return Ok(0);
                }
            }
        }
        let count = buffer.len().min(self.pending.len());
        buffer[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

struct AgentInput(PersistentTerminalInput);

impl Write for AgentInput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes).map(|()| bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One interactive shell. Dropping the session terminates its child process.
/// The read/write workers never block the render thread on PTY I/O.
pub struct TerminalSession {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    input: Option<Arc<InputQueue>>,
    shared: Arc<SharedScreen>,
    observed_revision: u64,
    status: SessionStatus,
    size: Dimensions,
    shell: String,
    selection: Option<Selection>,
    scroll_remainder: f32,
    agent: Option<AgentLink>,
}

impl TerminalSession {
    /// Show a command the agent started: a copy of its output and, when it
    /// runs on a terminal, the user's keys, paste and size. Closing or
    /// dropping the session never stops the process. With `watch_preview`
    /// the start of the output is read for the address of a server.
    pub fn attach(
        terminal: PersistentTerminal,
        label: &str,
        watch_preview: bool,
    ) -> io::Result<Self> {
        // The size the harness opens a host terminal with, so the retained
        // tail lays out as printed; the dock resizes both when it shows them.
        let size = Dimensions::new(120, 30);
        let handle = terminal.input();
        let input = handle.as_ref().map(|_| Arc::new(InputQueue::default()));
        let shared = Arc::new(SharedScreen::new(size, input.as_ref().map(Arc::downgrade)));
        let exit = Arc::new(Mutex::new(None));
        let preview = Arc::new(Mutex::new(None));
        let woken = Arc::clone(&shared);
        start_reader(
            Box::new(AgentOutput {
                terminal,
                pending: Vec::new(),
                mirror: handle.is_none(),
                exit: Arc::clone(&exit),
                watch: watch_preview.then(|| {
                    crate::preview::OutputWatch::new(
                        Arc::clone(&preview),
                        Arc::new(move || woken.changed()),
                    )
                }),
            }),
            Arc::clone(&shared),
        )?;
        if let (Some(handle), Some(input)) = (&handle, &input) {
            start_writer(
                Box::new(AgentInput(handle.clone())),
                Arc::clone(input),
                Arc::clone(&shared),
            )?;
        }
        Ok(Self {
            master: None,
            child: None,
            input,
            shared,
            observed_revision: 0,
            status: SessionStatus::Running,
            size,
            shell: label.to_owned(),
            selection: None,
            scroll_remainder: 0.0,
            agent: Some(AgentLink {
                input: handle,
                exit,
                preview,
            }),
        })
    }

    /// Where the agent's command serves, once that address answered.
    pub fn agent_preview(&self) -> Option<String> {
        self.agent
            .as_ref()?
            .preview
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// How the agent's command ended, once it has. `None` for a shell.
    pub fn agent_exit(&self) -> Option<PersistentCommandExit> {
        self.agent
            .as_ref()?
            .exit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Cached process identity; reading the selector never snapshots the PTY.
    pub fn shell_label(&self) -> &str { &self.shell }

    /// The reader wakes the host when output arrives. Register once, avoiding
    /// a high-frequency idle timer just to make interactive input responsive.
    pub fn set_repaint_callback(&mut self, callback: impl Fn() + Send + Sync + 'static) {
        if let Ok(mut repaint) = self.shared.repaint.lock() {
            if repaint.is_none() {
                *repaint = Some(Arc::new(callback));
            }
        }
    }

    pub fn set_visible(&self, visible: bool) {
        self.shared.visible.store(visible, Ordering::Relaxed);
    }

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
        Self::spawn_with_env(workspace, cols, rows, program, args, &[])
    }

    /// Like `spawn_with_command`, with extra variables for the child only.
    pub fn spawn_with_env(
        workspace: &Path,
        cols: u16,
        rows: u16,
        program: &OsStr,
        args: &[OsString],
        env: &[(&str, String)],
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
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "openADE");
        command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        for (key, value) in env {
            command.env(key, value);
        }
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
        let input = Arc::new(InputQueue::default());
        let shared = Arc::new(SharedScreen::new(size, Some(Arc::downgrade(&input))));
        if let Err(error) = start_reader(reader, Arc::clone(&shared)) {
            let _ = child.kill();
            return Err(error);
        }
        if let Err(error) = start_writer(writer, Arc::clone(&input), Arc::clone(&shared)) {
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
            shell: Path::new(program)
                .file_name()
                .unwrap_or(program)
                .to_string_lossy()
                .trim_end_matches(".exe")
                .to_owned(),
            selection: None,
            scroll_remainder: 0.0,
            agent: None,
        })
    }

    /// Queue raw PTY input without waiting for the shell to consume it. Enter
    /// is `b"\r"`; special keys use their standard terminal escape sequences.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        // A read-only mirror takes no keys; its notice says so.
        if self.agent.as_ref().is_some_and(|agent| agent.input.is_none()) {
            return Ok(());
        }
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
        input.push(bytes)?;
        // Input always returns to the live prompt, including after a history
        // selection. Keep this in the session so all input routes agree.
        self.selection = None;
        self.scroll_remainder = 0.0;
        self.scroll(i32::MIN);
        self.shared.changed();
        Ok(())
    }

    /// Clipboard text is a single edit in shells that support bracketed
    /// paste. Strip escape/control bytes before wrapping so pasted output
    /// cannot terminate the bracket or inject terminal control sequences.
    pub fn paste(&mut self, text: &str) -> io::Result<()> {
        if text.len() > MAX_INPUT_BYTES - 12 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "el texto pegado supera los 16 KiB",
            ));
        }
        let bracketed = self
            .shared
            .parser
            .lock()
            .map_err(|_| io::Error::other("terminal screen lock poisoned"))?
            .screen()
            .bracketed_paste();
        self.write(&paste_bytes(text, bracketed))
    }

    /// Mode-aware special keys are required by full-screen programs such as
    /// vim and less; their cursor mode differs from the shell's default.
    pub fn key(&mut self, key: openide_ui::Key, shift: bool, ctrl: bool) -> io::Result<bool> {
        let application_cursor = self
            .shared
            .parser
            .lock()
            .map_err(|_| io::Error::other("terminal screen lock poisoned"))?
            .screen()
            .application_cursor();
        let Some(bytes) = key_bytes(key, shift, ctrl, application_cursor) else {
            return Ok(false);
        };
        self.write(&bytes)?;
        Ok(true)
    }

    pub fn begin_selection(&mut self, row: u16, col: u16) {
        let point = (row.min(self.size.rows - 1), col.min(self.size.cols));
        self.selection = Some(Selection {
            anchor: point,
            cursor: point,
            dragging: true,
        });
        self.shared.changed();
    }

    pub fn extend_selection(&mut self, row: u16, col: u16) -> bool {
        let Some(selection) = self.selection.as_mut().filter(|s| s.dragging) else {
            return false;
        };
        let point = (row.min(self.size.rows - 1), col.min(self.size.cols));
        if selection.cursor != point {
            selection.cursor = point;
            self.shared.changed();
        }
        true
    }

    pub fn end_selection(&mut self) -> bool {
        if let Some(selection) = self.selection.as_mut().filter(|s| s.dragging) {
            selection.dragging = false;
            true
        } else {
            false
        }
    }

    /// Preserve touchpad sub-line deltas instead of rounding each event to
    /// zero. Normal wheel input and touchpad input share the same history.
    pub fn scroll_pixels(&mut self, delta: f32) -> bool {
        if !delta.is_finite() {
            return false;
        }
        self.scroll_remainder += delta;
        let rows = (self.scroll_remainder / LINE_HEIGHT).trunc() as i32;
        self.scroll_remainder -= rows as f32 * LINE_HEIGHT;
        if rows == 0 {
            return false;
        }
        let modes = self.shared.parser.lock().ok().map(|parser| {
            (
                parser.screen().alternate_screen(),
                parser.screen().application_cursor(),
            )
        });
        // Full-screen programs own their viewport rather than our shell
        // scrollback. Translate wheel rows to their current cursor mode.
        if let Some((true, application_cursor)) = modes {
            let key = if rows > 0 {
                openide_ui::Key::ArrowUp
            } else {
                openide_ui::Key::ArrowDown
            };
            if let Some(sequence) = key_bytes(key, false, false, application_cursor) {
                return self
                    .write(&sequence.repeat(rows.unsigned_abs().min(24) as usize))
                    .is_ok();
            }
        }
        self.scroll(rows)
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
        if let Some(agent) = &self.agent {
            // shortcut: a refused resize leaves the process with its previous
            // size while the screen follows the pane; surface it if it matters.
            if let (Some(input), SessionStatus::Running) = (&agent.input, &self.status) {
                let _ = input.resize(next.cols, next.rows);
            }
        } else {
            let master = self
                .master
                .as_ref()
                .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"))?;
            master
                .resize(next.pty_size())
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
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
        self.selection = None;
        self.shared.changed();
        true
    }

    /// Check for output or process exit without blocking. The caller only
    /// needs to update the view when this returns true.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if matches!(&self.status, SessionStatus::Running) {
            if let Some(exit) = self.agent_exit() {
                self.status = match exit {
                    PersistentCommandExit::Exited { code: Some(code) } if code >= 0 => {
                        SessionStatus::Exited(code as u32)
                    }
                    _ => SessionStatus::Stopped,
                };
                changed = true;
            } else if let Some(child) = self.child.as_mut() {
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

    /// The window title set by the running program; empty until it sets one.
    pub fn title(&self) -> String {
        self.shared
            .parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .callbacks()
            .title
            .clone()
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        let parser = self.shared.parser.lock().unwrap_or_else(|e| e.into_inner());
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let cursor = screen.cursor_position();
        let visible_cursor = (matches!(self.status, SessionStatus::Running)
            && screen.scrollback() == 0
            && !screen.hide_cursor())
        .then_some((
            cursor.0.min(rows.saturating_sub(1)),
            cursor.1.min(cols.saturating_sub(1)),
        ));
        let error = match &self.status {
            SessionStatus::Failed(reason) => Some(reason.clone()),
            // A key sent as the agent's command ended is not an error to keep.
            SessionStatus::Exited(_) | SessionStatus::Stopped if self.agent.is_some() => None,
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
                .map(|row| styled_row(screen, row, cols, visible_cursor, self.selection))
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
            shell: self.shell.clone(),
            alternate_screen: screen.alternate_screen(),
            selection: self
                .selection
                .and_then(|selection| selected_text(screen, selection)),
        }
    }

    /// Stop the child and release PTY handles. Worker threads finish when the
    /// associated pipe reaches EOF; they are never joined on the UI thread.
    pub fn close(&mut self) {
        if matches!(&self.status, SessionStatus::Closed) {
            return;
        }
        self.shared.closed.store(true, Ordering::Release);
        if let Some(input) = self.input.take() {
            input.close();
        }
        if self.agent.is_some() {
            // Nothing to reap: the process belongs to the agent's manager.
            self.status = SessionStatus::Closed;
            self.shared.changed();
            return;
        }
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
    selection: Option<Selection>,
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
            selected: selection.is_some_and(|selection| selection.contains(row, col)),
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
    // The viewport already owns the full cell-grid background. Empty default
    // cells at the end of a row need no text shaping; keep any cursor,
    // selection or ANSI background spans, and retain spacing before them.
    if let Some(last) = spans
        .last_mut()
        .filter(|span| span.style == TerminalStyle::default())
    {
        last.text.truncate(last.text.trim_end_matches(' ').len());
        if last.text.is_empty() {
            spans.pop();
        }
    }
    spans
}

fn selected_text(screen: &vt100::Screen, selection: Selection) -> Option<String> {
    let (start, end) = selection.range();
    if start == end {
        return None;
    }
    let (_, cols) = screen.size();
    let mut text = String::new();
    for row in start.0..=end.0 {
        let first = if row == start.0 { start.1 } else { 0 };
        let last = if row == end.0 { end.1 } else { cols };
        let mut line = String::new();
        for col in first..last {
            if let Some(cell) = screen.cell(row, col) {
                if !cell.is_wide_continuation() {
                    line.push_str(if cell.has_contents() {
                        cell.contents()
                    } else {
                        " "
                    });
                }
            }
        }
        text.push_str(if screen.row_wrapped(row) {
            &line
        } else {
            line.trim_end_matches(' ')
        });
        if row < end.0 && !screen.row_wrapped(row) {
            text.push('\n');
        }
    }
    Some(text)
}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text: String = text
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\r' | '\n' | '\t'))
        .collect();
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}

pub(crate) fn key_bytes(
    key: openide_ui::Key,
    shift: bool,
    ctrl: bool,
    application_cursor: bool,
) -> Option<Vec<u8>> {
    use openide_ui::Key;
    if ctrl {
        let control = match key {
            Key::A => 1,
            Key::B => 2,
            Key::C => 3,
            Key::D => 4,
            Key::E => 5,
            Key::F => 6,
            Key::G => 7,
            Key::H => 8,
            Key::I => 9,
            Key::J => 10,
            Key::K => 11,
            Key::L => 12,
            Key::M => 13,
            Key::N => 14,
            Key::O => 15,
            Key::P => 16,
            Key::Q => 17,
            Key::R => 18,
            Key::S => 19,
            Key::T => 20,
            Key::U => 21,
            Key::V => 22,
            Key::W => 23,
            Key::X => 24,
            Key::Y => 25,
            Key::Z => 26,
            Key::Space => 0,
            _ => u8::MAX,
        };
        if control != u8::MAX {
            return Some(vec![control]);
        }
    }
    let suffix = match key {
        Key::ArrowUp => Some('A'),
        Key::ArrowDown => Some('B'),
        Key::ArrowRight => Some('C'),
        Key::ArrowLeft => Some('D'),
        Key::Home => Some('H'),
        Key::End => Some('F'),
        _ => None,
    };
    if let Some(suffix) = suffix {
        return Some(if ctrl || shift {
            let modifier = 1 + u8::from(shift) + 4 * u8::from(ctrl);
            format!("\x1b[1;{modifier}{suffix}").into_bytes()
        } else if application_cursor {
            format!("\x1bO{suffix}").into_bytes()
        } else {
            format!("\x1b[{suffix}").into_bytes()
        });
    }
    let bytes = match key {
        Key::Enter => b"\r".as_slice(),
        Key::Tab if shift => b"\x1b[Z",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",
        Key::Backspace if ctrl => b"\x08",
        Key::Backspace => b"\x7f",
        Key::Delete => b"\x1b[3~",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        _ => return None,
    };
    Some(bytes.to_vec())
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
                    // Linux PTYs signal slave closure with EIO, not EOF.
                    // It is normal termination and must not show an error.
                    #[cfg(target_os = "linux")]
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                    Err(error) => {
                        shared.set_error(error);
                        break;
                    }
                }
            }
            // An agent's command reports its exit here; a shell's is polled.
            shared.changed();
        })
        .map(|_| ())
}

fn start_writer(
    mut writer: Box<dyn Write + Send>,
    input: Arc<InputQueue>,
    shared: Arc<SharedScreen>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("openide-terminal-write".to_owned())
        .spawn(move || {
            loop {
                let bytes = match input.take() {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => break,
                    Err(error) => {
                        shared.set_error(error);
                        break;
                    }
                };
                if shared.closed.load(Ordering::Acquire) {
                    break;
                }
                if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                    shared.set_error(error);
                    break;
                }
            }
            input.close();
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
    use std::sync::mpsc;
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
        let reply = Arc::new(InputQueue::default());
        let shared = Arc::new(SharedScreen::new(size, Some(Arc::downgrade(&reply))));
        shared.parser.lock().unwrap().process(b"hi\x1b[6n\x1b[5n");
        assert_eq!(reply.take().unwrap().unwrap(), b"\x1b[1;3R\x1b[0n");
        let session = TerminalSession {
            master: None,
            child: None,
            input: None,
            shared,
            observed_revision: 0,
            status: SessionStatus::Running,
            size,
            shell: "test".into(),
            selection: None,
            scroll_remainder: 0.0,
            agent: None,
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
            shell: "test".into(),
            selection: None,
            scroll_remainder: 0.0,
            agent: None,
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

    #[test]
    fn blocked_writer_preserves_large_keyboard_bursts_in_exact_order() {
        struct BlockFirstWrite {
            started: Option<mpsc::Sender<()>>,
            resume: mpsc::Receiver<()>,
            flushed: mpsc::Sender<Vec<u8>>,
            captured: Vec<u8>,
        }
        impl Write for BlockFirstWrite {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(started) = self.started.take() {
                    started.send(()).unwrap();
                    self.resume.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                self.captured.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                self.flushed.send(self.captured.clone()).unwrap();
                Ok(())
            }
        }

        let mut session = fixture(b"");
        let input = Arc::new(InputQueue::default());
        session.input = Some(Arc::clone(&input));
        let (started, wait_started) = mpsc::channel();
        let (resume, wait_resume) = mpsc::channel();
        let (flushed, wait_flushed) = mpsc::channel();
        start_writer(
            Box::new(BlockFirstWrite {
                started: Some(started),
                resume: wait_resume,
                flushed,
                captured: Vec::new(),
            }),
            Arc::clone(&input),
            Arc::clone(&session.shared),
        )
        .unwrap();
        session.write(b"prefix:").unwrap();
        wait_started.recv_timeout(Duration::from_secs(5)).unwrap();

        let command = "export OPENIDE_E2E_TOKEN=kept; printf 'AGAIN:%s\\n' \"$OPENIDE_E2E_TOKEN\"; printf '%s' \"$$\" > '/tmp/fixture/output'; # café 世界 🧠\r".repeat(8);
        assert!(command.chars().count() > 240);
        for character in command.chars() {
            let mut bytes = [0; 4];
            session
                .write(character.encode_utf8(&mut bytes).as_bytes())
                .unwrap();
        }
        assert_eq!(input.pending.lock().unwrap().bytes, command.as_bytes());
        resume.send(()).unwrap();
        assert_eq!(
            wait_flushed.recv_timeout(Duration::from_secs(5)).unwrap(),
            b"prefix:"
        );
        let mut expected = b"prefix:".to_vec();
        expected.extend_from_slice(command.as_bytes());
        assert_eq!(
            wait_flushed.recv_timeout(Duration::from_secs(5)).unwrap(),
            expected
        );
        session.close();
    }

    #[test]
    fn input_queue_enforces_byte_limit_atomically_and_closes_waiters() {
        let input = Arc::new(InputQueue::default());
        let full = vec![b'x'; MAX_PENDING_INPUT_BYTES];
        input.push(&full).unwrap();
        assert_eq!(
            input.push(b"!").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(input.take().unwrap().unwrap(), full);
        // Rejection does not inject a partial character or poison future input.
        input.push("é".as_bytes()).unwrap();
        assert_eq!(input.take().unwrap().unwrap(), "é".as_bytes());
        let (finished, completion) = mpsc::channel();
        let waiting = Arc::clone(&input);
        thread::spawn(move || finished.send(waiting.take().unwrap()).unwrap());
        input.close();
        assert_eq!(
            completion.recv_timeout(Duration::from_secs(5)).unwrap(),
            None
        );
        assert_eq!(
            input.push(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    fn fixture(bytes: &[u8]) -> TerminalSession {
        let size = Dimensions::new(16, 3);
        let shared = Arc::new(SharedScreen::new(size, None));
        shared.parser.lock().unwrap().process(bytes);
        TerminalSession {
            master: None,
            child: None,
            input: None,
            shared,
            observed_revision: 0,
            status: SessionStatus::Running,
            size,
            shell: "test".into(),
            selection: None,
            scroll_remainder: 0.0,
            agent: None,
        }
    }

    #[test]
    fn bracketed_paste_is_one_edit_and_cannot_end_its_own_envelope() {
        assert_eq!(
            paste_bytes("one\r\ntwo", true),
            b"\x1b[200~one\ntwo\x1b[201~"
        );
        assert_eq!(paste_bytes("one\ntwo", false), b"one\rtwo");
        assert_eq!(
            paste_bytes("a\x1b[201~\0\x03b", true),
            b"\x1b[200~a[201~b\x1b[201~"
        );
    }

    #[test]
    fn interactive_programs_receive_cursor_mode_and_modifiers() {
        use openide_ui::Key;
        assert_eq!(
            key_bytes(Key::ArrowUp, false, false, true).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            key_bytes(Key::ArrowUp, false, false, false).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            key_bytes(Key::ArrowLeft, false, true, false).unwrap(),
            b"\x1b[1;5D"
        );
        assert_eq!(key_bytes(Key::Tab, true, false, false).unwrap(), b"\x1b[Z");
        assert_eq!(
            key_bytes(Key::PageDown, false, false, false).unwrap(),
            b"\x1b[6~"
        );
    }

    #[test]
    fn selection_preserves_unicode_and_logical_line_breaks() {
        let mut session = fixture("héllo\r\n世界!".as_bytes());
        session.begin_selection(0, 0);
        assert!(session.extend_selection(1, 5));
        assert!(session.end_selection());
        let snapshot = session.snapshot();
        assert_eq!(snapshot.selection.as_deref(), Some("héllo\n世界!"));
        assert!(snapshot
            .styled_lines
            .iter()
            .flatten()
            .any(|span| span.style.selected));
        // A backwards drag has exactly the same textual result.
        session.begin_selection(1, 5);
        session.extend_selection(0, 0);
        assert_eq!(session.snapshot().selection, snapshot.selection);
    }

    #[test]
    fn touchpad_accumulates_small_deltas_and_input_returns_to_prompt() {
        let mut session = fixture(b"a\r\nb\r\nc\r\nd\r\ne\r\nf");
        assert!(!session.scroll_pixels(5.0));
        assert!(!session.scroll_pixels(5.0));
        assert!(session.scroll_pixels(8.0));
        assert_eq!(session.snapshot().scrollback_position, 1);
        let input = Arc::new(InputQueue::default());
        session.input = Some(Arc::clone(&input));
        session.begin_selection(0, 0);
        session.extend_selection(0, 1);
        session.write(b"x").unwrap();
        assert_eq!(input.take().unwrap().unwrap(), b"x");
        let snapshot = session.snapshot();
        assert_eq!(snapshot.scrollback_position, 0);
        assert_eq!(snapshot.selection, None);
    }

    #[test]
    fn blank_cells_do_not_allocate_text_runs_but_cursor_and_background_survive() {
        let session = fixture(b"hello");
        let snapshot = session.snapshot();
        assert_eq!(snapshot.lines[0], "hello");
        assert!(snapshot.styled_lines[1].is_empty());
        assert!(snapshot.styled_lines[2].is_empty());
        assert_eq!(
            snapshot.styled_lines[0]
                .iter()
                .map(|span| span.text.len())
                .sum::<usize>(),
            6,
            "five characters plus the visible cursor, without padding the whole row"
        );
        assert!(snapshot.styled_lines[0].last().unwrap().style.inverse);

        let session = fixture(b"\x1b[41m \x1b[0m\x1b[?25l");
        let snapshot = session.snapshot();
        assert_eq!(snapshot.styled_lines[0].len(), 1);
        assert_eq!(snapshot.styled_lines[0][0].text, " ");
        assert_eq!(
            snapshot.styled_lines[0][0].style.background,
            TerminalColor::Indexed(1)
        );
    }

    #[test]
    fn selected_blank_cells_keep_their_columns_after_empty_tail_elision() {
        let mut session = fixture(b"x\x1b[?25l");
        session.begin_selection(0, 2);
        session.extend_selection(0, 8);
        let snapshot = session.snapshot();
        let spans = &snapshot.styled_lines[0];
        assert_eq!(spans[0].text, "x ");
        assert_eq!(spans[1].text, "      ");
        assert!(spans[1].style.selected);
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn output_revision_wakes_the_host_without_an_idle_loop() {
        let mut session = fixture(b"");
        let count = Arc::new(AtomicU64::new(0));
        let callback_count = count.clone();
        session.set_repaint_callback(move || {
            callback_count.fetch_add(1, Ordering::Relaxed);
        });
        assert!(!session.poll());
        assert_eq!(count.load(Ordering::Relaxed), 0);
        session.shared.changed();
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert!(session.poll());
        assert!(!session.poll());
        session.set_visible(false);
        session.shared.changed();
        assert_eq!(
            count.load(Ordering::Relaxed),
            1,
            "hidden PTYs must not repaint the workspace"
        );
        assert!(session.poll());
    }

    #[test]
    fn wheel_in_full_screen_program_uses_its_application_cursor_mode() {
        let mut session = fixture(b"\x1b[?1049h\x1b[?1h");
        let input = Arc::new(InputQueue::default());
        session.input = Some(Arc::clone(&input));
        assert!(session.scroll_pixels(LINE_HEIGHT * 2.0));
        assert_eq!(input.take().unwrap().unwrap(), b"\x1bOA\x1bOA");
    }

    #[cfg(unix)]
    #[test]
    fn real_shell_has_terminal_capabilities_and_clean_exit() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = TerminalSession::spawn_with_command(
            directory.path(),
            80,
            4,
            OsStr::new("sh"),
            &[
                OsString::from("-c"),
                OsString::from("printf '%s/%s' \"$TERM\" \"$COLORTERM\""),
            ],
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            session.poll();
            let snapshot = session.snapshot();
            if !snapshot.running
                && snapshot
                    .lines
                    .iter()
                    .any(|line| line.contains("xterm-256color/truecolor"))
            {
                assert_eq!(snapshot.error, None);
                assert_eq!(snapshot.exit_code, Some(0));
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("shell did not finish: {:?}", session.snapshot());
    }
}
