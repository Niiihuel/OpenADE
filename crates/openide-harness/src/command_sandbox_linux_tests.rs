use super::*;

#[tokio::test]
async fn setup_rejects_invalid_helper_and_workspace_alias_without_payload() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let invalid = dir.path().join("invalid-helper");
    std::fs::write(&invalid, b"not an ELF helper").unwrap();
    let result = prepare_with_helper(
        "/bin/sh",
        &["-c".into(), "echo bad > payload-started".into()],
        &workspace,
        &invalid,
    );
    assert!(matches!(result, Err(AgentError::Tool(message)) if message.contains("not started")));
    assert!(!workspace.join("payload-started").exists());
    assert!(
        matches!(prepare_with_helper("-a", &["fake-argv0".into(),"/bin/sh".into(),"-c".into(),"echo bad > payload-started".into()], &workspace, &invalid), Err(AgentError::Tool(message)) if message.contains("starts with '-'"))
    );
    assert!(!workspace.join("payload-started").exists());
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&workspace, &alias).unwrap();
    assert!(
        matches!(prepare_with_helper("/bin/sh", &[], &alias, &invalid), Err(AgentError::Tool(message)) if message.contains("canonical"))
    );
    let secret = dir.path().join("outside");
    std::fs::write(&secret, b"safe").unwrap();
    std::fs::hard_link(&secret, workspace.join("alias-file")).unwrap();
    assert!(
        matches!(prepare_with_helper("/bin/sh", &[], &workspace, &invalid), Err(AgentError::Tool(message)) if message.contains("hard-linked"))
    );
    assert_eq!(std::fs::read(secret).unwrap(), b"safe");
}

#[tokio::test]
#[ignore = "requires installed Bubblewrap and working Linux namespaces/seccomp/pidfd/close_range"]
async fn actual_workspace_sandbox_handshake_executes_literal_argv() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    let output = run(
        "/bin/sh",
        &[
            "-c",
            "printf '%s' \"$1\" > literal",
            "probe",
            "$(touch leaked); emoji 🦀",
        ],
        10_000,
        &ToolContext {
            workspace: workspace.clone(),
            cancel: CancellationToken::new(),
        },
    )
    .await
    .unwrap();
    assert!(!output.is_error, "{}", output.content);
    assert_eq!(
        std::fs::read_to_string(workspace.join("literal")).unwrap(),
        "$(touch leaked); emoji 🦀"
    );
    assert!(!workspace.join("leaked").exists());
    use std::os::unix::fs::PermissionsExt;
    let dash_program = workspace.join("-name");
    std::fs::write(
        &dash_program,
        "#!/bin/sh\nprintf '%s' \"$1\" > dash-program-output\n",
    )
    .unwrap();
    std::fs::set_permissions(&dash_program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let context = ToolContext {
        workspace: workspace.clone(),
        cancel: CancellationToken::new(),
    };
    let output = run("./-name", &["$(touch dash-leak); 🦀"], 10_000, &context)
        .await
        .unwrap();
    assert!(!output.is_error, "{}", output.content);
    assert_eq!(
        std::fs::read_to_string(workspace.join("dash-program-output")).unwrap(),
        "$(touch dash-leak); 🦀"
    );
    assert!(!workspace.join("dash-leak").exists());
    for executable in ["-a", "-c", "--"] {
        let result = run(
            executable,
            &["fake-argv0", "./-name", "reinterpreted"],
            10_000,
            &context,
        )
        .await;
        assert!(
            matches!(result,Err(AgentError::Tool(message)) if message.contains("starts with '-'") && message.contains("not started"))
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("dash-program-output")).unwrap(),
            "$(touch dash-leak); 🦀"
        );
    }
}

