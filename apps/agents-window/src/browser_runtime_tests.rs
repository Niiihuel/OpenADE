use super::*;

/// A real bounded actor queue with no process, socket, environment or worker.
/// Host tests explicitly advance replies so startup/navigation races are stable.
pub(crate) struct BrowserCommands {
    pending: Receiver<PendingCommand>,
    handle: BrowserHandle,
}
impl BrowserCommands {
    pub(crate) fn finish_next(&self) -> Option<BrowserCommand> {
        self.finish_next_with_clipboard(None)
    }
    pub(crate) fn finish_next_with_clipboard(
        &self,
        clipboard: Option<BrowserClipboard>,
    ) -> Option<BrowserCommand> {
        let pending = self.pending.try_recv().ok()?;
        pending.lifecycle.store(FINISHED, Ordering::Release);
        let mut snapshot = self.handle.snapshot();
        snapshot.clipboard = clipboard;
        let _ = pending.reply.try_send(Ok(snapshot));
        Some(pending.command)
    }
    pub(crate) fn set_snapshot(&self, snapshot: BrowserSnapshot) {
        *self.handle.state.lock().unwrap() = snapshot;
    }
}

pub(crate) fn fixture(
    scope: BrowserScope,
    snapshot: BrowserSnapshot,
) -> (BrowserRuntime, BrowserCommands) {
    let (queue, pending) = mpsc::sync_channel(COMMAND_CAPACITY);
    let handle = BrowserHandle {
        queue,
        state: Arc::new(Mutex::new(snapshot)),
        closed: Arc::new(AtomicBool::new(false)),
        context_lost: Arc::new(AtomicBool::new(false)),
        visible: Arc::new(AtomicBool::new(false)),
        scope,
        identity_digest: "fixture".into(),
    };
    (
        BrowserRuntime {
            handle: handle.clone(),
            child: Arc::new(Mutex::new(None)),
            node_path: PathBuf::new(),
            proxy_path: PathBuf::new(),
            broker_address: String::new(),
            broker_token: String::new(),
        },
        BrowserCommands { pending, handle },
    )
}

fn handle() -> BrowserHandle {
    let (queue, _) = mpsc::sync_channel(1);
    BrowserHandle {
        queue,
        state: Arc::new(Mutex::new(BrowserSnapshot::default())),
        closed: Arc::new(AtomicBool::new(false)),
        context_lost: Arc::new(AtomicBool::new(false)),
        visible: Arc::new(AtomicBool::new(true)),
        scope: BrowserScope {
            workspace: PathBuf::from("/fixture"),
            session_id: "browser-test".into(),
        },
        identity_digest: "fixture".into(),
    }
}

#[test]
fn temporary_environment_replaces_every_stale_shell_alias_without_global_mutation() {
    let mut command = Command::new("node");
    for name in ["TMPDIR", "TMP", "TEMP"] {
        command.env(name, "/tmp/deleted-nix-shell");
    }
    let directory = Path::new("/private/browser/tmp");
    browser_temporary_environment(&mut command, directory);
    for name in ["TMPDIR", "TMP", "TEMP", "OPENIDE_BROWSER_TEMP_DIR"] {
        assert_eq!(
            command.get_envs().find(|(key, _)| *key == name).unwrap().1,
            Some(directory.as_os_str())
        );
    }
}

#[test]
fn private_temporary_directory_is_scoped_and_installer_cleanup_is_owned() {
    let data = tempfile::tempdir().unwrap();
    let unrelated = data.path().join("unrelated");
    fs::write(&unrelated, "keep").unwrap();
    let path = data.path().join("install-tmp");
    {
        let temporary = InstallerTemporaryDirectory(path.clone());
        private_directory(&temporary.0).unwrap();
        fs::write(temporary.0.join("artifact"), "test").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
    assert!(!path.exists());
    assert_eq!(fs::read_to_string(unrelated).unwrap(), "keep");
}

#[test]
fn passive_capture_does_not_clear_failed_navigation_and_explicit_retry_does() {
    let handle = handle();
    assert!(apply_response(&handle, &json!({"ok":false,"code":"command_failed","error":"net::ERR_CONNECTION_REFUSED","snapshot":{"url":"http://localhost:1/"}})).is_err());
    let refresh =
        json!({"ok":true,"snapshot":{"url":"http://localhost:1/","title":"No disponible"}});
    assert_eq!(
        apply_refresh_response(&handle, &refresh)
            .unwrap()
            .error
            .as_deref(),
        Some("net::ERR_CONNECTION_REFUSED")
    );
    assert!(apply_response(
        &handle,
        &json!({"ok":true,"snapshot":{"url":"http://localhost:3000/","title":"Proyecto"}})
    )
    .unwrap()
    .error
    .is_none());
}

#[test]
fn command_validation_preserves_viewport_protocol_and_input_bounds() {
    assert!(BrowserCommand::Navigate {
        url: "file:///etc/passwd".into()
    }
    .validate()
    .is_err());
    assert!(BrowserCommand::Navigate {
        url: "https://name:secret@example.test".into()
    }
    .validate()
    .is_err());
    assert!(BrowserCommand::Navigate {
        url: "http://localhost:3000".into()
    }
    .validate()
    .is_ok());
    assert!(BrowserCommand::Scroll {
        dx: f64::NAN,
        dy: 1.0
    }
    .validate()
    .is_err());
    assert!(BrowserCommand::Type {
        text: "á".repeat(8193)
    }
    .validate()
    .is_err());
    assert!(BrowserCommand::Resize {
        width: 800,
        height: 600
    }
    .validate()
    .is_ok());
}

#[test]
fn clipboard_reply_is_bounded_and_never_retained_in_shared_browser_state() {
    let handle = handle();
    let reply = apply_response(&handle, &json!({"ok":true,"snapshot":{"url":"about:blank","clipboard":{"text":"Niño 🧠","cut_token":"token"}}})).unwrap();
    assert_eq!(reply.clipboard.unwrap().text, "Niño 🧠");
    assert!(handle.snapshot().clipboard.is_none());
    assert!(apply_response(
        &handle,
        &json!({"ok":true,"snapshot":{"clipboard":{"text":"x".repeat(16*1024+1)}}})
    )
    .is_err());
}
