//! Bounded workspace IO. On Unix every component is opened relative to a
//! directory descriptor with O_NOFOLLOW; a replaced parent cannot escape root.
use crate::error::{ApiError, ApiResult};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Component, Path},
};

pub const MAX_FILE: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 1000;
const MAX_VISITED: usize = 20_000;

pub fn conflict() -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        code: "file_conflict",
        message: "The file changed; reload and compare before saving".into(),
    }
}
fn io_error(error: std::io::Error) -> ApiError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ApiError {
            status: StatusCode::NOT_FOUND,
            code: "file_not_found",
            message: "File or directory does not exist".into(),
        }
    } else {
        ApiError::bad("Workspace path is inaccessible, unsupported, or a symlink")
    }
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn validate_path(value: &str, allow_empty: bool) -> ApiResult<()> {
    if value.is_empty() && allow_empty {
        return Ok(());
    }
    if value.is_empty()
        || value.len() > 2048
        || value.contains(['\0', '\\', '\n', '\r'])
        || value.ends_with('/')
    {
        return Err(ApiError::bad("A relative workspace path is required"));
    }
    if Path::new(value)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
        || value
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.eq_ignore_ascii_case(".git"))
    {
        return Err(ApiError::bad(
            "Path must stay inside the project and outside Git internals",
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveFile {
    pub request_id: String,
    pub path: String,
    pub content: String,
    pub expected_revision: Option<String>,
}

#[derive(Serialize)]
struct Entry {
    path: String,
    name: String,
    kind: &'static str,
    size_bytes: Option<u64>,
}

#[cfg(unix)]
mod guarded {
    use super::*;
    use std::{
        ffi::{CStr, CString},
        os::fd::{AsRawFd, FromRawFd},
    };

    pub struct Directory(pub File);
    fn name(value: &str) -> ApiResult<CString> {
        CString::new(value).map_err(|_| ApiError::bad("Invalid path"))
    }
    fn opened(fd: i32) -> ApiResult<File> {
        if fd < 0 {
            Err(io_error(std::io::Error::last_os_error()))
        } else {
            // SAFETY: successful open/openat returns a fresh owned descriptor.
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    impl Directory {
        pub fn root(root: &Path) -> ApiResult<Self> {
            // A project is a folder below the root, never the whole disk.
            if !root.is_absolute() || root.components().count() < 2 {
                return Err(ApiError::bad("Project path must be an absolute folder"));
            }
            let start = name("/")?;
            let mut current = Self(opened(unsafe {
                libc::open(
                    start.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            })?);
            for part in root.components() {
                match part {
                    Component::RootDir => {}
                    Component::Normal(p) => {
                        let p = p
                            .to_str()
                            .ok_or_else(|| ApiError::bad("Project path must be UTF-8"))?;
                        current = current.child(p)?;
                    }
                    _ => return Err(ApiError::bad("Project path must be canonical")),
                }
            }
            Ok(current)
        }
        pub fn child(&self, part: &str) -> ApiResult<Self> {
            let part = name(part)?;
            Ok(Self(opened(unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    part.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            })?))
        }
        pub fn parent(root: &Path, relative: &str) -> ApiResult<(Self, String)> {
            validate_path(relative, false)?;
            let mut parts = relative.split('/').collect::<Vec<_>>();
            let last = parts.pop().unwrap().to_owned();
            let mut dir = Self::root(root)?;
            for part in parts {
                dir = dir.child(part)?;
            }
            Ok((dir, last))
        }
        pub fn open_file(&self, part: &str) -> ApiResult<File> {
            let part = name(part)?;
            opened(unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    part.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            })
        }
        pub fn entries(&self) -> ApiResult<Vec<(String, &'static str, u64)>> {
            let fd = unsafe { libc::fcntl(self.0.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            if fd < 0 {
                return Err(ApiError::unavailable());
            }
            let stream = unsafe { libc::fdopendir(fd) };
            if stream.is_null() {
                unsafe {
                    libc::close(fd);
                }
                return Err(ApiError::unavailable());
            }
            struct Stream(*mut libc::DIR);
            impl Drop for Stream {
                fn drop(&mut self) {
                    unsafe {
                        libc::closedir(self.0);
                    }
                }
            }
            let stream = Stream(stream);
            let mut entries = Vec::new();
            loop {
                let entry = unsafe { libc::readdir(stream.0) };
                if entry.is_null() {
                    break;
                }
                let raw = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                let Ok(part) = raw.to_str() else { continue };
                if matches!(part, "." | "..") || part.eq_ignore_ascii_case(".git") {
                    continue;
                }
                let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                if unsafe {
                    libc::fstatat(
                        self.0.as_raw_fd(),
                        raw.as_ptr(),
                        &mut stat,
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                } != 0
                {
                    continue;
                }
                let kind = match stat.st_mode & libc::S_IFMT {
                    libc::S_IFDIR => "directory",
                    libc::S_IFREG => "file",
                    _ => continue,
                };
                entries.push((part.to_owned(), kind, stat.st_size.max(0) as u64));
                if entries.len() > MAX_VISITED {
                    break;
                }
            }
            entries.sort_by(|a, b| (a.1 != "directory", &a.0).cmp(&(b.1 != "directory", &b.0)));
            Ok(entries)
        }
        pub fn replace(&self, part: &str, content: &[u8], expected: Option<&str>) -> ApiResult<()> {
            let current = match self.open_file(part) {
                Ok(file) => Some(read_bytes(file)?),
                Err(e) if e.code == "file_not_found" => None,
                Err(e) => return Err(e),
            };
            if current.as_deref().map(hash).as_deref() != expected {
                return Err(conflict());
            }
            let temp = name(&format!(".eas-{}.tmp", uuid::Uuid::new_v4()))?;
            let dest = name(part)?;
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    temp.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            let mut file = opened(fd)?;
            let result = (|| {
                // Preserve the existing executable bit and permissions.
                if let Ok(old) = self.open_file(part) {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = old.metadata().map_err(io_error)?.permissions().mode() & 0o777;
                    file.set_permissions(std::fs::Permissions::from_mode(mode))
                        .map_err(io_error)?;
                }
                file.write_all(content).map_err(io_error)?;
                file.sync_all().map_err(io_error)?;
                let latest = match self.open_file(part) {
                    Ok(file) => Some(read_bytes(file)?),
                    Err(e) if e.code == "file_not_found" => None,
                    Err(e) => return Err(e),
                };
                if latest.as_deref().map(hash).as_deref() != expected {
                    return Err(conflict());
                }
                if expected.is_none() {
                    // linkat is an atomic no-clobber publish for a new file.
                    if unsafe {
                        libc::linkat(
                            self.0.as_raw_fd(),
                            temp.as_ptr(),
                            self.0.as_raw_fd(),
                            dest.as_ptr(),
                            0,
                        )
                    } != 0
                    {
                        let error = std::io::Error::last_os_error();
                        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                            conflict()
                        } else {
                            io_error(error)
                        });
                    }
                } else if unsafe {
                    libc::renameat(
                        self.0.as_raw_fd(),
                        temp.as_ptr(),
                        self.0.as_raw_fd(),
                        dest.as_ptr(),
                    )
                } != 0
                {
                    return Err(io_error(std::io::Error::last_os_error()));
                }
                self.0.sync_all().map_err(io_error)?;
                Ok(())
            })();
            unsafe {
                libc::unlinkat(self.0.as_raw_fd(), temp.as_ptr(), 0);
            }
            result
        }
    }
}

fn read_bytes(file: File) -> ApiResult<Vec<u8>> {
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.len() > MAX_FILE as u64 {
        return Err(ApiError::bad(
            "Only regular files up to 1 MiB are supported",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_FILE {
        return Err(ApiError::bad("File exceeds 1 MiB"));
    }
    Ok(bytes)
}

pub fn bytes(root: &Path, path: &str) -> ApiResult<Vec<u8>> {
    #[cfg(unix)]
    {
        let (dir, name) = guarded::Directory::parent(root, path)?;
        read_bytes(dir.open_file(&name)?)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, path);
        Err(ApiError::bad(
            "Guarded workspace IO is unavailable on this platform",
        ))
    }
}

pub fn read(root: &Path, path: &str) -> ApiResult<Value> {
    let bytes = bytes(root, path)?;
    let revision = hash(&bytes);
    let content = String::from_utf8(bytes).map_err(|_| ApiError::bad("File is not UTF-8 text"))?;
    if content.contains('\0') {
        return Err(ApiError::bad("Binary files cannot be edited as text"));
    }
    Ok(json!({"path":path,"content":content,"revision":revision,"language":null,"truncated":false}))
}

pub fn validate_save(root: &Path, request: &SaveFile) -> ApiResult<()> {
    validate_path(&request.path, false)?;
    if request.content.len() > MAX_FILE || request.content.contains('\0') {
        return Err(ApiError::bad("Content must be UTF-8 text up to 1 MiB"));
    }
    if request
        .expected_revision
        .as_ref()
        .is_some_and(|r| r.len() != 64 || !r.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(ApiError::bad(
            "expected_revision must be a SHA-256 digest or null for a new file",
        ));
    }
    let current = match bytes(root, &request.path) {
        Ok(v) => Some(hash(&v)),
        Err(e) if e.code == "file_not_found" => None,
        Err(e) => return Err(e),
    };
    if current != request.expected_revision {
        return Err(conflict());
    }
    Ok(())
}

pub fn save(root: &Path, request: &SaveFile) -> ApiResult<Value> {
    #[cfg(unix)]
    {
        let (dir, name) = guarded::Directory::parent(root, &request.path)?;
        dir.replace(
            &name,
            request.content.as_bytes(),
            request.expected_revision.as_deref(),
        )?;
        Ok(
            json!({"path":request.path,"content":request.content,"revision":hash(request.content.as_bytes()),"language":null,"truncated":false}),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (root, request);
        Err(ApiError::bad(
            "Guarded workspace IO is unavailable on this platform",
        ))
    }
}

pub fn list(root: &Path, path: &str, query: Option<&str>) -> ApiResult<Value> {
    validate_path(path, true)?;
    let query = query.filter(|s| !s.is_empty()).map(str::to_lowercase);
    if query.as_ref().is_some_and(|q| q.len() > 256) {
        return Err(ApiError::bad("Search exceeds 256 bytes"));
    }
    #[cfg(unix)]
    {
        let mut dir = guarded::Directory::root(root)?;
        if !path.is_empty() {
            for part in path.split('/') {
                dir = dir.child(part)?;
            }
        }
        let mut stack = vec![(dir, path.to_owned(), 0)];
        let mut entries = Vec::new();
        let mut visited = 0;
        let mut truncated = false;
        while let Some((dir, base, depth)) = stack.pop() {
            for (name, kind, size) in dir.entries()? {
                visited += 1;
                if visited > MAX_VISITED || entries.len() >= MAX_ENTRIES {
                    truncated = true;
                    break;
                }
                let relative = if base.is_empty() {
                    name.clone()
                } else {
                    format!("{base}/{name}")
                };
                if query
                    .as_ref()
                    .is_none_or(|q| name.to_lowercase().contains(q))
                {
                    entries.push(Entry {
                        path: relative.clone(),
                        name: name.clone(),
                        kind,
                        size_bytes: (kind == "file").then_some(size),
                    });
                }
                if query.is_some() && kind == "directory" {
                    if depth >= 32 {
                        truncated = true;
                    } else if let Ok(child) = dir.child(&name) {
                        stack.push((child, relative, depth + 1));
                    }
                }
            }
            if visited > MAX_VISITED || entries.len() >= MAX_ENTRIES {
                // Reaching the cap on the final entry of this directory can
                // still leave whole queued subdirectories unexplored.
                truncated |= !stack.is_empty();
                break;
            }
        }
        Ok(json!({"path":path,"entries":entries,"truncated":truncated}))
    }
    #[cfg(not(unix))]
    {
        let _ = (root, query);
        Err(ApiError::bad(
            "Guarded workspace IO is unavailable on this platform",
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn exact_result_cap_reports_unvisited_directories() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..MAX_ENTRIES {
            std::fs::write(root.path().join(format!("needle{index:04}.txt")), b"").unwrap();
        }
        let complete = list(root.path(), "", Some("needle")).unwrap();
        assert_eq!(complete["entries"].as_array().unwrap().len(), MAX_ENTRIES);
        assert_eq!(complete["truncated"], false);

        std::fs::create_dir(root.path().join("child")).unwrap();
        std::fs::write(root.path().join("child/needle-hidden.txt"), b"").unwrap();
        let partial = list(root.path(), "", Some("needle")).unwrap();
        assert_eq!(partial["entries"].as_array().unwrap().len(), MAX_ENTRIES);
        assert_eq!(partial["truncated"], true);
    }
}
