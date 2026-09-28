//! Launch non-interactive helpers without allocating a Windows console.
//!
//! The GUI is built with the Windows subsystem, so a normal `Command::new`
//! would briefly create a console for each Git or GitHub CLI invocation.
//! Interactive terminal sessions use their own PTY and must not use this helper.

use std::ffi::OsStr;
use std::process::Command;

pub(crate) fn command_no_window(program: impl AsRef<OsStr>) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new(program);
        // Win32 CREATE_NO_WINDOW. This keeps the process and its redirected
        // stdin/stdout/stderr functional while avoiding a transient console.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    {
        Command::new(program)
    }
}
