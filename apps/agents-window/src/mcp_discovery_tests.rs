use super::*;
use std::collections::BTreeMap;

fn server(program: impl Into<String>, args: &[&str]) -> McpServerSettings {
    McpServerSettings {
        id: "fixture".into(),
        enabled: false,
        program: program.into(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: None,
        credential_env_names: Vec::new(),
        env: BTreeMap::new(),
        url: None,
        http_bearer_token_env_name: None,
        http_oauth: None,
        http_env_headers: Vec::new(),
    }
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn executable(path: &Path, marker: &Path) {
    // Executing detection's fixture would create this marker and fail the test.
    write(
        path,
        &format!("#!/bin/sh\ntouch '{}'\nexit 91\n", marker.display()),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn configured_programs_resolve_from_path_or_absolute_without_execution_or_activation() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("must-not-run");
    let program = root.path().join("bin/arbitrary-mcp");
    executable(&program, &marker);
    let environment = DiscoveryEnvironment {
        search_path: root.path().join("bin").into_os_string(),
        ..Default::default()
    };
    let configured = server("arbitrary-mcp", &[]);
    let found = inspect(&configured, root.path(), &environment);
    assert_eq!(found.executable, Some(program.canonicalize().unwrap()));
    assert_eq!(found.launcher, McpLauncher::Direct);
    assert!(!configured.enabled);
    assert!(found.warnings.is_empty());
    let absolute = inspect(
        &server(program.to_string_lossy(), &[]),
        root.path(),
        &DiscoveryEnvironment::default(),
    );
    assert_eq!(absolute.executable, found.executable);
    assert!(!marker.exists());
    let missing = inspect(&server("missing-fixture", &[]), root.path(), &environment);
    assert!(missing.executable.is_none());
    assert_eq!(missing.warnings.len(), 1);
}

#[test]
fn environment_path_and_relative_entries_match_the_configured_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("tools/bin/local");
    let marker = root.path().join("must-not-run");
    executable(&program, &marker);
    let mut configured = server("local", &[]);
    configured.cwd = Some("tools".into());
    configured.env.insert("PATH".into(), "bin".into());
    let found = inspect(&configured, root.path(), &DiscoveryEnvironment::default());
    assert_eq!(found.executable, Some(program.canonicalize().unwrap()));
    assert!(!marker.exists());
}

#[test]
fn npx_launcher_and_exact_cached_package_are_separate_evidence() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("must-not-run");
    executable(&root.path().join("bin/npx"), &marker);
    let manifest = root
        .path()
        .join("cache/_npx/a/node_modules/@arbitrary/tool/package.json");
    write(
        &manifest,
        r#"{"name":"@arbitrary/tool","version":"2.1.0","description":"A locally described server","homepage":"https://vendor.example/tool?credential=private#fragment","repository":{"url":"git+https://code.example/vendor/tool.git"},"scripts":{"postinstall":"touch must-not-run"}}"#,
    );
    let environment = DiscoveryEnvironment {
        search_path: root.path().join("bin").into_os_string(),
        npm_cache: Some(root.path().join("cache")),
        ..Default::default()
    };
    let configured = server(
        "npx",
        &[
            "-y",
            "@arbitrary/tool@2.1.0",
            "--secret",
            "never-display-this",
        ],
    );
    let found = inspect(&configured, root.path(), &environment);
    assert!(found.executable.is_some());
    assert_eq!(
        found.launcher,
        McpLauncher::Npx {
            package: Some("@arbitrary/tool@2.1.0".into())
        }
    );
    let package = found.package.as_ref().unwrap();
    assert_eq!(package.name, "@arbitrary/tool");
    assert_eq!(package.version.as_deref(), Some("2.1.0"));
    assert_eq!(
        package.homepage.as_deref(),
        Some("https://vendor.example/tool")
    );
    assert_eq!(
        package.repository.as_deref(),
        Some("https://code.example/vendor/tool.git")
    );
    assert_eq!(package.manifest, manifest);
    assert!(found
        .warnings
        .iter()
        .all(|warning| !warning.contains("never-display-this")));
    let hints = found.identity_hints(&configured);
    assert_eq!(hints.package_name.as_deref(), Some("@arbitrary/tool"));
    assert!(!marker.exists());
    let other_version = inspect(
        &server("npx", &["@arbitrary/tool@3.0.0"]),
        root.path(),
        &environment,
    );
    assert!(other_version.executable.is_some());
    assert!(
        other_version.package.is_none(),
        "an older cache does not establish the requested version"
    );
}

#[test]
fn package_argument_parsing_does_not_treat_option_values_or_urls_as_packages() {
    let args = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        package_argument(&args(&["--package", "@vendor/tool@1.0.0", "binary"]), false).as_deref(),
        Some("@vendor/tool@1.0.0")
    );
    assert_eq!(
        package_argument(&args(&["--from=vendor-tool==1.0", "binary"]), true).as_deref(),
        Some("vendor-tool==1.0")
    );
    assert_eq!(
        package_argument(&args(&["--unknown", "could-be-a-secret"]), false),
        None
    );
    assert_eq!(
        package_argument(
            &args(&["--package", "first", "--package", "second", "first-binary"]),
            false
        ),
        None,
        "multiple packages cannot establish a single server identity"
    );
    assert_eq!(
        package_argument(&args(&["--package=first", "-p", "second", "binary"]), false),
        None
    );
    for invalid in [
        "../local",
        "https://user:private@example.test/tool",
        "pkg@https://example.test/archive",
        "@scope/../escape",
        "pkg@",
    ] {
        assert!(npm_request(invalid).is_none(), "{invalid}");
    }
    for invalid in [
        "../local",
        "pkg @ https://example.test/pkg",
        "pkg[broken",
        "pkg[]",
        "pkg==",
    ] {
        assert!(python_request(invalid).is_none(), "{invalid}");
    }
    let root = tempfile::tempdir().unwrap();
    let found = inspect(
        &server("npx", &["https://user:private@example.test/tool"]),
        root.path(),
        &DiscoveryEnvironment::default(),
    );
    assert_eq!(found.launcher, McpLauncher::Npx { package: None });
    assert_eq!(found.identity_hints(&server("npx", &[])).package_name, None);
}

