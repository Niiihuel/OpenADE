//! The search path the user's own shell would use. An app started from the
//! desktop launcher does not inherit what shell start-up files add (version
//! managers, `~/.local/bin`), so CLIs the user can run in a terminal look
//! missing. The value is only used to find and start agent CLIs; this
//! process's own `PATH` is never changed, because hook identities include it.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const START: &str = "__OPENIDE_PATH_START__";
const END: &str = "__OPENIDE_PATH_END__";
const MAX_OUTPUT_BYTES: u64 = 256 * 1024;

/// Asks `shell` for its `PATH` as a login, interactive shell. `printenv`
/// reads the exported value, which is colon-separated in every shell (fish
/// would print its list form with a builtin). Start-up files may print
/// anything, so the value is taken from between two markers.
#[cfg(unix)]
pub(crate) fn probe(shell: &OsStr, timeout: Duration) -> Option<OsString> {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c"])
        .arg(format!("printf '{START}'; printenv PATH; printf '{END}'"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Its own group, so a start-up file that spawns something is stopped too.
        .process_group(0)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.take(MAX_OUTPUT_BYTES).read_to_end(&mut output);
        output
    });
    let deadline = Instant::now() + timeout;
    let finished = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => break false,
        }
    };
    if !finished {
        // SAFETY: the child was started as the leader of its own process group.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.wait();
        return None;
    }
    parse(&String::from_utf8_lossy(&reader.join().ok()?))
}

#[cfg(not(unix))]
pub(crate) fn probe(_shell: &OsStr, _timeout: Duration) -> Option<OsString> {
    None
}

fn parse(output: &str) -> Option<OsString> {
    let (_, rest) = output.rsplit_once(START)?;
    let (path, _) = rest.split_once(END)?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.into())
}

/// The shell's directories first, then any of this process's that it lacks.
pub(crate) fn merged(shell_path: &OsStr, process_path: Option<&OsStr>) -> OsString {
    let mut directories: Vec<_> = std::env::split_paths(shell_path).collect();
    for directory in process_path.into_iter().flat_map(std::env::split_paths) {
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    std::env::join_paths(directories).unwrap_or_else(|_| shell_path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_taken_from_the_last_marked_span_only() {
        assert_eq!(parse("motd\n__OPENIDE_PATH_START__/a:/b\n__OPENIDE_PATH_END__bye"), Some("/a:/b".into()));
        assert_eq!(
            parse("__OPENIDE_PATH_START__x__OPENIDE_PATH_END__ noise __OPENIDE_PATH_START__/real__OPENIDE_PATH_END__"),
            Some("/real".into())
        );
        for output in ["", "/a:/b", "__OPENIDE_PATH_START__/a", "__OPENIDE_PATH_START____OPENIDE_PATH_END__"] {
            assert_eq!(parse(output), None, "{output}");
        }
    }

    #[test]
    fn merge_keeps_shell_order_and_adds_only_missing_directories() {
        assert_eq!(
            merged(OsStr::new("/shell/bin:/usr/bin"), Some(OsStr::new("/usr/bin:/app/bin"))),
            OsString::from("/shell/bin:/usr/bin:/app/bin")
        );
        assert_eq!(merged(OsStr::new("/only"), None), OsString::from("/only"));
    }

    #[cfg(unix)]
    #[test]
    fn real_shell_reports_its_path_and_a_hanging_one_is_cut_off() {
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        // Stands in for a shell whose start-up files print noise and extend PATH.
        let noisy = script("noisy", "echo 'welcome back'; PATH=\"/from/rc:$PATH\"; export PATH; shift 3; eval \"$1\"");
        let found = probe(noisy.as_os_str(), Duration::from_secs(5)).unwrap();
        assert!(found.to_string_lossy().starts_with("/from/rc:"), "{found:?}");

        let hanging = script("hanging", "sleep 30");
        let started = Instant::now();
        assert_eq!(probe(hanging.as_os_str(), Duration::from_millis(200)), None);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(probe(OsStr::new("/nonexistent/shell"), Duration::from_secs(1)), None);
    }
}
