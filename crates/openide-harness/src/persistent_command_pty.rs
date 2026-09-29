//! Windows ConPTY spawn with the command already in a terminating Job Object.
//!
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST` is essential here: assigning a running
//! process after `CreateProcessW` would leave a window in which it could start
//! descendants outside the job. A PTY request fails before model code runs if
//! either attribute or the job cannot be created.

use super::HOST_BOOTSTRAP_ENV_KEYS;
use std::ffi::{c_void, OsStr};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};

pub(super) struct PtyProcess {
    pub(super) stdin: File,
    pub(super) output: std::fs::File,
    pub(super) process: Arc<OwnedHandle>,
    pub(super) console: PseudoConsole,
    pub(super) job: PtyJob,
}

pub(super) struct SpawnFailure {
    pub(super) error: io::Error,
    pub(super) launched: bool,
}

impl From<io::Error> for SpawnFailure {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            launched: false,
        }
    }
}

pub(super) struct PtyJob {
    job: OwnedHandle,
}

impl PtyJob {
    fn new() -> io::Result<Self> {
        let job = owned_handle(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { job })
    }

    fn raw(&self) -> HANDLE {
        self.job.as_raw_handle()
    }
}

pub(super) struct PseudoConsole {
    raw: HPCON,
    columns: u16,
    rows: u16,
    // ConPTY continues using these pipe ends for its full lifetime.
    _input_read: OwnedHandle,
    _output_write: OwnedHandle,
}

impl PseudoConsole {
    pub(super) fn resize(&mut self, columns: u16, rows: u16) -> io::Result<()> {
        if self.columns == columns && self.rows == rows {
            return Ok(());
        }
        let result = unsafe {
            ResizePseudoConsole(
                self.raw,
                COORD {
                    X: columns as i16,
                    Y: rows as i16,
                },
            )
        };
        if result < 0 {
            return Err(io::Error::other(format!(
                "ResizePseudoConsole failed with HRESULT 0x{:08x}",
                result as u32
            )));
        }
        self.columns = columns;
        self.rows = rows;
        Ok(())
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        unsafe { ClosePseudoConsole(self.raw) };
    }
}

struct AttributeList {
    storage: Vec<usize>,
}

impl AttributeList {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &mut bytes);
        }
        if bytes == 0 || bytes > 64 * 1024 {
            return Err(io::Error::other("invalid process attribute list size"));
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let ok = unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 2, 0, &mut bytes)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { storage })
    }

    fn ptr(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr().cast()
    }

    fn set_pseudoconsole(&mut self, console: HPCON) -> io::Result<()> {
        let ok = unsafe {
            UpdateProcThreadAttribute(
                self.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const c_void,
                size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn owned_handle(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    let ok = unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((owned_handle(read)?, owned_handle(write)?))
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

// The documented Windows C argv quoting rule. `CreateProcessW` receives a
// mutable command line while lpApplicationName stays null for PATH resolution.
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|ch| ch.is_whitespace() || ch == '"') {
        return arg.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => slashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', 2 * slashes + 1));
                quoted.push('"');
                slashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', slashes));
                quoted.push(ch);
                slashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', 2 * slashes));
    quoted.push('"');
    quoted
}

fn environment_block() -> Vec<u16> {
    let mut block = Vec::new();
    let mut keys = HOST_BOOTSTRAP_ENV_KEYS.to_vec();
    keys.sort_unstable();
    for key in keys {
        if let Some(value) = std::env::var_os(key) {
            block.extend(OsStr::new(key).encode_wide());
            block.push('=' as u16);
            block.extend(value.encode_wide());
            block.push(0);
        }
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    block
}

pub(super) fn spawn(
    program: &str,
    args: &[String],
    workspace: &Path,
    columns: u16,
    rows: u16,
) -> Result<PtyProcess, SpawnFailure> {
    let job = PtyJob::new()?;
    let (pty_input_read, host_input_write) = pipe()?;
    let (host_output_read, pty_output_write) = pipe()?;
    let mut raw_console: HPCON = 0;
    let result = unsafe {
        CreatePseudoConsole(
            COORD {
                X: columns as i16,
                Y: rows as i16,
            },
            pty_input_read.as_raw_handle(),
            pty_output_write.as_raw_handle(),
            0,
            &mut raw_console,
        )
    };
    if result < 0 || raw_console == 0 {
        return Err(io::Error::other(format!(
            "CreatePseudoConsole failed with HRESULT 0x{:08x}",
            result as u32
        ))
        .into());
    }
    let console = PseudoConsole {
        raw: raw_console,
        columns,
        rows,
        _input_read: pty_input_read,
        _output_write: pty_output_write,
    };

    let mut attributes = AttributeList::new()?;
    attributes.set_pseudoconsole(raw_console)?;
    let job_handles = [job.raw()];
    let ok = unsafe {
        UpdateProcThreadAttribute(
            attributes.ptr(),
            0,
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            job_handles.as_ptr().cast(),
            size_of::<HANDLE>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error().into());
    }

    let command_line = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote_arg)
        .collect::<Vec<_>>()
        .join(" ");
    let mut command_line = wide(OsStr::new(&command_line));
    if command_line.len() > 32_767 {
        return Err(io::Error::other("PTY command line exceeds Windows limit").into());
    }
    let cwd = wide(workspace.as_os_str());
    let environment = environment_block();
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attributes.ptr();
    let mut process_info = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error().into());
    }
    // Even a malformed success response closes `job` and kills the process.
    if process_info.hProcess.is_null()
        || process_info.hProcess == INVALID_HANDLE_VALUE
        || process_info.hThread.is_null()
        || process_info.hThread == INVALID_HANDLE_VALUE
    {
        for handle in [process_info.hProcess, process_info.hThread] {
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                unsafe { CloseHandle(handle) };
            }
        }
        return Err(SpawnFailure {
            error: io::Error::other("CreateProcessW returned invalid process handles"),
            launched: true,
        });
    }
    let process = Arc::new(unsafe { OwnedHandle::from_raw_handle(process_info.hProcess) });
    let _thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread) };
    Ok(PtyProcess {
        stdin: File::from_std(std::fs::File::from(host_input_write)),
        output: std::fs::File::from(host_output_read),
        process,
        console,
        job,
    })
}

pub(super) fn wait_process(process: Arc<OwnedHandle>) -> io::Result<i32> {
    let wait = unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) };
    if wait != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error());
    }
    let mut code = 0;
    let ok = unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(code as i32)
}

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn quotes_windows_arguments_without_shell_interpretation() {
        assert_eq!(quote_arg("simple"), "simple");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(quote_arg("a b\\"), "\"a b\\\\\"");
    }
}
