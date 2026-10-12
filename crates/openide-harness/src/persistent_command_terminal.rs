//! Host view of a started command, and the Unix terminal it can run on.
//!
//! The tap copies every output byte for a host tab without touching the
//! agent's poll cursor or the durable capture. On Unix a host can ask for the
//! process to run on a pseudo-terminal: the process then has a controlling
//! terminal, is its own session leader and sees `TERM`; its environment,
//! working directory, approval and process-tree containment are unchanged.

use super::PersistentCommandExit;
use std::sync::{mpsc, Mutex};

const TAP_BACKLOG_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistentTerminalEvent {
    Output(Vec<u8>),
    Exited(PersistentCommandExit),
}

#[derive(Default)]
pub(super) struct TerminalTap {
    state: Mutex<TapState>,
}

#[derive(Default)]
struct TapState {
    backlog: Vec<u8>,
    exit: Option<PersistentCommandExit>,
    subscriber: Option<mpsc::Sender<PersistentTerminalEvent>>,
}

impl TerminalTap {
    pub(super) fn push(&self, bytes: &[u8]) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.backlog.extend_from_slice(bytes);
        let excess = state.backlog.len().saturating_sub(TAP_BACKLOG_BYTES);
        state.backlog.drain(..excess);
        if let Some(subscriber) = &state.subscriber {
            if subscriber
                .send(PersistentTerminalEvent::Output(bytes.to_vec()))
                .is_err()
            {
                state.subscriber = None;
            }
        }
    }

    pub(super) fn finish(&self, exit: &PersistentCommandExit) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.exit.is_some() {
            return;
        }
        state.exit = Some(exit.clone());
        if let Some(subscriber) = state.subscriber.take() {
            let _ = subscriber.send(PersistentTerminalEvent::Exited(exit.clone()));
        }
    }

    /// The newest attachment wins; it first receives the retained tail.
    pub(super) fn attach(&self) -> mpsc::Receiver<PersistentTerminalEvent> {
        let (sender, receiver) = mpsc::channel();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.backlog.is_empty() {
            let _ = sender.send(PersistentTerminalEvent::Output(state.backlog.clone()));
        }
        match &state.exit {
            Some(exit) => {
                let _ = sender.send(PersistentTerminalEvent::Exited(exit.clone()));
            }
            None => state.subscriber = Some(sender),
        }
        receiver
    }
}

/// What the agent's poll, the durable capture and the transcript read of a
/// command on a host terminal: no escape sequences and `\n` line ends, as
/// pipes gave them. The host tab keeps the raw bytes.
#[derive(Default)]
pub(super) struct PlainText {
    ansi: crate::output_condense::AnsiStripper,
    held_return: bool,
}

impl PlainText {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut stripped = Vec::with_capacity(bytes.len());
        self.ansi.strip(bytes, &mut stripped);
        let mut out = Vec::with_capacity(stripped.len());
        for byte in stripped {
            // The terminal turns `\n` into `\r\n`; any other return is kept.
            if std::mem::take(&mut self.held_return) && byte != b'\n' {
                out.push(b'\r');
            }
            if byte == b'\r' {
                self.held_return = true;
            } else {
                out.push(byte);
            }
        }
        out
    }
}

/// The typing half of a host tab, for a host that reads the events on another
/// thread. It exists only for a command on a terminal.
#[derive(Clone)]
pub struct PersistentTerminalInput {
    #[cfg(unix)]
    master: std::sync::Arc<unix::Master>,
}

impl PersistentTerminalInput {
    /// Bytes typed by the user in the host tab.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(unix)]
        return self.master.write_blocking(bytes);
        #[cfg(not(unix))]
        {
            let _ = bytes;
            Err(std::io::Error::other("command has no terminal"))
        }
    }

    pub fn resize(&self, columns: u16, rows: u16) -> std::io::Result<()> {
        #[cfg(unix)]
        return self.master.resize(columns, rows);
        #[cfg(not(unix))]
        {
            let _ = (columns, rows);
            Err(std::io::Error::other("command has no terminal"))
        }
    }
}

/// A command that just started, for a host that asked to observe starts so
/// it can show each one as a tab.
pub struct PersistentCommandLaunch {
    pub owner_session_id: Option<String>,
    /// The manager instance: tells a revoked or replaced manager apart.
    pub generation: String,
    pub id: String,
    /// Literal program followed by its arguments.
    pub argv: Vec<String>,
    pub terminal: PersistentTerminal,
}

pub type PersistentCommandStartObserver =
    std::sync::Arc<dyn Fn(PersistentCommandLaunch) + Send + Sync>;

/// A host tab's handle on one started command. Dropping it changes nothing
/// for the process. Output is a copy; input and resize exist only when the
/// command runs on a terminal.
pub struct PersistentTerminal {
    pub(super) events: mpsc::Receiver<PersistentTerminalEvent>,
    #[cfg(unix)]
    pub(super) master: Option<std::sync::Arc<unix::Master>>,
}

impl PersistentTerminal {
    /// Whether the user can type into it. Pipe-backed commands (the contained
    /// profile) are a read-only mirror.
    pub fn interactive(&self) -> bool {
        #[cfg(unix)]
        return self.master.is_some();
        #[cfg(not(unix))]
        false
    }