#[test]
fn uvx_metadata_is_read_locally_and_matches_name_and_requested_version() {
    let root = tempfile::tempdir().unwrap();
    let manifest = root
        .path()
        .join("uv/tools/any-tool/lib/python3.13/site-packages/any_tool-1.2.dist-info/METADATA");
    write(&manifest, "Metadata-Version: 2.3\nName: Any_Tool\nVersion: 1.2\nSummary: Local Python metadata\nHome-page: https://python.example/any?token=private\nProject-URL: Source, https://code.example/any\n\nBody is not displayed\n");
    let environment = DiscoveryEnvironment {
        uv_roots: vec![root.path().join("uv/tools")],
        ..Default::default()
    };
    let found = inspect(
        &server("uvx", &["--from", "Any.Tool[extra]==1.2", "any-command"]),
        root.path(),
        &environment,
    );
    assert!(
        found.executable.is_none(),
        "cached metadata does not prove a launcher exists"
    );
    let package = found.package.unwrap();
    assert_eq!(package.name, "any-tool");
    assert_eq!(package.version.as_deref(), Some("1.2"));
    assert_eq!(
        package.homepage.as_deref(),
        Some("https://python.example/any")
    );
    assert_eq!(
        package.repository.as_deref(),
        Some("https://code.example/any")
    );
    assert_eq!(package.manifest, manifest);
    assert!(inspect(
        &server("uvx", &["any-tool==9.0"]),
        root.path(),
        &environment
    )
    .package
    .is_none());
    assert!(
        inspect(&server("uvx", &["another-tool"]), root.path(), &environment)
            .package
            .is_none()
    );
}

#[test]
fn direct_npm_metadata_must_declare_the_detected_executable() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("must-not-run");
    let program = root.path().join("download/bin/tool");
    executable(&program, &marker);
    let manifest = root.path().join("download/package.json");
    write(
        &manifest,
        r#"{"name":"downloaded-tool","version":"1.0","bin":{"tool":"bin/tool"}}"#,
    );
    let configured = server(program.to_string_lossy(), &[]);
    let found = inspect(&configured, root.path(), &DiscoveryEnvironment::default());
    assert_eq!(found.package.unwrap().name, "downloaded-tool");
    write(
        &manifest,
        r#"{"name":"unrelated-workspace","version":"1.0"}"#,
    );
    assert!(
        inspect(&configured, root.path(), &DiscoveryEnvironment::default())
            .package
            .is_none()
    );
    assert!(!marker.exists());
}

#[test]
fn remote_metadata_uses_only_the_endpoint_host_and_never_contacts_it() {
    let root = tempfile::tempdir().unwrap();
    let mut configured = server("", &[]);
    configured.url = Some("https://remote.example/mcp?private=query".into());
    let found = inspect(&configured, root.path(), &DiscoveryEnvironment::default());
    assert_eq!(found.launcher, McpLauncher::Remote);
    assert!(found.executable.is_none());
    assert!(found.package.is_none());
    assert_eq!(
        found.identity_hints(&configured).url_host.as_deref(),
        Some("remote.example")
    );
    assert!(safe_url(Some("https://user:private@example.test/icon.svg")).is_none());
    assert!(safe_url(Some("file:///private/local")).is_none());
}

#[test]
fn oversized_or_non_regular_metadata_is_bounded_and_never_blocks_for_a_writer() {
    let root = tempfile::tempdir().unwrap();
    let manifest = root.path().join("node_modules/any-tool/package.json");
    write(&manifest, &" ".repeat(MAX_MANIFEST_BYTES as usize + 1));
    let environment = DiscoveryEnvironment::default();
    let found = inspect(&server("npx", &["any-tool"]), root.path(), &environment);
    assert!(found.package.is_none());
    assert!(found
        .warnings
        .iter()
        .any(|warning| warning.contains("límite")));
    std::fs::remove_file(&manifest).unwrap();
    std::fs::create_dir(&manifest).unwrap();
    assert!(
        inspect(&server("npx", &["any-tool"]), root.path(), &environment)
            .package
            .is_none()
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::fs::remove_dir(&manifest).unwrap();
        let path = std::ffi::CString::new(manifest.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(
            inspect(&server("npx", &["any-tool"]), root.path(), &environment)
                .package
                .is_none()
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}

#[test]
fn detection_results_are_owned_by_the_exact_workspace_scope_and_registrations() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let configured = vec![server("missing-fixture", &[])];
    let mut controller =
        McpDiscoveryController::start(root.path(), McpSettingsScope::User, configured.clone())
            .unwrap();
    assert!(controller.matches_saved(root.path(), McpSettingsScope::User, &configured));
    assert!(!controller.matches_saved(other.path(), McpSettingsScope::User, &configured));
    assert!(!controller.matches_saved(root.path(), McpSettingsScope::Project, &configured));
    let mut changed = configured.clone();
    changed[0].enabled = true;
    assert!(!controller.matches_saved(root.path(), McpSettingsScope::User, &changed));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let results = loop {
        if let Some(result) = controller.try_recv() {
            break result.unwrap();
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(results.len(), 1);
    assert!(controller.try_recv().is_none());
    controller.invalidate();
    assert!(!controller.matches_saved(root.path(), McpSettingsScope::User, &configured));
}
