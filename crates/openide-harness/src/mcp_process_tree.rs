//! Process-tree lifetime for one local MCP server.
//!
//! The Windows path follows the same suspended-spawn / Job Object pattern as
//! `run_command`: descendants cannot run before the server joins the job.

#[cfg(unix)]
mod platform {
    use std::io;

    pub(crate) struct ProcessTreeGuard {
        pgid: i32,
    }

    impl ProcessTreeGuard {
        pub(crate) fn new(pid: Option<u32>) -> io::Result<Self> {
            let pid = pid.ok_or_else(|| io::Error::other("MCP server PID is unavailable"))?;
            let pgid = i32::try_from(pid)
                .map_err(|_| io::Error::other("MCP server PID exceeds process-group range"))?;
            Ok(Self { pgid })
        }
    }

    impl Drop for ProcessTreeGuard {
        fn drop(&mut self) {
            // Descendants deliberately moved into a different session can
            // escape a Unix process group.
            unsafe {
                libc::kill(-self.pgid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, ResumeThread, CREATE_SUSPENDED as WIN_CREATE_SUSPENDED,
        PROCESS_SET_QUOTA, PROCESS_TERMINATE, THREAD_SUSPEND_RESUME,
    };

    pub(crate) const CREATE_SUSPENDED: u32 = WIN_CREATE_SUSPENDED;

    pub(crate) struct ProcessTreeGuard {
        job: OwnedHandle,
    }

    impl ProcessTreeGuard {
        pub(crate) fn new() -> io::Result<Self> {
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            let job = owned_handle(raw)?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let success = unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if success == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { job })
        }

        pub(crate) fn assign_and_resume(&self, child: &tokio::process::Child) -> io::Result<()> {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("MCP server PID is unavailable"))?;
            let process = owned_handle(unsafe {
                OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid)
            })?;
            let success = unsafe {
                AssignProcessToJobObject(self.job.as_raw_handle(), process.as_raw_handle())
            };
            if success == 0 {
                return Err(io::Error::last_os_error());
            }
            resume_primary_thread(pid)
        }
    }

    fn resume_primary_thread(pid: u32) -> io::Result<()> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = owned_handle(snapshot)?;
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = owned_handle(unsafe {
                    OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID)
                })?;
                let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
                if previous == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                if previous != 1 {
                    return Err(io::Error::other(
                        "MCP server primary thread had an unexpected suspend count",
                    ));
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        Err(io::Error::other("MCP server primary thread was not found"))
    }

    fn owned_handle(raw: HANDLE) -> io::Result<OwnedHandle> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
        }
    }
}

pub(crate) use platform::ProcessTreeGuard;
#[cfg(windows)]
pub(crate) use platform::CREATE_SUSPENDED;