    pub fn try_event(&self) -> Option<PersistentTerminalEvent> {
        self.events.try_recv().ok()
    }

    /// Waits for the next event. `None` once the exit was delivered, or when
    /// the command is gone without one (its manager was dropped).
    pub fn next_event(&self) -> Option<PersistentTerminalEvent> {
        self.events.recv().ok()
    }

    pub fn input(&self) -> Option<PersistentTerminalInput> {
        #[cfg(unix)]
        return self
            .master
            .clone()
            .map(|master| PersistentTerminalInput { master });
        #[cfg(not(unix))]
        None
    }

    /// Bytes typed by the user in the host tab.
    pub fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(unix)]
        if let Some(master) = &self.master {
            return master.write_blocking(bytes);
        }
        let _ = bytes;
        Err(std::io::Error::other("command has no terminal"))
    }

    pub fn resize(&self, columns: u16, rows: u16) -> std::io::Result<()> {
        #[cfg(unix)]
        if let Some(master) = &self.master {
            return master.resize(columns, rows);
        }
        let _ = (columns, rows);
        Err(std::io::Error::other("command has no terminal"))
    }
}

#[cfg(unix)]
pub(super) mod unix {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{ready, Context, Poll};
    use tokio::io::unix::AsyncFd;
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    pub(crate) struct Master(AsyncFd<OwnedFd>);

    fn winsize(columns: u16, rows: u16) -> libc::winsize {
        libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    /// Open a pseudo-terminal: a nonblocking master registered with the
    /// current runtime, and the slave the child will use as stdio.
    pub(crate) fn open(columns: u16, rows: u16) -> io::Result<(Arc<Master>, OwnedFd)> {
        let (mut master, mut slave) = (-1, -1);
        let size = winsize(columns, rows);
        // SAFETY: openpty writes two descriptors into valid locals; the name
        // and termios pointers may be null and the size outlives the call.
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &size as *const _ as *mut _,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both descriptors were just returned to us and are unowned.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        for fd in [&master, &slave] {
            // SAFETY: fcntl on a descriptor we own.
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: fcntl on a descriptor we own.
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((Arc::new(Master(AsyncFd::new(master)?)), slave))
    }

    /// Runs in the child between fork and exec, after stdio is the slave: a
    /// new session (hence a new process group whose id is the child's pid,
    /// which the tree guard kills) with the slave as controlling terminal.
    pub(crate) fn become_session_leader() -> io::Result<()> {
        // SAFETY: async-signal-safe calls on the child's own stdin.
        if unsafe { libc::setsid() } < 0
            || unsafe { libc::ioctl(0, libc::TIOCSCTTY as _, 0) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    impl Master {
        fn raw_write(&self, bytes: &[u8]) -> io::Result<usize> {
            // SAFETY: the buffer is valid for its length for the whole call.
            let written =
                unsafe { libc::write(self.0.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
            if written < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(written as usize)
            }
        }

        /// For the host's UI thread: bounded, never waits on the runtime.
        pub(crate) fn write_blocking(&self, mut bytes: &[u8]) -> io::Result<()> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !bytes.is_empty() {
                match self.raw_write(bytes) {
                    Ok(written) => bytes = &bytes[written..],
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }

        pub(crate) fn resize(&self, columns: u16, rows: u16) -> io::Result<()> {
            if !(2..=1000).contains(&columns) || !(2..=1000).contains(&rows) {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let size = winsize(columns, rows);
            // SAFETY: TIOCSWINSZ reads one winsize from a valid pointer.
            if unsafe { libc::ioctl(self.0.as_raw_fd(), libc::TIOCSWINSZ as _, &size) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    /// One async end over the shared master; reader and writer each hold one.
    pub(crate) struct MasterIo(pub(crate) Arc<Master>);

    impl AsyncRead for MasterIo {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            loop {
                let mut guard = ready!(self.0 .0.poll_read_ready(cx))?;
                let unfilled = buf.initialize_unfilled();
                let read = guard.try_io(|fd| {
                    // SAFETY: the slice is valid and writable for its length.
                    let read = unsafe {
                        libc::read(fd.as_raw_fd(), unfilled.as_mut_ptr().cast(), unfilled.len())
                    };
                    if read < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(read as usize)
                    }
                });
                match read {
                    Ok(Ok(read)) => {
                        buf.advance(read);
                        return Poll::Ready(Ok(()));
                    }
                    // Linux reports the last slave closing as EIO: end of output.
                    Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => {
                        return Poll::Ready(Ok(()))
                    }
                    Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                    Ok(Err(error)) => return Poll::Ready(Err(error)),
                    Err(_would_block) => {}
                }
            }
        }
    }

    impl AsyncWrite for MasterIo {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            loop {
                let mut guard = ready!(self.0 .0.poll_write_ready(cx))?;
                match guard.try_io(|_| self.0.raw_write(bytes)) {
                    Ok(result) => return Poll::Ready(result),
                    Err(_would_block) => {}
                }
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
}