fn build_probe(workspace: &Path) -> PathBuf {
    let source = workspace.join("linux_sandbox_probe.rs");
    let binary = workspace.join("linux_sandbox_probe");
    std::fs::write(
        &source,
        include_str!("../tests/fixtures/linux_sandbox_probe.rs"),
    )
    .unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = std::process::Command::new(compiler)
        .args(["--edition=2021"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "probe compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

#[tokio::test]
#[ignore = "requires installed Bubblewrap, Rust compiler and working Linux namespaces/seccomp/pidfd/close_range"]
async fn actual_filesystem_sockets_and_inherited_descriptors_are_confined() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let secret = outside.join("secret");
    std::fs::write(&secret, b"outside-canary").unwrap();
    std::os::unix::fs::symlink(&secret, workspace.join("escape")).unwrap();
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    tcp.set_nonblocking(true).unwrap();
    let socket = std::os::unix::net::UnixListener::bind(workspace.join("local.sock")).unwrap();
    socket.set_nonblocking(true).unwrap();
    // A deliberately inheritable host file must still disappear at payload exec.
    let canary = File::open(&secret).unwrap();
    assert_eq!(
        unsafe { libc::fcntl(canary.as_raw_fd(), libc::F_SETFD, 0) },
        0
    );
    let binary = build_probe(&workspace);
    let args = [
        "confinement".to_owned(),
        outside.display().to_string(),
        tcp.local_addr().unwrap().to_string(),
        canary.as_raw_fd().to_string(),
    ];
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    let result = run(
        binary.to_str().unwrap(),
        &borrowed,
        10_000,
        &ToolContext {
            workspace: workspace.clone(),
            cancel: CancellationToken::new(),
        },
    )
    .await
    .unwrap();
    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("confinement-ok"));
    assert_eq!(
        std::fs::read(workspace.join("inside")).unwrap(),
        b"workspace-ok"
    );
    assert_eq!(std::fs::read(secret).unwrap(), b"outside-canary");
    assert!(matches!(tcp.accept(),Err(error) if error.kind()==io::ErrorKind::WouldBlock));
    assert!(matches!(socket.accept(),Err(error) if error.kind()==io::ErrorKind::WouldBlock));
    assert!(!std::env::temp_dir().join("private-temp").exists());
}

#[tokio::test]
#[ignore = "requires installed Bubblewrap, Rust compiler and working Linux namespaces/seccomp/pidfd/close_range"]
async fn actual_setup_filter_failure_never_executes_target() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    let mut launch = prepare(
        "/bin/sh",
        &["-c".into(), "echo bad > payload-started".into()],
        &workspace,
    )
    .unwrap();
    // Fault injection replaces this launch's own filter FD without changing
    // pathname lookup or adding a production escape hatch. Bubblewrap must
    // reject this malformed filter before the trusted shell/target can start.
    let invalid = workspace.join("invalid-filter");
    std::fs::write(&invalid, b"bad").unwrap();
    let file = File::open(invalid).unwrap();
    assert!(
        unsafe {
            libc::dup3(
                file.as_raw_fd(),
                launch.inherited[2].as_raw_fd(),
                libc::O_CLOEXEC,
            )
        } >= 0
    );
    launch.command().stdin(Stdio::piped());
    let result = launch.spawn(&CancellationToken::new()).await;
    assert!(
        matches!(result,Err(AgentError::Tool(message)) if message.contains("not started") && message.contains("seccomp"))
    );
    assert!(!workspace.join("payload-started").exists());
    let token = CancellationToken::new();
    token.cancel();
    let launch = prepare(
        "/bin/sh",
        &["-c".into(), "echo bad > payload-started".into()],
        &workspace,
    )
    .unwrap();
    assert!(matches!(
        launch.spawn(&token).await,
        Err(AgentError::Cancelled)
    ));
    assert!(!workspace.join("payload-started").exists());
}

async fn wait_started(workspace: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !workspace.join("detached-started").exists() || !workspace.join("heartbeat").exists()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn assert_stopped(workspace: &Path) {
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = std::fs::read(workspace.join("heartbeat")).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        std::fs::read(workspace.join("heartbeat")).unwrap(),
        before,
        "setsid descendant survived sandbox cleanup"
    );
}

#[tokio::test]
#[ignore = "requires installed Bubblewrap, Rust compiler and working Linux namespaces/seccomp/pidfd/close_range"]
async fn actual_pid_namespace_ends_setsid_descendants_on_cancel_timeout_drop_and_exit() {
    for mode in ["cancel", "timeout", "drop", "exit"] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        let binary = build_probe(&workspace);
        let token = CancellationToken::new();
        let context = ToolContext {
            workspace: workspace.clone(),
            cancel: token.clone(),
        };
        let timeout = if mode == "timeout" { 1_000 } else { 10_000 };
        let running = tokio::spawn(async move {
            run(binary.to_str().unwrap(), &["tree", mode], timeout, &context).await
        });
        wait_started(&workspace).await;
        match mode {
            "cancel" => {
                token.cancel();
                assert!(matches!(running.await.unwrap(), Err(AgentError::Cancelled)));
            }
            "timeout" => {
                assert!(
                    matches!(running.await.unwrap(),Err(AgentError::EffectUncertain(message)) if message.contains("timed out"))
                );
            }
            "drop" => {
                running.abort();
                assert!(running.await.unwrap_err().is_cancelled());
            }
            "exit" => {
                let output = running.await.unwrap().unwrap();
                assert!(!output.is_error, "{}", output.content);
            }
            _ => unreachable!(),
        }
        assert_stopped(&workspace).await;
    }
}

#[cfg(target_arch = "x86_64")]
#[tokio::test]
#[ignore = "requires installed Bubblewrap, Rust compiler and working Linux namespaces/seccomp/pidfd/close_range"]
async fn actual_seccomp_rejects_x32_and_compat_syscall_abis() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    let binary = build_probe(&workspace);
    for mode in ["abi-x32", "abi-compat"] {
        let output = run(
            binary.to_str().unwrap(),
            &[mode],
            10_000,
            &ToolContext {
                workspace: workspace.clone(),
                cancel: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
        assert!(output.is_error, "{mode}: {}", output.content);
        assert!(
            output.content.contains("exit code: 159"),
            "{mode}: {}",
            output.content
        );
        assert_eq!(
            std::fs::read(workspace.join("abi-before")).unwrap(),
            b"entered"
        );
        assert!(!workspace.join("abi-bypass").exists());
    }
}
